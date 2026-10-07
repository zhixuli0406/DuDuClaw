//! Per-(employee, server) records of connected remote MCP servers.
//!
//! File: `<home>/remote_mcp/servers.json`, mode 0600 (written through the same
//! owner-only atomic writer as `.mcp.json`), directory 0700, every
//! read-modify-write under `duduclaw_core::with_file_lock`.
//!
//! What is plaintext: employee id, server name, auth kind, the URL's host (for
//! display), connection status, timestamps, whether a refresh token exists.
//! Everything else — the full URL (a hosted aggregator's URL can embed an API
//! key in its path), the bearer token, OAuth client credentials, access and
//! refresh tokens — is one JSON blob encrypted with the per-machine keyfile
//! (`config_crypto::encrypt_value`, AES-256-GCM), the same key the gateway
//! uses for channel tokens and OAuth tokens. If encryption is unavailable the
//! write fails; nothing is ever stored in plaintext.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Directory under the home that holds this feature's state.
pub const STORE_DIR: &str = "remote_mcp";
/// The record file inside [`STORE_DIR`].
pub const STORE_FILE: &str = "servers.json";

/// How the gateway authenticates to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    None,
    Bearer,
    Oauth,
}

impl AuthKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "bearer" => Some(Self::Bearer),
            "oauth" => Some(Self::Oauth),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bearer => "bearer",
            Self::Oauth => "oauth",
        }
    }
}

/// Whether the bridge can use the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnStatus {
    /// Installed (URL known) but never connected, or disconnected.
    NotConnected,
    /// Usable.
    Connected,
    /// The refresh token was refused; an operator must sign in again.
    NeedsReauth,
}

impl ConnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConnected => "not_connected",
            Self::Connected => "connected",
            Self::NeedsReauth => "needs_reauth",
        }
    }
}

/// One record. Only the non-secret fields are readable without the keyfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteServerRecord {
    pub agent_id: String,
    pub server: String,
    pub auth: AuthKind,
    /// `host[:port]` of the URL, for display only.
    pub host: String,
    pub status: ConnStatus,
    pub created_at: String,
    pub updated_at: String,
    /// Unix seconds when the stored access token expires (OAuth only).
    #[serde(default)]
    pub access_expires_at: Option<i64>,
    #[serde(default)]
    pub has_refresh_token: bool,
    /// [`RemoteSecrets`] as JSON, encrypted. See the module doc.
    pub secret_enc: String,
}

/// The encrypted half of a record.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct RemoteSecrets {
    pub url: String,
    #[serde(default)]
    pub bearer: Option<String>,
    #[serde(default)]
    pub oauth: Option<OAuthSecrets>,
}

impl std::fmt::Debug for RemoteSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSecrets")
            .field("url", &"«redacted»")
            .field("bearer", &self.bearer.as_ref().map(|_| "«set»"))
            .field("oauth", &self.oauth.as_ref().map(|_| "«set»"))
            .finish()
    }
}

/// OAuth state needed to refresh and to use the token.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OAuthSecrets {
    pub issuer: String,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// `none`, `client_secret_post` or `client_secret_basic`.
    pub token_endpoint_auth: String,
    /// RFC 8707 resource indicator sent on every token request.
    pub resource: String,
    #[serde(default)]
    pub scope: Option<String>,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    servers: Vec<RemoteServerRecord>,
}

fn store_version() -> u32 {
    1
}

/// `<home>/remote_mcp/servers.json`.
pub fn store_path(home: &Path) -> PathBuf {
    home.join(STORE_DIR).join(STORE_FILE)
}

/// `<home>/remote_mcp/locks/`, one refresh lock per record.
pub fn lock_dir(home: &Path) -> PathBuf {
    home.join(STORE_DIR).join("locks")
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

/// Read the file. Missing ⇒ empty; present but unreadable or malformed ⇒
/// error (a broken store is never treated as "no servers", which would let a
/// write silently drop every other record).
fn read_file(home: &Path) -> Result<StoreFile, String> {
    let path = store_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid ({e}); fix or remove it", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_file(home: &Path, file: &StoreFile) -> Result<(), String> {
    let path = store_path(home);
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let json = serde_json::to_string_pretty(file).map_err(|e| format!("serialize: {e}"))?;
    duduclaw_agent::mcp_template::write_mcp_config_atomic(&path, json.as_bytes())
}

/// Run `f` on the whole record list under the store lock and write the
/// result back when `f` returns `Ok(true)`.
fn modify<T>(
    home: &Path,
    f: impl FnOnce(&mut Vec<RemoteServerRecord>) -> Result<(bool, T), String>,
) -> Result<T, String> {
    let path = store_path(home);
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let mut out: Option<Result<T, String>> = None;
    duduclaw_core::with_file_lock(&path, || {
        let res = (|| {
            let mut file = read_file(home)?;
            let (changed, value) = f(&mut file.servers)?;
            if changed {
                file.version = store_version();
                write_file(home, &file)?;
            }
            Ok(value)
        })();
        out = Some(res);
        Ok(())
    })
    .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
    out.unwrap_or_else(|| Err("store update did not run".into()))
}

/// Every record (secrets still sealed).
pub fn load_all(home: &Path) -> Result<Vec<RemoteServerRecord>, String> {
    Ok(read_file(home)?.servers)
}

/// One record.
pub fn get(home: &Path, agent_id: &str, server: &str) -> Result<Option<RemoteServerRecord>, String> {
    Ok(load_all(home)?
        .into_iter()
        .find(|r| r.agent_id == agent_id && r.server == server))
}

/// Insert or replace the record with the same (agent, server).
pub fn upsert(home: &Path, record: RemoteServerRecord) -> Result<(), String> {
    modify(home, move |list| {
        list.retain(|r| !(r.agent_id == record.agent_id && r.server == record.server));
        list.push(record);
        list.sort_by(|a, b| (&a.agent_id, &a.server).cmp(&(&b.agent_id, &b.server)));
        Ok((true, ()))
    })
}

/// Remove a record. Returns whether one existed.
pub fn remove(home: &Path, agent_id: &str, server: &str) -> Result<bool, String> {
    modify(home, |list| {
        let before = list.len();
        list.retain(|r| !(r.agent_id == agent_id && r.server == server));
        let removed = list.len() != before;
        Ok((removed, removed))
    })
}

/// Change one record in place under the lock. `f` returns whether it changed
/// anything. Missing record ⇒ `Ok(false)` without calling `f`.
pub fn update(
    home: &Path,
    agent_id: &str,
    server: &str,
    f: impl FnOnce(&mut RemoteServerRecord) -> Result<bool, String>,
) -> Result<bool, String> {
    modify(home, |list| {
        match list
            .iter_mut()
            .find(|r| r.agent_id == agent_id && r.server == server)
        {
            Some(rec) => {
                let changed = f(rec)?;
                if changed {
                    rec.updated_at = now_rfc3339();
                }
                Ok((changed, changed))
            }
            None => Ok((false, false)),
        }
    })
}

/// Encrypt secrets for a record. Fails (never falls back to plaintext) when
/// the keyfile cannot be created or used.
pub fn seal(home: &Path, secrets: &RemoteSecrets) -> Result<String, String> {
    let json = serde_json::to_string(secrets).map_err(|e| format!("serialize secrets: {e}"))?;
    crate::config_crypto::encrypt_value(&json, home)
        .ok_or_else(|| "cannot encrypt the connection secrets (keyfile unavailable); nothing was stored".to_string())
}

/// Decrypt a record's secrets.
pub fn open(home: &Path, record: &RemoteServerRecord) -> Result<RemoteSecrets, String> {
    let json = duduclaw_security::keyfile::decrypt_keyfile_value(&record.secret_enc, home)
        .ok_or_else(|| {
            format!(
                "cannot decrypt the stored connection for {}/{} (keyfile missing or changed); connect it again",
                record.agent_id, record.server
            )
        })?;
    serde_json::from_str(&json).map_err(|e| format!("stored connection is malformed: {e}"))
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Validate the (employee, server) pair every entry point takes. Server names
/// follow the `.mcp.json` key rule, and names starting with `duduclaw` are
/// reserved (the redaction rewrite treats `duduclaw*` entries as DuDuClaw's
/// own and would not wrap them).
pub fn validate_ids(agent_id: &str, server: &str) -> Result<(), String> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err("invalid agent_id".into());
    }
    if !crate::mcp_scan::is_valid_mcp_server_name(server) {
        return Err("invalid server name (allowed: A-Za-z0-9._- max 64)".into());
    }
    if server.to_ascii_lowercase().starts_with("duduclaw") {
        return Err("server names starting with \"duduclaw\" are reserved".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(home: &Path, agent: &str, server: &str, token: &str) -> RemoteServerRecord {
        let secrets = RemoteSecrets {
            url: "https://mcp.example.com/api/s/KEY123/mcp".into(),
            bearer: Some(token.into()),
            oauth: None,
        };
        RemoteServerRecord {
            agent_id: agent.into(),
            server: server.into(),
            auth: AuthKind::Bearer,
            host: "mcp.example.com".into(),
            status: ConnStatus::Connected,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            access_expires_at: None,
            has_refresh_token: false,
            secret_enc: seal(home, &secrets).unwrap(),
        }
    }

    #[test]
    fn secrets_are_encrypted_on_disk_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        upsert(home, record(home, "a1", "zap", "sk-very-secret-token")).unwrap();
        let raw = std::fs::read_to_string(store_path(home)).unwrap();
        assert!(!raw.contains("sk-very-secret-token"));
        assert!(!raw.contains("KEY123"), "full URL must not be on disk in plaintext");
        assert!(raw.contains("mcp.example.com"));
        let rec = get(home, "a1", "zap").unwrap().unwrap();
        let s = open(home, &rec).unwrap();
        assert_eq!(s.bearer.as_deref(), Some("sk-very-secret-token"));
        assert!(!format!("{s:?}").contains("sk-very"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store_path(home)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn upsert_update_and_remove_keep_other_records() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        upsert(home, record(home, "a1", "one", "t1")).unwrap();
        upsert(home, record(home, "a2", "one", "t2")).unwrap();
        upsert(home, record(home, "a1", "one", "t3")).unwrap();
        assert_eq!(load_all(home).unwrap().len(), 2);
        assert!(update(home, "a2", "one", |r| {
            r.status = ConnStatus::NeedsReauth;
            Ok(true)
        })
        .unwrap());
        assert_eq!(get(home, "a2", "one").unwrap().unwrap().status, ConnStatus::NeedsReauth);
        assert!(!update(home, "zz", "one", |_| Ok(true)).unwrap());
        assert!(remove(home, "a1", "one").unwrap());
        assert!(!remove(home, "a1", "one").unwrap());
        assert_eq!(load_all(home).unwrap().len(), 1);
    }

    #[test]
    fn a_malformed_store_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(STORE_DIR)).unwrap();
        std::fs::write(store_path(home), "{not json").unwrap();
        assert!(load_all(home).is_err());
        assert!(upsert(home, record(home, "a1", "x", "t")).is_err());
    }

    #[test]
    fn reserved_and_invalid_ids_are_refused() {
        assert!(validate_ids("a1", "zapier").is_ok());
        assert!(validate_ids("a1", "duduclaw").is_err());
        assert!(validate_ids("a1", "DuDuClaw-x").is_err());
        assert!(validate_ids("../x", "zapier").is_err());
        assert!(validate_ids("a1", "bad/name").is_err());
    }
}
