//! RM-148: opt-in routing for the company app's proven one-to-one DMs.

use std::path::Path;
use std::sync::Arc;
use crate::channel_reply::ReplyContext;
use crate::channel_settings::keys;

pub(crate) const DEFAULT_UNBOUND_REPLY: &str =
    "這個 Chat 應用程式尚未為你開通，請聯絡管理員。";
const ONBOARDING_URL: &str = "{onboarding_url}";

pub(crate) fn validate_unbound_reply(text: &str) -> Result<(), String> {
    if text.chars().count() > 1000 {
        return Err("unbound_reply must be at most 1000 characters".into());
    }
    let literal = text.replace(ONBOARDING_URL, "");
    if literal.contains(['{', '}']) {
        return Err("unbound_reply only supports {onboarding_url}".into());
    }
    if text.contains(['<', '>']) || text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err("unbound_reply must be plain text".into());
    }
    Ok(())
}

pub(crate) fn render_unbound_reply(home: &Path, text: &str) -> String {
    // The channel resolver rejects userinfo, query/fragment and local hosts.
    // Its HTTP / native-localhost fallback is never exposed by onboarding.
    let url = crate::deep_link::public_dashboard_base_url(home)
        .filter(|base| url::Url::parse(base).is_ok_and(|url| url.scheme() == "https"))
        .and_then(|base| crate::deep_link::channel_link_base(&base));
    let rendered = text.replace(ONBOARDING_URL, url.as_deref().unwrap_or(""));
    if rendered.chars().count() > 1000 {
        text.replace(ONBOARDING_URL, "")
    } else {
        rendered
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Legacy,
    Avatar(String),
    Called { agent: String, via_trigger: bool },
    Direct(String),
    Unbound(String),
}

/// Called before attachments, placeholders, commands or model/session work.
/// Unknown space kinds and employee-owned apps never enter shared DM routing.
pub(crate) async fn route(
    ctx: &Arc<ReplyContext>, own_app: bool, is_dm: bool,
    conversation: &str, sender: &str, text: &str,
) -> Route {
    if own_app || !is_dm { return Route::Legacy; }
    // Read SQLite on every message: dashboard writes use another connection.
    let rows = match ctx.channel_settings.read_global_rows_raw(
        "googlechat", &[keys::SHARED_BOT_BINDING, keys::UNBOUND_REPLY],
    ).await {
        Ok(rows) => rows,
        Err(_) => return Route::Unbound(DEFAULT_UNBOUND_REPLY.into()),
    };
    match rows.iter().find(|(_, key, _)| key == keys::SHARED_BOT_BINDING).map(|(_, _, value)| value.as_str()) {
        None | Some("false") => return Route::Legacy,
        Some("true") => {}
        _ => return Route::Unbound(DEFAULT_UNBOUND_REPLY.into()),
    }
    let template = rows.iter().find(|(_, key, _)| key == keys::UNBOUND_REPLY)
        .map(|(_, _, value)| value.as_str()).unwrap_or(DEFAULT_UNBOUND_REPLY);
    if validate_unbound_reply(template).is_err() {
        return Route::Unbound(DEFAULT_UNBOUND_REPLY.into());
    }
    // Membership and a usable avatar are prerequisites for every shared DM,
    // including employee lists, explicit mentions and already armed picks.
    let Some((access, avatar)) = crate::shared_app_route::member_route_under(
        ctx, "googlechat", sender, duduclaw_auth::acl::personal_restriction(),
    ).await else {
        return Route::Unbound(render_unbound_reply(&ctx.home_dir, template));
    };
    // Check for a mention without consuming a one-shot pick, then apply the
    // same gate as the ordinary pipeline before making its live decision.
    // Never hand a peek back to the default pipeline: if a grant or pending
    // pick disappears in between, it must resolve to avatar/unbound instead
    // of silently reaching the company default.
    if !crate::chat_commands::is_command(text) {
        let (peek, _) = decide_turn(ctx, conversation, sender, text, false, Some(&access)).await;
        if !matches!(peek, crate::trigger_route::turn::TurnDecision::Default) {
            if let Some(reply) = crate::channel_reply::gate_direct_reply(ctx, conversation, sender, text).await {
                return Route::Direct(reply);
            }
            let (decision, fallback) = decide_turn(ctx, conversation, sender, text, true, Some(&access)).await;
            use crate::trigger_route::turn::{ReplyKind, TurnDecision};
            match decision {
                TurnDecision::Agent { id, via_trigger } => return Route::Called { agent: id, via_trigger },
                TurnDecision::Reply { text, kind, .. } => {
                    return Route::Direct(if kind != ReplyKind::AskTask
                        && !crate::trigger_route::allow_direct_reply("googlechat", conversation, sender, kind)
                    { String::new() } else { text });
                }
                TurnDecision::Assign { id } => {
                    let caller = crate::trigger_route::assign::AssignCaller {
                        home: &ctx.home_dir, channel: "googlechat", conversation, sender,
                        event: None, seq: crate::trigger_route::pending::next_seq(),
                        fallback: fallback.as_deref(),
                        access: access.clone(),
                    };
                    return Route::Direct(Box::pin(crate::trigger_route::assign::run_pending(ctx, &id, text, &caller)).await.text);
                }
                TurnDecision::Default => {}
            }
        }
    }
    Route::Avatar(avatar)
}

/// Peek only for pre-model recovery registration; the guarded pipeline still
/// makes and authorizes the live decision, then durably binds its final result.
async fn peek_turn(
    ctx: &ReplyContext, conversation: &str, sender: &str, text: &str,
) -> (crate::trigger_route::turn::TurnDecision, Option<String>) {
    decide_turn(ctx, conversation, sender, text, false, None).await
}

async fn decide_turn(
    ctx: &ReplyContext, conversation: &str, sender: &str, text: &str, live: bool,
    access: Option<&crate::trigger_route::SenderAccess>,
) -> (crate::trigger_route::turn::TurnDecision, Option<String>) {
    let reg = ctx.registry.read().await;
    let fallback = crate::channel_reply::resolve_fallback_agent(ctx, &reg, conversation, sender).await
        .map(|agent| agent.config.agent.name.clone());
    let event = uuid::Uuid::new_v4().to_string();
    let turn = crate::trigger_route::turn::ChannelTurn {
        channel: "googlechat", conversation, sender, text, fallback: fallback.as_deref(),
        event: &event, seq: crate::trigger_route::pending::next_seq(), quiet: !live,
        pending: if live { crate::trigger_route::turn::PendingPolicy::Live }
            else { crate::trigger_route::turn::PendingPolicy::Peek },
    };
    let decision = match access {
        Some(access) => crate::trigger_route::turn::decide_channel_turn_with_access(
            &ctx.home_dir, &reg, turn, access,
        ),
        None => crate::trigger_route::turn::decide_channel_turn(&ctx.home_dir, &reg, turn),
    };
    (decision, fallback)
}

pub(crate) async fn recovery_identity(
    ctx: &ReplyContext, conversation: &str, sender: &str, text: &str,
) -> (String, String) {
    let (decision, fallback) = peek_turn(ctx, conversation, sender, text).await;
    match decision {
        crate::trigger_route::turn::TurnDecision::Agent { id, via_trigger: true } => {
            let bucket = crate::trigger_route::called_session(conversation, &id);
            (id, bucket)
        }
        crate::trigger_route::turn::TurnDecision::Agent { id, .. } => (id, conversation.into()),
        _ => (fallback.unwrap_or_default(), conversation.into()),
    }
}

/// One window per canonical sender, across spaces and binding changes.
/// Audit labels remain account-based; quotas never depend on those labels.
pub(crate) fn allow_unbound_reply(home: &Path, sender: &str) -> bool {
    if crate::googlechat_identity::normalize_user_id(sender).as_deref() != Some(sender) {
        return false;
    }
    let key = crate::trigger_route::access::keyed_digest_full(home, "googlechat", sender);
    if key == "unavailable" { return false; }
    let allowed = crate::trigger_route::allow_direct_reply(
        "googlechat", "shared-app-unbound", &key, crate::trigger_route::turn::ReplyKind::Refusal,
    );
    if allowed {
        let who = crate::shared_app_route::manual_googlechat_sender(home, sender)
            .audit_label(home, "googlechat", sender);
        tracing::info!(sender = %who, route = "unbound_reply", "Google Chat shared app unbound reply");
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm148_unbound_text_is_unicode_bounded_and_accepts_only_the_onboarding_placeholder() {
        assert!(validate_unbound_reply(&"字".repeat(1000)).is_ok());
        assert!(validate_unbound_reply(&"🐾".repeat(1000)).is_ok());
        for invalid in ["字".repeat(1001), "{employee}".into(), "{{onboarding_url}}".into(), "{onboarding_url".into(), "<script>hello</script>".into(), "bad\0text".into()] {
            assert!(validate_unbound_reply(&invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_unbound_reply("請聯絡管理員。\n{onboarding_url}").is_ok());
    }

    #[test]
    fn rm148_onboarding_expands_only_safe_https_and_removes_missing_placeholder() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(render_unbound_reply(home.path(), "A{onboarding_url}B"), "AB");
        for invalid in ["http://dashboard.example.com", "https://localhost", "https://127.0.0.1", "https://user:pass@example.com", "https://example.com/?token=secret", "javascript:alert(1)"] {
            std::fs::write(home.path().join("config.toml"), format!("[dashboard]\npublic_url = {invalid:?}\n[gateway]\nport = 8080\n")).unwrap();
            assert_eq!(render_unbound_reply(home.path(), "A{onboarding_url}B"), "AB", "{invalid}");
        }
        std::fs::write(home.path().join("config.toml"), "[dashboard]\npublic_url = \"https://dashboard.example.com/team/\"\n").unwrap();
        assert_eq!(render_unbound_reply(home.path(), "A{onboarding_url}B"), "Ahttps://dashboard.example.com/teamB");
    }

    #[test]
    fn rm148_onboarding_expansion_respects_unicode_character_limit() {
        let home = tempfile::tempdir().unwrap();
        let url = "https://dashboard.example.com/team";
        std::fs::write(home.path().join("config.toml"), format!("[dashboard]\npublic_url = {url:?}\n")).unwrap();
        for placeholders in [1, 2] {
            let literal = "🐾".repeat(1000 - placeholders * url.chars().count());
            let template = format!("{literal}{}", ONBOARDING_URL.repeat(placeholders));
            assert!(validate_unbound_reply(&template).is_ok());
            let rendered = render_unbound_reply(home.path(), &template);
            assert_eq!(rendered, format!("{literal}{}", url.repeat(placeholders)));
            assert_eq!(rendered.chars().count(), 1000);
            let overflowing = format!("界{template}");
            assert!(validate_unbound_reply(&overflowing).is_ok());
            assert_eq!(render_unbound_reply(home.path(), &overflowing), format!("界{literal}"));
        }
    }

    #[test]
    fn rm148_unbound_throttle_is_per_sender_and_home() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert!(allow_unbound_reply(first.path(), "users/42"));
        assert!(!allow_unbound_reply(first.path(), "users/42"));
        assert!(allow_unbound_reply(first.path(), "users/43"));
        assert!(allow_unbound_reply(second.path(), "users/42"));
    }

    #[test]
    fn rm148_unbound_throttle_survives_binding_changes_and_separates_mapped_homes() {
        use duduclaw_auth::{UserDb, UserRole};
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let db = UserDb::new(&first.path().join("users.db")).unwrap();
        let user = db.create_user("throttle@rm148.test", "Staff", "StaffPass-12345", UserRole::Employee).unwrap();
        assert!(allow_unbound_reply(first.path(), "users/42"));
        db.bind_channel_identity(&user.id, "googlechat", "users/42", true).unwrap();
        assert!(!allow_unbound_reply(first.path(), "users/42"), "binding cannot reset the quota");
        db.bind_channel_identity(&user.id, "googlechat", "users/43", true).unwrap();
        assert!(allow_unbound_reply(first.path(), "users/43"), "distinct senders keep distinct quotas");
        // Clone the dashboard DB, including its account id, into another home.
        rusqlite::Connection::open(first.path().join("users.db")).unwrap()
            .execute("VACUUM INTO ?1", [second.path().join("users.db").to_str().unwrap()]).unwrap();
        let label = |home: &Path| crate::shared_app_route::manual_googlechat_sender(home, "users/42")
            .audit_label(home, "googlechat", "users/42");
        assert_eq!(label(first.path()), format!("user:{}", user.id));
        assert_eq!(label(first.path()), label(second.path()));
        assert!(allow_unbound_reply(second.path(), "users/42"), "account ids must not share quotas across homes");
        assert!(allow_unbound_reply(second.path(), "users/43"));
        db.unbind_channel_identity("googlechat", "users/42").unwrap();
        assert!(!allow_unbound_reply(first.path(), "users/42"), "unbinding cannot reset the quota");
        assert!(!allow_unbound_reply(first.path(), "42"), "only canonical sender names are admitted");
    }
}
