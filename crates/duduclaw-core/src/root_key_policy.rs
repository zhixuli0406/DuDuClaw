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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvent {
    pub timestamp: String,
    pub id: String,
    pub event: String,
    pub detail: Option<String>,
    pub action: EngineSocketAction,
}

#[derive(Default)]
struct Runtime {
    warning: Option<String>,
    events: Vec<PolicyEvent>,
    imported: std::collections::HashSet<String>,
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
struct Journal { last_warning: Option<String>, events: Vec<PolicyEvent> }
impl Runtime {
    fn persist(&self) -> std::io::Result<()> {
        if let Some(dir) = &self.journal { dir.write_json_bounded(JOURNAL, &Journal { last_warning: self.warning.clone(), events: self.events.clone() }, JOURNAL_LIMIT)?; }
        Ok(())
    }
    fn initialize(&mut self, home: &std::path::Path) -> std::io::Result<()> {
        if self.journal.is_some() { return Ok(()); }
        let dir = super::state_files::StateDir::open_home(home, true)?;
        let historical: Journal = match dir.read_json_bounded(JOURNAL, JOURNAL_LIMIT) {
            Ok(events) => events,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal::default(),
            Err(error) => return Err(error),
        };
        // Journal contents are diagnostic only, never policy or key authority.
        let mut current = std::mem::take(&mut self.events);
        // The new intake owns live status. Historical state only suppresses a
        // duplicate start or records the end discovered on this fresh intake.
        if let Some(previous) = historical.last_warning {
            if current.first().is_some_and(|event| event.event == "engine_socket_warning_started") {
                current.remove(0);
            }
            if self.warning.is_none() && !current.iter().any(|event| event.event == "engine_socket_warning_ended") {
                current.push(PolicyEvent { id: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(), event: "engine_socket_warning_ended".into(),
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
            self.events.push(PolicyEvent {
                id: uuid::Uuid::new_v4().to_string(), timestamp: chrono::Utc::now().to_rfc3339(),
                event: if next.is_some() { "engine_socket_warning_started" } else { "engine_socket_warning_ended" }.into(),
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

/// Import root audit history as well as runtime transitions. IDs survive restarts;
/// the gateway also deduplicates against its persisted audit log.
pub fn take_events() -> Vec<PolicyEvent> {
    #[cfg(target_os = "linux")]
    let root_events = files::Dir::open(false).and_then(|dir| dir.read_audit()).unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let root_events: Vec<PolicyEvent> = Vec::new();
    let mut rt = runtime();
    let _ = rt.persist();
    let mut events = rt.events.clone();
    for event in root_events {
        if rt.imported.insert(event.id.clone()) { events.push(event); }
    }
    events
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
        assert_eq!(restarted.events[1].event, "engine_socket_warning_ended");
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
        assert!(rt.events[0].event.ends_with("started"));
        assert!(rt.events[1].event.ends_with("ended"));
    }
}
