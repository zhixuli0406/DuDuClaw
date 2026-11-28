//! P16 integration tests: deliberately not compiled/run before gateway review.
use super::*;
use crate::approval::decision_service::tests::Fixture;
use duduclaw_auth::mfa::MfaCipher;
use hmac::{Hmac, Mac};
use rusqlite::Connection;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};

struct Cipher;
impl MfaCipher for Cipher {
    fn encrypt(&self, raw: &str) -> Result<String, String> { Ok(format!("fixture:{raw}")) }
    fn decrypt(&self, sealed: &str) -> Result<String, String> {
        sealed.strip_prefix("fixture:").map(str::to_owned).ok_or_else(|| "fixture cipher".into())
    }
}
pub(crate) fn code(secret: &str, step: i64) -> String {
    let (mut bits, mut acc, mut key) = (0u32, 0u32, Vec::new());
    for c in secret.bytes() {
        let v = match c { b'A'..=b'Z' => c-b'A', b'2'..=b'7' => c-b'2'+26, _ => panic!("base32") };
        acc = (acc << 5) | u32::from(v); bits += 5;
        if bits >= 8 { bits -= 8; key.push((acc >> bits) as u8); }
    }
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&key).unwrap();
    mac.update(&(step as u64).to_be_bytes());
    let bytes = mac.finalize().into_bytes();
    let offset = (bytes[19] & 15) as usize;
    let n = u32::from_be_bytes(bytes[offset..offset+4].try_into().unwrap()) & 0x7fff_ffff;
    format!("{:06}", n % 1_000_000)
}
#[derive(Default)]
pub(crate) struct Recording { pub(crate) deletes: AtomicUsize, pub(crate) replies: Mutex<Vec<String>> }
#[async_trait::async_trait]
impl ReplySink for Recording {
    async fn delete_code(&self) { self.deletes.fetch_add(1, Ordering::SeqCst); }
    async fn reply(&self, text: &str) { self.replies.lock().unwrap().push(text.to_owned()); }
}
pub(crate) struct DmFixture { pub(crate) service: Fixture, pub(crate) id: ApprovalId, pub(crate) receipt: Receipt, pub(crate) ticket: String, pub(crate) secret: String, pub(crate) recovery: String }
impl DmFixture {
    async fn new() -> Self {
        let service = Fixture::new();
        service.config(true, "telegram", &[]);
        let path = service.env.home.join("config.toml");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str("step_up_methods=['web','dm_totp']\n");
        std::fs::write(path, config).unwrap();
        let id = service.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
        let proof = service.proof(&id).await;
        service.env.broker.store.conn.lock().await.execute(
            "UPDATE chat_admin_cards SET message_id='42' WHERE card_id=?1", [&proof.card_id]).unwrap();
        let receipt = receipts::by_card_id(&service.env.broker.store, &proof.card_id).await.unwrap().unwrap();
        Self::from_service(service, id, receipt).await
    }
    pub(crate) async fn from_service(service: Fixture, id: ApprovalId, receipt: Receipt) -> Self {
        let now = chrono::Utc::now().timestamp() - 60;
        let db = service.db();
        let challenge = db.mfa_password_challenge(&service.user, "fixture-password-only", "fixture-session", now).unwrap();
        db.mfa_prepare_enrollment(&challenge, false, now).unwrap();
        let secret = db.mfa_begin(&service.user, Some(&challenge), &Cipher, now).unwrap();
        let recovery = db.mfa_confirm(&service.user, Some(&challenge), &code(&secret, now/30), &Cipher, now).unwrap()[0].clone();
        let store = &mut Store::open(&service.env.home).unwrap();
        let ticket = store.issue_totp(&receipt).unwrap();
        store.attach_prompt(&ticket, "43").unwrap();
        Self { service, id, receipt, ticket, secret, recovery }
    }
    pub(crate) fn next_prompt(&mut self, id: ApprovalId, receipt: Receipt, prompt: &str) {
        self.id = id;
        self.receipt = receipt;
        let mut store = Store::open(&self.service.env.home).unwrap();
        self.ticket = store.issue_totp(&self.receipt).unwrap();
        store.attach_prompt(&self.ticket, prompt).unwrap();
    }
    pub(crate) fn event(&self, text: &str) -> Event {
        Event { account:self.receipt.account.clone(), dm:self.receipt.dm.clone(), principal:self.receipt.principal.clone(),
            envelope_private:true, authorized:true, reply_to:Some("43".into()), prompt_hint:true, text:text.into() }
    }
    pub(crate) async fn receive(&self, event: Event, sink: &Recording) -> bool {
        intercept_with_verify(&self.service.env, event, Lang::ZhTw, sink, |db, _, user, text, now|
            db.mfa_step_up_verify_totp_only(user, text, &Cipher, now)).await
    }
    pub(crate) fn state(&self) -> (String, i64) {
        Connection::open(self.service.env.home.join("protected/approval_step_up.db")).unwrap().query_row(
            "SELECT status,attempts FROM tickets WHERE id=?1", [&self.ticket], |r| Ok((r.get(0)?,r.get(1)?))).unwrap()
    }
    async fn pending(&self) {
        assert_eq!(self.service.status(&self.id).await, ApprovalStatus::Pending);
        let conn = self.service.env.broker.store.conn.lock().await;
        assert_eq!(conn.query_row("SELECT count(*) FROM chat_admin_decisions", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    }
    fn sql(&self, query: &str) {
        Connection::open(self.service.env.home.join("protected/approval_step_up.db")).unwrap().execute(query, []).unwrap();
    }
}

#[tokio::test]
async fn ordinary_reply_passes_through_when_ticket_store_fails() {
    let f = DmFixture::new().await;
    let protected = f.service.env.home.join("protected");
    std::fs::rename(&protected, f.service.env.home.join("protected-backup")).unwrap();
    std::fs::write(&protected, "not a directory").unwrap();
    assert!(Store::open(&f.service.env.home).is_err());
    for private in [false, true] {
        let sink = Recording::default();
        let mut event = f.event("thanks");
        event.envelope_private = private;
        event.prompt_hint = false;
        assert!(!intercept_with_verify(&f.service.env, event, Lang::En, &sink,
            |_, _, _, _, _| panic!("failed lookup must never verify")).await);
        assert_eq!(sink.deletes.load(Ordering::SeqCst), 0);
        assert!(sink.replies.lock().unwrap().is_empty());
    }
    f.pending().await;
}

#[tokio::test]
async fn private_potential_codes_are_consumed_when_ticket_store_fails() {
    let f = DmFixture::new().await;
    let protected = f.service.env.home.join("protected");
    std::fs::rename(&protected, f.service.env.home.join("protected-backup")).unwrap();
    std::fs::write(&protected, "not a directory").unwrap();
    assert!(Store::open(&f.service.env.home).is_err());
    for (text, reply_to, prompt_hint) in [
        ("123456", Some("99"), false),
        ("123456", None, false),
        ("0123456789abcdef0123456789abcdef", Some("99"), false),
        ("malformed code", Some("43"), true),
    ] {
        let sink = Recording::default();
        let mut event = f.event(text);
        event.reply_to = reply_to.map(str::to_owned);
        event.prompt_hint = prompt_hint;
        assert!(intercept_with_verify(&f.service.env, event, Lang::En, &sink,
            |_, _, _, _, _| panic!("failed lookup must never verify")).await);
        assert_eq!(sink.deletes.load(Ordering::SeqCst), 1);
        assert_eq!(sink.replies.lock().unwrap().as_slice(), [dashboard(Lang::En)]);
    }
    f.pending().await;
}

#[tokio::test]
async fn bound_prompt_still_consumes_ordinary_text_after_receipt_lookup_fails() {
    let f = DmFixture::new().await;
    f.service.env.broker.store.conn.lock().await.execute(
        "DELETE FROM chat_admin_cards WHERE card_id=?1", [&f.receipt.card_id]).unwrap();
    let sink = Recording::default();
    let mut event = f.event("thanks");
    event.prompt_hint = false;
    assert!(intercept_with_verify(&f.service.env, event, Lang::En, &sink,
        |_, _, _, _, _| panic!("missing receipt must never verify")).await);
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(sink.replies.lock().unwrap().as_slice(), [dashboard(Lang::En)]);
    assert_eq!(f.state(), ("live".into(), 0));
    f.pending().await;
}

#[tokio::test]
async fn dm_totp_rejects_recovery_code() {
    let f = DmFixture::new().await;
    let sink = Recording::default();
    assert!(f.receive(f.event(&f.recovery), &sink).await);
    assert_eq!(f.state(), ("live".into(), 1));
    f.pending().await;
    // The real recovery-capable verifier must still accept this exact code.
    f.service.db().mfa_step_up_verify(&f.service.user, &f.recovery, &Cipher, chrono::Utc::now().timestamp()).unwrap();
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 1);
    assert!(!sink.replies.lock().unwrap()[0].contains(&f.recovery));
}

#[tokio::test]
async fn dm_totp_replay_refused() {
    let mut f = DmFixture::new().await;
    let now = chrono::Utc::now().timestamp();
    let code = code(&f.secret, now/30);
    let sink = Recording::default();
    assert!(f.receive(f.event(&code), &sink).await);
    assert_eq!(f.service.status(&f.id).await, ApprovalStatus::Approved);
    // A second live approval and prompt cannot reuse the first approval's TOTP.
    let id = f.service.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    let proof = f.service.proof(&id).await;
    f.service.env.broker.store.conn.lock().await.execute(
        "UPDATE chat_admin_cards SET message_id='50' WHERE card_id=?1", [&proof.card_id]).unwrap();
    let receipt = receipts::by_card_id(&f.service.env.broker.store, &proof.card_id).await.unwrap().unwrap();
    f.next_prompt(id, receipt, "51");
    let mut reply = f.event(&code); reply.reply_to = Some("51".into());
    assert!(f.receive(reply, &sink).await);
    assert_eq!(f.state(), ("live".into(), 1));
    assert_eq!(f.service.status(&f.id).await, ApprovalStatus::Pending);
    let conn = f.service.env.broker.store.conn.lock().await;
    assert_eq!(conn.query_row("SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
        [f.id.as_str()], |r| r.get::<_,i64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn five_failures_void_and_later_correct_code_cannot_decide() {
    let f = DmFixture::new().await;
    let sink = Recording::default();
    let step = chrono::Utc::now().timestamp() / 30;
    let nearby = (step-2..=step+2).map(|step| code(&f.secret, step)).collect::<Vec<_>>();
    let wrong = (0..10).map(|n| format!("{n:06}")).find(|candidate| !nearby.contains(candidate)).unwrap();
    for index in 0..5 {
        assert!(f.receive(f.event(&wrong), &sink).await);
        assert_eq!(f.state().1, index as i64 + 1);
    }
    assert_eq!(f.state(), ("void".into(), 5));
    assert!(sink.replies.lock().unwrap()[4].contains("儀表板"));
    assert!(f.receive(f.event(&code(&f.secret, chrono::Utc::now().timestamp()/30)), &sink).await);
    f.pending().await;
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn invalid_origins_expired_and_used_prompts_never_enter_conversation_or_decide() {
    let f = DmFixture::new().await;
    let code = code(&f.secret, chrono::Utc::now().timestamp()/30);
    let sink = Recording::default();
    for case in ["principal", "group", "account", "dm", "no_reply"] {
        let mut event = f.event(&code);
        match case {
            "principal" => event.principal = "other".into(),
            "group" => event.envelope_private = false,
            "account" => event.account = "telegram:other".into(),
            "dm" => event.dm = "other".into(),
            "no_reply" => { event.reply_to = None; event.prompt_hint = false; },
            _ => { event.reply_to = Some("99".into()); event.prompt_hint = false; },
        }
        assert!(f.receive(event, &sink).await, "{case}");
        assert_eq!(f.state(), ("live".into(), 0));
        f.pending().await;
    }
    f.sql("UPDATE tickets SET expires=strftime('%s','now')-1");
    assert!(f.receive(f.event("even malformed input"), &sink).await);
    f.sql("UPDATE tickets SET expires=strftime('%s','now')+180,status='done'");
    assert!(f.receive(f.event(&code), &sink).await);
    // After purge, the quoted prompt still suppresses arbitrary potential codes.
    f.sql("UPDATE tickets SET expires=strftime('%s','now')-86401");
    assert!(f.receive(f.event("malformed after purge"), &sink).await);
    f.pending().await;
    let mut ordinary = f.event("hello"); ordinary.reply_to = None; ordinary.prompt_hint = false;
    assert!(!f.receive(ordinary, &sink).await);
}

#[tokio::test]
async fn bare_codes_require_a_live_ticket_for_the_exact_private_sender() {
    let f = DmFixture::new().await;
    for input in ["123456", "0123456789abcdef0123456789abcdef"] {
        for case in ["live", "group", "account", "dm", "principal", "unrelated_reply", "ordinary"] {
            let sink = Recording::default();
            let mut event = f.event(input);
            event.reply_to = None;
            event.prompt_hint = false;
            match case {
                "group" => event.envelope_private = false,
                "account" => event.account = "telegram:other".into(),
                "dm" => event.dm = "other".into(),
                "principal" => event.principal = "other".into(),
                "unrelated_reply" => event.reply_to = Some("99".into()),
                "ordinary" => event.text = "hello".into(),
                _ => {},
            }
            let consumed = intercept_with_verify(&f.service.env, event, Lang::En, &sink,
                |_, _, _, _, _| panic!("unbound code must never be verified")).await;
            assert_eq!(consumed, case == "live", "{case}");
            assert_eq!(sink.deletes.load(Ordering::SeqCst), usize::from(consumed));
            let replies = sink.replies.lock().unwrap();
            if consumed { assert_eq!(replies.as_slice(), ["Please reply to the verification prompt with your code."]); }
            else { assert!(replies.is_empty()); }
            assert_eq!(f.state(), ("live".into(), 0));
        }
    }
    for change in [
        "UPDATE tickets SET expires=strftime('%s','now')-1",
        "UPDATE tickets SET expires=strftime('%s','now')+180,status='done'",
        "UPDATE tickets SET status='void'",
        "UPDATE tickets SET status='live',attempts=5",
        "UPDATE tickets SET attempts=0,method='web'",
        "DELETE FROM tickets",
    ] {
        f.sql(change);
        let sink = Recording::default();
        let mut event = f.event("123456"); event.reply_to = None; event.prompt_hint = false;
        assert!(!intercept_with_verify(&f.service.env, event, Lang::En, &sink,
            |_, _, _, _, _| panic!("no live TOTP ticket")).await, "{change}");
        assert_eq!(sink.deletes.load(Ordering::SeqCst), 0);
        assert!(sink.replies.lock().unwrap().is_empty());
    }
    f.pending().await;
}

#[tokio::test]
async fn unbound_prompt_hint_is_consumed_only_for_a_private_reply() {
    let f = DmFixture::new().await;
    f.sql("DELETE FROM tickets");
    for private in [false, true] {
        let sink = Recording::default();
        let mut event = f.event("arbitrary content"); event.envelope_private = private;
        assert_eq!(intercept_with_verify(&f.service.env, event, Lang::En, &sink,
            |_, _, _, _, _| panic!("expired prompt must never verify")).await, private);
        assert_eq!(sink.deletes.load(Ordering::SeqCst), usize::from(private));
        if private { assert_eq!(sink.replies.lock().unwrap().as_slice(), [expired(Lang::En)]); }
        else { assert!(sink.replies.lock().unwrap().is_empty()); }
    }
    f.pending().await;
}

#[tokio::test]
async fn success_records_totp_required_and_spends_ticket_once() {
    let f = DmFixture::new().await;
    let sink = Recording::default();
    let code = code(&f.secret, chrono::Utc::now().timestamp()/30);
    let mut event = f.event(&code); event.prompt_hint = false;
    assert!(f.receive(event, &sink).await);
    assert_eq!(f.state(), ("done".into(), 0));
    let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
    assert_eq!(row.status, ApprovalStatus::Approved);
    assert_eq!(row.decided_by, Some(format!("chat-admin:{}:telegram:totp", f.service.user)));
    {
        let conn = f.service.env.broker.store.conn.lock().await;
        let row: (String, i64) = conn.query_row("SELECT step,step_required FROM chat_admin_decisions WHERE approval_id=?1",
            [f.id.as_str()], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(row, ("totp".into(), 1));
    }
    assert!(f.receive(f.event(&code), &sink).await);
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 2);
    assert!(sink.replies.lock().unwrap()[1].contains("已失效"));
}

#[tokio::test]
async fn disabled_dm_totp_has_no_prompt_and_service_refuses_proof() {
    let f = DmFixture::new().await;
    f.service.config(true, "telegram", &[]);
    let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
    super::super::begin_with_prompt(&f.service.env, &row, &f.receipt, Lang::ZhTw, |_, _| async {
        panic!("disabled method must not send a prompt")
    }).await;
    let proof = ChatAdminProof { user_id:f.service.user.clone(), channel:config::ChatChannel::Telegram,
        account:f.receipt.account.clone(), principal:f.receipt.principal.clone(), dm:f.receipt.dm.clone(),
        card_id:f.receipt.card_id.clone(), digest:f.receipt.digest.clone(), step:ledger::Step::Totp };
    for approve in [true, false] {
        assert!(matches!(decision_service::decide(&f.service.env, &f.id, approve, "", &Decider::ChatAdmin(proof.clone())).await,
            Err(crate::approval::decision_service::DecisionRefusal::StepUpRequired)));
        assert_eq!(f.state(), ("live".into(), 0));
        f.pending().await;
    }
    let before = f.state();
    let sink = Recording::default();
    assert!(intercept_with_verify(&f.service.env,
        f.event(&code(&f.secret, chrono::Utc::now().timestamp()/30)), Lang::ZhTw, &sink,
        |_, _, _, _, _| panic!("disabled method must not verify the factor")).await);
    assert_eq!(sink.replies.lock().unwrap().as_slice(), [dashboard(Lang::ZhTw)]);
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state(), before, "disabled method must preserve ticket and attempts");
    assert_eq!(f.state(), ("live".into(), 0));
    f.pending().await;
}

#[tokio::test]
async fn begin_prompt_and_web_share_limit_and_prompt_is_bound() {
    let f = DmFixture::new().await;
    let path = f.service.env.home.join("config.toml");
    let mut cfg = std::fs::read_to_string(&path).unwrap();
    cfg.push_str("[dashboard]\npublic_url='https://dashboard.example.test/'\n");
    std::fs::write(path, cfg).unwrap();
    let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
    let result = super::super::begin_with_prompt(&f.service.env, &row, &f.receipt, Lang::ZhTw, |r, text| async move {
        assert_eq!(r.message_id.as_deref(), Some("42"));
        assert_eq!(r.account, "telegram");
        assert_eq!(text, prompt_text(Lang::ZhTw));
        Some("44".into())
    }).await;
    assert!(result.card_link.is_some());
    let mut store = Store::open(&f.service.env.home).unwrap();
    let ticket = store.prompt("telegram", &f.receipt.dm, "44").unwrap().unwrap();
    assert_eq!(ticket.ticket.digest, f.receipt.digest);
    let ttl = ticket.ticket.expires - chrono::Utc::now().timestamp();
    assert!((175..=180).contains(&ttl));
    assert!(store.issue(&f.receipt).is_err());
    assert!(store.issue_totp(&f.receipt).is_err());
    f.pending().await;
}

#[tokio::test]
async fn googlechat_and_unenrolled_admin_never_receive_totp_prompt() {
    let f = DmFixture::new().await;
    let path = f.service.env.home.join("config.toml");
    let mut cfg = std::fs::read_to_string(&path).unwrap();
    cfg = cfg.replace("['web','dm_totp']", "['dm_totp']");
    std::fs::write(&path, &cfg).unwrap();
    let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
    let mut receipt = f.receipt.clone(); receipt.channel = "googlechat".into();
    let result = super::super::begin_with_prompt(&f.service.env, &row, &receipt, Lang::ZhTw, |_, _| async {
        panic!("Google Chat must not solicit codes")
    }).await;
    assert!(result.reply.contains("儀表板"));
    f.service.db().mfa_operator_reset("admin@example.com").unwrap();
    let result = super::super::begin_with_prompt(&f.service.env, &row, &f.receipt, Lang::ZhTw, |_, _| async {
        panic!("unenrolled admin must not receive prompt")
    }).await;
    assert!(result.reply.contains("儀表板"));
    f.pending().await;
}

#[tokio::test]
async fn ticket_cas_has_one_winner_and_prompt_send_failure_voids_ticket() {
    let f = DmFixture::new().await;
    let store = Store::open(&f.service.env.home).unwrap();
    let ticket = store.prompt("telegram", &f.receipt.dm, "43").unwrap().unwrap();
    store.consume_totp(&ticket.ticket).unwrap();
    assert!(Store::open(&f.service.env.home).unwrap().consume_totp(&ticket.ticket).is_err());
    let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
    let result = super::super::begin_with_prompt(&f.service.env, &row, &f.receipt, Lang::ZhTw, |_, _| async { None }).await;
    assert!(result.reply.contains("儀表板"));
    let conn = Connection::open(f.service.env.home.join("protected/approval_step_up.db")).unwrap();
    assert_eq!(conn.query_row("SELECT count(*) FROM tickets WHERE status='live' AND method='totp'", [], |r|r.get::<_,i64>(0)).unwrap(), 0);
    assert_eq!(conn.query_row("SELECT count(*) FROM tickets WHERE status='void'", [], |r|r.get::<_,i64>(0)).unwrap(), 1);
    f.pending().await;
}

#[tokio::test]
async fn dm_only_prompts_without_https_and_googlechat_keeps_web_option() {
    let f = DmFixture::new().await;
    let path = f.service.env.home.join("config.toml");
    let cfg = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, cfg.replace("['web','dm_totp']", "['dm_totp']")).unwrap();
    let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
    let result = super::super::begin_with_prompt(&f.service.env, &row, &f.receipt, Lang::En, |_, text| async move {
        assert_eq!(text, prompt_text(Lang::En)); Some("46".into())
    }).await;
    assert!(result.card_link.is_none());
    assert!(result.reply.contains("new prompt"));
    std::fs::write(&path, format!("{cfg}[dashboard]\npublic_url='https://dashboard.example.test/'\n")).unwrap();
    let mut receipt = f.receipt.clone(); receipt.channel = "googlechat".into();
    let result = super::super::begin_with_prompt(&f.service.env, &row, &receipt, Lang::Ja, |_, _| async {
        panic!("Google Chat must use web, not DM TOTP")
    }).await;
    assert!(result.card_link.is_some());
    f.pending().await;
}

#[tokio::test]
async fn malformed_prompt_reply_counts_and_never_enters_conversation() {
    let f = DmFixture::new().await;
    let sink = Recording::default();
    for (index, input) in ["12345", "1234567", "abcdef", "１２３", " 123456 "].into_iter().enumerate() {
        assert!(f.receive(f.event(input), &sink).await);
        assert_eq!(f.state().1, index as i64 + 1);
    }
    assert_eq!(f.state(), ("void".into(), 5));
    assert_eq!(sink.deletes.load(Ordering::SeqCst), 5);
    f.pending().await;
}

#[tokio::test]
async fn denied_private_codes_and_prompt_replies_stay_consumed_when_store_fails() {
    let f = DmFixture::new().await;
    let protected = f.service.env.home.join("protected");
    std::fs::rename(&protected, f.service.env.home.join("protected-backup")).unwrap();
    std::fs::write(&protected, "not a directory").unwrap();
    assert!(Store::open(&f.service.env.home).is_err());
    for (text, reply_to, prompt_hint) in [
        ("123456", None, false),
        ("123456", Some("99"), false),
        ("0123456789abcdef0123456789abcdef", None, false),
        ("malformed code", Some("43"), true),
    ] {
        let sink = Recording::default();
        let mut event = f.event(text);
        event.authorized = false;
        event.reply_to = reply_to.map(str::to_owned);
        event.prompt_hint = prompt_hint;
        assert!(intercept_with_verify(&f.service.env, event, Lang::En, &sink,
            |_, _, _, _, _| panic!("denied sender must never verify")).await);
        assert_eq!(sink.deletes.load(Ordering::SeqCst), 1);
        assert_eq!(sink.replies.lock().unwrap().as_slice(), [dashboard(Lang::En)]);
    }
    f.pending().await;
}

#[tokio::test]
async fn denied_private_prompt_reply_does_not_verify_or_count_attempts() {
    let f = DmFixture::new().await;
    for (text, reply_to) in [("123456", Some("43")), ("malformed code", Some("43")), ("123456", None)] {
        let sink = Recording::default();
        let mut event = f.event(text);
        event.authorized = false;
        event.reply_to = reply_to.map(str::to_owned);
        event.prompt_hint = false;
        assert!(intercept_with_verify(&f.service.env, event, Lang::En, &sink,
            |_, _, _, _, _| panic!("denied sender must never verify")).await);
        assert_eq!(sink.deletes.load(Ordering::SeqCst), 1);
        assert_eq!(sink.replies.lock().unwrap().as_slice(), [dashboard(Lang::En)]);
        assert_eq!(f.state(), ("live".into(), 0));
        f.pending().await;
    }
}
