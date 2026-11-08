//! Real Shared employee stores; only the privileged allocator ReadBack is doubled.
use super::*;
use duduclaw_auth::{IsolationAuthoritySnapshot, UserDb, UserRole};
use duduclaw_isolation::{bounded::{List, Text}, capability::Lane};
use duduclaw_migration::{allocation::{self, Receipt}, confirmation::{ApprovalSession,
    ConfirmationError, ConfirmationStore, ProposalApplier, ProposalValidator},
    contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalField,
        ProposalOperationId, ProposalValue}, ids::RequestId};
use rusqlite::Connection;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

struct Accept;
impl ProposalValidator for Accept {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}
impl ProposalApplier for Accept {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}

struct Root(Receipt);
impl RootAllocation for Root {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, lifecycle::LifecycleError> {
        if self.0.binding != item.binding { return Err(lifecycle::LifecycleError::Unavailable); }
        Ok(self.0.clone())
    }
}

/// Owns fixture authority facts without creating a wiki or other resource grants.
pub struct SharedEmployee {
    home: PathBuf,
    label: String,
    user: String,
    epoch: u64,
    root: Root,
    /// H9 selector producers the issuer may use (none by default).
    sources: SelectorSources,
}

/// A complete `agent.toml` that the typed `AgentConfig` parser accepts, for
/// fixtures whose issuer selectors (H9) read the employee profile.
/// `public_web` is the admin-only protected-mode web switch (`[isolation]`);
/// `capabilities` is appended verbatim to the `[capabilities]` table. The task
/// sandbox's `[container] network_access` stays `false`.
pub fn agent_toml(label: &str, public_web: bool, capabilities: &str) -> String {
    agent_toml_with(label, false, public_web, capabilities)
}

/// [`agent_toml`] with the task-sandbox network switch set independently.
pub fn agent_toml_with(label: &str, network_access: bool, public_web: bool, capabilities: &str) -> String {
    format!(r#"[agent]
name = "{label}"
display_name = "Fixture employee"
role = "main"
status = "active"
trigger = "@{label}"
reports_to = ""
icon = "robot"
[model]
preferred = "claude-sonnet-4-6"
fallback = ""
api_mode = "cli"
account_pool = []
[budget]
monthly_limit_cents = 1000
warn_threshold_percent = 80
hard_stop = false
[container]
sandbox_enabled = false
network_access = {network_access}
timeout_ms = 60000
max_concurrent = 2
readonly_project = false
additional_mounts = []
[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = ""
[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = false
[isolation]
public_web = {public_web}
[capabilities]
{capabilities}
"#)
}
impl SharedEmployee {
    pub fn new(home: &Path, label: &str, employee: EmployeeId) -> Self {
        // Match gateway-private installer stores, independent of the test umask.
        std::fs::DirBuilder::new().mode(0o700).create(home.join("migration-authority")).unwrap();
        for name in ["users.db", "migration-authority/apply.db", "migration-confirmations.db"] {
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .open(home.join(name)).unwrap();
        }
        let users = UserDb::new(&home.join("users.db")).unwrap();
        let user = users.create_user("owner@example.invalid", "Owner", "password1", UserRole::Admin).unwrap().id;
        Connection::open(home.join("users.db")).unwrap().execute(
            "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1",
            [&user]).unwrap();
        let current = users.isolation_authority_snapshot(Some(&user), label).unwrap();
        let mut store = ConfirmationStore::from_connection(Connection::open(home.join("migration-confirmations.db")).unwrap()).unwrap();
        let fields = [("employee_id", label), ("disk_bytes", "67108864"),
            ("binding_kind", "shared"), ("tenant_id", current.tenant_id.as_str())].into_iter()
            .map(|(name, value)| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } }).collect();
        let staged = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
            ConfirmationCategory::Employee, fields, &Accept).unwrap();
        let session = ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]));
        store.approve(&[staged.binding], &session, [2; 32], 100, &Accept).unwrap();
        for _ in 0..3 { store.apply_next(&staged.binding, &mut Accept).unwrap(); }
        let item = store.list().unwrap().into_iter().find(|item| item.binding == staged.binding).unwrap();
        assert_eq!(item.status, ConfirmationStatus::Applied);
        let spec = allocation::specification(&item).unwrap();
        let receipt = Receipt { binding: item.binding, label: spec.label, employee, uid: 70_002,
            bytes: spec.bytes, inodes: spec.inodes, disabled: false, principal_binding: spec.binding };
        crate::handlers::save_confirmed_receipt(home, &item, &receipt).unwrap();
        Self { home: home.to_owned(), label: label.into(), user,
            epoch: current.human.unwrap().credential_epoch, root: Root(receipt), sources: SelectorSources::default() }
    }

    /// Install a root work broker double, as boot does with H5's bridge.
    pub fn with_work_broker(mut self, broker: Arc<dyn duduclaw_mcp_boundary::resources::WorkBroker>) -> Self {
        self.sources.work_broker = Some(broker);
        self
    }

    /// Install spawnd's readiness feature view, as boot does with the
    /// protected backend's `features()` (HX5 helper workspace selector).
    pub fn with_spawnd_features(mut self, view: Arc<dyn Fn() -> duduclaw_isolation::ipc::SpawndFeatures + Send + Sync>)
        -> Self {
        self.sources.spawnd_features = Some(view);
        self
    }

    /// W1: replace every selector producer, e.g. with boot's
    /// `SelectorSources::installed_over` (run-bound work brokers).
    pub(crate) fn with_selector_sources(mut self, sources: SelectorSources) -> Self {
        self.sources = sources;
        self
    }

    pub fn employee(&self) -> EmployeeId { self.root.0.employee }
    pub fn user(&self) -> &str { &self.user }
    pub fn epoch(&self) -> u64 { self.epoch }
    /// The fixture root ReadBack, for tests that wrap it.
    pub(super) fn root(&self) -> &dyn RootAllocation { &self.root }
    pub(super) fn sources(&self) -> &SelectorSources { &self.sources }

    pub fn snapshot(&self) -> IsolationAuthoritySnapshot {
        let mut users = Connection::open_with_flags(self.home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        duduclaw_auth::read_isolation_authority(&mut users, Some(&self.user), &self.label).unwrap()
    }

    /// Same issuer and lifecycle rechecks as production, with fixture root I/O.
    pub fn authorize(&self, current: &IsolationAuthoritySnapshot, lane: Lane,
        tools: List<Text<128>, 256>, turn: super::super::HostTurn, expiry: Option<u64>)
        -> Result<super::super::RuntimeAuthorization, VerificationError> {
        self.authorize_record(current, lane, tools, turn, expiry, None)
    }

    /// Same issuer for a record-backed run; `task` is the host-pinned record
    /// task (the caller supplies `turn.background`).
    pub fn authorize_record(&self, current: &IsolationAuthoritySnapshot, lane: Lane,
        tools: List<Text<128>, 256>, turn: super::super::HostTurn, expiry: Option<u64>, task: Option<&str>)
        -> Result<super::super::RuntimeAuthorization, VerificationError> {
        let lifecycle = lifecycle::read_lifecycle_with(&self.home, &self.label, &self.root).map_err(lifecycle_refusal)?;
        human_authorization_with(self.home.clone(), self.label.clone(), self.root.0.employee,
            self.user.clone(), self.epoch, lane, current, tools, turn, expiry, &lifecycle, &self.root,
            &self.sources, task)
    }
}
