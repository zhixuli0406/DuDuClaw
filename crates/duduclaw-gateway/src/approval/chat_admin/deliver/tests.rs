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
        f.config("['telegram','line','googlechat']", "", true, false);
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
}

impl<'a> Recording<'a> {
    fn new(store: &'a ApprovalStore) -> Self {
        Self { store, sent: Mutex::new(Vec::new()), replaced: Mutex::new(Vec::new()),
            reminders: Mutex::new(Vec::new()), fail_send:false, fail_channel:None, no_message_channel:None,
            no_google_clicks:false, mutate_on_send:false }
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
    let line = format!("U{}", "1".repeat(32));
    f.prove(&f.admin, "line", &line, &line, "line");
    for (channels, expected) in [("['telegram','line']", "telegram"), ("['line','telegram']", "line")] {
        f.config(channels, "", true, false);
        let rec = f.record();
        let store = ApprovalStore::open_in_memory().unwrap();
        let sender = Recording::new(&store);
        let result = run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        assert_eq!(result.delivered, 1);
        assert_eq!(sender.sent.lock().unwrap()[0].1, expected);
        let live = receipts::list_live(&store, &rec.id).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].message_id.is_none(), expected == "line");
    }
}

#[tokio::test]
async fn failed_send_or_missing_message_tries_next_channel_but_success_stops() {
    let f = Fixture::new();
    f.config("['telegram','line']", "", true, false);
    f.prove(&f.admin, "telegram", "123", "123", "telegram");
    let line = format!("U{}", "1".repeat(32));
    f.prove(&f.admin, "line", &line, &line, "line");
    for (fail_first, missing_message, expected) in [
        (true, false, "line"), (false, true, "line"), (false, false, "telegram"),
    ] {
        let rec = f.record();
        let store = ApprovalStore::open_in_memory().unwrap();
        let mut sender = Recording::new(&store);
        sender.fail_channel = fail_first.then_some("telegram");
        sender.no_message_channel = missing_message.then_some("telegram");
        let result = run(f.home.path(), &store, &rec, f.plan(&rec), &sender).await;
        assert_eq!(result.delivered, 1);
        let expected_dm = if expected == "line" { line.clone() } else { "123".into() };
        assert_eq!(result.first, Some((expected.into(), expected_dm.clone())));
        let mut expected_sends = vec![(f.admin.clone(), "telegram".into(), "123".into())];
        if expected == "line" {
            expected_sends.push((f.admin.clone(), "line".into(), line.clone()));
        }
        assert_eq!(*sender.sent.lock().unwrap(), expected_sends);
        let live = receipts::list_live(&store, &rec.id).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].user_id, f.admin);
        assert_eq!(live[0].channel, expected);
        assert_eq!(live[0].dm, expected_dm);
        assert_eq!(live[0].message_id.as_deref(), if expected == "line" { None } else { Some("1") });
        assert_eq!(sender.replaced.lock().unwrap().len(), usize::from(missing_message));
    }
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
