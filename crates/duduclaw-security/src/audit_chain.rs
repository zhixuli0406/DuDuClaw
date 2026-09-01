//! Tamper-evident hash-chained JSONL audit writer (B1, OS security line P0
//! — `commercial/docs/DESIGN-os-security-line-2026-09.md` §2 支柱二).
//!
//! Generalizes the `_prev_hash` SHA-256 chain pattern pioneered by
//! `duduclaw-gateway::screenshot_audit::BrowserAuditLog` (the only prior
//! example in the codebase, and still its own independent implementation —
//! this module does not touch it, see the note at the end of this doc) into
//! a shared primitive: each JSONL line gets a `_prev_hash` field holding the
//! SHA-256 of the previous line's raw bytes, so editing any historical line
//! breaks the chain from that point forward and [`verify_chain`] can
//! pinpoint exactly which line first stopped matching.
//!
//! ## Concurrency: read-tail + write must be one critical section
//!
//! Two concurrent writers must never both read the same "last line" and
//! each compute `_prev_hash` from it — that would silently **fork** the
//! chain (both new lines individually validate, but only one is really
//! "next", and `verify_chain` cannot tell forked lines apart from a genuine
//! sequence). [`append_chained_line`] therefore holds the SAME advisory lock
//! (`duduclaw_core::platform::flock_exclusive`) the audit writers already
//! took for corruption-avoidance, but widens its scope to cover the
//! read-then-write critical section, not just the write. A flock failure is
//! warned, not fatal — matching the pre-existing convention in this
//! module's callers (best-effort concurrency safety, not a hard guarantee;
//! see `duduclaw_security::audit`'s own doc comments on the same trade-off).
//!
//! ## Rotation boundary semantics (documented choice — task requirement)
//!
//! `tool_calls.jsonl` rotates at 16 MB (`audit::maybe_rotate_tool_calls`),
//! renaming the live file to `<path>.jsonl.old` (overwriting any previous
//! backup) and starting a fresh file on the next append. Two designs were
//! considered for what the new file's first `_prev_hash` should be:
//!
//! 1. **Continue the chain in-band**: embed the old file's last-line hash as
//!    the new file's first `_prev_hash`. **Rejected.** `maybe_rotate_tool_calls`
//!    keeps only ONE backup generation — `rename` overwrites `.jsonl.old` on
//!    every rotation — so after a SECOND rotation the file whose tail hash
//!    was embedded in the first rotation is already gone. The "continuation"
//!    would become an unverifiable assertion (nothing left to check it
//!    against), which is worse than no assertion at all: it *looks*
//!    cryptographically anchored but silently stops being so.
//! 2. **Genesis + rotation event** (chosen). The new file starts a fresh
//!    chain at [`genesis_hash`], exactly like a brand-new file — so
//!    [`verify_chain`] only ever has to reason about ONE file at a time,
//!    matching the existing `screenshot_audit::verify_chain` contract (no
//!    cross-file dependency to keep alive across however many rotations
//!    happen over the file's lifetime). The rotation itself is separately
//!    recorded as an ordinary audit event (`audit_chain_rotated`, itself
//!    chained into `security_audit.jsonl`) carrying the old file's path and
//!    final-line hash — so the boundary is still cryptographically anchored
//!    and forensically discoverable, just as an auditable EVENT rather than
//!    an in-band chain link whose verifiability quietly expires. See
//!    `duduclaw_security::audit::log_audit_chain_rotated` and its call site
//!    in `maybe_rotate_tool_calls`.
//!
//! ## Backward compatibility with pre-chaining rows
//!
//! Rows written before this module shipped have no `_prev_hash` field at
//! all. [`verify_chain`] treats such a row as unchecked (nothing to verify
//! about IT), but its raw on-disk bytes still participate as the "previous
//! line" that the NEXT chained row's `_prev_hash` is verified against — so a
//! file that mixes legacy rows followed by newly-chained rows verifies
//! cleanly from the point chaining began, and a corruption of a legacy row
//! is still detectable (it breaks the very next chained row's claim).
//!
//! ## Scope
//!
//! Wired into `security_audit.jsonl` and `tool_calls.jsonl` (the two files
//! named in the task). `screenshot_audit.rs`'s `audit/browser/audit.jsonl`
//! already has its own working `_prev_hash` implementation predating this
//! module and is out of scope here — consolidating it onto this shared
//! writer is a follow-up, not required for B1's stated targets.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

use ring::digest;
use tracing::warn;

/// Sentinel `_prev_hash` for the first record in a chain — a brand-new file,
/// or the first record after a rotation boundary (see module docs). 64
/// hex-zero chars, matching the width of a real SHA-256 hex digest so a
/// genesis line is structurally indistinguishable from a real hash at parse
/// time (only its all-zero value marks it as the sentinel).
pub fn genesis_hash() -> String {
    "0".repeat(64)
}

/// SHA-256 hex digest of `data`, lower-case — same convention as
/// `soul_guard::sha256_hex` / `template_sanitizer`'s local helpers (`ring`,
/// not `sha2`+`hex`, so this module adds zero new dependencies).
fn sha256_hex(data: &[u8]) -> String {
    let d = digest::digest(&digest::SHA256, data);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Read the SHA-256 hash of the last non-empty line in `path`. Returns the
/// [`genesis_hash`] sentinel when the file does not exist or has no
/// non-empty lines.
///
/// Hashes the RAW bytes of the line exactly as stored on disk — deliberately
/// not re-serializing the parsed JSON — so a newly-chained row correctly
/// links to a legacy (pre-chaining) row's exact bytes too, and so a
/// corrupted-but-still-valid-JSON line is still caught (its raw bytes
/// changed even if reformatting would produce the same semantic content).
pub fn last_line_hash(path: &Path) -> io::Result<String> {
    if !path.exists() {
        return Ok(genesis_hash());
    }
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut last = String::new();
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            last = line;
        }
    }
    if last.is_empty() {
        return Ok(genesis_hash());
    }
    Ok(sha256_hex(last.as_bytes()))
}

/// Append `record` (a JSON object) as one JSONL line to `path`, injecting a
/// `_prev_hash` field computed from the previous line's raw bytes.
///
/// `unix_create_mode`: when `Some(mode)`, the file is created with that mode
/// (Unix only, mirrors the `tool_calls.jsonl` 0600 discipline) and tightened
/// to it on every append if an existing file's permissions drifted wider.
/// `None` leaves file creation at the process umask — unchanged from the
/// historical `security_audit.jsonl` behavior.
///
/// See the module doc for the concurrency contract (lock spans read+write).
pub fn append_chained_line(
    path: &Path,
    mut record: serde_json::Map<String, serde_json::Value>,
    unix_create_mode: Option<u32>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    if let Some(mode) = unix_create_mode {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    let file = opts.open(path)?;

    // Widen the lock to cover the read-tail step below too — see module doc
    // "Concurrency" section. Warn-not-fail matches the pre-existing
    // convention in `duduclaw_security::audit`'s writers.
    if let Err(e) = duduclaw_core::platform::flock_exclusive(&file) {
        warn!("flock failed on {}: {e}", path.display());
    }

    #[cfg(unix)]
    if let Some(mode) = unix_create_mode {
        tighten_permissions(&file, mode, path);
    }

    let prev_hash = last_line_hash(path)?;
    record.insert("_prev_hash".to_string(), serde_json::Value::String(prev_hash));
    let line = serde_json::to_string(&serde_json::Value::Object(record))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mut f = &file;
    writeln!(f, "{line}")?;
    // Lock automatically released when `file` drops at end of scope (same
    // convention as the pre-existing `audit.rs` writers — no explicit
    // unlock primitive is exposed by `duduclaw_core::platform`).
    Ok(())
}

#[cfg(unix)]
fn tighten_permissions(file: &fs::File, mode: u32, path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = file.metadata() {
        if meta.permissions().mode() & 0o077 != 0 {
            let mut perms = meta.permissions();
            perms.set_mode(mode);
            if let Err(e) = file.set_permissions(perms) {
                warn!("failed to tighten {} permissions: {e}", path.display());
            }
        }
    }
}

/// Outcome of [`verify_chain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainVerifyResult {
    /// The chain is internally consistent. `lines_checked` counts only the
    /// lines that actually carried a `_prev_hash` claim — legacy
    /// pre-chaining rows have none and are not "checked" themselves, though
    /// their raw bytes still serve as chain material for whatever chained
    /// row follows (see module docs, backward-compatibility section).
    Intact { lines_checked: usize },
    /// The first line (1-indexed, counting blank lines as the file itself
    /// does) whose declared `_prev_hash` does not match the SHA-256 of the
    /// immediately preceding non-empty line's raw bytes.
    Broken { line_number: usize },
}

impl ChainVerifyResult {
    pub fn is_intact(&self) -> bool {
        matches!(self, ChainVerifyResult::Intact { .. })
    }
}

/// Verify the hash-chain integrity of a JSONL audit file.
///
/// Missing file ⇒ trivially `Intact { lines_checked: 0 }`. A line without a
/// `_prev_hash` field (legacy row, or a line that isn't even valid JSON) is
/// not itself checked, but its raw bytes still participate as the "previous
/// line" a later chained row's `_prev_hash` is verified against — see module
/// docs. This is what makes a corruption that breaks a line's own JSON
/// syntax still detectable: the corruption changes that line's raw bytes,
/// so the NEXT chained line's `_prev_hash` claim (computed against the
/// pre-corruption bytes) no longer matches.
pub fn verify_chain(path: &Path) -> io::Result<ChainVerifyResult> {
    if !path.exists() {
        return Ok(ChainVerifyResult::Intact { lines_checked: 0 });
    }
    let content = fs::read_to_string(path)?;

    let mut prev_raw: Option<String> = None;
    let mut checked = 0usize;

    for (line_index, line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(claimed) = v.get("_prev_hash").and_then(|x| x.as_str()) {
                let expected = match &prev_raw {
                    None => genesis_hash(),
                    Some(p) => sha256_hex(p.as_bytes()),
                };
                if claimed != expected {
                    return Ok(ChainVerifyResult::Broken { line_number });
                }
                checked += 1;
            }
        }

        prev_raw = Some(line.to_string());
    }

    Ok(ChainVerifyResult::Intact { lines_checked: checked })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_home() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "dudu-audit-chain-{}-{}-{nanos}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn obj(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), serde_json::Value::String((*v).to_string()));
        }
        m
    }

    #[test]
    fn genesis_hash_is_64_hex_zero_chars() {
        let g = genesis_hash();
        assert_eq!(g.len(), 64);
        assert!(g.chars().all(|c| c == '0'));
    }

    #[test]
    fn first_line_chains_from_genesis() {
        let home = fresh_home();
        let path = home.join("test.jsonl");
        append_chained_line(&path, obj(&[("a", "1")]), None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let rec: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(rec["_prev_hash"], genesis_hash());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn chain_correctness_across_multiple_appends() {
        let home = fresh_home();
        let path = home.join("test.jsonl");
        for i in 0..5 {
            append_chained_line(&path, obj(&[("seq", &i.to_string())]), None).unwrap();
        }
        let result = verify_chain(&path).unwrap();
        assert_eq!(result, ChainVerifyResult::Intact { lines_checked: 5 });

        // Each line's `_prev_hash` really is the sha256 of the previous raw
        // line, not just "present".
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        for i in 1..lines.len() {
            let rec: serde_json::Value = serde_json::from_str(lines[i]).unwrap();
            assert_eq!(rec["_prev_hash"], sha256_hex(lines[i - 1].as_bytes()));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tampering_a_middle_line_breaks_the_chain_at_the_next_line() {
        let home = fresh_home();
        let path = home.join("test.jsonl");
        for i in 0..4 {
            append_chained_line(&path, obj(&[("seq", &i.to_string())]), None).unwrap();
        }
        assert!(verify_chain(&path).unwrap().is_intact());

        // Corrupt line 2 (1-indexed) in place — same byte length swap so
        // this is purely a content tamper, not a line-count change.
        let body = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = body.lines().map(String::from).collect();
        assert!(lines[1].contains("\"seq\":\"1\""));
        lines[1] = lines[1].replace("\"seq\":\"1\"", "\"seq\":\"9\"");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let result = verify_chain(&path).unwrap();
        // Line 2 was tampered; line 3's `_prev_hash` claim (computed at
        // write time against the ORIGINAL line 2) no longer matches.
        assert_eq!(result, ChainVerifyResult::Broken { line_number: 3 });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tampering_the_prev_hash_field_itself_is_also_detected() {
        let home = fresh_home();
        let path = home.join("test.jsonl");
        append_chained_line(&path, obj(&[("a", "1")]), None).unwrap();
        append_chained_line(&path, obj(&[("a", "2")]), None).unwrap();
        assert!(verify_chain(&path).unwrap().is_intact());

        let body = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = body.lines().map(String::from).collect();
        // Flip the first character of line 2's own `_prev_hash` claim —
        // computed generically (the real hash value is unpredictable) so
        // this doesn't assume anything about what character it starts with.
        let mut rec: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        let original = rec["_prev_hash"].as_str().unwrap().to_string();
        let first = original.chars().next().unwrap();
        let replacement = if first == '0' { '1' } else { '0' };
        let mut corrupted = original.clone();
        corrupted.replace_range(0..1, &replacement.to_string());
        assert_ne!(original, corrupted);
        rec["_prev_hash"] = serde_json::Value::String(corrupted);
        lines[1] = serde_json::to_string(&rec).unwrap();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let result = verify_chain(&path).unwrap();
        assert_eq!(result, ChainVerifyResult::Broken { line_number: 2 });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rotation_boundary_new_file_restarts_at_genesis_and_verifies_independently() {
        let home = fresh_home();
        let path = home.join("test.jsonl");
        append_chained_line(&path, obj(&[("a", "1")]), None).unwrap();
        append_chained_line(&path, obj(&[("a", "2")]), None).unwrap();
        let old_tail = last_line_hash(&path).unwrap();

        // Simulate the rotation `maybe_rotate_tool_calls` performs: rename
        // the live file away, then append to the (now-fresh) path.
        let backup = path.with_extension("jsonl.old");
        std::fs::rename(&path, &backup).unwrap();
        append_chained_line(&path, obj(&[("a", "3-post-rotation")]), None).unwrap();

        // New file: genesis-based, verifies on its own — no dependency on
        // the old file still existing.
        let new_result = verify_chain(&path).unwrap();
        assert_eq!(new_result, ChainVerifyResult::Intact { lines_checked: 1 });
        let new_body = std::fs::read_to_string(&path).unwrap();
        let new_rec: serde_json::Value = serde_json::from_str(new_body.lines().next().unwrap()).unwrap();
        assert_eq!(new_rec["_prev_hash"], genesis_hash());

        // Old file, independently, still verifies too — and its own tail
        // hash is exactly what a rotation event would have recorded as the
        // cryptographic anchor between the two files.
        let old_result = verify_chain(&backup).unwrap();
        assert!(old_result.is_intact());
        assert_eq!(old_tail.len(), 64);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn mixed_legacy_and_chained_rows_verify_cleanly() {
        let home = fresh_home();
        let path = home.join("test.jsonl");

        // Two "legacy" rows written directly, bypassing append_chained_line
        // entirely — no `_prev_hash` field at all, exactly like a pre-B1
        // on-disk file.
        std::fs::write(
            &path,
            "{\"event\":\"legacy-1\"}\n{\"event\":\"legacy-2\"}\n",
        )
        .unwrap();

        // Now the upgraded binary starts chaining.
        append_chained_line(&path, obj(&[("event", "chained-1")]), None).unwrap();
        append_chained_line(&path, obj(&[("event", "chained-2")]), None).unwrap();

        let result = verify_chain(&path).unwrap();
        // Only the two chained rows carry a checkable claim.
        assert_eq!(result, ChainVerifyResult::Intact { lines_checked: 2 });

        // The first chained row's `_prev_hash` really does point at the
        // last LEGACY row's raw bytes.
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        let first_chained: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(first_chained["_prev_hash"], sha256_hex(lines[1].as_bytes()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_file_verifies_as_trivially_intact() {
        let home = fresh_home();
        let path = home.join("does-not-exist.jsonl");
        assert_eq!(verify_chain(&path).unwrap(), ChainVerifyResult::Intact { lines_checked: 0 });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unix_create_mode_creates_and_tightens_permissions() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let home = fresh_home();
            let path = home.join("test.jsonl");
            append_chained_line(&path, obj(&[("a", "1")]), Some(0o600)).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);

            // Existing file drifted wider — next append tightens it back.
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&path, perms).unwrap();
            append_chained_line(&path, obj(&[("a", "2")]), Some(0o600)).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let _ = std::fs::remove_dir_all(&home);
        }
    }

    #[test]
    fn concurrent_appends_never_fork_the_chain() {
        // Exercises the flock-widened critical section: many threads append
        // "simultaneously"; the result must be exactly N lines with an
        // unbroken, unforked chain (never lost writes, never two lines
        // legitimately claiming the same `_prev_hash`).
        let home = fresh_home();
        let path = home.join("test.jsonl");
        let threads = 8;
        let per_thread = 15;

        std::thread::scope(|scope| {
            for t in 0..threads {
                let path = path.clone();
                scope.spawn(move || {
                    for i in 0..per_thread {
                        append_chained_line(
                            &path,
                            obj(&[("thread", &t.to_string()), ("seq", &i.to_string())]),
                            None,
                        )
                        .unwrap();
                    }
                });
            }
        });

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), threads * per_thread);

        let result = verify_chain(&path).unwrap();
        assert_eq!(
            result,
            ChainVerifyResult::Intact { lines_checked: threads * per_thread },
            "concurrent writers must never fork the chain"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
