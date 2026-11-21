//! RM-191 review fix (MEDIUM): a fixed notice the pipeline returns before
//! the turn ensured its session (failsafe canned replies, budget refusal,
//! injection block, …) still takes the whole output pipeline through the
//! real guarded builders — CONTRACT.toml `must_not`, channel sanitising and
//! zh-TW normalisation; only secret restore is skipped. Its FINAL text is
//! what is delivered and what decides persistence: live with nothing stored
//! on a brand-new WebChat conversation, stored once in an existing one. The
//! failsafe and budget gates are triggered for real; no model is reached
//! (the cloud spy panics if one is). Not run here (owner forbids builds);
//! the reviewer runs them.
use super::*;
use duduclaw_security::failsafe::FailsafeLevel;

const FORBIDDEN: &str = "OPS-7731";
const HALTED: &str = "⚠️ 服務暫停中（維運代號 OPS-7731）";
const RESTRICTED: &str = "⚠️ 服務受限中（維運代號 OPS-7731）";
/// Matches the budget refusal shown when the budget cannot be confirmed.
const BUDGET_FORBIDDEN: &str = "無法確認預算";

/// One employee (`worker`, Claude, the default) with these failsafe canned
/// replies and CONTRACT.toml `must_not` rules.
async fn gate_home(halted: &str, restricted: &str, must_not: &[&str]) -> (tempfile::TempDir, ReplyContext) {
    let home = tempfile::tempdir().unwrap();
    crate::trigger_route::tests::write_agent(&home.path().join("agents"), "worker", "Worker", "@worker", "main", "");
    std::fs::write(home.path().join("config.toml"),
        "[general]\ndefault_agent='worker'\ndefault_language='zh-TW'\n[goal_intent]\nenabled=false\nmode='off'\n").unwrap();
    std::fs::write(home.path().join("KILLSWITCH.toml"), format!(
        "[failsafe]\ndefault_halted_reply = '{halted}'\ndefault_restricted_reply = '{restricted}'\n")).unwrap();
    let rules: Vec<String> = must_not.iter().map(|rule| format!("'{rule}'")).collect();
    std::fs::write(home.path().join("agents/worker/CONTRACT.toml"),
        format!("[boundaries]\nmust_not = [{}]\n", rules.join(", "))).unwrap();
    let registry = Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
    let sessions = Arc::new(SessionManager::new(&home.path().join("sessions.db")).unwrap());
    let status: ChannelStatusMap = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let ctx = ReplyContext::new(registry, home.path().to_path_buf(), sessions, status, tx);
    (home, ctx)
}

/// Make only the budget policy unreadable at turn time. Runtime/model
/// settings remain valid, so the early runtime gate allows the budget gate
/// to exercise its own fail-closed refusal. The registry is already loaded.
fn break_budget_config(home: &std::path::Path) {
    let path = home.join("agents/worker/agent.toml");
    let mut config: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    config.insert("budget".into(), "unreadable budget policy".into());
    std::fs::write(path, toml::to_string_pretty(&config).unwrap()).unwrap();
}

async fn set_level(ctx: &ReplyContext, sid: &str, level: FailsafeLevel) {
    ctx.failsafe.as_ref().expect("failsafe").set_level(sid, level, "rm191 test").await;
}

struct Turn {
    text: String,
    live_only: Option<String>,
    delivered: Result<(), ()>,
}

/// WebChat's `send_terminal` order: stage, then publish.
async fn deliver_webchat(ctx: &ReplyContext, sid: &str, reply: &GuardedReply) -> Result<(), ()> {
    reply.persist_webchat(&ctx.session_manager, sid, &reply.text).await?;
    let users = duduclaw_auth::UserDb::new(&ctx.home_dir.join("users.db")).unwrap();
    reply.publish_webchat(&ctx.session_manager, sid, &users, users.acl_generation().unwrap(),
        &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await
}

/// One turn through a real guarded builder (`for_agent`: the explicit-agent
/// entry WebChat, AG-UI and Odoo Live Chat use; otherwise the session entry
/// messaging channels use), delivered like WebChat on `webchat:` sessions.
async fn turn(ctx: &ReplyContext, sid: &str, text: &str, for_agent: bool) -> Turn {
    let (turn, cleanup) = with_delivery_cleanup(async {
        let reply = if for_agent {
            build_guarded_reply_for_agent(text, ctx, "worker", sid, "rm191-user", None).await
        } else {
            build_guarded_reply_with_session(text, ctx, sid, "rm191-user", None).await
        };
        let live_only = reply.rollback.as_ref().and_then(|rollback| rollback.delivery.live_only_text());
        let delivered = if sid.starts_with("webchat:") { deliver_webchat(ctx, sid, &reply).await } else { Ok(()) };
        Turn { text: reply.text.clone(), live_only, delivered }
    }).await;
    cleanup.unwrap();
    turn
}

/// Every stored row of `sid`, hidden or not.
fn raw_contents(home: &std::path::Path, sid: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(home.join("sessions.db")).unwrap();
    let mut stmt = conn.prepare("SELECT content FROM session_messages WHERE session_id = ?1").unwrap();
    let rows: Vec<String> = stmt.query_map([sid], |row| row.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap();
    rows
}

async fn assert_nothing_stored(ctx: &ReplyContext, sid: &str) {
    assert!(ctx.session_manager.turn_generation(sid).await.unwrap().is_none(), "{sid}: no session row");
    assert!(raw_contents(&ctx.home_dir, sid).is_empty(), "{sid}: nothing stored");
}

async fn assert_stored_once(ctx: &ReplyContext, sid: &str, text: &str) {
    let assistant: Vec<String> = ctx.session_manager.get_messages(sid).await.unwrap().into_iter()
        .filter(|m| m.role == "assistant").map(|m| m.content).collect();
    assert_eq!(assistant, vec![text.to_string()], "{sid}: stored once as the final text");
}

fn assert_contract_block(sid: &str, t: &Turn, forbidden: &str) {
    // The replacement itself still passes language normalisation.
    let block = crate::prompt_identity::normalize_channel_reply(CONTRACT_BLOCK_MESSAGE, Some("zh-TW"));
    assert_eq!(t.delivered, Ok(()), "{sid}: delivered, not refused");
    assert!(t.text.contains(&block), "{sid}: the contract's replacement: {}", t.text);
    assert!(!t.text.contains(forbidden), "{sid}: forbidden content delivered: {}", t.text);
}

/// Item 1 (failsafe canned replies): with a `must_not` rule matching the
/// operator's canned text, the delivered text is the contract's block on a
/// brand-new WebChat conversation (live only, nothing stored, the marker
/// bound to the replacement) and in an existing one (the replacement is
/// what is stored). Both guarded entry points, plus a messaging channel.
#[tokio::test]
async fn contract_blocks_a_forbidden_failsafe_notice_on_new_and_existing_conversations() {
    let (home, ctx) = gate_home(HALTED, RESTRICTED, &[FORBIDDEN]).await;
    let spy = NoImplicitCloud::at(home.path());
    for for_agent in [false, true] {
        for level in [FailsafeLevel::L4Halted, FailsafeLevel::L2Restricted] {
            // (a) brand-new conversation: live-only path.
            let sid = format!("webchat:rm191-contract-new-{for_agent}-{level:?}");
            set_level(&ctx, &sid, level).await;
            let t = turn(&ctx, &sid, "hello", for_agent).await;
            assert_contract_block(&sid, &t, FORBIDDEN);
            assert_eq!(t.live_only.as_deref(), Some(t.text.as_str()), "{sid}: bound to the final text");
            assert_nothing_stored(&ctx, &sid).await;
            // (b) existing conversation: the replacement is stored.
            let sid = format!("webchat:rm191-contract-old-{for_agent}-{level:?}");
            ctx.session_manager.get_or_create(&sid, "worker").await.unwrap();
            set_level(&ctx, &sid, level).await;
            let t = turn(&ctx, &sid, "hello", for_agent).await;
            assert_contract_block(&sid, &t, FORBIDDEN);
            assert_stored_once(&ctx, &sid, &t.text).await;
            assert!(raw_contents(home.path(), &sid).iter().all(|c| !c.contains(FORBIDDEN)), "{sid}: nothing forbidden stored");
            // Messaging channel (no WebChat persistence): same replacement.
            let sid = format!("line:rm191-contract-{for_agent}-{level:?}");
            set_level(&ctx, &sid, level).await;
            let t = turn(&ctx, &sid, "hello", for_agent).await;
            assert_contract_block(&sid, &t, FORBIDDEN);
        }
    }
    // The block is audited like any other contract violation.
    let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(log.lines().any(|l| l.contains("contract_violation")), "contract violation audited");
    assert!(!log.contains(HALTED), "the blocked text is not audited");
    assert_eq!(spy.counts(), [0, 0], "no model call");
}

/// Item 1 (budget refusal): the real budget gate refuses (its configuration
/// cannot be read) and a `must_not` rule matches the refusal text. New and
/// existing WebChat conversations get the contract's block, never the
/// refusal text, delivered or stored.
#[tokio::test]
async fn contract_blocks_a_forbidden_budget_refusal_on_new_and_existing_conversations() {
    let (home, ctx) = gate_home(HALTED, RESTRICTED, &[BUDGET_FORBIDDEN]).await;
    let spy = NoImplicitCloud::at(home.path());
    break_budget_config(home.path());
    for for_agent in [false, true] {
        let sid = format!("webchat:rm191-budget-new-{for_agent}");
        let t = turn(&ctx, &sid, "hello", for_agent).await;
        assert_contract_block(&sid, &t, BUDGET_FORBIDDEN);
        assert_eq!(t.live_only.as_deref(), Some(t.text.as_str()), "{sid}");
        assert_nothing_stored(&ctx, &sid).await;
        let sid = format!("webchat:rm191-budget-old-{for_agent}");
        ctx.session_manager.get_or_create(&sid, "worker").await.unwrap();
        let t = turn(&ctx, &sid, "hello", for_agent).await;
        assert_contract_block(&sid, &t, BUDGET_FORBIDDEN);
        assert_stored_once(&ctx, &sid, &t.text).await;
        assert!(raw_contents(home.path(), &sid).iter().all(|c| !c.contains(BUDGET_FORBIDDEN)), "{sid}");
    }
    assert_eq!(spy.counts(), [0, 0], "no model call");
}

/// Item 2: channel sanitising and zh-TW normalisation still apply to a
/// marked notice. An operator canned reply naming a local path and written
/// in Simplified characters is delivered with the path redacted and in
/// Traditional characters — live only on a new conversation, and stored as
/// that same final text in an existing one.
#[tokio::test]
async fn sanitising_and_language_normalisation_apply_to_a_marked_notice() {
    let raw = "⚠️ 服务暂停，说明见 /Users/ops/rm191-runbook.md";
    let (home, ctx) = gate_home(raw, raw, &[]).await;
    let spy = NoImplicitCloud::at(home.path());
    let check = |sid: &str, t: &Turn| {
        assert_eq!(t.delivered, Ok(()), "{sid}");
        assert!(!t.text.contains("/Users/"), "{sid}: path not sanitised: {}", t.text);
        assert!(t.text.contains("[path]"), "{sid}: {}", t.text);
        assert!(t.text.contains("服務") && !t.text.contains("服务"), "{sid}: not normalised: {}", t.text);
    };
    for for_agent in [false, true] {
        let sid = format!("webchat:rm191-sanitise-new-{for_agent}");
        set_level(&ctx, &sid, FailsafeLevel::L4Halted).await;
        let t = turn(&ctx, &sid, "hello", for_agent).await;
        check(&sid, &t);
        assert_eq!(t.live_only.as_deref(), Some(t.text.as_str()), "{sid}: the sanitised text is the live-only one");
        assert_nothing_stored(&ctx, &sid).await;
        let sid = format!("webchat:rm191-sanitise-old-{for_agent}");
        ctx.session_manager.get_or_create(&sid, "worker").await.unwrap();
        set_level(&ctx, &sid, FailsafeLevel::L4Halted).await;
        let t = turn(&ctx, &sid, "hello", for_agent).await;
        check(&sid, &t);
        assert_stored_once(&ctx, &sid, &t.text).await;
        assert!(raw_contents(home.path(), &sid).iter().all(|c| !c.contains("/Users/")), "{sid}");
        let sid = format!("line:rm191-sanitise-{for_agent}");
        set_level(&ctx, &sid, FailsafeLevel::L4Halted).await;
        check(&sid, &turn(&ctx, &sid, "hello", for_agent).await);
    }
    assert_eq!(spy.counts(), [0, 0], "no model call");
}

/// Item 7 (real gates): failsafe halted, failsafe restricted and the budget
/// refusal. New conversation: delivered live, nothing stored. Existing
/// conversation: stored once as the assistant notice. (Breaker, killswitch
/// and redaction-failure texts are covered at the delivery layer in
/// `live_only_notice.rs`; their gates are not reachable from a test home.)
#[tokio::test]
async fn real_gate_refusals_are_live_only_when_new_and_stored_once_when_existing() {
    for kind in ["failsafe halted", "failsafe restricted", "budget"] {
        let (home, ctx) = gate_home(HALTED, RESTRICTED, &[]).await;
        let spy = NoImplicitCloud::at(home.path());
        if kind == "budget" {
            break_budget_config(home.path());
        }
        let level = match kind {
            "failsafe halted" => Some(FailsafeLevel::L4Halted),
            "failsafe restricted" => Some(FailsafeLevel::L2Restricted),
            _ => None,
        };
        let new_sid = "webchat:rm191-gate-new";
        let old_sid = "webchat:rm191-gate-old";
        ctx.session_manager.get_or_create(old_sid, "worker").await.unwrap();
        if let Some(level) = level {
            set_level(&ctx, new_sid, level).await;
            set_level(&ctx, old_sid, level).await;
        }
        let new = turn(&ctx, new_sid, "hello", true).await;
        let old = turn(&ctx, old_sid, "hello", true).await;
        let expected = match kind {
            "failsafe halted" => "服務暫停中",
            "failsafe restricted" => "服務受限中",
            _ => BUDGET_FORBIDDEN,
        };
        for t in [&new, &old] {
            assert_eq!(t.delivered, Ok(()), "{kind}");
            assert!(t.text.contains(expected), "{kind}: {}", t.text);
        }
        assert_eq!(new.live_only.as_deref(), Some(new.text.as_str()), "{kind}");
        assert_nothing_stored(&ctx, new_sid).await;
        assert_stored_once(&ctx, old_sid, &old.text).await;
        assert_eq!(raw_contents(home.path(), old_sid).len(), 1, "{kind}: only the notice row");
        assert_eq!(spy.counts(), [0, 0], "{kind}: no model call");
    }
}
