//! Snapshot boundary for the long interactive onboarding/reset wizard.
//! Test source is declared before its implementation; not compiled or run.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn hash(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Private wizard state. Capture before prompts, never rebase a replacement
/// onto a later file, and retain the old config until one final locked commit.
pub(super) struct OnboardConfigPlan {
    home: PathBuf,
    original: Option<String>,
    expected_hash: String,
}
impl OnboardConfigPlan {
    pub(super) fn capture(home: &Path) -> Result<Self, String> {
        let original = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(raw) => Some(raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Failed to read config.toml: {e}")),
        };
        let expected_hash = hash(original.as_deref().unwrap_or(""));
        Ok(Self {
            home: home.to_path_buf(),
            original,
            expected_hash,
        })
    }
    pub(super) fn existed(&self) -> bool {
        self.original.is_some()
    }
    pub(super) fn check_automatic_reset(&self, skip_prompts: bool) -> Result<(), String> {
        if skip_prompts && self.existed() {
            return Err(
                "已存在設定檔，拒絕自動覆蓋。請手動執行 `duduclaw onboard` 進行互動式重設。".into(),
            );
        }
        Ok(())
    }
    pub(super) fn backup_config(&self, directory: &Path) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(original) = &self.original {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut backup = opts.open(directory.join("config.toml"))?;
            backup.write_all(original.as_bytes())?;
        }
        Ok(())
    }
    pub(super) fn commit_rendered(
        self,
        home: &Path,
        render: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        if home != self.home.as_path() {
            return Err("onboarding snapshot target mismatch".into());
        }
        duduclaw_gateway::commit_config_rendered(home, &self.expected_hash, |current| {
            // Hashes alone cannot distinguish absence from a newly created
            // empty file. Check presence inside the same final config lock,
            // before entering the caller's renderer.
            if home
                .join("config.toml")
                .try_exists()
                .map_err(|e| e.to_string())?
                != self.existed()
            {
                return Err(
                    "config.toml changed on disk while onboarding was open; reload and retry"
                        .into(),
                );
            }
            render(current)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(home: &Path) -> String {
        std::fs::read_to_string(home.join("config.toml")).unwrap()
    }
    fn save(home: &Path, next: &str) {
        let current = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
        duduclaw_gateway::commit_config_rendered(home, &hash(&current), |_| Ok(next.into()))
            .unwrap();
    }
    #[test]
    fn actual_reset_helper_keeps_config_and_gate_on_refusal() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "[integrations]\ngoogle_workspace=false\n");
        let plan = OnboardConfigPlan::capture(d.path()).unwrap();
        let original = raw(d.path());
        let gate = d.path().join(".google-home-publication-intent.json");
        std::fs::write(&gate, b"unresolved-owned-fixture").unwrap();
        assert!(
            plan.commit_rendered(d.path(), |_| Ok("[general]\nlog_level='debug'\n".into()))
                .is_err()
        );
        assert_eq!(raw(d.path()), original);
        assert_eq!(std::fs::read(&gate).unwrap(), b"unresolved-owned-fixture");
        assert!(!d.path().join("mcp-oauth-tokens.json").exists());
        assert!(!d.path().join(".keyfile").exists());
    }
    #[test]
    fn existing_reset_snapshot_and_backup_cannot_bless_wizard_concurrent_winner() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), "# A\n[integrations]\ngoogle_workspace=false\n");
        let a = raw(d.path());
        let plan = OnboardConfigPlan::capture(d.path()).unwrap();
        save(
            d.path(),
            "# B\n[integrations]\ngoogle_workspace=false\n[secret_manager]\nbackend='fixture-only'\n",
        );
        let b = raw(d.path());
        let backup = d.path().join("owned-backup");
        std::fs::create_dir(&backup).unwrap();
        plan.backup_config(&backup).unwrap();
        assert_eq!(
            std::fs::read_to_string(backup.join("config.toml")).unwrap(),
            a
        );
        let entered = std::cell::Cell::new(false);
        assert!(
            plan.commit_rendered(d.path(), |_| {
                entered.set(true);
                Ok("[general]\nlog_level='debug'\n".into())
            })
            .is_err()
        );
        assert!(!entered.get());
        assert_eq!(raw(d.path()), b);
    }
    #[test]
    fn initially_absent_wizard_cannot_replace_new_config_even_empty_file() {
        for winner in ["", "[integrations]\ngoogle_workspace=false\n"] {
            let d = tempfile::tempdir().unwrap();
            let plan = OnboardConfigPlan::capture(d.path()).unwrap();
            assert!(!plan.existed());
            // A newly created empty file has the same byte hash as absence;
            // the helper must also compare presence under the final lock.
            std::fs::write(d.path().join("config.toml"), winner).unwrap();
            let entered = std::cell::Cell::new(false);
            assert!(
                plan.commit_rendered(d.path(), |_| {
                    entered.set(true);
                    Ok("[general]\nlog_level='debug'\n".into())
                })
                .is_err()
            );
            assert!(!entered.get());
            assert_eq!(raw(d.path()), winner);
        }
    }
    #[test]
    fn automatic_reset_refusal_and_cancel_leave_existing_files_untouched() {
        let d = tempfile::tempdir().unwrap();
        let config = "[integrations]\ngoogle_workspace=false\n";
        std::fs::write(d.path().join("config.toml"), config).unwrap();
        std::fs::write(d.path().join("inference.toml"), b"owned-inference").unwrap();
        std::fs::create_dir(d.path().join("agents")).unwrap();
        let plan = OnboardConfigPlan::capture(d.path()).unwrap();
        assert!(plan.check_automatic_reset(true).is_err());
        assert!(plan.check_automatic_reset(false).is_ok());
        drop(plan); // The actual cancel branch returns before backup/removal.
        assert_eq!(raw(d.path()), config);
        assert_eq!(
            std::fs::read(d.path().join("inference.toml")).unwrap(),
            b"owned-inference"
        );
        assert!(d.path().join("agents").is_dir());
    }
    #[test]
    fn normal_onboarding_and_requested_reset_use_actual_narrow_final_writer() {
        let d = tempfile::tempdir().unwrap();
        let first = OnboardConfigPlan::capture(d.path()).unwrap();
        assert!(first.check_automatic_reset(true).is_ok());
        first
            .commit_rendered(d.path(), |_| Ok("[general]\nlog_level='info'\n".into()))
            .unwrap();
        let reset = OnboardConfigPlan::capture(d.path()).unwrap();
        let original = raw(d.path());
        reset
            .commit_rendered(d.path(), |current| {
                assert_eq!(current, original);
                Ok("[general]\nlog_level='debug'\n".into())
            })
            .unwrap();
        assert!(raw(d.path()).contains("debug"));
        assert!(!d.path().join("mcp-oauth-tokens.json").exists());
        assert!(!d.path().join(".keyfile").exists());
    }
}
