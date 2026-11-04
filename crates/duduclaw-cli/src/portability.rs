//! Personal-edition data portability: export / import the `~/.duduclaw/`
//! home directory as a single `.tar.gz` archive.
//!
//! The personal edition is a self-contained, single-owner deployment: all
//! state (agents, memory, config, license) lives under one directory. Because
//! a managed ("代管") personal instance is the *same artifact* as a self-hosted
//! one, exporting here and importing on another machine — or switching between
//! self-host and managed — is a lossless directory move. See
//! `docs/guides/personal-edition-portability.md`.
//!
//! Heavy / machine-specific / ephemeral entries are excluded from the archive
//! (local GGUF models, logs, prior backups) so the export stays small and
//! portable.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use unicode_normalization::UnicodeNormalization;
use duduclaw_gateway::backup_restore::RestoreLimits;

/// Top-level directory names skipped during export (heavy or ephemeral).
const EXCLUDE_DIRS: &[&str] = &["models", "logs", "target", "computer_profiles"];

/// `true` if a top-level entry name should be excluded from the export.
fn is_excluded(name: &str) -> bool {
    EXCLUDE_DIRS.contains(&name)
        || name.starts_with("backup_")
        || name.ends_with(".log")
        // never recursively pack an export sitting in the home dir
        || (name.starts_with("duduclaw-export") && name.ends_with(".tar.gz"))
}

fn io_err(ctx: &str, e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Config(format!("{ctx}: {e}"))
}

/// Export `home` into a gzip-compressed tar archive at `out`.
///
/// Archive entries are stored relative to the home root (e.g. `agents/foo/...`)
/// so unpacking into any home directory reproduces the same layout. Returns the
/// number of top-level entries written.
pub fn export_home(home: &Path, out: &Path) -> Result<usize> {
    if !home.is_dir() {
        return Err(io_err(
            "export",
            format!("home dir not found: {}", home.display()),
        ));
    }
    let file = File::create(out).map_err(|e| io_err("export create", e))?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(enc);
    builder.follow_symlinks(false);

    let mut count = 0usize;
    for entry in fs::read_dir(home).map_err(|e| io_err("export read_dir", e))? {
        let entry = entry.map_err(|e| io_err("export entry", e))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if is_excluded(&name_str) {
            continue;
        }
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| io_err("export file_type", e))?;
        if ft.is_dir() {
            builder
                .append_dir_all(&name, &path)
                .map_err(|e| io_err(&format!("export append_dir {name_str}"), e))?;
        } else if ft.is_file() {
            builder
                .append_path_with_name(&path, &name)
                .map_err(|e| io_err(&format!("export append_file {name_str}"), e))?;
        } else {
            continue; // skip symlinks / sockets / fifos
        }
        count += 1;
    }
    let enc = builder
        .into_inner()
        .map_err(|e| io_err("export finish tar", e))?;
    enc.finish().map_err(|e| io_err("export finish gz", e))?;
    Ok(count)
}

/// Import a `.tar.gz` archive (produced by [`export_home`]) into `home`.
///
/// Safety: refuses to overwrite an existing populated home (presence of an
/// `agents/` directory) unless `force` is set. When `force` is set and an
/// `agents/` dir already exists, the *entire* existing home is first moved
/// aside to `home/../<home-name>.pre-import.bak` is avoided — instead the
/// existing `agents/` is renamed to `agents.pre-import` in-place so nothing is
/// destroyed. The archive is then unpacked (the `tar` crate guards against
/// `..` path traversal).
pub fn import_archive(archive: &Path, home: &Path, force: bool, personal: bool) -> Result<()> {
    require_legacy_import(duduclaw_parse_worker::client::required())?;
    if !archive.is_file() {
        return Err(io_err(
            "import",
            format!("archive not found: {}", archive.display()),
        ));
    }
    // Create only the empty root before locking; no employee data is written.
    fs::create_dir_all(home).map_err(|e| io_err("import mkdir home", e))?;
    let _barrier = duduclaw_core::autonomy_gate::exclusive_barrier(home)
        .map_err(|error| io_err("import", error))?;
    // The Admin toggle uses this same lock. Recheck after acquiring it and
    // keep it through extraction, so a concurrent toggle cannot be undone.
    duduclaw_core::with_file_lock(&home.join("config.toml"), || {
        Ok(import_archive_locked(archive, home, force, personal))
    }).map_err(|error| io_err("import config lock", error))?
}

// Bound the compressed snapshot using the existing restore total-size ceiling (8 GiB).
const MAX_IMPORT_ARCHIVE_BYTES: u64 = duduclaw_gateway::backup_restore::DEFAULT_MAX_RESTORE_TOTAL_BYTES;

/// Apply the restore defaults in both passes, including entries skipped at unpack.
#[derive(Default)]
struct ImportBudget {
    entries: u64,
    bytes: u64,
}

impl ImportBudget {
    fn check<R: Read>(&mut self, entry: &tar::Entry<'_, R>, limits: &RestoreLimits) -> Result<()> {
        self.entries += 1;
        if self.entries > u64::from(limits.max_entries) {
            return Err(io_err("import limits", "備份項目數超過匯入安全上限"));
        }
        let size = entry.size();
        if size > limits.max_entry_bytes {
            return Err(io_err("import limits", "備份單一項目超過匯入大小上限"));
        }
        self.bytes = self.bytes.checked_add(size)
            .ok_or_else(|| io_err("import limits", "備份解壓總量超過匯入大小上限"))?;
        if self.bytes > limits.max_total_bytes {
            return Err(io_err("import limits", "備份解壓總量超過匯入大小上限"));
        }
        Ok(())
    }
}

// tar expands GNU/PAX metadata before yielding logical entries. Bound those
// physical entries first, so its internal read_all cannot bypass our budget.
const MAX_IMPORT_METADATA_BYTES: u64 = 1024 * 1024;

fn validate_import_tar_headers(snapshot: &mut File, limits: &RestoreLimits) -> Result<()> {
    require_legacy_import(duduclaw_parse_worker::client::required())?;
    snapshot.rewind().map_err(|e| io_err("import snapshot rewind", e))?;
    let mut archive = tar::Archive::new(GzDecoder::new(&mut *snapshot));
    let mut budget = ImportBudget::default();
    for entry in archive.entries().map_err(|e| io_err("import headers", e))?.raw(true) {
        let entry = entry.map_err(|e| io_err("import headers", e))?;
        budget.check(&entry, limits)?;
        let kind = entry.header().entry_type();
        if (kind.is_gnu_longname() || kind.is_gnu_longlink()
            || kind.is_pax_local_extensions() || kind.is_pax_global_extensions())
            && entry.size() > MAX_IMPORT_METADATA_BYTES {
            return Err(io_err("import limits", "備份路徑中繼資料超過匯入大小上限"));
        }
    }
    snapshot.rewind().map_err(|e| io_err("import snapshot rewind", e))
}

fn snapshot_archive(archive: &Path, home: &Path, max_bytes: u64) -> Result<File> {
    let source = File::open(archive).map_err(|e| io_err("import open", e))?;
    if !source.metadata().map_err(|e| io_err("import snapshot", e))?.is_file() {
        return Err(io_err("import snapshot", "archive must be a regular file"));
    }
    // Into_file removes the temporary name; later archive entries cannot replace
    // this private inode. Both passes read this same handle, never a pathname.
    let mut snapshot = tempfile::NamedTempFile::new_in(home)
        .map_err(|e| io_err("import snapshot", e))?.into_file();
    let copied = std::io::copy(&mut source.take(max_bytes + 1), &mut snapshot)
        .map_err(|e| io_err("import snapshot", e))?;
    if copied > max_bytes {
        return Err(io_err("import snapshot", format!("壓縮備份超過匯入大小上限 {max_bytes} bytes")));
    }
    snapshot.rewind().map_err(|e| io_err("import snapshot", e))?;
    Ok(snapshot)
}

fn import_archive_locked(archive: &Path, home: &Path, force: bool, personal: bool) -> Result<()> {
    let mut snapshot = snapshot_archive(archive, home, MAX_IMPORT_ARCHIVE_BYTES)?;
    let decision = validate_import_policy(&mut snapshot, home, personal)?;
    apply_import_snapshot(&mut snapshot, home, force, &decision, |_| Ok(()))
}

#[derive(Debug)]
struct ImportDecision {
    prepared_config: String,
    publish_before_unpack: bool,
}

fn publish_import_config(home: &Path, config: &str) -> Result<()> {
    let mut pending = tempfile::NamedTempFile::new_in(home)
        .map_err(|e| io_err("import preserve MFA policy", e))?;
    pending.write_all(config.as_bytes()).and_then(|_| pending.as_file().sync_all())
        .map_err(|e| io_err("import preserve MFA policy", e))?;
    durable_publish(pending, &home.join("config.toml"))
        .map_err(|e| io_err("import preserve MFA policy", e))?;
    Ok(())
}

fn durable_publish(pending: tempfile::NamedTempFile, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let wide = |path: &Path| -> std::io::Result<Vec<u16>> {
            let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
            if value.contains(&0) {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in config path"));
            }
            value.push(0);
            Ok(value)
        };
        // Close the flushed file before moving; TempPath still cleans up on failure.
        let source = pending.into_temp_path();
        let from = wide(&source)?;
        let to = wide(destination)?;
        // SAFETY: both paths are NUL-terminated UTF-16 buffers alive for the call.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        pending.persist(destination).map_err(|error| error.error)?;
        File::open(destination.parent().ok_or_else(|| std::io::Error::new(
            std::io::ErrorKind::InvalidInput, "config has no parent",
        ))?)?.sync_all()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pending, destination);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "durable config publication unavailable"))
    }
}

fn apply_import_snapshot(
    snapshot: &mut File,
    home: &Path,
    force: bool,
    decision: &ImportDecision,
    mut after_unpack: impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    apply_import_snapshot_with_limits(snapshot, home, force, decision, &RestoreLimits::default(), &mut after_unpack)
}

fn apply_import_snapshot_with_limits(
    snapshot: &mut File,
    home: &Path,
    force: bool,
    decision: &ImportDecision,
    limits: &RestoreLimits,
    mut after_unpack: impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    validate_import_tar_headers(snapshot, limits)?;
    let existing_agents = home.join("agents");
    let has_agents = existing_agents.exists();
    if has_agents && !force {
        return Err(io_err(
            "import",
            format!(
                "home already has agents at {} — pass --force to import (existing data is preserved as agents.pre-import)",
                existing_agents.display()
            ),
        ));
    }
    snapshot.rewind().map_err(|e| io_err("import snapshot rewind", e))?;
    let mut ar = tar::Archive::new(GzDecoder::new(snapshot));
    let entries = ar.entries().map_err(|e| io_err("import unpack", e))?;
    // Validation and every policy refusal have already completed. Publish strict
    // policy durably before moving agents or replacing any identity data.
    if decision.publish_before_unpack {
        publish_import_config(home, &decision.prepared_config)?;
    }
    if has_agents {
        let backup = home.join("agents.pre-import");
        if backup.exists() {
            fs::remove_dir_all(&backup).map_err(|e| io_err("import clear old backup", e))?;
        }
        fs::rename(&existing_agents, &backup).map_err(|e| io_err("import backup agents", e))?;
    }
    let mut targets = HashSet::new();
    let mut budget = ImportBudget::default();
    for entry in entries {
        let mut entry = entry.map_err(|e| io_err("import unpack", e))?;
        budget.check(&entry, limits)?;
        let path = checked_import_entry(&entry, &mut targets)?;
        if is_live_lock(&path) || (decision.publish_before_unpack && path == Path::new("config.toml")) {
            continue;
        }
        if !entry.unpack_in(home).map_err(|e| io_err("import unpack", e))? {
            return Err(io_err("import unpack", "archive entry escaped home"));
        }
        after_unpack(&path)?;
    }
    Ok(())
}

/// Default export path in the current working directory.
pub fn default_export_path() -> PathBuf {
    PathBuf::from("duduclaw-export.tar.gz")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(tag: &str) -> PathBuf {
        // Avoid extra dev-deps: derive a per-process unique dir. No Date needed.
        let base =
            std::env::temp_dir().join(format!("dudu_portability_{}_{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(p: &Path, contents: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    fn ordered_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let mut builder = tar::Builder::new(GzEncoder::new(File::create(path).unwrap(), Compression::default()));
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o600);
            header.set_size(data.len() as u64);
            // Preserve hostile Windows spelling in fixtures on every host.
            let bytes = name.as_bytes();
            header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn import_limits_are_enforced_in_validation_and_unpack() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let archive = root.path().join("input.tar.gz");
        ordered_archive(&archive, &[("first", b"123"), ("second", b"456")]);
        let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
        for (limits, message) in [
            (RestoreLimits { max_entries: 1, ..RestoreLimits::default() }, "項目數"),
            (RestoreLimits { max_total_bytes: 5, ..RestoreLimits::default() }, "解壓總量"),
            (RestoreLimits { max_entry_bytes: 2, ..RestoreLimits::default() }, "單一項目"),
        ] {
            let error = validate_import_policy_with_limits(&mut snapshot, &home, true, &limits).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
            assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "validation must not unpack anything");
            let unpack_home = tempfile::tempdir().unwrap();
            let decision = ImportDecision { prepared_config: String::new(), publish_before_unpack: false };
            let error = apply_import_snapshot_with_limits(&mut snapshot, unpack_home.path(), false, &decision, &limits, |_| Ok(())).unwrap_err();
            assert!(error.to_string().contains(message), "unpack: {error}");
            assert!(!unpack_home.path().join("second").exists());
        }
    }

    #[test]
    fn import_rejects_oversized_declared_entry_before_reading_body() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let archive = root.path().join("input.tar.gz");
        let mut header = tar::Header::new_gnu();
        header.set_path("huge").unwrap();
        header.set_mode(0o600);
        header.set_size(RestoreLimits::default().max_entry_bytes + 1);
        header.set_cksum();
        let mut gzip = GzEncoder::new(File::create(&archive).unwrap(), Compression::default());
        gzip.write_all(header.as_bytes()).unwrap();
        gzip.finish().unwrap();
        let error = import_archive(&archive, &home, false, true).unwrap_err();
        assert!(error.to_string().contains("單一項目"), "{error}");
        assert!(!home.join("huge").exists());
    }

    #[test]
    fn import_limits_reject_gnu_metadata_before_decoding_it() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let archive = root.path().join("input.tar.gz");
        let mut header = tar::Header::new_gnu();
        header.set_path("././@LongLink").unwrap();
        header.set_entry_type(tar::EntryType::GNULongName);
        header.set_size(MAX_IMPORT_METADATA_BYTES + 1);
        header.set_cksum();
        let mut gzip = GzEncoder::new(File::create(&archive).unwrap(), Compression::default());
        gzip.write_all(header.as_bytes()).unwrap();
        gzip.finish().unwrap();
        let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
        let error = validate_import_policy(&mut snapshot, &home, true).unwrap_err();
        assert!(error.to_string().contains("中繼資料"), "{error}");
        let decision = ImportDecision { prepared_config: String::new(), publish_before_unpack: false };
        let error = apply_import_snapshot(&mut snapshot, &home, false, &decision, |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("中繼資料"), "{error}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
    }

    #[test]
    fn import_budget_counts_pax_effective_size() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("input.tar.gz");
        let mut builder = tar::Builder::new(GzEncoder::new(File::create(&archive).unwrap(), Compression::default()));
        builder.append_pax_extensions([("size", &b"100"[..])]).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(&mut header, "payload", std::io::empty()).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let mut archive = tar::Archive::new(GzDecoder::new(File::open(&archive).unwrap()));
        let entry = archive.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(entry.header().size().unwrap(), 0);
        assert_eq!(entry.size(), 100);
        let limits = RestoreLimits { max_total_bytes: 99, ..RestoreLimits::default() };
        let error = ImportBudget::default().check(&entry, &limits).unwrap_err();
        assert!(error.to_string().contains("解壓總量"), "{error}");
    }

    #[test]
    fn export_import_round_trips_gnu_long_paths() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        let name = format!("notes/{}/{}", "a".repeat(80), "b".repeat(80));
        write(&source.join(&name), "long path");
        let archive = root.path().join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, false, true).unwrap();
        assert_eq!(fs::read(target.join(name)).unwrap(), b"long path");
    }

    #[test]
    fn import_nfc_duplicate_is_refused_but_fullwidth_is_not_nfkc_folded() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let archive = root.path().join("input.tar.gz");
        ordered_archive(&archive, &[("notes/café", b"one"), ("notes/cafe\u{301}", b"two")]);
        let error = import_archive(&archive, &home, false, true).unwrap_err();
        assert!(error.to_string().contains("duplicate archive target"), "{error}");
        assert!(!home.join("notes").exists());
        assert_eq!(canonical_import_path(Path::new("ｃｏｎｆｉｇ.toml")).unwrap(), Path::new("ｃｏｎｆｉｇ.toml"));
        ordered_archive(&archive, &[("ｃｏｎｆｉｇ.toml", b"ordinary file")]);
        import_archive(&archive, &home, false, true).unwrap();
        assert_eq!(fs::read(home.join("ｃｏｎｆｉｇ.toml")).unwrap(), b"ordinary file");
    }

    #[test]
    fn import_force_config_publication_failure_keeps_agents_in_place() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        write(&home.join("agents/kept/SOUL.md"), "keep");
        write(&home.join("agents.pre-import/older/SOUL.md"), "older");
        let archive = root.path().join("input.tar.gz");
        ordered_archive(&archive, &[("first", b"must not land")]);
        let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
        let mut decision = validate_import_policy(&mut snapshot, &home, true).unwrap();
        decision.publish_before_unpack = true;
        decision.prepared_config = "[auth]\nmfa_enforced=true\n".to_owned();
        // A directory at the destination deterministically fails atomic publication.
        fs::create_dir(home.join("config.toml")).unwrap();
        let error = apply_import_snapshot(&mut snapshot, &home, true, &decision, |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("preserve MFA policy"), "{error}");
        assert_eq!(fs::read(home.join("agents/kept/SOUL.md")).unwrap(), b"keep");
        assert_eq!(fs::read(home.join("agents.pre-import/older/SOUL.md")).unwrap(), b"older");
        assert!(!home.join("first").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_export_import_round_trips_colons_and_dos_names() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        for name in ["notes/2026-11-04T07:00", "aux/note", "CONFIG~1.TOM"] {
            write(&source.join(name), "keep");
        }
        let archive = root.path().join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, false, true).unwrap();
        for name in ["notes/2026-11-04T07:00", "aux/note", "CONFIG~1.TOM"] {
            assert_eq!(fs::read(target.join(name)).unwrap(), b"keep");
        }
    }

    #[test]
    fn import_snapshot_survives_original_archive_replacement_after_validation() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let archive = root.path().join("input.tar.gz");
        ordered_archive(&archive, &[("config.toml", b"[auth]\nmfa_enforced=true\n"), ("safe.txt", b"validated")]);
        let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = snapshot.metadata().unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 0, "snapshot must have no replaceable pathname");
        }
        let decision = validate_import_policy(&mut snapshot, &home, false).unwrap();
        let replacement = root.path().join("replacement.tar.gz");
        ordered_archive(&replacement, &[("config.toml", b"[auth]\nmfa_enforced=false\n"), ("evil.txt", b"unvalidated")]);
        fs::remove_file(&archive).unwrap();
        fs::rename(replacement, &archive).unwrap();
        apply_import_snapshot(&mut snapshot, &home, false, &decision, |_| Ok(())).unwrap();
        assert_eq!(fs::read(home.join("safe.txt")).unwrap(), b"validated");
        assert!(!home.join("evil.txt").exists());
        let config: toml::Table = fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap();
        assert_eq!(config["auth"]["mfa_enforced"].as_bool(), Some(true));
    }

    #[test]
    fn import_snapshot_copy_has_a_hard_byte_limit() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("input");
        fs::write(&archive, [0; 33]).unwrap();
        assert!(snapshot_archive(&archive, root.path(), 32).unwrap_err().to_string().contains("大小上限"));
        fs::write(&archive, [0; 32]).unwrap();
        assert_eq!(snapshot_archive(&archive, root.path(), 32).unwrap().metadata().unwrap().len(), 32);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1, "snapshot names must not survive");
    }

    #[test]
    fn import_refuses_protected_windows_aliases_before_any_data_moves() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        write(&home.join("config.toml"), "[auth]\nmfa_enforced=true\n");
        write(&home.join("agents/kept/SOUL.md"), "keep");
        let archive = root.path().join("input.tar.gz");
        for alias in ["confıg.toml", "uſers.db", "uſers.db-wal", "uſers.db-shm"] {
            ordered_archive(&archive, &[(alias, b"")]);
            assert!(import_archive(&archive, &home, true, false).is_err(), "{alias}");
            assert!(!home.join("agents.pre-import").exists());
        }
        for protected in ["config.toml", "users.db", "users.db-wal", "users.db-shm", "users.db-journal", "license.json"] {
            for alias in [format!("{protected}."), format!("{protected} "), format!("{protected}. "),
                format!("{protected}:stream"), protected.to_uppercase(), format!("{protected}./child")] {
                if !cfg!(windows) && alias.contains(':') { continue; }
                ordered_archive(&archive, &[("first.txt", b"must not land"), (&alias, b"")]);
                assert!(import_archive(&archive, &home, true, false).is_err(), "{alias}");
                assert!(!home.join("first.txt").exists(), "{alias}");
                assert!(!home.join("agents.pre-import").exists(), "{alias}");
                assert_eq!(fs::read_to_string(home.join("agents/kept/SOUL.md")).unwrap(), "keep");
            }
        }
    }

    #[test]
    fn import_refuses_dos_names_ads_and_duplicate_portable_targets() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let archive = root.path().join("input.tar.gz");
        let mut reserved: Vec<String> = ["CON", "NUL", "AUX", "PRN", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"]
            .into_iter().map(str::to_owned).collect();
        for number in 1..=9 {
            reserved.extend([format!("COM{number}"), format!("LPT{number}")]);
        }
        for name in reserved.into_iter().filter(|_| cfg!(windows)) {
            for component in [name.clone(), format!("{name}.txt"), format!("{name}. ")] {
                let path = format!("nested/{component}/file");
                ordered_archive(&archive, &[(&path, b"")]);
                assert!(import_archive(&archive, &home, false, true).is_err(), "{path}");
            }
        }
        for path in ["nested/file:stream", "nested:stream/file", "nested/.../file", "nested/   /file", "nested\\file",
            "CONFIG~1.TOM", "USERS~1.DB-", "nested/AGENTS~1/file"] {
            if !cfg!(windows) && (path.contains(':') || path.contains('~')) { continue; }
            ordered_archive(&archive, &[(path, b"")]);
            assert!(import_archive(&archive, &home, false, true).is_err(), "{path}");
        }
        for (first, second) in [("Dir/File", "dir/file"), ("dir./file ", "DIR/file"), ("a", "a."), ("nested/uſer", "nested/user")] {
            ordered_archive(&archive, &[(first, b"one"), (second, b"two")]);
            let err = import_archive(&archive, &home, false, true).unwrap_err();
            assert!(err.to_string().contains("duplicate archive target"), "{err}");
        }
    }

    #[test]
    fn import_unpack_rechecks_paths_types_and_protected_names() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let archive = root.path().join("input.tar.gz");
        let decision = ImportDecision { prepared_config: String::new(), publish_before_unpack: false };
        for name in ["config.toml.", "users.db:stream", "nested/NUL.txt"] {
            if !cfg!(windows) && name != "config.toml." { continue; }
            ordered_archive(&archive, &[(name, b"")]);
            let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
            assert!(apply_import_snapshot(&mut snapshot, &home, false, &decision, |_| Ok(())).is_err());
        }
        for kind in [tar::EntryType::Symlink, tar::EntryType::Link, tar::EntryType::Fifo] {
            let mut builder = tar::Builder::new(GzEncoder::new(File::create(&archive).unwrap(), Compression::default()));
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_link_name("config.toml").unwrap();
            header.set_cksum();
            builder.append_data(&mut header, "alias", std::io::empty()).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
            let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
            assert!(apply_import_snapshot(&mut snapshot, &home, false, &decision, |_| Ok(())).is_err());
        }
    }

    #[test]
    fn import_skips_live_lock_aliases_in_both_passes() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        write(&home.join("config.toml.lock"), "live");
        write(&home.join("locks/gateway.lock"), "live gateway");
        let archive = root.path().join("input.tar.gz");
        ordered_archive(&archive, &[("CONFIG.TOML.LOCK. ", b"archive"), ("LOCKS. /Gateway.lock ", b"archive")]);
        import_archive(&archive, &home, false, true).unwrap();
        assert_eq!(fs::read_to_string(home.join("config.toml.lock")).unwrap(), "live");
        assert_eq!(fs::read_to_string(home.join("locks/gateway.lock")).unwrap(), "live gateway");
        let home_entries: HashSet<_> = fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let expected_home_entries: HashSet<_> = [
            "config.toml.lock",
            "locks",
            duduclaw_core::autonomy_gate::ADMISSION_LOCK_FILE,
        ]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect();
        assert_eq!(home_entries, expected_home_entries);
        let locks_entries: HashSet<_> = fs::read_dir(home.join("locks"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let expected_locks_entries: HashSet<_> = ["gateway.lock"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect();
        assert_eq!(locks_entries, expected_locks_entries);
        ordered_archive(&archive, &[("lockſ/gateway.lock", b"archive")]);
        import_archive(&archive, &home, false, true).unwrap();
        assert_eq!(fs::read_to_string(home.join("locks/gateway.lock")).unwrap(), "live gateway");
        let home_entries: HashSet<_> = fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(home_entries, expected_home_entries);
        let locks_entries: HashSet<_> = fs::read_dir(home.join("locks"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(locks_entries, expected_locks_entries);
    }

    #[test]
    fn mfa_import_publishes_unchanged_strict_config_before_identity_and_survives_failure() {
        for personal in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            write(&home.join("config.toml"), "# Policy currently lives in users.db\n");
            drop(duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap());
            let mut conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
            drop(conn);
            let archived_db = root.path().join("archived.db");
            drop(duduclaw_auth::UserDb::new(&archived_db).unwrap());
            let db_bytes = fs::read(archived_db).unwrap();
            let strict = "[auth]\nmfa_enforced=true\n";
            let archive = root.path().join("input.tar.gz");
            // The identity database is deliberately before the config.
            ordered_archive(&archive, &[("users.db", &db_bytes), ("config.toml", strict.as_bytes()), ("later.txt", b"later")]);
            let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
            let decision = validate_import_policy(&mut snapshot, &home, personal).unwrap();
            assert_eq!(decision.prepared_config, strict, "regression: unchanged prepared text");
            assert!(decision.publish_before_unpack);
            let error = apply_import_snapshot(&mut snapshot, &home, false, &decision, |path| {
                if path == Path::new("users.db") { return Err(io_err("injected failure", "after users.db")); }
                Ok(())
            }).unwrap_err();
            assert!(error.to_string().contains("injected failure"));
            assert_eq!(fs::read(home.join("users.db")).unwrap(), db_bytes);
            let config: toml::Table = fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap();
            assert_eq!(config["auth"]["mfa_enforced"].as_bool(), Some(true));
            assert!(!home.join("later.txt").exists());
            let mut unpacked = Vec::new();
            apply_import_snapshot(&mut snapshot, &home, false, &decision, |path| {
                unpacked.push(path.to_owned()); Ok(())
            }).unwrap();
            assert!(!unpacked.contains(&PathBuf::from("config.toml")), "archive config must always be skipped");
        }
    }

    #[test]
    fn mfa_import_sidecar_replacement_always_prepublishes_strict_config() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        write(&home.join("config.toml"), "[auth]\nmfa_enforced=true\n");
        let archive = root.path().join("input.tar.gz");
        for name in ["users.db-wal", "users.db-shm", "users.db-journal"] {
            ordered_archive(&archive, &[(name, b"")]);
            let mut snapshot = snapshot_archive(&archive, &home, MAX_IMPORT_ARCHIVE_BYTES).unwrap();
            let decision = validate_import_policy(&mut snapshot, &home, false).unwrap();
            assert!(decision.publish_before_unpack, "{name}");
        }
    }

    #[test]
    fn excludes_heavy_and_ephemeral() {
        assert!(is_excluded("computer_profiles"));
        assert!(is_excluded("models"));
        assert!(is_excluded("logs"));
        assert!(is_excluded("backup_2026"));
        assert!(is_excluded("gateway.log"));
        assert!(is_excluded("duduclaw-export.tar.gz"));
        assert!(!is_excluded("agents"));
        assert!(!is_excluded("config.toml"));
        assert!(!is_excluded("memory.sqlite"));
        assert!(!is_excluded("license.json"));
    }

    #[test]
    fn export_import_round_trip_preserves_data_and_drops_excluded() {
        let root = unique_tmp("roundtrip");
        let home1 = root.join("home1");
        // included content
        write(&home1.join("agents/alice/SOUL.md"), "i am alice");
        write(&home1.join("config.toml"), "k = 1");
        write(
            &home1.join("license.json"),
            "{\"tier\":\"personal_pro_self_host\"}",
        );
        // excluded content
        write(&home1.join("models/big.gguf"), "HUGE");
        write(&home1.join("logs/run.log"), "noise");
        write(&home1.join("backup_old/x"), "old");

        let archive = root.join("export.tar.gz");
        let n = export_home(&home1, &archive).unwrap();
        assert!(archive.is_file());
        // 3 included top-level entries: agents/, config.toml, license.json
        assert_eq!(n, 3);

        let home2 = root.join("home2");
        import_archive(&archive, &home2, false, true).unwrap();

        assert_eq!(
            fs::read_to_string(home2.join("agents/alice/SOUL.md")).unwrap(),
            "i am alice"
        );
        assert_eq!(
            fs::read_to_string(home2.join("config.toml")).unwrap(),
            "k = 1"
        );
        assert!(home2.join("license.json").is_file());
        // excluded entries did NOT travel
        assert!(!home2.join("models").exists());
        assert!(!home2.join("logs").exists());
        assert!(!home2.join("backup_old").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn import_refuses_populated_home_without_force_then_backs_up_with_force() {
        let root = unique_tmp("force");
        let home1 = root.join("home1");
        write(&home1.join("agents/a/SOUL.md"), "new");
        let archive = root.join("e.tar.gz");
        export_home(&home1, &archive).unwrap();

        let target = root.join("target");
        write(&target.join("agents/existing/SOUL.md"), "OLD");

        // without force → refuses
        assert!(import_archive(&archive, &target, false, true).is_err());
        // existing data untouched
        assert_eq!(
            fs::read_to_string(target.join("agents/existing/SOUL.md")).unwrap(),
            "OLD"
        );

        // with force → existing agents preserved under agents.pre-import, new imported
        import_archive(&archive, &target, true, true).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("agents.pre-import/existing/SOUL.md")).unwrap(),
            "OLD"
        );
        assert_eq!(
            fs::read_to_string(target.join("agents/a/SOUL.md")).unwrap(),
            "new"
        );

        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn mfa_personal_import_succeeds_after_empty_home_restore() {
        let root = unique_tmp("personal-after-restore");
        let target = root.join("target");
        let staging = duduclaw_gateway::backup_restore::staging_dir(&target);
        write(&staging.join("config.toml"), "[auth]\nmfa_enforced=false\n");
        drop(duduclaw_auth::UserDb::new(&staging.join("users.db")).unwrap());
        let mut conn = rusqlite::Connection::open(staging.join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        drop(conn);
        duduclaw_gateway::backup_restore::write_marker(&target, &duduclaw_gateway::backup_restore::RestoreMarker {
            staged_at: chrono::Utc::now(), source_filename: "normal.tar.gz".into(),
        }).unwrap();
        duduclaw_gateway::backup_restore::perform_pending_restore_swap(&target, "personal").unwrap().unwrap();
        duduclaw_gateway::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
            &target, "127.0.0.1:18789".parse().unwrap(), true, None,
        ).unwrap();
        let config: toml::Table = fs::read_to_string(target.join("config.toml")).unwrap().parse().unwrap();
        assert_eq!(config["auth"]["mfa_enforced"].as_bool(), Some(true));
        let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
        assert!(duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
        drop(conn);
        let source = root.join("source");
        write(&source.join("config.toml"), "[gateway]\nport=18789\n");
        write(&source.join("agents/imported/SOUL.md"), "imported");
        drop(duduclaw_auth::UserDb::new(&source.join("users.db")).unwrap());
        let mut conn = rusqlite::Connection::open(source.join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, false, 456).unwrap();
        drop(conn);
        let archive = root.join("normal.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, true, true).unwrap();
        assert_eq!(fs::read_to_string(target.join("agents/imported/SOUL.md")).unwrap(), "imported");
        let config = fs::read_to_string(target.join("config.toml")).unwrap();
        let table: toml::Table = config.parse().unwrap();
        assert_eq!(table["auth"]["mfa_enforced"].as_bool(), Some(true));
        assert_eq!(table["gateway"]["port"].as_integer(), Some(18789));
        let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
        assert!(!duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
        drop(conn);
        // An ordinary config commit runs the real gateway reload_policy path.
        use sha2::{Digest, Sha256};
        let hash = hex::encode(Sha256::digest(config.as_bytes()));
        duduclaw_gateway::commit_config_rendered(&target, &hash, |current| {
            Ok(format!("{current}\n# Reload imported policy\n"))
        }).unwrap();
        let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
        assert!(duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
        drop(conn);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mfa_import_cannot_change_policy_before_moving_agents() {
        for (personal, restored) in [
            (true, "[auth]\nmfa_enforced=false\n"),
            (false, ""),
            (false, "[auth]\nmfa_enforced=false\n"),
        ] {
            let root = unique_tmp("mfa-policy");
            let source = root.join("source");
            let target = root.join("target");
            let original = "[auth]\nmfa_enforced=true\n";
            write(&target.join("config.toml"), original);
            write(&target.join("agents/kept/SOUL.md"), "keep");
            write(&source.join("agents/new/SOUL.md"), "new");
            drop(duduclaw_auth::UserDb::new(&source.join("users.db")).unwrap());
            let mut conn = rusqlite::Connection::open(source.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, false, 456).unwrap();
            drop(conn);
            drop(duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap());
            let mut conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
            drop(conn);
            write(&source.join("config.toml"), restored);
            let archive = root.join("input.tar.gz");
            export_home(&source, &archive).unwrap();
            let error = import_archive(&archive, &target, true, personal).unwrap_err();
            assert!(error.to_string().contains("system.update_config"));
            assert_eq!(fs::read_to_string(target.join("config.toml")).unwrap(), original);
            assert_eq!(fs::read_to_string(target.join("agents/kept/SOUL.md")).unwrap(), "keep");
            assert!(!target.join("agents.pre-import").exists());
            assert!(!target.join("agents/new").exists());
            let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            assert!(duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
            drop(conn);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mfa_personal_keyless_import_preserves_durable_policy_without_live_config() {
        let root = unique_tmp("personal-missing-config");
        let source = root.join("source");
        let target = root.join("target");
        write(&target.join("agents/kept/SOUL.md"), "keep");
        drop(duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap());
        let mut conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        drop(conn);
        write(&source.join("config.toml"), "[gateway]\nport=18789\n");
        drop(duduclaw_auth::UserDb::new(&source.join("users.db")).unwrap());
        let archive = root.join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, true, true).unwrap();
        let config: toml::Table = fs::read_to_string(target.join("config.toml")).unwrap().parse().unwrap();
        assert_eq!(config["auth"]["mfa_enforced"].as_bool(), Some(true));
        assert_eq!(config["gateway"]["port"].as_integer(), Some(18789));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(target.join("config.toml")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mfa_import_checks_fresh_edition_and_durable_policy_before_moving_data() {
        for (tag, personal, durable, config, restored, allowed) in [
            ("fresh-personal", true, None, "", Some("[auth]\nmfa_enforced=false\n"), true),
            ("fresh-team", false, None, "", Some("[auth]\nmfa_enforced=false\n"), false),
            ("existing-false", false, Some(false), "[auth]\nmfa_enforced=false\n", Some("[auth]\nmfa_enforced=false\n"), true),
            ("durable-true", false, Some(true), "# missing auth\n", Some("[auth]\nmfa_enforced=false\n"), false),
            ("personal-durable-true", true, Some(true), "# missing auth\n", Some("[auth]\nmfa_enforced=false\n"), false),
            ("missing-config", false, Some(true), "# missing auth\n", None, false),
            ("missing-false-key", false, Some(false), "# missing auth\n", None, false),
        ] {
            let root = unique_tmp(tag);
            let source = root.join("source");
            let target = root.join("target");
            write(&target.join("config.toml"), config);
            write(&target.join("agents/kept/SOUL.md"), "keep");
            write(&source.join("agents/new/SOUL.md"), "new");
            if let Some(restored) = restored { write(&source.join("config.toml"), restored); }
            {
                let _db = duduclaw_auth::UserDb::new(&source.join("users.db")).unwrap();
                let mut conn = rusqlite::Connection::open(source.join("users.db")).unwrap();
                duduclaw_auth::mfa::record_enforcement(&mut conn, false, 123).unwrap();
            }
            if let Some(durable) = durable {
                let _db = duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap();
                let mut conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
                duduclaw_auth::mfa::record_enforcement(&mut conn, durable, 123).unwrap();
            }
            let archive = root.join("input.tar.gz");
            export_home(&source, &archive).unwrap();
            let result = import_archive(&archive, &target, true, personal);
            assert_eq!(result.is_ok(), allowed, "{tag}: {result:?}");
            if !allowed {
                assert_eq!(fs::read_to_string(target.join("config.toml")).unwrap(), config);
                assert_eq!(fs::read_to_string(target.join("agents/kept/SOUL.md")).unwrap(), "keep");
                assert!(!target.join("agents.pre-import").exists());
                assert!(!target.join("agents/new").exists());
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mfa_agent_only_import_keeps_durable_true_without_a_config_key() {
        let root = unique_tmp("mfa-agents-only");
        let source = root.join("source");
        let target = root.join("target");
        write(&source.join("agents/new/SOUL.md"), "new");
        write(&target.join("config.toml"), "# missing auth\n");
        {
            let _db = duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap();
            let mut conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        }
        let archive = root.join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, false, false).unwrap();
        assert_eq!(fs::read_to_string(target.join("agents/new/SOUL.md")).unwrap(), "new");
        assert_eq!(fs::read_to_string(target.join("config.toml")).unwrap(), "# missing auth\n");
        {
            let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            assert!(conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get::<_, bool>(0)).unwrap());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mfa_import_refuses_unreadable_identity_store_even_with_explicit_false() {
        let root = unique_tmp("mfa-unreadable");
        let source = root.join("source");
        let target = root.join("target");
        let config = "[auth]\nmfa_enforced=false\n";
        write(&source.join("config.toml"), config);
        write(&target.join("config.toml"), config);
        write(&target.join("users.db"), "not a database");
        write(&target.join("agents/kept/SOUL.md"), "keep");
        let archive = root.join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        assert!(import_archive(&archive, &target, true, false).is_err());
        assert!(!target.join("agents.pre-import").exists());
        assert_eq!(fs::read_to_string(target.join("users.db")).unwrap(), "not a database");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mfa_import_refuses_unseeded_and_invalid_policy_before_migration() {
        for (index, rows) in ["", "(1,0),(1,0)", "(1,2)", "(1,'false')", "(1,NULL)", "(2,0)"].iter().enumerate() {
            let root = unique_tmp(&format!("mfa-invalid-policy-{index}"));
            let source = root.join("source");
            let target = root.join("target");
            let config = "[auth]\nmfa_enforced=false\n";
            write(&source.join("config.toml"), config);
            write(&source.join("agents/new/SOUL.md"), "new");
            write(&target.join("config.toml"), config);
            write(&target.join("agents/kept/SOUL.md"), "keep");
            let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            conn.execute_batch("CREATE TABLE users(id TEXT); CREATE TABLE mfa_policy_state(id, enforced);").unwrap();
            if !rows.is_empty() {
                conn.execute_batch(&format!("INSERT INTO mfa_policy_state VALUES {rows}")).unwrap();
            }
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM mfa_policy_state", [], |r| r.get(0)).unwrap();
            let archive = root.join("input.tar.gz");
            export_home(&source, &archive).unwrap();
            for personal in [false, true] {
                let error = import_archive(&archive, &target, true, personal).unwrap_err().to_string();
                assert!(error.contains("MFA policy") || error.contains("Invalid column type"), "{rows}: {error}");
                assert!(!target.join("agents.pre-import").exists());
                assert!(!target.join("agents/new").exists());
                assert_eq!(fs::read_to_string(target.join("agents/kept/SOUL.md")).unwrap(), "keep");
                assert_eq!(conn.query_row("SELECT COUNT(*) FROM mfa_policy_state", [], |r| r.get::<_, i64>(0)).unwrap(), count);
            }
            drop(conn);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mfa_imported_team_license_cannot_consume_fresh_personal_identity_signal() {
        for (index, tier, existing, restored, allowed) in [
            (0, duduclaw_license::LicenseTier::Oem, false, false, false),
            (1, duduclaw_license::LicenseTier::PersonalProSelfHost, false, false, true),
            (2, duduclaw_license::LicenseTier::Oem, true, false, true),
            (3, duduclaw_license::LicenseTier::Oem, false, true, true),
        ] {
            let root = unique_tmp(&format!("mfa-import-license-{index}"));
            let source = root.join("source");
            let target = root.join("target");
            write(&source.join("config.toml"), &format!("[auth]\nmfa_enforced={restored}\n"));
            write(&target.join("config.toml"), "[auth]\nmfa_enforced=false\n");
            write(&target.join("agents/kept/SOUL.md"), "keep");
            // Deliberately unsigned: candidate metadata can only tighten the
            // import check and never grants a live license or a Personal bypass.
            let license = duduclaw_license::License::new("sub", "customer", tier, "",
                chrono::Duration::days(30), "test");
            write(&source.join("license.json"), &serde_json::to_string(&license).unwrap());
            drop(duduclaw_auth::UserDb::new(&source.join("users.db")).unwrap());
            if existing { drop(duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap()); }
            let archive = root.join("input.tar.gz");
            export_home(&source, &archive).unwrap();
            assert_eq!(import_archive(&archive, &target, true, true).is_ok(), allowed, "case {index}");
            if !allowed {
                assert!(!target.join("users.db").exists());
                assert!(!target.join("agents.pre-import").exists());
                assert_eq!(fs::read_to_string(target.join("agents/kept/SOUL.md")).unwrap(), "keep");
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mfa_import_refuses_identity_sidecars_case_aliases_and_links_before_moving_data() {
        for (index, name, kind) in [
            (0, "users.db-wal", tar::EntryType::Regular),
            (1, "users.db-shm", tar::EntryType::Regular),
            (2, "users.db-journal", tar::EntryType::Regular),
            (3, "Users.db", tar::EntryType::Regular),
            (4, "Config.toml", tar::EntryType::Regular),
            (5, "alias", tar::EntryType::Link),
            (6, "alias", tar::EntryType::Symlink),
            (7, "License.json", tar::EntryType::Regular),
        ] {
            let root = unique_tmp(&format!("mfa-alias-{index}"));
            let target = root.join("target");
            write(&target.join("config.toml"), "# durable policy only\n");
            write(&target.join("agents/kept/SOUL.md"), "keep");
            {
                let _db = duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap();
                let mut conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
                duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
            }
            let archive = root.join("input.tar.gz");
            let mut builder = tar::Builder::new(GzEncoder::new(File::create(&archive).unwrap(), Compression::default()));
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o600);
            header.set_entry_type(kind);
            let data: &[u8] = if kind.is_file() { b"[auth]\nmfa_enforced=false\n" } else { b"" };
            header.set_size(data.len() as u64);
            if !kind.is_file() { header.set_link_name("users.db").unwrap(); }
            header.set_cksum();
            builder.append_data(&mut header, name, data).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
            assert!(import_archive(&archive, &target, true, false).is_err(), "{name} {kind:?}");
            assert!(!target.join("agents.pre-import").exists());
            assert_eq!(fs::read_to_string(target.join("agents/kept/SOUL.md")).unwrap(), "keep");
            let conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            assert!(conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get::<_, bool>(0)).unwrap());
            drop(conn);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn mfa_import_preserves_config_lock_and_rechecks_policy_in_locked_phase() {
        let root = unique_tmp("mfa-config-lock");
        let source = root.join("source");
        let target = root.join("target");
        for name in ["config.toml.lock", ".google-home-source.lock"] {
            write(&source.join(name), "archived lock");
            write(&target.join(name), "live lock");
        }
        write(&source.join("config.toml"), "[auth]\nmfa_enforced=false\n");
        write(&target.join("config.toml"), "[auth]\nmfa_enforced=false\n");
        let _db = duduclaw_auth::UserDb::new(&target.join("users.db")).unwrap();
        let archive = root.join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, true, false).unwrap();
        for name in ["config.toml.lock", ".google-home-source.lock"] {
            assert_eq!(fs::read_to_string(target.join(name)).unwrap(), "live lock");
        }
        validate_import_policy(&mut File::open(&archive).unwrap(), &target, false).unwrap();
        // Model an Admin toggle after preflight but before the final lock.
        duduclaw_core::with_file_lock(&target.join("config.toml"), || {
            fs::write(target.join("config.toml"), "[auth]\nmfa_enforced=true\n")?;
            let mut conn = rusqlite::Connection::open(target.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
            assert!(import_archive_locked(&archive, &target, true, false).is_err());
            Ok(())
        }).unwrap();
        assert!(fs::read_to_string(target.join("config.toml")).unwrap().contains("mfa_enforced=true"));
        drop(_db);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn import_preserves_the_live_admission_lock() {
        let root = unique_tmp("admission-lock");
        let source = root.join("source");
        let target = root.join("target");
        let lock = duduclaw_core::autonomy_gate::ADMISSION_LOCK_FILE;
        write(&source.join(lock), "archived lock");
        write(&source.join("agents/new/SOUL.md"), "new");
        write(&target.join(lock), "live lock");
        write(&target.join("agents/old/SOUL.md"), "old");
        let archive = root.join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        import_archive(&archive, &target, true, true).unwrap();
        assert_eq!(fs::read_to_string(target.join(lock)).unwrap(), "live lock");
        assert!(target.join("agents/new/SOUL.md").is_file());
        assert!(target.join("agents.pre-import/old/SOUL.md").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn import_cannot_remove_installation_profile_or_restore_blocked_agent() {
        let root = unique_tmp("profile-policy");
        let source = root.join("source");
        let target = root.join("target");
        write(&source.join("config.toml"), "[product]\nprofile='standard'");
        write(&target.join("config.toml"), "[product]\nprofile='restricted'");
        write(&target.join("agents/kept/SOUL.md"), "keep");
        let archive = root.join("input.tar.gz");
        export_home(&source, &archive).unwrap();
        assert!(import_archive(&archive, &target, true, true).is_err());
        assert_eq!(fs::read_to_string(target.join("agents/kept/SOUL.md")).unwrap(), "keep");
        write(&source.join("config.toml"), "[product]\nprofile='restricted'");
        write(&source.join("agents/test/agent.toml"), "[runtime]\nprovider='claude'");
        export_home(&source, &archive).unwrap();
        assert!(import_archive(&archive, &target, true, true).is_err());
        assert!(!target.join("agents.pre-import").exists());
        let _ = fs::remove_dir_all(&root);
    }
}

fn is_live_lock(path: &Path) -> bool {
    path.components().next().and_then(|part| part.as_os_str().to_str()).is_some_and(|name| {
        [duduclaw_core::autonomy_gate::ADMISSION_LOCK_FILE, "config.toml.lock", ".google-home-source.lock", "locks"]
            .iter().any(|lock| name.eq_ignore_ascii_case(lock))
    })
}

const IDENTITY_FILES: &[&str] = &["users.db", "users.db-wal", "users.db-shm", "users.db-journal"];

/// Portable comparison key, independent of the host filesystem's aliases.
fn canonical_import_path(path: &Path) -> Result<PathBuf> {
    let mut canonical = PathBuf::new();
    for component in path.components() {
        let std::path::Component::Normal(value) = component else {
            return Err(io_err("import policy", "invalid archive path"));
        };
        let name = value.to_str().ok_or_else(|| io_err("import policy", "non-UTF-8 archive path"))?;
        if name.contains('\\') || (cfg!(windows) && name.contains(':')) {
            return Err(io_err("import policy", "non-portable archive path"));
        }
        // Uppercase first also catches ASCII aliases such as dotless i and
        // long s used by Windows' ordinal uppercase comparison. Lowercase
        // keeps the comparison keys aligned with our protected-name constants.
        // NFC preserves compatibility characters (e.g. fullwidth letters); do not use NFKC.
        let name = name.trim_end_matches(['.', ' ']).nfc().collect::<String>().to_uppercase().to_lowercase();
        let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
        let device_number = stem.strip_prefix("com").or_else(|| stem.strip_prefix("lpt"));
        if name.is_empty() || (cfg!(windows) && (matches!(stem, "con" | "nul" | "aux" | "prn")
            || matches!(device_number, Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")))) {
            return Err(io_err("import policy", "reserved archive path component"));
        }
        // Existing Windows files may also be reachable through generated 8.3
        // names. Their mappings depend on the destination volume, so refuse
        // the short-name spelling instead of trying to resolve it during unpack.
        let (short_stem, extension) = name.rsplit_once('.').unwrap_or((&name, ""));
        if cfg!(windows) && short_stem.chars().count() <= 8 && !short_stem.contains('.') && extension.chars().count() <= 3
            && short_stem.rsplit_once('~').is_some_and(|(prefix, digits)|
                !prefix.is_empty() && !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())) {
            return Err(io_err("import policy", "DOS short-name archive alias"));
        }
        canonical.push(name);
    }
    if canonical.as_os_str().is_empty() {
        return Err(io_err("import policy", "empty archive path"));
    }
    Ok(canonical)
}

fn checked_import_entry<R: Read>(entry: &tar::Entry<'_, R>, targets: &mut HashSet<PathBuf>) -> Result<PathBuf> {
    // Check the wire spelling before Windows Path components consume backslashes.
    if entry.path_bytes().iter().any(|byte| *byte == b'\\' || (cfg!(windows) && *byte == b':')) {
        return Err(io_err("import policy", "non-portable archive path"));
    }
    let raw = entry.path().map_err(|e| io_err("import policy", e))?;
    let path = canonical_import_path(&raw)?;
    let kind = entry.header().entry_type();
    if !matches!(kind, tar::EntryType::Regular | tar::EntryType::Directory) {
        return Err(io_err("import policy", "invalid archive entry type"));
    }
    if !targets.insert(path.clone()) {
        return Err(io_err("import policy", "duplicate archive target"));
    }
    if is_live_lock(&path) { return Ok(path); }
    let name = path.components().next().and_then(|part| part.as_os_str().to_str()).unwrap();
    if IDENTITY_FILES.contains(&name) || matches!(name, "config.toml" | "license.json") {
        let normalized_raw: PathBuf = raw.components()
            .filter(|part| !matches!(part, std::path::Component::CurDir)).collect();
        if normalized_raw != path || path.components().count() != 1 || !kind.is_file() {
            return Err(io_err("import policy", "invalid protected file entry"));
        }
    }
    Ok(path)
}

fn validate_import_policy(snapshot: &mut File, home: &Path, personal: bool) -> Result<ImportDecision> {
    validate_import_policy_with_limits(snapshot, home, personal, &RestoreLimits::default())
}

fn validate_import_policy_with_limits(
    snapshot: &mut File, home: &Path, personal: bool, limits: &RestoreLimits,
) -> Result<ImportDecision> {
    validate_import_tar_headers(snapshot, limits)?;
    let policy = duduclaw_core::product_profile::load(home).map_err(DuDuClawError::Config)?;
    snapshot.rewind().map_err(|e| io_err("import policy", e))?;
    let mut archive = tar::Archive::new(GzDecoder::new(snapshot));
    let mut targets = HashSet::new();
    let mut budget = ImportBudget::default();
    let mut restored_config = None;
    let mut replaces_identity = false;
    let mut personal = personal;
    let mut license_seen = false;
    let fresh = matches!(fs::symlink_metadata(home.join("users.db")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    for entry in archive.entries().map_err(|e| io_err("import policy", e))? {
        let mut entry = entry.map_err(|e| io_err("import policy", e))?;
        budget.check(&entry, limits)?;
        let path = checked_import_entry(&entry, &mut targets)?;
        if is_live_lock(&path) { continue; }
        if path.to_str().is_some_and(|name| IDENTITY_FILES.contains(&name)) {
            replaces_identity = true;
        }
        if path == Path::new("license.json") {
            if license_seen || entry.size() > 1024 * 1024 {
                return Err(io_err("import policy", "duplicate or oversized license entry"));
            }
            license_seen = true;
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|e| io_err("import policy", e))?;
            if fresh && let Ok(candidate) = serde_json::from_str::<duduclaw_license::License>(&text) {
                // An imported team license must not consume the fresh signal
                // via an archived false DB. Unverified metadata may only make
                // this check stricter; live licensing still verifies signatures.
                personal &= duduclaw_core::EditionProfile::resolve_from_env(
                    None, Some(candidate.tier.as_toml_key()),
                ).is_personal();
            }
        }
        if path == Path::new("config.toml") || path.file_name().and_then(|v| v.to_str()) == Some("agent.toml") {
            if !entry.header().entry_type().is_file() || entry.size() > 1024 * 1024 {
                return Err(io_err("import policy", "invalid or oversized configuration entry"));
            }
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|e| io_err("import policy", e))?;
            let table: toml::Table = text.parse().map_err(|_| io_err("import policy", "invalid configuration"))?;
            duduclaw_gateway::product_policy::validate_table(policy, &table).map_err(DuDuClawError::Config)?;
            if path == Path::new("config.toml") {
                let before = match std::fs::read_to_string(home.join("config.toml")) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(error) => return Err(io_err("import policy", error)),
                };
                if restored_config.is_some() {
                    return Err(io_err("import policy", "duplicate configuration entry"));
                }
                restored_config = Some(text.clone());
                duduclaw_gateway::product_policy::validate_write(home, &home.join("config.toml"), &before, &text)
                    .map_err(DuDuClawError::Config)?;
            }
        }
    }
    let retained = match restored_config {
        Some(text) => text,
        None => match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(io_err("import policy", error)),
        },
    };
    // Personal may omit the key, but may never lower the persisted policy.
    let prepared = duduclaw_gateway::validate_mfa_import_policy(home, &retained, personal, replaces_identity)
        .map_err(DuDuClawError::Config)?;
    if replaces_identity && !personal {
        let table: toml::Table = retained.parse().map_err(|_| io_err("import policy", "invalid configuration"))?;
        // Without a config key, the archived database would become the policy
        // authority on reload. Require an explicit unchanged policy instead.
        if table.get("auth").and_then(|a| a.get("mfa_enforced")).is_none() {
            return Err(io_err("import policy", "匯入帳號資料庫時，設定檔必須明確保留目前的 auth.mfa_enforced；變更政策請使用 system.update_config 專用開關。"));
        }
    }
    // The unchanged MFA rule guarantees that an accepted identity replacement
    // carries true in the prepared text whenever the current policy is strict.
    let prepared_table: toml::Table = prepared.parse()
        .map_err(|_| io_err("import policy", "invalid prepared configuration"))?;
    let strict = prepared_table.get("auth").and_then(|auth| auth.get("mfa_enforced"))
        .and_then(|value| value.as_bool()) == Some(true);
    Ok(ImportDecision {
        publish_before_unpack: prepared != retained || (replaces_identity && strict),
        prepared_config: prepared,
    })
}

// Package G's importer emits bounded records for separate operator review. The
// old tar importer may never publish archive-supplied authority in protected mode.
fn require_legacy_import(isolation_required: bool) -> Result<()> {
    if isolation_required {
        Err(io_err("import isolation", "隔離模式下請使用受保護的資料匯入流程，已拒絕舊版備份解析"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod parse_isolation_tests {
    #[test]
    fn parse_isolation_portability_import_refuses() {
        assert!(super::require_legacy_import(true).is_err());
        assert!(super::require_legacy_import(false).is_ok());
    }
}
