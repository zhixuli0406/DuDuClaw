//! §3.10 steps 4–6: the allowlist transform, the adoption commit, the
//! compensation and the recovery actions (child module of `isolation_adopt`).
//!
//! Transform (S3), from descriptors held on `isolation-quarantine/<row>` and
//! `agents/`, never following a symlink:
//! - `agents/<target>` is created 0700 with `mkdirat` (an existing entry
//!   refuses: `LABEL_NOT_FREE`);
//! - `SOUL.md`, `CONTRACT.toml` and `SKILLS/` are moved with
//!   `renameat2(NOREPLACE)`; each moved inode must be the one the step-1
//!   verification recorded in its stat manifest;
//! - `agent.toml` is written new (the sealed bytes, `[agent] name` rewritten
//!   on a new label) and `.claude/settings.json` is written new with only
//!   [`ADOPT_SETTINGS_KEYS`]; the originals stay sealed in the row;
//! - everything else (`CLAUDE.md`, `.codex/`, `.env`, other `.claude/`
//!   files, …) stays in the quarantine as the row's remainder, purged with it.
//!
//! Any kept byte sequence naming `<home>/agents/<old label>` refuses
//! (`ADOPT_PATH_REJECTED`) before anything moves.
use super::*;

/// A committed (or already committed) adoption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Adopted { pub(crate) request_id: String, pub(crate) employee: EmployeeId, pub(crate) target: String,
    pub(crate) self_approved: bool }

/// Test pause / crash points (the hook returning `true` ends the call at once
/// without cleanup, as on process death).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdoptHook { AfterMove, MidRekey, AfterSave }

#[cfg(test)]
thread_local! {
    pub(crate) static ADOPT_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(AdoptHook) -> bool>>> = std::cell::RefCell::new(None);
}

fn crash_at(point: AdoptHook) -> bool {
    #[cfg(test)]
    { ADOPT_HOOK.with(|hook| hook.borrow_mut().as_mut().is_some_and(|hook| hook(point))) }
    #[cfg(not(test))]
    { let _ = point; false }
}

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SKILL_ENTRIES: usize = 4096;
const MAX_SKILL_DEPTH: usize = 8;
/// Moved from the row into the new directory (inode preserved).
const MOVED: [&str; 3] = ["SOUL.md", "CONTRACT.toml", "SKILLS"];

/// What the transform carries, read from the sealed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SealedInputs {
    /// The bytes `agents/<target>/agent.toml` gets.
    pub(crate) agent_toml: Vec<u8>,
    /// The filtered `.claude/settings.json`.
    pub(crate) settings_json: Vec<u8>,
    /// Top-level entries carried (as present in the row).
    pub(crate) kept: Vec<String>,
    /// Entries that stay sealed as the remainder (incl. the original settings).
    pub(crate) sealed: Vec<String>,
    pub(crate) kept_bytes: u64,
    pub(crate) soul_summary: String,
}

// ── Pure helpers ────────────────────────────────────────────────────────

/// `settings` as the approval validator compares them (same algorithm as
/// `employee_dirs::Snapshot::settings`).
pub(crate) fn settings_from_toml(bytes: &[u8]) -> Result<String, AdoptError> {
    let text = std::str::from_utf8(bytes).map_err(|_| AdoptError::SettingsInvalid)?;
    let mut config: duduclaw_core::types::AgentConfig = toml::from_str(text).map_err(|_| AdoptError::SettingsInvalid)?;
    config.proactive.sanitize();
    config.sticker.sanitize();
    let value = serde_json::to_value(&config).map_err(|_| AdoptError::SettingsInvalid)?;
    let encoded = serde_json::to_string(&value).map_err(|_| AdoptError::SettingsInvalid)?;
    if encoded.len() > duduclaw_migration::contract::MAX_PROPOSAL_VALUE_BYTES
        || serde_json::from_str::<Value>(&encoded).ok().as_ref() != Some(&value) {
        return Err(AdoptError::SettingsInvalid);
    }
    Ok(encoded)
}

/// Keep only [`ADOPT_SETTINGS_KEYS`]. `permissions.deny`/`ask` must be string
/// arrays and `model` a string; a malformed narrowing key refuses (dropping a
/// deny list would widen). Absent settings give `{}`.
pub(crate) fn filter_settings(original: Option<&[u8]>) -> Result<Vec<u8>, AdoptError> {
    let mut kept = serde_json::Map::new();
    if let Some(bytes) = original {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| AdoptError::SettingsInvalid)?;
        let object = value.as_object().ok_or(AdoptError::SettingsInvalid)?;
        for key in ADOPT_SETTINGS_KEYS {
            let (top, sub) = match key.split_once('.') { Some((top, sub)) => (top, Some(sub)), None => (key, None) };
            let found = match sub {
                None => object.get(top),
                Some(sub) => object.get(top).and_then(|parent| parent.as_object()).and_then(|parent| parent.get(sub)),
            };
            let Some(found) = found else { continue };
            let valid = match sub {
                None => found.is_string(),
                Some(_) => found.as_array().is_some_and(|list| list.iter().all(Value::is_string)),
            };
            if !valid { return Err(AdoptError::SettingsInvalid); }
            match sub {
                None => { kept.insert(top.to_owned(), found.clone()); }
                Some(sub) => {
                    let parent = kept.entry(top.to_owned()).or_insert_with(|| json!({}));
                    if let Some(parent) = parent.as_object_mut() { parent.insert(sub.to_owned(), found.clone()); }
                }
            }
        }
    }
    let mut out = serde_json::to_vec_pretty(&Value::Object(kept)).map_err(|_| AdoptError::SettingsInvalid)?;
    out.push(b'\n');
    Ok(out)
}

/// Rewrite only `[agent] name` (format-preserving: every other line is
/// byte-identical); the result must parse to the original with only that
/// value changed, else `ADOPT_SETTINGS_INVALID`.
pub(crate) fn rename_agent(bytes: &[u8], label: &str) -> Result<Vec<u8>, AdoptError> {
    let text = std::str::from_utf8(bytes).map_err(|_| AdoptError::SettingsInvalid)?;
    let (mut out, mut in_agent, mut done) = (String::with_capacity(text.len() + label.len()), false, false);
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_agent = trimmed == "[agent]";
        } else if in_agent && !done && trimmed.split_once('=').is_some_and(|(key, _)| key.trim() == "name") {
            let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let newline = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
            out.push_str(&format!("{indent}name = \"{label}\"{newline}"));
            done = true;
            continue;
        }
        out.push_str(line);
    }
    let mut expected: toml::Value = toml::from_str(text).map_err(|_| AdoptError::SettingsInvalid)?;
    let renamed: toml::Value = toml::from_str(&out).map_err(|_| AdoptError::SettingsInvalid)?;
    let slot = expected.get_mut("agent").and_then(|agent| agent.get_mut("name")).ok_or(AdoptError::SettingsInvalid)?;
    *slot = toml::Value::String(label.to_owned());
    if !done || renamed != expected { return Err(AdoptError::SettingsInvalid); }
    Ok(out.into_bytes())
}

/// spawnd's loop images and their mount points (`duduclaw-spawnd`
/// `host.rs` `STORAGE_IMAGES`; names `<hex>.img` / `<hex>.mnt` from
/// `duduclaw-isolation-storage` `linux.rs`): the old root workspace.
const ROOT_IMAGES: &str = "/var/lib/duduclaw-spawnd/images";

/// Where the old employee's data lived, as a kept file could name it
/// (F6fb item 6): `<home>/agents/<old label>`, the row's quarantine
/// directory and the old root workspace, each for the given and the
/// canonical home, normalised like the content they are matched against.
pub(crate) struct Needles { paths: Vec<Vec<u8>>, home_env: Option<Vec<u8>> }

/// A byte that continues a path component or a variable name.
fn path_char(byte: u8) -> bool { byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~' | b'$') }

fn replace_where(bytes: &[u8], token: &[u8], with: &[u8], at: impl Fn(&[u8], usize) -> bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(token) && at(bytes, i) {
            out.extend_from_slice(with);
            i += token.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// JSON-escaped `\/` → `/`; `${HOME}`, `$HOME` and a leading `~/` → the
/// process home; runs of `/` → one; `/./` → `/`.
fn normalize_paths(bytes: &[u8], home_env: Option<&[u8]>) -> Vec<u8> {
    let mut out = replace_where(bytes, b"\\/", b"/", |_, _| true);
    if let Some(home) = home_env {
        out = replace_where(&out, b"${HOME}", home, |_, _| true);
        out = replace_where(&out, b"$HOME", home,
            |text, i| text.get(i + 5).is_none_or(|next| !(next.is_ascii_alphanumeric() || *next == b'_')));
        let mut tilde = home.to_vec();
        tilde.push(b'/');
        out = replace_where(&out, b"~/", &tilde, |text, i| i == 0 || !path_char(text[i - 1]));
    }
    let mut collapsed = Vec::with_capacity(out.len());
    for byte in out {
        if byte == b'/' && collapsed.last() == Some(&b'/') { continue; }
        collapsed.push(byte);
    }
    for _ in 0..64 {
        let next = replace_where(&collapsed, b"/./", b"/", |_, _| true);
        if next == collapsed { break; }
        collapsed = next;
    }
    collapsed
}

fn needles(home: &Path, q: &Quarantined) -> Needles {
    let home_env = std::env::var_os("HOME").and_then(|value| value.into_string().ok())
        .map(|value| value.trim_end_matches('/').as_bytes().to_vec()).filter(|value| !value.is_empty());
    let mut raw: Vec<std::path::PathBuf> = Vec::new();
    for base in [Some(home.to_path_buf()), std::fs::canonicalize(home).ok()].into_iter().flatten() {
        raw.push(base.join("agents").join(&q.label));
        raw.push(base.join(lifecycle::QUARANTINE_DIR).join(q.name()));
    }
    let key = hex::encode(q.employee.0);
    for suffix in ["img", "mnt"] { raw.push(std::path::PathBuf::from(format!("{ROOT_IMAGES}/{key}.{suffix}"))); }
    let mut paths: Vec<Vec<u8>> = Vec::new();
    for path in raw {
        let Some(text) = path.to_str() else { continue };
        let mut normal = normalize_paths(text.as_bytes(), home_env.as_deref());
        while normal.len() > 1 && normal.last() == Some(&b'/') { normal.pop(); }
        if !normal.is_empty() && !paths.contains(&normal) { paths.push(normal); }
    }
    Needles { paths, home_env }
}

impl Needles {
    /// `ADOPT_PATH_REJECTED` when the normalised bytes name any needle as a
    /// whole path: the match must start after a non-path byte and end at a
    /// separator, quote, whitespace or the end (so `…/ali` never matches
    /// `…/alice-x`, `…/alice.md` is a sibling, `…/alice.` at the end is not).
    fn check(&self, bytes: &[u8]) -> Result<(), AdoptError> {
        let text = normalize_paths(bytes, self.home_env.as_deref());
        for needle in &self.paths {
            let mut from = 0;
            while let Some(offset) = text.get(from..).and_then(|rest| rest.windows(needle.len()).position(|window| window == needle.as_slice())) {
                let (at, end) = (from + offset, from + offset + needle.len());
                let before = at == 0 || !path_char(text[at - 1]);
                let after = match text.get(end) {
                    None => true,
                    Some(b'.') => text.get(end + 1).is_none_or(|next| !path_char(*next) && *next != b'/'),
                    Some(next) => !path_char(*next),
                };
                if before && after { return Err(AdoptError::PathRejected); }
                from = at + 1;
            }
        }
        Ok(())
    }
}

fn check_paths(bytes: &[u8], needles: &Needles) -> Result<(), AdoptError> { needles.check(bytes) }

/// Test probe: whether `bytes` name an old path of `q` (with `home_env` as
/// the process home for `~/` and `$HOME`).
#[cfg(test)]
pub(crate) fn names_old_path(home: &Path, q: &Quarantined, home_env: Option<&str>, bytes: &[u8]) -> bool {
    let mut needles = needles(home, q);
    needles.home_env = home_env.map(|value| value.trim_end_matches('/').as_bytes().to_vec());
    needles.check(bytes).is_err()
}

// ── Descriptor operations ───────────────────────────────────────────────

#[cfg(unix)]
mod fsops {
    use super::*;
    use crate::fs_safe::SafeDir;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    pub(super) fn make_dir(parent: &SafeDir, name: &str) -> Result<(), AdoptError> {
        let fd = parent.duplicate_fd().map_err(unavailable)?;
        match nix::sys::stat::mkdirat(Some(fd.as_raw_fd()), name, nix::sys::stat::Mode::S_IRWXU) {
            Ok(()) => Ok(()),
            Err(nix::errno::Errno::EEXIST) => Err(AdoptError::LabelNotFree),
            Err(_) => Err(AdoptError::Unavailable),
        }
    }

    pub(super) fn remove_dir(parent: &SafeDir, name: &str) -> Result<(), AdoptError> {
        let fd = parent.duplicate_fd().map_err(unavailable)?;
        nix::unistd::unlinkat(Some(fd.as_raw_fd()), name, nix::unistd::UnlinkatFlags::RemoveDir).map_err(|_| AdoptError::Inconsistent)
    }

    pub(super) fn move_entry(from: &SafeDir, name: &str, to: &SafeDir) -> Result<(), AdoptError> {
        from.rename_noreplace(name, to, name).map_err(|_| AdoptError::Inconsistent)
    }

    /// Inode of `name` (a regular file or a directory), never followed.
    pub(super) fn ino(dir: &SafeDir, name: &str) -> Result<Option<u64>, AdoptError> {
        match dir.entry(name).map_err(unavailable)? {
            None => Ok(None),
            Some(entry) if entry.kind == crate::fs_safe::EntryKind::Dir =>
                Ok(Some(dir.child_dir(name, false).map_err(unavailable)?.ok_or(AdoptError::Inconsistent)?.metadata().map_err(unavailable)?.ino())),
            Some(entry) if entry.kind == crate::fs_safe::EntryKind::File =>
                Ok(Some(dir.open_read_checked(name, false).map_err(unavailable)?.ok_or(AdoptError::Inconsistent)?.metadata().map_err(unavailable)?.ino())),
            Some(_) => Err(AdoptError::ForeignDir),
        }
    }

    pub(super) struct Dirs { pub(super) agents: SafeDir, pub(super) row: SafeDir }

    /// `agents/` and the row's quarantine directory (owner and mode of
    /// `isolation-quarantine` checked, row root `(dev, ino)` = recorded).
    pub(super) fn open_dirs(home: &Path, q: &Quarantined) -> Result<Dirs, AdoptError> {
        let home_dir = SafeDir::open_nofollow(home, false).map_err(unavailable)?;
        let agents = home_dir.child_dir("agents", false).map_err(unavailable)?.ok_or(AdoptError::Unavailable)?;
        let parent = home_dir.child_dir(lifecycle::QUARANTINE_DIR, false).map_err(unavailable)?.ok_or(AdoptError::Unavailable)?;
        let metadata = parent.metadata().map_err(unavailable)?;
        if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 { return Err(AdoptError::Unavailable); }
        let row = parent.child_dir(&q.name(), false).map_err(unavailable)?.ok_or(AdoptError::DigestMismatch)?;
        let metadata = row.metadata().map_err(unavailable)?;
        if Some((metadata.dev(), metadata.ino())) != q.root { return Err(AdoptError::DigestMismatch); }
        Ok(Dirs { agents, row })
    }

    /// §3.5 step 1 for the row (M1): re-walk through a held descriptor.
    pub(super) fn verify(home: &Path, q: &Quarantined) -> Result<quarantine::Sealed, AdoptError> {
        let root = q.root.ok_or(Refusal::NotSealed)?;
        let dir = lifecycle::reopen_quarantined(home, &q.label, q.employee, q.incarnation).map_err(unavailable)?
            .ok_or(AdoptError::DigestMismatch)?;
        let fresh = quarantine::seal(home, &q.label, &dir, root);
        if fresh.state != quarantine::SealState::Sealed || fresh.digest.is_none() || fresh.digest != q.digest {
            return Err(AdoptError::DigestMismatch);
        }
        Ok(fresh)
    }

    fn walk_skills(dir: &SafeDir, needles: &Needles, depth: usize, count: &mut usize, bytes: &mut u64) -> Result<(), AdoptError> {
        if depth > MAX_SKILL_DEPTH { return Err(AdoptError::ForeignDir); }
        for entry in dir.list().map_err(unavailable)? {
            *count += 1;
            if *count > MAX_SKILL_ENTRIES || entry.name.is_empty() { return Err(AdoptError::ForeignDir); }
            match entry.kind {
                crate::fs_safe::EntryKind::File => {
                    let data = dir.read_bytes_checked(&entry.name, MAX_FILE_BYTES, true).map_err(|_| AdoptError::ForeignDir)?
                        .ok_or(AdoptError::ForeignDir)?;
                    check_paths(&data, needles)?;
                    *bytes += data.len() as u64;
                }
                crate::fs_safe::EntryKind::Dir => {
                    let child = dir.child_dir(&entry.name, false).map_err(|_| AdoptError::ForeignDir)?.ok_or(AdoptError::ForeignDir)?;
                    walk_skills(&child, needles, depth + 1, count, bytes)?;
                }
                crate::fs_safe::EntryKind::Other => return Err(AdoptError::ForeignDir),
            }
        }
        Ok(())
    }

    pub(super) fn sealed_inputs(home: &Path, q: &Quarantined, target: &str) -> Result<SealedInputs, AdoptError> {
        let dir = lifecycle::reopen_quarantined(home, &q.label, q.employee, q.incarnation).map_err(unavailable)?
            .ok_or(AdoptError::DigestMismatch)?;
        let needles = needles(home, q);
        let (mut kept, mut sealed, mut bytes) = (Vec::new(), Vec::new(), 0u64);
        for entry in dir.list().map_err(unavailable)? {
            if entry.name.is_empty() { sealed.push("?".to_owned()); continue; }
            if ADOPT_ALLOWED_TOP.contains(&entry.name.as_str()) { kept.push(entry.name) } else { sealed.push(entry.name) }
        }
        let original = dir.read_bytes_checked("agent.toml", MAX_FILE_BYTES, true).map_err(|_| AdoptError::ForeignDir)?
            .ok_or(AdoptError::ForeignDir)?;
        sealed.push("agent.toml (original)".to_owned());
        let agent_toml = if target == q.label { original } else { rename_agent(&original, target)? };
        check_paths(&agent_toml, &needles)?;
        let mut soul_summary = String::new();
        for name in ["SOUL.md", "CONTRACT.toml"] {
            if let Some(data) = dir.read_bytes_checked(name, MAX_FILE_BYTES, true).map_err(|_| AdoptError::ForeignDir)? {
                check_paths(&data, &needles)?;
                bytes += data.len() as u64;
                if name == "SOUL.md" { soul_summary = String::from_utf8_lossy(&data).into_owned(); }
            }
        }
        if let Some(skills) = dir.child_dir("SKILLS", false).map_err(|_| AdoptError::ForeignDir)? {
            walk_skills(&skills, &needles, 0, &mut 0, &mut bytes)?;
        }
        let original_settings = match dir.child_dir(".claude", false).map_err(|_| AdoptError::ForeignDir)? {
            Some(claude) => {
                for entry in claude.list().map_err(unavailable)? {
                    sealed.push(format!(".claude/{}", if entry.name.is_empty() { "?" } else { entry.name.as_str() }));
                }
                claude.read_bytes_checked("settings.json", MAX_FILE_BYTES, true).map_err(|_| AdoptError::ForeignDir)?
            }
            None => None,
        };
        let settings_json = filter_settings(original_settings.as_deref())?;
        check_paths(&settings_json, &needles)?;
        bytes += (agent_toml.len() + settings_json.len()) as u64;
        Ok(SealedInputs { agent_toml, settings_json, kept, sealed, kept_bytes: bytes, soul_summary })
    }

    /// The allowlist transform into a new `agents/<target>`.
    pub(super) fn transform(dirs: &Dirs, target: &str, inputs: &SealedInputs, verified: &quarantine::Sealed) -> Result<(), AdoptError> {
        make_dir(&dirs.agents, target)?;
        let new = dirs.agents.child_dir(target, false).map_err(unavailable)?.ok_or(AdoptError::Inconsistent)?;
        let manifest = verified.stat_manifest.as_ref().and_then(|m| m["entries"].as_array()).ok_or(AdoptError::DigestMismatch)?;
        for name in MOVED {
            let Some(before) = ino(&dirs.row, name)? else { continue };
            let recorded = manifest.iter().find(|entry| entry[0].as_str() == Some(hex::encode(name).as_str()))
                .and_then(|entry| entry[1].as_u64());
            if recorded != Some(before) { return Err(AdoptError::DigestMismatch); }
            move_entry(&dirs.row, name, &new)?;
            if ino(&new, name)? != Some(before) { return Err(AdoptError::DigestMismatch); }
        }
        if !new.create_file_new("agent.toml", &inputs.agent_toml).map_err(unavailable)? { return Err(AdoptError::Inconsistent); }
        let claude = new.child_dir(".claude", true).map_err(unavailable)?.ok_or(AdoptError::Inconsistent)?;
        if !claude.create_file_new("settings.json", &inputs.settings_json).map_err(unavailable)? { return Err(AdoptError::Inconsistent); }
        Ok(())
    }

    /// Undo the transform: moved entries back (NOREPLACE), written files
    /// removed, the new directory removed. `Ok(true)` when anything was
    /// there. A foreign entry in the new directory refuses (`Inconsistent`).
    pub(super) fn undo(home: &Path, q: &Quarantined, target: &str) -> Result<bool, AdoptError> {
        let dirs = open_dirs(home, q)?;
        let Some(new) = dirs.agents.child_dir(target, false).map_err(|_| AdoptError::Inconsistent)? else { return Ok(false) };
        for name in MOVED {
            if new.entry(name).map_err(unavailable)?.is_none() { continue; }
            if dirs.row.entry(name).map_err(unavailable)?.is_some() { return Err(AdoptError::Inconsistent); }
            move_entry(&new, name, &dirs.row)?;
        }
        if new.entry("agent.toml").map_err(unavailable)?.is_some() { new.remove_file("agent.toml").map_err(|_| AdoptError::Inconsistent)?; }
        if let Some(claude) = new.child_dir(".claude", false).map_err(|_| AdoptError::Inconsistent)? {
            for entry in claude.list().map_err(unavailable)? {
                if entry.kind != crate::fs_safe::EntryKind::File || entry.name.is_empty() { return Err(AdoptError::Inconsistent); }
                claude.remove_file(&entry.name).map_err(|_| AdoptError::Inconsistent)?;
            }
            remove_dir(&new, ".claude")?;
        }
        if !new.list().map_err(unavailable)?.is_empty() { return Err(AdoptError::Inconsistent); }
        remove_dir(&dirs.agents, target)?;
        Ok(true)
    }
}

#[cfg(not(unix))]
mod fsops {
    use super::*;
    pub(super) struct Dirs;
    pub(super) fn open_dirs(_: &Path, _: &Quarantined) -> Result<Dirs, AdoptError> { Err(AdoptError::Unavailable) }
    pub(super) fn verify(_: &Path, _: &Quarantined) -> Result<quarantine::Sealed, AdoptError> { Err(AdoptError::Unavailable) }
    pub(super) fn sealed_inputs(_: &Path, _: &Quarantined, _: &str) -> Result<SealedInputs, AdoptError> { Err(AdoptError::Unavailable) }
    pub(super) fn transform(_: &Dirs, _: &str, _: &SealedInputs, _: &quarantine::Sealed) -> Result<(), AdoptError> { Err(AdoptError::Unavailable) }
    pub(super) fn undo(_: &Path, _: &Quarantined, _: &str) -> Result<bool, AdoptError> { Err(AdoptError::Unavailable) }
}

/// The sealed inputs of `q` for `target` (path check, settings filter, name rewrite).
pub(crate) fn sealed_inputs(home: &Path, q: &Quarantined, target: &str) -> Result<SealedInputs, AdoptError> {
    fsops::sealed_inputs(home, q, target)
}

// ── Stores (S10) ────────────────────────────────────────────────────────

/// Re-key every registered store except the digest-covered org entry (the
/// proposal states the new one) under the row key. Idempotent.
pub(crate) fn rekey_row(home: &Path, q: &Quarantined, adopt: &AdoptRef) -> Result<(), AdoptError> {
    rekey_stores(home, q, adopt, false).map(|_| ())
}

/// [`rekey_row`], with `Ok(true)` when the `MidRekey` crash point fired after
/// a store moved. The crash is reported apart from a store failure: a
/// failure rolls back (§3.10 step 6), a crash must leave the row `adopting`
/// and the request `applying` for recovery to finish the re-key (S10).
fn rekey_stores(home: &Path, q: &Quarantined, adopt: &AdoptRef, crash_point: bool) -> Result<bool, AdoptError> {
    use crate::isolation_label_stores::{TombstoneAction, LABEL_KEYED_STORES};
    // The principal side (users.db bindings) is never re-keyed: only counted.
    for store in LABEL_KEYED_STORES.iter().filter(|store| !matches!(store.action, TombstoneAction::Digest | TombstoneAction::Principal)) {
        store.rekey(home, &q.label, &adopt.store_key()).map_err(unavailable)?;
        if crash_point && crash_at(AdoptHook::MidRekey) { return Ok(true); }
    }
    Ok(false)
}

/// Which stores [`require_zero`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zero {
    /// Before `save()`: every non-digest store, incl. the users.db bindings
    /// (a binding left on the label would be inherited by the new id).
    BeforeSave,
    /// After `save()` (resume, roll forward): the new id's own mirror and
    /// bindings are on the target by then; every other store still counts
    /// (writes stay gated while the row is `adopting`).
    AfterSave,
}

/// A positive zero count under every label (an unreadable store is not zero).
fn require_zero(home: &Path, labels: &[&str], zero: Zero) -> Result<(), AdoptError> {
    use crate::isolation_label_stores::{TombstoneAction, LABEL_KEYED_STORES};
    for store in LABEL_KEYED_STORES.iter().filter(|store| store.action != TombstoneAction::Digest) {
        if zero == Zero::AfterSave && store.action == TombstoneAction::Principal { continue; }
        for label in labels {
            if store.count_under(home, label).map_err(unavailable)? != 0 { return Err(AdoptError::StoresNotEmpty); }
        }
    }
    Ok(())
}

// ── apply.db writes ─────────────────────────────────────────────────────

fn write_marker(home: &Path, target: &str, owner: &str, digest: &str, adopt: &AdoptRef) -> Result<(), AdoptError> {
    let db = restore::open_apply(home)?;
    ensure_marker_schema(&db).map_err(unavailable)?;
    db.execute("INSERT INTO employee_dir_markers(label,owner_user_id,digest,created_at,origin,quarantine_ref)
        VALUES(?1,?2,?3,?4,'adopted',?5)", params![target, owner, digest, chrono::Utc::now().to_rfc3339(), adopt.encode()])
        .map_err(|_| AdoptError::LabelNotFree)?;
    Ok(())
}

fn delete_marker(home: &Path, target: &str, adopt: &AdoptRef) -> Result<(), AdoptError> {
    let db = restore::open_apply(home)?;
    if marker(&db, target)?.is_some_and(|(_, _, origin, reference)| origin == "adopted" && reference.as_deref() == Some(adopt.encode().as_str())) {
        db.execute("DELETE FROM employee_dir_markers WHERE label=?1 AND origin='adopted' AND quarantine_ref=?2",
            params![target, adopt.encode()]).map_err(unavailable)?;
    }
    Ok(())
}

/// Shared: exactly the selected bindings (users.db, after apply.db, M6).
/// Idempotent for a resumed apply (F6fb item 1): bindings already equal to
/// the selection are left as they are; any other binding refuses.
fn bind_members(home: &Path, q: &Quarantined, target: &str, members: &Value) -> Result<(), AdoptError> {
    let selection = members.as_array().ok_or(AdoptError::MembersInvalid)?.iter().map(|member| Ok(duduclaw_auth::db::RestoredMember {
        user_id: member["user_id"].as_str().ok_or(AdoptError::MembersInvalid)?.to_owned(),
        access_level: member["access_level"].as_str().and_then(|level| level.parse().ok()).ok_or(AdoptError::MembersInvalid)?,
    })).collect::<Result<Vec<_>, AdoptError>>()?;
    if selection.is_empty() { return Ok(()); }
    let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadWrite).map_err(unavailable)?;
    let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    if duduclaw_auth::isolation::read_isolation_tenant(&tx).map_err(unavailable)? != q.tenant { return Err(AdoptError::NotSameTenant); }
    let existing: Vec<(String, String)> = {
        let mut query = tx.prepare("SELECT user_id,access_level FROM user_agent_bindings WHERE agent_name=?1 ORDER BY user_id")
            .map_err(unavailable)?;
        let rows = query.query_map([target], |r| Ok((r.get(0)?, r.get(1)?))).map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
        rows
    };
    if !existing.is_empty() {
        let mut wanted: Vec<(String, duduclaw_auth::AccessLevel)> =
            selection.iter().map(|member| (member.user_id.clone(), member.access_level)).collect();
        wanted.sort_by(|a, b| a.0.cmp(&b.0));
        let have: Vec<(String, Option<duduclaw_auth::AccessLevel>)> =
            existing.into_iter().map(|(user, level)| (user, level.parse().ok())).collect();
        let same = have.len() == wanted.len()
            && have.iter().zip(&wanted).all(|((user, level), (want, wanted))| user == want && *level == Some(*wanted));
        return if same { tx.finish().map_err(unavailable) } else { Err(AdoptError::MembersInvalid) };
    }
    duduclaw_auth::db::restore_shared_bindings_in_transaction(&tx, target, &selection).map_err(|_| AdoptError::MembersInvalid)?;
    tx.commit().map_err(unavailable)
}

/// Shared rollback (F6fb item 1): remove every member binding the adoption
/// wrote on `target` (one users.db transaction, ACL bump). Refuses
/// (`Inconsistent`) when a live row on the target is not this adoption's.
fn unbind_members(home: &Path, target: &str, adopt: &AdoptRef) -> Result<(), AdoptError> {
    if let Some((receipt, _, _)) = lifecycle::confirmed_row(home, target).map_err(unavailable)? {
        if !receipt.disabled {
            let db = restore::open_apply(home)?;
            let ours = marker(&db, target)?
                .is_some_and(|(_, _, origin, reference)| origin == "adopted" && reference.as_deref() == Some(adopt.encode().as_str()));
            if !ours || receipt.employee == adopt.employee { return Err(AdoptError::Inconsistent); }
        }
    }
    let bound: i64 = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(unavailable)?
        .query_row("SELECT count(*) FROM user_agent_bindings WHERE agent_name=?1", [target], |r| r.get(0)).map_err(unavailable)?;
    if bound == 0 { return Ok(()); }
    match lifecycle::remove_shared_bindings(home, target) {
        Ok(_) => Ok(()),
        Err(lifecycle::LifecycleError::Stale) => Err(AdoptError::Inconsistent),
        Err(_) => Err(AdoptError::Unavailable),
    }
}

/// F6fb item 5: after the commit, compensation moves the tree back only while
/// it is still exactly what the adoption wrote (its `adopted` marker digest).
/// Anything the new employee changed since is never moved into the old row.
fn committed_tree_unchanged(home: &Path, target: Option<&str>, adopt: &AdoptRef) -> Result<(), AdoptError> {
    let target = target.ok_or(AdoptError::Inconsistent)?;
    let digest = {
        let db = restore::open_apply(home)?;
        let (_, digest, origin, reference) = marker(&db, target)?.ok_or(AdoptError::Inconsistent)?;
        if origin != "adopted" || reference.as_deref() != Some(adopt.encode().as_str()) { return Err(AdoptError::Inconsistent); }
        digest
    };
    let snapshot = crate::handlers::employee_dirs::snapshot(home, &home.join("agents"), target).map_err(|_| AdoptError::Inconsistent)?;
    if snapshot.digest != digest { return Err(AdoptError::Inconsistent); }
    Ok(())
}

/// The commit's owner/approval re-check ([`recheck_commit`]) on a users.db transaction.
type CommitGate<'a> = dyn Fn(&Connection) -> Result<(), AdoptError> + 'a;

/// The adoption commit (§3.10 step 5, R12): row `adopted` by the new id,
/// request `done`, the old id `root_absent`. `gate` (security round 3 N1):
/// run under this apply.db writer lock in a users.db IMMEDIATE transaction
/// held until the apply.db commit (M6 order), so no users.db writer — e.g. a
/// password reset of the owner — lands between the re-check and the commit;
/// a refusal commits nothing. `None` only for recovery's roll forward of a
/// proposal the store already applied.
fn finish(home: &Path, q: &Quarantined, request: &RestoreRequest, employee: EmployeeId, target: &str,
    gate: Option<&CommitGate<'_>>) -> Result<(), AdoptError> {
    let mut db = restore::open_apply(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    restore::set_seal(&tx, q.employee, q.incarnation, &["adopting"], "adopted")?;
    tx.execute("UPDATE employee_quarantine SET adopted_by_employee=?3,closed_by_request=?4 WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(q.employee.0), q.incarnation, hex::encode(employee.0), request.request_id]).map_err(unavailable)?;
    restore::set_request_state(&tx, &request.request_id, &["applying"], "done", None, chrono::Utc::now().timestamp())?;
    let changed = tx.execute("UPDATE root_allocations SET root_state='root_absent' WHERE employee_hex=?1
        AND root_state IN ('allocated','root_absent')", [hex::encode(q.employee.0)]).map_err(unavailable)?;
    if changed != 1 { return Err(AdoptError::Inconsistent); }
    match gate {
        Some(gate) => {
            let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadWrite).map_err(unavailable)?;
            let held = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
            let conn: &Connection = &held;
            gate(conn)?;
            tx.commit().map_err(unavailable)?;
            held.finish().map_err(unavailable)?;
        }
        None => tx.commit().map_err(unavailable)?,
    }
    let details = restore::audit_details(request, json!({"new_employee": hex::encode(employee.0), "target_label": target}));
    restore::audit(home, "isolation.employee_adopted", &q.label, Severity::Info, details.clone());
    if request.self_approved == Some(true) {
        restore::audit(home, "isolation.employee_restore_self_approved", &q.label, Severity::Warning, details.clone());
    }
    // R12 / S5e: shown as 主機資源狀態不明; the operator is alerted.
    restore::audit(home, "isolation.employee_root_absent", &q.label, Severity::Warning,
        json!({"employee": hex::encode(q.employee.0), "stage": "adoption", "request_id": request.request_id}));
    Ok(())
}

/// Roll back (§3.10 step 6): undo the transform, drop the adopted marker,
/// row → `needs_reseal` (then re-verified) or `sealed` when nothing moved,
/// request → `state` (`failed`, `denied` or `expired`).
pub(crate) fn roll_back(home: &Path, env: &AdoptEnv<'_>, q: &Quarantined, request: Option<&RestoreRequest>, target: Option<&str>,
    (state, failure): (&str, Option<&str>)) -> Result<(), AdoptError> {
    let adopt = AdoptRef { employee: q.employee, incarnation: q.incarnation, digest: q.digest.clone().unwrap_or_default() };
    // F6fb item 5: past the commit, never move what the new employee changed.
    if q.seal == "adopted" {
        if let Err(error) = committed_tree_unchanged(home, target, &adopt) {
            restore::audit(home, "isolation.employee_adoption_compensation_refused", &q.label, Severity::Critical, json!({
                "request_id": request.map(|r| r.request_id.clone()), "employee": hex::encode(q.employee.0),
                "incarnation": q.incarnation, "target_label": target, "reason": "tree_changed_after_commit",
                "failure": failure}));
            return Err(error);
        }
    }
    // F6fb item 1: a Shared adoption's member bindings go with it (before the
    // row changes, so a users.db failure leaves the row for a retry).
    if let (Some(target), EmployeeKind::Shared) = (target, q.kind) { unbind_members(home, target, &adopt)?; }
    let moved = match target { Some(target) => fsops::undo(home, q, target)?, None => false };
    if let Some(target) = target { delete_marker(home, target, &adopt)?; }
    {
        let mut db = restore::open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        restore::set_seal(&tx, q.employee, q.incarnation, &["adopting", "adopted"], if moved { "needs_reseal" } else { "sealed" })?;
        tx.execute("UPDATE employee_quarantine SET adopted_by_employee=NULL,closed_by_request=NULL WHERE employee_hex=?1 AND incarnation=?2",
            params![hex::encode(q.employee.0), q.incarnation]).map_err(unavailable)?;
        if let Some(request) = request {
            restore::set_request_state(&tx, &request.request_id, &["requested", "applying", "done"], state, failure, env.now)?;
        }
        tx.commit().map_err(unavailable)?;
    }
    if moved { restore::recover_label(home, &env.restore(), &q.label)?; }
    restore::audit(home, "isolation.employee_adoption_rolled_back", &q.label, Severity::Warning, json!({
        "request_id": request.map(|r| r.request_id.clone()), "employee": hex::encode(q.employee.0), "incarnation": q.incarnation,
        "state": state, "failure": failure, "moved_back": moved}));
    Ok(())
}

/// Roll forward: the proposal is Applied and the live row on `target` is a
/// new id whose marker names this row.
pub(crate) fn roll_forward(home: &Path, env: &AdoptEnv<'_>, q: &Quarantined, adopt: &AdoptRef, request: Option<&RestoreRequest>,
    target: &str) -> Result<(), AdoptError> {
    let request = request.ok_or(AdoptError::Inconsistent)?;
    let (receipt, _, _) = lifecycle::confirmed_row(home, target).map_err(unavailable)?.ok_or(AdoptError::Inconsistent)?;
    if receipt.disabled || receipt.employee == q.employee || receipt.label != target { return Err(AdoptError::Inconsistent); }
    let db = restore::open_apply(home)?;
    let (_, _, origin, reference) = marker(&db, target)?.ok_or(AdoptError::Inconsistent)?;
    if origin != "adopted" || reference.as_deref() != Some(adopt.encode().as_str()) { return Err(AdoptError::Inconsistent); }
    drop(db);
    require_zero(home, &[&q.label, target], Zero::AfterSave)?;
    // The crash may have come between `save()` and the member bindings.
    if let Some(members) = &request.member_selection { bind_members(home, q, target, members)?; }
    let _ = env;
    finish(home, q, request, receipt.employee, target, None)
}

// ── Apply step and compensation ─────────────────────────────────────────

/// After a failure inside the apply step: roll back with `failed: <code>`.
fn failed(home: &Path, env: &AdoptEnv<'_>, q: &Quarantined, request: &RestoreRequest, target: &str, error: AdoptError) -> AdoptError {
    if let Err(rollback) = roll_back(home, env, q, Some(request), Some(target), ("failed", Some(error.code()))) {
        restore::audit(home, "isolation.employee_adoption_inconsistent", &q.label, Severity::Critical, json!({
            "request_id": request.request_id, "failure": error.code(), "rollback": rollback.code()}));
        return AdoptError::Inconsistent;
    }
    error
}

/// `EmployeeAuthority::stage` for an adoption item, after the root
/// `Allocate` gave `receipt` (a new id and UID): verify, transform, re-key,
/// zero count, marker, `save` (the confirmed row + Personal mirror), Shared
/// bindings, commit. Any failure rolls the tree back and returns `Err`; the
/// confirmation store then compensates the new allocation (`Disable`).
///
/// Security round 3 N1: `save` receives a gate it must run inside the
/// users.db write transaction that writes the Personal mirror, before the
/// mirror (`save_confirmed_receipt_gated`; with no mirror, in its own users.db
/// IMMEDIATE transaction): [`recheck_commit`] — owner eligibility, the Q3
/// credential check and the admin-filed / standard F6c2 decision. The commit
/// ([`finish`]) runs it once more under its apply.db lock; a resumed stage
/// (`applying`, before or after `save`) runs the same checks. A refusal rolls
/// back with `failed: <code>` and commits nothing; a `save` that never ran the
/// gate is refused `Inconsistent`.
pub(crate) fn apply_item(home: &Path, env: &AdoptEnv<'_>, item: &ConfirmationItem, receipt: &allocation::Receipt, approver: &str,
    save: &mut dyn FnMut(&dyn Fn(&Connection) -> Result<(), ()>) -> Result<(), ()>) -> Result<Adopted, AdoptError> {
    let adopt = proposal_ref(item).map_err(|_| AdoptError::Inconsistent)?.ok_or(AdoptError::Inconsistent)?;
    let settings = text_field(item, "settings").ok_or(AdoptError::SettingsInvalid)?.to_owned();
    let (request, q, target, self_approved) = match validate_item(home, env, item, &adopt, approver)? {
        Phase::Adopted { employee } => {
            // F6fb item 4: a replay is bound to this allocation.
            if receipt.employee != employee || receipt.binding != item.binding || receipt.disabled {
                return Err(AdoptError::Inconsistent);
            }
            let spec = allocation::specification(item).map_err(|_| AdoptError::Inconsistent)?;
            return Ok(Adopted { request_id: String::new(), employee: receipt.employee, target: spec.label, self_approved: false });
        }
        Phase::Saved { request, row, target, employee, self_approved } =>
            return resume_after_save(home, env, item, receipt, &adopt, approver, (request, row, target, employee, self_approved)),
        Phase::Pending { request, row, target, self_approved } => (request, row, target, self_approved),
    };
    // A new employee id only: old-id authorities stay dead (A1–A13).
    if receipt.employee == q.employee || receipt.label != target || receipt.disabled { return Err(AdoptError::Inconsistent); }
    let resumed = request.state == "applying";
    {
        let mut db = restore::open_apply(home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        if !resumed { restore::set_request_state(&tx, &request.request_id, &["requested"], "applying", None, env.now)?; }
        tx.execute("UPDATE employee_restores SET approver_user_id=?2,self_approved=?3 WHERE request_id=?1",
            params![request.request_id, approver, self_approved]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
    }
    let request = RestoreRequest { state: "applying".into(), approver: Some(approver.to_owned()), self_approved: Some(self_approved), ..request };
    restore::audit(home, "isolation.employee_adoption_approved", &q.label, Severity::Info,
        restore::audit_details(&request, json!({"target_label": target, "resumed": resumed})));
    // A retried step starts from a clean tree.
    if resumed {
        if fsops::undo(home, &q, &target).map_err(|e| failed(home, env, &q, &request, &target, e))? {
            delete_marker(home, &target, &adopt)?;
        }
    }
    // 1. Verify in quarantine, outside every lock (M1).
    let verified = match fsops::verify(home, &q) {
        Ok(verified) => verified,
        Err(error) => {
            let mut db = restore::open_apply(home)?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
            if error == AdoptError::DigestMismatch { restore::set_seal(&tx, q.employee, q.incarnation, &["adopting"], "invalid")?; }
            restore::set_request_state(&tx, &request.request_id, &["applying"], "failed", Some(error.code()), env.now)?;
            tx.commit().map_err(unavailable)?;
            return Err(error);
        }
    };
    let fail = |error: AdoptError| failed(home, env, &q, &request, &target, error);
    let inputs = sealed_inputs(home, &q, &target).map_err(fail)?;
    if settings_from_toml(&inputs.agent_toml).map_err(fail)? != settings { return Err(fail(AdoptError::SettingsChanged)); }
    // 2. The allowlist transform.
    let dirs = fsops::open_dirs(home, &q).map_err(fail)?;
    fsops::transform(&dirs, &target, &inputs, &verified).map_err(fail)?;
    if crash_at(AdoptHook::AfterMove) { return Err(AdoptError::Unavailable); }
    let agents = home.join("agents");
    (env.hooks)(&agents.join(&target)).map_err(|()| fail(AdoptError::Unavailable))?;
    // 3. The post-transform tree passes the v2 digest; settings = proposal's.
    let snapshot = crate::handlers::employee_dirs::snapshot(home, &agents, &target).map_err(|_| fail(AdoptError::ForeignDir))?;
    if snapshot.settings().map_err(|_| fail(AdoptError::SettingsInvalid))? != settings { return Err(fail(AdoptError::SettingsChanged)); }
    // 4. Nothing label-keyed is carried (N1, S10).
    // A crash point ends the call without `fail` (no cleanup, as on process death).
    if rekey_stores(home, &q, &adopt, true).map_err(fail)? { return Err(AdoptError::Unavailable); }
    require_zero(home, &[&q.label, &target], Zero::BeforeSave).map_err(fail)?;
    // 5. Marker (R8), confirmed row + mirror, Shared bindings, commit.
    write_marker(home, &target, q.owner.as_deref().unwrap_or(""), &snapshot.digest, &adopt).map_err(fail)?;
    // N1: the owner and approval checks again, inside the mirror's users.db
    // write transaction (the refusal's code is kept for the failure record).
    let (ran, refused) = (std::cell::Cell::new(false), std::cell::Cell::new(None::<AdoptError>));
    let gate = |users: &Connection| -> Result<(), ()> {
        ran.set(true);
        recheck_commit(home, env, &q, &request, approver, self_approved, users).map_err(|error| refused.set(Some(error)))
    };
    let saved = save(&gate);
    if let Some(error) = refused.take() { return Err(fail(error)); }
    saved.map_err(|()| fail(AdoptError::Unavailable))?;
    if !ran.get() { return Err(fail(AdoptError::Inconsistent)); }
    if let Some(members) = &request.member_selection { bind_members(home, &q, &target, members).map_err(fail)?; }
    if crash_at(AdoptHook::AfterSave) { return Err(AdoptError::Unavailable); }
    let commit_gate = |users: &Connection| recheck_commit(home, env, &q, &request, approver, self_approved, users);
    finish(home, &q, &request, receipt.employee, &target, Some(&commit_gate)).map_err(fail)?;
    Ok(Adopted { request_id: request.request_id, employee: receipt.employee, target, self_approved })
}

/// F6fb item 1: the stage step again after an interrupted apply got past
/// `save()` (store status `Applying`, process death or a lost reply). The
/// live tree and the confirmed row stay; the re-key is finished (idempotent),
/// the zero count re-read, the member bindings written unless already exact,
/// then the commit, gated by [`recheck_commit`] (security round 3 N1: a
/// credential change after the interrupted stage refuses it). Never the
/// transform again.
#[allow(clippy::too_many_arguments)]
fn resume_after_save(home: &Path, env: &AdoptEnv<'_>, item: &ConfirmationItem, receipt: &allocation::Receipt, adopt: &AdoptRef,
    approver: &str, (request, q, target, employee, self_approved): (RestoreRequest, Quarantined, String, EmployeeId, bool))
    -> Result<Adopted, AdoptError> {
    // The root's answer for this allocation must be the id that was saved.
    if receipt.employee != employee || receipt.binding != item.binding || receipt.label != target || receipt.disabled {
        return Err(AdoptError::Inconsistent);
    }
    restore::audit(home, "isolation.employee_adoption_resumed", &q.label, Severity::Info,
        restore::audit_details(&request, json!({"target_label": target, "new_employee": hex::encode(employee.0)})));
    let fail = |error: AdoptError| failed(home, env, &q, &request, &target, error);
    rekey_stores(home, &q, adopt, false).map_err(fail)?;
    require_zero(home, &[&q.label, &target], Zero::AfterSave).map_err(fail)?;
    if let Some(members) = &request.member_selection { bind_members(home, &q, &target, members).map_err(fail)?; }
    if crash_at(AdoptHook::AfterSave) { return Err(AdoptError::Unavailable); }
    let commit_gate = |users: &Connection| recheck_commit(home, env, &q, &request, approver, self_approved, users);
    finish(home, &q, &request, employee, &target, Some(&commit_gate)).map_err(fail)?;
    Ok(Adopted { request_id: request.request_id, employee, target, self_approved })
}

/// `EmployeeAuthority::disable` for an adoption item, after the new
/// allocation was disabled: moved entries go back, the new directory is
/// removed, the row `needs_reseal` (§3.10 step 6); a Shared adoption's
/// member bindings are removed. Idempotent. After the commit (row `adopted`)
/// it refuses (`Inconsistent`, Critical audit) unless the new tree is still
/// exactly the marker's (F6fb item 5).
pub(crate) fn compensate_item(home: &Path, env: &AdoptEnv<'_>, item: &ConfirmationItem) -> Result<(), AdoptError> {
    let adopt = proposal_ref(item).map_err(|_| AdoptError::Inconsistent)?.ok_or(AdoptError::Inconsistent)?;
    let target = allocation::specification(item).map_err(|_| AdoptError::Inconsistent)?.label;
    let (q, request) = {
        let db = restore::open_apply(home)?;
        let q = restore::read_row(&db, adopt.employee, adopt.incarnation)?.ok_or(Refusal::NotFound)?;
        (q, request_for_proposal(&db, adopt.employee, adopt.incarnation, &proposal_key(item))?)
    };
    let Some(request) = request else { return Ok(()) };
    if !matches!(q.seal.as_str(), "adopting" | "adopted") || q.closed_by_request.as_deref() != Some(request.request_id.as_str()) {
        return Ok(());
    }
    roll_back(home, env, &q, Some(&request), Some(&target), ("failed", Some("compensated")))
}
