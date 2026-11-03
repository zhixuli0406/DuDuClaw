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
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

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
    if !archive.is_file() {
        return Err(io_err(
            "import",
            format!("archive not found: {}", archive.display()),
        ));
    }
    validate_import_policy(archive, home, personal)?;
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

fn import_archive_locked(archive: &Path, home: &Path, force: bool, personal: bool) -> Result<()> {
    let prepared_config = validate_import_policy(archive, home, personal)?;
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

    // Persist the carried policy before an archived users.db can replace it.
    if let Some(config) = &prepared_config {
        use std::io::Write;
        // NamedTempFile creates owner-only files on Unix; publish atomically
        // so an interrupted write cannot discard the live policy or secrets.
        let mut pending = tempfile::NamedTempFile::new_in(home)
            .map_err(|e| io_err("import preserve MFA policy", e))?;
        pending.write_all(config.as_bytes()).and_then(|_| pending.as_file().sync_all())
            .map_err(|e| io_err("import preserve MFA policy", e))?;
        pending.persist(home.join("config.toml"))
            .map_err(|e| io_err("import preserve MFA policy", e))?;
    }
    let file = File::open(archive).map_err(|e| io_err("import open", e))?;
    let dec = GzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    for entry in ar.entries().map_err(|e| io_err("import unpack", e))? {
        let mut entry = entry.map_err(|e| io_err("import unpack", e))?;
        let path: PathBuf = entry.path().map_err(|e| io_err("import unpack", e))?
            .components()
            .filter(|component| !matches!(component, std::path::Component::CurDir))
            .collect();
        // Restoring a lock file would replace the inode carrying our lease.
        if is_live_lock(&path) || (prepared_config.is_some() && path == Path::new("config.toml")) {
            continue;
        }
        entry.unpack_in(home).map_err(|e| io_err("import unpack", e))?;
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
        validate_import_policy(&archive, &target, false).unwrap();
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
        [duduclaw_core::autonomy_gate::ADMISSION_LOCK_FILE, "config.toml.lock", ".google-home-source.lock"]
            .iter().any(|lock| name.eq_ignore_ascii_case(lock))
    })
}

const IDENTITY_FILES: &[&str] = &["users.db", "users.db-wal", "users.db-shm", "users.db-journal"];

fn validate_import_policy(archive: &Path, home: &Path, personal: bool) -> Result<Option<String>> {
    use std::io::Read;
    let policy = duduclaw_core::product_profile::load(home).map_err(DuDuClawError::Config)?;
    let file = File::open(archive).map_err(|e| io_err("import policy", e))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut restored_config = None;
    let mut replaces_identity = false;
    let mut personal = personal;
    let mut license_seen = false;
    let fresh = matches!(fs::symlink_metadata(home.join("users.db")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    for entry in archive.entries().map_err(|e| io_err("import policy", e))? {
        let mut entry = entry.map_err(|e| io_err("import policy", e))?;
        let path: PathBuf = entry.path().map_err(|e| io_err("import policy", e))?.components()
            .filter(|component| !matches!(component, std::path::Component::CurDir)).collect();
        // Export emits only regular files/directories. Links and special files
        // could alias a protected config/database under an unrelated name.
        let kind = entry.header().entry_type();
        if (!kind.is_file() && !kind.is_dir()) || path.components().any(|part|
            !matches!(part, std::path::Component::Normal(_))) {
            return Err(io_err("import policy", "invalid archive entry"));
        }
        if is_live_lock(&path) { continue; }
        if let Some(name) = path.components().next().and_then(|part| part.as_os_str().to_str()) {
            for protected in IDENTITY_FILES.iter().copied().chain(["config.toml", "license.json"]) {
                if name.eq_ignore_ascii_case(protected) {
                    if name != protected || path.components().count() != 1 || !kind.is_file() {
                        return Err(io_err("import policy", "invalid protected file entry"));
                    }
                    if IDENTITY_FILES.contains(&protected) { replaces_identity = true; }
                }
            }
        }
        if path == Path::new("license.json") {
            if license_seen || entry.header().size().unwrap_or(u64::MAX) > 1024 * 1024 {
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
            if !entry.header().entry_type().is_file() || entry.header().size().unwrap_or(u64::MAX) > 1024 * 1024 {
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
    Ok((prepared != retained).then_some(prepared))
}
