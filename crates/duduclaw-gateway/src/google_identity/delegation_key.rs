//! The delegation service-account key, sealed with the B2c protected key.
//!
//! A key that can impersonate anyone in the customer's domain must not sit
//! in a file an employee's `Read` tool can open. `delegate:` therefore only
//! ever uses this sealed copy: `<home>/protected/google-service-account.sealed`
//! (directory 0700, file 0600), whose secret part is `pk:v1:<key_id>:…`
//! (`ProtectedKey::seal`). The plaintext `key_file` of
//! `[integrations.google_service_account]` stays what it was — the home
//! account only.
//!
//! The record carries the non-secret `client_email` / `client_id` in clear so
//! the dashboard can show which client to authorize without unsealing; the
//! mint path uses only what is inside the sealed blob.
//!
//! A sealed blob that does not open with the current protected key means
//! the key was replaced (B2c design §6 item 3/4): delegation is refused, a
//! Critical security event is written once per process, and the admin is
//! told to reset the protection and re-import. The old record is never
//! overwritten silently.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use duduclaw_security::credential_protection::{ProtectedKey, parse_sealed};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::Refusal;
use crate::google_service_account::{ServiceAccountKey, parse_key};

const DIR: &str = "protected";
const FILE: &str = "google-service-account.sealed";
/// Largest key JSON accepted (a real key file is ~2.3 KiB).
pub const MAX_KEY_JSON_BYTES: usize = 16 * 1024;

pub fn path(home: &Path) -> PathBuf {
    home.join(DIR).join(FILE)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    client_email: String,
    #[serde(default)]
    client_id: Option<String>,
    imported_at: String,
    sealed: String,
}

/// What the dashboard may see. No secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyInfo {
    pub client_email: String,
    pub client_id: Option<String>,
    pub imported_at: String,
    /// The protected key id the record was sealed under.
    pub key_id: String,
}

fn read_record(home: &Path) -> Result<Option<Record>, String> {
    match std::fs::read_to_string(path(home)) {
        Ok(text) => serde_json::from_str::<Record>(&text)
            .map(Some)
            .map_err(|_| "封存的服務帳號金鑰紀錄已損壞".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("無法讀取封存的服務帳號金鑰".to_string()),
    }
}

/// The record's public part, without unsealing.
pub fn info(home: &Path) -> Result<Option<KeyInfo>, String> {
    let Some(r) = read_record(home)? else {
        return Ok(None);
    };
    let key_id = parse_sealed(&r.sealed)
        .map(|(k, _)| k.to_string())
        .map_err(|e| e.message_zh)?;
    Ok(Some(KeyInfo {
        client_email: r.client_email,
        client_id: r.client_id,
        imported_at: r.imported_at,
        key_id,
    }))
}

/// Validate a service-account key JSON, seal it and store it. Replaces any
/// previous record (an explicit admin act).
pub fn import(home: &Path, key: &ProtectedKey, raw_json: &str) -> Result<KeyInfo, String> {
    if raw_json.len() > MAX_KEY_JSON_BYTES {
        return Err("金鑰檔太大，這不是服務帳號金鑰。".into());
    }
    let parsed = parse_key(raw_json).map_err(|e| e.to_string())?;
    // A pasted token_uri decides where signed assertions go; only https.
    if let Some(uri) = parsed.token_uri.as_deref()
        && !token_uri_ok(uri)
    {
        return Err("金鑰檔的 token_uri 必須是 https 網址。".into());
    }
    let sealed = key.seal(raw_json.as_bytes()).map_err(|e| e.message_zh)?;
    let record = Record {
        version: 1,
        client_email: parsed.client_email.trim().to_string(),
        client_id: parsed.client_id.clone().filter(|c| !c.trim().is_empty()),
        imported_at: chrono::Utc::now().to_rfc3339(),
        sealed,
    };
    let body = serde_json::to_vec_pretty(&record).map_err(|e| format!("encode: {e}"))?;
    crate::disconnect_forget::connections::mark_connecting(home, "google:delegation")?;
    write_owner_only_atomic(home, &body)?;
    info(home)?.ok_or_else(|| "寫入後讀不回封存紀錄".into())
}

/// `https://…` (or, in a live-test build, the mock's loopback `http://`).
fn token_uri_ok(uri: &str) -> bool {
    let Ok(u) = url::Url::parse(uri) else {
        return false;
    };
    if u.scheme() == "https" {
        return u.host_str().is_some();
    }
    cfg!(any(test, feature = "live-test-mocks"))
        && u.scheme() == "http"
        && matches!(u.host_str(), Some("127.0.0.1") | Some("localhost"))
}

/// Remove the sealed record. Missing is success.
pub fn remove(home: &Path) -> Result<(), String> {
    match std::fs::remove_file(path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("無法刪除封存的服務帳號金鑰：{e}")),
    }
}

const KEY_REPLACED: &str = "保護金鑰已被更換，原本封存的服務帳號金鑰解不開。為了安全，公司授權代表已停用。請管理員到「安全」頁重設憑證保護，再到「整合 → Google → 公司授權代表」重新匯入服務帳號金鑰。";

/// Unseal the key for a mint. Missing → fallback-eligible
/// `delegate_unavailable`; any unseal failure → hard `credential_protection`
/// (the key-replaced case is also written to the security audit).
pub fn load(home: &Path, key: &ProtectedKey) -> Result<ServiceAccountKey, Refusal> {
    let record = match read_record(home) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(Refusal::soft(
                "delegate_unavailable",
                "公司授權代表還沒設定：管理員要先在「整合 → Google → 公司授權代表」匯入服務帳號金鑰。",
            ));
        }
        Err(e) => {
            return Err(Refusal::hard(
                "credential_protection",
                format!("{e}，請管理員重新匯入服務帳號金鑰。"),
            ));
        }
    };
    let plain: Zeroizing<Vec<u8>> = match key.unseal(&record.sealed) {
        Ok(p) => p,
        Err(e) if e.code == "key_changed" => {
            report_key_replaced(home, &record);
            return Err(Refusal::hard("credential_protection", KEY_REPLACED));
        }
        Err(e) => {
            return Err(Refusal::hard(
                "credential_protection",
                format!(
                    "封存的服務帳號金鑰解不開（{}），請管理員重新匯入。",
                    e.message_zh
                ),
            ));
        }
    };
    let text = std::str::from_utf8(&plain).map_err(|_| {
        Refusal::hard(
            "credential_protection",
            "封存的服務帳號金鑰內容錯誤，請管理員重新匯入。",
        )
    })?;
    parse_key(text).map_err(|_| {
        Refusal::hard(
            "credential_protection",
            "封存的服務帳號金鑰內容錯誤，請管理員重新匯入。",
        )
    })
}

fn report_key_replaced(home: &Path, record: &Record) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let first = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(home.to_path_buf());
    if !first {
        return;
    }
    let sealed_key_id = parse_sealed(&record.sealed)
        .map(|(k, _)| k.to_string())
        .unwrap_or_default();
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "google_delegation_key_unreadable",
            "gateway",
            duduclaw_security::audit::Severity::Critical,
            serde_json::json!({
                "reason": "key_changed",
                "sealed_key_id": sealed_key_id,
                "client_email": record.client_email,
            }),
        ),
    );
}

fn write_owner_only_atomic(home: &Path, body: &[u8]) -> Result<(), String> {
    let dir = home.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("無法建立資料夾：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("無法設定資料夾權限：{e}"))?;
    }
    let suffix: u64 = rand::random();
    // Random suffix + create_new: a planted file or symlink at the temporary
    // name fails the write instead of being followed.
    let tmp = dir.join(format!(".{FILE}.tmp-{suffix:016x}"));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| format!("無法寫入：{e}"))?;
        f.write_all(body).map_err(|e| format!("無法寫入：{e}"))?;
        f.sync_all().map_err(|e| format!("無法寫入：{e}"))?;
        duduclaw_core::platform::set_owner_only(&tmp).map_err(|e| format!("無法設定權限：{e}"))?;
        std::fs::rename(&tmp, path(home)).map_err(|e| format!("無法寫入：{e}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[path = "delegation_key/w3_material.rs"]
pub(crate) mod w3_material;
