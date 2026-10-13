//! Per-session bridge state, persisted in `<home>/odoo_livechat/state.json`.
//!
//! The file is what lets a restarted gateway pick up where it left off: the
//! per-session message cursor (never answer the same message twice), the AI
//! employee the session was routed to (routing is sticky), and whether a
//! human has taken the conversation over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Closed sessions are dropped from the state as soon as Odoo stops listing
/// them; this is only a backstop against a state file that grows without
/// bound if Odoo keeps sessions open forever.
const MAX_SESSIONS: usize = 1_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    /// Highest `mail.message` id already handled in this session.
    pub last_id: i64,
    /// AI employee answering this session (empty until the first visitor
    /// message routes it).
    pub agent: String,
    /// Audience rule that picked `agent`; `None` ⇒ the default applied.
    pub rule: Option<String>,
    /// A human operator spoke or was invited: the AI stays silent until an
    /// admin hands the session back from the dashboard.
    pub human: bool,
    /// Why `human` is set (`operator_spoke` / `visitor_requested` /
    /// `operator_member`: an operator was already in the session on first
    /// sight, e.g. invited before a restart).
    pub human_reason: Option<String>,
    pub greeted: bool,
    /// The first turn carries a context line; later turns do not.
    pub context_sent: bool,
    pub visitor_name: String,
    pub country: Option<String>,
    pub lang: Option<String>,
    pub logged_in: bool,
    /// RFC 3339.
    pub first_seen: String,
    pub last_activity: String,
    pub replies: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BridgeState {
    /// Which Odoo (url + database) the sessions belong to. `discuss.channel`
    /// ids restart from 1 in every database, so state from another instance
    /// must never be applied (cursors, routing, takeover flags).
    pub instance: String,
    /// Keyed by `discuss.channel` id.
    pub sessions: BTreeMap<i64, SessionState>,
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join("odoo_livechat").join("state.json")
}

impl BridgeState {
    /// Missing or unreadable ⇒ empty. An unreadable file is logged and set
    /// aside rather than silently overwritten, so its cursors can be inspected.
    pub fn load(home: &Path) -> Self {
        let path = state_path(home);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "odoo livechat: state.json unreadable — starting empty");
                let _ = std::fs::rename(&path, path.with_extension("json.corrupt"));
                Self::default()
            }
        }
    }

    /// Atomic write (temp + rename).
    pub fn save(&self, home: &Path) -> std::io::Result<()> {
        let path = state_path(home);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)
    }

    /// Bind the state to `instance`; a different instance starts empty.
    pub fn bind_instance(&mut self, instance: &str) -> bool {
        if self.instance == instance {
            return false;
        }
        self.instance = instance.to_string();
        self.sessions.clear();
        true
    }

    /// Forget sessions Odoo no longer reports as open.
    pub fn retain_open(&mut self, open: &[i64]) {
        self.sessions.retain(|id, _| open.contains(id));
        while self.sessions.len() > MAX_SESSIONS {
            let Some(oldest) = self.sessions.keys().next().copied() else {
                break;
            };
            self.sessions.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip_and_retain() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = BridgeState::default();
        s.sessions.insert(
            3,
            SessionState {
                last_id: 7,
                agent: "kiki".into(),
                ..Default::default()
            },
        );
        s.sessions.insert(4, SessionState::default());
        s.save(dir.path()).unwrap();
        let mut loaded = BridgeState::load(dir.path());
        assert_eq!(loaded, s);
        loaded.retain_open(&[4]);
        assert_eq!(loaded.sessions.keys().copied().collect::<Vec<_>>(), vec![4]);
    }

    #[test]
    fn switching_instance_drops_sessions() {
        let mut s = BridgeState::default();
        assert!(s.bind_instance("a"));
        s.sessions.insert(1, SessionState::default());
        assert!(!s.bind_instance("a"));
        assert_eq!(s.sessions.len(), 1);
        assert!(s.bind_instance("b"));
        assert!(s.sessions.is_empty());
    }

    #[test]
    fn corrupt_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(BridgeState::load(dir.path()), BridgeState::default());
        assert!(path.with_extension("json.corrupt").exists());
    }
}
