use super::*;
use std::sync::Mutex;
use crate::decision_card::PushedMessage;
use duduclaw_auth::UserStatus;

struct Fixture {
    home: tempfile::TempDir,
    db: UserDb,
    admin: String,
    ack: String,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@example.com", "Admin", "fixture-password-only", UserRole::Admin).unwrap().id;
        let ack = db.create_user("ack@example.com", "Ack", "fixture-password-only", UserRole::Admin).unwrap().id;
        let f = Self { home, db, admin, ack };
        f.config("['telegram','googlechat']", "", true, false);
        f
    }

    fn config(&self, channels: &str, extra: &str, enabled: bool, forbid_self: bool) {
        std::fs::write(self.home.path().join("config.toml"), format!(
            "[channels]\ntelegram_bot_token_enc='invalid-encrypted-fixture'\nline_channel_token_enc='invalid-encrypted-fixture'\nline_channel_secret='fixture'\ngooglechat_project_number='1'\ngooglechat_service_account_json_enc='invalid-encrypted-fixture'\n{extra}\n[approval]\nforbid_self_approval={forbid_self}\n[approval.chat_admin]\nenabled={enabled}\nchannels={channels}\nenabled_ack={{by='{}',at='2026-11-27T00:00:00Z'}}\n", self.ack,
        )).unwrap();
    }

    fn prove(&self, uid: &str, channel: &str, principal: &str, dm: &str, account: &str) {
        self.db.bind_channel_identity(uid, channel, principal, true).unwrap();
        // Same persisted evidence as the handshake; no transport/network.
        rusqlite::Connection::open(self.home.path().join("users.db")).unwrap().execute(
            "UPDATE channel_identities SET proof_method='chat_handshake', proof_user_id=user_id,
             proof_account=?1, proof_dm=?2, proven_at=?3 WHERE user_id=?4 AND channel=?5 AND channel_user_id=?6",
            rusqlite::params![account, dm, Utc::now().timestamp(), uid, channel, principal],
        ).unwrap();
    }

    fn record(&self) -> ApprovalRecord {
        serde_json::from_value(json!({"id":uuid::Uuid::new_v4().to_string(), "agent_id":"anna",
            "action_kind":"knowledge_quarantine", "summary":"review", "payload":{"snippet":"original claim"},
            "status":"pending", "created_at":Utc::now().to_rfc3339(), "ttl_seconds":3600,
            "decided_at":null, "decided_by":null})).unwrap()
    }

    fn plan(&self, rec: &ApprovalRecord) -> Plan { prepare(self.home.path(), rec).unwrap() }
}

struct Recording<'a> {
    store: &'a ApprovalStore,
    sent: Mutex<Vec<(String, String, String)>>,
    replaced: Mutex<Vec<String>>,
    reminders: Mutex<Vec<(String, Option<String>)>>,
    fail_send: bool,
    fail_channel: Option<&'static str>,
    no_message_channel: Option<&'static str>,
    no_google_clicks: bool,
    mutate_on_send: bool,
    edits: Mutex<Vec<(String, Option<String>, String)>>,
    edit_destinations: Mutex<Vec<(String, String)>>,
    fail_edit: bool,
}

impl<'a> Recording<'a> {
    fn new(store: &'a ApprovalStore) -> Self {
        Self { store, sent: Mutex::new(Vec::new()), replaced: Mutex::new(Vec::new()),
            reminders: Mutex::new(Vec::new()), fail_send:false, fail_channel:None, no_message_channel:None,
            no_google_clicks:false, mutate_on_send:false, edits:Mutex::new(Vec::new()),
            edit_destinations:Mutex::new(Vec::new()), fail_edit:false }
    }
}

#[async_trait::async_trait]
impl Sender for Recording<'_> {
    async fn supports(&self, target: &Target) -> bool { !(self.no_google_clicks && target.proof.channel == "googlechat") }
    async fn send(&self, target: &Target, _: &card::ChatCard, approval_id: &str) -> Result<Option<PushedMessage>, ()> {
        let current = receipts::list_live(self.store, &crate::approval::ApprovalId::from(approval_id.to_owned())).await.unwrap();
        assert!(!current.iter().any(|r| r.user_id == target.user_id), "receipt must not precede send");
        if self.mutate_on_send {
            self.store.conn.lock().await.execute("UPDATE approvals SET summary='changed during send' WHERE id=?1", [approval_id]).unwrap();
        }
        let count = {
            let mut sent = self.sent.lock().unwrap();
            sent.push((target.user_id.clone(), target.proof.channel.clone(), target.proof.proof_dm.clone()));
            sent.len()
        };
        if self.fail_send || self.fail_channel == Some(target.proof.channel.as_str()) { return Err(()); }
        if self.no_message_channel == Some(target.proof.channel.as_str()) { return Ok(None); }
        Ok(Some(PushedMessage { edit_chat_id:target.proof.proof_dm.clone(), message_id:count.to_string() }))
    }
    async fn replace(&self, target: &Target, _: Option<&PushedMessage>) -> bool {
        self.replaced.lock().unwrap().push(target.user_id.clone()); true
    }
    async fn remind(&self, target: &Target, message: Option<&str>, _: &str) -> bool {
        self.reminders.lock().unwrap().push((target.user_id.clone(), message.map(str::to_owned))); true
    }
    async fn edit(&self, target: &Target, message: &PushedMessage, text: &str) -> bool {
        self.edits.lock().unwrap().push((target.proof.proof_account.clone(), target.agent.clone(), text.into()));
        self.edit_destinations.lock().unwrap().push((message.edit_chat_id.clone(), message.message_id.clone()));
        !self.fail_edit
    }
}

#[tokio::test]
async fn cards_never_target_control_channel_or_group() {
    let f = Fixture::new();
    let mut rec = f.record();
    rec.notify_channel = Some("telegram".into()); rec.notify_chat_id = Some("-100999".into());
    rec.payload["origin_channel"] = json!("telegram"); rec.payload["origin_chat_id"] = json!("-100999");
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    for dm in ["-100999", "456"] {
        f.prove(&f.admin, "telegram", "123", dm, "telegram");
        assert_eq!(run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await.delivered, 0);
    }
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    assert_eq!(run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await.first, Some(("telegram".into(), "123".into())));
    assert_eq!(sender.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn one_card_per_admin_uses_configured_channel_order() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    f.prove(&f.admin, "googlechat", "users/123", "spaces/dm", "googlechat");
    for (channels, expected) in [("['telegram','googlechat']", "telegram"), ("['googlechat','telegram']", "googlechat")] {
        f.config(channels, "", true, false);
        let rec = f.record();
        let store = ApprovalStore::open_in_memory().unwrap();
        let sender = Recording::new(&store);
        let result = run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        assert_eq!(result.delivered, 1);
        assert_eq!(sender.sent.lock().unwrap()[0].1, expected);
        let live = receipts::list_live(&store, &rec.id).await.unwrap();
        assert_eq!(live.len(), 1);
        assert!(live[0].message_id.is_some());
    }
}

#[tokio::test]
async fn failed_send_or_missing_message_tries_next_channel_but_success_stops() {
    let f = Fixture::new();
    f.config("['telegram','googlechat']", "", true, false);
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    f.prove(&f.admin, "googlechat", "users/123", "spaces/dm", "googlechat");
    for (fail_first, missing_message, expected) in [
        (true, false, "googlechat"), (false, true, "googlechat"), (false, false, "telegram"),
    ] {
        let rec = f.record();
        let store = ApprovalStore::open_in_memory().unwrap();
        let mut sender = Recording::new(&store);
        sender.fail_channel = fail_first.then_some("telegram");
        sender.no_message_channel = missing_message.then_some("telegram");
        let result = run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        assert_eq!(result.delivered, 1);
        let expected_dm = if expected == "googlechat" { "spaces/dm".to_owned() } else { "123".into() };
        assert_eq!(result.first, Some((expected.into(), expected_dm.clone())));
        let mut expected_sends = vec![(f.admin.clone(), "telegram".into(), "123".into())];
        if expected == "googlechat" {
            expected_sends.push((f.admin.clone(), "googlechat".into(), "spaces/dm".to_owned()));
        }
        assert_eq!(*sender.sent.lock().unwrap(), expected_sends);
        let live = receipts::list_live(&store, &rec.id).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].user_id, f.admin);
        assert_eq!(live[0].channel, expected);
        assert_eq!(live[0].dm, expected_dm);
        assert_eq!(live[0].message_id.as_deref(), if expected == "googlechat" { Some("2") } else { Some("1") });
        assert_eq!(sender.replaced.lock().unwrap().len(), usize::from(missing_message));
    }
}

#[tokio::test]
async fn unsupported_channel_proofs_never_receive_cards() {
    let f = Fixture::new();
    f.config("['line','slack','discord','telegram','googlechat']",
        "slack_bot_token_enc='invalid-encrypted-fixture'\nslack_app_token_enc='invalid-encrypted-fixture'\ndiscord_bot_token_enc='invalid-encrypted-fixture'",
        true, false);
    for (channel, principal, dm) in [
        ("line", format!("U{}", "1".repeat(32)), format!("U{}", "1".repeat(32))),
        ("slack", "U123456789".into(), "D123456789".into()),
        ("discord", "123456789012345678".into(), "123456789012345678".into()),
    ] {
        f.prove(&f.admin, channel, &principal, &dm, channel);
    }
    assert_eq!(f.db.approval_proofs(&f.admin).unwrap().len(), 3);
    let rec = f.record();
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    let plan = f.plan(&rec);
    assert!(plan.recipients.is_empty());
    assert_eq!(run(f.home.path(), &store, &rec, plan, &sender).await.delivered, 0);
    assert!(sender.sent.lock().unwrap().is_empty());
    assert!(receipts::list_live(&store, &rec.id).await.unwrap().is_empty());
}

#[test]
fn only_current_admin_with_proof_is_a_recipient() {
    let f = Fixture::new();
    let rec = f.record();
    // A verified but manually entered identity is not an approval proof.
    f.db.bind_channel_identity(&f.admin, "telegram", "123", true).unwrap();
    assert!(f.plan(&rec).recipients.is_empty());
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    assert_eq!(f.plan(&rec).recipients.len(), 1);
    f.db.update_user(&f.admin, None, Some(UserRole::Manager), None).unwrap();
    assert!(f.plan(&rec).recipients.is_empty());
    f.db.update_user(&f.admin, None, Some(UserRole::Admin), None).unwrap();
    f.db.set_user_status(&f.admin, UserStatus::Suspended).unwrap();
    assert!(f.plan(&rec).recipients.is_empty());
    f.db.set_user_status(&f.admin, UserStatus::Active).unwrap();
    rusqlite::Connection::open(f.home.path().join("users.db")).unwrap()
        .execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&f.admin]).unwrap();
    assert!(f.plan(&rec).recipients.is_empty());
}

#[tokio::test]
async fn receipt_is_written_after_success_with_pre_send_digest() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let rec = f.record();
    let store = ApprovalStore::open_in_memory().unwrap();
    store.insert(&rec).await.unwrap();
    let mut sender = Recording::new(&store);
    sender.fail_send = true;
    assert_eq!(run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await.delivered, 0);
    assert!(receipts::list_live(&store, &rec.id).await.unwrap().is_empty());
    sender.fail_send = false; sender.mutate_on_send = true;
    let digest = crate::approval::live_events::decision_digest_at(f.home.path(), &rec);
    assert_eq!(run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await.delivered, 1);
    let live = receipts::list_live(&store, &rec.id).await.unwrap();
    assert_eq!(live[0].digest, digest);
    let mut changed = rec.clone(); changed.summary = "changed during send".into();
    assert_ne!(live[0].digest, crate::approval::live_events::decision_digest_at(f.home.path(), &changed));
    assert_eq!(live[0].account, "telegram"); assert_eq!(live[0].principal, "123");
    assert_eq!(live[0].dm, "123"); assert_eq!(live[0].status, "live");
    assert_eq!(live[0].purpose, "rm143"); assert_eq!(live[0].tier, "T1");
    assert!(uuid::Uuid::parse_str(&live[0].card_id).is_ok());
}

#[tokio::test]
async fn receipt_failure_replaces_card_and_does_not_count_delivery() {
    let f = Fixture::new(); f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let rec = f.record(); let store = ApprovalStore::open_in_memory().unwrap();
    store.conn.lock().await.execute_batch("CREATE TRIGGER refuse_receipt BEFORE INSERT ON chat_admin_cards BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    let sender = Recording::new(&store);
    let result = run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
    assert_eq!(result.delivered, 0); assert!(result.first.is_none());
    assert_eq!(*sender.replaced.lock().unwrap(), vec![f.admin.clone()]);
    assert!(receipts::list_live(&store, &rec.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn googlechat_without_http_clicks_tries_the_next_channel() {
    let f = Fixture::new();
    f.config("['googlechat','telegram']", "", true, false);
    f.prove(&f.admin, "googlechat", "users/123", "spaces/DM1", "googlechat");
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let rec = f.record(); let store = ApprovalStore::open_in_memory().unwrap();
    let mut sender = Recording::new(&store); sender.no_google_clicks = true;
    assert_eq!(run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await.first, Some(("telegram".into(), "123".into())));
    f.config("['googlechat','telegram']", "googlechat_connection='pubsub'\ngooglechat_pubsub_subscription='projects/p/subscriptions/s'", true, false);
    let plan = f.plan(&rec);
    assert_eq!(plan.recipients[0][0].proof.channel, "telegram");
    assert!(!crate::googlechat::sending_app_takes_http_clicks(f.home.path(), None, "spaces/DM1", true).await);
    f.config("['googlechat']", "googlechat_connection='pubsub'\ngooglechat_pubsub_subscription='projects/p/subscriptions/s'", true, false);
    assert!(f.plan(&rec).recipients.is_empty());
}

#[test]
fn forbid_self_approval_excludes_on_behalf_of_admin() {
    let f = Fixture::new(); f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let mut rec = f.record();
    rec.on_behalf_of = duduclaw_core::HumanPrincipal::user(&f.admin).map(|p| p.to_string());
    assert_eq!(f.plan(&rec).recipients.len(), 1);
    f.config("['telegram']", "", true, true);
    assert!(f.plan(&rec).recipients.is_empty());
}

#[tokio::test]
async fn t3_personal_and_feature_off_keep_the_existing_route() {
    let f = Fixture::new(); f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let mut rec = f.record();
    rec.action_kind = "unknown".into();
    assert!(try_push(f.home.path(), &rec, false).await.is_none());
    rec.action_kind = "knowledge_quarantine".into();
    f.config("['telegram']", "", false, false);
    assert!(try_push(f.home.path(), &rec, false).await.is_none());
    f.config("['telegram']", "", true, false);
    rusqlite::Connection::open(f.home.path().join("users.db")).unwrap().execute(
        "INSERT INTO isolation_personal_agents(agent_name, owner_user_id, created_at) VALUES('anna', ?1, '2026-11-27T00:00:00Z')", [&f.admin],
    ).unwrap();
    crate::personal_visibility::invalidate_owner_cache(f.home.path());
    assert!(try_push(f.home.path(), &rec, false).await.is_none());
}

#[tokio::test]
async fn reminders_reuse_each_live_receipt_without_new_cards() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    f.prove(&f.ack, "telegram", "456", "456", "telegram");
    let rec = f.record(); let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    assert_eq!(run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await.delivered, 2);
    let live = receipts::list_live(&store, &rec.id).await.unwrap();
    assert_eq!(remind_live(&live, &f.plan(&rec), &sender, "soon").await.delivered, 2);
    assert_eq!(sender.sent.lock().unwrap().len(), 2);
    assert_eq!(sender.reminders.lock().unwrap().len(), 2);
    assert!(sender.reminders.lock().unwrap().iter().all(|(_, message)| message.is_some()));
    assert_eq!(remind_live(&[], &f.plan(&rec), &sender, "soon").await.delivered, 0);
    receipts::mark(&store, &live[0].card_id, receipts::TerminalStatus::Retired).await.unwrap();
    let live = receipts::list_live(&store, &rec.id).await.unwrap();
    assert_eq!(remind_live(&live, &f.plan(&rec), &sender, "soon").await.delivered, 1);
}

#[test]
fn removed_account_and_unreadable_proofs_fail_closed() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram:removed");
    let rec = f.record(); assert!(f.plan(&rec).recipients.is_empty());
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    // Simulate corrupt proof storage while leaving the live Admin readable.
    rusqlite::Connection::open(f.home.path().join("users.db")).unwrap()
        .execute("UPDATE channel_identities SET proven_at='bad' WHERE user_id=?1", [&f.admin]).unwrap();
    assert!(f.plan(&rec).recipients.is_empty());
}

#[test]
fn usable_proof_is_not_hidden_by_another_proof_for_removed_bot() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "100", "100", "telegram:removed");
    f.prove(&f.admin, "telegram", "200", "200", "telegram");
    let plan = f.plan(&f.record());
    assert_eq!(plan.recipients.len(), 1);
    assert_eq!(plan.recipients[0][0].proof.proof_dm, "200");
}

#[tokio::test]
async fn resend_plan_only_delivers_to_requested_live_admin() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    f.prove(&f.ack, "telegram", "456", "456", "telegram");
    let rec = f.record();
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    let plan = prepare_for_user(f.home.path(), &rec, &f.admin).unwrap();
    assert_eq!(run(f.home.path(), &store, &rec, plan, &sender).await.delivered, 1);
    let sent = sender.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, f.admin);
    drop(sent);
    f.db.update_user(&f.admin, None, Some(UserRole::Manager), None).unwrap();
    assert!(prepare_for_user(f.home.path(), &rec, &f.admin).unwrap().recipients.is_empty());
}

// P17 exercises the same recording transport as delivery, without HTTP.
async fn receipt_status(store: &ApprovalStore, card_id: &str) -> String {
    store.conn.lock().await.query_row("SELECT status FROM chat_admin_cards WHERE card_id=?1",
        [card_id], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn chat_decision_rewrites_other_admins_but_never_consumed_card() {
    use super::super::sync;
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    f.prove(&f.ack, "telegram", "456", "456", "telegram");
    let rec = f.record();
    let store = std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap());
    store.insert(&rec).await.unwrap();
    let sender = Recording::new(&store);
    run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
    let live = receipts::list_live(&store, &rec.id).await.unwrap();
    let pressed = live.iter().find(|r| r.user_id == f.admin).unwrap();
    let entry = super::super::LedgerEntry { user_id:f.admin.parse().unwrap(),
        channel:config::ChatChannel::Telegram, step:super::super::ledger::Step::Tap,
        step_required:false, card_id:pressed.card_id.clone(), digest:pressed.digest.clone() };
    ApprovalBroker::new(store.clone()).decide_as(&rec.id, true,
        &super::super::DecisionActor::ChatAdmin(entry)).await.unwrap();
    let rec = store.get(&rec.id).await.unwrap().unwrap();
    let lang = crate::trigger_route::texts::Lang::ZhTw;
    sync::collapse_with_sender(&store, &rec, true, Some("Admin".into()),
        &sync::decision_channel_label(&rec, lang), lang, &sender).await;
    let edits = sender.edits.lock().unwrap();
    assert_eq!(edits.len(), 1);
    assert!(edits[0].2.contains("已核准"));
    assert!(edits[0].2.contains("Admin"));
    assert!(edits[0].2.contains("Telegram"));
    let lang = crate::trigger_route::texts::Lang::ZhTw;
    let decided_at = chrono::DateTime::parse_from_rfc3339(rec.decided_at.as_deref().unwrap()).unwrap().with_timezone(&Utc);
    assert_eq!(edits[0].2, card::result_text(card::kind_label(&rec, lang), true, "Admin", decided_at, "Telegram", lang));
    assert_eq!(sender.edit_destinations.lock().unwrap()[0].0, "456");
    drop(edits);
    for receipt in live {
        assert_eq!(receipt_status(&store, &receipt.card_id).await,
            if receipt.user_id == f.admin { "consumed" } else { "retired" });
    }
}

#[tokio::test]
async fn dashboard_decision_rewrites_all_cards_with_localized_dashboard_label() {
    use super::super::sync;
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    f.prove(&f.ack, "telegram", "456", "456", "telegram");
    let rec = f.record();
    let store = std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap());
    store.insert(&rec).await.unwrap();
    let sender = Recording::new(&store);
    run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
    let live = receipts::list_live(&store, &rec.id).await.unwrap();
    ApprovalBroker::new(store.clone()).decide_as(&rec.id, false,
        &super::super::DecisionActor::Dashboard { user_id:f.admin.clone() }).await.unwrap();
    let rec = store.get(&rec.id).await.unwrap().unwrap();
    let lang = crate::trigger_route::texts::Lang::ZhTw;
    sync::collapse_with_sender(&store, &rec, false, Some("Admin".into()),
        &sync::decision_channel_label(&rec, lang), lang, &sender).await;
    assert_eq!(sender.edits.lock().unwrap().len(), 2);
    assert!(sender.edits.lock().unwrap().iter().all(|(_, _, text)| text.contains("已拒絕") && text.contains("儀表板")));
    for receipt in live { assert_eq!(receipt_status(&store, &receipt.card_id).await, "retired"); }
}

#[tokio::test]
async fn feature_off_sweeps_live_cards_across_approvals() {
    use super::super::sync;
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    let mut cards = Vec::new();
    for _ in 0..2 {
        let rec = f.record();
        run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        cards.extend(receipts::list_live(&store, &rec.id).await.unwrap());
    }
    f.config("['telegram']", "", false, false);
    sync::sweep_feature_off_with_sender(f.home.path(), &store, &sender).await;
    assert_eq!(sender.edits.lock().unwrap().len(), 2);
    assert!(sender.edits.lock().unwrap().iter().all(|(_, _, text)| text == card::feature_off_text(crate::trigger_route::texts::Lang::ZhTw)));
    for receipt in cards { assert_eq!(receipt_status(&store, &receipt.card_id).await, "retired"); }
    sync::sweep_feature_off_with_sender(f.home.path(), &store, &sender).await;
    assert_eq!(sender.edits.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn approval_expiry_rewrites_live_cards_and_marks_expired() {
    use super::super::sync;
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let rec = f.record();
    let store = std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap());
    store.insert(&rec).await.unwrap();
    let sender = Recording::new(&store);
    run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
    let receipt = receipts::list_live(&store, &rec.id).await.unwrap().pop().unwrap();
    store.conn.lock().await.execute("UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1", [rec.id.as_str()]).unwrap();
    assert_eq!(ApprovalBroker::new(store.clone()).expire_stale().await.unwrap(), 1);
    let rec = store.get(&rec.id).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Expired);
    sync::expire_with_sender(&store, &rec, crate::trigger_route::texts::Lang::ZhTw, &sender).await;
    assert_eq!(receipt_status(&store, &receipt.card_id).await, "expired");
    assert_eq!(sender.edits.lock().unwrap()[0].2, card::expired_text(card::kind_label(&rec, crate::trigger_route::texts::Lang::ZhTw), crate::trigger_route::texts::Lang::ZhTw));
}

#[tokio::test]
async fn edit_failure_still_retires_and_line_needs_no_edit() {
    use super::super::sync;
    for line in [false, true] {
        let f = Fixture::new();
        f.prove(&f.admin, "telegram", "123", "123", "telegram");
        let mut rec = f.record();
        let store = ApprovalStore::open_in_memory().unwrap();
        let mut sender = Recording::new(&store);
        sender.fail_edit = true;
        run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        let receipt = receipts::list_live(&store, &rec.id).await.unwrap().pop().unwrap();
        if line {
            // Historical receipt only: current config never delivers LINE cards.
            store.conn.lock().await.execute("UPDATE chat_admin_cards SET channel='line' WHERE card_id=?1",
                [&receipt.card_id]).unwrap();
        }
        rec.status = ApprovalStatus::Denied;
        rec.decided_at = Some(Utc::now().to_rfc3339());
        sync::collapse_with_sender(&store, &rec, false, None, "儀表板", crate::trigger_route::texts::Lang::ZhTw, &sender).await;
        assert_eq!(receipt_status(&store, &receipt.card_id).await, "retired");
        assert_eq!(sender.edits.lock().unwrap().len(), usize::from(!line));
    }
}

#[tokio::test]
async fn maintenance_expires_invalid_or_elapsed_card_deadlines_without_deciding() {
    use super::super::sync;
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    let mut cards = Vec::new();
    for deadline in ["invalid", "2000-01-01T00:00:00Z", "2999-01-01T00:00:00Z"] {
        let rec = f.record();
        store.insert(&rec).await.unwrap();
        run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        let receipt = receipts::list_live(&store, &rec.id).await.unwrap().pop().unwrap();
        store.conn.lock().await.execute("UPDATE chat_admin_cards SET valid_until=?1 WHERE card_id=?2",
            rusqlite::params![deadline, receipt.card_id]).unwrap();
        cards.push(receipt);
    }
    sync::maintain_live(f.home.path(), &store, receipts::list_all_live(&store).await.unwrap(), &sender).await;
    assert_eq!(sender.edits.lock().unwrap().len(), 2);
    for (i, receipt) in cards.iter().enumerate() {
        assert_eq!(receipt_status(&store, &receipt.card_id).await, if i == 2 { "live" } else { "expired" });
        assert_eq!(store.get(&crate::approval::ApprovalId::from(receipt.approval_id.clone())).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    }
}

#[tokio::test]
async fn sync_uses_each_receipts_own_bot_or_app_and_ignores_other_purposes() {
    use super::super::sync;
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let mut rec = f.record();
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
    let base = receipts::list_live(&store, &rec.id).await.unwrap().pop().unwrap();
    for channel in ["telegram", "googlechat", "slack", "discord"] {
        let mut receipt = base.clone();
        receipt.card_id = uuid::Uuid::new_v4().to_string();
        receipt.channel = channel.into();
        receipt.account = format!("{channel}:anna");
        receipt.dm = format!("{channel}-dm");
        receipt.message_id = Some(format!("{channel}-message"));
        receipts::insert(&store, &receipt).await.unwrap();
    }
    // A future infrastructure consumer's receipt is not RM-143's to retire.
    store.conn.lock().await.execute("UPDATE chat_admin_cards SET purpose='other' WHERE card_id=?1", [&base.card_id]).unwrap();
    rec.status = ApprovalStatus::Approved;
    rec.decided_at = Some(Utc::now().to_rfc3339());
    sync::collapse_with_sender(&store, &rec, true, None, "Telegram", crate::trigger_route::texts::Lang::ZhTw, &sender).await;
    let edits = sender.edits.lock().unwrap();
    assert_eq!(edits.len(), 4);
    for channel in ["telegram", "googlechat", "slack", "discord"] {
        assert!(edits.iter().any(|(account, agent, _)| account == &format!("{channel}:anna") && agent.as_deref() == Some("anna")));
        assert!(sender.edit_destinations.lock().unwrap().contains(&(format!("{channel}-dm"), format!("{channel}-message"))));
    }
    drop(edits);
    assert_eq!(receipt_status(&store, &base.card_id).await, "live");
}

#[tokio::test]
async fn feature_off_sweep_keeps_cards_live_when_effective_config_is_enabled() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let rec = f.record();
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
    super::super::sync::sweep_feature_off_with_sender(f.home.path(), &store, &sender).await;
    assert_eq!(receipts::list_live(&store, &rec.id).await.unwrap().len(), 1);
    assert!(sender.edits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maintenance_reconciles_invalidated_and_withdrawn_cards() {
    let f = Fixture::new();
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let store = ApprovalStore::open_in_memory().unwrap();
    let sender = Recording::new(&store);
    let mut cards = Vec::new();
    for (status, actor) in [("invalidated", "system:invalidate"), ("denied", "system:withdraw")] {
        let rec = f.record();
        store.insert(&rec).await.unwrap();
        run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        cards.extend(receipts::list_live(&store, &rec.id).await.unwrap());
        store.conn.lock().await.execute("UPDATE approvals SET status=?1, decided_by=?2 WHERE id=?3",
            rusqlite::params![status, actor, rec.id.as_str()]).unwrap();
    }
    super::super::sync::maintain_live(f.home.path(), &store, receipts::list_all_live(&store).await.unwrap(), &sender).await;
    assert_eq!(sender.edits.lock().unwrap().len(), 2);
    assert!(sender.edits.lock().unwrap().iter().all(|(_, _, text)| text.starts_with("已失效")));
    for receipt in cards { assert_eq!(receipt_status(&store, &receipt.card_id).await, "expired"); }
}

#[test]
fn collapse_label_uses_recorded_actor_and_defaults_to_dashboard() {
    use super::super::{ledger::{DecidedBy, Step}, sync};
    use crate::trigger_route::texts::Lang;
    let f = Fixture::new();
    let mut rec = f.record();
    for (channel, label) in [(config::ChatChannel::Telegram, "Telegram"),
        (config::ChatChannel::Googlechat, "Google Chat")] {
        rec.decided_by = Some(DecidedBy::new(f.admin.parse().unwrap(), channel, Step::Web).to_string());
        assert_eq!(sync::decision_channel_label(&rec, Lang::ZhTw), label);
        assert_eq!(sync::decision_channel_label(&rec, Lang::En), label);
    }
    for by in [None, Some(format!("dashboard:{}", f.admin)), Some("system".into()),
        Some("chat-admin:invalid:telegram:tap".into()),
        Some(format!("chat-admin:{}:telegram:tap:extra", f.admin))] {
        rec.decided_by = by;
        assert_eq!(sync::decision_channel_label(&rec, Lang::ZhTw), "儀表板");
        assert_eq!(sync::decision_channel_label(&rec, Lang::En), "Dashboard");
    }
}
