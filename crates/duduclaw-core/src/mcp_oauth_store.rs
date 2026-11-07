//! Shared remote MCP OAuth grant store. Every mutation locks the whole
//! read/modify/write transaction so dashboard revocation cannot be overwritten.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const STORE_FILE: &str = "mcp_oauth_issued.json";
pub const CODE_TTL_SECS: i64 = 600;
pub const MAX_STORE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TOKEN_ROWS: usize = 16_384;
pub const MAX_FAMILY_ROWS: usize = 2_048;
pub const MAX_GRANTS: usize = 1_024;
const MAX_CODES: usize = 1_024;
pub const MAX_CLIENTS: usize = 200;
const ANONYMOUS_CLIENT_GRACE_SECS: i64 = 3600;
const UNUSED_CLIENT_TTL_SECS: i64 = 24 * 3600;
const INACTIVE_CLIENT_TTL_SECS: i64 = 90 * 24 * 3600;

pub fn digest(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredClient {
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub created_at: String,
    #[serde(default)]
    pub last_used_unix: Option<i64>,
}

impl RegisteredClient {
    /// Only successful operator authorization or token use extends a client's life.
    /// Registration and rejected authorization attempts cannot renew the TTL.
    pub fn active_at(&self, now: i64) -> bool {
        let Ok(created) = chrono::DateTime::parse_from_rfc3339(&self.created_at) else { return false; };
        let (last, ttl) = match self.last_used_unix {
            Some(last) => (last, INACTIVE_CLIENT_TTL_SECS),
            None => (created.timestamp(), UNUSED_CLIENT_TTL_SECS),
        };
        now.saturating_sub(last) < ttl
    }
}

/// Bind consent to one exact internal key and its metadata, never its secret.
/// Metadata edits (including scope edits) require fresh consent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Consent {
    pub key_hash: String,
    pub entry_hash: String,
}

impl Consent {
    pub fn capture(home: &Path, raw: &str, scope: &str) -> Option<Self> {
        let keys = key_registry(home).ok()?;
        let entry = keys.get(raw)?;
        if !valid_key_entry(raw, entry, scope) { return None; }
        Some(Self { key_hash: digest(raw), entry_hash: digest(&entry.to_string()) })
    }

    pub fn valid(&self, home: &Path, scope: &str) -> bool {
        let Ok(keys) = key_registry(home) else { return false; };
        self.valid_with_registry(&keys, scope)
    }

    fn valid_with_registry(&self, keys: &toml::Table, scope: &str) -> bool {
        keys.iter().any(|(raw, entry)| {
            digest(raw) == self.key_hash && digest(&entry.to_string()) == self.entry_hash
                && valid_key_entry(raw, entry, scope)
        })
    }
}

fn key_registry(home: &Path) -> io::Result<toml::Table> {
    let raw = std::fs::read_to_string(home.join("config.toml"))?;
    let config: toml::Table = toml::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    match config.get("mcp_keys") {
        None => Ok(toml::Table::new()),
        Some(toml::Value::Table(keys)) => Ok(keys.clone()),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid MCP key registry")),
    }
}

fn valid_key_entry(raw: &str, entry: &toml::Value, scope: &str) -> bool {
    let parts: Vec<_> = raw.split('_').collect();
    if parts.len() != 3 || parts[0] != "ddc" || !matches!(parts[1], "prod" | "staging" | "dev")
        || parts[2].len() != 32 || !parts[2].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return false;
    }
    if entry.get("client_id").and_then(|v| v.as_str()).is_none() { return false; }
    match entry.get("is_external") {
        None | Some(toml::Value::Boolean(false)) => {}
        _ => return false,
    }
    let Some(created) = entry.get("created_at").and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else { return false; };
    if chrono::Utc::now().signed_duration_since(created).num_days() > 30 { return false; }
    let Some(scopes) = entry.get("scopes").and_then(|v| v.as_array()) else { return false; };
    let Some(scopes) = scopes.iter().map(|v| v.as_str()).collect::<Option<Vec<_>>>() else { return false; };
    if scopes.iter().any(|s| !crate::mcp_scopes::MCP_SCOPE_STRINGS.contains(s)) { return false; }
    scope.split_whitespace().all(|s| scopes.contains(&"admin") || scopes.contains(&s))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCode {
    #[serde(default)]
    pub resource: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scope: String,
    pub issued_unix: i64,
    #[serde(default)]
    pub client_name: String,
    #[serde(default)]
    pub consent: Option<Consent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedToken {
    #[serde(default)]
    pub resource: String,
    pub client_id: String,
    pub client_name: String,
    pub scope: String,
    pub expires_unix: i64,
    #[serde(default)]
    pub family_id: String,
    #[serde(default)]
    pub used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    #[serde(default)]
    pub resource: String,
    pub client_id: String,
    pub client_name: String,
    pub scope: String,
    pub issued_unix: i64,
    pub last_used_unix: Option<i64>,
    pub consent: Consent,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthStore {
    #[serde(default)]
    pub clients: HashMap<String, RegisteredClient>,
    #[serde(default)]
    pub codes: HashMap<String, PendingCode>,
    #[serde(default)]
    pub tokens: HashMap<String, IssuedToken>,
    #[serde(default)]
    pub refresh: HashMap<String, IssuedToken>,
    #[serde(default)]
    pub grants: HashMap<String, Grant>,
}

impl OAuthStore {
    pub fn revoke_family(&mut self, family: &str) -> bool {
        // Empty family IDs belong to pre-lineage tokens; they are never valid.
        let removed = self.grants.remove(family).is_some();
        self.tokens.retain(|_, t| t.family_id != family);
        self.refresh.retain(|_, t| t.family_id != family);
        removed
    }

    pub fn valid_token(&self, token: &IssuedToken, home: &Path, resource: &str) -> bool {
        let Some(grant) = self.grants.get(&token.family_id) else { return false; };
        !resource.is_empty() && token.resource == resource && grant.resource == resource
            && token.expires_unix > chrono::Utc::now().timestamp() && !token.used
            && token.client_id == grant.client_id && scope_subset(&token.scope, &grant.scope)
            && grant.consent.valid(home, &grant.scope)
    }

    pub fn active_grants(&self, home: &Path) -> io::Result<Vec<serde_json::Value>> {
        let keys = key_registry(home)?;
        let now = chrono::Utc::now().timestamp();
        let live: HashSet<_> = self.tokens.values().chain(self.refresh.values())
            .filter(|t| !t.used && t.expires_unix > now).map(|t| t.family_id.as_str()).collect();
        let mut grants: Vec<_> = self.grants.iter().filter(|(id, g)| {
            live.contains(id.as_str()) && g.consent.valid_with_registry(&keys, &g.scope)
        }).map(|(id, g)| serde_json::json!({
            "id": id, "client_name": g.client_name, "scopes": g.scope.split_whitespace().collect::<Vec<_>>(),
            "issued_unix": g.issued_unix, "last_used_unix": g.last_used_unix,
        })).collect();
        grants.sort_by_key(|g| (g["issued_unix"].as_i64(), g["id"].as_str().unwrap_or("").to_owned()));
        grants.reverse();
        Ok(grants)
    }

    /// Spent refresh rows count too: never evict replay evidence to mint more.
    pub fn can_mint(&self, family: Option<&str>) -> bool {
        let rows = self.tokens.values().chain(self.refresh.values());
        self.tokens.len() + self.refresh.len() + 2 <= MAX_TOKEN_ROWS
            && match family {
                Some(id) => rows.filter(|t| t.family_id == id).count() + 2 <= MAX_FAMILY_ROWS,
                None => self.grants.len() < MAX_GRANTS,
            }
    }

    fn check_limits(&self) -> io::Result<()> {
        let mut families = HashMap::new();
        for token in self.tokens.values().chain(self.refresh.values()) {
            let count = families.entry(&token.family_id).or_insert(0usize);
            *count += 1;
            if *count > MAX_FAMILY_ROWS { return Err(capacity_error()); }
        }
        if self.tokens.len() + self.refresh.len() > MAX_TOKEN_ROWS
            || self.grants.len() > MAX_GRANTS || self.codes.len() > MAX_CODES
            || self.clients.len() > MAX_CLIENTS { return Err(capacity_error()); }
        Ok(())
    }

    /// At capacity, reclaim only the oldest never-authorized registration
    /// beyond its grace period. Call under update_store's transaction lock.
    pub fn registration_eviction_candidate(&self, now: i64) -> Option<String> {
        let referenced: HashSet<_> = self.codes.values().filter(|c| now - c.issued_unix <= CODE_TTL_SECS)
            .map(|c| c.client_id.as_str())
            .chain(self.grants.values().map(|g| g.client_id.as_str()))
            .chain(self.tokens.values().chain(self.refresh.values())
                .filter(|t| t.expires_unix > now).map(|t| t.client_id.as_str())).collect();
        self.clients.iter().filter_map(|(id, client)| {
            if client.last_used_unix.is_some() || referenced.contains(id.as_str()) { return None; }
            let created = chrono::DateTime::parse_from_rfc3339(&client.created_at).ok()?.timestamp();
            (now.saturating_sub(created) >= ANONYMOUS_CLIENT_GRACE_SECS).then_some((created, id))
        }).min().map(|(_, id)| id.clone())
    }

    fn prune(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.codes.retain(|_, c| now - c.issued_unix <= CODE_TTL_SECS);
        self.tokens.retain(|_, t| t.expires_unix > now);
        self.refresh.retain(|_, t| t.expires_unix > now);
        let live: HashSet<_> = self.tokens.values().chain(self.refresh.values())
            .map(|t| t.family_id.as_str()).collect();
        self.grants.retain(|id, _| live.contains(id.as_str()));
        // Preserve clients referenced by a live code or grant. Hydrate older
        // registrations from existing grants before applying inactivity expiry.
        for grant in self.grants.values() {
            if let Some(client) = self.clients.get_mut(&grant.client_id) {
                let last = grant.last_used_unix.unwrap_or(grant.issued_unix);
                client.last_used_unix = Some(client.last_used_unix.map_or(last, |old| old.max(last)));
            }
        }
        let referenced: HashSet<_> = self.codes.values().map(|c| c.client_id.as_str())
            .chain(self.grants.values().map(|g| g.client_id.as_str()))
            .chain(self.tokens.values().chain(self.refresh.values()).map(|t| t.client_id.as_str())).collect();
        self.clients.retain(|id, client| referenced.contains(id.as_str()) || client.active_at(now));
    }
}

pub fn scope_subset(scope: &str, ceiling: &str) -> bool {
    scope.split_whitespace().all(|s| ceiling.split_whitespace().any(|c| c == s))
}

pub fn store_path(home: &Path) -> PathBuf { home.join(STORE_FILE) }

fn capacity_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "OAuth store capacity exceeded")
}

// Bound both reads and serialization, including stores created by older versions.
fn encode_store(store: &OAuthStore) -> io::Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_STORE_BYTES { return Err(capacity_error()); }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut writer = Bounded(Vec::new());
    serde_json::to_writer_pretty(&mut writer, store)?;
    Ok(writer.0)
}

pub fn load_store(home: &Path) -> io::Result<OAuthStore> {
    use io::Read;
    match std::fs::File::open(store_path(home)) {
        Ok(file) => {
            let mut raw = Vec::new();
            file.take(MAX_STORE_BYTES as u64 + 1).read_to_end(&mut raw)?;
            if raw.len() > MAX_STORE_BYTES { return Err(capacity_error()); }
            serde_json::from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(OAuthStore::default()),
        Err(e) => Err(e),
    }
}

/// The closure may return an OAuth error response after removing invalid
/// material; that removal must still commit. I/O failure never returns success.
pub fn update_store<T>(home: &Path, update: impl FnOnce(&mut OAuthStore) -> T) -> io::Result<T> {
    let path = store_path(home);
    crate::with_file_lock(&path, || {
        let mut store = load_store(home)?;
        let before = encode_store(&store)?;
        let before_rows = store.clients.len() + store.codes.len() + store.grants.len() + store.tokens.len() + store.refresh.len();
        store.prune();
        let value = update(&mut store);
        store.prune();
        let json = encode_store(&store)?;
        let after_rows = store.clients.len() + store.codes.len() + store.grants.len() + store.tokens.len() + store.refresh.len();
        // Older stores above a row cap may still shrink through revocation.
        if let Err(error) = store.check_limits() {
            if after_rows >= before_rows || json.len() >= before.len() { return Err(error); }
        }
        // Unknown-token probes and repeated same-second uses need no disk write.
        if json == before { return Ok(value); }
        let tmp = path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // A unique temporary name also recovers cleanly after a crashed writer.
        let tmp = tmp.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let result = (|| {
            use std::io::Write;
            let mut file = options.open(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            #[cfg(unix)]
            std::fs::File::open(home)?.sync_all()?;
            Ok::<_, io::Error>(())
        })();
        if result.is_err() { let _ = std::fs::remove_file(&tmp); }
        result?;
        Ok(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const RESOURCE: &str = "https://mcp.example.com/mcp";
    const KEY: &str = "ddc_dev_0123456789abcdef0123456789abcdef";

    fn fixture(home: &Path) -> OAuthStore {
        std::fs::write(home.join("config.toml"), format!(
            "[mcp_keys.{KEY}]\nclient_id='operator'\nscopes=['memory:read']\nis_external=false\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let mut store = OAuthStore::default();
        store.grants.insert("family".into(), Grant {
            resource: RESOURCE.into(),
            client_id: "client".into(), client_name: "Example".into(), scope: "memory:read".into(),
            issued_unix: chrono::Utc::now().timestamp(), last_used_unix: None,
            consent: Consent::capture(home, KEY, "memory:read").unwrap(),
        });
        let token = IssuedToken {
            resource: RESOURCE.into(),
            client_id: "client".into(), client_name: "Example".into(), scope: "memory:read".into(),
            expires_unix: chrono::Utc::now().timestamp() + 3600, family_id: "family".into(), used: false,
        };
        store.tokens.insert("access-digest".into(), token.clone());
        store.refresh.insert("refresh-digest".into(), token);
        store
    }

    #[test]
    fn consent_revocation_and_scope_edits_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let store = fixture(home.path());
        let token = &store.tokens["access-digest"];
        assert!(store.valid_token(token, home.path(), RESOURCE));
        let path = home.path().join("config.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        for invalid in [String::new(), "[broken".into(), original.replace("memory:read", "wiki:read"), original.replace("false", "true")] {
            std::fs::write(&path, invalid).unwrap();
            assert!(!store.valid_token(token, home.path(), RESOURCE));
            assert!(store.active_grants(home.path()).map(|g| g.is_empty()).unwrap_or(true));
        }
    }

    #[test]
    fn capture_never_widens_operator_scopes() {
        let home = tempfile::tempdir().unwrap();
        fixture(home.path());
        assert!(Consent::capture(home.path(), KEY, "memory:read wiki:write").is_none());
    }

    #[test]
    fn family_revocation_removes_all_tokens_and_preserves_other_families() {
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture(home.path());
        let mut other = store.tokens["access-digest"].clone();
        other.family_id = "other".into();
        store.tokens.insert("other".into(), other);
        assert!(store.revoke_family("family"));
        assert_eq!(store.tokens.len(), 1);
        assert!(store.refresh.is_empty());
        assert!(!store.revoke_family("family"));
    }

    #[test]
    fn active_grants_exclude_secrets_and_legacy_tokens_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture(home.path());
        let listed = serde_json::to_string(&store.active_grants(home.path()).unwrap()).unwrap();
        assert!(listed.contains("Example"));
        for secret in [KEY, "access-digest", "refresh-digest", "key_hash", "entry_hash"] {
            assert!(!listed.contains(secret));
        }
        store.tokens.get_mut("access-digest").unwrap().family_id.clear();
        assert!(!store.valid_token(&store.tokens["access-digest"], home.path(), RESOURCE));
    }

    #[test]
    fn expired_or_expanded_consent_requires_authorization_again() {
        let home = tempfile::tempdir().unwrap();
        let store = fixture(home.path());
        let token = &store.tokens["access-digest"];
        let path = home.path().join("config.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut config: toml::Table = toml::from_str(&raw).unwrap();
        config["mcp_keys"][KEY]["created_at"] = toml::Value::String((chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339());
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        assert!(!store.valid_token(token, home.path(), RESOURCE));
        assert!(Consent::capture(home.path(), KEY, "memory:read").is_none());
        std::fs::write(&path, raw.replace("['memory:read']", "['admin']")).unwrap();
        assert!(!store.valid_token(token, home.path(), RESOURCE));
    }

    #[test]
    fn legacy_store_deserializes_but_cannot_authenticate() {
        let home = tempfile::tempdir().unwrap();
        let raw = serde_json::json!({"refresh": {"old": {
            "client_id": "client", "client_name": "Example", "scope": "memory:read",
            "expires_unix": chrono::Utc::now().timestamp() + 3600,
        }}});
        let store: OAuthStore = serde_json::from_value(raw).unwrap();
        assert!(!store.valid_token(&store.refresh["old"], home.path(), RESOURCE));
    }

    #[test]
    fn unchanged_transaction_preserves_file_and_unused_home() {
        let home = tempfile::tempdir().unwrap();
        update_store(home.path(), |_| ()).unwrap();
        assert!(!store_path(home.path()).exists());
        let store = fixture(home.path());
        let raw = serde_json::to_vec(&store).unwrap();
        std::fs::write(store_path(home.path()), &raw).unwrap();
        update_store(home.path(), |_| ()).unwrap();
        assert_eq!(std::fs::read(store_path(home.path())).unwrap(), raw);
    }

    #[test]
    fn corrupt_store_is_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(store_path(home.path()), "broken").unwrap();
        assert!(update_store(home.path(), |s| s.clients.clear()).is_err());
        assert_eq!(std::fs::read_to_string(store_path(home.path())).unwrap(), "broken");
    }

    #[test]
    fn concurrent_updates_keep_every_registration() {
        let home = tempfile::tempdir().unwrap();
        std::thread::scope(|threads| {
            for n in 0..8 {
                let home = home.path();
                threads.spawn(move || update_store(home, |s| {
                    s.clients.insert(n.to_string(), RegisteredClient {
                        client_name: n.to_string(), redirect_uris: vec![], created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
                    });
                }).unwrap());
            }
        });
        assert_eq!(load_store(home.path()).unwrap().clients.len(), 8);
    }

    #[test]
    fn later_transaction_observes_family_revocation() {
        let home = tempfile::tempdir().unwrap();
        let store = fixture(home.path());
        update_store(home.path(), |s| *s = store).unwrap();
        update_store(home.path(), |s| s.revoke_family("family")).unwrap();
        update_store(home.path(), |s| assert!(!s.refresh.contains_key("refresh-digest"))).unwrap();
        assert!(load_store(home.path()).unwrap().tokens.is_empty());
    }
    #[test]
    fn grant_list_distinguishes_config_failure_from_empty_registry() {
        let home = tempfile::tempdir().unwrap();
        let store = fixture(home.path());
        let config = home.path().join("config.toml");
        for raw in ["[broken", "mcp_keys=42"] {
            std::fs::write(&config, raw).unwrap();
            assert!(store.active_grants(home.path()).is_err());
        }
        std::fs::remove_file(&config).unwrap();
        assert!(store.active_grants(home.path()).is_err());
        for raw in ["", "[mcp_keys]"] {
            std::fs::write(&config, raw).unwrap();
            assert!(store.active_grants(home.path()).unwrap().is_empty());
        }
    }

    #[test]
    fn mint_capacity_counts_spent_rows_and_global_rows() {
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture(home.path());
        let mut spent = store.refresh["refresh-digest"].clone();
        spent.used = true;
        for n in 2..MAX_FAMILY_ROWS - 2 { store.refresh.insert(n.to_string(), spent.clone()); }
        assert!(store.can_mint(Some("family")));
        store.refresh.insert("last".into(), spent.clone());
        assert!(!store.can_mint(Some("family")));
        assert!(store.can_mint(Some("another")));
        store.refresh.clear();
        for n in 1..MAX_TOKEN_ROWS - 1 {
            spent.family_id = n.to_string();
            store.refresh.insert(n.to_string(), spent.clone());
        }
        assert!(!store.can_mint(None));
    }

    #[test]
    fn store_byte_limit_rejects_reads_and_writes_without_overwrite() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let path = store_path(home.path());
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_STORE_BYTES as u64 + 1).unwrap();
        assert!(load_store(home.path()).is_err());
        assert!(update_store(home.path(), |_| ()).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), MAX_STORE_BYTES as u64 + 1);
        std::fs::remove_file(&path).unwrap();
        assert!(update_store(home.path(), |s| {
            s.clients.insert("client".into(), RegisteredClient {
                client_name: "x".repeat(MAX_STORE_BYTES), redirect_uris: vec![], created_at: chrono::Utc::now().to_rfc3339(), last_used_unix: None,
            });
        }).is_err());
        assert!(!path.exists());
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"{}").unwrap();
        assert!(load_store(home.path()).is_ok());
    }

    #[test]
    fn over_limit_legacy_store_allows_revocation_but_not_growth() {
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture(home.path());
        let token = store.refresh["refresh-digest"].clone();
        for n in 0..MAX_FAMILY_ROWS { store.refresh.insert(n.to_string(), token.clone()); }
        // A second over-limit family must not prevent shrinking the first.
        for n in 0..MAX_FAMILY_ROWS + 1 {
            let mut other = token.clone(); other.family_id = "other".into();
            store.refresh.insert(format!("other-{n}"), other);
        }
        let path = store_path(home.path());
        std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
        assert!(update_store(home.path(), |s| s.revoke_family("family")).unwrap());
        assert!(load_store(home.path()).unwrap().refresh.values().all(|t| t.family_id == "other"));
        assert!(update_store(home.path(), |s| s.refresh.insert("extra".into(), token)).is_err());
        assert!(!load_store(home.path()).unwrap().refresh.contains_key("extra"));
    }

    #[test]
    fn concurrent_mint_capacity_is_checked_inside_transaction() {
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture(home.path());
        let token = store.refresh["refresh-digest"].clone();
        for n in 0..MAX_FAMILY_ROWS - 4 { store.refresh.insert(n.to_string(), token.clone()); }
        update_store(home.path(), |s| *s = store).unwrap();
        let successes = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|threads| {
            for _ in 0..8 {
                let home = home.path();
                let successes = &successes;
                let token = token.clone();
                threads.spawn(move || {
                    let minted = update_store(home, |s| {
                        if !s.can_mint(Some("family")) { return false; }
                        s.tokens.insert(uuid::Uuid::new_v4().to_string(), token.clone());
                        s.refresh.insert(uuid::Uuid::new_v4().to_string(), token);
                        true
                    }).unwrap();
                    if minted { successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                });
            }
        });
        assert_eq!(successes.load(std::sync::atomic::Ordering::Relaxed), 1);
        let stored = load_store(home.path()).unwrap();
        assert_eq!(stored.tokens.len() + stored.refresh.len(), MAX_FAMILY_ROWS);
    }

    #[test]
    fn access_and_refresh_require_the_exact_resource_on_token_and_grant() {
        let home = tempfile::tempdir().unwrap();
        let store = fixture(home.path());
        for token in [&store.tokens["access-digest"], &store.refresh["refresh-digest"]] {
            assert!(store.valid_token(token, home.path(), RESOURCE));
            assert!(!store.valid_token(token, home.path(), "https://other.example/mcp"));
            assert!(!store.valid_token(token, home.path(), ""));
            let mut changed = token.clone();
            changed.resource.clear();
            assert!(!store.valid_token(&changed, home.path(), RESOURCE));
            changed.resource = "https://other.example/mcp".into();
            assert!(!store.valid_token(&changed, home.path(), RESOURCE));
            let mut changed_store = store.clone();
            changed_store.grants.get_mut("family").unwrap().resource.clear();
            assert!(!changed_store.valid_token(token, home.path(), RESOURCE));
            changed_store.grants.get_mut("family").unwrap().resource = changed.resource.clone();
            assert!(!changed_store.valid_token(token, home.path(), RESOURCE));
        }
    }

    #[test]
    fn legacy_resource_less_rows_deserialize_but_require_new_authorization() {
        let home = tempfile::tempdir().unwrap();
        let mut raw = serde_json::to_value(fixture(home.path())).unwrap();
        for table in ["tokens", "refresh", "grants"] {
            for row in raw[table].as_object_mut().unwrap().values_mut() {
                row.as_object_mut().unwrap().remove("resource");
            }
        }
        let store: OAuthStore = serde_json::from_value(raw).unwrap();
        assert!(!store.valid_token(&store.tokens["access-digest"], home.path(), RESOURCE));
        assert!(!store.valid_token(&store.refresh["refresh-digest"], home.path(), RESOURCE));
    }

    fn registered_at(created: i64, last_used: Option<i64>) -> RegisteredClient {
        RegisteredClient {
            client_name: "Example".into(), redirect_uris: vec![],
            created_at: chrono::DateTime::from_timestamp(created, 0).unwrap().to_rfc3339(),
            last_used_unix: last_used,
        }
    }

    #[test]
    fn registration_lifetime_is_24_hours_unused_or_90_days_after_use() {
        let now = chrono::Utc::now().timestamp();
        assert!(registered_at(now - UNUSED_CLIENT_TTL_SECS + 1, None).active_at(now));
        assert!(!registered_at(now - UNUSED_CLIENT_TTL_SECS, None).active_at(now));
        assert!(registered_at(now - 100 * 86400, Some(now - INACTIVE_CLIENT_TTL_SECS + 1)).active_at(now));
        assert!(!registered_at(now - 100 * 86400, Some(now - INACTIVE_CLIENT_TTL_SECS)).active_at(now));
        let mut malformed = registered_at(now, Some(now));
        malformed.created_at = "invalid".into();
        assert!(!malformed.active_at(now));
    }

    #[test]
    fn prune_reclaims_unused_and_inactive_clients_but_keeps_live_references() {
        let home = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        let mut store = fixture(home.path());
        let old = registered_at(now - 100 * 86400, None);
        store.clients.insert("unused".into(), old.clone());
        store.clients.insert("inactive".into(), registered_at(now - 100 * 86400, Some(now - 91 * 86400)));
        store.clients.insert("recent".into(), registered_at(now, None));
        store.clients.insert("recently-used".into(), registered_at(now - 100 * 86400, Some(now)));
        store.clients.insert("client".into(), old.clone());
        store.clients.insert("pending".into(), old);
        store.codes.insert("code".into(), PendingCode {
            resource: RESOURCE.into(), client_id: "pending".into(), redirect_uri: "https://example.com/cb".into(),
            code_challenge: "challenge".into(), scope: "memory:read".into(), issued_unix: now,
            client_name: "Example".into(), consent: None,
        });
        update_store(home.path(), |s| *s = store).unwrap();
        let stored = load_store(home.path()).unwrap();
        assert_eq!(stored.clients.len(), 4);
        assert!(!stored.clients.contains_key("unused"));
        assert!(!stored.clients.contains_key("inactive"));
        assert!(stored.clients["client"].last_used_unix.is_some());
        assert!(stored.clients.contains_key("pending"));
        update_store(home.path(), |s| {
            s.codes.get_mut("code").unwrap().issued_unix = now - CODE_TTL_SECS - 1;
        }).unwrap();
        assert!(!load_store(home.path()).unwrap().clients.contains_key("pending"));
    }

    #[test]
    fn prune_frees_registration_capacity_before_the_transaction_callback() {
        let home = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        let mut store = OAuthStore::default();
        for n in 0..MAX_CLIENTS {
            store.clients.insert(n.to_string(), registered_at(now - 86401, None));
        }
        std::fs::write(store_path(home.path()), serde_json::to_vec(&store).unwrap()).unwrap();
        update_store(home.path(), |s| {
            assert!(s.clients.is_empty());
            s.clients.insert("new".into(), registered_at(now, None));
        }).unwrap();
        assert_eq!(load_store(home.path()).unwrap().clients.len(), 1);
    }

    #[test]
    fn capacity_eviction_selects_oldest_unreferenced_anonymous_client_after_grace() {
        let now = chrono::Utc::now().timestamp();
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture(home.path());
        for (id, age, used) in [
            ("oldest", 7200, None), ("younger", 3600, None),
            ("grace", 3599, None), ("authorized", 9000, Some(now - 8000)),
            ("client", 10000, None), ("pending", 11000, None),
            ("access-only", 12000, None), ("refresh-only", 13000, None),
        ] { store.clients.insert(id.into(), registered_at(now - age, used)); }
        store.codes.insert("code".into(), PendingCode {
            resource: RESOURCE.into(), client_id: "pending".into(), redirect_uri: "https://example.com/cb".into(),
            code_challenge: "challenge".into(), scope: "memory:read".into(), issued_unix: now,
            client_name: "Example".into(), consent: None,
        });
        let mut token = store.tokens["access-digest"].clone();
        token.family_id = "missing-grant".into();
        token.client_id = "access-only".into();
        store.tokens.insert("orphan-access".into(), token.clone());
        token.client_id = "refresh-only".into();
        store.refresh.insert("orphan-refresh".into(), token);
        assert_eq!(store.registration_eviction_candidate(now).as_deref(), Some("oldest"));
        store.clients.remove("oldest");
        assert_eq!(store.registration_eviction_candidate(now).as_deref(), Some("younger"));
        store.clients.remove("younger");
        assert!(store.registration_eviction_candidate(now).is_none());
        // Pruning must also retain live token references without a grant row.
        store.clients.get_mut("access-only").unwrap().created_at = registered_at(now - 86401, None).created_at;
        store.prune();
        assert!(store.clients.contains_key("access-only"));
    }

    #[test]
    fn full_registry_reclaims_one_old_anonymous_client_inside_transaction() {
        let home = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        update_store(home.path(), |store| {
            for n in 0..MAX_CLIENTS {
                store.clients.insert(format!("client-{n}"), registered_at(now - 3600 - n as i64, None));
            }
        }).unwrap();
        update_store(home.path(), |store| {
            let victim = store.registration_eviction_candidate(now).unwrap();
            assert_eq!(victim, format!("client-{}", MAX_CLIENTS - 1));
            store.clients.remove(&victim);
            store.clients.insert("new".into(), registered_at(now, None));
        }).unwrap();
        let stored = load_store(home.path()).unwrap();
        assert_eq!(stored.clients.len(), MAX_CLIENTS);
        assert!(stored.clients.contains_key("new"));
    }

    #[test]
    fn full_recent_or_previously_authorized_registry_has_no_eviction_candidate() {
        let now = chrono::Utc::now().timestamp();
        let mut store = OAuthStore::default();
        for n in 0..MAX_CLIENTS {
            store.clients.insert(n.to_string(), if n % 2 == 0 {
                registered_at(now - 3599, None)
            } else { registered_at(now - 7200, Some(now - 7000)) });
        }
        assert!(store.registration_eviction_candidate(now).is_none());
        assert_eq!(store.clients.len(), MAX_CLIENTS);
    }

}
