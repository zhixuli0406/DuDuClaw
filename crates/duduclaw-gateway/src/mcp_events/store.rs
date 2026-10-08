//! MCP Events subscriptions: `<home>/mcp_events/subscriptions.json`.
//!
//! Same discipline as `remote_mcp/store.rs`: file 0600 (owner-only atomic
//! writer), directory 0700, every read-modify-write under
//! `duduclaw_core::with_file_lock`, a malformed file is an error (never
//! "empty"), and the signing secrets are one JSON blob encrypted with the
//! per-machine keyfile — a write fails rather than storing them in
//! plaintext.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STORE_DIR: &str = "mcp_events";
pub const STORE_FILE: &str = "subscriptions.json";
/// Prefix of a local subscription id.
pub const ID_PREFIX: &str = "mev_";
/// Most subscriptions per gateway.
pub const MAX_SUBSCRIPTIONS: usize = 200;
/// Most event names per subscription.
pub const MAX_EVENT_TYPES: usize = 16;

/// Lane the work an event starts runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventMode {
    /// Read-only explore lane (default).
    Explore,
    /// Ordinary run; the operator opted in.
    Normal,
}

impl EventMode {
    pub fn as_str(self) -> &'static str {
        match self {
            EventMode::Explore => "explore",
            EventMode::Normal => "normal",
        }
    }
}

/// How the upstream hands events over (the draft's per-event `delivery`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// The server POSTs to this gateway's callback URL (`events/subscribe`).
    #[default]
    Webhook,
    /// This gateway calls `events/poll` (single gateway, bounded).
    Poll,
}

impl DeliveryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryKind::Webhook => "webhook",
            DeliveryKind::Poll => "poll",
        }
    }
}

/// Most serialized bytes of one event's subscription `arguments`.
pub const MAX_ARGUMENT_BYTES: usize = 4 * 1024;
/// Event ids remembered per upstream subscription for de-duplication.
pub const RECENT_IDS: usize = 64;

/// The server's `deliveryStatus` from a refresh, reduced to fixed fields:
/// `lastError` is one of the draft's category strings or dropped.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeliveryStatus {
    pub active: bool,
    #[serde(default)]
    pub last_delivery_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub failed_since: Option<String>,
    #[serde(default)]
    pub throttled: bool,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

/// `lastError` / `-32015 data.reason` categories the draft defines.
pub const DELIVERY_ERROR_CATEGORIES: [&str; 6] =
    ["connection_refused", "timeout", "tls_error", "http_4xx", "http_5xx", "challenge_failed"];

/// Where a subscription stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubStatus {
    /// Created; the upstream has not answered every subscribe yet, or has
    /// not sent its verification challenge.
    Pending,
    /// Subscribed upstream; deliveries are accepted.
    Active,
    /// A subscribe or refresh failed; deliveries are still accepted until
    /// the upstream stops sending them.
    Failed,
    /// The upstream sent `terminated`; deliveries are refused (410).
    Terminated,
}

impl SubStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SubStatus::Pending => "pending",
            SubStatus::Active => "active",
            SubStatus::Failed => "failed",
            SubStatus::Terminated => "terminated",
        }
    }
}

/// One upstream `events/subscribe` (one event name).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UpstreamSub {
    pub name: String,
    /// The server-derived id (`X-MCP-Subscription-Id`), when known.
    #[serde(default)]
    pub upstream_id: Option<String>,
    /// RFC 3339; `None` = not subscribed yet or no expiry granted.
    #[serde(default)]
    pub refresh_before: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// The persisted watermark (opaque; `None` = the event type has no
    /// replay). Advanced after an event is recorded, by a refresh answer and
    /// by a `gap` envelope; sent back on refresh so the server resumes.
    #[serde(default)]
    pub cursor: Option<String>,
    /// The last answer said delivery started later than the cursor sent.
    #[serde(default)]
    pub truncated: bool,
    /// RFC 3339 of the last `gap` / `truncated` signal.
    #[serde(default)]
    pub gap_at: Option<String>,
    /// The cursor from before a gap, kept for one best-effort catch-up poll.
    #[serde(default)]
    pub gap_cursor: Option<String>,
    #[serde(default)]
    pub delivery_status: Option<DeliveryStatus>,
    /// RFC 3339 of the last poll (poll mode).
    #[serde(default)]
    pub last_polled_at: Option<String>,
    /// Newest event ids recorded, for de-duplication across replays.
    #[serde(default)]
    pub recent_ids: Vec<String>,
}

/// One local subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscription {
    pub id: String,
    pub agent_id: String,
    pub server: String,
    pub event_types: Vec<String>,
    pub mode: EventMode,
    #[serde(default)]
    pub delivery: DeliveryKind,
    /// Subscription `arguments` per event name (the draft's `inputSchema`
    /// values); a name without an entry subscribes with `{}`.
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
    pub status: SubStatus,
    #[serde(default)]
    pub upstream: Vec<UpstreamSub>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub rotated_at: Option<String>,
    #[serde(default)]
    pub last_delivery_at: Option<String>,
    #[serde(default)]
    pub deliveries: u64,
    /// [`SubSecrets`] as JSON, encrypted.
    pub secret_enc: String,
}

/// The encrypted half of a record.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SubSecrets {
    pub current: String,
    /// The secret before the last rotation, accepted until `previous_until`.
    #[serde(default)]
    pub previous: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub previous_until: Option<i64>,
}

impl std::fmt::Debug for SubSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubSecrets").field("current", &"«redacted»").finish()
    }
}

impl SubSecrets {
    /// Secrets a delivery may be signed with at `now`.
    pub fn accepted(&self, now: i64) -> Vec<&str> {
        let mut out = vec![self.current.as_str()];
        if let (Some(p), Some(until)) = (&self.previous, self.previous_until)
            && now <= until
        {
            out.push(p.as_str());
        }
        out
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    subscriptions: Vec<EventSubscription>,
}

fn store_version() -> u32 {
    1
}

pub fn store_path(home: &Path) -> PathBuf {
    home.join(STORE_DIR).join(STORE_FILE)
}

/// Is `id` shaped like a local subscription id (`mev_` + 32 lowercase hex)?
pub fn is_valid_id(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX)
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// A new random id.
pub fn new_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{ID_PREFIX}{}", hex::encode(b))
}

/// Validate one event's subscription arguments: a JSON object of at most
/// [`MAX_ARGUMENT_BYTES`] and nesting depth 6.
pub fn validate_arguments(v: &Value) -> Result<(), String> {
    fn depth(v: &Value) -> usize {
        match v {
            Value::Array(a) => 1 + a.iter().map(depth).max().unwrap_or(0),
            Value::Object(o) => 1 + o.values().map(depth).max().unwrap_or(0),
            _ => 0,
        }
    }
    if !v.is_object() {
        return Err("event arguments must be a JSON object".into());
    }
    if v.to_string().len() > MAX_ARGUMENT_BYTES {
        return Err(format!("event arguments are limited to {MAX_ARGUMENT_BYTES} bytes"));
    }
    if depth(v) > 6 {
        return Err("event arguments are nested too deeply".into());
    }
    Ok(())
}

/// Event names: 1–128 chars of `A-Za-z0-9._:/-`.
pub fn is_valid_event_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'-'))
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn read_file(home: &Path) -> Result<StoreFile, String> {
    let path = store_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid ({e}); fix or remove it", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn modify<T>(
    home: &Path,
    f: impl FnOnce(&mut Vec<EventSubscription>) -> Result<(bool, T), String>,
) -> Result<T, String> {
    let path = store_path(home);
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let mut out: Option<Result<T, String>> = None;
    duduclaw_core::with_file_lock(&path, || {
        let res = (|| {
            let mut file = read_file(home)?;
            let (changed, value) = f(&mut file.subscriptions)?;
            if changed {
                file.version = store_version();
                let json = serde_json::to_string_pretty(&file).map_err(|e| format!("serialize: {e}"))?;
                duduclaw_agent::mcp_template::write_mcp_config_atomic(&path, json.as_bytes())?;
            }
            Ok(value)
        })();
        out = Some(res);
        Ok(())
    })
    .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
    out.unwrap_or_else(|| Err("store update did not run".into()))
}

pub fn load_all(home: &Path) -> Result<Vec<EventSubscription>, String> {
    Ok(read_file(home)?.subscriptions)
}

pub fn get(home: &Path, id: &str) -> Result<Option<EventSubscription>, String> {
    Ok(load_all(home)?.into_iter().find(|s| s.id == id))
}

/// Insert a new record (refused past [`MAX_SUBSCRIPTIONS`]).
pub fn insert(home: &Path, rec: EventSubscription) -> Result<(), String> {
    modify(home, move |list| {
        if list.len() >= MAX_SUBSCRIPTIONS {
            return Err(format!("at most {MAX_SUBSCRIPTIONS} event subscriptions"));
        }
        list.push(rec);
        Ok((true, ()))
    })
}

pub fn remove(home: &Path, id: &str) -> Result<Option<EventSubscription>, String> {
    modify(home, |list| {
        let pos = list.iter().position(|s| s.id == id);
        Ok(match pos {
            Some(i) => (true, Some(list.remove(i))),
            None => (false, None),
        })
    })
}

/// Change one record under the lock. Missing ⇒ `Ok(false)`.
pub fn update(
    home: &Path,
    id: &str,
    f: impl FnOnce(&mut EventSubscription) -> Result<bool, String>,
) -> Result<bool, String> {
    modify(home, |list| match list.iter_mut().find(|s| s.id == id) {
        Some(rec) => {
            let changed = f(rec)?;
            if changed {
                rec.updated_at = now_rfc3339();
            }
            Ok((changed, changed))
        }
        None => Ok((false, false)),
    })
}

pub fn seal(home: &Path, secrets: &SubSecrets) -> Result<String, String> {
    let json = serde_json::to_string(secrets).map_err(|e| format!("serialize secrets: {e}"))?;
    crate::config_crypto::encrypt_value(&json, home)
        .ok_or_else(|| "cannot encrypt the signing secret (keyfile unavailable); nothing was stored".to_string())
}

pub fn open(home: &Path, rec: &EventSubscription) -> Result<SubSecrets, String> {
    let json = duduclaw_security::keyfile::decrypt_keyfile_value(&rec.secret_enc, home)
        .ok_or_else(|| format!("cannot decrypt the signing secret of {}", rec.id))?;
    serde_json::from_str(&json).map_err(|e| format!("stored secret is malformed: {e}"))
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names_are_validated() {
        let id = new_id();
        assert!(is_valid_id(&id));
        assert!(!is_valid_id("mev_XYZ"));
        assert!(!is_valid_id(&id.to_uppercase()));
        assert!(!is_valid_id(&format!("{id}/../x")));
        assert!(is_valid_event_name("incident.created"));
        assert!(!is_valid_event_name("a b"));
        assert!(!is_valid_event_name(""));
    }

    #[test]
    fn arguments_are_bounded_objects() {
        assert!(validate_arguments(&serde_json::json!({"a": 1})).is_ok());
        assert!(validate_arguments(&serde_json::json!([1])).is_err());
        assert!(validate_arguments(&serde_json::json!({"a": "x".repeat(5000)})).is_err());
        assert!(validate_arguments(&serde_json::json!({"a": {"b": {"c": {"d": {"e": {"f": {"g": 1}}}}}}})).is_err());
    }

    #[test]
    fn secrets_are_sealed_and_rotation_grace_applies() {
        let dir = tempfile::tempdir().unwrap();
        let s = SubSecrets { current: "whsec_a".into(), previous: Some("whsec_b".into()), previous_until: Some(100) };
        let enc = seal(dir.path(), &s).unwrap();
        assert!(!enc.contains("whsec_a"));
        let rec = EventSubscription {
            id: new_id(),
            agent_id: "a1".into(),
            server: "srv".into(),
            event_types: vec!["x".into()],
            mode: EventMode::Explore,
            delivery: DeliveryKind::Webhook,
            arguments: Default::default(),
            status: SubStatus::Pending,
            upstream: vec![],
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            rotated_at: None,
            last_delivery_at: None,
            deliveries: 0,
            secret_enc: enc,
        };
        insert(dir.path(), rec.clone()).unwrap();
        let back = get(dir.path(), &rec.id).unwrap().unwrap();
        let opened = open(dir.path(), &back).unwrap();
        assert_eq!(opened.accepted(100), vec!["whsec_a", "whsec_b"]);
        assert_eq!(opened.accepted(101), vec!["whsec_a"]);
        assert!(remove(dir.path(), &rec.id).unwrap().is_some());
        assert!(get(dir.path(), &rec.id).unwrap().is_none());
    }
}

/// Has an event with this id been recorded for `(subscription, name)`?
pub fn seen_event(rec: &EventSubscription, name: &str, event_id: &str) -> bool {
    rec.upstream.iter().any(|u| u.name == name && u.recent_ids.iter().any(|i| i == event_id))
}

/// Remember a recorded event: its id (bounded list), the new cursor when the
/// event carried one, the delivery counters. Missing subscription ⇒ `false`.
pub fn note_event(home: &Path, id: &str, name: &str, event_id: &str, cursor: Option<&str>) -> Result<bool, String> {
    update(home, id, |r| {
        if let Some(u) = r.upstream.iter_mut().find(|u| u.name == name) {
            if !u.recent_ids.iter().any(|i| i == event_id) {
                u.recent_ids.push(event_id.to_string());
                let excess = u.recent_ids.len().saturating_sub(RECENT_IDS);
                if excess > 0 {
                    u.recent_ids.drain(..excess);
                }
            }
            if let Some(c) = cursor {
                u.cursor = Some(c.to_string());
            }
        }
        r.deliveries = r.deliveries.saturating_add(1);
        r.last_delivery_at = Some(now_rfc3339());
        Ok(true)
    })
}
