//! Installer-time account landing (WP3, `DESIGN-installer-settings-integration-2026-08.md` §4).
//!
//! The live installer collects the operator's account name/password into
//! `LiveInstallState`, then writes it onto the TARGET disk as
//! `<DUDUCLAW_HOME>/pending-account.json` (plaintext, deliberately
//! short-lived — design doc §4.3/§8 risk #1). No gateway process runs
//! during installation at all (the live image "carries no gateway
//! payload", design doc §4 "否決的替代案"), so the account can only be
//! materialized into [`UserDb`] the first time the TARGET system's own
//! gateway boots. This module is that landing step.
//!
//! Deliberately reuses the exact same [`UserDb::claim_default_admin`] gate
//! `handle_first_run_claim` (`server.rs`) already exposes over the loopback
//! HTTP API — the design doc's own §4 mandate is "既有… 不手刻 SQLite", and
//! this is that: no schema knowledge lives here beyond calling the one
//! method that already owns it.

use std::path::Path;

use duduclaw_auth::UserDb;
use tracing::{info, warn};

const PENDING_ACCOUNT_FILE: &str = "pending-account.json";

/// Mirrors `handle_first_run_claim`'s own minimum (`server.rs`). The
/// installer already enforces this on the UI side — this is a drift guard
/// against a future installer bug, not the primary gate.
const MIN_PASSWORD_CHARS: usize = 8;

#[derive(serde::Deserialize)]
struct PendingAccount {
    password: String,
}

/// Land an installer-written `pending-account.json`, if one exists, into the
/// user database, then remove the plaintext file. Call once at gateway
/// startup, right after `ensure_default_admin()`.
///
/// Never allowed to fail gateway startup: every error path is logged and
/// swallowed. A boot-blocking failure here would be worse than a missed
/// account claim — the operator can still recover through the dashboard's
/// own first-run claim flow as long as the row stays unclaimed.
pub(crate) fn land_pending_account(user_db: &UserDb, home_dir: &Path) {
    let path = home_dir.join(PENDING_ACCOUNT_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            // Unexpected (permissions, etc). Leave the file — a transient
            // read failure is worth retrying on the next boot, unlike a
            // parse failure below (which would never succeed no matter how
            // many times it's retried).
            warn!(
                error = %e,
                "failed to read pending-account.json — will retry next boot"
            );
            return;
        }
    };

    let pending: PendingAccount = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            // A retry can never succeed against the same malformed bytes,
            // and a residual file that looks like it might hold a plaintext
            // password is not worth leaving on disk. Discard.
            warn!(
                error = %e,
                "pending-account.json is malformed — discarding (retry would never succeed)"
            );
            remove_pending_file(&path);
            return;
        }
    };

    if pending.password.chars().count() < MIN_PASSWORD_CHARS {
        warn!("pending-account.json password is below the minimum length — discarding");
        remove_pending_file(&path);
        return;
    }

    match user_db.claim_default_admin(&pending.password) {
        Ok(true) => {
            info!("installer pending account landed");
            let _ = user_db.log_action(None, "first_run_claim_pending_file", None, None, None);
            // The plaintext password has done its one job — never leave it
            // on disk (design doc §4.3).
            remove_pending_file(&path);
        }
        Ok(false) => {
            // Already claimed (e.g. a stale file surviving a prior
            // successful landing, or a manual claim via the dashboard
            // before this boot) — nothing left to do, retrying is pointless.
            warn!("pending-account.json found but instance is already claimed — discarding");
            remove_pending_file(&path);
        }
        Err(e) => {
            // Genuinely retryable (e.g. a transient DB error) — keep the
            // file so the next boot tries again (design doc §4.3).
            warn!(
                error = %e,
                "failed to claim default admin from pending-account.json — will retry next boot"
            );
        }
    }
}

fn remove_pending_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(
                error = %e,
                path = %path.display(),
                "failed to remove pending-account.json"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_db(home: &Path) -> UserDb {
        let db = UserDb::new(&home.join("users.db")).expect("open UserDb");
        db.ensure_default_admin().expect("ensure default admin");
        db
    }

    #[test]
    fn no_pending_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path());
        // Must not panic.
        land_pending_account(&db, dir.path());
        assert!(db.is_unclaimed_default_admin());
    }

    #[test]
    fn valid_pending_file_lands_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path());
        let pending_path = dir.path().join(PENDING_ACCOUNT_FILE);
        std::fs::write(&pending_path, r#"{"password":"correct-horse-battery-staple"}"#).unwrap();

        land_pending_account(&db, dir.path());

        assert!(!db.is_unclaimed_default_admin());
        assert!(!pending_path.exists());
    }

    #[test]
    fn malformed_json_is_discarded_and_admin_stays_unclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path());
        let pending_path = dir.path().join(PENDING_ACCOUNT_FILE);
        std::fs::write(&pending_path, "{ not json").unwrap();

        land_pending_account(&db, dir.path());

        assert!(db.is_unclaimed_default_admin());
        assert!(!pending_path.exists());
    }

    #[test]
    fn short_password_is_discarded_and_admin_stays_unclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path());
        let pending_path = dir.path().join(PENDING_ACCOUNT_FILE);
        std::fs::write(&pending_path, r#"{"password":"short"}"#).unwrap();

        land_pending_account(&db, dir.path());

        assert!(db.is_unclaimed_default_admin());
        assert!(!pending_path.exists());
    }

    #[test]
    fn already_claimed_admin_discards_file_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path());
        db.claim_default_admin("already-claimed-password")
            .expect("claim");
        let pending_path = dir.path().join(PENDING_ACCOUNT_FILE);
        std::fs::write(&pending_path, r#"{"password":"another-password-12"}"#).unwrap();

        land_pending_account(&db, dir.path());

        assert!(!db.is_unclaimed_default_admin());
        assert!(!pending_path.exists());
    }
}
