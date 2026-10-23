//! Which Google Chat app is a member of which space (B11).
//!
//! Sends that carry no explicit sender (approval cards, goal progress,
//! reminders, cron notices built through the generic channel factory) must go
//! out through an app that is actually in the space. Every **verified**
//! inbound event records `(space, app)` here — `shared` for the shared app,
//! the employee id for an employee's own app — and those sends pick the app
//! from it. File: `<home>/googlechat_space_apps.json`, written under the
//! cross-process file lock, bounded in size.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Marker for the shared Google Chat app.
pub const SHARED: &str = "shared";
/// Upper bound on remembered spaces (oldest-inserted names are not tracked
/// separately; past the cap new spaces are simply not recorded and fall back
/// to the shared app, the pre-B11 behaviour).
const MAX_SPACES: usize = 5_000;

/// The app a send should use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceApp {
    Shared,
    Agent(String),
}

fn path(home: &Path) -> PathBuf {
    home.join("googlechat_space_apps.json")
}

fn load(home: &Path) -> BTreeMap<String, BTreeSet<String>> {
    std::fs::read_to_string(path(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Remember that `app` (`None` = shared app) received a verified event from
/// `space`. Best-effort: a failure is logged, never blocks the message.
pub fn record(home: &Path, space: &str, app: Option<&str>) {
    let space = space.trim();
    if space.is_empty() || !space.starts_with("spaces/") {
        return;
    }
    let member = app.unwrap_or(SHARED).to_string();
    if load(home).get(space).is_some_and(|m| m.contains(&member)) {
        return;
    }
    let file = path(home);
    let result = duduclaw_core::with_file_lock(&file, || {
        let mut map = load(home);
        if !map.contains_key(space) && map.len() >= MAX_SPACES {
            return Ok(());
        }
        map.entry(space.to_string())
            .or_default()
            .insert(member.clone());
        let body = serde_json::to_string(&map).map_err(std::io::Error::other)?;
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &file)
    });
    if let Err(e) = result {
        tracing::warn!(space, "Google Chat: could not record the space's app: {e}");
    }
}

/// Pick the app for a send into `space`: the shared app when it is a member
/// (or nothing is known — the pre-B11 behaviour), else the one employee app
/// seen there (the lowest id when several are, deterministic).
pub fn app_for_space(home: &Path, space: &str) -> SpaceApp {
    pick(load(home).get(space.trim()))
}

fn pick(members: Option<&BTreeSet<String>>) -> SpaceApp {
    match members {
        None => SpaceApp::Shared,
        Some(m) if m.is_empty() || m.contains(SHARED) => SpaceApp::Shared,
        Some(m) => m
            .iter()
            .next()
            .map(|a| SpaceApp::Agent(a.clone()))
            .unwrap_or(SpaceApp::Shared),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_shared_spaces_use_the_shared_app_and_others_their_own() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert_eq!(app_for_space(h, "spaces/NEW"), SpaceApp::Shared);
        record(h, "spaces/OWN", Some("sales"));
        assert_eq!(
            app_for_space(h, "spaces/OWN"),
            SpaceApp::Agent("sales".into())
        );
        record(h, "spaces/OWN", Some("hr"));
        assert_eq!(
            app_for_space(h, "spaces/OWN"),
            SpaceApp::Agent("hr".into()),
            "lowest id"
        );
        record(h, "spaces/MIX", Some("sales"));
        record(h, "spaces/MIX", None);
        assert_eq!(app_for_space(h, "spaces/MIX"), SpaceApp::Shared);
        record(h, "not-a-space", Some("sales"));
        assert_eq!(load(h).len(), 2);
    }
}
