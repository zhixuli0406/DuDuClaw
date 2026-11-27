//! Protected SQLite locator store. No raw ticket is persisted.
use std::{fs, path::Path};
use rusqlite::{Connection, params, TransactionBehavior};
use sha2::{Digest, Sha256};
use super::{Error, Receipt};

pub(super) const TTL: i64 = 600;
pub(super) struct Store { conn: Connection }
#[derive(Clone, Debug)]
pub(super) struct Ticket {
    pub id: String,
    pub approval_id: String,
    pub card_id: String,
    pub user_id: String,
    pub digest: String,
    pub expires: i64,
}
fn hash(raw: &str) -> String { hex::encode(Sha256::digest(raw.as_bytes())) }
fn regular(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(Error::unavailable("file")),
    }
}
impl Store {
    pub fn open(home: &Path) -> Result<Self, Error> {
        let home = home.canonicalize().map_err(Error::unavailable)?;
        let directory = home.join("protected");
        match fs::symlink_metadata(&directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)] {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                // Another request may have created it concurrently. Recheck below.
                if let Err(e) = builder.create(&directory) {
                    if e.kind() != std::io::ErrorKind::AlreadyExists { return Err(Error::unavailable(e)); }
                }
            }
            _ => return Err(Error::unavailable("directory")),
        }
        let meta = fs::symlink_metadata(&directory).map_err(Error::unavailable)?;
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err(Error::unavailable("directory")); }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(Error::unavailable)?;
        }
        let path = directory.join("approval_step_up.db");
        regular(&path)?;
        // Check SQLite companions too, before SQLite can open any of them.
        for suffix in ["-journal", "-wal", "-shm"] {
            regular(&directory.join(format!("approval_step_up.db{suffix}")))?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => regular(&path)?,
            Err(e) => return Err(Error::unavailable(e)),
        }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(Error::unavailable)?;
        }
        let conn = Connection::open(&path).map_err(Error::unavailable)?;
        conn.busy_timeout(std::time::Duration::from_secs(2)).map_err(Error::unavailable)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS tickets (
            id TEXT PRIMARY KEY, ticket_hash TEXT UNIQUE NOT NULL,
            purpose TEXT NOT NULL, approval_id TEXT NOT NULL, card_id TEXT NOT NULL,
            user_id TEXT NOT NULL, digest TEXT NOT NULL, method TEXT NOT NULL,
            prompt_message_id TEXT, attempts INTEGER NOT NULL DEFAULT 0,
            session_hash TEXT, status TEXT NOT NULL, expires INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS tickets_user_approval ON tickets(user_id,approval_id,status);")
            .map_err(Error::unavailable)?;
        conn.execute("DELETE FROM tickets WHERE expires < ?1", [chrono::Utc::now().timestamp() - 86400])
            .map_err(Error::unavailable)?;
        Ok(Self { conn })
    }

    pub fn issue(&mut self, receipt: &Receipt) -> Result<String, Error> {
        if receipt.purpose != "rm143" || receipt.status != "live"
            || uuid::Uuid::parse_str(&receipt.user_id).is_err() { return Err(Error::missing()); }
        let now = chrono::Utc::now().timestamp();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(Error::unavailable)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM tickets WHERE user_id=?1 AND approval_id=?2
            AND status='live' AND expires>?3", params![receipt.user_id, receipt.approval_id, now], |r| r.get(0))
            .map_err(Error::unavailable)?;
        if count >= 3 { return Err(Error::unavailable("ticket limit")); }
        use ring::rand::{SecureRandom, SystemRandom};
        let mut bytes = [0u8; 32];
        SystemRandom::new().fill(&mut bytes).map_err(|_| Error::unavailable("random"))?;
        let raw = hex::encode(bytes);
        tx.execute("INSERT INTO tickets(id,ticket_hash,purpose,approval_id,card_id,user_id,digest,method,status,expires)
            VALUES(?1,?2,'rm143',?3,?4,?5,?6,'web','live',?7)",
            params![uuid::Uuid::new_v4().to_string(), hash(&raw), receipt.approval_id, receipt.card_id,
                receipt.user_id, receipt.digest, now + TTL]).map_err(Error::unavailable)?;
        tx.commit().map_err(Error::unavailable)?;
        Ok(raw)
    }

    /// Only the authenticated owner's rows are searched; hash verification is
    /// constant-time in Rust, not a string equality predicate in SQLite.
    pub fn preview(&self, raw: &str, user: &str) -> Result<Ticket, Error> {
        if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(Error::missing());
        }
        let digest = hash(raw);
        let mut stmt = self.conn.prepare("SELECT id,approval_id,card_id,user_id,digest,expires,ticket_hash,status
            FROM tickets WHERE user_id=?1 AND purpose='rm143' AND method='web'").map_err(Error::unavailable)?;
        let rows = stmt.query_map([user], |r| Ok((Ticket { id:r.get(0)?, approval_id:r.get(1)?,
            card_id:r.get(2)?, user_id:r.get(3)?, digest:r.get(4)?, expires:r.get(5)? },
            r.get::<_,String>(6)?, r.get::<_,String>(7)?))).map_err(Error::unavailable)?;
        let mut matched = None;
        for row in rows {
            let (ticket, saved, status) = row.map_err(Error::unavailable)?;
            if ring::constant_time::verify_slices_are_equal(saved.as_bytes(), digest.as_bytes()).is_ok() {
                matched = Some((ticket, status));
            }
        }
        let (ticket, status) = matched.ok_or_else(Error::missing)?;
        if ticket.expires <= chrono::Utc::now().timestamp() { return Err(Error::expired()); }
        if status != "live" { return Err(Error::used()); }
        Ok(ticket)
    }

    /// Durable one-winner CAS, committed before the approval service runs.
    /// Never roll back or revive a ticket after an uncertain decision.
    pub fn consume(&self, ticket: &Ticket, session: &str) -> Result<(), Error> {
        let n = self.conn.execute("UPDATE tickets SET status='done',session_hash=?1 WHERE id=?2
            AND user_id=?3 AND card_id=?4 AND digest=?5 AND purpose='rm143' AND method='web'
            AND status='live' AND expires>?6", params![hash(session), ticket.id, ticket.user_id,
                ticket.card_id, ticket.digest, chrono::Utc::now().timestamp()]).map_err(Error::unavailable)?;
        if n != 1 { return Err(if ticket.expires <= chrono::Utc::now().timestamp() { Error::expired() } else { Error::used() }); }
        Ok(())
    }
}
