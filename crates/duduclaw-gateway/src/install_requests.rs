//! Two-stage install approval requests (Skill / MCP).
//!
//! Non-admin users cannot install a Skill or MCP server directly. They file
//! an **install request** that carries the item's functional description and
//! its **security-scan verdict**, then it flows through a signature chain:
//!
//! - **Employee** requester → a **department manager** signs, THEN an
//!   **admin** signs → install runs.
//! - **Manager** requester → an **admin** signs → install runs.
//! - **Admin** never files a request (installs directly).
//!
//! ## Why a dedicated store (not the single-decision `ApprovalBroker`)
//!
//! [`crate::approval::ApprovalBroker`] is a one-shot request→decide primitive:
//! the first decision is terminal. This chain needs **two** independent
//! signatures for an employee request, plus the rich payload (scan findings +
//! description) rendered to each approver. Modeling that on top of a single
//! terminal decision would be a hack, so this is its own SQLite store mirroring
//! the broker's idioms (WAL, `busy_timeout`, self-healing schema,
//! parameterized SQL, fail-closed status parsing).
//!
//! ## Role + department model
//!
//! Stage 1 (the manager gate) is scoped to the requester's **department**: a
//! `Manager` may clear it only for a requester in the SAME department
//! (`User.department`, exact case-insensitive match). A request whose requester
//! has no department falls back to "any manager" (graceful default). Stage 2 is
//! cleared by any `Admin`; an `Admin` approval covers BOTH gates in one action
//! and bypasses department routing (a superior short-circuits the chain).
//! The routing check lives in [`InstallRequest::manager_may_sign`] and is
//! enforced in [`InstallRequestStore::decide`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::info;

/// Default TTL for an install request: 7 days. Unlike a live tool-call
/// approval, an install request is not time-critical.
pub const DEFAULT_INSTALL_TTL_SECONDS: i64 = 7 * 24 * 3600;

/// Lifecycle status. Only `Approved` authorizes execution (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

impl RequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestStatus::Pending => "pending",
            RequestStatus::Approved => "approved",
            RequestStatus::Denied => "denied",
            RequestStatus::Expired => "expired",
        }
    }
    /// Unknown DB text fails closed to `Denied` (never authorizes).
    pub fn from_db(s: &str) -> Self {
        match s {
            "pending" => RequestStatus::Pending,
            "approved" => RequestStatus::Approved,
            "expired" => RequestStatus::Expired,
            _ => RequestStatus::Denied,
        }
    }
    pub fn is_terminal(self) -> bool {
        !matches!(self, RequestStatus::Pending)
    }
}

/// One install request row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRequest {
    pub id: String,
    /// "skill" | "mcp"
    pub kind: String,
    pub title: String,
    pub description: String,
    pub requester_id: String,
    pub requester_email: String,
    /// "employee" | "manager" | "admin"
    pub requester_role: String,
    /// The requester's department (for routing stage 1 to the right manager).
    /// `None`/empty ⇒ any manager may sign (graceful fallback).
    pub requester_department: Option<String>,
    pub risk_level: String,
    /// The security-scan findings array (as returned by the scanner).
    pub scan: Value,
    /// Exact install params to re-dispatch on final approval (opaque).
    pub payload: Value,
    pub status: RequestStatus,
    pub manager_by: Option<String>,
    pub manager_at: Option<String>,
    pub admin_by: Option<String>,
    pub admin_at: Option<String>,
    pub decided_reason: Option<String>,
    pub executed: bool,
    pub execute_error: Option<String>,
    pub created_at: String,
    pub ttl_seconds: i64,
}

impl InstallRequest {
    /// True when the requester is an employee (needs the manager gate).
    pub fn needs_manager(&self) -> bool {
        self.requester_role == "employee"
    }

    /// Non-empty requester department (trimmed), if any.
    fn dept(&self) -> Option<&str> {
        self.requester_department.as_deref().map(|d| d.trim()).filter(|d| !d.is_empty())
    }

    /// Whether a manager in `manager_department` may sign this request's
    /// **manager gate**. A request with no department falls back to "any
    /// manager"; a request with a department requires an exact (case-
    /// insensitive) department match.
    pub fn manager_may_sign(&self, manager_department: Option<&str>) -> bool {
        match self.dept() {
            None => true, // no department set ⇒ any manager (fallback)
            Some(req_dept) => manager_department
                .map(|m| m.trim())
                .filter(|m| !m.is_empty())
                .map(|m| m.eq_ignore_ascii_case(req_dept))
                .unwrap_or(false),
        }
    }

    /// Human stage label for the UI.
    pub fn stage(&self) -> &'static str {
        match self.status {
            RequestStatus::Approved => "approved",
            RequestStatus::Denied => "denied",
            RequestStatus::Expired => "expired",
            RequestStatus::Pending => {
                if self.needs_manager() && self.manager_by.is_none() {
                    "awaiting_manager"
                } else {
                    "awaiting_admin"
                }
            }
        }
    }

    fn expires_at(&self) -> Option<DateTime<Utc>> {
        let created = DateTime::parse_from_rfc3339(&self.created_at).ok()?;
        Some(created.with_timezone(&Utc) + chrono::Duration::seconds(self.ttl_seconds))
    }

    fn is_stale(&self, now: DateTime<Utc>) -> bool {
        if self.status != RequestStatus::Pending {
            return false;
        }
        match self.expires_at() {
            Some(exp) => now >= exp,
            None => true, // unparseable ⇒ fail closed
        }
    }

    /// JSON shape for the dashboard.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "kind": self.kind,
            "title": self.title,
            "description": self.description,
            "requester_id": self.requester_id,
            "requester_email": self.requester_email,
            "requester_role": self.requester_role,
            "requester_department": self.requester_department,
            "risk_level": self.risk_level,
            "scan": self.scan,
            "status": self.status.as_str(),
            "stage": self.stage(),
            "manager_by": self.manager_by,
            "manager_at": self.manager_at,
            "admin_by": self.admin_by,
            "admin_at": self.admin_at,
            "decided_reason": self.decided_reason,
            "executed": self.executed,
            "execute_error": self.execute_error,
            "created_at": self.created_at,
            "ttl_seconds": self.ttl_seconds,
        })
    }
}

/// The outcome of a `decide` call — tells the handler whether to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideOutcome {
    /// Manager cleared stage 1; the request now awaits an admin.
    AdvancedToAdmin,
    /// Fully approved — the handler MUST now run the install and call
    /// [`InstallRequestStore::mark_executed`].
    ReadyToExecute,
    /// The request was denied.
    Denied,
}

/// SQLite-backed store. Mirrors `ApprovalStore` idioms.
pub struct InstallRequestStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

impl InstallRequestStore {
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("install_requests.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open install requests: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn), db_path: Some(db_path) })
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn), db_path: None })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS install_requests (
                 id              TEXT PRIMARY KEY,
                 kind            TEXT NOT NULL,
                 title           TEXT NOT NULL,
                 description     TEXT NOT NULL DEFAULT '',
                 requester_id    TEXT NOT NULL,
                 requester_email TEXT NOT NULL DEFAULT '',
                 requester_role  TEXT NOT NULL,
                 requester_department TEXT,
                 risk_level      TEXT NOT NULL DEFAULT 'Clean',
                 scan            TEXT NOT NULL DEFAULT '[]',
                 payload         TEXT NOT NULL DEFAULT '{}',
                 status          TEXT NOT NULL DEFAULT 'pending',
                 manager_by      TEXT,
                 manager_at      TEXT,
                 admin_by        TEXT,
                 admin_at        TEXT,
                 decided_reason  TEXT,
                 executed        INTEGER NOT NULL DEFAULT 0,
                 execute_error   TEXT,
                 created_at      TEXT NOT NULL,
                 ttl_seconds     INTEGER NOT NULL DEFAULT 604800
             );

             CREATE INDEX IF NOT EXISTS idx_install_req_status ON install_requests(status);
             CREATE INDEX IF NOT EXISTS idx_install_req_requester ON install_requests(requester_id);",
        )
        .map_err(|e| format!("init install requests schema: {e}"))?;
        Ok(())
    }

    /// Create a pending request. Returns its id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        kind: &str,
        title: &str,
        description: &str,
        requester_id: &str,
        requester_email: &str,
        requester_role: &str,
        requester_department: Option<&str>,
        risk_level: &str,
        scan: &Value,
        payload: &Value,
        ttl_seconds: i64,
    ) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let ttl = if ttl_seconds > 0 { ttl_seconds } else { DEFAULT_INSTALL_TTL_SECONDS };
        let dept = requester_department.map(|d| d.trim()).filter(|d| !d.is_empty());
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO install_requests
                (id, kind, title, description, requester_id, requester_email,
                 requester_role, requester_department, risk_level, scan, payload, status, created_at, ttl_seconds)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',?12,?13)",
            params![
                id, kind, title, description, requester_id, requester_email,
                requester_role, dept, risk_level, scan.to_string(), payload.to_string(),
                Utc::now().to_rfc3339(), ttl,
            ],
        )
        .map_err(|e| format!("insert install request: {e}"))?;
        info!(request_id = %id, kind, requester = %requester_id, "install request filed");
        Ok(id)
    }

    pub async fn get(&self, id: &str) -> Result<Option<InstallRequest>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, kind, title, description, requester_id, requester_email,
                    requester_role, risk_level, scan, payload, status,
                    manager_by, manager_at, admin_by, admin_at, decided_reason,
                    executed, execute_error, created_at, ttl_seconds, requester_department
             FROM install_requests WHERE id = ?1",
            params![id],
            row_to_request,
        )
        .optional()
        .map_err(|e| format!("get install request: {e}"))
    }

    /// All pending requests (stale swept first), newest last.
    pub async fn list_pending(&self) -> Result<Vec<InstallRequest>, String> {
        self.expire_stale().await?;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, title, description, requester_id, requester_email,
                        requester_role, risk_level, scan, payload, status,
                        manager_by, manager_at, admin_by, admin_at, decided_reason,
                        executed, execute_error, created_at, ttl_seconds, requester_department
                 FROM install_requests WHERE status = 'pending' ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare list_pending: {e}"))?;
        let rows = stmt
            .query_map([], row_to_request)
            .map_err(|e| format!("query list_pending: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_pending: {e}"))?;
        Ok(rows)
    }

    /// A single requester's own requests (any status), newest first.
    pub async fn list_for_requester(&self, requester_id: &str) -> Result<Vec<InstallRequest>, String> {
        let _ = self.expire_stale().await;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, title, description, requester_id, requester_email,
                        requester_role, risk_level, scan, payload, status,
                        manager_by, manager_at, admin_by, admin_at, decided_reason,
                        executed, execute_error, created_at, ttl_seconds, requester_department
                 FROM install_requests WHERE requester_id = ?1 ORDER BY created_at DESC LIMIT 100",
            )
            .map_err(|e| format!("prepare list_for_requester: {e}"))?;
        let rows = stmt
            .query_map(params![requester_id], row_to_request)
            .map_err(|e| format!("query list_for_requester: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_for_requester: {e}"))?;
        Ok(rows)
    }

    /// Role-aware decision. `decider_role` is the caller's role string
    /// ("admin" | "manager"). Fail-closed: unknown roles / wrong stage / a
    /// terminal request are rejected without any state change.
    pub async fn decide(
        &self,
        id: &str,
        decider_id: &str,
        decider_role: &str,
        decider_department: Option<&str>,
        approve: bool,
        reason: &str,
    ) -> Result<DecideOutcome, String> {
        // Opportunistic expiry so a decision can't land on a stale request.
        let _ = self.expire_stale().await;
        let req = self.get(id).await?.ok_or_else(|| format!("install request {id} not found"))?;
        if req.status.is_terminal() {
            return Err(format!("此申請已{}，無法再次處理", zh_status(req.status)));
        }
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;

        if !approve {
            // Any manager+ may deny at any pending stage.
            let n = conn
                .execute(
                    "UPDATE install_requests SET status='denied', decided_reason=?1, admin_by=COALESCE(admin_by,?2)
                     WHERE id=?3 AND status='pending'",
                    params![reason, decider_id, id],
                )
                .map_err(|e| format!("deny: {e}"))?;
            if n == 0 {
                return Err("此申請已被他人處理".into());
            }
            return Ok(DecideOutcome::Denied);
        }

        let needs_manager = req.needs_manager();
        match decider_role {
            "admin" => {
                // Admin covers both gates in one action.
                let manager_by = req.manager_by.clone().or_else(|| Some(decider_id.to_string()));
                let manager_at = req.manager_at.clone().or_else(|| Some(now.clone()));
                let n = conn
                    .execute(
                        "UPDATE install_requests
                         SET status='approved', admin_by=?1, admin_at=?2, manager_by=?3, manager_at=?4
                         WHERE id=?5 AND status='pending'",
                        params![decider_id, now, manager_by, manager_at, id],
                    )
                    .map_err(|e| format!("admin approve: {e}"))?;
                if n == 0 {
                    return Err("此申請已被他人處理".into());
                }
                Ok(DecideOutcome::ReadyToExecute)
            }
            "manager" => {
                if !needs_manager {
                    return Err("此申請只需管理員核准，主管無法核准".into());
                }
                if req.manager_by.is_some() {
                    return Err("此申請已由主管核准，正在等待管理員核准".into());
                }
                // Department routing: a manager may only clear the manager gate
                // for a requester in their OWN department (a request with no
                // department falls back to any manager).
                if !req.manager_may_sign(decider_department) {
                    return Err("此申請屬於其他部門，需由該部門主管核准".into());
                }
                let n = conn
                    .execute(
                        "UPDATE install_requests SET manager_by=?1, manager_at=?2
                         WHERE id=?3 AND status='pending' AND manager_by IS NULL",
                        params![decider_id, now, id],
                    )
                    .map_err(|e| format!("manager approve: {e}"))?;
                if n == 0 {
                    return Err("此申請已被他人處理".into());
                }
                Ok(DecideOutcome::AdvancedToAdmin)
            }
            other => Err(format!("角色 '{other}' 無核准權限")),
        }
    }

    /// Record the result of executing the install for an approved request.
    pub async fn mark_executed(&self, id: &str, ok: bool, error: Option<&str>) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE install_requests SET executed=?1, execute_error=?2 WHERE id=?3",
            params![ok as i64, error, id],
        )
        .map_err(|e| format!("mark_executed: {e}"))?;
        Ok(())
    }

    /// Sweep pending rows past their TTL → `expired`. TTL expiry counts as DENY.
    pub async fn expire_stale(&self) -> Result<u64, String> {
        let now = Utc::now();
        let pending = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(
                    "SELECT id, kind, title, description, requester_id, requester_email,
                            requester_role, risk_level, scan, payload, status,
                            manager_by, manager_at, admin_by, admin_at, decided_reason,
                            executed, execute_error, created_at, ttl_seconds, requester_department
                     FROM install_requests WHERE status='pending'",
                )
                .map_err(|e| format!("prepare expire scan: {e}"))?;
            stmt.query_map([], row_to_request)
                .map_err(|e| format!("query expire scan: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect expire scan: {e}"))?
        };
        let mut expired = 0u64;
        let conn = self.conn.lock().await;
        for req in pending {
            if req.is_stale(now) {
                let n = conn
                    .execute(
                        "UPDATE install_requests SET status='expired' WHERE id=?1 AND status='pending'",
                        params![req.id],
                    )
                    .map_err(|e| format!("expire: {e}"))?;
                expired += n as u64;
            }
        }
        Ok(expired)
    }
}

fn zh_status(s: RequestStatus) -> &'static str {
    match s {
        RequestStatus::Approved => "核准",
        RequestStatus::Denied => "退回",
        RequestStatus::Expired => "逾時失效",
        RequestStatus::Pending => "待處理",
    }
}

fn row_to_request(row: &rusqlite::Row) -> rusqlite::Result<InstallRequest> {
    let scan_text: String = row.get(8)?;
    let payload_text: String = row.get(9)?;
    let status_text: String = row.get(10)?;
    Ok(InstallRequest {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        requester_id: row.get(4)?,
        requester_email: row.get(5)?,
        requester_role: row.get(6)?,
        risk_level: row.get(7)?,
        scan: serde_json::from_str(&scan_text).unwrap_or(Value::Null),
        payload: serde_json::from_str(&payload_text).unwrap_or(Value::Null),
        status: RequestStatus::from_db(&status_text),
        manager_by: row.get(11)?,
        manager_at: row.get(12)?,
        admin_by: row.get(13)?,
        admin_at: row.get(14)?,
        decided_reason: row.get(15)?,
        executed: row.get::<_, i64>(16)? != 0,
        execute_error: row.get(17)?,
        created_at: row.get(18)?,
        ttl_seconds: row.get(19)?,
        requester_department: row.get(20)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> InstallRequestStore {
        InstallRequestStore::open_in_memory().unwrap()
    }

    async fn make(store: &InstallRequestStore, role: &str) -> String {
        make_dept(store, role, None).await
    }

    async fn make_dept(store: &InstallRequestStore, role: &str, dept: Option<&str>) -> String {
        store
            .create(
                "skill", "test-skill", "does a thing", "u-1", "u1@x", role, dept,
                "Low", &json!([]), &json!({"scope":"global"}), 3600,
            )
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn employee_needs_manager_then_admin() {
        let s = store();
        let id = make(&s, "employee").await;
        // manager clears stage 1 → awaits admin (NOT executable yet)
        let o = s.decide(&id, "mgr-1", "manager", None, true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::AdvancedToAdmin);
        let r = s.get(&id).await.unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Pending);
        assert_eq!(r.stage(), "awaiting_admin");
        // admin clears stage 2 → ready
        let o = s.decide(&id, "adm-1", "admin", None, true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::ReadyToExecute);
        let r = s.get(&id).await.unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Approved);
        assert_eq!(r.manager_by.as_deref(), Some("mgr-1"));
        assert_eq!(r.admin_by.as_deref(), Some("adm-1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_request_needs_admin_only() {
        let s = store();
        let id = make(&s, "manager").await;
        // another manager cannot approve a manager's request
        assert!(s.decide(&id, "mgr-2", "manager", None, true, "").await.is_err());
        // admin approves → ready
        let o = s.decide(&id, "adm-1", "admin", None, true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::ReadyToExecute);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admin_covers_both_gates_for_employee() {
        let s = store();
        let id = make(&s, "employee").await;
        // admin approving an un-manager-signed employee request completes it
        let o = s.decide(&id, "adm-1", "admin", None, true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::ReadyToExecute);
        let r = s.get(&id).await.unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Approved);
        // admin recorded as both gates
        assert_eq!(r.manager_by.as_deref(), Some("adm-1"));
        assert_eq!(r.admin_by.as_deref(), Some("adm-1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_double_sign_rejected() {
        let s = store();
        let id = make(&s, "employee").await;
        s.decide(&id, "mgr-1", "manager", None, true, "").await.unwrap();
        // second manager sign on the already-manager-signed request is refused
        assert!(s.decide(&id, "mgr-2", "manager", None, true, "").await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deny_is_terminal() {
        let s = store();
        let id = make(&s, "employee").await;
        let o = s.decide(&id, "mgr-1", "manager", None, false, "not needed").await.unwrap();
        assert_eq!(o, DecideOutcome::Denied);
        let r = s.get(&id).await.unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Denied);
        assert_eq!(r.decided_reason.as_deref(), Some("not needed"));
        // no further decisions accepted
        assert!(s.decide(&id, "adm-1", "admin", None, true, "").await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expiry_counts_as_terminal() {
        let s = store();
        // insert an already-expired pending row directly
        {
            let conn = s.conn.lock().await;
            conn.execute(
                "INSERT INTO install_requests (id,kind,title,description,requester_id,requester_email,requester_role,risk_level,scan,payload,status,created_at,ttl_seconds)
                 VALUES ('old','skill','t','','u','e','employee','Low','[]','{}','pending',?1,1)",
                params![(Utc::now() - chrono::Duration::seconds(600)).to_rfc3339()],
            ).unwrap();
        }
        let n = s.expire_stale().await.unwrap();
        assert_eq!(n, 1);
        let r = s.get("old").await.unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Expired);
        assert!(s.decide("old", "adm-1", "admin", None, true, "").await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mine_and_pending_lists() {
        let s = store();
        let id1 = make(&s, "employee").await;
        let _id2 = make(&s, "manager").await;
        assert_eq!(s.list_pending().await.unwrap().len(), 2);
        assert_eq!(s.list_for_requester("u-1").await.unwrap().len(), 2);
        s.decide(&id1, "adm-1", "admin", None, true, "").await.unwrap();
        assert_eq!(s.list_pending().await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_may_sign_only_own_department() {
        let s = store();
        // employee in "sales" — only a sales manager may clear the manager gate
        let id = make_dept(&s, "employee", Some("sales")).await;
        // wrong-department manager is refused
        assert!(s.decide(&id, "m-eng", "manager", Some("eng"), true, "").await.is_err());
        // manager with no department is refused (not anyone's dept manager)
        assert!(s.decide(&id, "m-none", "manager", None, true, "").await.is_err());
        // same-department manager (case-insensitive) succeeds
        let o = s.decide(&id, "m-sales", "manager", Some("Sales"), true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::AdvancedToAdmin);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_department_falls_back_to_any_manager() {
        let s = store();
        let id = make_dept(&s, "employee", None).await;
        // request without a department can be signed by any manager
        let o = s.decide(&id, "m-any", "manager", Some("whatever"), true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::AdvancedToAdmin);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admin_bypasses_department_routing() {
        let s = store();
        let id = make_dept(&s, "employee", Some("sales")).await;
        // admin (no department) still clears an out-of-department request
        let o = s.decide(&id, "adm", "admin", None, true, "").await.unwrap();
        assert_eq!(o, DecideOutcome::ReadyToExecute);
    }

    #[test]
    fn status_from_db_fails_closed() {
        assert_eq!(RequestStatus::from_db("garbage"), RequestStatus::Denied);
    }
}
