//! AG-UI uses WebChat's durable delivery fence at each SSE emission boundary.
use super::*;
use crate::channel_reply::{GuardedReply, ReplyDeliveryFence};

pub(super) struct Permit {
    users: Arc<UserDb>,
    generation: u64,
    policy: crate::webchat::access::TargetPolicy,
    home: PathBuf,
    claims: duduclaw_auth::jwt::Claims,
    source: ReplyDeliveryFence,
    deadline: tokio::time::Instant,
}

impl Permit {
    pub(super) async fn capture(state: &AgUiState, user: &str, agent: &str, connection: &Connection, source: ReplyDeliveryFence) -> Result<Self, ()> {
        let users = state.chat.user_db.clone();
        let db = users.clone();
        // Capture before the account and target snapshots; any intervening
        // writer invalidates the permit instead of reviving a stale grant.
        let generation = tokio::task::spawn_blocking(move || db.acl_generation()).await.map_err(|_| ())?.map_err(|_| ())?;
        if state.chat.credential_validity(Some(&connection.claims), true).await != crate::handlers::live_context::SocketValidity::Valid
            || state.chat.authorize_target(user, agent, true).await.is_err()
            || !source.valid().await { return Err(()); }
        let policy = state.chat.target_policy(user, agent, true).await.map_err(|_| ())?;
        Ok(Self { users, generation, policy, home: state.chat.ctx.home_dir.clone(),
            claims: connection.claims.clone(), source, deadline: tokio::time::Instant::now() + SEND_TIMEOUT })
    }

    pub(super) fn with_current<T>(&self, emit: impl FnOnce() -> T) -> Result<T, ()> {
        let result = (|| self.users.with_acl_generation(self.generation, || {
            self.policy.with_current(|| {
                let now = chrono::Utc::now().timestamp().max(0) as u64;
                let valid = webchat_credential_expiry(&self.home, &self.claims)
                    .is_ok_and(|expiry| expiry.is_none_or(|expiry| now < expiry));
                if !valid || tokio::time::Instant::now() >= self.deadline || !self.source.latched_valid() { return Err(()); }
                Ok(emit())
            }).map_err(|_| ())?
        }).map_err(|_| ())?)();
        if result.is_err() { self.source.refuse(); }
        result
    }
}

struct RefuseOnDrop<'a>(&'a GuardedReply, bool);
impl Drop for RefuseOnDrop<'_> {
    fn drop(&mut self) { if !self.1 { self.0.refuse_delivery(); } }
}

pub(super) async fn reply(state: &AgUiState, user: &str, requested: &str, sid: &str, turn: &str,
    connection: &Connection, guarded: &GuardedReply) -> Result<(), ()> {
    // Cover cancellation while awaiting persistence, SSE consumption or policy.
    let mut completion = RefuseOnDrop(guarded, false);
    let agent = guarded.answered_by.as_ref().map(|a| a.id.as_str()).unwrap_or(requested);
    let source = guarded.delivery_fence();
    let check = || async {
        if crate::approval::is_turn_cancelled(&state.chat.ctx.home_dir, sid, turn).await != Ok(false) { return Err(()); }
        Permit::capture(state, user, agent, connection, source.clone()).await
    };
    check().await?;
    guarded.persist_webchat(&state.chat.ctx.session_manager, sid, &guarded.text).await?;
    check().await?;
    let message = uuid::Uuid::new_v4().to_string();
    let frames = [
        json!({"type":"TEXT_MESSAGE_START", "messageId":message, "role":"assistant"}),
        json!({"type":"TEXT_MESSAGE_CONTENT", "messageId":message, "delta":guarded.text}),
        json!({"type":"TEXT_MESSAGE_END", "messageId":message}),
        run::finished(&connection.input, json!({"type":"success"})),
    ];
    for frame in frames {
        let permit = check().await?;
        send_frame(connection, frame, Some(permit)).await?;
    }
    // Revalidate after the final SSE acknowledgement, then atomically expose
    // the staged row under the same account/edition/config/registry fences.
    let permit = check().await?;
    guarded.publish_webchat(&state.chat.ctx.session_manager, sid, &permit.users,
        permit.generation, &permit.policy, &tokio_util::sync::CancellationToken::new()).await?;
    completion.1 = true;
    Ok(())
}
