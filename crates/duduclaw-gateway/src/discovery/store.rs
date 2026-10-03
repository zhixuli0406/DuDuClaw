//! `discovery.db` (SQLite, WAL): runs, rounds (worlds), nodes and replay
//! evaluation rows (DESIGN-dream-rsi §7.3). Online experiments and imported
//! fixture worlds share the same schema; interrupted runs never auto-resume.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use super::tree::{Direction, FailClass, Node, NodeCost, World};

#[path = "store_metadata.rs"]
mod metadata;
pub use metadata::{RoundMetadata, RunRecord};
#[path = "store_night.rs"]
mod night;

/// Database file name under the DuDuClaw home directory.
pub const DB_FILE: &str = "discovery.db";

/// Store errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("corrupt row: {0}")]
    Corrupt(String),
    /// Discovery's private store needs unix ownership/permission checks
    /// (uid, mode, `O_NOFOLLOW`); other hosts refuse instead of opening an
    /// unverified database.
    #[error("discovery is unavailable on this platform: {0}")]
    UnsupportedPlatform(&'static str),
}

/// One replay evaluation row (`discovery_policy_evals`).
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyEvalRow {
    pub policy_id: String,
    /// Frozen policy parameters / source hash, as JSON text.
    pub policy_params: String,
    pub run_id: String,
    pub round: u32,
    pub beta: f64,
    pub attainment: f64,
    pub work: f64,
    pub probes: u64,
    pub decision_rounds: u64,
    pub effective_sequential_rounds: u64,
    pub parallel_penalty: f64,
    pub context_mismatch_rate: f64,
    pub pareto_auc: Option<f64>,
    pub pareto_reward: Option<f64>,
    pub out_of_support: bool,
}

/// Why the store refuses to open on a non-unix host.
#[cfg_attr(unix, allow(dead_code))]
const NON_UNIX_STORE: &str = "the private SQLite store needs a unix host to verify ownership and permissions";

/// Handle on `discovery.db`.
pub struct DiscoveryStore {
    conn: Connection,
    db_path: PathBuf,
}

/// The database contains frozen policy source. All readers and writers share
/// the same private file authority, including SQLite sidecar files.
pub(crate) fn private_connection(path: &Path) -> Result<Connection, StoreError> {
    #[cfg(not(unix))]
    { let _ = path; Err(StoreError::UnsupportedPlatform(NON_UNIX_STORE)) }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let fail = |error: std::io::Error| StoreError::Corrupt(format!("private SQLite: {error}"));
        let parent = path.parent().ok_or_else(|| StoreError::Corrupt("database has no parent".into()))?;
        let parent = super::workspace::canonical_real_directory(parent).map_err(fail)?;
        let metadata = std::fs::metadata(&parent).map_err(fail)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o022 != 0 {
            return Err(StoreError::Corrupt("database parent is not owned and protected".into()));
        }
        let filename = path.file_name().ok_or_else(|| StoreError::Corrupt("database filename missing".into()))?;
        let path = parent.join(filename);
        match std::fs::OpenOptions::new().create_new(true).read(true).write(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path) {
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(error) => return Err(fail(error)),
        }
        validate_private_files(&path)?;
        let authority = std::fs::OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path).map_err(fail)?;
        let before = authority.metadata().map_err(fail)?;
        let connection = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
        validate_private_files(&path)?;
        let after = std::fs::symlink_metadata(&path).map_err(fail)?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err(StoreError::Corrupt("database authority changed while opening".into()));
        }
        Ok(connection)
    }
}

fn validate_private_files(path: &Path) -> Result<(), StoreError> {
    #[cfg(not(unix))]
    { let _ = path; Err(StoreError::UnsupportedPlatform(NON_UNIX_STORE)) }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut filename = path.as_os_str().to_os_string(); filename.push(suffix);
            match std::fs::symlink_metadata(PathBuf::from(filename)) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() && meta.nlink() == 1
                    && meta.uid() == unsafe { libc::geteuid() } && meta.permissions().mode() & 0o777 == 0o600 => {},
                Err(error) if !suffix.is_empty() && error.kind() == std::io::ErrorKind::NotFound => {},
                _ => return Err(StoreError::Corrupt("SQLite file is shared, linked, or not privately owned".into())),
            }
        }
        Ok(())
    }
}

#[path="store_budget.rs"]
pub(crate) mod budget_ledger;
#[path = "store_requests.rs"]
mod requests;

const SCHEMA: &str = "PRAGMA journal_mode=WAL;
PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS discovery_runs (
    run_id          TEXT PRIMARY KEY,
    goal            TEXT NOT NULL DEFAULT '',
    agent_id        TEXT NOT NULL DEFAULT '',
    scorer_name     TEXT NOT NULL DEFAULT '',
    scorer_hash     TEXT NOT NULL DEFAULT '',
    direction       TEXT NOT NULL,
    budget_calls    INTEGER,
    budget_usd      REAL,
    budget_secs     INTEGER,
    status          TEXT NOT NULL DEFAULT 'imported',
    best_cell_id    TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS discovery_rounds (
    run_id          TEXT NOT NULL,
    round           INTEGER NOT NULL,
    schema          TEXT NOT NULL,
    direction       TEXT NOT NULL,
    baseline_score  REAL NOT NULL,
    branch_count    INTEGER NOT NULL,
    refine_count    INTEGER NOT NULL,
    max_parallelism INTEGER NOT NULL,
    policy_id       TEXT NOT NULL,
    beta            REAL NOT NULL,
    policy_params   TEXT,
    plan_reason     TEXT,
    full_grid       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    PRIMARY KEY (run_id, round)
);

CREATE TABLE IF NOT EXISTS discovery_nodes (
    run_id            TEXT NOT NULL,
    round             INTEGER NOT NULL,
    cell_id           TEXT NOT NULL,
    schema            TEXT NOT NULL,
    parent_id         TEXT,
    branch            INTEGER NOT NULL,
    attempt           INTEGER NOT NULL,
    seq               INTEGER NOT NULL,
    dispatched_at     TEXT,
    finished_at       TEXT,
    evaluated         INTEGER NOT NULL,
    valid             INTEGER NOT NULL,
    score             REAL,
    fail_class        TEXT NOT NULL,
    error             TEXT,
    visible_set       TEXT NOT NULL,
    input_tokens      INTEGER NOT NULL DEFAULT 0,
    output_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    usd               REAL NOT NULL DEFAULT 0,
    wall_secs         REAL NOT NULL DEFAULT 0,
    runtime           TEXT,
    model             TEXT,
    workspace         TEXT,
    proposal_excerpt  TEXT,
    PRIMARY KEY (run_id, cell_id)
);

CREATE INDEX IF NOT EXISTS idx_discovery_nodes_round
    ON discovery_nodes(run_id, round, seq);

CREATE TABLE IF NOT EXISTS discovery_policy_evals (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    policy_id                   TEXT NOT NULL,
    policy_params               TEXT NOT NULL,
    run_id                      TEXT NOT NULL,
    round                       INTEGER NOT NULL,
    beta                        REAL NOT NULL,
    attainment                  REAL NOT NULL,
    work                        REAL NOT NULL,
    probes                      INTEGER NOT NULL,
    decision_rounds             INTEGER NOT NULL,
    effective_sequential_rounds INTEGER NOT NULL,
    parallel_penalty            REAL NOT NULL,
    context_mismatch_rate       REAL NOT NULL,
    pareto_auc                  REAL,
    pareto_reward               REAL,
    out_of_support              INTEGER NOT NULL DEFAULT 0,
    created_at                  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_discovery_evals_policy
    ON discovery_policy_evals(policy_id, run_id, round);

CREATE TABLE IF NOT EXISTS discovery_requests (
    task_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    frozen_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS discovery_run_budget (
    run_id TEXT PRIMARY KEY REFERENCES discovery_runs(run_id),
    sequence INTEGER NOT NULL,
    snapshot TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS discovery_artifacts (
    run_id TEXT NOT NULL,
    cell_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(run_id,cell_id)
);";

/// Additive columns, applied idempotently (rusqlite has no
/// `ADD COLUMN IF NOT EXISTS`). `isolation_backend` (DESIGN §7.3, filled by
/// the online orchestrator) lives here rather than in the CREATE so the
/// migration path is exercised from the first release; later packages append.
const COLUMN_MIGRATIONS: &[(&str, &str, &str)] = &[
    ("discovery_nodes", "isolation_backend", "isolation_backend TEXT"),
    ("discovery_nodes", "usd_source", "usd_source TEXT NOT NULL DEFAULT 'unknown'"),
    ("discovery_nodes", "unknown_calls", "unknown_calls INTEGER NOT NULL DEFAULT 0"),
    ("discovery_nodes", "configured_model", "configured_model TEXT"),
    ("discovery_rounds", "completion", "completion TEXT NOT NULL DEFAULT 'unknown'"),
    ("discovery_runs", "task_id", "task_id TEXT"),
    ("discovery_runs", "creator_id", "creator_id TEXT"),
    ("discovery_runs", "creator_origin", "creator_origin TEXT"),
    ("discovery_runs", "approved_root_id", "approved_root_id TEXT"),
    ("discovery_runs", "has_unconfined", "has_unconfined INTEGER NOT NULL DEFAULT 0"),
    ("discovery_runs", "provenance_verified", "provenance_verified INTEGER NOT NULL DEFAULT 0"),
    ("discovery_runs", "runtime", "runtime TEXT"),
    ("discovery_runs", "configured_model", "configured_model TEXT"),
    ("discovery_policy_evals", "valid", "valid INTEGER NOT NULL DEFAULT 0"),
    ("discovery_policy_evals", "violation", "violation TEXT"),
    ("discovery_policy_evals", "comparison_available", "comparison_available INTEGER NOT NULL DEFAULT 0"),
    ("discovery_policy_evals", "source_origin_task_ids", "source_origin_task_ids TEXT NOT NULL DEFAULT '[]'"),
    ("discovery_policy_evals", "evaluation_origin", "evaluation_origin TEXT NOT NULL DEFAULT 'unknown'"),
    ("discovery_policy_evals", "occurrence_id", "occurrence_id TEXT"),
];

fn to_i64(v: u64, what: &str) -> Result<i64, StoreError> {
    i64::try_from(v).map_err(|_| StoreError::Corrupt(format!("{what} out of range")))
}

fn to_u64(v: i64, what: &str) -> Result<u64, StoreError> {
    u64::try_from(v).map_err(|_| StoreError::Corrupt(format!("{what} negative")))
}

fn to_u32(v: i64, what: &str) -> Result<u32, StoreError> {
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("{what} out of range")))
}

impl DiscoveryStore {
    /// Open (creating if needed) `<home_dir>/discovery.db`.
    pub fn open(home_dir: &Path) -> Result<Self, StoreError> {
        Self::open_path(&home_dir.join(DB_FILE))
    }

    /// Open (creating if needed) a database at an explicit path.
    pub fn open_path(db_path: &Path) -> Result<Self, StoreError> {
        let conn = private_connection(db_path)?;
        conn.execute_batch(SCHEMA)?;
        validate_private_files(db_path)?;
        for (table, column, decl) in COLUMN_MIGRATIONS {
            let exists: bool = {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
                let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
                let mut found = false;
                for n in names {
                    if n? == *column {
                        found = true;
                    }
                }
                found
            };
            if !exists {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {decl}"))?;
            }
        }
        Ok(Self {
            conn,
            db_path: db_path.to_path_buf(),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// An interrupted run is never resumed automatically. Call this only
    /// after acquiring the operator execution lock for the discovery home.
    pub fn running_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare("SELECT run_id FROM discovery_runs WHERE status='running'")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn interrupt_running(&self) -> Result<usize, StoreError> {
        Ok(self.conn.execute("UPDATE discovery_runs SET status='interrupted' WHERE status='running'", [])?)
    }

    pub fn create_run(&self, run_id: &str, goal: &str, agent_id: &str,
        scorer: &str, scorer_hash: &str, direction: Direction,
        budget: &super::contracts::RunBudget) -> Result<(), StoreError> {
        self.conn.execute("INSERT INTO discovery_runs
            (run_id,goal,agent_id,scorer_name,scorer_hash,direction,budget_calls,budget_usd,budget_secs,status,created_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'running',?10)",
            params![run_id,goal,agent_id,scorer,scorer_hash,direction.as_str(),budget.max_agent_calls,
                budget.max_usd,to_i64(budget.max_wall_secs,"budget_secs")?,chrono::Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub fn finish_run(&self, run_id: &str, status: &str, best: Option<&str>) -> Result<(), StoreError> {
        if !matches!(status, "complete" | "degraded" | "budget_exhausted" | "rate_limited" | "cancelled" | "failed" | "interrupted") {
            return Err(StoreError::Corrupt("unknown run status".into()));
        }
        self.conn.execute("UPDATE discovery_runs SET status=?2,best_cell_id=?3 WHERE run_id=?1",
            params![run_id,status,best])?;
        Ok(())
    }

    pub fn set_node_isolation(&self, run_id: &str, cell_id: &str,
        backend: super::contracts::IsolationBackend) -> Result<(), StoreError> {
        let backend = match backend { super::contracts::IsolationBackend::Container => "container",
            super::contracts::IsolationBackend::Native => "native", super::contracts::IsolationBackend::None => "none" };
        self.conn.execute("UPDATE discovery_nodes SET isolation_backend=?3 WHERE run_id=?1 AND cell_id=?2",
            params![run_id,cell_id,backend])?;
        if backend == "none" { self.mark_unconfined(run_id)?; }
        Ok(())
    }

    /// Insert a world header: the run row (if absent) and the round row
    /// (replacing an existing one for the same `(run_id, round)`).
    pub fn insert_world(&self, world: &World) -> Result<(), StoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO discovery_runs (run_id, direction, created_at)
             VALUES (?1, ?2, ?3)",
            params![world.run_id, world.direction.as_str(), now],
        )?;
        tx.execute(
            "INSERT INTO discovery_rounds
             (run_id, round, schema, direction, baseline_score, branch_count,
              refine_count, max_parallelism, policy_id, beta, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(run_id,round) DO UPDATE SET schema=excluded.schema,
             direction=excluded.direction,baseline_score=excluded.baseline_score,
             branch_count=excluded.branch_count,refine_count=excluded.refine_count,
             max_parallelism=excluded.max_parallelism,policy_id=excluded.policy_id,beta=excluded.beta
             WHERE discovery_rounds.policy_params IS NULL",
            params![
                world.run_id,
                world.round,
                world.schema,
                world.direction.as_str(),
                world.baseline_score,
                world.branch_count,
                world.refine_count,
                world.max_parallelism,
                world.policy_id,
                world.beta,
                now
            ],
        )?;
        if self.load_world(&world.run_id, world.round)?.as_ref() != Some(world) {
            return Err(StoreError::Corrupt("cannot replace a frozen discovery world".into()));
        }
        tx.commit()?;
        Ok(())
    }

    /// Load one world header.
    pub fn load_world(&self, run_id: &str, round: u32) -> Result<Option<World>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT schema, direction, baseline_score, branch_count, refine_count,
                        max_parallelism, policy_id, beta
                 FROM discovery_rounds WHERE run_id = ?1 AND round = ?2",
                params![run_id, round],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, f64>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((schema, direction, baseline_score, bc, rc, mp, policy_id, beta)) = row else {
            return Ok(None);
        };
        let direction = Direction::parse(&direction)
            .ok_or_else(|| StoreError::Corrupt(format!("direction {direction:?}")))?;
        Ok(Some(World {
            schema,
            run_id: run_id.to_string(),
            round,
            direction,
            baseline_score,
            branch_count: to_u32(bc, "branch_count")?,
            refine_count: to_u32(rc, "refine_count")?,
            max_parallelism: to_u32(mp, "max_parallelism")?,
            policy_id,
            beta,
        }))
    }

    /// Insert (or replace) nodes in one transaction.
    pub fn insert_nodes(&self, nodes: &[Node]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO discovery_nodes
                 (run_id, round, cell_id, schema, parent_id, branch, attempt, seq,
                  dispatched_at, finished_at, evaluated, valid, score, fail_class,
                  error, visible_set, input_tokens, output_tokens, cache_read_tokens,
                  usd, wall_secs, runtime, model, workspace, proposal_excerpt, usd_source, unknown_calls)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
            )?;
            for n in nodes {
                let visible = serde_json::to_string(&n.visible_set)?;
                stmt.execute(params![
                    n.run_id,
                    n.round,
                    n.cell_id,
                    n.schema,
                    n.parent_id,
                    n.branch,
                    n.attempt,
                    to_i64(n.seq, "seq")?,
                    n.dispatched_at,
                    n.finished_at,
                    n.evaluated,
                    n.valid,
                    n.score,
                    n.fail_class.as_str(),
                    n.error,
                    visible,
                    to_i64(n.cost.input_tokens, "input_tokens")?,
                    to_i64(n.cost.output_tokens, "output_tokens")?,
                    to_i64(n.cost.cache_read_tokens, "cache_read_tokens")?,
                    n.cost.usd,
                    n.cost.wall_secs,
                    n.runtime,
                    n.model,
                    n.workspace,
                    n.proposal_excerpt,
                    n.cost.usd_source.as_str(),
                    n.cost.unknown_calls,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Stream the complete run subtotal without materializing its tree. Legacy
    /// oversized runs remain listable, with every recorded cost accounted for.
    pub(crate) fn run_node_subtotal(&self, run_id: &str) -> Result<(NodeCost, usize), StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT usd, wall_secs, unknown_calls, input_tokens, output_tokens, cache_read_tokens
             FROM discovery_nodes WHERE run_id = ?1",
        )?;
        let records = statement.query_map([run_id], |row| Ok(NodeCost {
            usd: row.get(0)?, wall_secs: row.get(1)?, unknown_calls: row.get(2)?,
            input_tokens: row.get(3)?, output_tokens: row.get(4)?, cache_read_tokens: row.get(5)?,
            ..Default::default()
        }))?;
        let mut subtotal = NodeCost::default();
        let mut count = 0usize;
        for record in records {
            let cost = record?;
            subtotal.usd += cost.usd;
            subtotal.wall_secs += cost.wall_secs;
            subtotal.unknown_calls = subtotal.unknown_calls.saturating_add(cost.unknown_calls);
            subtotal.input_tokens = subtotal.input_tokens.saturating_add(cost.input_tokens);
            subtotal.output_tokens = subtotal.output_tokens.saturating_add(cost.output_tokens);
            subtotal.cache_read_tokens = subtotal.cache_read_tokens.saturating_add(cost.cache_read_tokens);
            count = count.saturating_add(1);
        }
        Ok((subtotal, count))
    }

    /// Load the nodes of one world, ordered by `seq` then `cell_id`.
    pub fn load_nodes(&self, run_id: &str, round: u32) -> Result<Vec<Node>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT cell_id, schema, parent_id, branch, attempt, seq, dispatched_at,
                    finished_at, evaluated, valid, score, fail_class, error, visible_set,
                    input_tokens, output_tokens, cache_read_tokens, usd, wall_secs,
                    runtime, model, workspace, proposal_excerpt, usd_source, unknown_calls
             FROM discovery_nodes WHERE run_id = ?1 AND round = ?2
             ORDER BY seq, cell_id",
        )?;
        let rows = stmt.query_map(params![run_id, round], |r| {
            Ok(RawNode {
                cell_id: r.get(0)?,
                schema: r.get(1)?,
                parent_id: r.get(2)?,
                branch: r.get(3)?,
                attempt: r.get(4)?,
                seq: r.get(5)?,
                dispatched_at: r.get(6)?,
                finished_at: r.get(7)?,
                evaluated: r.get(8)?,
                valid: r.get(9)?,
                score: r.get(10)?,
                fail_class: r.get(11)?,
                error: r.get(12)?,
                visible_set: r.get(13)?,
                input_tokens: r.get(14)?,
                output_tokens: r.get(15)?,
                cache_read_tokens: r.get(16)?,
                usd: r.get(17)?,
                wall_secs: r.get(18)?,
                runtime: r.get(19)?,
                model: r.get(20)?,
                workspace: r.get(21)?,
                proposal_excerpt: r.get(22)?,
                usd_source: r.get(23)?,
                unknown_calls: r.get(24)?,
            })
        })?;
        let mut out = Vec::new();
        for raw in rows {
            out.push(raw?.into_node(run_id, round)?);
        }
        Ok(out)
    }

    /// Insert one replay evaluation row; returns its id.
    pub fn insert_policy_eval(&self, row: &PolicyEvalRow) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO discovery_policy_evals
             (policy_id, policy_params, run_id, round, beta, attainment, work, probes,
              decision_rounds, effective_sequential_rounds, parallel_penalty,
              context_mismatch_rate, pareto_auc, pareto_reward, out_of_support, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                row.policy_id,
                row.policy_params,
                row.run_id,
                row.round,
                row.beta,
                row.attainment,
                row.work,
                to_i64(row.probes, "probes")?,
                to_i64(row.decision_rounds, "decision_rounds")?,
                to_i64(
                    row.effective_sequential_rounds,
                    "effective_sequential_rounds"
                )?,
                row.parallel_penalty,
                row.context_mismatch_rate,
                row.pareto_auc,
                row.pareto_reward,
                row.out_of_support,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Number of evaluation rows for a policy (mainly for tests/reports).
    pub fn count_policy_evals(&self, policy_id: &str) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM discovery_policy_evals WHERE policy_id = ?1",
            params![policy_id],
            |r| r.get(0),
        )?;
        to_u64(n, "count")
    }
}

struct RawNode {
    cell_id: String,
    schema: String,
    parent_id: Option<String>,
    branch: i64,
    attempt: i64,
    seq: i64,
    dispatched_at: Option<String>,
    finished_at: Option<String>,
    evaluated: bool,
    valid: bool,
    score: Option<f64>,
    fail_class: String,
    error: Option<String>,
    visible_set: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    usd: f64,
    wall_secs: f64,
    runtime: Option<String>,
    model: Option<String>,
    workspace: Option<String>,
    proposal_excerpt: Option<String>,
    usd_source: String,
    unknown_calls: i64,
}

impl RawNode {
    fn into_node(self, run_id: &str, round: u32) -> Result<Node, StoreError> {
        let fail_class = FailClass::parse(&self.fail_class)
            .ok_or_else(|| StoreError::Corrupt(format!("fail_class {:?}", self.fail_class)))?;
        Ok(Node {
            schema: self.schema,
            run_id: run_id.to_string(),
            round,
            cell_id: self.cell_id,
            parent_id: self.parent_id,
            branch: to_u32(self.branch, "branch")?,
            attempt: to_u32(self.attempt, "attempt")?,
            seq: to_u64(self.seq, "seq")?,
            dispatched_at: self.dispatched_at,
            finished_at: self.finished_at,
            evaluated: self.evaluated,
            valid: self.valid,
            score: self.score,
            fail_class,
            error: self.error,
            visible_set: serde_json::from_str(&self.visible_set)?,
            cost: NodeCost {
                input_tokens: to_u64(self.input_tokens, "input_tokens")?,
                output_tokens: to_u64(self.output_tokens, "output_tokens")?,
                cache_read_tokens: to_u64(self.cache_read_tokens, "cache_read_tokens")?,
                usd: self.usd,
                usd_source: serde_json::from_value(serde_json::Value::String(self.usd_source))?,
                unknown_calls: to_u32(self.unknown_calls, "unknown_calls")?,
                wall_secs: self.wall_secs,
            },
            runtime: self.runtime,
            model: self.model,
            workspace: self.workspace,
            proposal_excerpt: self.proposal_excerpt,
        })
    }
}
