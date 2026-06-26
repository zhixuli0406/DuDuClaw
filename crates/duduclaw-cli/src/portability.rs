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

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use flate2::Compression;

/// Top-level directory names skipped during export (heavy or ephemeral).
const EXCLUDE_DIRS: &[&str] = &["models", "logs", "target"];

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
        return Err(io_err("export", format!("home dir not found: {}", home.display())));
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
        let ft = entry.file_type().map_err(|e| io_err("export file_type", e))?;
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
    let enc = builder.into_inner().map_err(|e| io_err("export finish tar", e))?;
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
pub fn import_archive(archive: &Path, home: &Path, force: bool) -> Result<()> {
    if !archive.is_file() {
        return Err(io_err("import", format!("archive not found: {}", archive.display())));
    }
    let existing_agents = home.join("agents");
    if existing_agents.exists() {
        if !force {
            return Err(io_err(
                "import",
                format!(
                    "home already has agents at {} — pass --force to import (existing data is preserved as agents.pre-import)",
                    existing_agents.display()
                ),
            ));
        }
        let backup = home.join("agents.pre-import");
        if backup.exists() {
            fs::remove_dir_all(&backup).map_err(|e| io_err("import clear old backup", e))?;
        }
        fs::rename(&existing_agents, &backup).map_err(|e| io_err("import backup agents", e))?;
    }
    fs::create_dir_all(home).map_err(|e| io_err("import mkdir home", e))?;

    let file = File::open(archive).map_err(|e| io_err("import open", e))?;
    let dec = GzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    ar.unpack(home).map_err(|e| io_err("import unpack", e))?;
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
        let base = std::env::temp_dir().join(format!(
            "dudu_portability_{}_{}",
            std::process::id(),
            tag
        ));
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

    #[test]
    fn excludes_heavy_and_ephemeral() {
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
        write(&home1.join("license.json"), "{\"tier\":\"personal_pro_self_host\"}");
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
        import_archive(&archive, &home2, false).unwrap();

        assert_eq!(fs::read_to_string(home2.join("agents/alice/SOUL.md")).unwrap(), "i am alice");
        assert_eq!(fs::read_to_string(home2.join("config.toml")).unwrap(), "k = 1");
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
        assert!(import_archive(&archive, &target, false).is_err());
        // existing data untouched
        assert_eq!(fs::read_to_string(target.join("agents/existing/SOUL.md")).unwrap(), "OLD");

        // with force → existing agents preserved under agents.pre-import, new imported
        import_archive(&archive, &target, true).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("agents.pre-import/existing/SOUL.md")).unwrap(),
            "OLD"
        );
        assert_eq!(fs::read_to_string(target.join("agents/a/SOUL.md")).unwrap(), "new");

        let _ = fs::remove_dir_all(&root);
    }
}
