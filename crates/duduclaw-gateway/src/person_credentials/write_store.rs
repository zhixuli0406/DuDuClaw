//! Gateway-only Drive-write vault. It never writes the ordinary store.json.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use duduclaw_security::credential_protection::ProtectedKey;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::google_personal::oauth::DRIVE_WRITE_SCOPE as DRIVE_SCOPE;
pub use super::{Secret, Status};

const PURPOSE: &str = "drive_write";
const UNREADABLE: &str = "雲端硬碟寫入連線紀錄無法讀取，請聯絡管理員；沒有變更已保存的連線。";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub owner_kind: String,
    pub owner_id: String,
    pub purpose: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_data_consent: Option<crate::google_data_consent::Evidence>,
}

// Status and revision change without the protected key during suspension.
// Every other clear field is authenticated, including the vault's purpose
// and owner namespace, so envelopes cannot be transplanted between vaults.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct Binding(Metadata);

impl From<&Metadata> for Binding {
    fn from(metadata: &Metadata) -> Self {
        let mut bound = metadata.clone();
        bound.revision = 1;
        bound.status = Status::Connected;
        bound.status_before_suspension = None;
        Self(bound)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    #[serde(rename = "write_binding")]
    binding: Binding,
    secret: Secret,
}

#[derive(Serialize)]
struct EnvelopeRef<'a> {
    #[serde(rename = "write_binding")]
    binding: Binding,
    secret: &'a Secret,
}

#[derive(Clone, Serialize)]
pub struct Record {
    #[serde(flatten)]
    pub metadata: Metadata,
    secret: String,
}

// Deserialize the secret separately so deny_unknown_fields on Metadata also
// applies to flattened records (serde's flatten/deny combination is unsupported).
impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let secret = fields.remove("secret")
            .ok_or_else(|| serde::de::Error::missing_field("secret"))?;
        let secret = serde_json::from_value(secret).map_err(serde::de::Error::custom)?;
        let metadata = serde_json::from_value(serde_json::Value::Object(fields))
            .map_err(serde::de::Error::custom)?;
        Ok(Self { metadata, secret })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    records: Vec<Record>,
    #[serde(default)]
    epochs: BTreeMap<String, u64>,
    #[serde(default)]
    ever_granted: BTreeSet<String>,
}

impl Default for Store {
    fn default() -> Self {
        Self { version: super::VERSION, records: Vec::new(), epochs: BTreeMap::new(), ever_granted: BTreeSet::new() }
    }
}

pub fn path(home: &Path) -> PathBuf {
    home.join("person_credentials/write_store.json")
}

fn valid_owner(kind: &str, id: &str) -> bool {
    match kind {
        "user" => super::valid_user(id),
        "home" => id == "home",
        _ => false,
    }
}

fn owner_key(kind: &str, id: &str) -> Result<String, String> {
    if !valid_owner(kind, id) {
        return Err("帳號代號不正確。".into());
    }
    Ok(format!("{kind}:{id}:{PURPOSE}"))
}

fn valid_metadata(m: &Metadata) -> bool {
    // Reuse the ordinary vault's bounds, identity and consent validation;
    // that vault's owner namespace remains user-only and is never persisted here.
    valid_owner(&m.owner_kind, &m.owner_id)
        && m.purpose == PURPOSE
        && m.granted_scopes.iter().any(|scope| scope == DRIVE_SCOPE)
        && super::valid_metadata(&super::Metadata {
            owner_kind: "user".into(),
            owner_id: m.owner_id.clone(),
            provider: m.provider.clone(),
            credential_id: m.credential_id.clone(),
            revision: m.revision,
            account_email: m.account_email.clone(),
            google_sub: m.google_sub.clone(),
            hosted_domain: m.hosted_domain.clone(),
            granted_scopes: m.granted_scopes.clone(),
            local_purposes: Vec::new(),
            status: m.status,
            status_before_suspension: m.status_before_suspension,
            connected_at: m.connected_at.clone(),
            last_refresh_at: m.last_refresh_at.clone(),
            oauth_client_id: m.oauth_client_id.clone(),
            google_data_consent: m.google_data_consent.clone(),
        })
}

fn read(home: &Path) -> Result<Store, String> {
    let store: Store = super::read_store_file(&path(home), UNREADABLE)?;
    let mut keys = HashSet::new();
    if store.version != super::VERSION
        || store.records.iter().any(|r| {
            let m = &r.metadata;
            !valid_metadata(m)
                || !r.secret.starts_with("pk:v1:")
                || !keys.insert((m.owner_kind.clone(), m.owner_id.clone(), m.purpose.clone()))
        })
        || store.epochs.iter().any(|(key, epoch)| {
            let parts: Vec<_> = key.split(':').collect();
            *epoch == 0 || !matches!(parts.as_slice(), [kind, id, purpose]
                if valid_owner(kind, id) && *purpose == PURPOSE)
        })
        || store.ever_granted.iter().any(|key| {
            let parts: Vec<_> = key.split(':').collect();
            !matches!(parts.as_slice(), [kind, id] if valid_owner(kind, id))
        })
    {
        return Err(UNREADABLE.into());
    }
    Ok(store)
}

fn modify<T>(home: &Path, update: impl FnOnce(&mut Store) -> Result<T, String>) -> Result<T, String> {
    super::modify_store_file(&path(home), UNREADABLE, || read(home), |store| {
        // Migrate legacy records under the same lock before any removal.
        for record in &store.records {
            store.ever_granted.insert(format!("{}:{}", record.metadata.owner_kind, record.metadata.owner_id));
        }
        update(store)
    })
}

pub fn ever_granted(home: &Path, owner_kind: &str, owner_id: &str) -> Result<bool, String> {
    owner_key(owner_kind, owner_id)?;
    let store = read(home)?;
    Ok(store.ever_granted.contains(&format!("{owner_kind}:{owner_id}"))
        || store.records.iter().any(|r| {
            r.metadata.owner_kind == owner_kind && r.metadata.owner_id == owner_id
        }))
}

pub fn get(home: &Path, owner_kind: &str, owner_id: &str) -> Result<Option<Record>, String> {
    owner_key(owner_kind, owner_id)?;
    Ok(read(home)?.records.into_iter().find(|r| {
        r.metadata.owner_kind == owner_kind && r.metadata.owner_id == owner_id
    }))
}

pub fn all_metadata(home: &Path) -> Result<Vec<Metadata>, String> {
    Ok(read(home)?.records.into_iter().map(|r| r.metadata).collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteCredentialSnapshot {
    pub owner_kind: String,
    pub owner_id: String,
    pub credential_id: String,
    pub revision: u64,
    pub account_email: String,
    pub google_sub: String,
    pub granted_scopes: Vec<String>,
    pub status: Status,
}

pub fn snapshot(home: &Path, owner_kind: &str, owner_id: &str) -> Result<Option<WriteCredentialSnapshot>, String> {
    Ok(get(home, owner_kind, owner_id)?.map(|r| {
        let m = r.metadata;
        WriteCredentialSnapshot {
            owner_kind: m.owner_kind, owner_id: m.owner_id,
            credential_id: m.credential_id, revision: m.revision,
            account_email: m.account_email, google_sub: m.google_sub,
            granted_scopes: m.granted_scopes, status: m.status,
        }
    }))
}

pub fn unseal(key: &ProtectedKey, record: &Record) -> Result<Secret, String> {
    if !valid_metadata(&record.metadata) {
        return Err(UNREADABLE.into());
    }
    let plain = key.unseal(&record.secret)
        .map_err(|_| "你的寫入連線憑證目前無法解開，請聯絡管理員後重新連結。")?;
    let envelope: Envelope = serde_json::from_slice(&plain)
        .map_err(|_| "你的寫入連線憑證無法辨識，請重新連結。")?;
    if envelope.binding != Binding::from(&record.metadata) {
        return Err("寫入連線紀錄與封存憑證不符，已停止使用，請重新連結。".into());
    }
    Ok(envelope.secret)
}

fn seal(key: &ProtectedKey, metadata: &Metadata, secret: &Secret) -> Result<String, String> {
    if !valid_metadata(metadata)
        || secret.access_token.is_empty()
        || secret.access_token.len() > 16 * 1024
        || secret.refresh_token.as_ref().is_some_and(|v| v.is_empty() || v.len() > 16 * 1024)
    {
        return Err("Google 傳回的寫入憑證或授權範圍不正確，沒有保存。".into());
    }
    let envelope = EnvelopeRef { binding: Binding::from(metadata), secret };
    let bytes = Zeroizing::new(serde_json::to_vec(&envelope).map_err(|_| "無法封存寫入連線憑證。")?);
    key.seal(&bytes).map_err(|_| "無法封存寫入連線憑證，沒有保存。".into())
}

fn next_epoch(store: &mut Store, kind: &str, id: &str) -> Result<u64, String> {
    let key = owner_key(kind, id)?;
    let epoch = store.epochs.get(&key).copied().unwrap_or(0)
        .checked_add(1).ok_or("連線版本已達上限。")?;
    store.epochs.insert(key, epoch);
    Ok(epoch)
}

/// Persist the ever-granted marker with the epoch before returning a write
/// authorization URL. Google may merge the grant even if finish never succeeds;
/// connect also retains the marker when publishing the credential.
pub fn begin_connection(home: &Path, owner_kind: &str, owner_id: &str) -> Result<u64, String> {
    modify(home, |store| {
        let epoch = next_epoch(store, owner_kind, owner_id)?;
        store.ever_granted.insert(format!("{owner_kind}:{owner_id}"));
        Ok(epoch)
    })
}

pub fn connect(
    home: &Path,
    key: &ProtectedKey,
    metadata: Metadata,
    secret: &Secret,
    expected_epoch: u64,
) -> Result<(), String> {
    let sealed = seal(key, &metadata, secret)?;
    let owner = owner_key(&metadata.owner_kind, &metadata.owner_id)?;
    modify(home, |store| {
        if store.epochs.get(&owner).copied() != Some(expected_epoch) {
            return Err("這次連結已被較新的操作取代，請重新開始。".into());
        }
        next_epoch(store, &metadata.owner_kind, &metadata.owner_id)?;
        store.records.retain(|r| {
            r.metadata.owner_kind != metadata.owner_kind || r.metadata.owner_id != metadata.owner_id
        });
        store.ever_granted.insert(format!("{}:{}", metadata.owner_kind, metadata.owner_id));
        store.records.push(Record { metadata, secret: sealed });
        Ok(())
    })
}

/// Local-only removal; never calls Google's revocation endpoint.
/// Keep grant tombstones through write/ordinary disconnect and suspension.
/// Offboarding may clear one only together with deletion of the ordinary
/// credential; this shared cleanup conservatively retains it even then.
pub fn disconnect(home: &Path, owner_kind: &str, owner_id: &str) -> Result<Option<Record>, String> {
    modify(home, |store| {
        next_epoch(store, owner_kind, owner_id)?;
        Ok(store.records.iter().position(|r| {
            r.metadata.owner_kind == owner_kind && r.metadata.owner_id == owner_id
        }).map(|i| store.records.remove(i)))
    })
}

fn matching(record: &Record, expected: &Metadata) -> bool {
    record.metadata == *expected
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
        if store.records[index].metadata.status != Status::Connected {
            return Ok(false);
        }
        let mut metadata = store.records[index].metadata.clone();
        metadata.revision = metadata.revision.checked_add(1).ok_or("連線版本已達上限。")?;
        metadata.last_refresh_at = Some(chrono::Utc::now().to_rfc3339());
        if let Some(scopes) = scopes {
            metadata.granted_scopes = scopes.to_vec();
        }
        let sealed = seal(key, &metadata, secret)?;
        let mut before = expected.granted_scopes.clone();
        let mut after = metadata.granted_scopes.clone();
        before.sort(); before.dedup();
        after.sort(); after.dedup();
        if before != after {
            next_epoch(store, &metadata.owner_kind, &metadata.owner_id)?;
        }
        store.records[index] = Record { metadata, secret: sealed };
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
        next_epoch(store, &expected.owner_kind, &expected.owner_id)?;
        let m = &mut store.records[index].metadata;
        m.status = Status::NeedsReconnect;
        m.revision = m.revision.checked_add(1).ok_or("連線版本已達上限。")?;
        Ok(true)
    })
}

pub(super) fn sync_user_status(home: &Path, user: &str, active: bool) -> Result<(), String> {
    // Avoid creating or rewriting the write vault for ordinary-only members.
    // Re-read under the write lock below before applying any actual change.
    let Some(record) = get(home, "user", user)? else { return Ok(()); };
    if (active && record.metadata.status != Status::Suspended)
        || (!active && record.metadata.status == Status::Suspended) {
        return Ok(());
    }
    modify(home, |store| {
        let Some(index) = store.records.iter().position(|r| {
            r.metadata.owner_kind == "user" && r.metadata.owner_id == user
        }) else {
            return Ok(());
        };
        let m = &store.records[index].metadata;
        if (active && m.status != Status::Suspended) || (!active && m.status == Status::Suspended) {
            return Ok(());
        }
        next_epoch(store, "user", user)?;
        let m = &mut store.records[index].metadata;
        if active {
            m.status = m.status_before_suspension.take()
                .filter(|s| *s != Status::Suspended).unwrap_or(Status::NeedsReconnect);
        } else {
            m.status_before_suspension = Some(m.status);
            m.status = Status::Suspended;
        }
        m.revision = m.revision.checked_add(1).ok_or("連線版本已達上限。")?;
        Ok(())
    })
}

#[cfg(test)]
#[path = "write_store_tests.rs"]
mod write_store_tests;
