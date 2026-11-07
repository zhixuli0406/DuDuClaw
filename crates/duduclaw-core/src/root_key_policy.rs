//! Root-owned policy is the sole authority. Service-home configuration is never read.
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

pub const POLICY_PATH: &str = "/etc/duduclaw/protected-policy.toml";
pub const RISK_ZH: &str = "改成『只警告』後，主機上有程式開放容器引擎控制介面時，DuDuClaw 不會停用同仁的 Google 等個人帳號。能操作容器引擎的程式等同擁有整台主機的最高權限，若 AI 員工能碰到它，就可能讀出解密金鑰。只建議在您能確認 AI 員工碰不到容器引擎、且需要在同一台主機執行容器時使用；更安全的做法是把開發用的容器移到另一台主機，或以一般帳號（docker 群組）執行 docker。";
pub const RISK_EN: &str = "With warn, DuDuClaw keeps employees' personal accounts such as Google available when a container-engine control interface is reachable. Programs controlling the engine effectively have root access to the host; an AI employee that can reach it may extract the decryption key. Use this only when you can ensure AI employees cannot access the engine and containers must run on this host. Prefer another host for development containers, or run docker as a regular user in the docker group.";
pub const WARNING_ZH: &str = "容器引擎控制介面目前可被存取；依管理員設定僅警告，未停用個人帳號";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineSocketAction { #[default] Revoke, Warn }
impl EngineSocketAction {
    pub fn as_str(self) -> &'static str { match self { Self::Revoke => "revoke", Self::Warn => "warn" } }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySnapshot {
    #[serde(rename = "engine_socket_action")]
    pub action: EngineSocketAction,
    pub change_id: Option<String>,
}

fn parse(text: &str) -> std::io::Result<PolicySnapshot> {
    let policy: PolicySnapshot = toml::from_str(text).map_err(std::io::Error::other)?;
    if policy.change_id.as_ref().is_some_and(|id| uuid::Uuid::parse_str(id).is_err()) {
        return Err(std::io::Error::other("invalid policy change id"));
    }
    Ok(policy)
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyStatus {
    pub action: EngineSocketAction,
    pub warning_active: bool,
    pub detail: Option<String>,
    pub risk_zh: String,
    pub command_warn: String,
    pub command_revoke: String,
}

// Each source has its own deserialization allowlist. A runtime journal cannot
// deserialize a root policy change, even if it supplies a root-looking ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootPolicyEventKind {
    #[serde(rename = "engine_socket_policy_change_requested")]
    ChangeRequested,
    #[serde(rename = "engine_socket_policy_changed")]
    Changed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeWarningEventKind {
    #[serde(rename = "engine_socket_warning_started")]
    Started,
    #[serde(rename = "engine_socket_warning_ended")]
    Ended,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootPolicyEvent {
    pub timestamp: String,
    pub id: String,
    pub event: RootPolicyEventKind,
    pub detail: Option<String>,
    pub action: EngineSocketAction,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeWarningEvent {
    pub timestamp: String,
    pub id: String,
    pub event: RuntimeWarningEventKind,
    pub detail: Option<String>,
    pub action: EngineSocketAction,
}

#[derive(Default)]
struct Runtime {
    warning: Option<String>,
    events: Vec<RuntimeWarningEvent>,
    journal: Option<super::state_files::StateDir>,
}
static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();
fn runtime() -> std::sync::MutexGuard<'static, Runtime> {
    RUNTIME.get_or_init(|| Mutex::new(Runtime::default())).lock().unwrap_or_else(|p| p.into_inner())
}

pub fn read() -> PolicySnapshot {
    #[cfg(target_os = "linux")]
    { files::Dir::open(false).and_then(|dir| dir.read_policy()).unwrap_or_default() }
    #[cfg(not(target_os = "linux"))]
    { PolicySnapshot::default() }
}

pub fn status() -> PolicyStatus {
    let action = read().action;
    let rt = runtime();
    PolicyStatus {
        action, warning_active: rt.warning.is_some(), detail: rt.warning.clone(), risk_zh: RISK_ZH.into(),
        command_warn: "sudo duduclaw service protected-linux policy engine-socket warn --yes".into(),
        command_revoke: "sudo duduclaw service protected-linux policy engine-socket revoke".into(),
    }
}

const JOURNAL: &str = "engine-socket-warning-events.json";
const JOURNAL_LIMIT: u64 = 1024 * 1024;
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal { last_warning: Option<String>, events: Vec<RuntimeWarningEvent> }
fn decode_journal(value: serde_json::Value) -> (Journal, usize) {
    let mut journal = Journal::default();
    let mut quarantined = 0;
    if let Some(rows) = value.get("events").and_then(serde_json::Value::as_array) {
        let mut ids = std::collections::HashSet::new();
        for row in rows {
            let parsed = serde_json::from_value::<RuntimeWarningEvent>(row.clone()).ok().and_then(|mut event| {
                // Accept old UUID-only runtime journals, but never root: IDs.
                let raw = event.id.strip_prefix("runtime:").unwrap_or(&event.id);
                let id = uuid::Uuid::parse_str(raw).ok()?;
                chrono::DateTime::parse_from_rfc3339(&event.timestamp).ok()?;
                event.id = format!("runtime:{id}");
                Some(event)
            });
            match parsed {
                Some(event) => { if ids.insert(event.id.clone()) { journal.events.push(event); } }
                None => quarantined += 1,
            }
        }
        journal.last_warning = value.get("last_warning").and_then(serde_json::Value::as_str).map(str::to_owned);
    } else { quarantined += 1; }
    if quarantined != 0 {
        tracing::warn!(quarantined_rows = quarantined, "Malformed runtime warning rows quarantined (skipped)");
    }
    (journal, quarantined)
}

impl Runtime {
    fn persist(&self) -> std::io::Result<()> {
        if let Some(dir) = &self.journal { dir.write_json_bounded(JOURNAL, &Journal { last_warning: self.warning.clone(), events: self.events.clone() }, JOURNAL_LIMIT)?; }
        Ok(())
    }
    fn initialize(&mut self, home: &std::path::Path) -> std::io::Result<()> {
        if self.journal.is_some() { return Ok(()); }
        let dir = super::state_files::StateDir::open_home(home, true)?;
        // Parse rows separately: one poisoned row must not hide valid warnings
        // or interfere with the independent root log import.
        let historical = match dir.read_json_bounded::<serde_json::Value>(JOURNAL, JOURNAL_LIMIT) {
            Ok(value) => decode_journal(value).0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal::default(),
            Err(_) => {
                tracing::warn!(quarantined_rows = 1, "Malformed runtime warning journal skipped");
                Journal::default()
            }
        };
        // Journal contents are diagnostic only, never policy or key authority.
        let mut current = std::mem::take(&mut self.events);
        // The new intake owns live status. Historical state only suppresses a
        // duplicate start or records the end discovered on this fresh intake.
        if let Some(previous) = historical.last_warning {
            if current.first().is_some_and(|event| event.event == RuntimeWarningEventKind::Started) {
                current.remove(0);
            }
            if self.warning.is_none() && !current.iter().any(|event| event.event == RuntimeWarningEventKind::Ended) {
                current.push(RuntimeWarningEvent { id: format!("runtime:{}", uuid::Uuid::new_v4()),
                    timestamp: chrono::Utc::now().to_rfc3339(), event: RuntimeWarningEventKind::Ended,
                    detail: Some(super::recovery::sanitize_diagnostic(&previous)), action: read().action });
            }
        }
        self.events = historical.events;
        self.events.extend(current);
        self.journal = Some(dir);
        self.persist()
    }
    fn ack(&mut self, ids: &[String]) -> std::io::Result<()> {
        let ids: std::collections::HashSet<_> = ids.iter().collect();
        let previous = self.events.clone();
        self.events.retain(|event| !ids.contains(&event.id));
        if let Err(error) = self.persist() { self.events = previous; return Err(error); }
        Ok(())
    }
    fn observe(&mut self, action: EngineSocketAction, detail: Option<&str>) {
        let next = detail.map(super::recovery::sanitize_diagnostic);
        let changed = self.warning.is_some() != next.is_some();
        if changed {
            self.events.push(RuntimeWarningEvent {
                id: format!("runtime:{}", uuid::Uuid::new_v4()), timestamp: chrono::Utc::now().to_rfc3339(),
                event: if next.is_some() { RuntimeWarningEventKind::Started } else { RuntimeWarningEventKind::Ended },
                detail: next.clone().or_else(|| self.warning.clone()), action,
            });
        }
        self.warning = next;
        if changed { if let Err(error) = self.persist() {
            tracing::warn!(error = %super::recovery::sanitize_diagnostic(&error.to_string()), "Engine warning audit journal unavailable; events retained for retry");
        } }
    }
}

/// Called only after all other attestation checks passed, while the key lock is held.
pub(super) fn observe(action: EngineSocketAction, detail: Option<&str>) {
    if let Some(detail) = detail {
        tracing::warn!(detail = %super::recovery::sanitize_diagnostic(detail), "Native engine socket reachable; administrator policy keeps the root key");
    }
    runtime().observe(action, detail);
}

pub(super) fn initialize(home: &std::path::Path) {
    if let Err(error) = runtime().initialize(home) {
        tracing::warn!(error = %super::recovery::sanitize_diagnostic(&error.to_string()), "Engine warning audit journal unavailable");
    }
}

pub fn ack_events(ids: &[String]) -> std::io::Result<()> { runtime().ack(ids) }

/// Read only the pinned root-owned source, on every import pass. Service-owned
/// receipts, JSONL files and in-memory acknowledgements cannot suppress it.
pub fn read_root_events() -> std::io::Result<Vec<RootPolicyEvent>> {
    #[cfg(target_os = "linux")]
    {
        match files::Dir::open(false).and_then(|dir| dir.read_audit()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            result => result,
        }
    }
    #[cfg(not(target_os = "linux"))]
    { Ok(Vec::new()) }
}

pub fn take_runtime_events() -> Vec<RuntimeWarningEvent> {
    let rt = runtime();
    let _ = rt.persist();
    rt.events.clone()
}

pub fn write(action: EngineSocketAction) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 { return Err(std::io::ErrorKind::PermissionDenied.into()); }
        files::Dir::open(true)?.write_policy(action)
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = action; Err(std::io::ErrorKind::Unsupported.into()) }
}

#[cfg(unix)]
#[path = "root_key_policy_files.rs"]
mod files;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_key_policy_runtime_allowlist_namespace_and_quarantine() {
        let id = uuid::Uuid::new_v4().to_string();
        let row = serde_json::json!({"id": id, "timestamp": "2026-11-05T01:02:03Z",
            "event": "engine_socket_warning_started", "action": "warn", "detail": null});
        let mut forged_type = row.clone();
        forged_type["event"] = "engine_socket_policy_changed".into();
        let mut forged_id = row.clone();
        forged_id["id"] = format!("root:{id}").into();
        let (journal, quarantined) = decode_journal(serde_json::json!({
            "last_warning": null, "events": [forged_type, "broken", forged_id, row, row]
        }));
        assert_eq!(quarantined, 3);
        assert_eq!(journal.events.len(), 1);
        assert_eq!(journal.events[0].id, format!("runtime:{id}"));
        assert_eq!(journal.events[0].event, RuntimeWarningEventKind::Started);
        assert!(serde_json::from_value::<RootPolicyEvent>(serde_json::to_value(&journal.events[0]).unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn root_key_policy_malformed_runtime_journal_does_not_block_live_events() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = super::super::state_files::StateDir::open_home(home.path(), true).unwrap();
        dir.write_json_bounded(JOURNAL, &serde_json::json!({"events": ["broken"]}), JOURNAL_LIMIT).unwrap();
        let mut rt = Runtime::default();
        rt.observe(EngineSocketAction::Warn, Some("@docker"));
        rt.initialize(home.path()).unwrap();
        assert_eq!(rt.events.len(), 1);
        assert!(rt.events[0].id.starts_with("runtime:"));
        assert_eq!(rt.events[0].event, RuntimeWarningEventKind::Started);
    }

    #[cfg(unix)]
    #[test]
    fn root_key_policy_warning_journal_survives_restart_until_ack() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut rt = Runtime::default();
        rt.initialize(home.path()).unwrap();
        rt.observe(EngineSocketAction::Warn, Some("@docker: root"));
        let id = rt.events[0].id.clone();
        drop(rt);
        let mut restarted = Runtime::default();
        restarted.initialize(home.path()).unwrap();
        assert_eq!(restarted.events[0].id, id);
        assert!(restarted.warning.is_none()); // journal cannot claim live exposure or key state
        assert_eq!(restarted.events.len(), 2);
        assert_eq!(restarted.events[1].event, RuntimeWarningEventKind::Ended);
        let ids = restarted.events.iter().map(|event| event.id.clone()).collect::<Vec<_>>();
        restarted.ack(&ids).unwrap();
        drop(restarted);
        let mut restarted = Runtime::default();
        restarted.initialize(home.path()).unwrap();
        assert!(restarted.events.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn root_key_policy_restart_does_not_repeat_same_warning_start() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut rt = Runtime::default();
        rt.observe(EngineSocketAction::Warn, Some("@docker"));
        rt.initialize(home.path()).unwrap();
        let id = rt.events[0].id.clone();
        drop(rt);
        let mut rt = Runtime::default();
        rt.observe(EngineSocketAction::Warn, Some("@docker"));
        rt.initialize(home.path()).unwrap();
        assert_eq!(rt.events.len(), 1);
        assert_eq!(rt.events[0].id, id);
        assert!(rt.warning.is_some());
    }

    #[test]
    fn root_key_policy_strict_parse_defaults_and_values() {
        assert_eq!(PolicySnapshot::default().action, EngineSocketAction::Revoke);
        assert_eq!(parse("engine_socket_action = 'warn'").unwrap().action, EngineSocketAction::Warn);
        for bad in ["", "[", "engine_socket_action = 'WARN'", "engine_socket_action = true", "engine_socket_action = 'warn'\nextra = true", "engine_socket_action = 'warn'\nchange_id = 'bad'"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn root_key_policy_audits_only_warning_transitions() {
        let mut rt = Runtime::default();
        rt.observe(EngineSocketAction::Warn, None);
        for _ in 0..4 { rt.observe(EngineSocketAction::Warn, Some("@docker: root")); }
        assert_eq!(rt.events.len(), 1);
        rt.observe(EngineSocketAction::Warn, Some("/run/docker.sock: root"));
        assert_eq!(rt.events.len(), 1);
        rt.observe(EngineSocketAction::Revoke, None);
        rt.observe(EngineSocketAction::Revoke, None);
        assert_eq!(rt.events.len(), 2);
        assert_eq!(rt.events[0].event, RuntimeWarningEventKind::Started);
        assert_eq!(rt.events[1].event, RuntimeWarningEventKind::Ended);
    }
}
