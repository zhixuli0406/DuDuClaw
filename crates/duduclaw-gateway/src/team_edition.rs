//! Whether team grants (a Manager calling or assigning to a report's AI
//! employee, `duduclaw_auth::org`) apply in a home.
//!
//! Reporting lines are an Enterprise feature. The Personal edition blocks
//! every `users.*` write, but an Enterprise → Personal downgrade leaves
//! `users.manager_user_id` rows behind; those must not take effect again.
//! The gateway records the edition it resolves for its home
//! ([`record`], called from `MethodHandler::resolve_edition_profile`, which
//! the boot path and the 60 s edition watcher both run), and every place
//! that builds a caller's access drops team grants unless the recorded
//! edition is Enterprise ([`team_grants_enabled`]). Nothing recorded (an
//! early message before boot finished, a CLI process, a unit test that did
//! not opt in) means no team grants: the gate only narrows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use duduclaw_core::EditionProfile;

fn editions() -> std::sync::MutexGuard<'static, HashMap<PathBuf, EditionProfile>> {
    static EDITIONS: OnceLock<Mutex<HashMap<PathBuf, EditionProfile>>> = OnceLock::new();
    EDITIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Remember the edition resolved for `home`.
pub fn record(home: &Path, edition: EditionProfile) {
    editions().insert(home.to_path_buf(), edition);
}

/// Team grants apply only when `home` was last resolved as Enterprise.
pub fn team_grants_enabled(home: &Path) -> bool {
    editions()
        .get(home)
        .is_some_and(|e| *e == EditionProfile::Enterprise)
}

/// `access` with its team grants dropped unless they apply in `home`.
pub fn gate_access(
    home: &Path,
    access: duduclaw_auth::EffectiveAccess,
) -> duduclaw_auth::EffectiveAccess {
    if access.team.is_empty() || team_grants_enabled(home) {
        access
    } else {
        duduclaw_auth::EffectiveAccess {
            team: Default::default(),
            ..access
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_recorded_enterprise_home_keeps_team_grants() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert!(!team_grants_enabled(a.path()), "nothing recorded");
        record(a.path(), EditionProfile::Enterprise);
        record(b.path(), EditionProfile::Personal);
        assert!(team_grants_enabled(a.path()));
        assert!(!team_grants_enabled(b.path()));
        record(a.path(), EditionProfile::Personal);
        assert!(!team_grants_enabled(a.path()), "a downgrade takes effect");
    }
}
