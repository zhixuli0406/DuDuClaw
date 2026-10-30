//! Conversation credential metadata. Values exist only while detecting and
//! computing an optional protected HMAC; neither SQLite nor events store them.
use std::path::Path;
use duduclaw_security::audit::{append_audit_event, AuditEvent, Severity};
use duduclaw_security::credential_detect::{self, Finding};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

mod query;
pub use query::{declare, get, list, update};
#[cfg(test)]
mod tests;

pub const MESSAGE_LIMIT: usize = 5;
pub const SENDER_LIMIT: i64 = 50;
pub const ROW_LIMIT: i64 = 100_000;
pub const HOLDERS_PER_CREDENTIAL: i64 = 32;
pub const HOLDER_LIMIT: i64 = 200_000;
const ANONYMOUS_CHAT: &str = "webchat-anonymous";
const UNAVAILABLE: &str = "credential ledger unavailable";

fn new_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u128;
    let random = rand::random::<[u8; 10]>();
    let mut value = random.iter().fold(timestamp & 0xffff_ffff_ffff, |n, b| (n << 8) | u128::from(*b));
    let mut encoded = [b'0'; 26];
    for position in (0..26).rev() {
        encoded[position] = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    format!("cred_{}", encoded.into_iter().map(char::from).collect::<String>())
}

tokio::task_local! {
    pub(crate) static DISPATCH_SCANNED: bool;
}

pub fn mode(home: &Path) -> &'static str {
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return "audit",
        Err(_) => return "mask",
    };
    let Ok(table) = raw.parse::<toml::Table>() else { return "mask" };
    match table.get("security") {
        None => "audit",
        Some(section) => match section.as_table() {
            None => "mask",
            Some(section) => match section.get("chat_credentials") {
                None => "audit",
                Some(value) if value.as_str() == Some("audit") => "audit",
                Some(_) => "mask",
            },
        },
    }
}

fn open(home: &Path) -> Result<Connection, String> {
    open_at(home, chrono::Utc::now().timestamp())
}

fn open_at(home: &Path, now: i64) -> Result<Connection, String> {
    let dir = home.join("credentials");
    // Refuse a symlinked ledger directory or database, including broken links.
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err(UNAVAILABLE.into()); }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&dir) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(UNAVAILABLE.into()),
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).map_err(|_| UNAVAILABLE)?;
    let path = dir.join("ledger.db");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_file() || meta.file_type().is_symlink() { return Err(UNAVAILABLE.into()); }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() != 1 { return Err(UNAVAILABLE.into()); }
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|_| UNAVAILABLE)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|_| UNAVAILABLE)?;
    }
    drop(file);
    let db = Connection::open(&path).map_err(|_| UNAVAILABLE)?;
    db.busy_timeout(std::time::Duration::from_millis(100)).map_err(|_| UNAVAILABLE)?;
    db.execute_batch(include_str!("schema.sql")).map_err(|_| UNAVAILABLE)?;
    migrate(&db)?;
    if duduclaw_security::credential_protection::platform_store().level()
        != duduclaw_security::credential_protection::ProtectionLevel::AppBound
    {
        db.execute("UPDATE credentials SET fingerprint=NULL WHERE fingerprint IS NOT NULL", []).map_err(|_| UNAVAILABLE)?;
    }
    db.execute("DELETE FROM detection_quota WHERE at <= ?1", [now - 3600]).map_err(|_| UNAVAILABLE)?;
    db.execute("DELETE FROM credential_ingress_links WHERE at <= ?1", [now - 7 * 86400]).map_err(|_| UNAVAILABLE)?;
    Ok(db)
}

fn migrate(db: &Connection) -> Result<(), String> {
    let tx = db.unchecked_transaction().map_err(|_| UNAVAILABLE)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    if version < 1 {
        let has_note_source: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('credentials') WHERE name='note_source')", [], |r| r.get(0),
        ).map_err(|_| UNAVAILABLE)?;
        if !has_note_source {
            tx.execute_batch("ALTER TABLE credentials ADD COLUMN note_source TEXT NOT NULL DEFAULT 'detected';").map_err(|_| UNAVAILABLE)?;
        }
        // Old rows did not record structural evidence or key protection. Never
        // retain those unproven fingerprints. Preserve all metadata rows even
        // if several now share a source; writers deduplicate under IMMEDIATE.
        tx.execute_batch("DROP INDEX IF EXISTS credential_source_kind;
            CREATE INDEX credential_source_kind ON credentials(source_ref,kind) WHERE fingerprint IS NULL;
            UPDATE credentials SET fingerprint=NULL;
            UPDATE credentials SET note_source='admin' WHERE note IS NOT NULL;
            DELETE FROM credential_holders WHERE holder_kind='user' AND (holder_id LIKE 'webchat:%' OR holder_id LIKE 'widget-visitor:%');
            UPDATE credentials SET provided_by='webchat-anonymous' WHERE provided_by LIKE 'webchat:%' OR provided_by LIKE 'widget-visitor:%';
            PRAGMA user_version=1;").map_err(|_| UNAVAILABLE)?;
    }
    tx.commit().map_err(|_| UNAVAILABLE)?;
    Ok(())
}

/// Routing-owned abuse identity, independent of connection/session suffixes.
/// Unauthenticated WebChat always shares one bucket and is never a user holder.
pub fn chat_principal(session: &str, _sender: &str) -> String {
    if session.starts_with("webchat:") {
        return crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
            .try_with(|user| crate::webchat::credential_principal(user))
            .unwrap_or_else(|_| ANONYMOUS_CHAT.to_string());
    }
    let session = duduclaw_core::sensitivity::session_conversation(session);
    if let Some((channel, chat)) = session.split_once(':') {
        if !channel.is_empty() && !chat.is_empty() {
            return format!("channel:{session}");
        }
    }
    // Unknown routing must not turn caller-supplied random identities into
    // unlimited quota buckets or durable holders.
    "channel:unknown".into()
}

fn event(home: &Path, name: &str, details: Value) {
    let event = AuditEvent::new(name, "credential-ledger", Severity::Info, details);
    #[cfg(unix)]
    let _ = duduclaw_core::with_file_lock(&home.join("security_audit.jsonl"), || {
        append_audit_event(home, &event);
        Ok(())
    });
    // The existing Windows audit writer already holds this sidecar lock.
    #[cfg(not(unix))]
    append_audit_event(home, &event);
}

fn fingerprint(finding: &Finding, value: &str, key: Option<&duduclaw_security::credential_protection::ProtectedKey>) -> Option<String> {
    if !finding.fingerprint_eligible {
        return None;
    }
    key.and_then(|key| key.credential_fingerprint(&finding.kind, value.as_bytes()))
}

type SourceLinks = std::collections::HashMap<(std::path::PathBuf, String), Option<String>>;
fn source_links() -> &'static std::sync::Mutex<SourceLinks> {
    static LINKS: std::sync::OnceLock<std::sync::Mutex<SourceLinks>> = std::sync::OnceLock::new();
    LINKS.get_or_init(|| std::sync::Mutex::new(SourceLinks::new()))
}

fn finish_source(home: &Path, source: &str) {
    let resolved = source_links().lock().unwrap_or_else(|e| e.into_inner())
        .remove(&(home.to_path_buf(), source.to_owned())).flatten();
    if let Some(resolved) = resolved { write_source_link(home, source, &resolved); }
}

fn work_slots() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
}

async fn wait_for_job<T: Send + 'static>(
    permit: tokio::sync::OwnedSemaphorePermit,
    job: tokio::task::JoinHandle<T>,
) -> Option<T> {
    let _permit = permit;
    tokio::time::timeout(std::time::Duration::from_millis(250), job).await.ok()?.ok()
}

struct BlockingJob;
static BLOCKING_JOBS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
impl BlockingJob {
    fn acquire() -> Option<Self> {
        use std::sync::atomic::Ordering;
        BLOCKING_JOBS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 4).then_some(n + 1))
            .ok().map(|_| Self)
    }
}
impl Drop for BlockingJob {
    fn drop(&mut self) {
        BLOCKING_JOBS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Blocking key-store and audit-file locks cannot hold a channel task. A timed
/// out job releases its delivery permit. Separate worker accounting bounds
/// uninterruptible OS calls without retaining a semaphore permit.
pub async fn process_message(home: &Path, text: &str, source_ref: &str, sender: &str, agent: &str, source_kind: &str) -> String {
    let Ok(permit) = work_slots().clone().try_acquire_owned() else {
        return credential_detect::mask(text);
    };
    let Some(worker) = BlockingJob::acquire() else { return credential_detect::mask(text) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    let (home, input, source, sender, agent, kind) = (
        home.to_path_buf(), text.to_owned(), source_ref.to_owned(), sender.to_owned(), agent.to_owned(), source_kind.to_owned(),
    );
    if kind == "chat" {
        source_links().lock().unwrap_or_else(|e| e.into_inner())
            .insert((home.clone(), source.clone()), None);
    }
    let job = tokio::task::spawn_blocking(move || {
        let _worker = worker;
        let result = std::panic::catch_unwind(|| process_until(&home, &input, &source, &sender, &agent, &kind, Some(deadline)))
            .unwrap_or_else(|_| credential_detect::UNAVAILABLE_MASK.into());
        finish_source(&home, &source);
        result
    });
    wait_for_job(permit, job).await.unwrap_or_else(|| credential_detect::mask(text))
}

pub async fn observe_tool_bounded(home: &Path, agent: &str, text: String) {
    let Ok(permit) = work_slots().clone().try_acquire_owned() else { return };
    let Some(worker) = BlockingJob::acquire() else { return };
    let (home, agent) = (home.to_path_buf(), agent.to_owned());
    let job = tokio::task::spawn_blocking(move || {
        let _worker = worker;
        observe_tool(&home, &agent, &text);
    });
    let _ = wait_for_job(permit, job).await;
}

/// Does not block delivery on database failure. Detection failure substitutes
/// the entire message even in audit mode, so derived stores cannot inherit it.
pub fn process(home: &Path, text: &str, source_ref: &str, sender: &str, agent: &str, source_kind: &str) -> String {
    process_until(home, text, source_ref, sender, agent, source_kind, None)
}

fn process_until(home: &Path, text: &str, source_ref: &str, sender: &str, agent: &str, source_kind: &str, deadline: Option<std::time::Instant>) -> String {
    let detected = std::panic::catch_unwind(|| credential_detect::detect(text));
    let findings = match detected {
        Ok(Ok(findings)) => findings,
        _ => return "[憑證偵測失敗，訊息已遮罩]".into(),
    };
    if findings.is_empty() {
        return if mode(home) == "mask" { credential_detect::mask_with_ids(text, &findings, &[]) } else { text.to_owned() };
    }
    let ids = std::panic::catch_unwind(|| record(home, text, &findings, source_ref, sender, agent, source_kind, deadline))
        .unwrap_or_else(|_| Err(UNAVAILABLE.into()))
        .unwrap_or_else(|_| vec!["cred_unavailable".into(); findings.len()]);
    if mode(home) == "mask" {
        credential_detect::mask_with_ids(text, &findings, &ids)
    } else {
        text.to_owned()
    }
}

fn record(home: &Path, text: &str, findings: &[Finding], source_ref: &str, sender: &str, agent: &str, source_kind: &str, deadline: Option<std::time::Instant>) -> Result<Vec<String>, String> {
    let protection = duduclaw_security::credential_protection::CredentialProtection::platform(home);
    // No OS-store call for low-entropy-only messages.
    let key = if findings.iter().any(|f| f.fingerprint_eligible) {
        protection.load_key_for_credential_fingerprint().ok()
    } else { None };
    record_with_deadline(home, text, findings, source_ref, sender, agent, source_kind, key.as_ref(), chrono::Utc::now().timestamp(), deadline)
}

#[cfg(test)]
fn record_with_key(home: &Path, text: &str, findings: &[Finding], source_ref: &str, sender: &str, agent: &str, source_kind: &str, key: Option<&duduclaw_security::credential_protection::ProtectedKey>, now: i64) -> Result<Vec<String>, String> {
    record_with_deadline(home, text, findings, source_ref, sender, agent, source_kind, key, now, None)
}

fn record_with_deadline(home: &Path, text: &str, findings: &[Finding], source_ref: &str, sender: &str, agent: &str, source_kind: &str, key: Option<&duduclaw_security::credential_protection::ProtectedKey>, now: i64, deadline: Option<std::time::Instant>) -> Result<Vec<String>, String> {
    if deadline.is_some_and(|end| std::time::Instant::now() >= end) { return Err(UNAVAILABLE.into()); }
    let mut db = open_at(home, now)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    tx.execute("DELETE FROM detection_quota WHERE at <= ?1", [now - 3600]).map_err(|_| UNAVAILABLE)?;
    let mut used: i64 = tx.query_row("SELECT COALESCE(SUM(count),0) FROM detection_quota WHERE sender=?1", [sender], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    let mut rows: i64 = tx.query_row("SELECT count(*) FROM credentials", [], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    let mut holders: i64 = tx.query_row("SELECT count(*) FROM credential_holders", [], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    let mut ids = Vec::with_capacity(findings.len());
    let mut events = Vec::new();
    let mut skipped = 0;
    let at = chrono::DateTime::from_timestamp(now, 0).ok_or(UNAVAILABLE)?.to_rfc3339();
    for (index, finding) in findings.iter().enumerate() {
        if index >= MESSAGE_LIMIT || used >= SENDER_LIMIT {
            skipped += 1;
            ids.push("cred_throttled".into());
            continue;
        }
        let value = text.get(finding.span.clone()).ok_or(UNAVAILABLE)?;
        let fp = fingerprint(finding, value, key);
        let existing: Option<String> = if let Some(fp) = &fp {
            tx.query_row("SELECT id FROM credentials WHERE fingerprint=?1", [fp], |r| r.get(0)).optional()
        } else {
            tx.query_row("SELECT id FROM credentials WHERE source_ref=?1 AND kind=?2 AND fingerprint IS NULL", params![source_ref, finding.kind], |r| r.get(0)).optional()
        }.map_err(|_| UNAVAILABLE)?;
        if existing.is_none() && rows >= ROW_LIMIT {
            skipped += 1;
            ids.push("cred_throttled".into());
            continue;
        }
        let id = existing.clone().unwrap_or_else(new_id);
        let mut candidates = Vec::new();
        if !agent.is_empty() { candidates.push(("agent", agent)); }
        if source_kind == "chat" && sender != ANONYMOUS_CHAT && !sender.starts_with("webchat:")
            && !sender.starts_with("widget-visitor:")
        {
            candidates.push(("user", sender));
        }
        let existing_holders: i64 = tx.query_row("SELECT count(*) FROM credential_holders WHERE credential_id=?1", [&id], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
        let mut additions = Vec::new();
        for (kind, holder) in candidates {
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM credential_holders WHERE credential_id=?1 AND holder_kind=?2 AND holder_id=?3)", params![id,kind,holder], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
            if !exists { additions.push((kind, holder)); }
        }
        let needed = additions.len() as i64;
        if existing_holders + needed > HOLDERS_PER_CREDENTIAL || holders + needed > HOLDER_LIMIT {
            skipped += 1;
            ids.push("cred_throttled".into());
            continue;
        }
        let ingress = source_ref.starts_with("line-ingress:");
        if ingress {
            let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM credential_ingress_links WHERE source_ref=?1 AND credential_id=?2)", params![source_ref,id], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
            let links: i64 = tx.query_row("SELECT count(*) FROM credential_ingress_links", [], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
            if !linked && links >= ROW_LIMIT {
                skipped += 1;
                ids.push("cred_throttled".into());
                continue;
            }
        }
        if existing.is_none() {
            tx.execute("INSERT INTO credentials(id,fingerprint,kind,account_hint,service,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at) VALUES(?1,?2,?3,?4,?5,'detected',?6,?7,?8,'active',?9,?9)",
                params![id, fp, finding.kind, finding.account_hint, finding.service_hint, source_kind, source_ref, sender, at]).map_err(|_| UNAVAILABLE)?;
            rows += 1;
            events.push(("credential_detected", json!({"credential_id":id,"kind":finding.kind})));
        } else {
            tx.execute("UPDATE credentials SET last_seen_at=?2 WHERE id=?1", params![id, at]).map_err(|_| UNAVAILABLE)?;
        }
        // The IMMEDIATE transaction makes both limits atomic across writers,
        // including the deduplicated-credential path. Anonymous widgets have
        // no durable user holder; their stable shared bucket only limits abuse.
        for (kind, holder) in additions {
            tx.execute("INSERT INTO credential_holders(credential_id,holder_kind,holder_id,granted_at,granted_by) VALUES(?1,?2,?3,?4,'routing')", params![id,kind,holder,at]).map_err(|_| UNAVAILABLE)?;
            holders += 1;
            events.push(("credential_holder_added", json!({"credential_id":id,"holder_kind":kind})));
        }
        if ingress {
            tx.execute("INSERT INTO credential_ingress_links(source_ref,credential_id,at) VALUES(?1,?2,?3) ON CONFLICT(source_ref,credential_id) DO UPDATE SET at=excluded.at", params![source_ref,id,now]).map_err(|_| UNAVAILABLE)?;
        }
        used += 1;
        tx.execute("INSERT INTO detection_quota(sender,at,count) VALUES(?1,?2,1) ON CONFLICT(sender,at) DO UPDATE SET count=count+1", params![sender,now]).map_err(|_| UNAVAILABLE)?;
        ids.push(id);
    }
    // An expired ingress job must not insert metadata after the worker has
    // already attached a (then empty) link set. While this transaction holds
    // its write lock, attachment cannot overtake a successful commit.
    if deadline.is_some_and(|end| std::time::Instant::now() >= end) { return Err(UNAVAILABLE.into()); }
    tx.commit().map_err(|_| UNAVAILABLE)?;
    for (name, details) in events { event(home, name, details); }
    if skipped > 0 { event(home, "credential_detection_throttled", json!({"skipped":skipped,"admitted":findings.len()-skipped})); }
    Ok(ids)
}

/// LINE detects before durable enqueue, when an agent may not be selected.
/// Once routing succeeds, grant the selected agent only the credential ids
/// linked to that ingress event. No original text is needed for retry.
pub async fn attach_ingress_holder(home: &Path, source_ref: &str, agent: &str) -> Result<(), String> {
    let permit = work_slots().clone().try_acquire_owned().map_err(|_| UNAVAILABLE)?;
    let worker = BlockingJob::acquire().ok_or(UNAVAILABLE)?;
    let (home, source, agent) = (home.to_path_buf(), source_ref.to_owned(), agent.to_owned());
    let job = tokio::task::spawn_blocking(move || {
        let _worker = worker;
        attach_ingress_holder_inner(&home, &source, &agent, chrono::Utc::now().timestamp())
    });
    wait_for_job(permit, job).await.ok_or_else(|| UNAVAILABLE.to_string())?
}

fn attach_ingress_holder_inner(home: &Path, source: &str, agent: &str, now: i64) -> Result<(), String> {
    if !source.starts_with("line-ingress:") || agent.is_empty() { return Err(UNAVAILABLE.into()); }
    let mut db = open_at(home, now)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    let ids = {
        let mut stmt = tx.prepare("SELECT credential_id FROM credential_ingress_links WHERE source_ref=?1 ORDER BY credential_id").map_err(|_| UNAVAILABLE)?;
        let rows = stmt.query_map([source], |r| r.get::<_, String>(0)).map_err(|_| UNAVAILABLE)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|_| UNAVAILABLE)?
    };
    let mut total: i64 = tx.query_row("SELECT count(*) FROM credential_holders", [], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    let mut added = Vec::new();
    let at = chrono::DateTime::from_timestamp(now, 0).ok_or(UNAVAILABLE)?.to_rfc3339();
    for id in ids {
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM credential_holders WHERE credential_id=?1 AND holder_kind='agent' AND holder_id=?2)", params![id,agent], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
        if exists { continue; }
        let count: i64 = tx.query_row("SELECT count(*) FROM credential_holders WHERE credential_id=?1", [&id], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
        if count >= HOLDERS_PER_CREDENTIAL || total >= HOLDER_LIMIT { return Err("credential holder limit reached".into()); }
        tx.execute("INSERT INTO credential_holders(credential_id,holder_kind,holder_id,granted_at,granted_by) VALUES(?1,'agent',?2,?3,'routing')", params![id,agent,at]).map_err(|_| UNAVAILABLE)?;
        total += 1;
        added.push(id);
    }
    tx.execute("DELETE FROM credential_ingress_links WHERE source_ref=?1", [source]).map_err(|_| UNAVAILABLE)?;
    tx.commit().map_err(|_| UNAVAILABLE)?;
    for id in added { event(home, "credential_holder_added", json!({"credential_id":id,"holder_kind":"agent"})); }
    Ok(())
}

/// Replace the gateway's provisional source id with the persisted message id.
pub fn bind_message(home: &Path, provisional: &str, session: &str, message_id: i64) {
    let resolved = format!("{session}:m:{message_id}");
    if let Some(link) = source_links().lock().unwrap_or_else(|e| e.into_inner())
        .get_mut(&(home.to_path_buf(), provisional.to_owned()))
    {
        *link = Some(resolved.clone());
    }
    write_source_link(home, provisional, &resolved);
}

fn write_source_link(home: &Path, provisional: &str, resolved: &str) {
    if !home.join("credentials/ledger.db").exists() { return; }
    if let Ok(db) = open(home) {
        let _ = db.execute("UPDATE credentials SET source_ref=?2 WHERE source_ref=?1", params![provisional, resolved]);
    }
}

/// Tool-loop observation only matches existing high-entropy credentials held
/// by this employee. It proves submission to a tool, not successful use.
pub fn observe_tool(home: &Path, agent: &str, text: &str) {
    let _ = std::panic::catch_unwind(|| {
        if !home.join("credentials/ledger.db").exists() { return; }
        let Ok(findings) = credential_detect::detect(text) else { return };
        let protection = duduclaw_security::credential_protection::CredentialProtection::platform(home);
        let Ok(key) = protection.load_key_for_credential_fingerprint() else { return };
        let Ok(mut db) = open(home) else { return };
        let Ok(tx) = db.transaction_with_behavior(TransactionBehavior::Immediate) else { return };
        let mut ids = std::collections::BTreeSet::new();
        for finding in findings.iter().take(MESSAGE_LIMIT) {
            let Some(value) = text.get(finding.span.clone()) else { continue };
            let Some(fp) = fingerprint(finding, value, Some(&key)) else { continue };
            let id: Option<String> = tx.query_row("SELECT c.id FROM credentials c JOIN credential_holders h ON h.credential_id=c.id WHERE c.fingerprint=?1 AND h.holder_kind='agent' AND h.holder_id=?2 AND h.revoked_at IS NULL", params![fp,agent], |r| r.get(0)).optional().ok().flatten();
            if let Some(id) = id { ids.insert(id); }
        }
        let now = chrono::Utc::now().to_rfc3339();
        for id in &ids {
            if tx.execute("UPDATE credentials SET last_used_at=?2 WHERE id=?1", params![id,now]).is_err() { return; }
        }
        if tx.commit().is_err() { return; }
        for id in ids { event(home, "credential_seen_in_tool", json!({"credential_id":id})); }
    });
}
