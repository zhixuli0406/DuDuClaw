//! H9 issuer selectors (Package H). For one protected employee's live run they
//! decide which additional resource grants the boot issuer (H10,
//! `boot::human_authorization_with`) signs into the capability:
//!
//! * `Workspace { employee-work }` (H6 brokered root) — only when a root work
//!   broker is installed at boot, the run's tool snapshot lists a file tool and
//!   the employee's current profile (`agent.toml [capabilities]`
//!   `allowed_tools`/`denied_tools`, the same `tool_list_verdict` the ingress
//!   catalog uses) still allows it. HX5: the same grant is also selected when
//!   the snapshot lists an in-run helper tool (`execute_program`, `code_map`,
//!   `office_script`) that the profile allows, a work broker is installed and
//!   spawnd's readiness view advertises `helper_calls` right now; any of
//!   those missing (or an unreadable view) withholds it (fail closed). Only
//!   then are the helper tools signed (`workspace_selection`); each call still
//!   checks the run's own `Reserved.features`. W1: in production "a work
//!   broker is installed" means boot's run-bound factory (`run_broker.rs`)
//!   exists and spawnd's readiness view advertises `work_broker` now; the
//!   root gets a fresh broker bound to this capability's employee and
//!   incarnation (and, at first use, its run and control connection).
//! * `PublicOrigins { * }` (H7, D-H2) — only for an employee-scope run (no
//!   task) that lists `web_fetch_cached`/`web_extract` (or, since 2026-11-09,
//!   `skill_hub_install`) and the profile allows it, and only when the admin
//!   enabled protected public web for this employee: `agent.toml [isolation]
//!   public_web = true` (owner decision 2026-11-08). That switch is separate
//!   from `[container] network_access`, which keeps controlling only the task
//!   sandbox's network. `[isolation]` is admin-only (`org_field_guard` freezes
//!   it against the employee's own writes) and defaults to `false`, so the
//!   default is no web authority. Never from tool arguments. HX6: the same
//!   grant is also signed for a run whose recording is selected (`recording`,
//!   below), as the authority for `browser_record_start`'s navigation URL.
//! * `NewTask { requester }` (H9 decision 3) — only for an interactive run (not
//!   record-backed, no task) that lists `create_task` while the profile still
//!   allows it. The requester is the run's authenticated human (B1 has already
//!   required a Personal employee's requester to be its owner).
//! * the record `Task { id }` grant (H8) — only for a record-backed
//!   (background) run that names a task, and only when `tasks.db` says now that
//!   the employee's agent owns it (`tasks.assigned_to` + the existing audience
//!   rule, as the H8 resolver checks on every call). A run whose task is no
//!   longer the employee's (reassigned or deleted) is refused outright
//!   (`Revoked`, recorded as `isolation_task_run_refused`).
//!
//! Every input is read at issuance from its authoritative store, never cached
//! and never taken from the employee: the profile through a no-follow walk of
//! gateway-private `agents/<label>/agent.toml`, the task store through the
//! hardened private-SQLite opener. A store that is unsafe, unreadable or
//! malformed fails the whole issuance closed (`Unavailable`); an absent
//! setting only withholds its grant. Selectors that are not needed for this
//! run (no matching tool, no broker, no task) perform no I/O at all. The issuer
//! re-runs `select` after its post-I/O lifecycle and users.db rechecks and
//! refuses (`Revoked`) if anything changed (B2 pattern).
use std::{io::Read, path::Path, sync::Arc};
use duduclaw_isolation::{bounded::{List, Text}, capability::{PublicOrigin, ResourceGrant}, ids::EmployeeId,
    ipc::SpawndFeatures};
use duduclaw_mcp_boundary::{resources::{ResourceRoots, WorkBroker, EMPLOYEE_WORK_ROOT},
    tool_policy::{IN_RUN_HELPER_TOOLS, IN_RUN_RECORDING_TOOLS, PUBLIC_ORIGIN_TOOLS}};
use super::VerificationError;

/// File tools served from the brokered `employee-work` root (H6/H11).
pub(crate) const FILE_TOOLS: &[&str] = &["file_read", "csv_read", "xlsx_read"];
/// spawnd's current readiness feature view (`SpawndBackend::features`). Read
/// at issuance only; no view or an error must read as `SpawndFeatures::NONE`.
pub(crate) type SpawndFeatureView = Arc<dyn Fn() -> SpawndFeatures + Send + Sync>;
/// H12b: protected hub install; tied to the `public_web` switch (2026-11-09).
/// The hub archive URL is host-selected, the grant is the same generic one.
const HUB_INSTALL_TOOLS: &[&str] = &["skill_hub_install"];
/// The tool the interactive `NewTask` grant authorizes.
pub(crate) const NEW_TASK_TOOLS: &[&str] = &["create_task"];
/// Upper bound for `agent.toml`; a larger file is not a profile we trust.
const PROFILE_MAX_BYTES: u64 = 1024 * 1024;

/// Trusted producers the selectors may use. Installed by host boot (F1/H5
/// supply the spawnd work broker); absent producers withhold their grant.
#[derive(Clone, Default)]
pub(crate) struct SelectorSources {
    /// Test-only host broker double (`SharedEmployee::with_work_broker`,
    /// `BootSources.work_broker`); wrapped per capability by the weaker
    /// `run_broker::latched`, so it never exists in a production build.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) work_broker: Option<Arc<dyn WorkBroker>>,
    /// W1: production run-bound broker factory over the protected backend
    /// (`run_broker.rs`); used when no host broker is supplied.
    pub(crate) run_brokers: Option<super::run_broker::RunBrokers>,
    /// HX5: spawnd's advertised features; absent ⇒ no helper workspace.
    pub(crate) spawnd_features: Option<SpawndFeatureView>,
}

impl SelectorSources {
    /// Boot's producers: W1's run-bound brokers over the protected backend
    /// (none if the backend has no readiness source), and the backend's
    /// readiness feature view (`SpawndBackend::features`; no source reads as
    /// no features).
    #[cfg_attr(not(feature = "isolation-worker-integration"), allow(dead_code))]
    pub(crate) fn installed(home: &Path, backend: Arc<duduclaw_execution::SpawndBackend>) -> Self {
        let sources = Self::installed_over(home, Arc::new(super::run_broker::SpawndRunTransport::new(backend)));
        super::run_broker::mark_installed(sources.run_brokers.is_some());
        sources
    }
    /// [`Self::installed`] over any transport (boot and W1 tests).
    #[cfg_attr(not(feature = "isolation-worker-integration"), allow(dead_code))]
    pub(crate) fn installed_over(home: &Path, transport: Arc<dyn super::run_broker::RunWorkTransport>) -> Self {
        let run_brokers = super::run_broker::RunBrokers::new(home, transport.clone());
        Self { #[cfg(any(test, feature = "test-support"))] work_broker: None, run_brokers,
            spawnd_features: Some(Arc::new(move || transport.host_features())) }
    }
    /// Test fixtures only: replace the run-bound factory with a host broker
    /// double (`BootSources.work_broker`).
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(not(feature = "isolation-worker-integration"), allow(dead_code))]
    pub(crate) fn with_host_broker(mut self, broker: Arc<dyn WorkBroker>) -> Self {
        self.work_broker = Some(broker);
        self.run_brokers = None;
        super::run_broker::mark_installed(true);
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    fn host_broker(&self) -> Option<&Arc<dyn WorkBroker>> { self.work_broker.as_ref() }
    #[cfg(not(any(test, feature = "test-support")))]
    fn host_broker(&self) -> Option<&Arc<dyn WorkBroker>> { None }
    /// A broker can be bound for this issuance: the run-bound factory while
    /// spawnd advertises `work_broker` (or a test host broker).
    fn broker_available(&self) -> bool {
        self.host_broker().is_some()
            || self.run_brokers.as_ref().is_some_and(super::run_broker::RunBrokers::available)
    }
    /// HX5 precondition of the helper workspace: a work broker is installed
    /// and spawnd advertises `helper_calls` now. Anything else ⇒ false.
    fn helper_calls(&self) -> bool {
        self.broker_available()
            && self.spawnd_features.as_ref().is_some_and(|view| view().contains(SpawndFeatures::HELPER_CALLS))
    }
    /// HX6 gate: helper calls AND spawnd's `helper_recording` (Chromium proven
    /// to launch under the helper sandbox on this host). Anything else ⇒ false.
    fn recording_ready(&self) -> bool {
        self.helper_calls() && self.spawnd_features.as_ref().is_some_and(|view|
            view().contains(SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::HELPER_RECORDING)))
    }
}

/// Facts the issuer passes in; all host-pinned, none from the employee.
pub(super) struct SelectorInput<'a> {
    pub home: &'a Path,
    pub agent: &'a str,
    /// The authenticated human of this run (the issuer's `Human` principal).
    pub user: &'a str,
    pub tools: &'a List<Text<128>, 256>,
    /// The run's record task (task/schedule runs only).
    pub task: Option<&'a str>,
    /// The run is record-backed (`HostTurn.background`).
    pub record: bool,
}

/// The run's record task; selected only when the employee owns it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectedTask {
    pub id: String,
}

/// What the selectors decided. Compared before/after the issuer's I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Selection {
    pub workspace: bool,
    /// HX5: `workspace` was (also) selected for the in-run helper tools, so
    /// exactly those are signed alongside it (`workspace_selection`).
    pub helpers: bool,
    /// HX6: `workspace` was (also) selected for the in-run browser recording
    /// (`IN_RUN_RECORDING_TOOLS`), which is signed alongside it.
    pub recording: bool,
    /// The web tools (`PUBLIC_ORIGIN_TOOLS`) were selected: the only case in
    /// which they are signed alongside a PublicOrigins grant.
    pub public_web: bool,
    /// H12b (owner decision 2026-11-09): `skill_hub_install` was selected on
    /// the same `public_web` switch; it signs the grant but never the web tools.
    pub hub_install: bool,
    /// The requester of an interactive run's `NewTask` grant.
    pub new_task: Option<String>,
    pub task: Option<SelectedTask>,
}

pub(super) fn select(sources: &SelectorSources, input: &SelectorInput<'_>) -> Result<Selection, VerificationError> {
    if input.task.is_some() && !input.record { return Err(VerificationError::Invalid); }
    let listed = |name: &str| input.tools.as_slice().iter().any(|tool| tool.as_str() == name);
    // H9: the readiness view (`broker_available` reads spawnd's report) is
    // only consulted when a file tool is listed.
    let wants_files = FILE_TOOLS.iter().any(|name| listed(*name)) && sources.broker_available();
    // HX5: checked only when a helper tool is listed (no view read otherwise).
    let wants_helpers = IN_RUN_HELPER_TOOLS.iter().any(|name| listed(*name)) && sources.helper_calls();
    // HX6: the recording needs the host gate and an interactive run (its
    // egress is the employee's public-web authority, D-H2 / Q3 = A).
    let wants_recording = input.task.is_none() && IN_RUN_RECORDING_TOOLS.iter().any(|name| listed(*name))
        && sources.recording_ready();
    // D-H2: PublicOrigins exists only alongside EmployeeScope; a task-limited
    // run never carries it, so it is not even considered there.
    // Owner decision 2026-11-09: protected `skill_hub_install` rests on the
    // same `public_web` switch, so listing it also considers the grant.
    let wants_web = input.task.is_none()
        && PUBLIC_ORIGIN_TOOLS.iter().chain(HUB_INSTALL_TOOLS).any(|name| listed(*name));
    // NewTask: interactive runs only (a record-backed run plans under its own
    // record task, H8, and has no human requester of its own).
    let wants_new_task = !input.record && input.task.is_none() && NEW_TASK_TOOLS.iter().any(|name| listed(*name));
    let profile = if wants_files || wants_helpers || wants_recording || wants_web || wants_new_task {
        Some(read_profile(input.home, input.agent)?)
    } else { None };
    let allows = |names: &[&str]| profile.as_ref().is_some_and(|profile| {
        let caps = &profile.capabilities;
        names.iter().any(|name| listed(*name) && duduclaw_core::tool_catalog::tool_list_verdict(
            *name, &caps.denied_tools, &caps.allowed_tools) == duduclaw_core::tool_catalog::ToolListVerdict::Allowed)
    });
    let helpers = wants_helpers && allows(&IN_RUN_HELPER_TOOLS[..]);
    // Owner decision Q3 = A: enabling recording means granting this
    // employee's runtime public-web egress, so the same admin-only switch
    // (`[isolation] public_web`) gates it. No switch ⇒ no recording.
    let recording = wants_recording && profile.as_ref().is_some_and(|profile| profile.isolation.public_web)
        && allows(&IN_RUN_RECORDING_TOOLS[..]);
    let workspace = (wants_files && allows(FILE_TOOLS)) || helpers || recording;
    // Owner decision 2026-11-08: its own admin-only switch, independent of
    // `[container] network_access` (task sandbox only).
    let web_switch = wants_web && profile.as_ref().is_some_and(|profile| profile.isolation.public_web);
    let public_web = web_switch && allows(PUBLIC_ORIGIN_TOOLS);
    let hub_install = web_switch && allows(HUB_INSTALL_TOOLS);
    let new_task = (wants_new_task && allows(NEW_TASK_TOOLS)).then(|| input.user.to_owned());
    if new_task.as_deref().is_some_and(|user| user.is_empty() || user.len() > 128 || user.chars().any(char::is_control)) {
        return Err(VerificationError::Invalid);
    }
    let task = match input.task {
        None => None,
        Some(id) => {
            // Owner decision 2026-11-08: a task that is no longer this
            // employee's (reassigned or deleted) refuses the run outright.
            if !crate::isolation_background::task_owned_now(input.home, input.agent, id)? {
                crate::isolation_background::record_foreign_task(input.home, input.agent, id, "issuance");
                return Err(VerificationError::Revoked);
            }
            Some(SelectedTask { id: id.to_owned() })
        }
    };
    Ok(Selection { workspace, helpers, recording, public_web, hub_install, new_task, task })
}

impl Selection {
    /// Add the selected Workspace/PublicOrigins/NewTask grants (and the
    /// brokered root they need) to the issuer's resources. The Task grant is
    /// applied by the issuer through `RuntimeAuthorization::limit_to_task`.
    /// W1: the brokered root gets a fresh broker bound to this capability's
    /// employee and pinned incarnation (and, at first use, its run).
    pub(super) fn apply(&self, sources: &SelectorSources, lifecycle: &crate::isolation_lifecycle::EmployeeLifecycle,
        resources: &mut Vec<ResourceGrant>, roots: &mut ResourceRoots) -> Result<(), VerificationError> {
        let employee: EmployeeId = lifecycle.employee;
        if self.workspace {
            let broker = match (sources.host_broker(), &sources.run_brokers) {
                #[cfg(any(test, feature = "test-support"))]
                (Some(broker), _) => super::run_broker::latched(broker.clone(), employee),
                (_, Some(brokers)) => brokers.bind(&lifecycle.label, employee, lifecycle.incarnation),
                (_, None) => return Err(VerificationError::Unavailable),
            };
            roots.insert_brokered(EMPLOYEE_WORK_ROOT.to_owned(), employee, broker)?;
            resources.push(ResourceGrant::Workspace {
                handle: Text::new(EMPLOYEE_WORK_ROOT.to_owned()).map_err(|_| VerificationError::Invalid)? });
        }
        // HX6: the recording's navigation URL is authorized by this same grant
        // (`tool_policy::RECORDING_URL_TOOLS`), never a per-URL UrlOrigin. It
        // is signed only when `recording` holds (helper_calls +
        // helper_recording, `[isolation] public_web`, interactive run, profile
        // allows); it does not make the recording a web tool
        // (`PUBLIC_ORIGIN_TOOLS` is unchanged). H12b: a selected hub install
        // signs the same generic grant. At most one grant in every case.
        if self.public_web || self.recording || self.hub_install {
            resources.push(ResourceGrant::PublicOrigins {
                origins: List::new(vec![PublicOrigin::any()]).map_err(|_| VerificationError::Invalid)? });
        }
        if let Some(requester) = &self.new_task {
            resources.push(ResourceGrant::NewTask {
                requester: Text::new(requester.clone()).map_err(|_| VerificationError::Invalid)? });
        }
        Ok(())
    }

    /// The tool snapshot to sign. The `Workspace { employee-work }` grant is
    /// selected for the file tools and — only with `helpers` — the HX5 in-run
    /// helper tools. Every other listed tool whose eligibility would rest on
    /// that grant (the shared pending list, e.g. `skill_from_recording`, the
    /// HX6 recording tools unless `recording`, and any other A/E tool) is
    /// left out of the capability, so it stays
    /// hidden and refused. Only ever narrows.
    pub(super) fn signed_tools(&self, tools: &List<Text<128>, 256>)
        -> Result<List<Text<128>, 256>, VerificationError> {
        // HX6 / H12b: a PublicOrigins grant signed only for the recording or
        // the hub install is never authority for the web tools, so they leave
        // the snapshot unless their own `public_web` selection holds.
        let foreign_grant_web = (self.recording || self.hub_install) && !self.public_web;
        if !self.workspace && !foreign_grant_web { return Ok(tools.clone()); }
        // A task run is limited to its task and loses EmployeeScope.
        let scoped = self.task.is_none();
        List::new(tools.as_slice().iter().filter(|tool|
            !(foreign_grant_web && PUBLIC_ORIGIN_TOOLS.contains(&tool.as_str()))
            && !(self.workspace && duduclaw_mcp_boundary::workspace_selection::drops_from_signed_with(tool.as_str(),
                scoped, self.helpers, self.recording)))
            .cloned().collect()).map_err(|_| VerificationError::Invalid)
    }
}

/// The employee's current `agent.toml`, read without following symlinks from
/// gateway-private state. Missing, unsafe (symlink, not regular, hard-linked,
/// foreign owner, oversized), unreadable or malformed ⇒ `Unavailable`.
/// Also read by readiness (`reasons::installed_tool_needs`).
pub(crate) fn read_profile(home: &Path, agent: &str) -> Result<duduclaw_core::types::AgentConfig, VerificationError> {
    use std::os::{fd::{AsRawFd, FromRawFd}, unix::fs::MetadataExt};
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    let directory = super::ingress::own_agent_dir(home, agent)?;
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NOCTTY | OFlag::O_NONBLOCK;
    let fd = openat(Some(directory.as_raw_fd()), "agent.toml", flags, Mode::empty())
        .map_err(|_| VerificationError::Unavailable)?;
    // SAFETY: a successful openat returns a new owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|_| VerificationError::Unavailable)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > PROFILE_MAX_BYTES
        || metadata.uid() != nix::unistd::geteuid().as_raw() {
        return Err(VerificationError::Unavailable);
    }
    let mut raw = String::new();
    file.take(PROFILE_MAX_BYTES + 1).read_to_string(&mut raw).map_err(|_| VerificationError::Unavailable)?;
    if raw.len() as u64 > PROFILE_MAX_BYTES { return Err(VerificationError::Unavailable); }
    toml::from_str(&raw).map_err(|_| VerificationError::Unavailable)
}
