//! Which Google Chat app is a member of which space (B11).
//!
//! Sends that carry no explicit sender (approval cards, goal progress,
//! reminders, cron notices built through the generic channel factory) must go
//! out through an app that is actually in the space. Every **verified**
//! inbound event records `(space, app)` here — `shared` for the shared app,
//! the employee id for an employee's own app — and those sends pick the app
//! from it: the member that received a verified event most recently; a member
//! a send is refused for (403 / 404, the app left the space) is forgotten.
//! File: `<home>/googlechat_space_apps.json`, written under the
//! cross-process file lock, bounded in size.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Marker for the shared Google Chat app.
pub const SHARED: &str = "shared";
/// Upper bound on remembered spaces (past the cap new spaces are not
/// recorded and fall back to the shared app, the pre-B11 behaviour).
const MAX_SPACES: usize = 5_000;
/// A member's "last seen" is rewritten at most this often (seconds), so a
/// busy space does not rewrite the file on every message.
const TOUCH_INTERVAL_SECS: i64 = 600;

/// The app a send should use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceApp {
    Shared,
    Agent(String),
}

/// space → member (`shared` or employee id) → last verified event (unix s).
type Registry = BTreeMap<String, BTreeMap<String, i64>>;

fn path(home: &Path) -> PathBuf {
    home.join("googlechat_space_apps.json")
}

fn load(home: &Path) -> Registry {
    std::fs::read_to_string(path(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn update(home: &Path, change: impl FnOnce(&mut Registry) -> bool) {
    let file = path(home);
    let result = duduclaw_core::with_file_lock(&file, || {
        let mut map = load(home);
        if !change(&mut map) {
            return Ok(());
        }
        let body = serde_json::to_string(&map).map_err(std::io::Error::other)?;
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &file)
    });
    if let Err(e) = result {
        tracing::warn!("Google Chat: could not update the space → app record: {e}");
    }
}

/// Remember that `app` (`None` = shared app) received a verified event from
/// `space`. Best-effort: a failure is logged, never blocks the message.
pub fn record(home: &Path, space: &str, app: Option<&str>) {
    let space = space.trim();
    if space.is_empty() || !space.starts_with("spaces/") {
        return;
    }
    let member = app.unwrap_or(SHARED).to_string();
    let now = chrono::Utc::now().timestamp();
    let fresh = |m: &Registry| {
        m.get(space)
            .and_then(|members| members.get(&member))
            .is_some_and(|at| now - at < TOUCH_INTERVAL_SECS)
    };
    if fresh(&load(home)) {
        return;
    }
    update(home, |map| {
        if fresh(map) || (!map.contains_key(space) && map.len() >= MAX_SPACES) {
            return false;
        }
        map.entry(space.to_string())
            .or_default()
            .insert(member.clone(), now);
        true
    });
}

/// Drop `member` from `space` (a send through it was refused: the app is no
/// longer in the space).
pub fn forget(home: &Path, space: &str, member: &str) {
    update(home, |map| {
        let Some(members) = map.get_mut(space.trim()) else {
            return false;
        };
        let removed = members.remove(member).is_some();
        if members.is_empty() {
            map.remove(space.trim());
        }
        removed
    });
}

/// The app for a send into `space`: the member that received a verified
/// event most recently. `None` = nothing recorded (callers use the shared
/// app, the pre-B11 behaviour).
pub fn app_for_space(home: &Path, space: &str) -> Option<SpaceApp> {
    pick(load(home).get(space.trim()))
}

fn pick(members: Option<&BTreeMap<String, i64>>) -> Option<SpaceApp> {
    let (member, _) = members?
        .iter()
        .max_by_key(|(name, at)| (**at, std::cmp::Reverse((*name).clone())))?;
    Some(if member == SHARED {
        SpaceApp::Shared
    } else {
        SpaceApp::Agent(member.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_recent_member_wins_and_refused_members_are_forgotten() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert_eq!(app_for_space(h, "spaces/NEW"), None);
        record(h, "spaces/OWN", Some("sales"));
        assert_eq!(
            app_for_space(h, "spaces/OWN"),
            Some(SpaceApp::Agent("sales".into()))
        );
        // Shared app seen earlier, the employee app later: the employee app.
        update(h, |m| {
            m.entry("spaces/MIX".into())
                .or_default()
                .insert(SHARED.into(), 100);
            m.entry("spaces/MIX".into())
                .or_default()
                .insert("sales".into(), 200);
            true
        });
        assert_eq!(
            app_for_space(h, "spaces/MIX"),
            Some(SpaceApp::Agent("sales".into()))
        );
        forget(h, "spaces/MIX", "sales");
        assert_eq!(app_for_space(h, "spaces/MIX"), Some(SpaceApp::Shared));
        forget(h, "spaces/MIX", SHARED);
        assert_eq!(
            app_for_space(h, "spaces/MIX"),
            None,
            "empty spaces are dropped"
        );
        record(h, "not-a-space", Some("sales"));
        assert_eq!(load(h).len(), 1);
    }
}
