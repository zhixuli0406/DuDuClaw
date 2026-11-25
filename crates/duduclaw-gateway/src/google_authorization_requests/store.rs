//! Durable, home-bound single-use tickets. Raw tickets and OAuth states never
//! enter SQLite. A claimed outbox is never replayed after an uncertain crash.
use std::path::{Path, PathBuf};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const REFUSED: &str = "這次授權請求已失效或不屬於目前登入的帳號，請回原對話重新提出要求。";
pub(crate) const TTL: i64 = 600;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Request {
    pub id: String,
    pub home: PathBuf,
    pub user: String,
    pub agent: String,
    pub tool: String,
    pub purpose: String,
    pub mode: String,
    pub email: String,
    pub google_sub: Option<String>,
    pub channel: String,
    pub sender: String,
    pub conversation: String,
    pub session_id: String,
    pub session_generation: Option<String>,
    pub source: Option<serde_json::Value>,
    pub text: String,
    pub acl_version: u64,
    pub config_version: String,
    pub expires: i64,
}

pub(crate) struct Store { conn: Connection, home: PathBuf }
fn unavailable(_: impl std::fmt::Display) -> String { REFUSED.into() }
pub(crate) fn digest(value: &str) -> String { hex::encode(Sha256::digest(value.as_bytes())) }
impl Store {
    pub fn open(home: &Path) -> Result<Self, String> {
        let home = home.canonicalize().map_err(unavailable)?;
        let directory=home.join("protected");
        match std::fs::symlink_metadata(&directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink()=>{},
            Ok(_)=>return Err(REFUSED.into()),
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>std::fs::create_dir(&directory).map_err(unavailable)?,
            Err(e)=>return Err(unavailable(e)),
        }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory,std::fs::Permissions::from_mode(0o700)).map_err(unavailable)?;
        }
        let path = directory.join("google_authorization_requests.db");
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if !meta.is_file() || meta.file_type().is_symlink() { return Err(REFUSED.into()); }
        }
        let conn = Connection::open(&path).map_err(unavailable)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(unavailable)?;
        }
        conn.busy_timeout(std::time::Duration::from_secs(2)).map_err(unavailable)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS requests (
            id TEXT PRIMARY KEY, ticket_hash TEXT UNIQUE NOT NULL, user_id TEXT NOT NULL,
            payload TEXT NOT NULL, expires INTEGER NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('waiting','confirming','oauth','ready','claimed','done','stopped')),
            state_hash TEXT UNIQUE, session_hash TEXT, receipt TEXT);
            CREATE INDEX IF NOT EXISTS requests_member ON requests(user_id, status);").map_err(unavailable)?;
        // Bound retention; never keep the original chat indefinitely.
        conn.execute("DELETE FROM requests WHERE expires < ?1", [chrono::Utc::now().timestamp() - 86400]).map_err(unavailable)?;
        Ok(Self { conn, home })
    }
    pub fn issue(&self, mut request: Request) -> Result<String, String> {
        let count: i64 = self.conn.query_row("SELECT count(*) FROM requests WHERE user_id=?1 AND expires>?2 AND status IN ('waiting','confirming','oauth','ready')", params![request.user, chrono::Utc::now().timestamp()], |r| r.get(0)).map_err(unavailable)?;
        if count >= 8 { return Err(REFUSED.into()); }
        request.home = self.home.clone();
        request.expires = chrono::Utc::now().timestamp() + TTL;
        let ticket = hex::encode(rand::random::<[u8;32]>());
        self.conn.execute("INSERT INTO requests(id,ticket_hash,user_id,payload,expires,status) VALUES(?1,?2,?3,?4,?5,'waiting')", params![request.id, digest(&ticket), request.user, serde_json::to_string(&request).map_err(unavailable)?, request.expires]).map_err(unavailable)?;
        Ok(ticket)
    }
    /// Read-only preview: no state transition, session binding, or OAuth.
    pub fn preview(&self, ticket: &str, user: &str) -> Result<Request, String> {
        if ticket.len()!=64 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(REFUSED.into()); }
        let raw: Option<String> = self.conn.query_row("SELECT payload FROM requests WHERE ticket_hash=?1 AND user_id=?2 AND status='waiting' AND expires>?3", params![digest(ticket), user, chrono::Utc::now().timestamp()], |r| r.get(0)).optional().map_err(unavailable)?;
        self.decode(raw.ok_or(REFUSED)?)
    }
    fn decode(&self, raw: String) -> Result<Request,String> {
        let row: Request = serde_json::from_str(&raw).map_err(unavailable)?;
        if row.home != self.home || row.expires <= chrono::Utc::now().timestamp() { return Err(REFUSED.into()); }
        Ok(row)
    }
    pub fn claim_ticket(&self, ticket: &str, user: &str, session: &str) -> Result<Request,String> {
        let row = self.preview(ticket,user)?;
        self.cas(&row.id,"waiting","confirming")?;
        self.conn.execute("UPDATE requests SET session_hash=?2 WHERE id=?1",params![row.id,digest(session)]).map_err(unavailable)?;
        Ok(row)
    }
    pub fn bind_oauth(&self, id: &str, state: &str) -> Result<(),String> {
        let changed = self.conn.execute("UPDATE requests SET state_hash=?2,status='oauth' WHERE id=?1 AND status='confirming'",params![id,digest(state)]).map_err(unavailable)?;
        if changed!=1 { return Err(REFUSED.into()); } Ok(())
    }
    pub fn oauth(&self, state: &str, user: &str, session: &str) -> Result<Option<Request>,String> {
        let entry: Option<(String,String,String,String)> = self.conn.query_row("SELECT payload,user_id,session_hash,status FROM requests WHERE state_hash=?1",[digest(state)], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(unavailable)?;
        let Some((raw,owner,bound,status)) = entry else { return Ok(None); };
        if owner!=user || bound!=digest(session) || status!="oauth" { return Err(REFUSED.into()); }
        self.decode(raw).map(Some)
    }
    pub fn ready(&self, row: &Request, receipt: &serde_json::Value) -> Result<(),String> {
        let changed = self.conn.execute("UPDATE requests SET payload=?2,receipt=?3,status='ready' WHERE id=?1 AND status IN ('oauth','confirming') AND expires>?4",params![row.id,serde_json::to_string(row).map_err(unavailable)?,receipt.to_string(),chrono::Utc::now().timestamp()]).map_err(unavailable)?;
        if changed!=1 { return Err(REFUSED.into()); } Ok(())
    }
    pub fn ready_rows(&self) -> Result<Vec<(Request,serde_json::Value)>,String> {
        let mut stmt = self.conn.prepare("SELECT payload,receipt FROM requests WHERE status='ready' AND expires>?1 LIMIT 32").map_err(unavailable)?;
        let rows = stmt.query_map([chrono::Utc::now().timestamp()],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(unavailable)?;
        rows.map(|r| { let (raw,receipt)=r.map_err(unavailable)?; Ok((self.decode(raw)?,serde_json::from_str(&receipt).map_err(unavailable)?)) }).collect()
    }
    pub fn cas(&self, id: &str, from: &str, to: &str) -> Result<(),String> {
        let changed = self.conn.execute("UPDATE requests SET status=?3 WHERE id=?1 AND status=?2 AND expires>?4",params![id,from,to,chrono::Utc::now().timestamp()]).map_err(unavailable)?;
        if changed!=1 { return Err(REFUSED.into()); } Ok(())
    }
    pub fn statuses(&self,user:&str) -> Result<Vec<serde_json::Value>,String> {
        let mut stmt=self.conn.prepare("SELECT id,payload,status,expires FROM requests WHERE user_id=?1 ORDER BY expires DESC LIMIT 20").map_err(unavailable)?;
        let rows=stmt.query_map([user],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?))).map_err(unavailable)?;
        rows.map(|r| { let (id,raw,status,expires)=r.map_err(unavailable)?; let row:Request=serde_json::from_str(&raw).map_err(unavailable)?;
            if row.home!=self.home { return Err(REFUSED.into()); }
            Ok(serde_json::json!({"id":id,"agent":row.agent,"purpose":row.purpose,"status":if expires<=chrono::Utc::now().timestamp() && status!="done" {"expired"} else {&status}})) }).collect()
    }
}
