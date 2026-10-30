//! MFA policy and machine encryption, shared by HTTP and dashboard RPC.
use duduclaw_auth::{UserDb, UserRole, UserStatus, mfa::MfaCipher};
use serde_json::{Value, json};
use std::path::Path;

pub(crate) struct Cipher<'a>(pub &'a Path);
impl MfaCipher for Cipher<'_> {
    fn encrypt(&self, plaintext: &str) -> Result<String, String> {
        // Do not invoke the channel writer's legacy replace-invalid-key fallback.
        // create_new never overwrites an existing, unreadable or corrupted key.
        if !self
            .0
            .join(".keyfile")
            .try_exists()
            .map_err(|_| "無法讀取加密金鑰。")?
        {
            use std::io::Write;
            let key = duduclaw_security::crypto::CryptoEngine::generate_key()
                .map_err(|_| "無法建立加密金鑰。")?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(self.0.join(".keyfile")) {
                Ok(mut file) => {
                    file.write_all(&key)
                        .and_then(|_| file.sync_all())
                        .map_err(|_| "無法儲存加密金鑰。")?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("無法儲存加密金鑰。".into()),
            }
        }
        let key = crate::config_crypto::load_keyfile_public(self.0).ok_or("無法讀取加密金鑰。")?;
        duduclaw_security::crypto::CryptoEngine::new(&key)
            .and_then(|c| c.encrypt_string(plaintext))
            .map_err(|_| "無法加密驗證資料。".into())
    }
    fn decrypt(&self, ciphertext: &str) -> Result<String, String> {
        crate::config_crypto::decrypt_value(ciphertext, self.0)
            .ok_or_else(|| "無法讀取雙因素驗證資料。".into())
    }
}

pub(crate) fn enforced(home: &Path, personal: bool) -> Result<bool, String> {
    let text =
        std::fs::read_to_string(home.join("config.toml")).map_err(|_| "無法讀取登入設定。")?;
    let table: toml::Table = text.parse().map_err(|_| "登入設定格式不正確。")?;
    // Read errors always deny; a readable Personal configuration ignores the switch.
    if personal {
        return Ok(false);
    }
    let Some(auth) = table.get("auth") else {
        return Ok(false);
    };
    let auth = auth.as_table().ok_or("登入設定格式不正確。")?;
    match auth.get("mfa_enforced") {
        None => Ok(false),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| "雙因素驗證設定必須是布林值。".into()),
    }
}
pub(crate) fn require_admin(db: &UserDb, id: &str) -> Result<(), String> {
    match db.get_user(id)? {
        Some(u) if u.role == UserRole::Admin && u.status == UserStatus::Active => Ok(()),
        _ => Err("需要目前的管理員權限。".into()),
    }
}
pub(crate) fn enrollment_payload(secret: String, email: &str) -> Value {
    let label: String =
        url::form_urlencoded::byte_serialize(format!("DuDuClaw:{email}").as_bytes()).collect();
    json!({"secret":secret,"otpauth_uri":format!("otpauth://totp/{label}?secret={secret}&issuer=DuDuClaw&algorithm=SHA1&digits=6&period=30")})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mfa_policy_default_personal_and_unreadable_config() {
        let home = tempfile::tempdir().unwrap();
        assert!(enforced(home.path(), false).is_err());
        assert!(enforced(home.path(), true).is_err());
        for text in ["", "[auth]\nmfa_enforced=false"] {
            std::fs::write(home.path().join("config.toml"), text).unwrap();
            assert!(!enforced(home.path(), false).unwrap());
        }
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=true").unwrap();
        assert!(enforced(home.path(), false).unwrap());
        assert!(!enforced(home.path(), true).unwrap());
        for text in ["[auth", "auth=true", "[auth]\nmfa_enforced='true'"] {
            std::fs::write(home.path().join("config.toml"), text).unwrap();
            assert!(enforced(home.path(), false).is_err());
        }
    }
    #[test]
    fn mfa_cipher_roundtrip_corruption_never_replaces_key() {
        let home = tempfile::tempdir().unwrap();
        let cipher = Cipher(home.path());
        let encrypted = cipher.encrypt("secret").unwrap();
        assert_ne!(encrypted, "secret");
        assert_eq!(cipher.decrypt(&encrypted).unwrap(), "secret");
        std::fs::write(home.path().join(".keyfile"), b"bad").unwrap();
        assert!(cipher.encrypt("secret").is_err());
        assert!(cipher.decrypt(&encrypted).is_err());
        assert_eq!(std::fs::read(home.path().join(".keyfile")).unwrap(), b"bad");
    }
}
