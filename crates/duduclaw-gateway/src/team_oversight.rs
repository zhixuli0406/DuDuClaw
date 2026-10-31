//! Live, read-only reporting-line authority for team history and screenshots.

use chrono::{DateTime, Utc};
use duduclaw_auth::{OverseenReport, UserContext, UserDb, UserRole};
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub enum OversightDenied {
    Edition,
    SettingOff,
    NotManager,
    PasswordChange,
    NotInTeam,
    ReadError,
}

pub struct TeamOversight {
    pub viewer_user_id: String,
    pub agent: String,
    pub reports: BTreeMap<String, OverseenReport>,
    pub since: DateTime<Utc>,
    pub generation: u64,
}

/// A missing/unreadable/malformed file is off. A valid config without the
/// key uses the enterprise default. Wrong table/key types never widen it.
pub fn configured(home: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(home.join("config.toml")) else {
        return false;
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return false;
    };
    match table.get("people") {
        None => true,
        Some(toml::Value::Table(people)) => match people.get("manager_team_history") {
            None => true,
            Some(toml::Value::Boolean(on)) => *on,
            _ => false,
        },
        _ => false,
    }
}

/// Boot, edition watcher, settings writes and every read share this durable
/// activation fence. Its strict audit must land before enabling a window.
pub fn observe_window(
    home: &Path,
    db: &UserDb,
    actor: &str,
) -> Result<duduclaw_auth::team_history::HistoryWindow, String> {
    // Failed observations stay latched in this process even if the database
    // could not persist the reset marker. Recovery must first durably record
    // an off state and then activate a fresh window.
    static OBSERVE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>>,
    > = std::sync::OnceLock::new();
    let mut failed = OBSERVE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "team history observation lock poisoned")?;
    let enabled = crate::team_edition::team_grants_enabled(home) && configured(home);
    let result = (|| {
        if !enabled || failed.contains(home) {
            db.require_team_history_reset()?;
        }
        if db.team_history_reset_required()? {
            db.observe_team_history(false, Utc::now().timestamp_micros(), |old, new, since| {
                try_append_audit_event(
                    home,
                    &AuditEvent::new(
                        "team_history_setting_changed",
                        "",
                        Severity::Warning,
                        serde_json::json!({"actor":actor,"old":old,"new":new,"since_micros":since}),
                    ),
                )
            })?;
            db.clear_team_history_reset()?;
        }
        db.observe_team_history(enabled, Utc::now().timestamp_micros(), |old, new, since| {
        try_append_audit_event(
            home,
            &AuditEvent::new(
                "team_history_setting_changed",
                "",
                Severity::Warning,
                serde_json::json!({"actor": actor, "old": old, "new": new, "since_micros": since}),
            ),
        )
    })
    })();
    match result {
        Ok(window) => {
            failed.remove(home);
            Ok(window)
        }
        Err(error) => {
            failed.insert(home.to_path_buf());
            let _ = db.require_team_history_reset();
            Err(error)
        }
    }
}

pub fn manager_may_read(home: &Path, db: &UserDb, ctx: &UserContext, agent: &str) -> bool {
    if !crate::team_edition::team_grants_enabled(home) || !configured(home) {
        return false;
    }
    let Ok(Some(live)) = crate::machine_employee::live_user_context(home, db, &ctx.user_id) else {
        return false;
    };
    if !live
        .agent_access
        .get(agent)
        .is_some_and(|l| l.at_least(duduclaw_auth::AccessLevel::Operator))
    {
        return false;
    }
    let Ok(Some(user)) = db.get_user(&ctx.user_id) else {
        return false;
    };
    let mut parent = user.manager_user_id;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..duduclaw_auth::MAX_ORG_DEPTH {
        let Some(id) = parent else { return false };
        if !seen.insert(id.clone()) {
            return false;
        }
        let Ok(Some(user)) = db.get_user(&id) else {
            return false;
        };
        if user.status == duduclaw_auth::UserStatus::Offboarded {
            return false;
        }
        if user.status == duduclaw_auth::UserStatus::Active && user.role.at_least(UserRole::Manager)
        {
            return true;
        }
        parent = user.manager_user_id;
    }
    false
}

pub fn can_view_team_history(
    home: &Path,
    db: &UserDb,
    ctx: &UserContext,
    agent: &str,
) -> Result<TeamOversight, OversightDenied> {
    let window = observe_window(home, db, "gateway").map_err(|_| OversightDenied::ReadError)?;
    if !crate::team_edition::team_grants_enabled(home) {
        return Err(OversightDenied::Edition);
    }
    if !window.enabled {
        return Err(OversightDenied::SettingOff);
    }
    if ctx.user_id == "system" {
        return Err(OversightDenied::NotManager);
    }
    let live = crate::machine_employee::live_user_context(home, db, &ctx.user_id)
        .map_err(|_| OversightDenied::ReadError)?
        .ok_or(OversightDenied::NotManager)?;
    if !live.role.at_least(UserRole::Manager) {
        return Err(OversightDenied::NotManager);
    }
    if live.requires_password_change() {
        return Err(OversightDenied::PasswordChange);
    }
    if !live.is_admin() && crate::machine_employee::requires_admin_for_id(home, agent) {
        return Err(OversightDenied::NotInTeam);
    }
    let reports = db
        .overseen_reports(&live.user_id, agent)
        .map_err(|_| OversightDenied::ReadError)?;
    if reports.is_empty() {
        return Err(OversightDenied::NotInTeam);
    }
    Ok(TeamOversight {
        viewer_user_id: live.user_id,
        agent: agent.to_owned(),
        reports: reports
            .into_iter()
            .map(|r| (r.user_id.clone(), r))
            .collect(),
        since: DateTime::from_timestamp_micros(
            window.since_micros.ok_or(OversightDenied::ReadError)?,
        )
        .ok_or(OversightDenied::ReadError)?,
        generation: db
            .acl_generation()
            .map_err(|_| OversightDenied::ReadError)?,
    })
}

#[cfg(test)]
#[path = "team_oversight_tests.rs"]
mod tests;
