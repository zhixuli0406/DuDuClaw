//! Gateway-only website credentials. Each complete record is AES-GCM sealed
//! with the filesystem `.keyfile`; no OS protected-key dependency is involved.
//! Public responses use `Metadata`, which has no password field.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use duduclaw_security::crypto::CryptoEngine;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const UNAVAILABLE: &str = "網站登入資料目前無法讀取或保存，請管理員檢查。";
const HELD: &str = "這筆網站登入資料保留給已註銷員工的復原使用；清除該員工的保留資料之前不能刪除。";
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
    /// F6h (§3.3): grants of tombstoned employees, held for a same-id
    /// restore. Never usable (only `allowed_agents` is), never deleted while
    /// held; the purge of the quarantine row removes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_grants: Vec<HeldGrant>,
}

/// One held grant: the tombstoned label and its quarantine row key
/// (`quarantine:<employee_hex>-<incarnation>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HeldGrant {
    pub agent: String,
    pub key: String,
}

const HELD_KEY_PREFIX: &str = "quarantine:";

fn valid_held(grant: &HeldGrant) -> bool {
    duduclaw_core::is_valid_agent_id(&grant.agent)
        && grant.key.len() <= 128 && grant.key.len() > HELD_KEY_PREFIX.len()
        && grant.key.starts_with(HELD_KEY_PREFIX)
        && grant.key.chars().all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '-')
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
        // A record whose only grants are held (F6h) is valid but unusable.
        && (!m.allowed_agents.is_empty() || !m.held_grants.is_empty())
        && m.allowed_agents.len() <= 1000
        && m.allowed_agents.iter().all(|agent| duduclaw_core::is_valid_agent_id(agent))
        && m.held_grants.len() <= 1000 && m.held_grants.iter().all(valid_held)
        && m.held_grants.iter().enumerate().all(|(i, grant)| !m.held_grants.iter().take(i).any(|g| g == grant))
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
            // An admin edit never drops grants held for a restore (F6h).
            held_grants: previous.map(|(_, r)| r.metadata.held_grants.clone()).unwrap_or_default(),
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
        // F6h: a record a quarantine row still references stays until that
        // row is purged (or the grant is restored).
        if !records[index].metadata.held_grants.is_empty() { return Err(HELD.into()); }
        store.records.remove(index);
        Ok(true)
    })
}

// ── F6h (isolation_label_stores, §3.3): grants keyed by one label ──

/// Records granting `key` (exact label), or holding a grant under `key`
/// (a quarantine row key). A missing store is 0; an unreadable one is `Err`,
/// never zero.
pub(crate) fn agent_grant_count(home: &Path, key: &str) -> Result<u64, String> {
    Ok(list(home)?.iter().filter(|m| m.allowed_agents.iter().any(|a| a == key)
        || m.held_grants.iter().any(|held| held.key == key)).count() as u64)
}

enum Change { Keep, Update(Metadata), Remove }

/// Re-seal every record `decide` changes, under the store lock (all records
/// validated first). A missing store is not created. Returns the metadata of
/// the changed records as they were before the change (never a secret).
fn rewrite(home: &Path, mut decide: impl FnMut(&Metadata) -> Change) -> Result<Vec<Metadata>, String> {
    if matches!(std::fs::symlink_metadata(path(home)), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(Vec::new());
    }
    modify(home, |store, records| {
        let changes: Vec<Change> = records.iter().map(|record| decide(&record.metadata)).collect();
        if changes.iter().all(|change| matches!(change, Change::Keep)) { return Ok(Vec::new()); }
        let engine = read_engine(home).ok_or(UNAVAILABLE)?;
        let mut next = Vec::with_capacity(store.records.len());
        let mut touched = Vec::new();
        for ((sealed, record), change) in store.records.drain(..).zip(&records).zip(changes) {
            match change {
                Change::Keep => next.push(sealed),
                Change::Remove => touched.push(record.metadata.clone()),
                Change::Update(mut metadata) => {
                    metadata.updated_at = chrono::Utc::now().to_rfc3339();
                    if !valid_metadata(&metadata) { return Err(UNAVAILABLE.into()); }
                    let envelope = SealedRecord { purpose: "website_credentials/v1".into(),
                        record: SecretRecord { metadata, password: record.password.clone() } };
                    let plain = Zeroizing::new(serde_json::to_string(&envelope).map_err(|_| UNAVAILABLE)?);
                    next.push(engine.encrypt_string(&plain).map_err(|_| UNAVAILABLE)?);
                    touched.push(record.metadata.clone());
                }
            }
        }
        store.records = next;
        Ok(touched)
    })
}

/// Tombstone (F6h, §3.3): move `agent`'s grant on every record into a held
/// grant under `key` (the quarantine row key). Nothing is deleted, so a
/// same-id restore can re-grant it; a record granted only to `agent` stays
/// as a held-only (unusable) record. Idempotent. Returns `(moved, held)`:
/// `(id, label)` of the records changed now, and of every record that holds
/// the grant under `key` afterwards (so a retry can record them again).
pub(crate) fn hold_agent(home: &Path, agent: &str, key: &str)
    -> Result<(Vec<(String, String)>, Vec<(String, String)>), String> {
    let grant = HeldGrant { agent: agent.to_owned(), key: key.to_owned() };
    if !valid_held(&grant) { return Err(UNAVAILABLE.into()); }
    let touched = rewrite(home, |m| {
        if !m.allowed_agents.iter().any(|a| a == agent) { return Change::Keep; }
        let mut next = m.clone();
        next.allowed_agents.retain(|a| a != agent);
        if !next.held_grants.contains(&grant) { next.held_grants.push(grant.clone()); }
        Change::Update(next)
    })?;
    let moved: Vec<(String, String)> = touched.into_iter().map(|m| (m.id, m.label)).collect();
    let held: Vec<(String, String)> = list(home)?.into_iter()
        .filter(|m| m.held_grants.contains(&grant)).map(|m| (m.id, m.label)).collect();
    Ok((moved, held))
}

/// Outcome of [`regrant_held`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Regrant {
    /// The held grant was moved back to `allowed_agents`.
    Regranted,
    /// Nothing held, but the record already grants the agent (a retry).
    AlreadyGranted,
    /// The record is gone, or the grant is no longer held under that key.
    Missing,
}

/// Same-id restore (F6c): move the grant of `agent` held under `key` on the
/// record `id` back into `allowed_agents`. Exact id, agent and key.
pub(crate) fn regrant_held(home: &Path, agent: &str, key: &str, id: &str) -> Result<Regrant, String> {
    let grant = HeldGrant { agent: agent.to_owned(), key: key.to_owned() };
    if !valid_held(&grant) || uuid::Uuid::parse_str(id).is_err() { return Err(UNAVAILABLE.into()); }
    let touched = rewrite(home, |m| {
        if m.id != id || !m.held_grants.contains(&grant) { return Change::Keep; }
        let mut next = m.clone();
        next.held_grants.retain(|held| held != &grant);
        next.allowed_agents.push(agent.to_owned());
        next.allowed_agents.sort();
        next.allowed_agents.dedup();
        Change::Update(next)
    })?;
    if !touched.is_empty() { return Ok(Regrant::Regranted); }
    let granted = list(home)?.iter().any(|m| m.id == id && m.allowed_agents.iter().any(|a| a == agent));
    Ok(if granted { Regrant::AlreadyGranted } else { Regrant::Missing })
}

/// Purge (F6i) and adoption-time detach: remove `key` from every grant list
/// and every grant held under `key` (a quarantine row key), re-sealing the
/// changed records; a record left with no grant and no held grant is deleted
/// for good. Grants held under other keys are untouched. Returns the
/// credential labels (never a username or password).
pub(crate) fn purge_agent(home: &Path, key: &str) -> Result<Vec<String>, String> {
    let touched = rewrite(home, |m| {
        let granted = m.allowed_agents.iter().any(|a| a == key);
        let held = m.held_grants.iter().any(|grant| grant.key == key);
        if !granted && !held { return Change::Keep; }
        let mut next = m.clone();
        next.allowed_agents.retain(|a| a != key);
        next.held_grants.retain(|grant| grant.key != key);
        if next.allowed_agents.is_empty() && next.held_grants.is_empty() { Change::Remove } else { Change::Update(next) }
    })?;
    let mut labels: Vec<String> = touched.into_iter().map(|m| m.label.clone()).collect();
    labels.sort();
    Ok(labels)
}

#[cfg(test)]
mod tests;
