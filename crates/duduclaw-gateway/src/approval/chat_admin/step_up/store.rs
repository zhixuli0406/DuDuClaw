//! Protected SQLite locator store. No raw ticket is persisted.
use std::{fs, path::Path};
use rusqlite::{Connection, params, TransactionBehavior, OptionalExtension};
use sha2::{Digest, Sha256};
use super::{Error, Receipt};

pub(super) const TTL: i64 = 600;
pub(super) const TOTP_TTL: i64 = 180;
#[derive(Clone)]
pub(super) struct TotpTicket {
    pub ticket: Ticket,
    pub channel: String,
    pub account: String,
    pub dm: String,
    pub principal: String,
    pub status: String,
    pub attempts: i64,
}
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
            CREATE INDEX IF NOT EXISTS tickets_user_approval ON tickets(user_id,approval_id,status);
            CREATE TABLE IF NOT EXISTS totp_bindings (
                ticket_id TEXT PRIMARY KEY, channel TEXT NOT NULL, account TEXT NOT NULL,
                dm TEXT NOT NULL, principal TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS totp_binding_origin ON totp_bindings(channel,account,dm);")
            .map_err(Error::unavailable)?;
        conn.execute("DELETE FROM tickets WHERE expires < ?1", [chrono::Utc::now().timestamp() - 86400])
            .map_err(Error::unavailable)?;
        conn.execute("DELETE FROM totp_bindings WHERE ticket_id NOT IN (SELECT id FROM tickets)", [])
            .map_err(Error::unavailable)?;
        Ok(Self { conn })
    }

    pub fn issue(&mut self, receipt: &Receipt) -> Result<String, Error> {
        self.issue_method(receipt, "web").map(|(raw, _)| raw)
    }

    pub fn issue_totp(&mut self, receipt: &Receipt) -> Result<String, Error> {
        if receipt.channel != "telegram" || receipt.dm != receipt.principal
            || receipt.message_id.as_deref().and_then(|m| m.parse::<i64>().ok()).is_none_or(|m| m <= 0) {
            return Err(Error::forbidden());
        }
        self.issue_method(receipt, "totp").map(|(_, id)| id)
    }

    fn issue_method(&mut self, receipt: &Receipt, method: &str) -> Result<(String, String), Error> {
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
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO tickets(id,ticket_hash,purpose,approval_id,card_id,user_id,digest,method,status,expires)
            VALUES(?1,?2,'rm143',?3,?4,?5,?6,?8,'live',?7)",
            params![id, hash(&raw), receipt.approval_id, receipt.card_id,
                receipt.user_id, receipt.digest, now + if method == "totp" { TOTP_TTL } else { TTL }, method])
            .map_err(Error::unavailable)?;
        if method == "totp" {
            tx.execute("INSERT INTO totp_bindings(ticket_id,channel,account,dm,principal) VALUES(?1,?2,?3,?4,?5)",
                params![id, receipt.channel, receipt.account, receipt.dm, receipt.principal]).map_err(Error::unavailable)?;
        }
        tx.commit().map_err(Error::unavailable)?;
        Ok((raw, id))
    }

    /// Keep expired/used prompt bindings until the one-day purge, so their
    /// replies are intercepted even though they can no longer authorize.
    pub fn prompt(&self, account: &str, dm: &str, message: &str) -> Result<Option<TotpTicket>, Error> {
        self.conn.query_row("SELECT t.id,t.approval_id,t.card_id,t.user_id,t.digest,t.expires,
                b.channel,b.account,b.dm,b.principal,t.status,t.attempts
            FROM tickets t JOIN totp_bindings b ON b.ticket_id=t.id
            WHERE t.purpose='rm143' AND t.method='totp' AND b.channel='telegram'
                AND b.account=?1 AND b.dm=?2 AND t.prompt_message_id=?3",
            params![account, dm, message], |r| Ok(TotpTicket {
                ticket: Ticket { id:r.get(0)?, approval_id:r.get(1)?, card_id:r.get(2)?,
                    user_id:r.get(3)?, digest:r.get(4)?, expires:r.get(5)? },
                channel:r.get(6)?, account:r.get(7)?, dm:r.get(8)?, principal:r.get(9)?,
                status:r.get(10)?, attempts:r.get(11)?,
            })).optional().map_err(Error::unavailable)
    }

    /// A bare code is control-plane input only for this sender's live DM flow.
    /// This lookup neither verifies a factor nor spends an attempt.
    pub fn has_live_totp(&self, account: &str, dm: &str, principal: &str) -> Result<bool, Error> {
        self.conn.query_row("SELECT EXISTS(SELECT 1
            FROM tickets t JOIN totp_bindings b ON b.ticket_id=t.id
            WHERE t.purpose='rm143' AND t.method='totp' AND t.status='live'
                AND t.attempts<5 AND t.expires>?4 AND b.channel='telegram'
                AND b.account=?1 AND b.dm=?2 AND b.principal=?3)",
            params![account, dm, principal, chrono::Utc::now().timestamp()], |r| r.get(0))
            .map_err(Error::unavailable)
    }

    pub fn attach_prompt(&self, id: &str, message: &str) -> Result<(), Error> {
        if message.parse::<i64>().ok().is_none_or(|m| m <= 0) { return Err(Error::invalid()); }
        let n = self.conn.execute("UPDATE tickets SET prompt_message_id=?2 WHERE id=?1
            AND method='totp' AND status='live' AND prompt_message_id IS NULL AND expires>?3",
            params![id, message, chrono::Utc::now().timestamp()]).map_err(Error::unavailable)?;
        if n == 1 { Ok(()) } else { Err(Error::used()) }
    }

    pub fn void_totp(&self, id: &str) -> Result<(), Error> {
        self.conn.execute("UPDATE tickets SET status='void' WHERE id=?1 AND method='totp' AND status='live'", [id])
            .map_err(Error::unavailable)?;
        Ok(())
    }

    pub fn fail_totp(&mut self, id: &str) -> Result<i64, Error> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(Error::unavailable)?;
        tx.execute("UPDATE tickets SET attempts=attempts+1,
            status=CASE WHEN attempts>=4 THEN 'void' ELSE status END
            WHERE id=?1 AND method='totp' AND status='live' AND attempts<5 AND expires>?2",
            params![id, chrono::Utc::now().timestamp()]).map_err(Error::unavailable)?;
        let attempts = tx.query_row("SELECT attempts FROM tickets WHERE id=?1", [id], |r| r.get(0))
            .map_err(Error::unavailable)?;
        tx.commit().map_err(Error::unavailable)?;
        Ok(attempts)
    }

    pub fn consume_totp(&self, ticket: &Ticket) -> Result<(), Error> {
        let n = self.conn.execute("UPDATE tickets SET status='done' WHERE id=?1 AND user_id=?2
            AND card_id=?3 AND digest=?4 AND purpose='rm143' AND method='totp' AND status='live'
            AND attempts<5 AND prompt_message_id IS NOT NULL AND expires>?5",
            params![ticket.id, ticket.user_id, ticket.card_id, ticket.digest, chrono::Utc::now().timestamp()])
            .map_err(Error::unavailable)?;
        if n == 1 { Ok(()) } else { Err(Error::used()) }
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
