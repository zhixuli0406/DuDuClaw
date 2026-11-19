//! Authenticated, one-use ingress identity. No environment or queue-row fallback.
use std::{future::Future, path::{Path, PathBuf}, sync::Mutex};
use duduclaw_auth::jwt::Claims;
use duduclaw_knowledge::requester::RequesterIdentity;
use super::{RuntimeAuthorization, VerificationError};
use super::refusal::{site, RefusalSite, Sited};

#[derive(Clone)]
enum Source {
    Web(Claims),
    Channel { channel: String, sender: String },
}
struct HumanIngress {
    home: PathBuf,
    user: String,
    epoch: u64,
    source: Source,
}
/// R8x: a failed capture keeps its fixed reason label (read once, by the
/// run that would have consumed the identity); it never becomes authority.
type Captured = Result<HumanIngress, RefusalSite>;
tokio::task_local! {
    static WEB_CREDENTIAL: Option<Claims>;
    static INGRESS: Mutex<Option<Captured>>;
}

/// R8x: a refusal with its site (recorded for the current protected turn).
/// F3: [`authorize_current`] returns [`Sited`], so an exit without a site
/// does not compile.
fn noted(site: RefusalSite, error: VerificationError) -> Sited<VerificationError> {
    Sited::new(site, error)
}

/// Called only after the WebChat socket and this queued turn revalidate Claims.
/// The Claims already crossed the socket-task boundary in TurnInput.
pub(crate) async fn webchat_scope<F: Future>(claims: Option<Claims>, future: F) -> F::Output {
    if WEB_CREDENTIAL.try_with(|_| ()).is_ok() { return future.await; }
    WEB_CREDENTIAL.scope(claims, future).await
}

/// Capture the channel credential epoch before the reply pipeline does any
/// asynchronous model/routing work. A mapped requester alone is insufficient:
/// authorize_current additionally requires the adapter's live attended proof.
pub(crate) async fn reply_scope<F: Future>(home: &Path, requester: &RequesterIdentity, future: F) -> F::Output {
    if INGRESS.try_with(|_| ()).is_ok() { return future.await; }
    if !duduclaw_core::execution_gate::isolation_required() { return future.await; }
    let web = WEB_CREDENTIAL.try_with(Clone::clone).ok().flatten();
    let (home, requester) = (home.to_owned(), requester.clone());
    let captured = match duduclaw_mcp_boundary::authority::verify_blocking(move ||
        Ok(capture_human(home, requester, web))).await {
        Ok(captured) => captured,
        Err(_) => Err(site!("authorization.ingress.capture.blocking")),
    };
    // A failed capture is a consumed/absent identity, never background authority.
    INGRESS.scope(Mutex::new(Some(captured)), future).await
}

/// The identity checks of [`reply_scope`]; a refusal is a fixed label only.
fn capture_human(home: PathBuf, requester: RequesterIdentity, web: Option<Claims>) -> Captured {
    let (user, source, expected_epoch) = match requester {
        RequesterIdentity::DashboardUser { user_id } => {
            let claims = web.ok_or(site!("authorization.ingress.capture.no_web_credential"))?;
            let now = duduclaw_mcp_boundary::authority::now_seconds()
                .map_err(|_| site!("authorization.ingress.capture.web_identity"))?;
            check_web_identity(&claims, &user_id, now).map_err(|_| site!("authorization.ingress.capture.web_identity"))?;
            let epoch = claims.credential_epoch;
            (user_id, Source::Web(claims), Some(epoch))
        }
        RequesterIdentity::ChannelUser { channel, sender, user_id: Some(user), .. } => {
            if web.is_some() { return Err(site!("authorization.ingress.capture.channel_conflict")); }
            check_channel_mapping(&user, crate::trigger_route::access::verified_user_for_channel_sender(
                &home, &channel, &sender).as_deref())
                .map_err(|_| site!("authorization.ingress.capture.channel_mapping"))?;
            (user, Source::Channel { channel, sender }, None)
        }
        _ => return Err(site!("authorization.ingress.capture.requester")),
    };
    let db = crate::decision_notify::open_user_db(&home).ok().flatten()
        .ok_or(site!("authorization.ingress.capture.user_db"))?;
    let context = db.live_user_context(&user).map_err(|_| site!("authorization.ingress.capture.user_db"))?
        .ok_or(site!("authorization.ingress.capture.user_inactive"))?;
    if let Some(epoch) = expected_epoch {
        require_epoch(epoch, Some(context.credential_epoch)).map_err(|_| site!("authorization.ingress.capture.epoch"))?;
    }
    Ok(HumanIngress { home, user, epoch: context.credential_epoch, source })
}

pub(super) async fn authorize_current(agent_dir: &Path, principal_context: Option<String>)
    -> Result<RuntimeAuthorization, Sited<VerificationError>> {
    // R8x: each refusal carries its fixed `authorization.ingress.*` site.
    let human = take_human()?;
    let agent = agent_dir.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| noted(site!("authorization.ingress.agent_path"), VerificationError::Invalid))?.to_owned();
    if agent_dir != human.home.join("agents").join(&agent) {
        return Err(noted(site!("authorization.ingress.home_mismatch"), VerificationError::BindingMismatch));
    }
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).ok().flatten()
        .ok_or_else(|| noted(site!("authorization.ingress.no_turn"), VerificationError::Unavailable))?;
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(Clone::clone).ok().flatten()
        .ok_or_else(|| noted(site!("authorization.ingress.no_session"), VerificationError::Unavailable))?;
    let proof = crate::computer_use_sessions::attended::capture(&agent, &turn)
        .ok_or_else(|| noted(site!("authorization.ingress.no_attended_proof"), VerificationError::Unavailable))?;
    if !proof.valid_for(&agent, &turn) {
        return Err(noted(site!("authorization.ingress.attended_proof_invalid"), VerificationError::Revoked));
    }
    // A fresh human message must not launder a nested/background dispatch.
    if crate::claude_runner::DELEGATION_ENV.try_with(|_| ()).is_ok()
        || crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten().is_some()
        || crate::runtime::round_task_env().is_some() {
        return Err(noted(site!("authorization.ingress.nested_dispatch"), VerificationError::BindingMismatch));
    }
    let expected = duduclaw_core::HumanPrincipal::user(&human.user)
        .ok_or_else(|| noted(site!("authorization.ingress.principal_mismatch"), VerificationError::Invalid))?
        .to_string();
    if crate::on_behalf_of::current().as_deref() != Some(expected.as_str()) {
        return Err(noted(site!("authorization.ingress.principal_mismatch"), VerificationError::BindingMismatch));
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
    super::boot::labelled_blocking(site!("authorization.ingress.blocking"), move || {
        const RECHECK: RefusalSite = site!("authorization.ingress.identity_recheck");
        if !proof.valid_for(&agent, &turn) {
            return Err((site!("authorization.ingress.proof_expired"), VerificationError::Revoked));
        }
        match &human.source {
            Source::Web(claims) => check_web_identity(claims, &human.user,
                duduclaw_mcp_boundary::authority::now_seconds().map_err(|error| (RECHECK, error))?)
                .map_err(|error| (RECHECK, error))?,
            Source::Channel { channel, sender } => check_channel_mapping(&human.user,
                crate::trigger_route::access::verified_user_for_channel_sender(&human.home, channel, sender).as_deref())
                .map_err(|error| (RECHECK, error))?,
        }
        let expiry = match &human.source { Source::Web(claims) => Some(claims.exp), _ => None };
        super::boot::record_authorization(human.home, agent, human.user, human.epoch, lane,
            super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
                turn, approval_session: Some(session) }, metadata: Some(metadata), principal_context,
                egress_turn, background: None }, expiry, None)
            // Read on this blocking thread, right after the step that failed.
            .map_err(|error| (super::boot::issue_step_reached(), error))
    }).await
}

/// The one-use identity. R8x: a refusal records why it is absent (no ingress
/// scope, already consumed, or the capture's own fixed reason).
fn take_human() -> Result<HumanIngress, Sited<VerificationError>> {
    let site = match INGRESS.try_with(|slot| slot.lock().ok().and_then(|mut slot| slot.take())) {
        Ok(Some(Ok(human))) => return Ok(human),
        Ok(Some(Err(site))) => site,
        Ok(None) => site!("authorization.ingress.consumed"),
        Err(_) => site!("authorization.ingress.no_scope"),
    };
    Err(noted(site, VerificationError::Unavailable))
}

fn check_web_identity(claims: &Claims, user: &str, now: u64) -> Result<(), VerificationError> {
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
    // Every component above the agent directory is only traversed: O_PATH
    // needs search permission alone (the installed home's root-owned 0711
    // parent is not readable by the gateway). O_PATH|O_NOFOLLOW|O_DIRECTORY
    // refuses a symlink (ENOTDIR); the descriptors are used only as openat
    // dirfds. The agent directory itself keeps the read open.
    let search = OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let root = open(Path::new("/"), search, Mode::empty()).map_err(|_| VerificationError::Unavailable)?;
    // SAFETY: each successful open/openat returns a new owned descriptor.
    let mut directory = unsafe { std::fs::File::from_raw_fd(root) };
    use std::os::fd::AsRawFd;
    if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(agent) { return Err(VerificationError::Invalid); }
    let mut parts = Vec::new();
    for part in home.components().chain(Path::new("agents").components()).chain(Path::new(agent).components()) {
        let std::path::Component::Normal(part) = part else {
            if part == std::path::Component::RootDir { continue; }
            return Err(VerificationError::Invalid);
        };
        parts.push(part);
    }
    let count = parts.len();
    for (index, part) in parts.into_iter().enumerate() {
        let step = if index + 1 == count { flags } else { search };
        let fd = openat(Some(directory.as_raw_fd()), Path::new(part), step, Mode::empty())
            .map_err(|_| VerificationError::Unavailable)?;
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(check_web_identity(&claims, "original-user", 99).is_ok());
        assert!(check_web_identity(&claims, "other-user", 99).is_err());
        assert!(check_web_identity(&claims, "original-user", 100).is_err());
        claims.token_type = "refresh".into();
        assert!(check_web_identity(&claims, "original-user", 99).is_err());
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
        INGRESS.scope(Mutex::new(Some(Ok(human()))), async {
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
        INGRESS.scope(Mutex::new(Some(Ok(human()))), async {
            assert!(authorize_current(Path::new("/other/agents/alice"), None).await.is_err());
            assert!(take_human().is_err());
        }).await;
    }
    /// R8x: an absent identity names why, inside the turn's carrier: no
    /// ingress scope, already consumed, or the capture's own fixed reason.
    #[tokio::test]
    async fn absent_identity_records_a_fixed_site() {
        let cases: [(Option<Option<Captured>>, &str); 3] = [
            (None, "authorization.ingress.no_scope"),
            (Some(None), "authorization.ingress.consumed"),
            (Some(Some(Err(site!("authorization.ingress.capture.web_identity")))),
                "authorization.ingress.capture.web_identity"),
        ];
        for (slot, expected) in cases {
            let probe = async { super::super::runtime::tests::site_of(async {
                assert!(take_human().is_err());
            }).await };
            let site = match slot {
                None => probe.await,
                Some(slot) => INGRESS.scope(Mutex::new(slot), probe).await,
            };
            assert_eq!(site, Some(expected));
            assert!(super::super::refusal::SITES.contains(&expected));
        }
        // The capture refuses with fixed labels only.
        let captured = capture_human("/trusted".into(), RequesterIdentity::Unknown, None);
        assert_eq!(captured.err().map(RefusalSite::as_str), Some("authorization.ingress.capture.requester"));
        let captured = capture_human("/trusted".into(),
            RequesterIdentity::DashboardUser { user_id: "original-user".into() }, None);
        assert_eq!(captured.err().map(RefusalSite::as_str), Some("authorization.ingress.capture.no_web_credential"));
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
