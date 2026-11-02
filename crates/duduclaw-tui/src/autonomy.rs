//! Authoritative autonomy snapshots; change events only invalidate this data.
use crate::{Error, Result, sanitize::TerminalText, state::{Model, Snapshot}, strings};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize)]
pub struct Pause {
    pub reason: TerminalText,
    pub who: TerminalText,
    pub at: TerminalText,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Status {
    pub global: Option<Pause>,
    pub agents: BTreeMap<String, Pause>,
    pub state_error: Option<TerminalText>,
}
impl Status {
    pub fn parse(value: Value) -> Result<Self> {
        // Missing nullable fields must not turn a malformed response into running.
        if !["global", "agents", "state_error"].iter().all(|key| value.get(key).is_some()) {
            return Err(Error::Protocol);
        }
        serde_json::from_value(value).map_err(|_| Error::Protocol)
    }
}
impl Snapshot<Status> {
    pub fn paused(&self, id: &str) -> bool {
        self.items.first().is_some_and(|s| s.global.is_some() || s.agents.contains_key(id))
    }
    pub fn unreadable(&self) -> bool {
        self.error.is_some() || self.items.first().is_some_and(|s| s.state_error.is_some())
    }
    pub fn unconfirmed(&self) -> bool {
        self.unreadable() || self.stale || self.refetch_pending || self.items.is_empty()
    }
}
pub fn status_line(model: &Model) -> Option<TerminalText> {
    let snapshot = &model.autonomy;
    let text = if snapshot.unreadable() {
        strings::AUTONOMY_UNREADABLE.to_owned()
    } else if snapshot.unconfirmed() {
        strings::AUTONOMY_UNKNOWN.to_owned()
    } else if let Some(status) = snapshot.items.first() {
        if status.global.is_some() {
            strings::AUTONOMY_ALL.to_owned()
        } else if !status.agents.is_empty() {
            let names: Vec<_> = status.agents.keys().take(2).map(|id| {
                let name = model.employees.items.iter().find(|e| e.id() == id)
                    .map(|e| e.display_name.clone()).filter(|name| !name.plain().trim().is_empty())
                    .unwrap_or_else(|| TerminalText::new(id.clone()));
                let plain = name.plain().replace('\n', " ");
                let short: String = plain.chars().take(12).collect();
                if plain.chars().count() > 12 { format!("{short}…") } else { short }
            }).collect();
            let remaining = status.agents.len().saturating_sub(names.len());
            let extra = if remaining > 0 { format!(" +{remaining}") } else { String::new() };
            format!("{}（{}{extra}）", strings::AUTONOMY_PAUSED, names.join("、"))
        } else { return None; }
    } else { strings::AUTONOMY_UNKNOWN.to_owned() };
    Some(TerminalText::new(format!("{}: {text}", strings::AUTONOMY)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn malformed_status_is_never_running() {
        for value in [json!({}),json!({"agents":{}}),json!({"global":false,"agents":{},"state_error":null}),
            json!({"global":null,"agents":{"adu":true},"state_error":null})] {
            assert!(Status::parse(value).is_err());
        }
    }
}
