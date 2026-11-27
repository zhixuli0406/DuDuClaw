//! Session-bound self-service OIDC tickets and one-use MFA grants.
use super::*;
use duduclaw_auth::User;

const GRANT_TTL: Duration = Duration::from_secs(300);
const MAX_GRANTS: usize = 256;
static GRANTS: LazyLock<Mutex<HashMap<(PathBuf, String, String, u64), Instant>>> = LazyLock::new(Default::default);

pub(crate) fn live_user(db: &UserDb, binding: &SessionBinding) -> Result<User, &'static str> {
    if binding.session_id.is_empty() { return Err("signin_dashboard_required"); }
    if !db.remember_session_valid(Some(&binding.session_id), &binding.user_id, chrono::Utc::now().timestamp())
        .map_err(|_| "signin_storage_unavailable")? { return Err("signin_session_expired"); }
    db.get_user(&binding.user_id).map_err(|_| "signin_storage_unavailable")?
        .filter(|u| u.status == UserStatus::Active && u.credential_epoch == binding.epoch && !u.must_change_password)
        .ok_or("signin_session_expired")
}

pub(crate) fn available(home: &Path) -> bool { configuration(home).is_ok() }
fn configuration(home: &Path) -> Result<Available, &'static str> {
    let settings = config::read_strict(home);
    if !settings.enabled { return Err("signin_unavailable"); }
    let client = configured_client(home).ok_or("signin_unavailable")?;
    let (base, secure) = public_base(home).ok_or("signin_unavailable")?;
    Ok(Available { settings, client, base, secure })
}

fn personal_sub(home: &Path, user: &str) -> Result<Option<String>, &'static str> {
    crate::person_credentials::get(home, user)
        .map(|r| r.map(|r| r.metadata.google_sub)).map_err(|_| "signin_storage_unavailable")
}
fn same_personal_sub(personal: Option<&str>, sub: &str) -> Result<(), &'static str> {
    if personal.is_some_and(|p| p != sub) { Err("signin_personal_sub_mismatch") } else { Ok(()) }
}

/// Called on the blocking pool, with identity captured before leaving task-local scope.
pub(crate) fn start(home: &Path, db: &UserDb, binding: SessionBinding, reauth: bool, password: &str) -> Result<String, &'static str> {
    let user = live_user(db, &binding)?;
    let available = configuration(home)?;
    let link = db.google_signin_link_for_user(&user.id).map_err(|_| "signin_storage_unavailable")?;
    let credential_revision = if reauth { None } else {
        Some(db.google_signin_credential_revision(&user.id).map_err(|_| "signin_storage_unavailable")?)
    };
    let hint = if reauth {
        require_passwordless(db, &user.id)?;
        link.ok_or("signin_not_linked")?.email_at_link
    } else {
        db.google_signin_verify_password(&user.id, binding.epoch, password)?;
        let personal = personal_sub(home, &user.id)?;
        if let Some(link) = link {
            same_personal_sub(personal.as_deref(), &link.google_sub)?;
            return Err("signin_already_linked");
        }
        user.email
    };
    let (state_value, nonce, verifier) = (oidc::random_state().map_err(|_| "signin_unavailable")?,
        oidc::random_nonce().map_err(|_| "signin_unavailable")?, oidc::random_pkce_verifier().map_err(|_| "signin_unavailable")?);
    let redirect_uri = redirect_uri(&available.base);
    let url = oidc::authorization_url(&available.client.client_id, &redirect_uri, &state_value, &nonce,
        &oidc::pkce_challenge_s256(&verifier), &available.settings.allowed_hd,
        if reauth { oidc::SigninPurpose::Reauth } else { oidc::SigninPurpose::Login }, Some(&hint));
    let mut map = PENDING.lock().map_err(|_| "signin_unavailable")?;
    map.retain(|_, p| p.created.elapsed() < STATE_TTL);
    if map.len() >= MAX_PENDING || map.contains_key(&hash(&state_value))
        || map.values().filter(|p| p.home == home && p.purpose.binding().is_some_and(|b| b.user_id == binding.user_id)).count() >= MAX_PENDING_PER_IP {
        return Err("signin_rate_limited");
    }
    map.insert(hash(&state_value), Pending {
        credential_revision,
        purpose: if reauth { Purpose::Reauth(binding) } else { Purpose::Link(binding) },
        home: home.into(), ip: IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), cookie_sha256: [0; 32],
        nonce_sha256: hash(&nonce), verifier, redirect_uri, client_id: available.client.client_id,
        settings_revision: available.settings.revision, secure: available.secure, created: Instant::now(),
    });
    Ok(url)
}

fn require_passwordless(db: &UserDb, user: &str) -> Result<(), &'static str> {
    if db.has_local_password(user).map_err(|_| "signin_storage_unavailable")? {
        return Err("signin_reauth_password_account");
    }
    Ok(())
}
fn reauth_identity(link_sub: &str, token: &oidc::VerifiedIdToken, now: i64) -> Result<(), &'static str> {
    if token.sub != link_sub { return Err("signin_reauth_sub_mismatch"); }
    if !oidc::reauth_fresh(token, now) { return Err("signin_reauth_unavailable"); }
    Ok(())
}
fn claim_ticket(home: &Path, binding: &SessionBinding, value: &str, reauth: bool) -> Result<(oidc::VerifiedIdToken, Option<i64>), &'static str> {
    let ticket = take_ticket(value).ok_or("signin_session_expired")?;
    let expected = if reauth { Purpose::Reauth(binding.clone()) } else { Purpose::Link(binding.clone()) };
    if ticket.home != home || ticket.purpose != expected { return Err("signin_session_expired"); }
    match ticket.result {
        LoginResult::Identity(identity) => Ok((identity, ticket.credential_revision)),
        LoginResult::Code("personal_account") => Err("signin_personal_account"),
        LoginResult::Code("domain_not_allowed") => Err("signin_domain_not_allowed"),
        _ => Err("signin_session_expired"),
    }
}

/// The caller holds the personal Google grant lease during link completion.
pub(crate) fn complete(home: &Path, db: &UserDb, binding: &SessionBinding, value: &str, reauth: bool, now: i64) -> Result<(), &'static str> {
    let (identity, revision) = claim_ticket(home, binding, value, reauth)?;
    live_user(db, binding)?;
    let config = configuration(home)?;
    if !config.settings.allowed_hd.contains(&identity.hd) { return Err("signin_domain_not_allowed"); }
    if reauth {
        require_passwordless(db, &binding.user_id)?;
        let link = db.google_signin_link_for_user(&binding.user_id).map_err(|_| "signin_storage_unavailable")?.ok_or("signin_not_linked")?;
        reauth_identity(&link.google_sub, &identity, now)?;
        put_grant(home, binding)
    } else {
        same_personal_sub(personal_sub(home, &binding.user_id)?.as_deref(), &identity.sub)?;
        db.self_link_google_signin(&binding.user_id, binding.epoch, revision.ok_or("signin_session_expired")?, &VerifiedGoogleIdentity {
            sub: identity.sub, email: identity.email, hd: identity.hd, display_name: identity.name,
        }, now)
    }
}
fn grant_key(home: &Path, b: &SessionBinding) -> (PathBuf, String, String, u64) {
    (home.into(), b.user_id.clone(), b.session_id.clone(), b.epoch)
}
fn put_grant(home: &Path, b: &SessionBinding) -> Result<(), &'static str> {
    let mut map = GRANTS.lock().map_err(|_| "signin_unavailable")?;
    map.retain(|_, at| at.elapsed() < GRANT_TTL);
    let key = grant_key(home, b);
    if map.len() >= MAX_GRANTS && !map.contains_key(&key) { return Err("signin_rate_limited"); }
    map.insert(key, Instant::now());
    Ok(())
}
fn take_grant(home: &Path, b: &SessionBinding) -> bool {
    GRANTS.lock().ok().and_then(|mut map| map.remove(&grant_key(home, b)))
        .is_some_and(|at| at.elapsed() < GRANT_TTL)
}

/// Consume a passwordless step-up grant without creating an MFA challenge.
pub(crate) fn consume_grant(home: &Path, db: &UserDb, user: &str, session: &str, epoch: u64) -> Result<(), String> {
    let binding = SessionBinding { user_id: user.into(), session_id: session.into(), epoch };
    live_user(db, &binding).map_err(|_| "登入憑證已失效，請重新登入。")?;
    if db.has_local_password(user)? || db.google_signin_link_for_user(user)?.is_none() || !take_grant(home, &binding) {
        return Err("請先用公司 Google 帳號重新驗證身分；無法驗證時，請管理員重設兩步驟驗證。".into());
    }
    Ok(())
}

/// Blocking-only MFA adapter. Password accounts cannot use or consume a grant.
pub(crate) fn challenge(home: &Path, db: &UserDb, user: &str, session: &str, epoch: u64, password: &str, now: i64, factor: bool) -> Result<String, String> {
    if db.has_local_password(user)? {
        if factor { db.mfa_factor_password_challenge(user, password, session, now) }
        else { db.mfa_password_challenge(user, password, session, now) }
    } else {
        consume_grant(home, db, user, session, epoch)?;
        if factor { db.mfa_factor_reauth_challenge(user, session, epoch, now) }
        else { db.mfa_reauth_challenge(user, session, epoch, now) }
    }
}

/// Proof retries authenticate again, but keep their independent enrollment proof.
pub(crate) fn retry(home: &Path, db: &UserDb, user: &str, session: &str, epoch: u64, password: &str, token: &str, now: i64) -> Result<(), String> {
    if db.has_local_password(user)? {
        let account = db.get_user(user)?.ok_or("無法讀取帳號資料。")?;
        db.verify_password(&account.email, password)?;
    } else {
        let binding = SessionBinding { user_id: user.into(), session_id: session.into(), epoch };
        live_user(db, &binding).map_err(|_| "登入憑證已失效，請重新登入。")?;
        if db.google_signin_link_for_user(user)?.is_none() || !take_grant(home, &binding) {
            return Err("請先用公司 Google 帳號重新驗證身分。".into());
        }
    }
    db.mfa_check_session(token, user, session, now)
}

#[cfg(test)]
#[path = "routes_me_tests.rs"]
mod tests;
