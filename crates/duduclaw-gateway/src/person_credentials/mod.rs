//! Gateway-only personal credential vault. The clear metadata contains no
//! bearer or refresh token; the secret JSON is sealed with the protected
//! credential key. Every mutation holds the cross-process file lock.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use duduclaw_security::credential_protection::ProtectedKey;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const UNREADABLE: &str = "個人連線紀錄無法讀取，請聯絡管理員；沒有變更已保存的連線。";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Connected,
    NeedsReconnect,
    Suspended,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub owner_kind: String,
    pub owner_id: String,
    pub provider: String,
    pub credential_id: String,
    pub revision: u64,
    pub account_email: String,
    pub google_sub: String,
    pub hosted_domain: Option<String>,
    pub granted_scopes: Vec<String>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_before_suspension: Option<Status>,
    pub connected_at: String,
    pub last_refresh_at: Option<String>,
    pub oauth_client_id: String,
}

/// Intentionally not Debug or Serialize on a public response type.
#[derive(Serialize, Deserialize)]
pub struct Secret {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(refresh) = &mut self.refresh_token {
            refresh.zeroize();
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct Binding {
    owner_id: String,
    credential_id: String,
    google_sub: String,
    account_email: String,
    hosted_domain: Option<String>,
    oauth_client_id: String,
    granted_scopes: Vec<String>,
}

impl From<&Metadata> for Binding {
    fn from(m: &Metadata) -> Self {
        Self {
            owner_id: m.owner_id.clone(),
            credential_id: m.credential_id.clone(),
            google_sub: m.google_sub.clone(),
            account_email: m.account_email.clone(),
            hosted_domain: m.hosted_domain.clone(),
            oauth_client_id: m.oauth_client_id.clone(),
            granted_scopes: m.granted_scopes.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    binding: Binding,
    secret: Secret,
}

#[derive(Serialize)]
struct EnvelopeRef<'a> {
    binding: Binding,
    secret: &'a Secret,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    #[serde(flatten)]
    pub metadata: Metadata,
    secret: String,
}

#[derive(Serialize, Deserialize)]
struct Store {
    version: u32,
    records: Vec<Record>,
    /// A disconnect must fence even a finish that already consumed state
    /// and is still waiting for Google. Epochs persist without a record.
    #[serde(default)]
    epochs: BTreeMap<String, u64>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: VERSION,
            records: Vec::new(),
            epochs: BTreeMap::new(),
        }
    }
}

pub fn path(home: &Path) -> PathBuf {
    home.join("person_credentials/store.json")
}

fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 128
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn read(home: &Path) -> Result<Store, String> {
    let file = path(home);
    match std::fs::metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
        Ok(meta) if meta.is_file() && meta.len() <= MAX_STORE_BYTES => {}
        _ => return Err(UNREADABLE.into()),
    }
    let bytes = std::fs::read(file).map_err(|_| UNREADABLE)?;
    let store: Store = serde_json::from_slice(&bytes).map_err(|_| UNREADABLE)?;
    let mut keys = HashSet::new();
    if store.version != VERSION
        || store.records.iter().any(|r| {
            let m = &r.metadata;
            !valid_metadata(m)
                || !r.secret.starts_with("pk:v1:")
                || !keys.insert(m.owner_id.clone())
        })
        || store
            .epochs
            .iter()
            .any(|(user, epoch)| !valid_user(user) || *epoch == 0)
    {
        return Err(UNREADABLE.into());
    }
    Ok(store)
}

fn valid_metadata(m: &Metadata) -> bool {
    m.owner_kind == "user"
        && m.provider == "google"
        && valid_user(&m.owner_id)
        && m.revision > 0
        && uuid::Uuid::parse_str(&m.credential_id).is_ok()
        && duduclaw_core::google_identity::normalize_email(&m.account_email)
            .is_ok_and(|email| email == m.account_email)
        && !m.google_sub.is_empty()
        && m.google_sub.len() <= 255
        && !m.google_sub.chars().any(char::is_control)
        && m.hosted_domain
            .as_deref()
            .is_none_or(duduclaw_core::google_identity::is_domain)
        && !m.oauth_client_id.is_empty()
        && m.oauth_client_id.len() <= 1024
        && !m.oauth_client_id.chars().any(char::is_control)
        && m.granted_scopes.len() <= 128
        && m.granted_scopes.iter().all(|scope| {
            !scope.is_empty() && scope.len() <= 2048 && !scope.chars().any(char::is_whitespace)
        })
        && chrono::DateTime::parse_from_rfc3339(&m.connected_at).is_ok()
        && m.last_refresh_at
            .as_deref()
            .is_none_or(|time| chrono::DateTime::parse_from_rfc3339(time).is_ok())
}

fn modify<T>(
    home: &Path,
    update: impl FnOnce(&mut Store) -> Result<T, String>,
) -> Result<T, String> {
    let file = path(home);
    let dir = file.parent().ok_or(UNREADABLE)?;
    std::fs::create_dir_all(dir).map_err(|_| UNREADABLE)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| UNREADABLE)?;
    }
    let mut answer = None;
    duduclaw_core::with_file_lock(&file, || {
        answer = Some((|| {
            let mut store = read(home)?;
            let out = update(&mut store)?;
            let bytes = serde_json::to_vec_pretty(&store).map_err(|_| UNREADABLE)?;
            if bytes.len() as u64 > MAX_STORE_BYTES {
                return Err("個人連線紀錄已達容量上限。".into());
            }
            duduclaw_agent::mcp_template::write_mcp_config_atomic(&file, &bytes)
                .map_err(|_| UNREADABLE)?;
            Ok(out)
        })());
        Ok(())
    })
    .map_err(|_| UNREADABLE)?;
    answer.ok_or(UNREADABLE.to_string())?
}

pub fn all_metadata(home: &Path) -> Result<Vec<Metadata>, String> {
    Ok(read(home)?
        .records
        .into_iter()
        .map(|r| r.metadata)
        .collect())
}

pub fn get(home: &Path, user: &str) -> Result<Option<Record>, String> {
    Ok(read(home)?
        .records
        .into_iter()
        .find(|r| r.metadata.owner_id == user))
}

/// Logical grant identity, excluding refresh-only token revision and sealed
/// payload. The epoch fences disconnect/reconnect, scope and status ABA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialAuthority {
    pub owner_id: String,
    pub credential_id: String,
    pub account_email: String,
    pub google_sub: String,
    pub hosted_domain: Option<String>,
    pub oauth_client_id: String,
    pub granted_scopes: Vec<String>,
    pub status: Status,
    pub epoch: u64,
}

impl CredentialAuthority {
    fn from_metadata(metadata: &Metadata, epoch: u64) -> Self {
        let mut scopes = metadata.granted_scopes.clone();
        scopes.sort();
        scopes.dedup();
        Self {
            owner_id: metadata.owner_id.clone(),
            credential_id: metadata.credential_id.clone(),
            account_email: metadata.account_email.clone(),
            google_sub: metadata.google_sub.clone(),
            hosted_domain: metadata.hosted_domain.clone(),
            oauth_client_id: metadata.oauth_client_id.clone(),
            granted_scopes: scopes,
            status: metadata.status,
            epoch,
        }
    }
}

/// Metadata and epoch from one atomic store read; never exposes the envelope.
pub struct AuthorityState {
    pub metadata: Option<Metadata>,
    pub epoch: u64,
}
impl AuthorityState {
    pub fn authority(&self) -> Option<CredentialAuthority> {
        self.metadata
            .as_ref()
            .map(|m| CredentialAuthority::from_metadata(m, self.epoch))
    }
}
pub fn authority_state(home: &Path, user: &str) -> Result<AuthorityState, String> {
    let store = read(home)?;
    let epoch = store.epochs.get(user).copied().unwrap_or(0);
    Ok(AuthorityState {
        metadata: store
            .records
            .into_iter()
            .find(|r| r.metadata.owner_id == user)
            .map(|r| r.metadata),
        epoch,
    })
}
pub fn authority_snapshot(home: &Path, user: &str) -> Result<Option<CredentialAuthority>, String> {
    Ok(authority_state(home, user)?.authority())
}

pub fn unseal(key: &ProtectedKey, record: &Record) -> Result<Secret, String> {
    let plain = key
        .unseal(&record.secret)
        .map_err(|_| "你的連線憑證目前無法解開，請管理員檢查憑證保護後重新連結。")?;
    let envelope: Envelope =
        serde_json::from_slice(&plain).map_err(|_| "你的連線憑證無法辨識，請重新連結。")?;
    if envelope.binding != Binding::from(&record.metadata) {
        return Err("連線紀錄與封存憑證不符，已停止使用，請重新連結。".into());
    }
    Ok(envelope.secret)
}

fn seal(key: &ProtectedKey, metadata: &Metadata, secret: &Secret) -> Result<String, String> {
    if !valid_metadata(metadata)
        || secret.access_token.is_empty()
        || secret.access_token.len() > 16 * 1024
        || secret
            .refresh_token
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 16 * 1024)
    {
        return Err("Google 傳回的憑證無法辨識，沒有保存。".into());
    }
    let value = EnvelopeRef {
        binding: Binding::from(metadata),
        secret,
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&value).map_err(|_| "無法封存連線憑證。")?);
    key.seal(&bytes)
        .map_err(|_| "無法封存連線憑證，沒有保存。".into())
}

fn next_epoch(store: &mut Store, user: &str) -> Result<u64, String> {
    if !valid_user(user) {
        return Err("帳號代號不正確。".into());
    }
    let epoch = store
        .epochs
        .get(user)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("連線版本已達上限。")?;
    store.epochs.insert(user.into(), epoch);
    Ok(epoch)
}

pub fn begin_connection(home: &Path, user: &str) -> Result<u64, String> {
    modify(home, |store| next_epoch(store, user))
}

pub fn connect(
    home: &Path,
    key: &ProtectedKey,
    metadata: Metadata,
    secret: &Secret,
    expected_epoch: u64,
) -> Result<(), String> {
    let sealed = seal(key, &metadata, secret)?;
    modify(home, |store| {
        if store.epochs.get(&metadata.owner_id).copied() != Some(expected_epoch) {
            return Err("這次連結已被較新的操作取代，請重新開始。".into());
        }
        // Publication itself is a grant mutation. Consume the pending epoch
        // even if a caller republishes the same visible account/id/scopes.
        next_epoch(store, &metadata.owner_id)?;
        store
            .records
            .retain(|r| r.metadata.owner_id != metadata.owner_id);
        store.records.push(Record {
            metadata,
            secret: sealed,
        });
        Ok(())
    })
}

pub fn disconnect(home: &Path, user: &str) -> Result<Option<Record>, String> {
    modify(home, |store| {
        next_epoch(store, user)?;
        Ok(store
            .records
            .iter()
            .position(|r| r.metadata.owner_id == user)
            .map(|i| store.records.remove(i)))
    })
}

fn matching(record: &Record, expected: &Metadata) -> bool {
    record.metadata.owner_id == expected.owner_id
        && record.metadata.credential_id == expected.credential_id
        && record.metadata.revision == expected.revision
}

pub fn finish_refresh(
    home: &Path,
    key: &ProtectedKey,
    expected: &Metadata,
    secret: &Secret,
) -> Result<bool, String> {
    finish_refresh_with_scopes(home, key, expected, secret, None)
}

pub fn finish_refresh_with_scopes(
    home: &Path,
    key: &ProtectedKey,
    expected: &Metadata,
    secret: &Secret,
    scopes: Option<&[String]>,
) -> Result<bool, String> {
    modify(home, |store| {
        let Some(index) = store.records.iter().position(|r| matching(r, expected)) else {
            return Ok(false);
        };
        let actual = &store.records[index].metadata;
        // The caller's expected value is a CAS assertion, never the source
        // of account/grant authority. Check it under the same store lock.
        if actual.status != Status::Connected
            || expected.status != actual.status
            || expected.status_before_suspension != actual.status_before_suspension
            || !valid_metadata(expected)
            || Binding::from(actual) != Binding::from(expected)
        {
            return Ok(false);
        }
        let mut before = actual.granted_scopes.clone();
        let mut metadata = actual.clone();
        metadata.revision = metadata
            .revision
            .checked_add(1)
            .ok_or("連線版本已達上限。")?;
        metadata.last_refresh_at = Some(chrono::Utc::now().to_rfc3339());
        if let Some(scopes) = scopes {
            metadata.granted_scopes = scopes.to_vec();
        }
        let sealed = seal(key, &metadata, secret)?;
        let mut after = metadata.granted_scopes.clone();
        before.sort();
        before.dedup();
        after.sort();
        after.dedup();
        if before != after {
            next_epoch(store, &metadata.owner_id)?;
        }
        let record = &mut store.records[index];
        record.metadata = metadata;
        record.secret = sealed;
        Ok(true)
    })
}

pub fn mark_needs_reconnect(home: &Path, expected: &Metadata) -> Result<bool, String> {
    modify(home, |store| {
        let Some(index) = store.records.iter().position(|r| matching(r, expected)) else {
            return Ok(false);
        };
        if store.records[index].metadata.status != Status::Connected {
            return Ok(false);
        }
        next_epoch(store, &expected.owner_id)?;
        let record = &mut store.records[index];
        record.metadata.status = Status::NeedsReconnect;
        record.metadata.revision = record
            .metadata
            .revision
            .checked_add(1)
            .ok_or("連線版本已達上限。")?;
        Ok(true)
    })
}

/// Explicit gateway metadata preparation, not credential access. A live
/// active Owner/consent/tool check must precede this operation. It changes
/// only status/revision/epoch; no key, token or envelope unseal or grant lock.
/// The existing sealed bytes are copied verbatim, never exposed or replaced.
/// Return the exact post-CAS metadata from the same vault lock; callers must
/// retain their pre-CAS directory/config generations and never reselect.
pub(crate) fn resume_status_if_current(
    home: &Path,
    expected: &Metadata,
    expected_epoch: u64,
    authorize: impl FnOnce() -> Result<(), String>,
) -> Result<Option<AuthorityState>, String> {
    modify(home, |store| {
        if store.epochs.get(&expected.owner_id).copied().unwrap_or(0) != expected_epoch {
            return Ok(None);
        }
        let Some(index) = store.records.iter().position(|r| matching(r, expected)) else {
            return Ok(None);
        };
        let actual = &store.records[index].metadata;
        if actual.status != Status::Suspended
            || !valid_metadata(expected)
            || expected.status != actual.status
            || actual.status_before_suspension != expected.status_before_suspension
            || Binding::from(actual) != Binding::from(expected)
        {
            return Ok(None);
        }
        // The callback may only recheck fixed directory/config facts. It
        // must not acquire a grant/protected-key lock or read this vault.
        // Run it under this lock immediately before metadata mutation.
        authorize()?;
        let revision = actual.revision.checked_add(1).ok_or("連線版本已達上限。")?;
        let epoch = next_epoch(store, &expected.owner_id)?;
        let metadata = &mut store.records[index].metadata;
        metadata.status = metadata
            .status_before_suspension
            .take()
            .filter(|s| *s != Status::Suspended)
            .unwrap_or(Status::NeedsReconnect);
        metadata.revision = revision;
        Ok(Some(AuthorityState {
            metadata: Some(metadata.clone()),
            epoch,
        }))
    })
}

/// A temporary suspension preserves both the sealed grant and whether it
/// already needed reconnection. Restoring an account never cures a revoked
/// Google grant. A missing connection is not created by a status check.
pub fn sync_user_status(home: &Path, user: &str, active: bool) -> Result<(), String> {
    let Some(current) = get(home, user)? else {
        return Ok(());
    };
    if (active && current.metadata.status != Status::Suspended)
        || (!active && current.metadata.status == Status::Suspended)
    {
        return Ok(());
    }
    modify(home, |store| {
        let Some(index) = store
            .records
            .iter()
            .position(|r| matching(r, &current.metadata))
        else {
            return Ok(());
        };
        next_epoch(store, user)?;
        let record = &mut store.records[index];
        if active && record.metadata.status == Status::Suspended {
            record.metadata.status = record
                .metadata
                .status_before_suspension
                .take()
                .filter(|s| *s != Status::Suspended)
                .unwrap_or(Status::NeedsReconnect);
        } else if !active && record.metadata.status != Status::Suspended {
            record.metadata.status_before_suspension = Some(record.metadata.status);
            record.metadata.status = Status::Suspended;
        }
        record.metadata.revision = record
            .metadata
            .revision
            .checked_add(1)
            .ok_or("連線版本已達上限。")?;
        Ok(())
    })
}

#[cfg(test)]
mod tests;
