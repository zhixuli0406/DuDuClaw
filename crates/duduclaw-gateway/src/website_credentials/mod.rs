//! Gateway-only website credentials. Each complete record is AES-GCM sealed
//! with the filesystem `.keyfile`; no OS protected-key dependency is involved.
//! Public responses use `Metadata`, which has no password field.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use duduclaw_security::crypto::CryptoEngine;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const UNAVAILABLE: &str = "網站登入資料目前無法讀取或保存，請管理員檢查。";
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Owner {
    pub owner_kind: String,
    pub owner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metadata {
    pub id: String,
    pub label: String,
    pub login_url: String,
    pub origin: String,
    pub login_path: String,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    pub username: String,
    pub allowed_agents: Vec<String>,
    pub created_by: Owner,
    pub updated_at: String,
}

/// Never Debug: the only consumer of the password is the stdin login helper.
#[derive(Serialize, Deserialize)]
pub struct SecretRecord {
    #[serde(flatten)]
    pub metadata: Metadata,
    pub password: String,
}

impl Drop for SecretRecord {
    fn drop(&mut self) {
        self.password.zeroize();
        self.metadata.username.zeroize();
    }
}

/// No Debug and no Serialize: request values are never reflected to clients.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upsert {
    #[serde(default)]
    pub id: Option<String>,
    pub label: String,
    pub login_url: String,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    pub username: String,
    pub password: String,
    pub allowed_agents: Vec<String>,
}

impl Drop for Upsert {
    fn drop(&mut self) {
        self.password.zeroize();
        self.username.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct DiskStore {
    version: u32,
    records: Vec<String>,
}

impl Default for DiskStore {
    fn default() -> Self {
        Self { version: 1, records: Vec::new() }
    }
}

#[derive(Serialize, Deserialize)]
struct SealedRecord {
    purpose: String,
    record: SecretRecord,
}

pub fn path(home: &Path) -> PathBuf {
    home.join("secrets/website_credentials.enc")
}

/// The gateway stores the canonical URL together with its derived origin and
/// pathname. No origin-only fallback is safe for an existing credential.
pub fn normalize_login_url(value: &str) -> Result<url::Url, String> {
    let invalid = "請輸入不含帳密、查詢參數或片段的 HTTPS 登入網址。";
    if value.len() > 4096 || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid.into());
    }
    let url = url::Url::parse(value).map_err(|_| invalid)?;
    if url.as_str().len() > 4096 || url.scheme() != "https" || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some()
        || !valid_login_path(&url.origin().ascii_serialization(), url.path())
    {
        return Err(invalid.into());
    }
    Ok(url)
}

/// Additional destinations are exact same-origin pathnames, never URL patterns.
fn valid_login_path(origin: &str, path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && path.starts_with('/')
        && !path.starts_with("//") && !path.contains('\\')
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
        && url::Url::parse(&format!("{origin}{path}")).is_ok_and(|url| {
            url.origin().ascii_serialization() == origin && url.path() == path
                && url.query().is_none() && url.fragment().is_none()
        })
}

fn short_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_metadata(m: &Metadata) -> bool {
    uuid::Uuid::parse_str(&m.id).is_ok()
        && short_text(&m.label, 128)
        && m.label == m.label.trim()
        && short_text(&m.username, 1024)
        && normalize_login_url(&m.login_url).is_ok_and(|url| {
            url.as_str() == m.login_url && url.origin().ascii_serialization() == m.origin
                && url.path() == m.login_path
        })
        && m.allowed_paths.len() <= 100
        && m.allowed_paths.iter().all(|path| valid_login_path(&m.origin, path))
        && !m.allowed_agents.is_empty() && m.allowed_agents.len() <= 1000
        && m.allowed_agents.iter().all(|agent| duduclaw_core::is_valid_agent_id(agent))
        && m.created_by.owner_kind == "user"
        && short_text(&m.created_by.owner_id, 128)
        && chrono::DateTime::parse_from_rfc3339(&m.updated_at).is_ok()
}

fn read_store(home: &Path) -> Result<DiskStore, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    let file = path(home);
    match std::fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DiskStore::default()),
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BYTES => {}
        _ => return Err(UNAVAILABLE.into()),
    }
    let mut bytes = Vec::new();
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    std::fs::File::open(&file).map_err(|_| UNAVAILABLE)?
        .take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| UNAVAILABLE)?;
    if bytes.len() as u64 > MAX_BYTES { return Err(UNAVAILABLE.into()); }
    let store: DiskStore = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
    if store.version != 1 || store.records.len() > MAX_RECORDS {
        return Err(UNAVAILABLE.into());
    }
    Ok(store)
}

fn read_engine(home: &Path) -> Option<CryptoEngine> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().ok()?;
    let file = home.join(".keyfile");
    let meta = std::fs::symlink_metadata(&file).ok()?;
    if !meta.is_file() || meta.len() != 32 { return None; }
    duduclaw_mcp_boundary::io_authority::verify_before_io().ok()?;
    let bytes = Zeroizing::new(std::fs::read(file).ok()?);
    CryptoEngine::new(bytes.as_slice().try_into().ok()?).ok()
}

fn write_engine(home: &Path, store: &DiskStore) -> Result<CryptoEngine, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    if let Some(engine) = read_engine(home) { return Ok(engine); }
    // Never replace a broken/missing key when ciphertext already exists.
    if !store.records.is_empty() { return Err(UNAVAILABLE.into()); }
    let file = home.join(".keyfile");
    duduclaw_core::with_file_lock(&file, || {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(engine) = read_engine(home) { return Ok(engine); }
        let key = Zeroizing::new(CryptoEngine::generate_key().map_err(std::io::Error::other)?);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut out = opts.open(&file)?;
        duduclaw_core::platform::set_owner_only(&file)?;
        out.write_all(key.as_ref())?;
        out.sync_all()?;
        CryptoEngine::new(&key).map_err(std::io::Error::other)
    }).map_err(|_| UNAVAILABLE.to_string())
}

fn unseal(engine: &CryptoEngine, value: &str) -> Option<SecretRecord> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().ok()?;
    let plain = Zeroizing::new(engine.decrypt_string(value).ok()?);
    let envelope: SealedRecord = serde_json::from_str(&plain).ok()?;
    if envelope.purpose != "website_credentials/v1" || !valid_metadata(&envelope.record.metadata)
        || envelope.record.password.is_empty() || envelope.record.password.len() > 16 * 1024
    {
        return None;
    }
    Some(envelope.record)
}

fn decrypt_records(home: &Path, store: &DiskStore) -> Result<Vec<SecretRecord>, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    if store.records.is_empty() { return Ok(Vec::new()); }
    let engine = read_engine(home).ok_or(UNAVAILABLE)?;
    let records: Vec<_> = store.records.iter()
        .map(|sealed| unseal(&engine, sealed).ok_or_else(|| UNAVAILABLE.to_string()))
        .collect::<Result<_, _>>()?;
    let mut ids = std::collections::HashSet::new();
    let mut labels = std::collections::HashSet::new();
    if records.iter().any(|record| {
        !ids.insert(record.metadata.id.clone()) || !labels.insert(record.metadata.label.clone())
    }) {
        return Err(UNAVAILABLE.into());
    }
    Ok(records)
}

fn records(home: &Path) -> Result<Vec<SecretRecord>, String> {
    decrypt_records(home, &read_store(home)?)
}

pub fn list(home: &Path) -> Result<Vec<Metadata>, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    Ok(records(home)?.iter().map(|r| r.metadata.clone()).collect())
}

/// Only call with the employee identity verified by the signed loopback route.
/// Unknown and ungranted labels are absent; an unreadable store fails closed.
pub fn for_agent(home: &Path, agent: &str, label: &str) -> Result<Option<SecretRecord>, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    Ok(records(home)?.into_iter().find(|record| {
        record.metadata.label == label && record.metadata.allowed_agents.iter().any(|a| a == agent)
    }))
}

fn modify<T>(home: &Path, f: impl FnOnce(&mut DiskStore, Vec<SecretRecord>) -> Result<T, String>) -> Result<T, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    let file = path(home);
    let dir = file.parent().ok_or(UNAVAILABLE)?;
    std::fs::create_dir_all(dir).map_err(|_| UNAVAILABLE)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| UNAVAILABLE)?;
    }
    duduclaw_core::with_file_lock(&file, || {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut store = read_store(home).map_err(std::io::Error::other)?;
        // Validate every existing record under the same lock before any write.
        let existing = decrypt_records(home, &store).map_err(std::io::Error::other)?;
        let result = f(&mut store, existing).map_err(std::io::Error::other)?;
        let bytes = serde_json::to_vec(&store).map_err(std::io::Error::other)?;
        if bytes.len() as u64 > MAX_BYTES { return Err(std::io::Error::other(UNAVAILABLE)); }
        let mut staged = tempfile::NamedTempFile::new_in(dir)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| std::io::Error::other(e.to_string()))?;
        staged.persist(&file).map_err(|e| e.error)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(result)
    }).map_err(|e| e.to_string())
}

pub fn upsert(home: &Path, request: Upsert, actor: &str) -> Result<Metadata, String> {
    modify(home, |store, records| {
        let url = normalize_login_url(&request.login_url)?;
        let login_url = url.as_str().to_string();
        let origin = url.origin().ascii_serialization();
        let login_path = url.path().to_string();
        if request.allowed_paths.len() > 100
            || !request.allowed_paths.iter().all(|path| valid_login_path(&origin, path))
        {
            return Err("第二步登入路徑必須是同網站的完整路徑，不可含查詢參數或片段。".into());
        }
        let label = request.label.trim().to_string();
        if !short_text(&label, 128) || !short_text(&request.username, 1024)
            || request.password.is_empty() || request.password.len() > 16 * 1024
            || request.allowed_agents.is_empty() || request.allowed_agents.len() > 1000
            || !request.allowed_agents.iter().all(|a| duduclaw_core::is_valid_agent_id(a))
            || !short_text(actor, 128)
        {
            return Err("請填寫標籤、帳號、密碼，並至少選擇一位允許使用的員工。".into());
        }
        let engine = write_engine(home, store)?;
        let existing: Vec<_> = records.into_iter().enumerate().collect();
        let previous = match request.id.as_deref() {
            Some(id) => Some(existing.iter().find(|(_, r)| r.metadata.id == id)
                .ok_or("找不到這筆網站登入資料，請重新整理。")?),
            None => None,
        };
        if existing.iter().any(|(_, r)| r.metadata.label == label
            && Some(r.metadata.id.as_str()) != request.id.as_deref())
        {
            return Err("此標籤已被使用，請選擇其他標籤。".into());
        }
        let mut allowed_agents = request.allowed_agents.clone();
        allowed_agents.sort();
        allowed_agents.dedup();
        let metadata = Metadata {
            id: previous.map(|(_, r)| r.metadata.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            label, login_url, origin, login_path, allowed_paths: request.allowed_paths.clone(),
            username: request.username.clone(), allowed_agents,
            created_by: previous.map(|(_, r)| r.metadata.created_by.clone())
                .unwrap_or_else(|| Owner { owner_kind: "user".into(), owner_id: actor.into() }),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        let envelope = SealedRecord { purpose: "website_credentials/v1".into(),
            record: SecretRecord { metadata: metadata.clone(), password: request.password.clone() } };
        let plain = Zeroizing::new(serde_json::to_string(&envelope).map_err(|_| UNAVAILABLE)?);
        let sealed = engine.encrypt_string(&plain).map_err(|_| UNAVAILABLE)?;
        if let Some((index, _)) = previous {
            store.records[*index] = sealed;
        } else {
            if store.records.len() >= MAX_RECORDS { return Err("網站登入資料已達數量上限。".into()); }
            store.records.push(sealed);
        }
        Ok(metadata)
    })
}

pub fn delete(home: &Path, id: &str) -> Result<bool, String> {
    modify(home, |store, records| {
        if uuid::Uuid::parse_str(id).is_err() { return Err("網站登入資料代號不正確。".into()); }
        let Some(index) = records.iter().position(|record| record.metadata.id == id) else {
            return Ok(false);
        };
        store.records.remove(index);
        Ok(true)
    })
}

// ── F6h (isolation_label_stores, §3.3): grants keyed by one label ──

/// Records granting `agent` (exact label). A missing store is 0; an
/// unreadable one is `Err`, never zero.
pub(crate) fn agent_grant_count(home: &Path, agent: &str) -> Result<u64, String> {
    Ok(list(home)?.iter().filter(|m| m.allowed_agents.iter().any(|a| a == agent)).count() as u64)
}

/// Remove `agent` from every grant list under the store lock, re-sealing the
/// changed records. A record granted to nobody else is deleted (an empty
/// grant list is not a valid record); the admin re-creates it. Returns the
/// credential labels (never a username or password).
pub(crate) fn strip_agent(home: &Path, agent: &str) -> Result<Vec<String>, String> {
    if matches!(std::fs::symlink_metadata(path(home)), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(Vec::new());
    }
    modify(home, |store, records| {
        if !records.iter().any(|r| r.metadata.allowed_agents.iter().any(|a| a == agent)) { return Ok(Vec::new()); }
        let engine = read_engine(home).ok_or(UNAVAILABLE)?;
        let mut kept = Vec::with_capacity(records.len());
        let mut labels = Vec::new();
        for record in &records {
            if !record.metadata.allowed_agents.iter().any(|a| a == agent) {
                kept.push(None);
                continue;
            }
            labels.push(record.metadata.label.clone());
            let mut metadata = record.metadata.clone();
            metadata.allowed_agents.retain(|a| a != agent);
            if metadata.allowed_agents.is_empty() { kept.push(Some(None)); continue; }
            metadata.updated_at = chrono::Utc::now().to_rfc3339();
            let envelope = SealedRecord { purpose: "website_credentials/v1".into(),
                record: SecretRecord { metadata, password: record.password.clone() } };
            let plain = Zeroizing::new(serde_json::to_string(&envelope).map_err(|_| UNAVAILABLE)?);
            kept.push(Some(Some(engine.encrypt_string(&plain).map_err(|_| UNAVAILABLE)?)));
        }
        // `None` keeps the stored ciphertext; `Some(None)` drops the record.
        let mut next = Vec::with_capacity(store.records.len());
        for (sealed, change) in store.records.drain(..).zip(kept) {
            match change {
                None => next.push(sealed),
                Some(Some(resealed)) => next.push(resealed),
                Some(None) => {}
            }
        }
        store.records = next;
        labels.sort();
        Ok(labels)
    })
}

#[cfg(test)]
mod tests;
