//! WebAuthn verification is delegated to webauthn-rs. Ceremony state never leaves users.db.
use super::*;
use serde_json::{Value, json};
use webauthn_rs::prelude::*;

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS mfa_passkeys (
        id TEXT PRIMARY KEY, user_id TEXT NOT NULL, name TEXT NOT NULL,
        credential TEXT NOT NULL, created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_mfa_passkeys_user ON mfa_passkeys(user_id);
        CREATE TABLE IF NOT EXISTS mfa_passkey_ceremonies (
        token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, purpose TEXT NOT NULL,
        state TEXT NOT NULL, origin TEXT NOT NULL, binding TEXT NOT NULL,
        session TEXT, epoch INTEGER NOT NULL, expires_at INTEGER NOT NULL, name TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_mfa_passkey_ceremonies_user ON mfa_passkey_ceremonies(user_id);")
        .map_err(|e| e.to_string())
}

/// Accept an operator-configured exact origin, never a host supplied by a login request.
pub fn relying_party(origin: &str) -> Result<Webauthn> {
    let url = Url::parse(origin).map_err(|_| "通行金鑰的控制台網址設定無效。")?;
    let host = url.host_str().ok_or("通行金鑰的控制台網址設定無效。")?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some()
        || url.fragment().is_some() || url.path() != "/"
        || !(url.scheme() == "https" || (url.scheme() == "http" && host == "localhost")) {
        return Err("通行金鑰需要 HTTPS；本機測試請使用 localhost。".into());
    }
    WebauthnBuilder::new(host, &url).map_err(|_| DENIED.to_string())?
        .rp_name("DuDuClaw").build().map_err(|_| DENIED.into())
}

fn keys(conn: &Connection, user: &str) -> Result<Vec<Passkey>> {
    let mut stmt = conn.prepare("SELECT credential FROM mfa_passkeys WHERE user_id=?1 ORDER BY created_at,id")
        .map_err(|e| e.to_string())?;
    let values = stmt.query_map([user], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    values.into_iter().map(|value| serde_json::from_str(&value).map_err(|_| DENIED.into())).collect()
}

// Only read credential internals to compare the CURRENT persisted counter.
// Never reconstruct a credential from caller input or change its verification policy.
fn counter(key: &Passkey) -> u32 {
    let credential: Credential = key.clone().into();
    credential.counter
}
fn counter_valid(old: u32, new: u32) -> bool { (old == 0 && new == 0) || new > old }
fn credential_id(key: &Passkey) -> Result<String> {
    serde_json::to_string(key.cred_id()).map_err(|e| e.to_string())
}

fn authorize_registration(conn: &Connection, user: &str, token: &str, session: Option<&str>, now: i64, _exempt: bool) -> Result<()> {
    active_user(conn, user)?;
    if let Some(session) = session {
        check_reauth(conn, user, token, session, now)?;
    } else {
        if challenge_user(conn, token, "enroll", now)? != user || !proof_verified(conn, token)? {
            return Err(DENIED.into());
        }
        let public: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM mfa_enrollment_proofs WHERE token_hash=?1 AND session IS NULL)",
            [token_hash(token)], |r|r.get(0)).map_err(|e|e.to_string())?;
        if !public { return Err(DENIED.into()); }
    }
    if policy_enforced(conn)? && !any_factor(conn, user)? {
        enrollment::require_independent_proof(conn, token, now)?;
    }
    Ok(())
}

struct Ceremony { user: String, state: String, name: String }
#[allow(clippy::too_many_arguments)]
fn take_ceremony(conn: &Connection, id: &str, purpose: &str, token: &str, session: Option<&str>, origin: &str, now: i64) -> Result<Ceremony> {
    let result = conn.query_row("SELECT c.user_id,c.state,c.name FROM mfa_passkey_ceremonies c
        JOIN users u ON u.id=c.user_id WHERE c.token_hash=?1 AND c.purpose=?2 AND c.binding=?3
        AND c.session IS ?4 AND c.origin=?5 AND c.expires_at>?6 AND u.status='active'
        AND u.credential_epoch=c.epoch",
        params![token_hash(id), purpose, token_hash(token), session, origin, now],
        |r| Ok(Ceremony { user:r.get(0)?, state:r.get(1)?, name:r.get(2)? })).map_err(|_| DENIED)?;
    conn.execute("DELETE FROM mfa_passkey_ceremonies WHERE token_hash=?1", [token_hash(id)]).map_err(|e|e.to_string())?;
    Ok(result)
}
#[allow(clippy::too_many_arguments)]
fn save_ceremony(conn: &Connection, user: &str, purpose: &str, token: &str, session: Option<&str>, origin: &str, name: &str, state: String, now: i64) -> Result<String> {
    let epoch = crate::user_row::load_user(conn, user)?.ok_or(DENIED)?.credential_epoch;
    let id = random_hex(32)?;
    conn.execute("DELETE FROM mfa_passkey_ceremonies WHERE expires_at<=?1 OR (user_id=?2 AND purpose=?3 AND binding=?4)",
        params![now,user,purpose,token_hash(token)]).map_err(|e|e.to_string())?;
    let count: i64 = conn.query_row("SELECT count(*) FROM mfa_passkey_ceremonies WHERE user_id=?1", [user], |r|r.get(0)).map_err(|e|e.to_string())?;
    if count >= 10 { return Err("驗證要求過多，請稍後再試。".into()); }
    conn.execute("INSERT INTO mfa_passkey_ceremonies(token_hash,user_id,purpose,state,origin,binding,session,epoch,expires_at,name)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![token_hash(&id),user,purpose,state,origin,token_hash(token),session,epoch,now+TTL,name]).map_err(|e|e.to_string())?;
    Ok(id)
}

/// The one assertion verifier (login and the F6d restore proof): webauthn-rs
/// over the server-held ceremony state, user verification required, the
/// signature counter compared with the LIVE row and persisted on `tx`.
fn verify_assertion(tx: &Connection, rp: &Webauthn, user: &str, state: &str, credential: Value) -> Result<()> {
    let state: PasskeyAuthentication = serde_json::from_str(state).map_err(|_|DENIED)?;
    let response: PublicKeyCredential = serde_json::from_value(credential).map_err(|_|DENIED)?;
    let result = rp.finish_passkey_authentication(&response,&state).map_err(|_|DENIED)?;
    if !result.user_verified() { return Err(DENIED.to_string()); }
    // Compare with the live row, not the snapshot captured by begin.
    let mut key = keys(tx,user)?.into_iter().find(|k|k.cred_id()==result.cred_id()).ok_or(DENIED)?;
    if !counter_valid(counter(&key),result.counter()) { return Err(DENIED.into()); }
    key.update_credential(&result).ok_or(DENIED)?;
    tx.execute("UPDATE mfa_passkeys SET credential=?2 WHERE id=?1 AND user_id=?3",
        params![credential_id(&key)?,serde_json::to_string(&key).map_err(|_|DENIED)?,user]).map_err(|e|e.to_string())?;
    Ok(())
}

/// F6d restore proof: an assertion challenge bound to `user` and `purpose`
/// (`restore:<employee_id>:<incarnation>`): the ceremony row carries the
/// purpose and binding = hash(purpose), no session, the operator-configured
/// origin and the credential epoch; [`TTL`] seconds; a new one for the same
/// purpose replaces the old. Returns the WebAuthn request options with
/// `challenge_id`.
pub(super) fn purpose_begin(tx: &Connection, origin: &str, user: &str, purpose: &str, now: i64) -> Result<Value> {
    let rp = relying_party(origin)?;
    let keys = keys(tx,user)?;
    if keys.is_empty() { return Err(DENIED.into()); }
    let (challenge,state) = rp.start_passkey_authentication(&keys).map_err(|_|DENIED)?;
    let id = save_ceremony(tx,user,purpose,purpose,None,origin,"",serde_json::to_string(&state).map_err(|_|DENIED)?,now)?;
    let mut response = serde_json::to_value(challenge).map_err(|_|DENIED)?;
    response["challenge_id"] = json!(id);
    Ok(response)
}

/// Consume the `purpose` ceremony `id` of `user` (single use: deleted before
/// the assertion is checked, so a failed proof cannot be retried) and verify
/// the assertion with [`verify_assertion`]. A ceremony of another purpose,
/// user, origin or epoch, or an expired one, is never found.
pub(super) fn purpose_finish(tx: &Connection, origin: &str, user: &str, purpose: &str, id: &str, credential: Value, now: i64)
    -> Result<()> {
    let rp = relying_party(origin)?;
    // Another account's attempt must not consume the owner's ceremony.
    let owner: Option<String> = tx.query_row("SELECT user_id FROM mfa_passkey_ceremonies WHERE token_hash=?1 AND purpose=?2",
        params![token_hash(id),purpose], |r|r.get(0)).optional().map_err(|e|e.to_string())?;
    if owner.as_deref() != Some(user) { return Err(DENIED.into()); }
    let ceremony = take_ceremony(tx,id,purpose,purpose,None,origin,now)?;
    if ceremony.user != user { return Err(DENIED.into()); }
    verify_assertion(tx,&rp,user,&ceremony.state,credential)
}

impl UserDb {
    pub fn mfa_passkeys(&self, user: &str) -> Result<Vec<Value>> {
        let conn = self.conn();
        active_user(&conn, user)?;
        let mut stmt = conn.prepare("SELECT id,name,created_at FROM mfa_passkeys WHERE user_id=?1 ORDER BY created_at,id").map_err(|e|e.to_string())?;
        stmt.query_map([user], |r| Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?})))
            .map_err(|e|e.to_string())?.collect::<std::result::Result<_,_>>().map_err(|e|e.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mfa_passkey_register_begin(&self, user: &str, token: &str, session: Option<&str>, origin: &str, name: &str, now: i64, exempt: bool) -> Result<Value> {
        if name.trim().is_empty() || name.chars().count()>80 || name.chars().any(char::is_control) { return Err("請輸入 1–80 字的通行金鑰名稱。".into()); }
        let rp = relying_party(origin)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        authorize_registration(&tx, user, token, session, now, exempt)?;
        let account = crate::user_row::load_user(&tx, user)?.ok_or(DENIED)?;
        let existing = keys(&tx, user)?;
        if existing.len() >= 20 { return Err("最多可註冊 20 把通行金鑰。".into()); }
        let (challenge, state) = rp.start_passkey_registration(
            Uuid::parse_str(user).map_err(|_| DENIED)?, &account.email, &account.display_name,
            Some(existing.iter().map(|k|k.cred_id().clone()).collect()),
        ).map_err(|_| DENIED)?;
        let id = save_ceremony(&tx,user,"register",token,session,origin,name.trim(),serde_json::to_string(&state).map_err(|_| DENIED)?,now)?;
        let mut response = serde_json::to_value(challenge).map_err(|_| DENIED)?;
        response["ceremony_id"] = json!(id);
        response["enrollment_token"] = json!(token);
        tx.commit().map_err(|e|e.to_string())?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mfa_passkey_register_finish<T>(&self, id: &str, token: &str, session: Option<&str>, origin: &str, credential: Value, now: i64, exempt: bool,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)], Vec<String>) -> Result<T>) -> Result<T> {
        let rp = relying_party(origin)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let since = crate::credential_changes::high_water(&tx)?;
        let ceremony = take_ceremony(&tx,id,"register",token,session,origin,now)?;
        let verified = (|| {
            authorize_registration(&tx,&ceremony.user,token,session,now,exempt)?;
            let state: PasskeyRegistration = serde_json::from_str(&ceremony.state).map_err(|_| DENIED)?;
            let response: RegisterPublicKeyCredential = serde_json::from_value(credential).map_err(|_| DENIED)?;
            rp.finish_passkey_registration(&response,&state).map_err(|_| DENIED.to_string())
        })();
        let key = match verified { Ok(key)=>key, Err(error)=>{
            audit(&tx,&ceremony.user,"mfa.passkey_registration_failed",None)?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(error);
        }};
        // A globally unique credential id prevents sharing one key across accounts.
        let key_id = credential_id(&key)?;
        let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_passkeys WHERE id=?1)",[&key_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if duplicate || keys(&tx,&ceremony.user)?.len() >= 20 {
            audit(&tx,&ceremony.user,"mfa.passkey_registration_failed",None)?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(DENIED.into());
        }
        tx.execute("INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![key_id,ceremony.user,ceremony.name,serde_json::to_string(&key).map_err(|_|DENIED)?,now]).map_err(|_|DENIED)?;
        // Existing recovery codes survive adding a passkey. First-time passkey
        // enrollment does not replace or issue recovery secrets over HTTP.
        let codes = Vec::new();
        tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1", [&ceremony.user]).map_err(|e|e.to_string())?;
        tx.execute("DELETE FROM mfa_passkey_ceremonies WHERE user_id=?1", [&ceremony.user]).map_err(|e|e.to_string())?;
        bump_epoch(&tx,&ceremony.user)?;
        audit(&tx,&ceremony.user,"mfa.passkey_added",None)?;
        stamp_self(&tx,since,&ceremony.user)?;
        let result = issue_mfa_result(&tx,&ceremony.user,codes,issue)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(result)
    }

    pub fn mfa_passkey_login_begin(&self, token: &str, origin: &str, now: i64) -> Result<Value> {
        let rp = relying_party(origin)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let user = challenge_user(&tx,token,"login",now)?;
        active_user(&tx,&user)?;
        let (challenge,state) = rp.start_passkey_authentication(&keys(&tx,&user)?).map_err(|_|DENIED)?;
        let id = save_ceremony(&tx,&user,"login",token,None,origin,"",serde_json::to_string(&state).map_err(|_|DENIED)?,now)?;
        let mut response = serde_json::to_value(challenge).map_err(|_|DENIED)?;
        response["ceremony_id"] = json!(id);
        tx.commit().map_err(|e|e.to_string())?;
        Ok(response)
    }

    pub fn mfa_passkey_login_finish<T>(&self, id: &str, token: &str, origin: &str, credential: Value, now: i64,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)], Vec<String>) -> Result<T>) -> Result<T> {
        let rp = relying_party(origin)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let user = challenge_user(&tx,token,"login",now)?;
        let ceremony = take_ceremony(&tx,id,"login",token,None,origin,now)?;
        if ceremony.user != user { return Err(DENIED.into()); }
        tx.execute("UPDATE mfa_challenges SET attempts=attempts+1 WHERE token_hash=?1", [token_hash(token)]).map_err(|e|e.to_string())?;
        if let Err(error) = verify_assertion(&tx,&rp,&user,&ceremony.state,credential) {
            audit(&tx,&user,"mfa.verification_failed",None)?;
            tx.commit().map_err(|e|e.to_string())?;
            return Err(error);
        }
        tx.execute("UPDATE mfa_challenges SET consumed=1 WHERE token_hash=?1", [token_hash(token)]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO mfa_factor_preferences(user_id,last_used) VALUES(?1,'passkey') ON CONFLICT(user_id) DO UPDATE SET last_used='passkey'", [&user]).map_err(|e|e.to_string())?;
        audit(&tx,&user,"mfa.passkey_login",None)?;
        let result = issue_mfa_result(&tx,&user,Vec::new(),issue)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mfa_passkey_delete<T>(&self, user: &str, id: &str, reauth: &str, session: &str, now: i64, _exempt: bool,
        issue: impl FnOnce(&crate::User, &[(String, crate::AccessLevel)], Vec<String>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
        let since = crate::credential_changes::high_water(&tx)?;
        active_user(&tx,user)?;
        check_reauth(&tx,user,reauth,session,now)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM mfa_passkeys WHERE user_id=?1 AND id=?2",params![user,id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if count != 1 { return Err(DENIED.into()); }
        if policy_enforced(&tx)? && factor_count(&tx,user)?<=1 { return Err("管理員要求兩步驟驗證，請先新增其他驗證方式。".into()); }
        tx.execute("DELETE FROM mfa_passkeys WHERE id=?1 AND user_id=?2",params![id,user]).map_err(|e|e.to_string())?;
        tx.execute("DELETE FROM mfa_challenges WHERE user_id=?1",[user]).map_err(|e|e.to_string())?;
        tx.execute("DELETE FROM mfa_passkey_ceremonies WHERE user_id=?1",[user]).map_err(|e|e.to_string())?;
        if !any_factor(&tx,user)? { tx.execute("DELETE FROM mfa_recovery_codes WHERE user_id=?1",[user]).map_err(|e|e.to_string())?; }
        bump_epoch(&tx,user)?;
        audit(&tx,user,"mfa.passkey_removed",None)?;
        stamp_self(&tx,since,user)?;
        let result = issue_mfa_result(&tx,user,Vec::new(),issue)?;
        tx.commit().map_err(|e|e.to_string())?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, UserDb, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("users.db")).unwrap();
        let user = db.create_user("passkey@example.test", "Passkey", "password-test", UserRole::Admin).unwrap();
        (dir, db, user.id)
    }
    #[test]
    fn passkey_origin_and_counter_fail_closed() {
        for origin in ["http://example.com","http://localhost.evil.com","https://a.example/path","https://user@a.example","https://a.example/?x=1"] {
            assert!(relying_party(origin).is_err(), "{origin}");
        }
        assert!(relying_party("https://dashboard.example.com").is_ok());
        assert!(relying_party("http://localhost:18889").is_ok());
        assert!(counter_valid(0,0));
        assert!(counter_valid(0,1));
        assert!(counter_valid(5,6));
        assert!(!counter_valid(5,5));
        assert!(!counter_valid(5,4));
        assert!(!counter_valid(5,0));
    }
    #[test]
    fn passkey_registration_binds_session_origin_epoch_and_consumes_failure() {
        let (_dir,db,user)=setup();
        let token=db.mfa_factor_password_challenge(&user,"password-test","session",3000).unwrap();
        let begin=db.mfa_passkey_register_begin(&user,&token,Some("session"),"https://dashboard.example.com","Laptop",3000,false).unwrap();
        let id=begin["ceremony_id"].as_str().unwrap();
        assert_eq!(begin["publicKey"]["authenticatorSelection"]["userVerification"],"required");
        let finish=|session,origin,now| db.mfa_passkey_register_finish(id,&token,Some(session),origin,json!({}),now,false,|_,_,_|Ok(()));
        assert!(finish("other","https://dashboard.example.com",3000).is_err());
        assert!(db.mfa_passkey_register_begin(&user,&token,None,"https://dashboard.example.com","Other",3000,false).is_err());
        assert!(finish("session","https://other.example.com",3000).is_err());
        assert!(finish("session","https://dashboard.example.com",3300).is_err());
        assert!(finish("session","https://dashboard.example.com",3000).is_err());
        assert_eq!(db.conn().query_row("SELECT count(*) FROM mfa_passkey_ceremonies",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert!(!db.mfa_active(&user).unwrap());
        let begin=db.mfa_passkey_register_begin(&user,&token,Some("session"),"https://dashboard.example.com","Laptop",3000,false).unwrap();
        db.update_user(&user,None,None,Some("new-password")).unwrap();
        assert!(db.mfa_passkey_register_finish(begin["ceremony_id"].as_str().unwrap(),&token,Some("session"),"https://dashboard.example.com",json!({}),3000,false,|_,_,_|Ok(())).is_err());
    }
    #[test]
    fn passkey_forced_first_enrollment_requires_independent_proof() {
        let (_dir,db,user)=setup();
        let token=db.mfa_factor_password_challenge(&user,"password-test","session",3000).unwrap();
        record_enforcement(&mut db.conn(),true,3000).unwrap();
        for personal in [false, true] {
            assert!(db.mfa_passkey_register_begin(&user,&token,Some("session"),"https://dashboard.example.com","Laptop",3000,personal).is_err());
        }
    }
}

#[cfg(test)]
pub(crate) mod ceremony_tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    // A small test authenticator emits signed protocol messages. All verification
    // still runs through the production webauthn-rs library, never this fixture.
    fn bytes(value: &[u8]) -> Vec<u8> {
        let mut encoded = if value.len()<24 { vec![0x40 | value.len() as u8] }
            else if value.len()<256 { vec![0x58,value.len() as u8] }
            else { let length=(value.len() as u16).to_be_bytes(); vec![0x59,length[0],length[1]] };
        encoded.extend_from_slice(value);
        encoded
    }
    pub(crate) const ORIGIN: &str = "https://dashboard.example.com";
    pub(crate) struct Authenticator { key: EcdsaKeyPair, id: Vec<u8> }
    impl Authenticator {
        pub(crate) fn new() -> Self {
            let rng=SystemRandom::new();
            let document=EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING,&rng).unwrap();
            Self {key:EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING,document.as_ref(),&rng).unwrap(),id:vec![42;32]}
        }
        fn data(&self, counter:u32, flags:u8)->Vec<u8> {
            let mut value=ring::digest::digest(&ring::digest::SHA256,b"dashboard.example.com").as_ref().to_vec();
            value.push(flags);value.extend_from_slice(&counter.to_be_bytes());value
        }
        fn client(challenge:&Value,kind:&str)->Vec<u8> {
            serde_json::to_vec(&json!({"type":kind,"challenge":challenge["publicKey"]["challenge"],"origin":ORIGIN,"crossOrigin":false})).unwrap()
        }
        pub(crate) fn registration(&self,challenge:&Value)->Value {
            let mut data=self.data(0,0x45);
            data.extend_from_slice(&[0;16]);data.extend_from_slice(&(self.id.len() as u16).to_be_bytes());data.extend_from_slice(&self.id);
            let public=self.key.public_key().as_ref();
            // COSE EC2 / ES256 / P-256, x and y from the uncompressed public key.
            data.extend_from_slice(&[0xa5,0x01,0x02,0x03,0x26,0x20,0x01,0x21]);
            data.extend(bytes(&public[1..33]));data.push(0x22);data.extend(bytes(&public[33..65]));
            let mut attestation=vec![0xa3,0x63,b'f',b'm',b't',0x64,b'n',b'o',b'n',b'e',0x67,b'a',b't',b't',b'S',b't',b'm',b't',0xa0,0x68,b'a',b'u',b't',b'h',b'D',b'a',b't',b'a'];
            attestation.extend(bytes(&data));
            json!({"id":URL_SAFE_NO_PAD.encode(&self.id),"rawId":URL_SAFE_NO_PAD.encode(&self.id),"type":"public-key","extensions":{},
                "response":{"attestationObject":URL_SAFE_NO_PAD.encode(attestation),"clientDataJSON":URL_SAFE_NO_PAD.encode(Self::client(challenge,"webauthn.create")),"transports":["internal"]}})
        }
        pub(crate) fn assertion(&self,challenge:&Value,counter:u32,verified:bool)->Value {
            let data=self.data(counter,if verified {0x05}else{0x01});
            let client=Self::client(challenge,"webauthn.get");
            let mut signed=data.clone();signed.extend_from_slice(ring::digest::digest(&ring::digest::SHA256,&client).as_ref());
            let signature=self.key.sign(&SystemRandom::new(),&signed).unwrap();
            json!({"id":URL_SAFE_NO_PAD.encode(&self.id),"rawId":URL_SAFE_NO_PAD.encode(&self.id),"type":"public-key","extensions":{},
                "response":{"authenticatorData":URL_SAFE_NO_PAD.encode(data),"clientDataJSON":URL_SAFE_NO_PAD.encode(client),"signature":URL_SAFE_NO_PAD.encode(signature.as_ref()),"userHandle":null}})
        }
    }
    /// Register one passkey for `user` (password `password`) through the
    /// real API at `now`; returns the authenticator that holds its key.
    pub(crate) fn register(db: &UserDb, user: &str, password: &str, now: i64) -> Authenticator {
        let key=Authenticator::new();
        let token=db.mfa_factor_password_challenge(user,password,"session",now).unwrap();
        let start=db.mfa_passkey_register_begin(user,&token,Some("session"),ORIGIN,"Laptop",now,false).unwrap();
        db.mfa_passkey_register_finish(start["ceremony_id"].as_str().unwrap(),&token,Some("session"),ORIGIN,key.registration(&start),now,false,|_,_,_|Ok(())).unwrap();
        key
    }

    #[test]
    fn passkey_signed_ceremony_uv_replay_counter_and_revocation() {
        let dir=tempfile::tempdir().unwrap();
        let db=UserDb::new(&dir.path().join("users.db")).unwrap();
        let user=db.create_user("passkey@example.test","Key","password-test",UserRole::Admin).unwrap();
        let key=Authenticator::new();
        let token=db.mfa_factor_password_challenge(&user.id,"password-test","session",3000).unwrap();
        let start=db.mfa_passkey_register_begin(&user.id,&token,Some("session"),ORIGIN,"Laptop",3000,false).unwrap();
        let response=key.registration(&start);
        db.mfa_passkey_register_finish(start["ceremony_id"].as_str().unwrap(),&token,Some("session"),ORIGIN,response.clone(),3000,false,|_,_,_|Ok(())).unwrap();
        assert!(db.mfa_active(&user.id).unwrap());
        assert!(db.mfa_passkey_register_finish(start["ceremony_id"].as_str().unwrap(),&token,Some("session"),ORIGIN,response,3000,false,|_,_,_|Ok(())).is_err());
        for (counter,uv,allowed) in [(1,true,true),(1,true,false),(2,false,false),(2,true,true),(0,true,false)] {
            let token=db.mfa_challenge(&user.id,FirstFactor::Password,false,3030).unwrap();
            let begin=db.mfa_passkey_login_begin(&token,ORIGIN,3030).unwrap();
            let response=key.assertion(&begin,counter,uv);
            let id=begin["ceremony_id"].as_str().unwrap();
            assert_eq!(db.mfa_passkey_login_finish(id,&token,ORIGIN,response.clone(),3030,|_,_,_|Ok(())).is_ok(),allowed);
            assert!(db.mfa_passkey_login_finish(id,&token,ORIGIN,response,3030,|_,_,_|Ok(())).is_err());
        }
        crate::mfa::issuance_tests::assert_enrolled_refresh_under_policy(&db,&user.id,3040);
        let reauth=db.mfa_factor_password_challenge(&user.id,"password-test","session",3041).unwrap();
        let credential:String=db.conn().query_row("SELECT id FROM mfa_passkeys WHERE user_id=?1", [&user.id], |r| r.get(0)).unwrap();
        for personal in [false, true] {
            assert!(db.mfa_passkey_delete(&user.id,&credential,&reauth,"session",3041,personal,
                |_,_,_| -> Result<()> { panic!("last-factor removal issued tokens") }).is_err());
        }
        let token=db.mfa_challenge(&user.id,FirstFactor::Password,false,3060).unwrap();
        let begin=db.mfa_passkey_login_begin(&token,ORIGIN,3060).unwrap();
        db.mfa_operator_reset(&user.email).unwrap();
        assert!(db.mfa_passkeys(&user.id).unwrap().is_empty());
        assert!(db.mfa_passkey_login_finish(begin["ceremony_id"].as_str().unwrap(),&token,ORIGIN,key.assertion(&begin,3,true),3060,|_,_,_|Ok(())).is_err());
    }
}
