//! The workspace registry: `<home>/computer_workspaces.db` (design §3).
//!
//! Every lease change is a conditional update inside one SQLite transaction
//! (`BEGIN IMMEDIATE`): acquire, renew, release, fence, revoke and the
//! expiry sweep all name the epoch / holder they expect, and an update that
//! touches zero rows changes nothing. `lease_epoch` is the only authority
//! that crosses processes; the session lock never decides ownership.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

use super::paths::DB_FILE;
use super::state::WorkspaceState;

/// sha256 of an empty manifest.
pub const EMPTY_MANIFEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Why a registry call refused or failed. Closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The database could not be opened / read / written.
    Unavailable(String),
    /// No such workspace for this caller (also: someone else's).
    NotFound,
    /// The workspace exists but its state forbids this.
    State(WorkspaceState),
    /// Bound to another Docker daemon.
    RunnerMismatch,
    /// Another live lease holds it.
    Busy,
    /// `max_per_agent` reached.
    Quota,
    /// The caller's lease is no longer the current one.
    LeaseLost,
    /// `expected_revision` did not match.
    RevisionMismatch(i64),
}

pub(super) fn db(e: impl std::fmt::Display) -> StoreError {
    StoreError::Unavailable(e.to_string())
}

/// One registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub workspace_id: String,
    pub owner_agent_id: String,
    pub runner_id: String,
    pub state: WorkspaceState,
    pub state_reason: Option<String>,
    pub created_at: i64,
    pub last_attached_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub image_digest: Option<String>,
    pub data_revision: i64,
    pub manifest_hash: String,
    pub bytes_used: i64,
    pub files_used: i64,
    pub permission_revision: i64,
    pub lease_epoch: i64,
    pub lease_holder: Option<String>,
    pub lease_instance: Option<String>,
    pub lease_until: Option<i64>,
    /// The owner credential (review M-5); `None` on a row created before
    /// it existed, which [`super::owner_cred::matches`] treats as no match.
    pub owner_credential: Option<String>,
}

impl WorkspaceRow {
    /// Whether a lease is live at `now` (design §3.5).
    pub fn lease_active(&self, now: i64) -> bool {
        self.state == WorkspaceState::Ready
            && self.lease_holder.is_some()
            && self.lease_until.is_some_and(|u| u > now)
    }
}

/// What a session holds after a successful acquire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub workspace_id: String,
    pub epoch: i64,
    /// The holder session id (`cu-…`).
    pub holder: String,
    pub permission_revision: i64,
}

/// Inputs of an acquire.
pub struct AcquireRequest<'a> {
    pub workspace_id: &'a str,
    pub caller: &'a str,
    pub runner_id: &'a str,
    pub holder: &'a str,
    pub instance: &'a str,
    pub now: i64,
    pub ttl_secs: i64,
    pub retention_days: u32,
}

/// One pending write (crash consistency, design §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteIntent {
    pub intent_id: String,
    pub workspace_id: String,
    pub rel_path_hash: String,
    pub temp_name: String,
    pub target_sha256: Option<String>,
    pub lease_epoch: i64,
    pub created_at: i64,
}

/// The registry handle.
pub struct WorkspaceStore {
    pub(super) conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS workspaces (
  workspace_id TEXT PRIMARY KEY,
  owner_agent_id TEXT NOT NULL,
  runner_id TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_attached_at INTEGER,
  expires_at INTEGER,
  image_digest TEXT,
  data_revision INTEGER NOT NULL DEFAULT 0,
  manifest_hash TEXT NOT NULL,
  bytes_used INTEGER NOT NULL DEFAULT 0,
  files_used INTEGER NOT NULL DEFAULT 0,
  permission_revision INTEGER NOT NULL DEFAULT 1,
  lease_epoch INTEGER NOT NULL DEFAULT 0,
  lease_holder TEXT,
  lease_instance TEXT,
  lease_until INTEGER,
  state_reason TEXT,
  owner_credential TEXT
);
CREATE INDEX IF NOT EXISTS workspaces_owner ON workspaces(owner_agent_id, state);
CREATE TABLE IF NOT EXISTS workspace_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id TEXT NOT NULL, at INTEGER NOT NULL, kind TEXT NOT NULL,
  actor TEXT NOT NULL,
  lease_epoch INTEGER, permission_revision INTEGER, data_revision INTEGER,
  detail_json TEXT
);
CREATE INDEX IF NOT EXISTS workspace_events_ws ON workspace_events(workspace_id, kind);
CREATE TABLE IF NOT EXISTS workspace_write_intents (
  intent_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  rel_path_hash TEXT NOT NULL, temp_name TEXT NOT NULL, target_sha256 TEXT,
  lease_epoch INTEGER NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS workspace_files (
  workspace_id TEXT NOT NULL, path TEXT NOT NULL,
  size INTEGER NOT NULL, sha256 TEXT NOT NULL,
  PRIMARY KEY (workspace_id, path)
);
CREATE INDEX IF NOT EXISTS workspace_events_at ON workspace_events(at);";

const COLUMNS: &str = "workspace_id, owner_agent_id, runner_id, state, state_reason, created_at, \
    last_attached_at, expires_at, image_digest, data_revision, manifest_hash, bytes_used, \
    files_used, permission_revision, lease_epoch, lease_holder, lease_instance, lease_until, \
    owner_credential";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<(String, WorkspaceRowRaw)> {
    Ok((
        r.get::<_, String>(3)?,
        WorkspaceRowRaw {
            workspace_id: r.get(0)?,
            owner_agent_id: r.get(1)?,
            runner_id: r.get(2)?,
            state_reason: r.get(4)?,
            created_at: r.get(5)?,
            last_attached_at: r.get(6)?,
            expires_at: r.get(7)?,
            image_digest: r.get(8)?,
            data_revision: r.get(9)?,
            manifest_hash: r.get(10)?,
            bytes_used: r.get(11)?,
            files_used: r.get(12)?,
            permission_revision: r.get(13)?,
            lease_epoch: r.get(14)?,
            lease_holder: r.get(15)?,
            lease_instance: r.get(16)?,
            lease_until: r.get(17)?,
            owner_credential: r.get(18)?,
        },
    ))
}

struct WorkspaceRowRaw {
    workspace_id: String,
    owner_agent_id: String,
    runner_id: String,
    state_reason: Option<String>,
    created_at: i64,
    last_attached_at: Option<i64>,
    expires_at: Option<i64>,
    image_digest: Option<String>,
    data_revision: i64,
    manifest_hash: String,
    bytes_used: i64,
    files_used: i64,
    permission_revision: i64,
    lease_epoch: i64,
    lease_holder: Option<String>,
    lease_instance: Option<String>,
    lease_until: Option<i64>,
    owner_credential: Option<String>,
}

/// An unknown state string fails closed (the row is unusable).
fn finish(state: String, r: WorkspaceRowRaw) -> Result<WorkspaceRow, StoreError> {
    let state = WorkspaceState::parse(&state)
        .ok_or_else(|| StoreError::Unavailable(format!("unknown state {state:?}")))?;
    Ok(WorkspaceRow {
        workspace_id: r.workspace_id,
        owner_agent_id: r.owner_agent_id,
        runner_id: r.runner_id,
        state,
        state_reason: r.state_reason,
        created_at: r.created_at,
        last_attached_at: r.last_attached_at,
        expires_at: r.expires_at,
        image_digest: r.image_digest,
        data_revision: r.data_revision,
        manifest_hash: r.manifest_hash,
        bytes_used: r.bytes_used,
        files_used: r.files_used,
        permission_revision: r.permission_revision,
        lease_epoch: r.lease_epoch,
        lease_holder: r.lease_holder,
        lease_instance: r.lease_instance,
        lease_until: r.lease_until,
        owner_credential: r.owner_credential,
    })
}

pub(super) fn get_row(conn: &Connection, id: &str) -> Result<Option<WorkspaceRow>, StoreError> {
    let sql = format!("SELECT {COLUMNS} FROM workspaces WHERE workspace_id = ?1");
    match conn
        .query_row(&sql, params![id], row_from)
        .optional()
        .map_err(db)?
    {
        Some((state, raw)) => finish(state, raw).map(Some),
        None => Ok(None),
    }
}

/// Append one event inside the caller's transaction. `detail` must hold
/// closed codes, hashes and lengths only (never content or paths).
pub(crate) fn record_event(
    conn: &Connection,
    id: &str,
    kind: &str,
    actor: &str,
    row: Option<&WorkspaceRow>,
    detail: Value,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO workspace_events (workspace_id, at, kind, actor, lease_epoch, \
         permission_revision, data_revision, detail_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            id,
            chrono::Utc::now().timestamp(),
            kind,
            actor,
            row.map(|r| r.lease_epoch),
            row.map(|r| r.permission_revision),
            row.map(|r| r.data_revision),
            detail.to_string()
        ],
    )
    .map_err(db)?;
    Ok(())
}

/// Refuse a symlinked registry or sidecar, create the registry privately when
/// it does not exist yet, and set 0600 on what exists. An existing database
/// file is never opened and closed here: closing any descriptor on it drops
/// every POSIX lock this process holds there, so another process could
/// checkpoint and remove the WAL under live connections (same rule as
/// `approval/store.rs` and `channel_ingress/schema.rs`).
#[cfg(unix)]
pub(super) fn prepare_db_file(db_path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    for suffix in ["", "-wal", "-shm"] {
        let path = std::path::PathBuf::from(format!("{}{suffix}", db_path.display()));
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("workspace registry symlink refused".into());
        }
        if !suffix.is_empty() && path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
    }
    if !db_path.exists() {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(db_path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("private workspace registry: {e}")),
        }
    }
    std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())
}

/// Idempotent `ALTER TABLE … ADD COLUMN` for registries created before the
/// column existed.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<(), StoreError> {
    let present: bool = conn
        .prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
        ))
        .map_err(db)?
        .exists(params![column])
        .map_err(db)?;
    if !present {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))
            .map_err(db)?;
    }
    Ok(())
}

impl WorkspaceStore {
    /// Open (or create) `<home>/computer_workspaces.db`: canonical home, no
    /// symlinked database or sidecar, 0600, `SQLITE_OPEN_NOFOLLOW`.
    pub fn open(home: &Path) -> Result<Self, StoreError> {
        if !cfg!(unix) {
            return Err(StoreError::Unavailable("unsupported platform".into()));
        }
        let db_path = std::fs::canonicalize(home).map_err(db)?.join(DB_FILE);
        // One open sequence at a time across threads and processes: the WAL
        // switch and the schema creation of a fresh file answer a concurrent
        // second connection with "database is locked" without consulting the
        // busy handler (see `lock::lock_registry_init`). Held until `Ok`.
        #[cfg(unix)]
        let _init_guard = super::lock::lock_registry_init(home)
            .map_err(|e| StoreError::Unavailable(e.to_string()))?;
        #[cfg(unix)]
        prepare_db_file(&db_path).map_err(StoreError::Unavailable)?;
        let conn = Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(db)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db)?;
        conn.execute_batch(SCHEMA).map_err(db)?;
        add_column_if_missing(&conn, "workspaces", "owner_credential", "TEXT")?;
        #[cfg(unix)]
        prepare_db_file(&db_path).map_err(StoreError::Unavailable)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Run `f` in one `BEGIN IMMEDIATE` transaction; committed only on `Ok`.
    pub(super) fn tx<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let out = f(&tx)?;
        tx.commit().map_err(db)?;
        Ok(out)
    }

    pub fn get(&self, id: &str) -> Result<Option<WorkspaceRow>, StoreError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        get_row(&conn, id)
    }

    /// The row when it exists AND belongs to `caller`; everything else is
    /// [`StoreError::NotFound`] (no existence signal).
    pub fn get_owned(&self, id: &str, caller: &str) -> Result<WorkspaceRow, StoreError> {
        match self.get(id)? {
            Some(row) if row.owner_agent_id == caller => Ok(row),
            _ => Err(StoreError::NotFound),
        }
    }

    fn list_where(&self, filter: &str, arg: Option<&str>) -> Result<Vec<WorkspaceRow>, StoreError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let sql =
            format!("SELECT {COLUMNS} FROM workspaces {filter} ORDER BY created_at, workspace_id");
        let mut stmt = conn.prepare(&sql).map_err(db)?;
        let rows: Vec<_> = match arg {
            Some(a) => stmt.query_map(params![a], row_from),
            None => stmt.query_map([], row_from),
        }
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
        rows.into_iter().map(|(s, r)| finish(s, r)).collect()
    }

    /// The caller's workspaces, tombstones excluded.
    pub fn list_owned(&self, caller: &str) -> Result<Vec<WorkspaceRow>, StoreError> {
        self.list_where(
            "WHERE owner_agent_id = ?1 AND state NOT IN ('deleted','failed_create')",
            Some(caller),
        )
    }

    /// Every row (operator views, reconciliation).
    pub fn list_all(&self) -> Result<Vec<WorkspaceRow>, StoreError> {
        self.list_where("", None)
    }

    /// Insert a `creating` row with a fresh id (tests; production goes
    /// through [`super::lock::create_workspace`], which holds the lock).
    pub fn create(
        &self,
        owner: &str,
        runner_id: &str,
        now: i64,
        max_per_agent: u32,
    ) -> Result<String, StoreError> {
        let id = super::paths::new_workspace_id();
        self.create_with_id(&id, owner, runner_id, now, max_per_agent, None)?;
        Ok(id)
    }

    /// Insert a `creating` row `id` for `owner` unless `max_per_agent` is
    /// reached. Expired, deleted and never-finished workspaces do not count
    /// toward the limit.
    pub fn create_with_id(
        &self,
        id: &str,
        owner: &str,
        runner_id: &str,
        now: i64,
        max_per_agent: u32,
        owner_credential: Option<&str>,
    ) -> Result<(), StoreError> {
        self.tx(|c| {
            let count: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM workspaces WHERE owner_agent_id = ?1 \
                     AND state NOT IN ('deleted','failed_create','expired')",
                    params![owner],
                    |r| r.get(0),
                )
                .map_err(db)?;
            if count >= i64::from(max_per_agent) {
                return Err(StoreError::Quota);
            }
            c.execute(
                "INSERT INTO workspaces (workspace_id, owner_agent_id, runner_id, state, created_at, \
                 manifest_hash, owner_credential) VALUES (?1,?2,?3,'creating',?4,?5,?6)",
                params![id, owner, runner_id, now, EMPTY_MANIFEST, owner_credential],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    /// `from` → `to` (with an event), refused unless the row is in `from`.
    pub fn transition(
        &self,
        id: &str,
        from: &[WorkspaceState],
        to: WorkspaceState,
        actor: &str,
        kind: &str,
        reason: Option<&str>,
    ) -> Result<WorkspaceRow, StoreError> {
        self.tx(|c| {
            let row = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            if !from.contains(&row.state) {
                return Err(StoreError::State(row.state));
            }
            // Leaving `ready` (or entering a terminal state) also ends any
            // lease: the epoch moves so every old holder is fenced.
            let fence = to != WorkspaceState::Ready || row.state != WorkspaceState::Ready;
            let perm_bump = matches!(to, WorkspaceState::Revoked)
                || (row.state == WorkspaceState::Revoked && to == WorkspaceState::Ready);
            c.execute(
                "UPDATE workspaces SET state = ?2, state_reason = ?3, \
                 lease_epoch = lease_epoch + ?4, \
                 lease_holder = CASE WHEN ?4 = 1 THEN NULL ELSE lease_holder END, \
                 lease_instance = CASE WHEN ?4 = 1 THEN NULL ELSE lease_instance END, \
                 lease_until = CASE WHEN ?4 = 1 THEN NULL ELSE lease_until END, \
                 permission_revision = permission_revision + ?5 WHERE workspace_id = ?1",
                params![
                    id,
                    to.as_str(),
                    reason,
                    i64::from(fence),
                    i64::from(perm_bump)
                ],
            )
            .map_err(db)?;
            let row = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            record_event(
                c,
                id,
                kind,
                actor,
                Some(&row),
                json!({"from": from_str(from), "to": to.as_str(), "reason": reason}),
            )?;
            Ok(row)
        })
    }

    /// Acquire the lease (design §4.2 step 2).
    pub fn acquire(&self, req: &AcquireRequest<'_>) -> Result<(Lease, WorkspaceRow), StoreError> {
        self.tx(|c| {
            let row = get_row(c, req.workspace_id)?
                .filter(|r| r.owner_agent_id == req.caller)
                .ok_or(StoreError::NotFound)?;
            if row.state != WorkspaceState::Ready {
                return Err(StoreError::State(row.state));
            }
            if row.runner_id != req.runner_id {
                return Err(StoreError::RunnerMismatch);
            }
            if row.lease_active(req.now) {
                return Err(StoreError::Busy);
            }
            let expires =
                (req.retention_days > 0).then(|| req.now + i64::from(req.retention_days) * 86_400);
            let changed = c
                .execute(
                    "UPDATE workspaces SET lease_epoch = lease_epoch + 1, lease_holder = ?3, \
                     lease_instance = ?4, lease_until = ?5, last_attached_at = ?6, expires_at = ?7 \
                     WHERE workspace_id = ?1 AND lease_epoch = ?2 AND state = 'ready'",
                    params![
                        req.workspace_id,
                        row.lease_epoch,
                        req.holder,
                        req.instance,
                        req.now + req.ttl_secs,
                        req.now,
                        expires
                    ],
                )
                .map_err(db)?;
            if changed != 1 {
                return Err(StoreError::Busy);
            }
            let row = get_row(c, req.workspace_id)?.ok_or(StoreError::NotFound)?;
            record_event(
                c,
                req.workspace_id,
                "lease_acquired",
                &format!("agent:{}", req.caller),
                Some(&row),
                json!({}),
            )?;
            let lease = Lease {
                workspace_id: row.workspace_id.clone(),
                epoch: row.lease_epoch,
                holder: req.holder.to_string(),
                permission_revision: row.permission_revision,
            };
            Ok((lease, row))
        })
    }

    /// Extend a live lease. `false` (and nothing changed) when it is no
    /// longer this exact lease or it already lapsed.
    pub fn renew(&self, lease: &Lease, now: i64, ttl_secs: i64) -> Result<bool, StoreError> {
        self.tx(|c| {
            let changed = c
                .execute(
                    "UPDATE workspaces SET lease_until = ?4 WHERE workspace_id = ?1 \
                     AND lease_epoch = ?2 AND lease_holder = ?3 AND state = 'ready' \
                     AND lease_until > ?5",
                    params![
                        lease.workspace_id,
                        lease.epoch,
                        lease.holder,
                        now + ttl_secs,
                        now
                    ],
                )
                .map_err(db)?;
            Ok(changed == 1)
        })
    }

    /// Release (design §4.3): `WHERE lease_epoch = ? AND lease_holder = ?`.
    /// `false` = someone else holds it now; nothing changed.
    pub fn release(&self, lease: &Lease) -> Result<bool, StoreError> {
        self.tx(|c| {
            let changed = c
                .execute(
                    "UPDATE workspaces SET lease_holder = NULL, lease_instance = NULL, \
                     lease_until = NULL WHERE workspace_id = ?1 AND lease_epoch = ?2 \
                     AND lease_holder = ?3",
                    params![lease.workspace_id, lease.epoch, lease.holder],
                )
                .map_err(db)?;
            if changed == 1 {
                let row = get_row(c, &lease.workspace_id)?;
                record_event(
                    c,
                    &lease.workspace_id,
                    "lease_released",
                    "system:session_end",
                    row.as_ref(),
                    json!({}),
                )?;
            }
            Ok(changed == 1)
        })
    }

    /// Fence: the epoch moves and the holder is cleared (operator freeze, a
    /// session fencing itself on rollback). Returns the new epoch.
    pub fn fence(&self, id: &str, actor: &str, reason: &str) -> Result<i64, StoreError> {
        self.tx(|c| {
            let row = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            if row.state == WorkspaceState::Deleted {
                return Err(StoreError::State(row.state));
            }
            c.execute(
                "UPDATE workspaces SET lease_epoch = lease_epoch + 1, lease_holder = NULL, \
                 lease_instance = NULL, lease_until = NULL WHERE workspace_id = ?1",
                params![id],
            )
            .map_err(db)?;
            let row = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            record_event(
                c,
                id,
                "fenced",
                actor,
                Some(&row),
                json!({"reason": reason}),
            )?;
            Ok(row.lease_epoch)
        })
    }

    /// A session fencing its own lease (rollback / lease lost): epoch +1 and
    /// holder cleared, but only `WHERE lease_epoch = ? AND lease_holder = ?`.
    /// `false` = it was no longer ours; nothing changed.
    pub fn fence_own(&self, lease: &Lease, reason: &str) -> Result<bool, StoreError> {
        self.tx(|c| {
            let changed = c
                .execute(
                    "UPDATE workspaces SET lease_epoch = lease_epoch + 1, lease_holder = NULL, \
                     lease_instance = NULL, lease_until = NULL WHERE workspace_id = ?1 \
                     AND lease_epoch = ?2 AND lease_holder = ?3",
                    params![lease.workspace_id, lease.epoch, lease.holder],
                )
                .map_err(db)?;
            if changed == 1 {
                let row = get_row(c, &lease.workspace_id)?;
                record_event(
                    c,
                    &lease.workspace_id,
                    "fenced",
                    "system:session",
                    row.as_ref(),
                    json!({"reason": reason}),
                )?;
            }
            Ok(changed == 1)
        })
    }

    /// Whether `lease` is still the live, current lease at `now` with the
    /// same permission revision.
    pub fn lease_current(&self, lease: &Lease, now: i64) -> Result<bool, StoreError> {
        Ok(self.get(&lease.workspace_id)?.is_some_and(|row| {
            row.lease_active(now)
                && row.lease_epoch == lease.epoch
                && row.lease_holder.as_deref() == Some(lease.holder.as_str())
                && row.permission_revision == lease.permission_revision
        }))
    }

    /// Record the image that runs for this lease (D2: only recorded; a change
    /// writes `image_changed`).
    pub fn set_image_digest(&self, lease: &Lease, digest: &str) -> Result<(), StoreError> {
        self.tx(|c| {
            let row = get_row(c, &lease.workspace_id)?.ok_or(StoreError::NotFound)?;
            if row.lease_epoch != lease.epoch || row.lease_holder.as_deref() != Some(&lease.holder)
            {
                return Err(StoreError::LeaseLost);
            }
            let previous = row.image_digest.clone();
            c.execute(
                "UPDATE workspaces SET image_digest = ?2 WHERE workspace_id = ?1",
                params![lease.workspace_id, digest],
            )
            .map_err(db)?;
            if previous.as_deref().is_some_and(|p| p != digest) {
                record_event(
                    c,
                    &lease.workspace_id,
                    "image_changed",
                    "system:attach",
                    Some(&row),
                    json!({"old": previous, "new": digest}),
                )?;
            }
            Ok(())
        })
    }

    /// Operator: bind to another runner (never automatic).
    pub fn rebind_runner(&self, id: &str, actor: &str, runner_id: &str) -> Result<(), StoreError> {
        self.tx(|c| {
            let row = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            if matches!(row.state, WorkspaceState::Deleted | WorkspaceState::Deleting) {
                return Err(StoreError::State(row.state));
            }
            c.execute(
                "UPDATE workspaces SET runner_id = ?2, lease_epoch = lease_epoch + 1, \
                 lease_holder = NULL, lease_instance = NULL, lease_until = NULL WHERE workspace_id = ?1",
                params![id, runner_id],
            )
            .map_err(db)?;
            let row = get_row(c, id)?;
            record_event(c, id, "runner_rebound", actor, row.as_ref(), json!({}))
        })
    }

    /// Append an event outside any other change (refusals, notices).
    pub fn note(&self, id: &str, kind: &str, actor: &str, detail: Value) -> Result<(), StoreError> {
        self.tx(|c| {
            let row = get_row(c, id)?;
            record_event(c, id, kind, actor, row.as_ref(), detail)
        })
    }

    /// Workspace ids that have at least one event of `kind`.
    pub fn ids_with_event(&self, kind: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn
            .prepare("SELECT DISTINCT workspace_id FROM workspace_events WHERE kind = ?1 ORDER BY workspace_id")
            .map_err(db)?;
        let rows = stmt.query_map(params![kind], |r| r.get(0)).map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }
}

fn from_str(from: &[WorkspaceState]) -> Vec<&'static str> {
    from.iter().map(|s| s.as_str()).collect()
}
