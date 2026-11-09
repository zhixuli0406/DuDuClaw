//! Google tenant credential sources for protected mode (native Linux
//! isolation v1.81, task E4).
//!
//! A tenant source is existing, admin-configured product configuration; this
//! module adds no secret store:
//!
//! - **Delegate** (domain-wide delegation): the sealed delegation key
//!   `<home>/protected/google-service-account.sealed` (imported by an Admin
//!   through `google.delegation.import_key`) plus the employee's admin-written
//!   `agent.toml [google] identity = "delegate:<subject>"`.
//! - **ServiceAccount**: `config.toml [integrations.google_service_account]`
//!   (`key_file` + `subject`), used when the employee's identity is `home`.
//!
//! Both are served only through the employee's own SharedTenant Google route
//! in the C1 catalog (provisioned by C3 with admin step-up), so only Shared
//! employees of the route's tenant can reach them.
//!
//! **Subject rule (Decision D-5).** The subject a tenant-source token is
//! minted for comes only from the admin configuration above, never from
//! employee arguments, the run environment or run metadata. It is admitted
//! only when, comparing normalized emails by exact equality against users.db:
//! - it is listed in `[integrations.google_identity] service_mailboxes` and
//!   matches NO users.db account (a person's mailbox is never a service
//!   mailbox): a Shared action, for any requester the Shared employee's
//!   authority admitted; or
//! - (Delegate only) it matches exactly ONE users.db account, that account is
//!   Active, and the authenticated requester is that account (exact user id):
//!   a Personal-like action, on behalf of the requesting human only.
//!
//! Anything else (an undesignated non-person subject, a person mailbox that
//! is not the requester, an ambiguous email, a System requester for a person
//! mailbox, a service account acting as a person) is refused. Tools are
//! further limited to `[integrations.google_identity] tenant_scopes` (each
//! entry must be one of the minted scopes; empty serves no data tool).
//!
//! Every read fails closed: unreadable or malformed config or key material is
//! `Unavailable`; a configuration that would let a failure fall back to a
//! home source (`fallback = "home"`, an Apps Script bridge beside the
//! service account) is `Refused`.
use std::{io::Read as _, path::Path};

use duduclaw_auth::UserDb;
use duduclaw_core::google_identity::{self as gi, Effective, Fallback};
use duduclaw_mcp_boundary::VerificationError;
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Largest sealed delegation record read here (the importer caps the key at 16 KiB).
const MAX_SEALED_RECORD: u64 = 64 * 1024;
/// Largest service-account key file read here (same bound as the SA owner).
const MAX_KEY_FILE: u64 = 16 * 1024;
/// Largest `config.toml` read here (same bound as the W3 source observer).
const MAX_CONFIG: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantSourceKind {
    /// Domain-wide delegation through the sealed delegation key.
    Delegate,
    /// `[integrations.google_service_account]`; never acts as a person.
    ServiceAccount,
}
impl TenantSourceKind {
    fn as_str(self) -> &'static str {
        match self { Self::Delegate => "delegate", Self::ServiceAccount => "service_account" }
    }
}

/// Non-sensitive refusals. Every variant refuses the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TenantSourceError {
    /// Config or key material cannot be read or is malformed.
    Unavailable,
    /// The configuration is readable but not admissible in protected mode.
    Refused,
}
impl From<TenantSourceError> for VerificationError {
    fn from(error: TenantSourceError) -> Self {
        match error {
            TenantSourceError::Unavailable => Self::Unavailable,
            TenantSourceError::Refused => Self::BindingMismatch,
        }
    }
}

/// One configured tenant source as of one read. Holds no secret: key
/// material is represented only by a domain-separated digest of its bytes.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TenantSource {
    kind: TenantSourceKind,
    subject: String,
    service_mailboxes: Vec<String>,
    scopes: Vec<String>,
    material: String,
}
impl std::fmt::Debug for TenantSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantSource").field("kind", &self.kind).finish_non_exhaustive()
    }
}
impl TenantSource {
    pub(crate) fn kind(&self) -> TenantSourceKind { self.kind }
    pub(crate) fn subject(&self) -> &str { &self.subject }
    /// `google_status` is metadata only; every data tool needs a configured scope.
    pub(crate) fn serves(&self, tool: &str) -> bool {
        tool == "google_status" || crate::google_personal::scopes::permitted(tool, &self.scopes)
    }
    /// The source proof the bridge pins per tool. Changes to the subject,
    /// mailbox designation, scopes or key material all change it.
    pub(crate) fn digest(&self) -> String {
        gi::google_arguments_digest(&json!({"tenant_source": self.kind.as_str(), "subject": self.subject,
            "service_mailboxes": self.service_mailboxes, "scopes": self.scopes, "material": self.material}))
    }
    #[cfg(test)]
    pub(crate) fn fixture(kind: TenantSourceKind, subject: &str, service_mailboxes: &[&str], scopes: &[&str]) -> Self {
        Self { kind, subject: subject.into(), service_mailboxes: service_mailboxes.iter().map(|s| (*s).into()).collect(),
            scopes: scopes.iter().map(|s| (*s).into()).collect(), material: "fixture".into() }
    }
}

/// Who asks, as established by the bridge from the capability principal and
/// the host's live run association only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Actor<'a> {
    /// An authenticated person (users.db id).
    Human(&'a str),
    /// Background / scheduled / delegated work; never a person.
    System,
}

/// A users.db account whose normalized email equals the subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubjectAccount {
    pub(crate) user_id: String,
    pub(crate) active: bool,
}

/// Which branch of the subject rule admitted the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubjectUse {
    ServiceMailbox,
    RequestingUser,
}

/// Every users.db account whose email, normalized with the same normalizer as
/// the configured subject, is exactly `subject`. Unreadable ⇒ `Unavailable`.
pub(crate) fn subject_accounts(db: &UserDb, subject: &str) -> Result<Vec<SubjectAccount>, VerificationError> {
    let users = db.list_users().map_err(|_| VerificationError::Unavailable)?;
    Ok(users.into_iter()
        .filter(|user| gi::normalize_email(&user.email).is_ok_and(|email| email == subject))
        .map(|user| SubjectAccount { active: user.status == duduclaw_auth::models::UserStatus::Active, user_id: user.id })
        .collect())
}

/// The subject rule (D-5). Pure: callers supply the users.db facts.
pub(crate) fn admit_subject(source: &TenantSource, actor: Actor<'_>, accounts: &[SubjectAccount])
    -> Result<SubjectUse, VerificationError> {
    let designated = source.service_mailboxes.iter().any(|mailbox| mailbox == &source.subject);
    match (designated, accounts) {
        (true, []) => Ok(SubjectUse::ServiceMailbox),
        // A person's account can never be designated as a service mailbox.
        (true, _) => Err(VerificationError::BindingMismatch),
        (false, [account]) if source.kind == TenantSourceKind::Delegate => match actor {
            Actor::Human(user_id) if account.active && account.user_id == user_id => Ok(SubjectUse::RequestingUser),
            _ => Err(VerificationError::BindingMismatch),
        },
        _ => Err(VerificationError::BindingMismatch),
    }
}

/// The tenant source configured for `agent` now, if any. Blocking file
/// reads; call on the same blocking boundary as the other live checks.
/// `Ok(None)`: the employee's Google identity is not a tenant source (home
/// OAuth, Personal, none).
pub(crate) fn read(home: &Path, agent: &str) -> Result<Option<TenantSource>, TenantSourceError> {
    let home_config = gi::read_home_config(home);
    let effective = gi::combine(&gi::read_agent_section(home, agent), &home_config);
    let config = home_config.map_err(|_| TenantSourceError::Unavailable)?;
    let configured_scopes = || -> Result<Vec<String>, TenantSourceError> {
        // Only scopes the tenant mint actually requests; anything else is a
        // configuration error, never silently widened or dropped.
        if config.tenant_scopes.iter().any(|scope| !crate::google_workspace::REQUIRED_SCOPES.contains(&scope.as_str())) {
            return Err(TenantSourceError::Unavailable);
        }
        Ok(config.tenant_scopes.clone())
    };
    match &effective.effective {
        Effective::Invalid(_) => Err(TenantSourceError::Unavailable),
        Effective::Delegate { email } => {
            // A delegate failure must never be served from the home account.
            if effective.fallback != Fallback::None { return Err(TenantSourceError::Refused); }
            let subject = gi::normalize_email(email).map_err(|_| TenantSourceError::Unavailable)?;
            if &subject != email { return Err(TenantSourceError::Unavailable); }
            if !gi::delegate_domain_allowed(&subject, &config, super::home_subject(home).as_deref()) {
                return Err(TenantSourceError::Refused);
            }
            let bytes = read_bounded(&super::delegation_key::path(home), MAX_SEALED_RECORD)?;
            // The record must be the importer's (public part parses, sealed
            // under a protected key id); unsealing stays with the host owner.
            match super::delegation_key::info(home) {
                Ok(Some(_)) => {}
                _ => return Err(TenantSourceError::Unavailable),
            }
            Ok(Some(TenantSource { kind: TenantSourceKind::Delegate, subject,
                service_mailboxes: config.service_mailboxes.clone(), scopes: configured_scopes()?,
                material: material_digest("delegate", &bytes) }))
        }
        Effective::Home => {
            let config_path = home.join("config.toml");
            let raw = match std::fs::symlink_metadata(&config_path) {
                // No config.toml: no service account (home OAuth path).
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(TenantSourceError::Unavailable),
                Ok(_) => read_bounded_or_empty(&config_path, MAX_CONFIG)?,
            };
            let raw = std::str::from_utf8(&raw).map_err(|_| TenantSourceError::Unavailable)?;
            let service_account = match crate::google_service_account::parse_config(raw, home) {
                Ok(None) => return Ok(None),
                Ok(Some(service_account)) => service_account,
                Err(_) => return Err(TenantSourceError::Unavailable),
            };
            // A service-account failure falls back to the Apps Script bridge,
            // a home source: never admissible for a tenant route.
            match crate::google_apps_script::parse_bridge_section(raw) {
                Ok(None) => {}
                _ => return Err(TenantSourceError::Refused),
            }
            let subject = gi::normalize_email(&service_account.subject).map_err(|_| TenantSourceError::Unavailable)?;
            let key = read_bounded(&service_account.key_file, MAX_KEY_FILE)?;
            let text = std::str::from_utf8(&key).map_err(|_| TenantSourceError::Unavailable)?;
            crate::google_service_account::parse_key(text).map_err(|_| TenantSourceError::Unavailable)?;
            Ok(Some(TenantSource { kind: TenantSourceKind::ServiceAccount, subject,
                service_mailboxes: config.service_mailboxes.clone(), scopes: configured_scopes()?,
                material: material_digest("service_account", &key) }))
        }
        Effective::Personal { .. } | Effective::Disabled => Ok(None),
    }
}

fn material_digest(kind: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"duduclaw.e4.google-tenant-source.v1\0");
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// A regular, non-empty file of at most `max` bytes. Anything else
/// (missing, a directory, unreadable, oversized, empty) is `Unavailable`.
fn read_bounded(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>, TenantSourceError> {
    let bytes = read_bounded_or_empty(path, max)?;
    if bytes.is_empty() { return Err(TenantSourceError::Unavailable); }
    Ok(bytes)
}
fn read_bounded_or_empty(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>, TenantSourceError> {
    let unavailable = |_| TenantSourceError::Unavailable;
    let file = std::fs::File::open(path).map_err(unavailable)?;
    let meta = file.metadata().map_err(unavailable)?;
    if !meta.is_file() || meta.len() > max { return Err(TenantSourceError::Unavailable); }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max + 1).read_to_end(&mut bytes).map_err(unavailable)?;
    if bytes.len() as u64 > max { return Err(TenantSourceError::Unavailable); }
    Ok(bytes)
}

// ── F5 UI-RPC: dashboard view of the tenant sources ───────────────────────

/// Domain tag of the dashboard key fingerprint. Distinct from
/// [`material_digest`], so the value shown on the dashboard is never the
/// bridge's pinned source proof.
const FINGERPRINT_DOMAIN: &[u8] = b"duduclaw.f5.google-tenant-key-fingerprint.v1\0";

/// Length-prefixed, domain-separated SHA-256 (hex) of non-secret parts.
fn fingerprint(kind: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

/// The non-secret identity fields of a service-account key file. Serde skips
/// every other field (the private key is never kept in this value).
#[derive(serde::Deserialize)]
struct KeyIdentity {
    client_email: String,
    #[serde(default)]
    private_key_id: Option<String>,
}

/// F5 (UI-RPC): what the dashboard's Google tenant-source tab shows. Holds no
/// secret — the state, identifiers the dashboard already shows elsewhere
/// (service-account email, the SA `subject`) and a key fingerprint:
/// - `delegate` (domain-wide delegation): SHA-256 over the sealed record as
///   stored (ciphertext + public fields); a new key import changes it.
/// - `service_account`: SHA-256 over the key file's `client_email` and its
///   Google key id (`private_key_id`, the id the Google console lists);
///   `null` when the file carries no well-formed key id.
///
/// Each entry is `{"state": "not_configured" | "installed" | "unavailable"}`
/// plus fields when installed. Unreadable or malformed material is
/// `unavailable`, never a guessed fingerprint. Blocking file reads.
pub(crate) fn dashboard_view(home: &Path) -> serde_json::Value {
    json!({ "delegate": delegate_view(home), "service_account": service_account_view(home) })
}

fn delegate_view(home: &Path) -> serde_json::Value {
    let unavailable = json!({ "state": "unavailable" });
    let sealed = super::delegation_key::path(home);
    match std::fs::symlink_metadata(&sealed) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return json!({ "state": "not_configured" }),
        Err(_) => return unavailable,
        Ok(_) => {}
    }
    let Ok(bytes) = read_bounded(&sealed, MAX_SEALED_RECORD) else { return unavailable };
    match super::delegation_key::info(home) {
        Ok(Some(info)) => json!({ "state": "installed", "client_email": info.client_email,
            "fingerprint": fingerprint("delegate", &[bytes.as_slice()]) }),
        _ => unavailable,
    }
}

fn service_account_view(home: &Path) -> serde_json::Value {
    let unavailable = json!({ "state": "unavailable" });
    let config_path = home.join("config.toml");
    let raw = match std::fs::symlink_metadata(&config_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return json!({ "state": "not_configured" }),
        Err(_) => return unavailable,
        Ok(_) => match read_bounded_or_empty(&config_path, MAX_CONFIG) { Ok(raw) => raw, Err(_) => return unavailable },
    };
    let Ok(raw) = std::str::from_utf8(&raw) else { return unavailable };
    // `parse_config` reads unparsable TOML as "no section"; the view must not.
    if raw.parse::<toml::Table>().is_err() { return unavailable; }
    let service_account = match crate::google_service_account::parse_config(raw, home) {
        Ok(None) => return json!({ "state": "not_configured" }),
        Ok(Some(service_account)) => service_account,
        Err(_) => return unavailable,
    };
    let subject = gi::normalize_email(&service_account.subject).ok();
    let Ok(key) = read_bounded(&service_account.key_file, MAX_KEY_FILE) else {
        return json!({ "state": "unavailable", "subject": subject });
    };
    let Ok(text) = std::str::from_utf8(&key) else { return json!({ "state": "unavailable", "subject": subject }) };
    if crate::google_service_account::parse_key(text).is_err() {
        return json!({ "state": "unavailable", "subject": subject });
    }
    let Ok(identity) = serde_json::from_str::<KeyIdentity>(text) else {
        return json!({ "state": "unavailable", "subject": subject });
    };
    let key_id = identity.private_key_id.filter(|id| !id.is_empty() && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric()));
    let key_fingerprint = key_id.map(|id| fingerprint("service_account", &[identity.client_email.as_bytes(), id.as_bytes()]));
    json!({
        "state": "installed",
        "client_email": identity.client_email,
        "subject": subject,
        "fingerprint": key_fingerprint,
    })
}

/// Whether any tenant source material is installed in this home at all
/// (C3 provision gate for Google SharedTenant routes). The per-employee
/// subject is decided at use time by [`read`].
pub(crate) fn installed(home: &Path) -> Result<bool, TenantSourceError> {
    let sealed = super::delegation_key::path(home);
    let delegate = match std::fs::symlink_metadata(&sealed) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(TenantSourceError::Unavailable),
        Ok(_) => {
            read_bounded(&sealed, MAX_SEALED_RECORD)?;
            match super::delegation_key::info(home) {
                Ok(Some(_)) => true,
                _ => return Err(TenantSourceError::Unavailable),
            }
        }
    };
    if delegate { return Ok(true); }
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(TenantSourceError::Unavailable),
    };
    match crate::google_service_account::parse_config(&raw, home) {
        Ok(None) => Ok(false),
        Ok(Some(service_account)) => {
            read_bounded(&service_account.key_file, MAX_KEY_FILE)?;
            Ok(true)
        }
        Err(_) => Err(TenantSourceError::Unavailable),
    }
}

#[cfg(test)]
#[path = "tenant_source_tests.rs"]
mod tests;
