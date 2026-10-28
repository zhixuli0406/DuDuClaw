//! Scheduled assignments: a person hands an AI employee work for later or
//! on a repeat ("every Monday 09:00, send the weekly report"), from the
//! dashboard (`tasks.create` / `tasks.goal_create` with `schedule`) or a
//! chat (`/交辦 @員工 任務 || 時間:每週一 9:00`).
//!
//! # Rights
//!
//! Creating one needs the same right as assigning now
//! (`UserContext::can_assign_to`: Admin, an Operator binding, or a team
//! grant that derived Operator), read live. **Every fire checks again** on
//! the creator's live account ([`manage::creator_right`]): an account that
//! was suspended, unbound or moved out of the team no longer gets work done
//! in its name. The fire is skipped, the schedule paused (`access_lost`),
//! the creator told on their linked channels, and an audit line written.
//! A cron expression and a per-task risk boundary are Admin-only, at
//! creation and at every fire.
//!
//! # Firing
//!
//! No new loop: the cron scheduler's 30-second tick calls [`fire::tick`]
//! (only in the process holding the gateway lock). Each due row creates an
//! ordinary task or goal (`goal_create_core`) with `on_behalf_of =
//! user:<creator>`, `created_via` the original entry point and, for chat
//! schedules, the source conversation, so progress goes back there. The
//! task id is derived from the schedule and the slot. Task insertion and
//! slot consumption share one transaction with the active-slot fence, so
//! pause/remove committed first prevents a run and a restart or a second
//! gateway never creates the same run twice. A missed slot older than [`CATCH_UP_HOURS`] is
//! skipped, not replayed. A slot whose previous run is still unfinished is
//! skipped (`previous_open`), so one schedule never piles up runs.
//!
//! # Caps
//!
//! At most `[goal_defaults] assign_schedule_max_active` (default 20) active
//! or paused schedules per creator; non-Admins fire at most every
//! `assign_schedule_min_interval_minutes` (default 60, never lower), Admins
//! every [`spec::ADMIN_MIN_INTERVAL_MINUTES`]. Fired runs do not count
//! toward the chat `/交辦` rate caps (those limit what a person types); they
//! are listed under the creator like any other assigned task.

pub mod fire;
pub mod manage;
pub mod natural;
pub mod spec;

mod audit;
mod notify;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use manage::{CreateError, Creator, ManageAction, ManageError, NewSchedule};
pub use spec::{ScheduleSpec, SpecError, Zone};

/// A missed slot older than this is skipped instead of run late.
pub const CATCH_UP_HOURS: i64 = 24;
/// Consecutive create failures after which a schedule pauses itself.
pub const MAX_CONSECUTIVE_FAILURES: i64 = 3;

pub const DEFAULT_MAX_ACTIVE: i64 = 20;
pub const DEFAULT_MIN_INTERVAL_MINUTES: i64 = 60;

/// What a scheduled assignment creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignKind {
    /// A board task (`tasks.create`).
    Task,
    /// An autonomous goal (`tasks.goal_create`, chat `/交辦`).
    Goal,
}

impl AssignKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Goal => "goal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task" => Some(Self::Task),
            "goal" => Some(Self::Goal),
            _ => None,
        }
    }
}

/// The create request every fire replays. Validated at creation (the goal
/// fields again by `goal_create_core` at each fire).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssignPayload {
    /// Board task title (tasks only).
    #[serde(default)]
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub acceptance_criteria: Option<String>,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub duration_hours: Option<f64>,
    #[serde(default)]
    pub cost_cap_cents: Option<u64>,
    /// Admin-only (it replaces the deployment baseline).
    #[serde(default)]
    pub risk_boundary: Option<String>,
    #[serde(default)]
    pub require_beliefs: bool,
    #[serde(default)]
    pub plan_first: bool,
    /// Comma-separated board tags (tasks only).
    #[serde(default)]
    pub tags: String,
}

impl AssignPayload {
    /// Whether keeping this payload needs an Admin.
    pub fn admin_only(&self) -> bool {
        self.risk_boundary
            .as_deref()
            .is_some_and(|b| !b.trim().is_empty())
    }
}

/// `(max active or paused per creator, non-Admin minimum interval in
/// minutes)` from `[goal_defaults]`. The interval can only be raised above
/// the default. Only a missing file/setting gets defaults; an existing
/// unreadable, malformed or wrongly typed setting must not widen limits.
pub fn caps(home: &Path) -> Result<(i64, i64), String> {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((DEFAULT_MAX_ACTIVE, DEFAULT_MIN_INTERVAL_MINUTES));
        }
        Err(_) => return Err("scheduling configuration unavailable".into()),
    };
    let table = text
        .parse::<toml::Table>()
        .map_err(|_| "invalid scheduling configuration".to_string())?;
    let section = table
        .get("goal_defaults")
        .map(|v| {
            v.as_table()
                .ok_or_else(|| "invalid scheduling limits".to_string())
        })
        .transpose()?;
    let read = |key: &str| -> Result<Option<i64>, String> {
        section
            .and_then(|s| s.get(key))
            .map(|v| {
                v.as_integer()
                    .ok_or_else(|| "invalid scheduling limits".to_string())
            })
            .transpose()
    };
    let max_active = read("assign_schedule_max_active")?.unwrap_or(DEFAULT_MAX_ACTIVE);
    if !(0..=200).contains(&max_active) {
        return Err("invalid scheduling limits".into());
    }
    let min_interval = read("assign_schedule_min_interval_minutes")?
        .map(|n| n.clamp(DEFAULT_MIN_INTERVAL_MINUTES, 10_080))
        .unwrap_or(DEFAULT_MIN_INTERVAL_MINUTES);
    Ok((max_active, min_interval))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_default_and_the_interval_cannot_be_lowered() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(caps(dir.path()).unwrap(), (20, 60));
        std::fs::write(
            dir.path().join("config.toml"),
            "[goal_defaults]\nassign_schedule_max_active = 5\nassign_schedule_min_interval_minutes = 1\n",
        )
        .unwrap();
        assert_eq!(caps(dir.path()).unwrap(), (5, 60));
        std::fs::write(
            dir.path().join("config.toml"),
            "[goal_defaults]\nassign_schedule_max_active = 9999\nassign_schedule_min_interval_minutes = 240\n",
        )
        .unwrap();
        assert!(
            caps(dir.path()).is_err(),
            "an out-of-range cap is not a default"
        );
    }

    #[test]
    fn an_existing_bad_configuration_never_falls_back_to_looser_limits() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            "[goal_defaults",
            "goal_defaults = false",
            "[goal_defaults]\nassign_schedule_max_active = false",
            "[goal_defaults]\nassign_schedule_min_interval_minutes = '240'",
        ] {
            std::fs::write(dir.path().join("config.toml"), text).unwrap();
            assert!(caps(dir.path()).is_err(), "{text}");
        }
        std::fs::remove_file(dir.path().join("config.toml")).unwrap();
        std::fs::create_dir(dir.path().join("config.toml")).unwrap();
        assert!(
            caps(dir.path()).is_err(),
            "an existing unreadable path is not a missing config"
        );
    }
}
