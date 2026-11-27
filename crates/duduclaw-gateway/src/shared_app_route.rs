//! Shared-app member routing. Telegram keeps its existing identity rules;
//! Google Chat deliberately accepts only an administrator's manual binding.

use std::sync::Arc;
use crate::channel_reply::ReplyContext;
use crate::trigger_route::SenderAccess;

/// Only authenticated event resource names are accepted, never an email alias,
/// display name, bare numeric id or Google-account-derived identity.
pub(crate) fn manual_googlechat_sender(home: &std::path::Path, sender: &str) -> SenderAccess {
    if crate::googlechat_identity::normalize_user_id(sender).as_deref() != Some(sender) {
        return SenderAccess::unmapped();
    }
    let Ok(Some(db)) = crate::decision_notify::open_user_db_read_only(home) else {
        return SenderAccess::unmapped();
    };
    let Ok(Some(account)) = db.find_verified_user_id_by_channel("googlechat", sender) else {
        return SenderAccess::unmapped();
    };
    match crate::trigger_route::access::load_account(home, &db, &account) {
        Ok(Some(access)) => SenderAccess::from_effective(access),
        _ => SenderAccess::unmapped(),
    }
}

pub(crate) async fn member_agent_route(ctx: &Arc<ReplyContext>, channel: &'static str, user_id: &str) -> Option<String> {
    member_agent_route_under(ctx, channel, user_id, duduclaw_auth::acl::personal_restriction()).await
}

/// Keep the protected-mode policy explicit so tests can exercise denial
/// without mutating the process-wide ownership guard.
pub(crate) async fn member_agent_route_under(
    ctx: &Arc<ReplyContext>, channel: &'static str, user_id: &str,
    restriction: Option<&dyn duduclaw_auth::acl::PersonalRestriction>,
) -> Option<String> {
    member_route_under(ctx, channel, user_id, restriction).await.map(|(_, agent)| agent)
}

/// Return the same resolved identity that passed the avatar checks, so shared
/// DM mention/pending routing never resolves an automatic Google identity.
pub(crate) async fn member_route_under(
    ctx: &Arc<ReplyContext>, channel: &'static str, user_id: &str,
    restriction: Option<&dyn duduclaw_auth::acl::PersonalRestriction>,
) -> Option<(SenderAccess, String)> {
    if user_id.is_empty() {
        return None;
    }
    let home = ctx.home_dir.clone();
    let sender_id = user_id.to_string();
    let (sender, primary) = tokio::task::spawn_blocking(move || {
        // Verified identity → active account, the same resolution `@` routing
        // and the "who is asking" checks use; unverified rows stay unmapped.
        let sender = if channel == "googlechat" {
            manual_googlechat_sender(&home, &sender_id)
        } else {
            crate::trigger_route::access::for_channel_sender(&home, channel, &sender_id)
        };
        let account = match &sender.sender {
            crate::trigger_route::Sender::Admin { user_id: Some(id) }
            | crate::trigger_route::Sender::Account { user_id: id, .. } => id.clone(),
            _ => return None,
        };
        let db = if channel == "googlechat" {
            crate::decision_notify::open_user_db_read_only(&home).ok().flatten()?
        } else {
            crate::decision_notify::open_user_db(&home).ok().flatten()?
        };
        let primary = db.member_avatar_or_primary(&account).ok().flatten()?;
        Some((sender, primary))
    })
    .await
    .ok()
    .flatten()?;
    // Never the raw platform id in logs: a mapped sender is labelled by
    // dashboard account (an unmapped one would be a per-home keyed digest).
    let who = sender.audit_label(&ctx.home_dir, channel, user_id);
    let avatar_access =
        sender.clone().for_webchat_converse(&ctx.home_dir).await;
    let usable = {
        let reg = ctx.registry.read().await;
        reg.get(&primary)
            .is_some_and(|agent| crate::webchat::access::primary_candidate_under(&avatar_access, agent, restriction))
    };
    if usable {
        tracing::debug!(
            channel,
            agent = primary.as_str(),
            sender = %who,
            route = "primary_agent",
            "Shared app routed to the sender's avatar or primary employee"
        );
    }
    usable.then_some((sender, primary))
}

