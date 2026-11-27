//! Integration tests intentionally left unrun until gateway review (P13).
use super::*;
use crate::approval::decision_service::tests::Fixture as ServiceFixture;
use duduclaw_auth::UserRole;
use tokio::sync::RwLock;

const PRIVATE: DecisionAccessScope<'static> = DecisionAccessScope {
    channel_id: None, guild_id: None, session_id: None,
};

struct Fixture {
    service: ServiceFixture,
    ctx: ReplyContext,
    context: DecisionContext,
    origin: PressOrigin,
    row: ApprovalRecord,
    action: DecisionAction,
    receipt: receipts::Receipt,
}

impl Fixture {
    async fn new() -> Self { Self::with_kind(false).await }

    async fn with_kind(quarantine: bool) -> Self {
        let service = ServiceFixture::new();
        let id = if quarantine { service.quarantine().await }
            else { service.simple(crate::expert_skills::SKILLS_ACTION_KIND).await };
        service.proof(&id).await;
        let receipt = receipts::list_live(&service.env.broker.store, &id).await.unwrap().pop().unwrap();
        let home = &service.env.home;
        let registry = duduclaw_agent::AgentRegistry::new(home.join("agents"));
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let ctx = ReplyContext::new(Arc::new(RwLock::new(registry)), home.clone(),
            Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap()),
            Arc::new(RwLock::new(std::collections::HashMap::new())), event_tx);
        let context = DecisionContext { channel: "telegram".into(), account_id: "verified-bot-id".into(),
            conversation_id: receipt.dm.clone(), principal_id: receipt.principal.clone() };
        let origin = PressOrigin { message_id: receipt.message_id.clone(), private: true,
            account: receipt.account.clone(), dm: receipt.dm.clone() };
        let row = service.env.broker.get(&id).await.unwrap().unwrap();
        let action = DecisionAction { source: DecisionSource::Approval, act: DecisionAct::Approve, id: id.to_string() };
        Self { service, ctx, context, origin, row, action, receipt }
    }

    async fn press(&self) -> PressOutcome {
        handle(&self.ctx, &self.context, &self.action, &self.row, &self.origin, PRIVATE).await
    }

    async fn pending(&self) {
        assert_eq!(self.service.status(&self.row.id).await, ApprovalStatus::Pending);
        let conn = self.service.env.broker.store.conn.lock().await;
        let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
            [self.row.id.as_str()], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    async fn refused(&self, reason: &str) -> PressOutcome {
        let outcome = self.press().await;
        assert_eq!(outcome.refused_reason, Some(reason));
        assert!(!outcome.reply.contains(&self.context.principal_id));
        self.pending().await;
        outcome
    }

    async fn saved_receipt(&self) -> receipts::Receipt {
        receipts::lookup_message(&self.service.env.broker.store, &self.receipt.channel,
            &self.receipt.account, &self.receipt.dm, self.receipt.message_id.as_deref().unwrap())
            .await.unwrap().unwrap()
    }
}

#[tokio::test]
async fn channel_access_refused() {
    let f = Fixture::new().await;
    f.ctx.channel_settings.set("telegram", "global", "blocked_users", r#"["123456789"]"#).await.unwrap();
    let outcome = f.refused("channel_access").await;
    assert!(outcome.card_text.is_none());
}

#[tokio::test]
async fn group_press_refused() {
    let mut f = Fixture::new().await;
    f.origin.private = false;
    f.refused("not_private").await;
}

#[tokio::test]
async fn press_on_unreceipted_message_refused() {
    let mut f = Fixture::new().await;
    f.origin.message_id = Some("invented-message".into());
    f.refused("receipt_missing").await;
}

#[tokio::test]
async fn missing_message_id_refused() {
    let mut f = Fixture::new().await;
    f.origin.message_id = None;
    f.refused("message_id_missing").await;
}

#[tokio::test]
async fn press_with_other_approval_id_refused() {
    let mut f = Fixture::new().await;
    let other = f.service.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    f.row = f.service.env.broker.get(&other).await.unwrap().unwrap();
    f.action.id = other.to_string();
    f.refused("approval_id_mismatch").await;
    assert_eq!(f.service.status(&ApprovalId::from(f.receipt.approval_id.clone())).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn principal_mismatch_refused() {
    let mut f = Fixture::new().await;
    f.context.principal_id = "different-verified-person".into();
    f.refused("principal_mismatch").await;
}

#[tokio::test]
async fn account_and_dm_mismatch_refused() {
    let mut f = Fixture::new().await;
    f.origin.account.push_str(":other");
    f.refused("receipt_missing").await;
    assert_eq!(receipt_mismatch(&f.receipt, &f.context, &f.action, &f.origin), Some("account_mismatch"));
    f.origin.account = f.receipt.account.clone();
    f.origin.dm.push_str("-other");
    f.refused("receipt_missing").await;
    assert_eq!(receipt_mismatch(&f.receipt, &f.context, &f.action, &f.origin), Some("dm_mismatch"));
}

#[tokio::test]
async fn expired_card_refused_and_receipt_expired() {
    let f = Fixture::new().await;
    f.service.env.broker.store.conn.lock().await.execute(
        "UPDATE chat_admin_cards SET valid_until='2000-01-01T00:00:00Z' WHERE card_id=?1",
        [&f.receipt.card_id]).unwrap();
    let outcome = f.refused("card_expired").await;
    assert_eq!(outcome.reply, "這張卡已失效，請到儀表板處理");
    assert_eq!(outcome.card_text, Some(card::expired_text(card::kind_label(&f.row, Lang::ZhTw), Lang::ZhTw)));
    assert_eq!(f.saved_receipt().await.status, "expired");
}

#[tokio::test]
async fn digest_drift_refuses_and_resends() {
    let f = Fixture::new().await;
    f.service.env.broker.store.conn.lock().await.execute(
        "UPDATE approvals SET summary='updated content' WHERE id=?1", [f.row.id.as_str()]).unwrap();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let fixture = &f;
    let counter = &calls;
    let outcome = handle_with_resend(&f.ctx, &f.context, &f.action, &f.row, &f.origin, PRIVATE,
        |home, broker, row, user| async move {
            assert_eq!(home, fixture.service.env.home);
            assert_eq!(user, fixture.service.user);
            assert_eq!(row.summary, "updated content");
            assert_eq!(fixture.saved_receipt().await.status, "retired");
            assert!(receipts::list_live(&broker.store, &row.id).await.unwrap().is_empty());
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(outcome.reply, "內容已更新，已送出新卡");
    assert_eq!(outcome.refused_reason, Some("digest_mismatch"));
    assert_eq!(outcome.card_text.as_ref(), Some(&outcome.reply));
    f.pending().await;
}

#[tokio::test]
async fn failed_resend_does_not_claim_delivery() {
    let f = Fixture::new().await;
    f.service.env.broker.store.conn.lock().await.execute(
        "UPDATE approvals SET summary='updated content' WHERE id=?1", [f.row.id.as_str()]).unwrap();
    let outcome = handle_with_resend(&f.ctx, &f.context, &f.action, &f.row, &f.origin, PRIVATE,
        |_, _, _, _| async { false }).await;
    assert_eq!(outcome.reply, DecisionRefusal::DigestMismatch.user_text(Lang::ZhTw));
    assert_eq!(f.saved_receipt().await.status, "retired");
    f.pending().await;
}

#[tokio::test]
async fn prefilled_binding_cannot_decide() {
    let f = Fixture::new().await;
    rusqlite::Connection::open(f.service.env.home.join("users.db")).unwrap().execute(
        "UPDATE channel_identities SET proof_method=NULL, proof_user_id=NULL, proof_account=NULL,
         proof_dm=NULL, proven_at=NULL WHERE user_id=?1", [&f.service.user]).unwrap();
    f.refused("forbidden").await;
}

#[tokio::test]
async fn manager_with_proof_refused() {
    let f = Fixture::new().await;
    f.service.db().update_user(&f.service.user, None, Some(UserRole::Manager), None).unwrap();
    f.refused("forbidden").await;
}

#[tokio::test]
async fn demoted_admin_old_card_refused() {
    let f = Fixture::new().await;
    f.service.db().update_user(&f.service.user, None, Some(UserRole::Employee), None).unwrap();
    f.refused("forbidden").await;
}

#[tokio::test]
async fn unbound_admin_old_card_refused() {
    let f = Fixture::new().await;
    f.service.db().unbind_channel_identity("telegram", &f.context.principal_id).unwrap();
    f.refused("forbidden").await;
}

#[tokio::test]
async fn proof_revoked_admin_old_card_refused_without_unbinding() {
    let f = Fixture::new().await;
    assert!(f.service.db().clear_approval_proof(&f.service.user, "telegram", &f.context.principal_id).unwrap());
    assert_eq!(f.service.db().find_verified_user_id_by_channel("telegram", &f.context.principal_id).unwrap(),
        Some(f.service.user.clone()));
    f.refused("forbidden").await;
    assert_eq!(f.saved_receipt().await.status, "live");
}

#[tokio::test]
async fn unsupported_adapter_press_with_matching_proof_and_receipt_is_refused() {
    for channel in ["line", "slack", "discord"] {
        let mut f = Fixture::new().await;
        // Include Telegram so filtering does not simply disable the feature.
        f.service.config(true, &format!("telegram','{channel}"), &[]);
        f.context.channel = channel.into();
        f.origin.account = channel.into();
        rusqlite::Connection::open(f.service.env.home.join("users.db")).unwrap().execute(
            "UPDATE channel_identities SET channel=?1, proof_account=?1 WHERE user_id=?2",
            rusqlite::params![channel, f.service.user]).unwrap();
        f.service.env.broker.store.conn.lock().await.execute(
            "UPDATE chat_admin_cards SET channel=?1, account=?1,
                message_id=CASE WHEN ?1='line' THEN NULL ELSE message_id END WHERE card_id=?2",
            rusqlite::params![channel, f.receipt.card_id]).unwrap();
        f.refused("feature_off").await;
    }
}

#[tokio::test]
async fn double_press_executes_once() {
    let f = Fixture::with_kind(true).await;
    let (first, second) = tokio::join!(f.press(), f.press());
    assert_eq!(usize::from(first.refused_reason.is_none()) + usize::from(second.refused_reason.is_none()), 1);
    let repeat = if first.refused_reason.is_some() { first } else { second };
    assert_eq!(repeat.refused_reason, Some("already_decided"));
    assert!(repeat.reply.contains("Ada"));
    assert!(repeat.reply.contains("Telegram"));
    assert_eq!(f.service.effect(&f.row.id).await, serde_json::json!(1));
    assert_eq!(f.press().await.reply, repeat.reply);
    let conn = f.service.env.broker.store.conn.lock().await;
    let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
        [f.row.id.as_str()], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn step_up_without_web_prerequisites_falls_back_but_deny_decides() {
    let mut f = Fixture::new().await;
    f.service.config(true, "telegram", &[]);
    let outcome = f.refused("step_up_required").await;
    assert!(outcome.reply.starts_with("這類審批請到儀表板核准"));
    assert_eq!(outcome.card_text.as_ref(), Some(&outcome.reply));
    assert_eq!(f.saved_receipt().await.status, "live");
    f.action.act = DecisionAct::Deny;
    assert_eq!(f.press().await.refused_reason, None);
    assert_eq!(f.service.status(&f.row.id).await, ApprovalStatus::Denied);
}

#[tokio::test]
async fn strict_text_never_decides_chat_admin_kind() {
    let f = Fixture::new().await;
    for verb in ["approve", "deny", "確認", "取消", "answer"] {
        let text = format!("{verb} {}", f.row.id);
        let result = crate::decision_notify::route_trusted_decision_fastlane_with_scope(
            &f.ctx, &f.context, &text, PRIVATE).await;
        assert_eq!(result, Some(Err("這類審批請按卡片上的按鈕".into())));
        assert_eq!(crate::decision_notify::route_bound_text(&f.ctx.home_dir, &f.context, &text).await, result);
        f.pending().await;
    }
}

#[tokio::test]
async fn reply_to_unreceipted_message_is_conversation() {
    let mut f = Fixture::new().await;
    f.origin.message_id = Some("fake-card".into());
    crate::decision_message_store::record_card_message(&f.ctx.home_dir, DecisionSource::Approval.namespace(),
        f.row.id.as_str(), "telegram", &f.origin.dm, &crate::decision_card::PushedMessage {
            edit_chat_id: f.origin.dm.clone(), message_id: "fake-card".into(),
        });
    for text in ["approve", "拒絕", "retry"] {
        assert!(crate::decision_text::route_verified_text_reply(&f.ctx, &f.context, &f.origin, PRIVATE, text).await.is_none());
        assert!(crate::decision_text::route_text_reply(&f.ctx.home_dir, "telegram", &f.context.principal_id,
            &f.origin.dm, "fake-card", text).await.is_none());
    }
    f.pending().await;
}

#[tokio::test]
async fn reply_to_receipted_message_uses_receipt_id() {
    let f = Fixture::new().await;
    // No decision_message_store entry is needed or trusted.
    let result = crate::decision_text::route_verified_text_reply(&f.ctx, &f.context, &f.origin, PRIVATE, "拒絕").await;
    assert!(matches!(result, Some(Ok(_))));
    assert_eq!(f.service.status(&f.row.id).await, ApprovalStatus::Denied);
}

#[tokio::test]
async fn feature_off_refuses_without_legacy_fallback() {
    let f = Fixture::new().await;
    f.service.config(false, "telegram", &[]);
    assert!(!owns(&f.ctx.home_dir, &f.row).await);
    let data = crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Approve, f.row.id.as_str());
    let old = crate::decision_notify::route_bound_press(&f.ctx.home_dir, &f.context, &data).await;
    assert!(matches!(old, Some(Err(_))));
    let result = crate::decision_notify::route_verified_bound_press(&f.ctx, &f.context, &data, PRIVATE, &f.origin).await;
    let result = result.unwrap();
    assert_eq!(result.refused_reason, Some("feature_off"));
    assert_eq!(result.reply, card::feature_off_text(Lang::ZhTw));
    let outcome = f.refused("feature_off").await;
    assert_eq!(outcome.card_text, Some(card::feature_off_text(Lang::ZhTw).into()));
}

#[tokio::test]
async fn closing_feature_refuses_old_cards() {
    let f = Fixture::new().await;
    f.service.config(false, "telegram", &[]);
    super::super::sync::sweep_feature_off(&f.ctx.home_dir).await;
    assert_eq!(f.saved_receipt().await.status, "retired");
    let data = crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Approve, f.row.id.as_str());
    let outcome = crate::decision_notify::route_verified_bound_press(&f.ctx, &f.context, &data, PRIVATE, &f.origin).await.unwrap();
    assert_eq!(outcome.refused_reason, Some("receipt_not_live"));
    assert_eq!(outcome.reply, card::feature_off_text(Lang::ZhTw));
    f.pending().await;
    // Re-enabling cannot revive the old card's authority either.
    f.service.config(true, "telegram", &[]);
    f.refused("receipt_not_live").await;
}

#[tokio::test]
async fn shared_entry_preserves_card_text_and_never_falls_through() {
    let f = Fixture::new().await;
    let data = crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Approve, f.row.id.as_str());
    let mut missing = f.origin.clone();
    missing.message_id = None;
    let refusal = crate::decision_notify::route_verified_bound_press(&f.ctx, &f.context,
        &data, PRIVATE, &missing).await.unwrap();
    assert_eq!(refusal.refused_reason, Some("message_id_missing"));
    f.pending().await;
    let accepted = crate::decision_notify::route_verified_bound_press(&f.ctx, &f.context,
        &data, PRIVATE, &f.origin).await.unwrap();
    assert_eq!(accepted.refused_reason, None);
    assert_eq!(accepted.card_text.as_ref(), Some(&accepted.reply));
    assert_eq!(f.service.status(&f.row.id).await, ApprovalStatus::Approved);
}

#[tokio::test]
async fn dashboard_decision_retires_old_chat_card() {
    let f = Fixture::new().await;
    let user = f.service.db().live_user_context(&f.service.user).unwrap().unwrap();
    decision_service::decide(&f.service.env, &f.row.id, true, "", &Decider::Dashboard(user)).await.unwrap();
    super::super::sync::collapse_after_decision(&f.ctx.home_dir, &f.row, true, None).await;
    assert_eq!(f.saved_receipt().await.status, "retired");
    let reply = f.press().await;
    assert_eq!(reply.refused_reason, Some("receipt_not_live"));
}
