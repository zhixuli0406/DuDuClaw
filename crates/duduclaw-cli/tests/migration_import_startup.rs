//! A refused import boot must not reach ordinary CLI initialization.
#![cfg(target_os = "linux")]
use std::process::Command;

#[test]
fn migration_import_refuses_before_logging_or_configuration() {
    for args in [vec!["run", "--yes"], vec!["doctor"]] {
        let home = tempfile::tempdir().unwrap();
        let canary = b"legacy database must not be opened";
        std::fs::write(home.path().join("users.db"), canary).unwrap();
        // A malformed explicit import request must never fall back to a normal
        // run or another command, even before the logger creates logs/.
        let output = Command::new(env!("CARGO_BIN_EXE_duduclaw"))
            .args(args)
            .env("DUDUCLAW_HOME", home.path())
            .env("DUDUCLAW_MIGRATION_IMPORT_ONLY", "invalid")
            .output().unwrap();
        assert!(!output.status.success());
        assert_eq!(std::fs::read(home.path().join("users.db")).unwrap(), canary);
        assert!(!home.path().join("logs").exists());
        assert!(!home.path().join("config.toml").exists());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("migration import startup refused"));
    }
}
