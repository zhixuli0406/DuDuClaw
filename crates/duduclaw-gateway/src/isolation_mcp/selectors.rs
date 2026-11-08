//! H9 issuer selectors (Package H). For one protected employee's live run they
//! decide which additional resource grants the boot issuer (H10,
//! `boot::human_authorization_with`) signs into the capability:
//!
//! * `Workspace { employee-work }` (H6 brokered root) — only when a root work
//!   broker is installed at boot, the run's tool snapshot lists a file tool and
//!   the employee's current profile (`agent.toml [capabilities]`
//!   `allowed_tools`/`denied_tools`, the same `tool_list_verdict` the ingress
//!   catalog uses) still allows it.
//! * `PublicOrigins { * }` (H7, D-H2) — only for an employee-scope run (no
//!   task) that lists `web_fetch_cached`/`web_extract`, and only when the admin
//!   enabled network access for this employee: `agent.toml [container]
//!   network_access = true`. That section is admin-only (`org_field_guard`
//!   freezes it against the employee's own writes) and defaults to `false`, so
//!   the default is no web authority. Never from tool arguments.
//! * the record `Task { id }` grant (H8) — only for a record-backed
//!   (background) run that names a task, and only when `tasks.db` says now that
//!   the employee's agent owns it (`tasks.assigned_to` + the existing audience
//!   rule, as the H8 resolver checks on every call).
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
use duduclaw_isolation::{bounded::{List, Text}, capability::{PublicOrigin, ResourceGrant}, ids::EmployeeId};
use duduclaw_mcp_boundary::{resources::{ResourceRoots, WorkBroker, EMPLOYEE_WORK_ROOT},
    tool_policy::PUBLIC_ORIGIN_TOOLS};
use super::VerificationError;

/// File tools served from the brokered `employee-work` root (H6/H11).
pub(crate) const FILE_TOOLS: &[&str] = &["file_read", "csv_read", "xlsx_read"];
/// Upper bound for `agent.toml`; a larger file is not a profile we trust.
const PROFILE_MAX_BYTES: u64 = 1024 * 1024;

/// Trusted producers the selectors may use. Installed by host boot (F1/H5
/// supply the spawnd work broker); absent producers withhold their grant.
#[derive(Clone, Default)]
pub(crate) struct SelectorSources {
    pub(crate) work_broker: Option<Arc<dyn WorkBroker>>,
}

/// Facts the issuer passes in; all host-pinned, none from the employee.
pub(super) struct SelectorInput<'a> {
    pub home: &'a Path,
    pub agent: &'a str,
    pub tools: &'a List<Text<128>, 256>,
    /// The run's record task (task/schedule runs only).
    pub task: Option<&'a str>,
    /// The run is record-backed (`HostTurn.background`).
    pub record: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectedTask {
    pub id: String,
    /// The employee's agent owns the task in `tasks.db` at issuance.
    pub owned: bool,
}

/// What the selectors decided. Compared before/after the issuer's I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Selection {
    pub workspace: bool,
    pub public_web: bool,
    pub task: Option<SelectedTask>,
}

pub(super) fn select(sources: &SelectorSources, input: &SelectorInput<'_>) -> Result<Selection, VerificationError> {
    if input.task.is_some() && !input.record { return Err(VerificationError::Invalid); }
    let listed = |name: &str| input.tools.as_slice().iter().any(|tool| tool.as_str() == name);
    let wants_files = sources.work_broker.is_some() && FILE_TOOLS.iter().any(|name| listed(*name));
    // D-H2: PublicOrigins exists only alongside EmployeeScope; a task-limited
    // run never carries it, so it is not even considered there.
    let wants_web = input.task.is_none() && PUBLIC_ORIGIN_TOOLS.iter().any(|name| listed(*name));
    let profile = if wants_files || wants_web { Some(read_profile(input.home, input.agent)?) } else { None };
    let allows = |names: &[&str]| profile.as_ref().is_some_and(|profile| {
        let caps = &profile.capabilities;
        names.iter().any(|name| listed(*name) && duduclaw_core::tool_catalog::tool_list_verdict(
            *name, &caps.denied_tools, &caps.allowed_tools) == duduclaw_core::tool_catalog::ToolListVerdict::Allowed)
    });
    let workspace = wants_files && allows(FILE_TOOLS);
    let public_web = wants_web && profile.as_ref().is_some_and(|profile| profile.container.network_access)
        && allows(PUBLIC_ORIGIN_TOOLS);
    let task = match input.task {
        None => None,
        Some(id) => Some(SelectedTask { id: id.to_owned(), owned: task_owned(input.home, input.agent, id)? }),
    };
    Ok(Selection { workspace, public_web, task })
}

impl Selection {
    /// Add the selected Workspace/PublicOrigins grants (and the brokered root
    /// they need) to the issuer's resources. The Task grant is applied by the
    /// issuer through `RuntimeAuthorization::limit_to_task_with`.
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
        Ok(())
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

/// Live task ownership, same rule as the H8 resolver (`isolation_resources`
/// `task_owned` for the task domain): a `tasks` row with this id assigned to
/// the agent, then the existing audience rule. The store must already exist
/// and pass the private-SQLite checks; it is never created here.
fn task_owned(home: &Path, agent: &str, id: &str) -> Result<bool, VerificationError> {
    use rusqlite::{params, OptionalExtension};
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) { return Err(VerificationError::Invalid); }
    let db = crate::fs_safe::sqlite::open_existing(home, "tasks.db", crate::fs_safe::sqlite::Access::ReadOnly)
        .map_err(|_| VerificationError::Unavailable)?;
    db.busy_timeout(std::time::Duration::from_millis(500)).map_err(|_| VerificationError::Unavailable)?;
    let found = db.query_row("SELECT 1 FROM tasks WHERE id=?1 AND assigned_to=?2", params![id, agent], |_| Ok(()))
        .optional().map_err(|_| VerificationError::Unavailable)?.is_some();
    if !found { return Ok(false); }
    let audience = crate::review_evidence::audience::task_audience(home, id);
    Ok(crate::review_evidence::audience::agent_may_read_task(agent, &[agent], &audience))
}
