//! Shared classification for directly bound Viewer/Operator history reads.
//! This never widens ordinary ACLs or changes who may chat/call an employee.
pub mod handles;
pub mod projection;
pub mod read;
use crate::{
    session_origin::SessionOrigin,
    team_history::classify::{self as origin_class, HistoryClass},
};
use duduclaw_auth::{AccessLevel, UserContext};
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Full,
    Filtered,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    Own,
    External { channel: String },
    Hidden,
}

/// Explicit false is a migration escape hatch. Missing, unreadable or
/// malformed configuration always keeps the privacy restriction enabled.
pub fn configured(home: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(home.join("config.toml")) else {
        return true;
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return true;
    };
    table
        .get("people")
        .and_then(toml::Value::as_table)
        .and_then(|p| p.get("bound_history_privacy"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}
pub fn mode(home: &Path, ctx: &UserContext, agent: &str) -> Result<Mode, String> {
    if ctx.requires_password_change() || !ctx.has_agent_access(agent, AccessLevel::Viewer) {
        return Err("permission denied".into());
    }
    if ctx.is_admin() || ctx.has_agent_access(agent, AccessLevel::Owner) || !configured(home) {
        Ok(Mode::Full)
    } else {
        Ok(Mode::Filtered)
    }
}
pub fn classify(id: &str, origin: Option<&SessionOrigin>, user: &str) -> Class {
    match origin_class::classify(id, origin, user, &BTreeSet::new()) {
        HistoryClass::ViewerOwn => Class::Own,
        HistoryClass::External { channel } => Class::External { channel },
        _ => Class::Hidden,
    }
}
#[cfg(test)]
mod tests;

/// Plain export/search text is derived only from the same closed parts sent
/// to the UI. Attachment names and paths never enter a masked projection.
pub fn plain_parts(parts: &[crate::team_history::mask::Part]) -> String {
    use crate::team_history::mask::Part;
    parts
        .iter()
        .map(|p| match p {
            Part::Text { v } => v.clone(),
            Part::Mask { cat, n } => format!("[{cat}_{n}]"),
            Part::Attachment { kind, .. } => format!("[attachment:{kind}]"),
        })
        .collect()
}
