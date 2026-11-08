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
//!   checks the run's own `Reserved.features`.
//! * `PublicOrigins { * }` (H7, D-H2) — only for an employee-scope run (no
//!   task) that lists `web_fetch_cached`/`web_extract`, and only when the admin
//!   enabled protected public web for this employee: `agent.toml [isolation]
//!   public_web = true` (owner decision 2026-11-08). That switch is separate
//!   from `[container] network_access`, which keeps controlling only the task
//!   sandbox's network. `[isolation]` is admin-only (`org_field_guard` freezes
//!   it against the employee's own writes) and defaults to `false`, so the
//!   default is no web authority. Never from tool arguments.
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
    tool_policy::{IN_RUN_HELPER_TOOLS, PUBLIC_ORIGIN_TOOLS}};
use super::VerificationError;

/// File tools served from the brokered `employee-work` root (H6/H11).
pub(crate) const FILE_TOOLS: &[&str] = &["file_read", "csv_read", "xlsx_read"];
/// spawnd's current readiness feature view (`SpawndBackend::features`). Read
/// at issuance only; no view or an error must read as `SpawndFeatures::NONE`.
pub(crate) type SpawndFeatureView = Arc<dyn Fn() -> SpawndFeatures + Send + Sync>;
/// The tool the interactive `NewTask` grant authorizes.
pub(crate) const NEW_TASK_TOOLS: &[&str] = &["create_task"];
/// Upper bound for `agent.toml`; a larger file is not a profile we trust.
const PROFILE_MAX_BYTES: u64 = 1024 * 1024;

/// Trusted producers the selectors may use. Installed by host boot (F1/H5
/// supply the spawnd work broker); absent producers withhold their grant.
#[derive(Clone, Default)]
pub(crate) struct SelectorSources {
    pub(crate) work_broker: Option<Arc<dyn WorkBroker>>,
    /// HX5: spawnd's advertised features; absent ⇒ no helper workspace.
    pub(crate) spawnd_features: Option<SpawndFeatureView>,
}

impl SelectorSources {
    /// Boot's producers: the optional work broker and the protected backend's
    /// readiness feature view (`SpawndBackend::features`; a backend without a
    /// feature source reads as no features, so no helper workspace).
    #[cfg_attr(not(feature = "isolation-worker-integration"), allow(dead_code))]
    pub(crate) fn installed(work_broker: Option<Arc<dyn WorkBroker>>, backend: Arc<duduclaw_execution::SpawndBackend>)
        -> Self {
        Self { work_broker, spawnd_features: Some(Arc::new(move || backend.features())) }
    }
    /// HX5 precondition of the helper workspace: a work broker is installed
    /// and spawnd advertises `helper_calls` now. Anything else ⇒ false.
    fn helper_calls(&self) -> bool {
        self.work_broker.is_some()
            && self.spawnd_features.as_ref().is_some_and(|view| view().contains(SpawndFeatures::HELPER_CALLS))
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
    pub public_web: bool,
    /// The requester of an interactive run's `NewTask` grant.
    pub new_task: Option<String>,
    pub task: Option<SelectedTask>,
}

pub(super) fn select(sources: &SelectorSources, input: &SelectorInput<'_>) -> Result<Selection, VerificationError> {
    if input.task.is_some() && !input.record { return Err(VerificationError::Invalid); }
    let listed = |name: &str| input.tools.as_slice().iter().any(|tool| tool.as_str() == name);
    let wants_files = sources.work_broker.is_some() && FILE_TOOLS.iter().any(|name| listed(*name));
    // HX5: checked only when a helper tool is listed (no view read otherwise).
    let wants_helpers = IN_RUN_HELPER_TOOLS.iter().any(|name| listed(*name)) && sources.helper_calls();
    // D-H2: PublicOrigins exists only alongside EmployeeScope; a task-limited
    // run never carries it, so it is not even considered there.
    let wants_web = input.task.is_none() && PUBLIC_ORIGIN_TOOLS.iter().any(|name| listed(*name));
    // NewTask: interactive runs only (a record-backed run plans under its own
    // record task, H8, and has no human requester of its own).
    let wants_new_task = !input.record && input.task.is_none() && NEW_TASK_TOOLS.iter().any(|name| listed(*name));
    let profile = if wants_files || wants_helpers || wants_web || wants_new_task {
        Some(read_profile(input.home, input.agent)?)
    } else { None };
    let allows = |names: &[&str]| profile.as_ref().is_some_and(|profile| {
        let caps = &profile.capabilities;
        names.iter().any(|name| listed(*name) && duduclaw_core::tool_catalog::tool_list_verdict(
            *name, &caps.denied_tools, &caps.allowed_tools) == duduclaw_core::tool_catalog::ToolListVerdict::Allowed)
    });
    let helpers = wants_helpers && allows(&IN_RUN_HELPER_TOOLS[..]);
    let workspace = (wants_files && allows(FILE_TOOLS)) || helpers;
    // Owner decision 2026-11-08: its own admin-only switch, independent of
    // `[container] network_access` (task sandbox only).
    let public_web = wants_web && profile.as_ref().is_some_and(|profile| profile.isolation.public_web)
        && allows(PUBLIC_ORIGIN_TOOLS);
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
    Ok(Selection { workspace, helpers, public_web, new_task, task })
}

impl Selection {
    /// Add the selected Workspace/PublicOrigins/NewTask grants (and the
    /// brokered root they need) to the issuer's resources. The Task grant is
    /// applied by the issuer through `RuntimeAuthorization::limit_to_task`.
    pub(super) fn apply(&self, sources: &SelectorSources, employee: EmployeeId,
        resources: &mut Vec<ResourceGrant>, roots: &mut ResourceRoots) -> Result<(), VerificationError> {
        if self.workspace {
            let broker = sources.work_broker.clone().ok_or(VerificationError::Unavailable)?;
            roots.insert_brokered(EMPLOYEE_WORK_ROOT.to_owned(), employee, broker)?;
            resources.push(ResourceGrant::Workspace {
                handle: Text::new(EMPLOYEE_WORK_ROOT.to_owned()).map_err(|_| VerificationError::Invalid)? });
        }
        if self.public_web {
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
    /// that grant (the shared pending list, e.g. browser recording until HX6,
    /// and any other A/E tool) is left out of the capability, so it stays
    /// hidden and refused. Only ever narrows.
    pub(super) fn signed_tools(&self, tools: &List<Text<128>, 256>)
        -> Result<List<Text<128>, 256>, VerificationError> {
        if !self.workspace { return Ok(tools.clone()); }
        // A task run is limited to its task and loses EmployeeScope.
        let scoped = self.task.is_none();
        List::new(tools.as_slice().iter().filter(|tool|
            !duduclaw_mcp_boundary::workspace_selection::drops_from_signed(tool.as_str(), scoped, self.helpers))
            .cloned().collect()).map_err(|_| VerificationError::Invalid)
    }
}

/// The employee's current `agent.toml`, read without following symlinks from
/// gateway-private state. Missing, unsafe (symlink, not regular, hard-linked,
/// foreign owner, oversized), unreadable or malformed ⇒ `Unavailable`.
fn read_profile(home: &Path, agent: &str) -> Result<duduclaw_core::types::AgentConfig, VerificationError> {
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
