//! Authenticated, one-use ingress identity. No environment or queue-row fallback.
use std::{future::Future, path::{Path, PathBuf}, sync::Mutex};
use duduclaw_auth::jwt::Claims;
use duduclaw_knowledge::requester::RequesterIdentity;
use crate::webchat::ChatCredential;
use super::{RuntimeAuthorization, VerificationError};

#[derive(Clone)]
enum Source {
    Web(ChatCredential),
    Channel { channel: String, sender: String },
}
struct HumanIngress {
    home: PathBuf,
    user: String,
    epoch: u64,
    source: Source,
}
tokio::task_local! {
    static WEB_CREDENTIAL: Option<ChatCredential>;
    static INGRESS: Mutex<Option<HumanIngress>>;
}

/// Called only after the WebChat socket and this queued turn revalidate the credential.
/// The credential already crossed the socket-task boundary in TurnInput.
pub(crate) async fn webchat_scope<F: Future>(credential: Option<ChatCredential>, future: F) -> F::Output {
    if WEB_CREDENTIAL.try_with(|_| ()).is_ok() { return future.await; }
    WEB_CREDENTIAL.scope(credential, future).await
}

/// Capture the channel credential epoch before the reply pipeline does any
/// asynchronous model/routing work. A mapped requester alone is insufficient:
/// authorize_current additionally requires the adapter's live attended proof.
pub(crate) async fn reply_scope<F: Future>(home: &Path, requester: &RequesterIdentity, future: F) -> F::Output {
    if INGRESS.try_with(|_| ()).is_ok() { return future.await; }
    if !duduclaw_core::execution_gate::isolation_required() { return future.await; }
    protected_reply_scope(home, requester, future).await
}

async fn protected_reply_scope<F: Future>(home: &Path, requester: &RequesterIdentity, future: F) -> F::Output {
    let web = WEB_CREDENTIAL.try_with(Clone::clone).ok().flatten();
    let (home, requester) = (home.to_owned(), requester.clone());
    let captured = duduclaw_mcp_boundary::authority::verify_blocking(move || {
        let (user, source, expected_epoch) = match requester {
            RequesterIdentity::DashboardUser { user_id } => {
                let credential = web.ok_or(VerificationError::Invalid)?;
                let epoch = check_web_identity(&home, &credential, &user_id,
                    duduclaw_mcp_boundary::authority::now_seconds()?)?.epoch;
                (user_id, Source::Web(credential), Some(epoch))
            }
            RequesterIdentity::ChannelUser { channel, sender, user_id: Some(user), .. } => {
                if web.is_some() { return Err(VerificationError::Revoked); }
                check_channel_mapping(&user, crate::trigger_route::access::verified_user_for_channel_sender(
                    &home, &channel, &sender).as_deref())?;
                (user, Source::Channel { channel, sender }, None)
            }
            _ => return Err(VerificationError::Invalid),
        };
        let db = crate::decision_notify::open_user_db(&home)
            .map_err(|_| VerificationError::Unavailable)?.ok_or(VerificationError::Unavailable)?;
        let context = db.live_user_context(&user).map_err(|_| VerificationError::Unavailable)?
            .ok_or(VerificationError::Revoked)?;
        if let Some(epoch) = expected_epoch { require_epoch(epoch, Some(context.credential_epoch))?; }
        Ok(HumanIngress { home, user, epoch: context.credential_epoch, source })
    }).await.ok();
    // A failed capture is a consumed/absent identity, never background authority.
    INGRESS.scope(Mutex::new(captured), future).await
}

pub(super) async fn authorize_current(agent_dir: &Path, principal_context: Option<String>)
    -> Result<RuntimeAuthorization, VerificationError> {
    let human = take_human()?;
    let agent = agent_dir.file_name().and_then(|name| name.to_str()).ok_or(VerificationError::Invalid)?.to_owned();
    if agent_dir != human.home.join("agents").join(&agent) { return Err(VerificationError::BindingMismatch); }
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).ok().flatten()
        .ok_or(VerificationError::Unavailable)?;
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(Clone::clone).ok().flatten()
        .ok_or(VerificationError::Unavailable)?;
    let proof = crate::computer_use_sessions::attended::capture(&agent, &turn)
        .ok_or(VerificationError::Unavailable)?;
    if !proof.valid_for(&agent, &turn) { return Err(VerificationError::Revoked); }
    // A fresh human message must not launder a nested/background dispatch.
    if crate::claude_runner::DELEGATION_ENV.try_with(|_| ()).is_ok()
        || crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten().is_some()
        || crate::runtime::round_task_env().is_some() {
        return Err(VerificationError::BindingMismatch);
    }
    let expected = duduclaw_core::HumanPrincipal::user(&human.user)
        .ok_or(VerificationError::Invalid)?.to_string();
    if crate::on_behalf_of::current().as_deref() != Some(expected.as_str()) {
        return Err(VerificationError::BindingMismatch);
    }
    let metadata = crate::isolation_egress::RunMetadata {
        reply_channel: crate::claude_runner::REPLY_CHANNEL.try_with(Clone::clone).ok(),
        reply_sender: crate::claude_runner::REPLY_SENDER.try_with(Clone::clone).ok(),
        user_message: crate::memory_provenance::TURN_USER_MESSAGE.try_with(Clone::clone).ok().flatten(),
        ingress: Some(match &human.source {
            Source::Web(_) => crate::isolation_egress::IngressRequester::Dashboard,
            Source::Channel { channel, sender } => crate::isolation_egress::IngressRequester::Channel {
                channel: channel.clone(), sender: sender.clone() },
        }),
        ..crate::isolation_egress::RunMetadata::root()
    };
    let lane = if crate::explore_lane::in_explore() { duduclaw_isolation::capability::Lane::Explore }
        else { duduclaw_isolation::capability::Lane::Standard };
    let egress_turn = crate::runtime_turn::current();
    duduclaw_mcp_boundary::authority::verify_blocking(move || {
        if !proof.valid_for(&agent, &turn) { return Err(VerificationError::Revoked); }
        let (expiry, generation) = match &human.source {
            Source::Web(credential) => {
                let live = check_web_identity(&human.home, credential, &human.user,
                    duduclaw_mcp_boundary::authority::now_seconds()?)?;
                require_epoch(human.epoch, Some(live.epoch))?;
                (live.expiry, live.generation)
            }
            Source::Channel { channel, sender } => {
                check_channel_mapping(&human.user,
                    crate::trigger_route::access::verified_user_for_channel_sender(&human.home, channel, sender).as_deref())?;
                (None, None)
            }
        };
        super::boot::record_authorization_at_generation(human.home, agent, human.user, human.epoch, lane,
            super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
                turn, approval_session: Some(session) }, metadata: Some(metadata), principal_context,
                egress_turn, background: None }, expiry, None, generation)
    }).await
}

fn take_human() -> Result<HumanIngress, VerificationError> {
    INGRESS.try_with(|slot| slot.lock().ok().and_then(|mut slot| slot.take()))
        .ok().flatten().ok_or(VerificationError::Unavailable)
}

/// Re-read authority at capture and again before issuance; a cached user alone
/// must never turn a revoked member key into a human ingress credential.
struct WebIdentity {
    epoch: u64,
    expiry: Option<u64>,
    generation: Option<u64>,
}
fn check_web_identity(home: &Path, credential: &ChatCredential, user: &str, now: u64)
    -> Result<WebIdentity, VerificationError> {
    if user.is_empty() { return Err(VerificationError::Revoked); }
    let db = crate::decision_notify::open_user_db(home)
        .map_err(|_| VerificationError::Unavailable)?.ok_or(VerificationError::Unavailable)?;
    match credential {
        ChatCredential::Jwt(claims) => {
            check_jwt_identity(claims, user, now)?;
            let live = db.live_user_context(user).map_err(|_| VerificationError::Unavailable)?
                .ok_or(VerificationError::Revoked)?;
            require_epoch(claims.credential_epoch, Some(live.credential_epoch))?;
            Ok(WebIdentity { epoch: live.credential_epoch, expiry: Some(claims.exp), generation: None })
        }
        ChatCredential::Member(ctx) => {
            if ctx.user_id != user { return Err(VerificationError::Revoked); }
            let live = crate::member_keys::refresh_authority(home, &db, ctx)
                .map_err(|_| VerificationError::Revoked)?;
            Ok(WebIdentity { epoch: live.context.credential_epoch, expiry: Some(live.expires_at),
                generation: Some(live.context.acl_generation) })
        }
    }
}

fn check_jwt_identity(claims: &Claims, user: &str, now: u64) -> Result<(), VerificationError> {
    if claims.sub != user || user.is_empty() || claims.token_type != "access" || claims.exp <= now {
        return Err(VerificationError::Revoked);
    }
    Ok(())
}
fn check_channel_mapping(original: &str, current: Option<&str>) -> Result<(), VerificationError> {
    if original.is_empty() || current != Some(original) { return Err(VerificationError::Revoked); }
    Ok(())
}
pub(super) fn require_epoch(original: u64, current: Option<u64>) -> Result<(), VerificationError> {
    if current != Some(original) { return Err(VerificationError::Revoked); }
    Ok(())
}

pub(super) fn own_wiki_root(home: &Path, agent: &str) -> Result<Option<std::fs::File>, VerificationError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let directory = own_agent_dir(home, agent)?;
    match openat(Some(directory.as_raw_fd()), "wiki", flags, Mode::empty()) {
        Ok(fd) => Ok(Some(unsafe { std::fs::File::from_raw_fd(fd) })),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(_) => Err(VerificationError::Unavailable),
    }
}

/// `<home>/agents/<agent>` (gateway-private state, never the employee mount),
/// walked from `/` without following a symlink at any component. Shared by
/// the wiki root and the H9 issuer selectors' profile read.
pub(super) fn own_agent_dir(home: &Path, agent: &str) -> Result<std::fs::File, VerificationError> {
    use std::os::fd::FromRawFd;
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::Mode;
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let root = open(Path::new("/"), flags, Mode::empty()).map_err(|_| VerificationError::Unavailable)?;
    // SAFETY: each successful open/openat returns a new owned descriptor.
    let mut directory = unsafe { std::fs::File::from_raw_fd(root) };
    use std::os::fd::AsRawFd;
    if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(agent) { return Err(VerificationError::Invalid); }
    for part in home.components().chain(Path::new("agents").components()).chain(Path::new(agent).components()) {
        let std::path::Component::Normal(part) = part else {
            if part == std::path::Component::RootDir { continue; }
            return Err(VerificationError::Invalid);
        };
        let fd = openat(Some(directory.as_raw_fd()), Path::new(part), flags, Mode::empty())
            .map_err(|_| VerificationError::Unavailable)?;
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn web_fixture() -> (tempfile::TempDir, duduclaw_auth::UserDb, Claims) {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[member_api_keys]\nenabled=true\n").unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("ingress@example.invalid", "Ingress", "test-password",
            duduclaw_auth::UserRole::Employee).unwrap();
        let now = duduclaw_mcp_boundary::authority::now_seconds().unwrap();
        let claims = serde_json::from_value(serde_json::json!({
            "sub":user.id, "email":user.email, "role":"employee",
            "bound_agents":[], "access_levels":{}, "exp":now + 3600, "iat":now,
            "token_type":"access", "credential_epoch":user.credential_epoch
        })).unwrap();
        (home, db, claims)
    }
    // Exercise the production protected branch without setting the process-wide,
    // irreversible isolation latch (which would affect unrelated gateway tests).
    async fn capture_web(home: &Path, user: &str, credential: Option<ChatCredential>) -> Option<HumanIngress> {
        webchat_scope(credential, async {
            protected_reply_scope(home, &RequesterIdentity::DashboardUser { user_id: user.into() }, async {
                let human = take_human().ok();
                assert!(take_human().is_err());
                human
            }).await
        }).await
    }
    #[tokio::test]
    async fn ingress_jwt_scope_preserves_user_and_refuses_absent_revoked_or_retargeted_credentials() {
        let (home, _db, claims) = web_fixture();
        let credential = ChatCredential::Jwt(claims.clone());
        let human = capture_web(home.path(), &claims.sub, Some(credential.clone())).await.unwrap();
        assert_eq!(human.user, claims.sub);
        assert_eq!(human.epoch, claims.credential_epoch);
        assert!(matches!(human.source, Source::Web(ChatCredential::Jwt(_))));
        assert!(capture_web(home.path(), &claims.sub, None).await.is_none());
        assert!(capture_web(home.path(), "another-user", Some(credential.clone())).await.is_none());
        let raw = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        raw.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&claims.sub]).unwrap();
        assert!(check_web_identity(home.path(), &credential, &claims.sub,
            duduclaw_mcp_boundary::authority::now_seconds().unwrap()).is_err());
        assert!(capture_web(home.path(), &claims.sub, Some(credential)).await.is_none());
        let mut current = claims;
        current.credential_epoch += 1;
        raw.execute("UPDATE users SET status='suspended' WHERE id=?1", [&current.sub]).unwrap();
        assert!(capture_web(home.path(), &current.sub, Some(ChatCredential::Jwt(current.clone()))).await.is_none());
        let absent_home = tempfile::tempdir().unwrap();
        assert!(capture_web(absent_home.path(), &current.sub, Some(ChatCredential::Jwt(current.clone()))).await.is_none());
    }
    #[tokio::test]
    async fn ingress_member_key_scope_rechecks_the_original_key_at_verification() {
        for reason in ["revoke", "delete", "expiry", "malformed_expiry", "epoch", "account", "admin", "switch"] {
            let (home, db, claims) = web_fixture();
            let key = db.member_key_create(&claims.sub, "ingress", 1, claims.credential_epoch, false, &[]).unwrap();
            let ctx = crate::member_keys::authenticate(home.path(), &db, &key.secret).unwrap();
            let credential = ChatCredential::Member(ctx.clone());
            let human = capture_web(home.path(), &claims.sub, Some(credential.clone())).await.unwrap();
            assert_eq!(human.user, claims.sub);
            assert_eq!(human.epoch, claims.credential_epoch);
            let Source::Web(original) = human.source else { panic!("expected web credential") };
            assert!(matches!(&original, ChatCredential::Member(ctx) if ctx.member_key_id.as_deref() == Some(key.info.id.as_str())));
            assert!(capture_web(home.path(), "another-user", Some(credential.clone())).await.is_none());
            let mut missing_key = ctx;
            missing_key.member_key_id = None;
            assert!(capture_web(home.path(), &claims.sub, Some(ChatCredential::Member(missing_key))).await.is_none());
            let raw = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            match reason {
                "revoke" => db.member_key_revoke_own(&claims.sub, &key.info.id).unwrap(),
                "delete" => { raw.execute("DELETE FROM member_api_keys WHERE id=?1", [&key.info.id]).unwrap(); }
                "expiry" => { raw.execute("UPDATE member_api_keys SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1", [&key.info.id]).unwrap(); }
                "malformed_expiry" => { raw.execute("UPDATE member_api_keys SET expires_at='2999-01-01' WHERE id=?1", [&key.info.id]).unwrap(); }
                "epoch" => { raw.execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&claims.sub]).unwrap(); }
                "account" => { raw.execute("UPDATE users SET status='suspended' WHERE id=?1", [&claims.sub]).unwrap(); }
                "admin" => { raw.execute("UPDATE users SET role='admin' WHERE id=?1", [&claims.sub]).unwrap(); }
                "switch" => std::fs::write(home.path().join("config.toml"), "broken [").unwrap(),
                _ => unreachable!(),
            }
            // The same verifier is called by authorize_current after capture.
            assert!(check_web_identity(home.path(), &original, &human.user,
                duduclaw_mcp_boundary::authority::now_seconds().unwrap()).is_err(), "{reason}");
            assert!(capture_web(home.path(), &claims.sub, Some(credential)).await.is_none(), "{reason}");
        }
    }
    fn issuer_fixture() -> (tempfile::TempDir, duduclaw_auth::UserDb, super::super::boot::test_support::SharedEmployee) {
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::write(home.path().join("config.toml"), "[member_api_keys]\nenabled=true\n").unwrap();
        let agent = home.path().join("agents/ingress");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"),
            super::super::boot::test_support::agent_toml("ingress", false, "")).unwrap();
        let employee = super::super::boot::test_support::SharedEmployee::new_personal(
            home.path(), "ingress", duduclaw_isolation::ids::EmployeeId([23; 16]));
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        (home, db, employee)
    }
    fn issue_web(employee: &super::super::boot::test_support::SharedEmployee, live: &WebIdentity)
        -> Result<RuntimeAuthorization, VerificationError> {
        // Same first-snapshot gate as record_authorization_at_generation; only
        // the root allocator is doubled by the existing issuer fixture.
        let current = employee.snapshot();
        super::super::boot::check_issuance_credential(live.epoch, live.generation, &current)?;
        employee.authorize(&current, duduclaw_isolation::capability::Lane::Standard,
            duduclaw_isolation::bounded::List::new(vec![
                duduclaw_isolation::bounded::Text::new("tasks_update".to_owned()).unwrap()]).unwrap(),
            super::super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
                turn: "ingress-expiry".into(), approval_session: None }, metadata: None,
                principal_context: None, egress_turn: None, background: None }, live.expiry)
    }
    #[test]
    fn ingress_member_key_grant_is_capped_by_key_expiry() {
        let (home, db, employee) = issuer_fixture();
        let key = db.member_key_create(employee.user(), "ingress", 1, employee.epoch(), false, &[]).unwrap();
        let credential = ChatCredential::Member(crate::member_keys::authenticate(home.path(), &db, &key.secret).unwrap());
        let now = duduclaw_mcp_boundary::authority::now_seconds().unwrap();
        // Set the expiry explicitly; no sleeps or dependence on creation TTL.
        let expiry = now + 300;
        let stamp = chrono::DateTime::<chrono::Utc>::from_timestamp(i64::try_from(expiry).unwrap(), 0)
            .unwrap().to_rfc3339();
        rusqlite::Connection::open(home.path().join("users.db")).unwrap().execute(
            "UPDATE member_api_keys SET expires_at=?1 WHERE id=?2", rusqlite::params![stamp, key.info.id]).unwrap();
        let live = check_web_identity(home.path(), &credential, employee.user(), now).unwrap();
        assert_eq!(live.expiry, Some(expiry));
        assert_eq!(live.generation, Some(db.acl_generation().unwrap()));
        let authorization = issue_web(&employee, &live).unwrap();
        assert_eq!(authorization.grant.expires_at, expiry);
        assert!(authorization.grant.expires_at - authorization.grant.not_before < 30 * 60);
    }
    #[test]
    fn ingress_member_key_revoke_between_refresh_and_issuer_snapshot_is_refused() {
        let (home, db, employee) = issuer_fixture();
        let key = db.member_key_create(employee.user(), "ingress", 1, employee.epoch(), false, &[]).unwrap();
        let credential = ChatCredential::Member(crate::member_keys::authenticate(home.path(), &db, &key.secret).unwrap());
        let live = check_web_identity(home.path(), &credential, employee.user(),
            duduclaw_mcp_boundary::authority::now_seconds().unwrap()).unwrap();
        // Deterministically inject the revoke after refresh and before the
        // issuer's first snapshot, when the old epoch-only gate would pass.
        db.member_key_revoke_own(employee.user(), &key.info.id).unwrap();
        let after = employee.snapshot();
        assert_eq!(after.human.as_ref().unwrap().credential_epoch, live.epoch);
        assert_ne!(Some(after.acl_generation), live.generation);
        assert!(matches!(issue_web(&employee, &live), Err(VerificationError::Revoked)));
    }
    #[test]
    fn ingress_jwt_grant_keeps_expiry_cap_and_does_not_pin_refresh_generation() {
        let (home, db, employee) = issuer_fixture();
        let now = duduclaw_mcp_boundary::authority::now_seconds().unwrap();
        for lifetime in [300, 3600] {
            let claims: Claims = serde_json::from_value(serde_json::json!({
                "sub":employee.user(), "email":"ingress@example.invalid", "role":"employee",
                "bound_agents":[], "access_levels":{}, "exp":now + lifetime, "iat":now,
                "token_type":"access", "credential_epoch":employee.epoch()
            })).unwrap();
            let live = check_web_identity(home.path(), &ChatCredential::Jwt(claims.clone()), employee.user(), now).unwrap();
            assert_eq!(live.expiry, Some(claims.exp));
            assert_eq!(live.generation, None);
            // Unrelated member-key revocation must not change JWT semantics.
            let key = db.member_key_create(employee.user(), "unrelated", 1, employee.epoch(), false, &[]).unwrap();
            db.member_key_revoke_own(employee.user(), &key.info.id).unwrap();
            let authorization = issue_web(&employee, &live).unwrap();
            assert_eq!(authorization.grant.expires_at, claims.exp.min(authorization.grant.not_before + 30 * 60));
        }
    }
    fn human() -> HumanIngress {
        HumanIngress { home: "/trusted".into(), user: "original-user".into(), epoch: 7,
            source: Source::Channel { channel: "telegram".into(), sender: "verified-sender".into() } }
    }
    #[test]
    fn ingress_original_web_identity_and_expiry_cannot_be_replaced() {
        let mut claims: Claims = serde_json::from_value(serde_json::json!({
            "sub":"original-user", "email":"test@example.invalid", "role":"employee",
            "bound_agents":[], "access_levels":{}, "exp":100, "iat":1,
            "token_type":"access", "credential_epoch":7
        })).unwrap();
        assert!(check_jwt_identity(&claims, "original-user", 99).is_ok());
        assert!(check_jwt_identity(&claims, "other-user", 99).is_err());
        assert!(check_jwt_identity(&claims, "original-user", 100).is_err());
        claims.token_type = "refresh".into();
        assert!(check_jwt_identity(&claims, "original-user", 99).is_err());
    }
    #[test]
    fn ingress_revoked_credential_and_rebound_channel_refuse() {
        assert!(require_epoch(7, Some(7)).is_ok());
        assert!(require_epoch(7, Some(8)).is_err());
        assert!(require_epoch(7, None).is_err());
        assert!(check_channel_mapping("original-user", Some("original-user")).is_ok());
        assert!(check_channel_mapping("original-user", Some("new-owner")).is_err());
        assert!(check_channel_mapping("original-user", None).is_err());
    }
    #[tokio::test]
    async fn ingress_is_one_use_and_never_implicitly_crosses_spawn() {
        assert!(take_human().is_err());
        INGRESS.scope(Mutex::new(Some(human())), async {
            assert!(tokio::spawn(async { take_human().is_err() }).await.unwrap());
            let captured = take_human().unwrap();
            assert_eq!(captured.user, "original-user");
            assert_eq!(captured.epoch, 7);
            assert!(take_human().is_err());
        }).await;
        assert!(take_human().is_err());
    }
    #[tokio::test]
    async fn ingress_wrong_home_consumes_proof_without_retargeting() {
        INGRESS.scope(Mutex::new(Some(human())), async {
            assert!(authorize_current(Path::new("/other/agents/alice"), None).await.is_err());
            assert!(take_human().is_err());
        }).await;
    }
    #[tokio::test]
    async fn ingress_unknown_or_consumed_scope_cannot_be_replaced() {
        INGRESS.scope(Mutex::new(None), async {
            reply_scope(Path::new("/trusted"), &RequesterIdentity::DashboardUser { user_id: "new-user".into() }, async {
                assert!(take_human().is_err());
            }).await;
        }).await;
    }
    #[test]
    fn ingress_wiki_root_rejects_symlink_at_every_component() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("agents/alice")).unwrap();
        assert!(own_wiki_root(&home, "alice").unwrap().is_none());
        std::fs::create_dir(home.join("agents/alice/wiki")).unwrap();
        assert!(own_wiki_root(&home, "alice").unwrap().is_some());
        symlink(home.join("agents/alice/wiki"), home.join("agents/alice/alias")).unwrap();
        std::fs::rename(home.join("agents/alice/wiki"), home.join("saved-wiki")).unwrap();
        symlink(home.join("saved-wiki"), home.join("agents/alice/wiki")).unwrap();
        assert!(own_wiki_root(&home, "alice").is_err());
        symlink(home.join("agents/alice"), home.join("agents/bob")).unwrap();
        assert!(own_wiki_root(&home, "bob").is_err());
        assert!(own_wiki_root(&home, "../alice").is_err());
    }
}
