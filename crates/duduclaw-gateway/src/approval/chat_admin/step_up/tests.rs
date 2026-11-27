//! P15 regression tests. Gateway compilation/execution is reserved for review.
use super::*;
use super::{store::Store, routes::{Choice, Decision}};
use crate::approval::{ApprovalId, ApprovalStatus, decision_service::{self, tests::Fixture}};
use crate::approval::chat_admin::receipts;
use chrono::Utc;
use hmac::{Hmac, Mac};
use rusqlite::{Connection, params};

pub(crate) struct WebFixture {
    pub(crate) service: Fixture,
    pub(crate) id: ApprovalId,
    pub(crate) receipt: Receipt,
    pub(crate) raw: String,
    pub(crate) session: String,
}
impl WebFixture {
    async fn new() -> Self {
        let service = Fixture::new();
        service.config(true, "telegram", &[]);
        let id = service.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
        let proof = service.proof(&id).await;
        let receipt = receipts::by_card_id(&service.env.broker.store, &proof.card_id).await.unwrap().unwrap();
        Self::from_service(service, id, receipt)
    }
    pub(crate) fn from_service(service: Fixture, id: ApprovalId, receipt: Receipt) -> Self {
        let raw = Store::open(&service.env.home).unwrap().issue(&receipt).unwrap();
        Self { service, id, receipt, raw, session:uuid::Uuid::new_v4().to_string() }
    }
    pub(crate) fn ctx(&self) -> UserContext { self.service.db().live_user_context(&self.service.user).unwrap().unwrap() }
    pub(crate) fn input(&self, approve: bool, csrf: Option<[u8;32]>) -> Decision {
        Decision { decision:if approve { Choice::Approve } else { Choice::Reject },
            expected_digest:self.receipt.digest.clone(), step_up_csrf:csrf }
    }
    pub(crate) async fn post(&self, input: Decision) -> Result<Value, Error> {
        routes::confirm(&self.service.env, &self.ctx(), &self.session, &self.raw, input).await
    }
    async fn handler(&self) -> crate::handlers::MethodHandler {
        let handler = crate::handlers::MethodHandler::new(self.service.env.home.clone()).await;
        handler.set_task_store(std::sync::Arc::new(
            crate::task_store::TaskStore::open(&self.service.env.home).unwrap(),
        )).await;
        handler.set_user_db(self.service.env.users.clone().unwrap(),
            std::sync::Arc::new(duduclaw_auth::JwtConfig::new(b"step-up-test-secret-32-bytes!!!!!"))).await;
        handler
    }
    pub(crate) async fn get(&self) -> Result<Value, Error> {
        routes::preview(&self.handler().await, &self.service.env, &self.ctx(), &self.session, &self.raw).await
    }
    fn sql(&self, query: &str) {
        Connection::open(self.service.env.home.join("protected/approval_step_up.db")).unwrap()
            .execute(query, []).unwrap();
    }
    fn factor(&self) {
        // The auth tests cover enrollment. Here seed an enrolled TOTP, then use
        // the REAL password+TOTP migration_step_up API to issue the proof.
        Connection::open(self.service.env.home.join("users.db")).unwrap().execute(
            "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,?2,0)",
            params![self.service.user, format!("fixture:{}", serde_json::to_string(
                &(&self.service.user, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")).unwrap())]).unwrap();
    }
    pub(crate) fn step_up(&self) -> [u8;32] {
        self.factor();
        let now = Utc::now().timestamp();
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(b"12345678901234567890").unwrap();
        mac.update(&(now as u64 / 30).to_be_bytes());
        let bytes = mac.finalize().into_bytes();
        let offset = (bytes[19] & 15) as usize;
        let n = u32::from_be_bytes(bytes[offset..offset+4].try_into().unwrap()) & 0x7fff_ffff;
        let code = format!("{:06}", n % 1_000_000);
        self.service.db().migration_step_up(&self.service.user, &self.session, self.ctx().credential_epoch,
            "fixture-password-only", &code, &FixtureCipher, now).unwrap().csrf
    }
    async fn pending(&self) {
        assert_eq!(self.service.status(&self.id).await, ApprovalStatus::Pending);
        let conn = self.service.env.broker.store.conn.lock().await;
        let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}
struct FixtureCipher;
impl duduclaw_auth::mfa::MfaCipher for FixtureCipher {
    fn encrypt(&self, raw: &str) -> Result<String, String> { Ok(format!("fixture:{raw}")) }
    fn decrypt(&self, sealed: &str) -> Result<String, String> {
        sealed.strip_prefix("fixture:").map(str::to_owned).ok_or_else(|| "fixture cipher".into())
    }
}

#[tokio::test]
async fn ticket_other_user_refused() {
    let f = WebFixture::new().await;
    let other = f.service.db().create_user("other@test.invalid", "Other", "fixture-password-only", UserRole::Admin).unwrap();
    let ctx = f.service.db().live_user_context(&other.id).unwrap().unwrap();
    let handler = f.handler().await;
    let get = routes::preview(&handler, &f.service.env, &ctx, &f.session, &f.raw).await.unwrap_err();
    let post = routes::confirm(&f.service.env, &ctx, &f.session, &f.raw, f.input(false, None)).await.unwrap_err();
    assert_eq!((get.status, get.code), (StatusCode::NOT_FOUND, "not_found"));
    assert_eq!(get.body(), post.body());
    f.pending().await;
}

#[tokio::test]
async fn get_does_not_consume_or_extend_and_uses_dashboard_projection() {
    let f = WebFixture::new().await;
    let handler = f.handler().await;
    let first = routes::preview(&handler, &f.service.env, &f.ctx(), &f.session, &f.raw).await.unwrap();
    let second = routes::preview(&handler, &f.service.env, &f.ctx(), &f.session, &f.raw).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first["status"], "live");
    assert_eq!(first["has_totp"], false);
    let crate::protocol::WsFrame::Response { payload:Some(dashboard), ok:true, .. } =
        handler.handle_approvals_list(json!({"id":f.id.as_str()}), &f.ctx()).await else { panic!("dashboard projection"); };
    assert_eq!(first["approval"], dashboard["approvals"][0]);
    assert!(Store::open(&f.service.env.home).unwrap().preview(&f.raw, &f.service.user).is_ok());
    assert_eq!(f.post(f.input(false, None)).await.unwrap()["status"], "done");
}

#[tokio::test]
async fn ticket_reuse_refused() {
    let f = WebFixture::new().await;
    f.post(f.input(false, None)).await.unwrap();
    let second = f.post(f.input(false, None)).await.unwrap_err();
    assert_eq!((second.status, second.code), (StatusCode::CONFLICT, "used"));
}

#[tokio::test]
async fn concurrent_posts_have_one_winner() {
    let f = WebFixture::new().await;
    let (a,b) = tokio::join!(f.post(f.input(false, None)), f.post(f.input(false, None)));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let conn = f.service.env.broker.store.conn.lock().await;
    assert_eq!(conn.query_row("SELECT count(*) FROM chat_admin_decisions", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn expired_get_has_distinct_shape_and_post_refuses() {
    let f = WebFixture::new().await;
    f.sql("UPDATE tickets SET expires=strftime('%s','now')-1");
    for error in [f.get().await.unwrap_err(), f.post(f.input(false, None)).await.unwrap_err()] {
        assert_eq!(error.status, StatusCode::GONE);
        assert_eq!(error.body()["status"], "expired");
    }
    assert_eq!(Store::open(&f.service.env.home).unwrap().preview(&f.raw, "foreign").unwrap_err().code, "not_found");
    f.pending().await;
}

#[tokio::test]
async fn approve_requires_fresh_same_session_epoch_and_csrf() {
    let f = WebFixture::new().await;
    assert_eq!(f.post(f.input(true, None)).await.unwrap_err().code, "step_up_required");
    let csrf = f.step_up();
    assert_eq!(f.post(f.input(true, Some([0;32]))).await.unwrap_err().code, "step_up_required");
    let other_session = uuid::Uuid::new_v4().to_string();
    let error = routes::confirm(&f.service.env, &f.ctx(), &other_session, &f.raw, f.input(true, Some(csrf))).await.unwrap_err();
    assert_eq!(error.code, "step_up_required");
    Connection::open(f.service.env.home.join("users.db")).unwrap().execute(
        "UPDATE migration_dashboard_proofs SET verified_at=?1", [Utc::now().timestamp()-301]).unwrap();
    assert_eq!(f.post(f.input(true, Some(csrf))).await.unwrap_err().code, "step_up_required");
    f.pending().await;
}

#[tokio::test]
async fn stale_or_mismatched_expected_digest_refused() {
    let f = WebFixture::new().await;
    let csrf = f.step_up();
    let mut input = f.input(true, Some(csrf));
    input.expected_digest = "different".into();
    assert_eq!(f.post(input).await.unwrap_err().code, "digest_mismatch");
    f.service.env.broker.store.conn.lock().await.execute(
        "UPDATE approvals SET summary='changed after delivery' WHERE id=?1", [f.id.as_str()]).unwrap();
    assert_eq!(f.post(f.input(true, Some(csrf))).await.unwrap_err().code, "digest_mismatch");
    assert_eq!(f.post(f.input(false, None)).await.unwrap_err().code, "digest_mismatch");
    f.pending().await;
}

#[tokio::test]
async fn reject_without_step_up_records_web_false_and_approve_records_required() {
    for approve in [false,true] {
        let f = WebFixture::new().await;
        let csrf = approve.then(|| f.step_up());
        let result = f.post(f.input(approve, csrf)).await.unwrap();
        let by = format!("chat-admin:{}:telegram:web", f.service.user);
        assert_eq!(result["decided_by"], by);
        let saved = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
        assert_eq!(saved.decided_by.as_deref(), Some(by.as_str()));
        assert_eq!(saved.status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
        let conn = f.service.env.broker.store.conn.lock().await;
        let row: (String,bool,bool) = conn.query_row(
            "SELECT step,step_required,approve FROM chat_admin_decisions WHERE approval_id=?1",
            [f.id.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(row, ("web".into(), approve, approve));
    }
}

#[tokio::test]
async fn removed_receipt_or_revoked_admin_cannot_use_ticket() {
    let f = WebFixture::new().await;
    f.service.env.broker.store.conn.lock().await.execute(
        "UPDATE chat_admin_cards SET status='retired' WHERE card_id=?1", [&f.receipt.card_id]).unwrap();
    assert_eq!(f.post(f.input(false, None)).await.unwrap_err().code, "forbidden");
    let old = f.ctx();
    f.service.db().update_user(&f.service.user, None, Some(UserRole::Employee), None).unwrap();
    assert!(active_admin(f.service.db(), &old, &f.session).is_err());
    f.pending().await;
}

#[tokio::test]
async fn begin_never_decides_and_falls_back_without_https_totp_or_web() {
    for case in ["no_https", "no_totp", "dm_only", "web"] {
        let f = WebFixture::new().await;
        if case != "no_totp" { f.factor(); }
        let path = f.service.env.home.join("config.toml");
        let mut cfg = std::fs::read_to_string(&path).unwrap();
        if case == "dm_only" { cfg.push_str("step_up_methods=['dm_totp']\n"); }
        if case != "no_https" { cfg.push_str("[dashboard]\npublic_url='https://dashboard.example.test/base/'\n"); }
        std::fs::write(path, cfg).unwrap();
        let row = f.service.env.broker.get(&f.id).await.unwrap().unwrap();
        let result = begin(&f.service.env, &row, &f.receipt, Lang::ZhTw).await;
        if case == "web" {
            let link = result.card_link.unwrap();
            assert!(link.url.starts_with("https://dashboard.example.test/base/approvals/confirm#ticket="));
            let raw = link.url.split_once("#ticket=").unwrap().1;
            assert_eq!(raw.len(), 64);
            assert!(Store::open(&f.service.env.home).unwrap().preview(raw, &f.service.user).is_ok());
            assert_eq!(result.card_text.as_deref(), Some(card::step_up_needed_text(Lang::ZhTw)));
        } else {
            assert!(result.reply.starts_with("這類審批請到儀表板核准"));
            assert!(result.card_link.is_none());
            if case == "no_totp" { assert!(result.reply.contains("/inbox?item=")); }
        }
        assert_eq!(receipts::by_card_id(&f.service.env.broker.store, &f.receipt.card_id).await.unwrap().unwrap().status, "live");
        f.pending().await;
    }
}

#[tokio::test]
async fn raw_ticket_never_stored_and_cap_and_retention_are_bounded() {
    use sha2::{Digest,Sha256};
    let f = WebFixture::new().await;
    let mut store = Store::open(&f.service.env.home).unwrap();
    store.issue(&f.receipt).unwrap();
    store.issue(&f.receipt).unwrap();
    assert!(store.issue(&f.receipt).is_err());
    drop(store);
    let path = f.service.env.home.join("protected/approval_step_up.db");
    let conn = Connection::open(&path).unwrap();
    let hashes: Vec<String> = conn.prepare("SELECT ticket_hash FROM tickets").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert!(hashes.contains(&hex::encode(Sha256::digest(f.raw.as_bytes()))));
    assert!(!std::fs::read(&path).unwrap().windows(f.raw.len()).any(|w| w == f.raw.as_bytes()));
    f.sql("UPDATE tickets SET expires=strftime('%s','now')-86401");
    drop(Store::open(&f.service.env.home).unwrap());
    assert_eq!(conn.query_row("SELECT count(*) FROM tickets", [], |r|r.get::<_,i64>(0)).unwrap(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn permissions_and_symlinks_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = WebFixture::new().await;
    let directory = f.service.env.home.join("protected");
    let path = directory.join("approval_step_up.db");
    assert_eq!(std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let home = tempfile::tempdir().unwrap();
    symlink(&directory, home.path().join("protected")).unwrap();
    assert!(Store::open(home.path()).is_err());
    std::fs::remove_file(home.path().join("protected")).unwrap();
    std::fs::create_dir(home.path().join("protected")).unwrap();
    symlink(&path, home.path().join("protected/approval_step_up.db")).unwrap();
    assert!(Store::open(home.path()).is_err());
    std::fs::remove_file(home.path().join("protected/approval_step_up.db")).unwrap();
    symlink(&path, home.path().join("protected/approval_step_up.db-journal")).unwrap();
    assert!(Store::open(home.path()).is_err());
}

#[tokio::test]
async fn member_key_and_system_contexts_cannot_be_promoted_by_ticket() {
    let f = WebFixture::new().await;
    let mut member = f.ctx();
    member.member_key_id = Some(uuid::Uuid::new_v4().to_string());
    for ctx in [member, UserContext::admin_fallback()] {
        let result = routes::confirm(&f.service.env, &ctx, &f.session, &f.raw, f.input(false, None)).await;
        assert_eq!(result.unwrap_err().code, "unauthenticated");
    }
    f.pending().await;
}

#[tokio::test]
async fn credential_epoch_change_invalidates_old_session_and_step_up() {
    let f = WebFixture::new().await;
    let csrf = f.step_up();
    let old = f.ctx();
    Connection::open(f.service.env.home.join("users.db")).unwrap().execute(
        "UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.service.user]).unwrap();
    let error = routes::confirm(&f.service.env, &old, &f.session, &f.raw, f.input(true, Some(csrf))).await.unwrap_err();
    assert_eq!(error.code, "forbidden");
    assert_eq!(f.post(f.input(true, Some(csrf))).await.unwrap_err().code, "step_up_required");
    f.pending().await;
}

#[test]
fn web_link_requires_external_https_and_preserves_base_path() {
    let home = tempfile::tempdir().unwrap();
    for base in ["http://dashboard.example.test", "https://localhost", "https://127.0.0.1",
        "https://name:secret@dashboard.example.test", "https://dashboard.example.test/?ticket=x"] {
        std::fs::write(home.path().join("config.toml"), format!("[dashboard]\npublic_url='{base}'\n")).unwrap();
        assert!(landing_base(home.path()).is_none());
    }
    std::fs::write(home.path().join("config.toml"), "[dashboard]\npublic_url='https://dashboard.example.test/dudu/'\n").unwrap();
    assert_eq!(landing_base(home.path()).as_deref(), Some("https://dashboard.example.test/dudu/approvals/confirm"));
}

#[tokio::test]
async fn disabled_web_refuses_both_decisions_without_consuming_old_ticket() {
    for enabled in [true, false] {
        let f = WebFixture::new().await;
        let csrf = f.step_up();
        f.service.config(enabled, "telegram", &[]);
        let path = f.service.env.home.join("config.toml");
        let cfg = std::fs::read_to_string(&path).unwrap() + "step_up_methods=['dm_totp']\n";
        std::fs::write(path, cfg).unwrap();
        let proof = decision_service::ChatAdminProof { user_id:f.service.user.clone(), channel:config::ChatChannel::Telegram,
            account:f.receipt.account.clone(), principal:f.receipt.principal.clone(), dm:f.receipt.dm.clone(),
            card_id:f.receipt.card_id.clone(), digest:f.receipt.digest.clone(), step:super::super::ledger::Step::Web };
        for approve in [true, false] {
            assert!(f.post(f.input(approve, Some(csrf))).await.is_err());
            let result = decision_service::decide(&f.service.env, &f.id, approve, "",
                &decision_service::Decider::ChatAdmin(proof.clone())).await;
            if enabled {
                assert!(matches!(result, Err(decision_service::DecisionRefusal::StepUpRequired)));
            } else {
                assert!(matches!(result, Err(decision_service::DecisionRefusal::FeatureOff)));
            }
            assert!(Store::open(&f.service.env.home).unwrap().preview(&f.raw, &f.service.user).is_ok());
            f.pending().await;
        }
    }
}
