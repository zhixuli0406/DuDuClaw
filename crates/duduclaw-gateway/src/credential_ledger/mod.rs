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
    Ok(db)
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

fn fingerprint(kind: &str, value: &str, key: Option<&duduclaw_security::credential_protection::ProtectedKey>) -> Option<String> {
    if !matches!(kind, "api_key" | "oauth_token" | "ssh_key" | "certificate") || value.len() < 20 {
        return None;
    }
    key.and_then(|key| key.credential_fingerprint(kind, value.as_bytes()))
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

/// Blocking key-store and audit-file locks cannot hold a channel task. A timed
/// out job keeps its permit until it actually exits; at most four may linger.
pub async fn process_message(home: &Path, text: &str, source_ref: &str, sender: &str, agent: &str, source_kind: &str) -> String {
    let Ok(permit) = work_slots().clone().try_acquire_owned() else {
        return credential_detect::mask(text);
    };
    let (home, input, source, sender, agent, kind) = (
        home.to_path_buf(), text.to_owned(), source_ref.to_owned(), sender.to_owned(), agent.to_owned(), source_kind.to_owned(),
    );
    if kind == "chat" {
        source_links().lock().unwrap_or_else(|e| e.into_inner())
            .insert((home.clone(), source.clone()), None);
    }
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = std::panic::catch_unwind(|| process(&home, &input, &source, &sender, &agent, &kind))
            .unwrap_or_else(|_| credential_detect::UNAVAILABLE_MASK.into());
        finish_source(&home, &source);
        result
    });
    match tokio::time::timeout(std::time::Duration::from_millis(250), job).await {
        Ok(Ok(text)) => text,
        _ => credential_detect::mask(text),
    }
}

pub async fn observe_tool_bounded(home: &Path, agent: &str, text: String) {
    let Ok(permit) = work_slots().clone().try_acquire_owned() else { return };
    let (home, agent) = (home.to_path_buf(), agent.to_owned());
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        observe_tool(&home, &agent, &text);
    });
    let _ = tokio::time::timeout(std::time::Duration::from_millis(250), job).await;
}

/// Does not block delivery on database failure. Detection failure substitutes
/// the entire message even in audit mode, so derived stores cannot inherit it.
pub fn process(home: &Path, text: &str, source_ref: &str, sender: &str, agent: &str, source_kind: &str) -> String {
    let detected = std::panic::catch_unwind(|| credential_detect::detect(text));
    let findings = match detected {
        Ok(Ok(findings)) => findings,
        _ => return "[憑證偵測失敗，訊息已遮罩]".into(),
    };
    if findings.is_empty() {
        return if mode(home) == "mask" { credential_detect::mask_with_ids(text, &findings, &[]) } else { text.to_owned() };
    }
    let ids = std::panic::catch_unwind(|| record(home, text, &findings, source_ref, sender, agent, source_kind))
        .unwrap_or_else(|_| Err(UNAVAILABLE.into()))
        .unwrap_or_else(|_| vec!["cred_unavailable".into(); findings.len()]);
    if mode(home) == "mask" {
        credential_detect::mask_with_ids(text, &findings, &ids)
    } else {
        text.to_owned()
    }
}

fn record(home: &Path, text: &str, findings: &[Finding], source_ref: &str, sender: &str, agent: &str, source_kind: &str) -> Result<Vec<String>, String> {
    let protection = duduclaw_security::credential_protection::CredentialProtection::platform(home);
    // No OS-store call for low-entropy-only messages.
    let key = if findings.iter().any(|f| matches!(f.kind.as_str(), "api_key" | "oauth_token" | "ssh_key" | "certificate")) {
        protection.load_key_for_gateway().ok()
    } else { None };
    record_with_key(home, text, findings, source_ref, sender, agent, source_kind, key.as_ref(), chrono::Utc::now().timestamp())
}

fn record_with_key(home: &Path, text: &str, findings: &[Finding], source_ref: &str, sender: &str, agent: &str, source_kind: &str, key: Option<&duduclaw_security::credential_protection::ProtectedKey>, now: i64) -> Result<Vec<String>, String> {
    let mut db = open(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    tx.execute("DELETE FROM detection_quota WHERE at <= ?1", [now - 3600]).map_err(|_| UNAVAILABLE)?;
    let mut used: i64 = tx.query_row("SELECT COALESCE(SUM(count),0) FROM detection_quota WHERE sender=?1", [sender], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    let mut rows: i64 = tx.query_row("SELECT count(*) FROM credentials", [], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
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
        let fp = fingerprint(&finding.kind, value, key);
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
        if existing.is_none() {
            tx.execute("INSERT INTO credentials(id,fingerprint,kind,account_hint,service,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at) VALUES(?1,?2,?3,?4,?5,'detected',?6,?7,?8,'active',?9,?9)",
                params![id, fp, finding.kind, finding.account_hint, finding.service_hint, source_kind, source_ref, sender, at]).map_err(|_| UNAVAILABLE)?;
            rows += 1;
            events.push(("credential_detected", json!({"credential_id":id,"kind":finding.kind})));
        } else {
            tx.execute("UPDATE credentials SET last_seen_at=?2 WHERE id=?1", params![id, at]).map_err(|_| UNAVAILABLE)?;
        }
        // Holders come only from gateway routing facts, never from text or a
        // model-supplied declaration. Passwords cannot merge across sources.
        let inserted = tx.execute("INSERT OR IGNORE INTO credential_holders(credential_id,holder_kind,holder_id,granted_at,granted_by) VALUES(?1,'agent',?2,?3,'routing')", params![id,agent,at]).map_err(|_| UNAVAILABLE)?;
        if inserted > 0 { events.push(("credential_holder_added", json!({"credential_id":id,"holder_kind":"agent"}))); }
        if source_kind == "chat" {
            let inserted = tx.execute("INSERT OR IGNORE INTO credential_holders(credential_id,holder_kind,holder_id,granted_at,granted_by) VALUES(?1,'user',?2,?3,'routing')", params![id,sender,at]).map_err(|_| UNAVAILABLE)?;
            if inserted > 0 { events.push(("credential_holder_added", json!({"credential_id":id,"holder_kind":"user"}))); }
        }
        used += 1;
        tx.execute("INSERT INTO detection_quota(sender,at,count) VALUES(?1,?2,1) ON CONFLICT(sender,at) DO UPDATE SET count=count+1", params![sender,now]).map_err(|_| UNAVAILABLE)?;
        ids.push(id);
    }
    tx.commit().map_err(|_| UNAVAILABLE)?;
    for (name, details) in events { event(home, name, details); }
    if skipped > 0 { event(home, "credential_detection_throttled", json!({"skipped":skipped,"admitted":findings.len()-skipped})); }
    Ok(ids)
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
        let Ok(key) = protection.load_key_for_gateway() else { return };
        let Ok(mut db) = open(home) else { return };
        let Ok(tx) = db.transaction_with_behavior(TransactionBehavior::Immediate) else { return };
        let mut ids = std::collections::BTreeSet::new();
        for finding in findings.iter().take(MESSAGE_LIMIT) {
            let Some(value) = text.get(finding.span.clone()) else { continue };
            let Some(fp) = fingerprint(&finding.kind, value, Some(&key)) else { continue };
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
