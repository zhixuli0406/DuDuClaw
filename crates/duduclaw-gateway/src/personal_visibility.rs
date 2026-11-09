//! Personal employee data visibility (native Linux isolation v1.81, task P1,
//! SR2 finding 5).
//!
//! A Personal employee belongs to exactly one person (the `users.db`
//! `isolation_personal_agents` mirror written by the employee apply path).
//! Surfaces built for Shared employees must not hand its private data —
//! conversations, memory, agent files (SOUL.md, CONTRACT.toml, working state)
//! and wiki pages — to anyone else:
//!
//! - **Team grants** (`duduclaw_auth::org::team_grants_on`,
//!   `overseen_reports_on`) skip Personal employees in SQL, so a manager gets
//!   no derived access and no team-history oversight of a subordinate's
//!   Personal employee.
//! - **Dashboard reads** go through [`classify_reader`]: the owner reads as
//!   before; an Admin who is not the owner gets [`Reader::BreakGlass`] and the
//!   dashboard gate (`handlers/personal_data_gate.rs`) requires a fresh
//!   step-up (the migration/isolation dashboard proof) and writes an audit
//!   row before anything is read; everybody else is refused. Cross-agent
//!   listings hide the Personal employees the caller does not own
//!   ([`hidden_from`]), so listing and access agree.
//! - **Cross-agent management MCP tools** call [`management_refusal`]: an AI
//!   caller never manages another person's Personal employee; human owner
//!   actions go through the dashboard.
//! - **P1b:** the same refusal covers delegation and cross-agent reads
//!   (`spawn_agent`, `send_to_agent`, `check_responses`, `task_status`, memory
//!   namespaces); `list_agents` hides ([`hidden_from_employee`]); an
//!   ephemeral sub-agent acts as its parent ([`acting_employee`]); task,
//!   activity, timeline, decision, canvas and approval reads are category
//!   [`DataCategory::Work`] on the dashboard.
//! - **P1c:** deciding a Personal employee's approval card is category
//!   [`DataCategory::Approval`] (owner, or an audited Admin step-up on the
//!   dashboard; owner only on channels — `crate::personal_decision`);
//!   handing work to one is owner-only on the dashboard
//!   ([`delegation_refusal_for_user`]) and, for AI callers, allowed only from
//!   the employee itself or another Personal employee of the same owner
//!   ([`delegation_refusal`]); tasks and plans named by id resolve their
//!   employee through [`work_item_employee`].
//! - **P1d:** an employee id must be canonical ([`canonical_agent`]: no
//!   surrounding whitespace) before it is compared or used, so the gates and
//!   the handlers always name the same employee; mail is category
//!   [`DataCategory::Mail`]; a goal of the goal hierarchy belongs to the
//!   employee that created it ([`WorkItem::Goal`], [`goal_creators`]).
//! - **P1e:** config / identity writes are categories [`DataCategory::Config`]
//!   / [`DataCategory::Identity`] (owner, or an audited Admin step-up); goal
//!   ownership is metadata only — `goal_created` is reserved
//!   ([`is_reserved_activity_event`]), legacy rows were linked once by a
//!   store migration and title matching is gone — and a goal with several
//!   candidate creators is hidden when any of them is ([`goal_hidden`],
//!   [`work_item_employees`]); discovery runs resolve their employee
//!   ([`WorkItem::DiscoveryRun`]).
//!
//! Every check here runs only in protected mode (`protected = true`, from
//! `runtime_readiness::is_protected_linux()`); otherwise nothing is read and
//! the existing behaviour is unchanged. The mirror is read through the
//! hardened authority-store opener; an unreadable store is [`Unavailable`]
//! and every caller refuses.
//!
//! - **P1h (N2):** the mirror is no longer opened per check. [`owner_map`]
//!   keeps one snapshot per dashboard request (in the request scope) and a
//!   process cache keyed on the ACL generation plus the users.db / WAL file
//!   stamps; every mirror write bumps the generation and the lifecycle also
//!   calls [`invalidate_owner_cache`]. A read error is never cached.
//! - **P1h (N1):** calling or assigning a Personal employee from a channel
//!   (`trigger_route`) is delegation: the mapped person must be its owner,
//!   Admins included; an unmapped sender is no person — only an
//!   owner-configured `[channels] public_trigger` opens calling (never
//!   assigning). **N4:** the fire-time "acting employee" exception applies
//!   only when no principal is recorded.
//! - **SR5 item 2:** privacy does not end at the tombstone. The owner map
//!   also carries the owner of every OPEN Personal quarantine row in apply.db
//!   (`isolation_label_stores::open_personal_owners`; closed = `purged`,
//!   `adopted`, `restored`, or `purging` from `adopted`), so for the whole
//!   retention window a tombstoned Personal employee stays hidden from
//!   cross-agent listings, needs the audited Admin step-up for its sessions /
//!   memory / timeline / mail, and is refused to other AI employees. An
//!   unreadable apply.db is [`Unavailable`]. The cache key includes the
//!   apply.db / WAL stamps; tombstone, purge and adoption also invalidate.
use std::collections::BTreeSet;
use std::path::Path;

use crate::fs_safe::sqlite::{Access, open_existing};

const USERS_DB: &str = "users.db";

/// P1g: the ACL decision installed into `duduclaw_auth::acl` at gateway boot.
/// `true` = `user_id` must not reach `agent` through any ACL predicate: a
/// Personal employee recorded for another owner, or a users.db that cannot
/// be read (fail closed, every employee). The owner and Shared / unknown
/// employees are not affected; nothing is read outside protected mode.
pub fn acl_denies(home: &Path, protected: bool, user_id: &str, agent: &str) -> bool {
    if !protected {
        return false;
    }
    // `is_admin = false`: the ACL never grants break-glass — that is the
    // audited step-up of the dashboard gate, not a role shortcut.
    !matches!(classify_reader(home, true, user_id, false, agent), Ok(Reader::Unrestricted | Reader::Owner))
}

/// One dashboard request's Personal state (P1g break-glass, P1h owner map).
#[derive(Default)]
struct RequestScope {
    /// P1g: `(user, employee)` pairs an audited Admin step-up admitted during
    /// THIS request (`personal_data_gate` records each successful break-glass).
    break_glass: std::cell::RefCell<BTreeSet<(String, String)>>,
    /// P1h (N2): the owner map read once for this request, per home. A
    /// request answers from one ownership snapshot; [`invalidate_owner_cache`]
    /// drops it (a lifecycle write inside the request).
    owners: std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, std::sync::Arc<OwnerMap>>>,
}

tokio::task_local! {
    /// `MethodHandler::handle_conn` opens the scope around each request. Only
    /// the request's own task sees it: a spawned task, a blocking-pool
    /// closure, an HTTP route or a background job has no scope — break-glass
    /// stays refused there and the owner map comes from the process cache.
    static REQUEST: RequestScope;
}

/// P1g: run `future` (one dashboard request) with an empty break-glass set
/// (P1h: and an empty per-request owner map).
pub async fn with_break_glass_scope<F: std::future::Future>(future: F) -> F::Output {
    REQUEST.scope(RequestScope::default(), future).await
}

/// P1g: record that `user` passed the audited step-up for `agent` in the
/// current request. No-op outside a request scope.
pub fn record_break_glass(user: &str, agent: &str) {
    let _ = REQUEST.try_with(|scope| scope.break_glass.borrow_mut().insert((user.to_owned(), agent.to_owned())));
}

/// P1g: `user` passed the audited step-up for `agent` in the current request.
pub fn break_glass_granted(user: &str, agent: &str) -> bool {
    REQUEST
        .try_with(|scope| scope.break_glass.borrow().contains(&(user.to_owned(), agent.to_owned())))
        .unwrap_or(false)
}

/// P1g: the restriction object (`home` fixed at boot; protected mode read
/// on every call so an unprotected process never touches users.db).
pub struct AclRestriction {
    home: std::path::PathBuf,
    /// `None` = the process flag (production); tests pass it explicitly.
    protected: Option<bool>,
}

impl AclRestriction {
    /// A restriction with an explicit protected flag (tests: no test sets
    /// the process-wide flag).
    pub fn with_flag(home: &Path, protected: bool) -> Self {
        Self { home: home.to_path_buf(), protected: Some(protected) }
    }

    fn protected(&self) -> bool {
        self.protected.unwrap_or_else(crate::runtime_readiness::is_protected_linux)
    }
}

impl duduclaw_auth::acl::PersonalRestriction for AclRestriction {
    fn denies(&self, user_id: &str, agent: &str) -> bool {
        acl_denies(&self.home, self.protected(), user_id, agent) && !break_glass_granted(user_id, agent)
    }

    /// Delegation never uses a break-glass grant (owner only).
    fn denies_delegation(&self, user_id: &str, agent: &str) -> bool {
        acl_denies(&self.home, self.protected(), user_id, agent)
    }
}

/// P1g: install [`AclRestriction`] for this gateway's home (once per
/// process; `server.rs::start_gateway`).
pub fn install_acl_restriction(home: &Path) {
    let restriction = AclRestriction { home: home.to_path_buf(), protected: None };
    if !duduclaw_auth::acl::install_personal_restriction(Box::new(restriction)) {
        tracing::warn!("Personal ACL restriction was already installed");
    }
}

/// The private data category named in the audit row of an Admin read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataCategory {
    /// Conversations / session history / transcripts and cross-source search.
    Sessions,
    /// The employee's memory store.
    Memory,
    /// Profile and working files (SOUL.md, CONTRACT.toml, agent.toml, …).
    AgentFiles,
    /// The employee's own wiki pages.
    Wiki,
    /// Its work record (P1b): tasks and their detail, activity, timeline,
    /// open decisions, canvas and approval payloads.
    Work,
    /// A decision on one of its approval cards (P1c): `approvals.decide`,
    /// `mail.decide`.
    Approval,
    /// Its received and drafted mail (P1d): `mail.list`, `mail.read`,
    /// `mail.archive`, `mail.outbox`.
    Mail,
    /// A WRITE of its configuration (P1e): `agents.update` (agent.toml,
    /// display name into SOUL/IDENTITY, model, tools, sources, supervisor,
    /// heartbeat), avatar / outfit, tool-pruning restore, archive, removal
    /// and handing its work over.
    Config,
    /// A WRITE of its acting Google identity (P1e): `agents.google_identity_set`.
    Identity,
}

impl DataCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Memory => "memory",
            Self::AgentFiles => "agent_files",
            Self::Wiki => "wiki",
            Self::Work => "work",
            Self::Approval => "approval",
            Self::Mail => "mail",
            Self::Config => "config",
            Self::Identity => "identity",
        }
    }

    /// P1e: a configuration / identity write (audited under its own action,
    /// never as a read).
    pub fn is_write(self) -> bool {
        matches!(self, Self::Config | Self::Identity)
    }
}

/// A work item named by id in a tool argument (P1c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItem {
    /// `tasks.db` `tasks.id` → `assigned_to`.
    Task,
    /// `tasks.db` `plans.id` → `agent_id`.
    Plan,
    /// `tasks.db` `goals.id` → the employees whose `goal_created` activity
    /// rows link it (P1d/P1e, [`goal_creators`]). A goal no employee created
    /// (dashboard / company goal) has none.
    Goal,
    /// P1e: a discovery run id (`discovery_tree` / `discovery_artifact` /
    /// `discovery_cancel`) → `tasks.db` `tasks.assigned_to` of its task.
    DiscoveryRun,
}

/// The activity event type that records a goal's creating employee (P1d).
/// P1e: reserved — only `goals_create` writes it (MCP `activity_post`
/// refuses it, [`is_reserved_activity_event`]).
pub const GOAL_CREATED_EVENT: &str = "goal_created";

/// P1e: an activity `event_type` an employee may not post because it carries
/// ownership: [`GOAL_CREATED_EVENT`] in any case / surrounding whitespace.
pub fn is_reserved_activity_event(event_type: &str) -> bool {
    event_type.trim().eq_ignore_ascii_case(GOAL_CREATED_EVENT)
}

/// P1e: the goal ids one `goal_created` row links: `metadata.goal_id` and
/// `metadata.goal_ids` (written by the legacy backfill for a row whose title
/// matched several goals). Malformed metadata links nothing. There is no
/// title matching any more: legacy rows were linked once by the store
/// migration (`task_store::goal_creator_backfill`).
pub fn linked_goals(metadata: Option<&str>) -> Vec<String> {
    let Some(value) = metadata.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()) else {
        return Vec::new();
    };
    let mut goals: Vec<String> = value.get("goal_id").and_then(serde_json::Value::as_str).map(str::to_owned)
        .into_iter().collect();
    if let Some(list) = value.get("goal_ids").and_then(serde_json::Value::as_array) {
        goals.extend(list.iter().filter_map(serde_json::Value::as_str).map(str::to_owned));
    }
    goals.retain(|goal| !goal.is_empty());
    goals
}

/// P1g (F7): the metadata of a [`GOAL_CREATED_EVENT`] row when it is exactly
/// a goal link — an object whose keys are only `goal_id` / `goal_ids` /
/// `goal_backfill`, with id-shaped values (`[A-Za-z0-9_-]`, ≤ 128). Such
/// metadata is stored unmasked (canonical JSON); anything else is `None`
/// and goes through the normal redaction.
pub fn goal_link_metadata(event_type: &str, raw: &str) -> Option<String> {
    if event_type != GOAL_CREATED_EVENT {
        return None;
    }
    let id_shaped = |value: &serde_json::Value| {
        value.as_str().is_some_and(|id| {
            !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
    };
    let serde_json::Value::Object(map) = serde_json::from_str::<serde_json::Value>(raw).ok()? else {
        return None;
    };
    let valid = !map.is_empty()
        && map.iter().all(|(key, value)| match key.as_str() {
            "goal_id" | "goal_backfill" => id_shaped(value),
            "goal_ids" => value.as_array().is_some_and(|ids| !ids.is_empty() && ids.iter().all(id_shaped)),
            _ => false,
        })
        && (map.contains_key("goal_id") || map.contains_key("goal_ids"));
    valid.then(|| serde_json::Value::Object(map).to_string())
}

/// Shown when an employee id carries surrounding whitespace (P1d).
pub const NON_CANONICAL_AGENT: &str = "AI 員工代號前後不能有空白。 / An employee id must not have leading or \
    trailing whitespace.";

/// P1d: `raw` as a canonical employee id. Every gate and handler compares
/// and uses this exact value; an id with surrounding whitespace is refused
/// rather than trimmed, so a gate can never check one string while the
/// handler acts on another.
pub fn canonical_agent(raw: &str) -> Result<&str, &'static str> {
    if raw.trim() == raw { Ok(raw) } else { Err(NON_CANONICAL_AGENT) }
}

/// What a dashboard caller may do with one employee's private data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reader {
    /// Not protected mode, or not a Personal employee: the existing checks
    /// decide alone.
    Unrestricted,
    /// The caller is the employee's recorded owner.
    Owner,
    /// An Admin who is not the owner: only with a fresh step-up, audited.
    BreakGlass { owner: String },
    /// Anyone else, including the legacy `system` admin context.
    Refused,
}

/// The `users.db` mirror could not be read; callers must refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable;

/// Shown to an AI caller that targets another person's Personal employee.
pub const PERSONAL_TARGET_REFUSED: &str = "這位 AI 員工是某位同仁的個人專屬員工，其他 AI 員工不能讀取、委派工作給它，\
    也不能修改或移除它的設定與檔案。如需變更，請由擁有者在控制台操作。 / This is a person's Personal employee; another \
    AI employee cannot read, delegate to, change or remove it. The owner manages it from the dashboard.";
/// Shown when the Personal mirror cannot be read (fail closed).
pub const OWNERSHIP_UNAVAILABLE: &str = "無法確認這位 AI 員工的歸屬，已拒絕操作。 / Employee ownership could not be \
    verified; the operation was refused.";
/// Shown when a dashboard user who is not the owner hands work to a
/// Personal employee (P1c).
pub const PERSONAL_DELEGATION_REFUSED: &str = "這位 AI 員工是某位同仁的個人專屬員工，只有它的擁有者能交辦工作給它。 / This is a \
    person's Personal employee; only its owner can hand work to it.";
/// Shown when someone who is not the owner decides a Personal employee's
/// approval card where no step-up is possible (channels, chat replies) or
/// is not an Admin (P1c).
pub const PERSONAL_DECISION_REFUSED: &str = "這張審核卡屬於某位同仁的個人專屬員工，只有它的擁有者能決定。 / This approval \
    belongs to a person's Personal employee; only its owner can decide it.";

fn open(home: &Path) -> Result<rusqlite::Connection, Unavailable> {
    #[cfg(test)]
    count_open(home);
    open_existing(home, USERS_DB, Access::ReadOnly).map_err(|_| Unavailable)
}

/// P1h (N2): every Personal employee → its recorded owner, one snapshot.
pub type OwnerMap = std::collections::BTreeMap<String, String>;

/// P1h (N2): what a process-cache entry was read under. Reused only while
/// BOTH the ACL generation (bumped by every mirror write in the auth crate:
/// insert, owner bind, lifecycle removal) and the users.db / WAL file stamps
/// (size, mtime, inode, mode, owner — also catches a raw write that bypassed
/// the generation, and a permission change the hardened opener would refuse)
/// are unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerKey {
    generation: u64,
    /// SR5 item 2: users.db, users.db-wal, apply.db, apply.db-wal (a
    /// quarantine row opened / closed by tombstone, restore, adoption or
    /// purge changes the owner map too).
    files: [Option<FileStamp>; 4],
}

/// SR5 item 2: the lifecycle index holding the quarantine rows.
const APPLY_DB_PATH: [&str; 2] = ["migration-authority", "apply.db"];
const APPLY_WAL_PATH: [&str; 2] = ["migration-authority", "apply.db-wal"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, u32, u32),
}

fn file_stamp(path: &Path) -> Result<Option<FileStamp>, Unavailable> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(FileStamp {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino(), meta.mode(), meta.uid())
            },
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Unavailable),
    }
}

/// The key observed NOW (before any fresh read, so an entry is never older
/// than its key). `None` = no usable generation: the cache is bypassed.
fn owner_key(home: &Path) -> Option<OwnerKey> {
    // Generation first: its persistent read connection may create the WAL
    // sidecar on first use, which the stamps below then already include.
    let generation = crate::trigger_route::access::read_generation(home)?;
    let apply = |parts: [&str; 2]| home.join(parts[0]).join(parts[1]);
    let files = [
        file_stamp(&home.join(USERS_DB)).ok()?,
        file_stamp(&home.join("users.db-wal")).ok()?,
        file_stamp(&apply(APPLY_DB_PATH)).ok()?,
        file_stamp(&apply(APPLY_WAL_PATH)).ok()?,
    ];
    files[0].as_ref()?;
    Some(OwnerKey { generation, files })
}

type OwnerCache = std::collections::HashMap<std::path::PathBuf, (OwnerKey, std::sync::Arc<OwnerMap>)>;

fn owner_cache() -> std::sync::MutexGuard<'static, OwnerCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<OwnerCache>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| std::sync::Mutex::new(OwnerCache::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// P1h (N2): drop every cached owner map of `home` (the process cache and
/// the current request's snapshot). Called after each Personal mirror
/// write of the lifecycle (`isolation_lifecycle::record_personal_mirror` /
/// `remove_personal_mirror`); the generation key would also miss.
pub fn invalidate_owner_cache(home: &Path) {
    owner_cache().remove(home);
    let _ = REQUEST.try_with(|scope| scope.owners.borrow_mut().remove(home));
}

/// The full mirror through the hardened authority-store opener. Any read
/// error is [`Unavailable`] (callers refuse) and is never cached.
///
/// SR5 item 2: plus the owner of every OPEN Personal quarantine row in
/// apply.db (`isolation_label_stores::open_personal_owners`). A tombstone
/// removes the users.db mirror, but the employee's data is kept sealed for
/// the retention window, so its privacy rules (hidden rows, break-glass,
/// AI-side refusals) stay until the row is purged, adopted or restored. A
/// live mirror row wins over a quarantine row on the same label (a restore
/// re-records it). An unreadable apply.db is [`Unavailable`].
fn read_owner_map(home: &Path) -> Result<OwnerMap, Unavailable> {
    let conn = open(home)?;
    let mut statement = conn
        .prepare("SELECT agent_name, owner_user_id FROM isolation_personal_agents")
        .map_err(|_| Unavailable)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|_| Unavailable)?;
    let mut map: OwnerMap = rows.collect::<Result<_, _>>().map_err(|_| Unavailable)?;
    // Oldest incarnation first: a later open row on the label wins.
    let quarantined: OwnerMap =
        crate::isolation_label_stores::open_personal_owners(home).map_err(|_| Unavailable)?.into_iter().collect();
    for (label, owner) in quarantined {
        map.entry(label).or_insert(owner);
    }
    Ok(map)
}

/// P1h (N2): the owner map for one ACL check. The current request's snapshot
/// if it has one; else the process cache when its key is still current; else
/// one fresh read (cached under the key observed BEFORE the read). So a
/// request with N rows opens users.db at most once, and a request served from
/// a current process entry not at all.
pub fn owner_map(home: &Path) -> Result<std::sync::Arc<OwnerMap>, Unavailable> {
    if let Ok(Some(map)) = REQUEST.try_with(|scope| scope.owners.borrow().get(home).cloned()) {
        return Ok(map);
    }
    let key = owner_key(home);
    let cached = key.as_ref().and_then(|key| {
        owner_cache().get(home).filter(|(at, _)| at == key).map(|(_, map)| map.clone())
    });
    let map = match cached {
        Some(map) => map,
        None => {
            let map = std::sync::Arc::new(read_owner_map(home)?);
            if let Some(key) = key {
                // One entry per gateway home (a process serves one); the
                // bound only stops unbounded growth.
                let mut cache = owner_cache();
                if cache.len() >= 4_096 {
                    cache.clear();
                }
                cache.insert(home.to_path_buf(), (key, map.clone()));
            }
            map
        }
    };
    let _ = REQUEST.try_with(|scope| scope.owners.borrow_mut().insert(home.to_path_buf(), map.clone()));
    Ok(map)
}

#[cfg(test)]
fn opens() -> std::sync::MutexGuard<'static, std::collections::HashMap<std::path::PathBuf, usize>> {
    static OPENS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>> =
        std::sync::OnceLock::new();
    OPENS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn count_open(home: &Path) {
    *opens().entry(home.to_path_buf()).or_default() += 1;
}

/// Test probe: how many times the mirror was opened for `home`.
#[cfg(test)]
pub(crate) fn open_count(home: &Path) -> usize {
    opens().get(home).copied().unwrap_or(0)
}

/// The recorded Personal owner of `agent` ([`owner_map`]: current as of the
/// last mirror write). `Ok(None)` = Shared or unknown employee. Evidence
/// only; the run-time authority still checks the full lifecycle.
pub fn personal_owner(home: &Path, agent: &str) -> Result<Option<String>, Unavailable> {
    Ok(owner_map(home)?.get(agent).cloned())
}

/// P1g: every Personal employee → its recorded owner (list metadata).
pub fn personal_owners(home: &Path) -> Result<OwnerMap, Unavailable> {
    Ok(owner_map(home)?.as_ref().clone())
}

/// Personal employees whose private data `user_id` does not own, for hiding
/// them from cross-agent listings. Empty outside protected mode.
pub fn hidden_from(home: &Path, protected: bool, user_id: &str) -> Result<BTreeSet<String>, Unavailable> {
    if !protected {
        return Ok(BTreeSet::new());
    }
    Ok(owner_map(home)?.iter().filter(|(_, owner)| owner.as_str() != user_id).map(|(agent, _)| agent.clone()).collect())
}

/// Decide how a dashboard caller may read `agent`'s private data.
/// `user_id`/`is_admin` come from the live (users.db) caller context.
pub fn classify_reader(
    home: &Path,
    protected: bool,
    user_id: &str,
    is_admin: bool,
    agent: &str,
) -> Result<Reader, Unavailable> {
    if !protected {
        return Ok(Reader::Unrestricted);
    }
    // The legacy token / local-mode context has no person behind it.
    let person = !user_id.is_empty() && user_id != "system";
    Ok(match personal_owner(home, agent)? {
        None => Reader::Unrestricted,
        Some(owner) if person && owner == user_id => Reader::Owner,
        Some(owner) if person && is_admin => Reader::BreakGlass { owner },
        Some(_) => Reader::Refused,
    })
}

/// The regular employee an acting AI id works as (P1b). A regular id is
/// itself (no I/O). An ephemeral sub-agent (`eph-…`) is the root of its
/// host-recorded parent chain, `duduclaw_core::autonomy_gate::
/// effective_identities`: at every hop the scaffold's `ephemeral.toml`
/// `parent` must equal the host-owned org store's `reports_to`, so neither
/// the sub-agent's own files nor a tool argument can choose its parent.
/// `None` = blank id or an unresolvable chain (callers refuse).
pub fn acting_employee(home: &Path, acting: &str) -> Option<String> {
    if acting.is_empty() {
        return None;
    }
    if !crate::ephemeral::is_ephemeral_id(acting) {
        return Some(acting.to_owned());
    }
    duduclaw_core::autonomy_gate::effective_identities(
        home,
        acting,
        duduclaw_core::autonomy_gate::AutonomyPath::Background,
    )
    .ok()?
    .pop()
}

/// `acting` is `target` itself, or an ephemeral sub-agent of `target`
/// ([`acting_employee`]). Exact id equality; never reads `users.db`.
pub fn same_employee(home: &Path, acting: &str, target: &str) -> bool {
    if acting.is_empty() || target.is_empty() {
        return false;
    }
    acting == target
        || (crate::ephemeral::is_ephemeral_id(acting)
            && acting_employee(home, acting).as_deref() == Some(target))
}

/// Personal employees an AI caller must not see in a cross-agent listing
/// (MCP `list_agents`): every one except the acting employee itself (an
/// ephemeral sub-agent counts as its parent). Other AI employees get no
/// metadata at all — the row is hidden, not redacted. Empty outside
/// protected mode; an unreadable store is [`Unavailable`].
pub fn hidden_from_employee(home: &Path, protected: bool, acting: &str)
    -> Result<BTreeSet<String>, Unavailable> {
    if !protected {
        return Ok(BTreeSet::new());
    }
    let me = acting_employee(home, acting);
    let mut hidden: BTreeSet<String> = owner_map(home)?.keys().cloned().collect();
    if let Some(me) = me {
        hidden.remove(&me);
    }
    Ok(hidden)
}

/// Refusal for a cross-agent MCP tool (`agent_update`, `agent_remove`,
/// `spawn_agent`, `check_responses`, …) whose `target` is a Personal
/// employee and whose acting employee is not that employee itself (an
/// ephemeral sub-agent of it counts as it, [`same_employee`]). A self-target
/// is decided before any `users.db` read. `None` = allowed (the tool's own
/// checks still apply).
pub fn management_refusal(home: &Path, protected: bool, acting_agent: &str, target: &str)
    -> Option<&'static str> {
    if !protected || same_employee(home, acting_agent, target) {
        return None;
    }
    match personal_owner(home, target) {
        Ok(None) => None,
        Ok(Some(_)) => Some(PERSONAL_TARGET_REFUSED),
        Err(Unavailable) => Some(OWNERSHIP_UNAVAILABLE),
    }
}

/// P1c: an AI caller handing work to `target` (MCP `spawn_agent`,
/// `send_to_agent`, `tasks_create`/`tasks_update` `assigned_to`, …). As
/// [`management_refusal`], except that an acting employee which is itself a
/// Personal employee of the same owner may hand work to it: that is the
/// owner's own work moving between the owner's own employees. Both owners
/// are read fresh; an unreadable store refuses. `None` = allowed.
pub fn delegation_refusal(home: &Path, protected: bool, acting_agent: &str, target: &str)
    -> Option<&'static str> {
    let refusal = management_refusal(home, protected, acting_agent, target)?;
    if refusal != PERSONAL_TARGET_REFUSED {
        return Some(refusal);
    }
    let Some(me) = acting_employee(home, acting_agent) else {
        return Some(refusal);
    };
    match (personal_owner(home, &me), personal_owner(home, target)) {
        (Ok(Some(mine)), Ok(Some(theirs))) if !mine.is_empty() && mine == theirs => None,
        (Err(Unavailable), _) | (_, Err(Unavailable)) => Some(OWNERSHIP_UNAVAILABLE),
        _ => Some(refusal),
    }
}

/// P1c: a dashboard user (`user_id` from the live users.db context) handing
/// work to `agent` (`tasks.create`/`tasks.update` `assigned_to`,
/// `tasks.goal_create`/`plans.create` `agent_id`). A Personal employee takes
/// work only from its owner — an Admin who is not the owner is refused too
/// (delegation has no break-glass). `None` = allowed (the existing checks
/// still apply). Empty outside protected mode.
pub fn delegation_refusal_for_user(home: &Path, protected: bool, user_id: &str, agent: &str)
    -> Option<&'static str> {
    if !protected {
        return None;
    }
    match classify_reader(home, true, user_id, false, agent) {
        Ok(Reader::Unrestricted | Reader::Owner) => None,
        Ok(Reader::BreakGlass { .. } | Reader::Refused) => Some(PERSONAL_DELEGATION_REFUSED),
        Err(Unavailable) => Some(OWNERSHIP_UNAVAILABLE),
    }
}

/// P1g: shown when a queued / scheduled job would run work on a Personal
/// employee for someone other than its owner (checked at fire time).
pub const PERSONAL_FIRE_REFUSED: &str = "這項排程或排隊中的工作要交給某位同仁的個人專屬員工，但不是由它的擁有者交辦，\
    已在執行前拒絕。 / This scheduled or queued job targets a person's Personal employee and was not handed over by \
    its owner; it was refused before it ran.";

/// P1g: the dashboard user a stored principal names (`user:<id>`, or a bare
/// account id from a creator column). `None` for unmapped senders, system
/// senders and malformed values.
pub fn principal_user(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let parsed = if raw.contains(':') {
        duduclaw_core::human_principal::HumanPrincipal::parse(raw).ok()
    } else {
        duduclaw_core::human_principal::HumanPrincipal::user(raw)
    };
    parsed.and_then(|principal| principal.user_id().map(str::to_owned))
}

/// P1g fire-time check, run by every queued / scheduled job right before it
/// makes `target` run work (dispatcher, cron, reminders, workflow routines):
/// a Personal employee runs only work handed over by its owner (`principals`:
/// the stored `on_behalf_of` / creator / editor — every one that is set must
/// be the owner, at least one must be) or by the employee itself / another
/// Personal employee of the same owner (`acting`, [`delegation_refusal`]).
/// Ownership is read NOW, so rules created before the upgrade (or before an
/// adoption changed the owner) are covered. Unreadable store ⇒ refused.
/// `None` = allowed (outside protected mode always).
///
/// P1h (N4, as B1 in `isolation_background::personal_delegation::gate`): the
/// `acting` exception applies only when NO principal is recorded (every
/// value absent or blank). A recorded principal that is not the owner — an
/// Admin, an unmapped sender, `system`, a malformed value — always refuses,
/// whatever employee the row claims sent it (a forged JSONL bus line naming
/// the target as its own sender cannot launder it).
pub fn fire_time_refusal(home: &Path, protected: bool, principals: &[Option<&str>], acting: Option<&str>, target: &str)
    -> Option<&'static str> {
    if !protected {
        return None;
    }
    let owner = match personal_owner(home, target) {
        Ok(None) => return None,
        Ok(Some(owner)) => owner,
        Err(Unavailable) => return Some(OWNERSHIP_UNAVAILABLE),
    };
    let named: Vec<Option<String>> = principals
        .iter()
        .flatten()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| principal_user(raw))
        .collect();
    if !named.is_empty() {
        let all_owner = !owner.is_empty() && named.iter().all(|user| user.as_deref() == Some(owner.as_str()));
        return if all_owner { None } else { Some(PERSONAL_FIRE_REFUSED) };
    }
    if let Some(acting) = acting.filter(|acting| !acting.is_empty())
        && delegation_refusal(home, true, acting, target).is_none()
    {
        return None;
    }
    Some(PERSONAL_FIRE_REFUSED)
}

/// P1c: the employee a task or plan named by `id` belongs to, read from
/// `tasks.db` without opening a writable store. `Ok(None)` = no such item
/// (the tool answers); `Err` = the store could not be read (callers refuse).
/// P1e: a goal can have several candidate creators; this returns the first
/// (sorted) — a gate must use [`work_item_employees`], which returns all.
pub fn work_item_employee(home: &Path, item: WorkItem, id: &str) -> Result<Option<String>, Unavailable> {
    work_item_employees(home, item, id).map(|employees| employees.into_iter().next())
}

/// P1e: every employee the item `id` could belong to. A task, plan or
/// discovery run has at most one; a goal has every employee a
/// `goal_created` row links it to ([`goal_creators`]), so a gate that
/// refuses when ANY of them is refused fails closed when ownership is
/// ambiguous. Blank employee ids are dropped (the item belongs to nobody).
pub fn work_item_employees(home: &Path, item: WorkItem, id: &str) -> Result<BTreeSet<String>, Unavailable> {
    use rusqlite::OptionalExtension;
    let sql = match item {
        WorkItem::Task => "SELECT assigned_to FROM tasks WHERE id=?1",
        WorkItem::Plan => "SELECT agent_id FROM plans WHERE id=?1",
        WorkItem::DiscoveryRun => "SELECT assigned_to FROM tasks WHERE discovery_run_id=?1",
        WorkItem::Goal => return goal_creators(home).map(|mut creators| creators.remove(id).unwrap_or_default()),
    };
    let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| Unavailable)?;
    let employee = conn
        .query_row(sql, [id], |row| row.get::<_, Option<String>>(0))
        .optional()
        .map(Option::flatten)
        .map_err(|_| Unavailable)?;
    Ok(employee.into_iter().filter(|agent| !agent.is_empty()).collect())
}

/// P1d/P1e: goal id → every employee a [`GOAL_CREATED_EVENT`] row links it
/// to ([`linked_goals`]: metadata only, never the title). Several distinct
/// creators can only come from rows forged before P1e reserved the event
/// type, or from an ambiguous legacy backfill; callers hide the goal when
/// ANY of them is hidden (fail closed). `Err` = `tasks.db` could not be read.
pub fn goal_creators(home: &Path)
    -> Result<std::collections::BTreeMap<String, BTreeSet<String>>, Unavailable> {
    let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| Unavailable)?;
    let mut statement = conn
        .prepare("SELECT agent_id, metadata FROM activity WHERE event_type = ?1")
        .map_err(|_| Unavailable)?;
    let rows = statement
        .query_map([GOAL_CREATED_EVENT], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))
        .map_err(|_| Unavailable)?;
    let mut creators: std::collections::BTreeMap<String, BTreeSet<String>> = std::collections::BTreeMap::new();
    // P1g (F7): employees with a `goal_created` row that links no goal (a
    // legacy row the one-time backfill could not match, or a link the old
    // masker broke). Which goal it created is unknown, so every goal no row
    // links is held by these employees until the row is reviewed (linked).
    let mut unreviewed = BTreeSet::new();
    for row in rows {
        let (agent, metadata) = row.map_err(|_| Unavailable)?;
        if agent.is_empty() {
            continue;
        }
        let goals = linked_goals(metadata.as_deref());
        if goals.is_empty() {
            unreviewed.insert(agent.clone());
        }
        for goal in goals {
            creators.entry(goal).or_default().insert(agent.clone());
        }
    }
    drop(statement);
    if !unreviewed.is_empty() {
        let mut goals = conn.prepare("SELECT id FROM goals").map_err(|_| Unavailable)?;
        let ids = goals.query_map([], |row| row.get::<_, String>(0)).map_err(|_| Unavailable)?;
        for id in ids {
            let id = id.map_err(|_| Unavailable)?;
            creators.entry(id).or_insert_with(|| unreviewed.clone());
        }
    }
    Ok(creators)
}

/// P1e: `true` when a goal with these creators must be left out of a
/// listing for a caller from whom `hidden` is hidden: ANY creator hidden.
pub fn goal_hidden(creators: Option<&BTreeSet<String>>, hidden: &BTreeSet<String>) -> bool {
    creators.is_some_and(|creators| !creators.is_disjoint(hidden))
}

/// Shared with the dashboard gate tests (`handlers/personal_data_gate_tests.rs`).
#[cfg(all(test, unix))]
#[path = "personal_visibility_tests.rs"]
pub(crate) mod tests;
