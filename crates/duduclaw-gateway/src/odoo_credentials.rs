//! Gateway machine sealer for member Odoo credentials. Never export this to MCP.
use std::io::Write;
use std::path::Path;
use duduclaw_auth::odoo_credentials::OdooSecretSealer;
use duduclaw_security::crypto::CryptoEngine;
use zeroize::Zeroizing;

const UNAVAILABLE: &str = "目前無法讀取或保存 Odoo 連線，請管理員檢查。";
pub(crate) struct Sealer(CryptoEngine);
impl Sealer {
    pub(crate) fn load(home: &Path) -> Result<Self, String> {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| UNAVAILABLE.to_owned())?;
        let path = home.join(".keyfile");
        let meta = std::fs::symlink_metadata(&path).map_err(|_| UNAVAILABLE.to_owned())?;
        if !meta.is_file() || meta.len() != 32 { return Err(UNAVAILABLE.into()); }
        let key = Zeroizing::new(std::fs::read(path).map_err(|_| UNAVAILABLE.to_owned())?);
        CryptoEngine::new(key.as_slice().try_into().map_err(|_| UNAVAILABLE.to_owned())?)
            .map(Self).map_err(|_| UNAVAILABLE.to_owned())
    }
    pub(crate) fn for_write(home: &Path) -> Result<Self, String> {
        if let Ok(sealer) = Self::load(home) { return Ok(sealer); }
        let path = home.join(".keyfile");
        duduclaw_core::with_file_lock(&path, || {
            if let Ok(sealer) = Self::load(home) { return Ok(sealer); }
            duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| std::io::Error::other(UNAVAILABLE))?;
            // A missing key must never strand an existing member ciphertext.
            let db = rusqlite::Connection::open_with_flags(home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(std::io::Error::other)?;
            let existing: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM member_odoo_credentials)", [], |r| r.get(0))
                .map_err(std::io::Error::other)?;
            if existing { return Err(std::io::Error::other(UNAVAILABLE)); }
            let key = Zeroizing::new(CryptoEngine::generate_key().map_err(std::io::Error::other)?);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            // create_new refuses a corrupt file or symlink; never replace it.
            let mut file = options.open(&path)?;
            duduclaw_core::platform::set_owner_only(&path)?;
            file.write_all(key.as_ref())?;
            file.sync_all()?;
            CryptoEngine::new(&key).map(Self).map_err(std::io::Error::other)
        }).map_err(|_| UNAVAILABLE.to_owned())
    }
}
impl OdooSecretSealer for Sealer {
    fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        // CryptoEngine currently uses empty GCM AAD. Bind the owner inside the
        // authenticated plaintext, with an unambiguous NUL-delimited prefix.
        if aad.contains(&0) { return Err(UNAVAILABLE.into()); }
        let mut envelope = Zeroizing::new(aad.to_vec());
        envelope.push(0);
        envelope.extend_from_slice(plaintext);
        self.0.encrypt(&envelope).map_err(|_| UNAVAILABLE.to_owned())
    }
    fn open(&self, ct: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
        if aad.contains(&0) { return Err(UNAVAILABLE.into()); }
        let envelope = Zeroizing::new(self.0.decrypt(ct).map_err(|_| UNAVAILABLE.to_owned())?);
        let mut prefix = aad.to_vec();
        prefix.push(0);
        envelope.strip_prefix(prefix.as_slice()).map(|secret| Zeroizing::new(secret.to_vec()))
            .ok_or_else(|| UNAVAILABLE.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn odoo_member_machine_cipher_binds_aad_and_refuses_missing_or_broken_key() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("member@test.invalid", "Member", "password", duduclaw_auth::UserRole::Employee).unwrap();
        let sealer = Sealer::for_write(home.path()).unwrap();
        let ct = sealer.seal(b"key", b"odoo-member-v1|a").unwrap();
        assert_eq!(sealer.open(&ct, b"odoo-member-v1|a").unwrap().as_slice(), b"key");
        assert!(sealer.open(&ct, b"odoo-member-v1|b").is_err());
        db.odoo_credential_put(&user.id, "member", 7, "Member", "key", "fp", &sealer).unwrap();
        std::fs::remove_file(home.path().join(".keyfile")).unwrap();
        assert!(Sealer::for_write(home.path()).is_err());
        assert!(!home.path().join(".keyfile").exists());
        db.odoo_credential_delete(&user.id).unwrap();
        std::fs::write(home.path().join(".keyfile"), "broken").unwrap();
        assert!(Sealer::for_write(home.path()).is_err());
        assert_eq!(std::fs::read(home.path().join(".keyfile")).unwrap(), b"broken");
    }
}
