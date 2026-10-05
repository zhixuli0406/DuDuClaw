//! CLI tests for `duduclaw memory forget-source` (P2-B): operator-only entry,
//! namespace checks, plan/apply lifecycle, and privacy of the audit trail.

use super::*;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::SourceRef;

pub(super) const AGENT: &str = "agnes";
pub(super) const SESSION: &str = "telegram:777";
const SECRET: &str = "prefers jasmine tea";

/// A home with employee directories, a session owned by `AGENT` with two
/// messages, and one memory row per message.
pub(super) async fn home() -> (tempfile::TempDir, Vec<String>) {
    let home = tempfile::tempdir().unwrap();
    for a in [AGENT, "bob"] {
        std::fs::create_dir_all(home.path().join("agents").join(a)).unwrap();
    }
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    mgr.get_or_create(SESSION, AGENT).await.unwrap();
    mgr.get_or_create("telegram:bob", "bob").await.unwrap();
    let mut ids = Vec::new();
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    for text in [SECRET, "second thing"] {
        let seq = mgr
            .append_message_with_id(SESSION, "user", text, 1)
            .await
            .unwrap();
        ids.push(store(&engine, text, seq).await);
    }
    (home, ids)
}

async fn store(engine: &SqliteMemoryEngine, text: &str, seq: i64) -> String {
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: AGENT.into(),
        content: text.into(),
        timestamp: chrono::Utc::now(),
        tags: vec![],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "test".into(),
    };
    let src = SourceRef::channel_message(SESSION, seq, chrono::Utc::now(), None);
    engine
        .store_temporal(
            AGENT,
            entry,
            Default::default(),
            duduclaw_memory::lineage::Provenance::source(src),
        )
        .await
        .unwrap()
}

pub(super) fn plan_cmd(agent: &str, session: &str, message: Option<&str>) -> ForgetSourceCommands {
    ForgetSourceCommands::Plan {
        agent: agent.into(),
        session: session.into(),
        message: message.map(|m| vec![m.to_string()]),
        show_snippets: false,
        max_rows: None,
        ttl_minutes: None,
    }
}

pub(super) async fn op(home: &Path, cmd: ForgetSourceCommands) -> Result<CmdOutput> {
    run_with_env(home, cmd, "", "").await
}

/// Approve a plan's request as an Admin would in the dashboard.
pub(super) async fn approve(home: &Path, plan_id: &str) {
    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home).unwrap();
    let rec = broker
        .list_by_kind(duduclaw_gateway::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.payload["plan_id"] == plan_id)
        .expect("a request was filed with the plan");
    broker
        .decide(&rec.id, true, "dashboard:admin")
        .await
        .unwrap();
}

pub(super) fn plan_id_of(text: &str) -> String {
    text.split("--plan ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .unwrap()
        .to_string()
}

pub(super) fn audit(home: &Path) -> String {
    std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default()
}

fn refusal_reasons(home: &Path) -> Vec<String> {
    audit(home)
        .lines()
        .filter(|l| l.contains(AUDIT_REFUSED))
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v.to_string()
        })
        .collect()
}

#[test]
fn ai_session_identity_is_refused() {
    assert!(agent_session_refusal("", "").is_none());
    assert!(agent_session_refusal("agnes", "").is_some());
    assert!(agent_session_refusal("", "tok").is_some());
}

#[tokio::test]
async fn every_subcommand_refuses_an_ai_session_and_audits_it() {
    let (home, _) = home().await;
    for cmd in [
        plan_cmd(AGENT, SESSION, None),
        ForgetSourceCommands::Apply {
            plan: "x".into(),
            confirm: true,
        },
        ForgetSourceCommands::Resume { plan: "x".into() },
        ForgetSourceCommands::List {
            agent: AGENT.into(),
            session: None,
        },
        ForgetSourceCommands::Show { plan: "x".into() },
    ] {
        assert!(
            run_with_env(home.path(), cmd, AGENT, "token")
                .await
                .is_err()
        );
    }
    let reasons = refusal_reasons(home.path());
    assert_eq!(reasons.len(), 5);
    assert!(reasons.iter().all(|r| r.contains("ai_session")));
}

#[tokio::test]
async fn cross_namespace_unknown_namespace_and_stray_db_are_refused() {
    let (home, _) = home().await;
    assert!(
        op(home.path(), plan_cmd(AGENT, "telegram:bob", None))
            .await
            .is_err()
    );
    assert!(
        op(home.path(), plan_cmd("nobody", SESSION, None))
            .await
            .is_err()
    );
    std::fs::create_dir_all(home.path().join("agents/agnes/state")).unwrap();
    std::fs::write(home.path().join("agents/agnes/state/memory.db"), b"").unwrap();
    assert!(
        op(home.path(), plan_cmd(AGENT, SESSION, None))
            .await
            .is_err()
    );
    let reasons = refusal_reasons(home.path()).join("\n");
    for r in ["cross_namespace", "unknown_namespace", "stray_db"] {
        assert!(reasons.contains(r), "{r} missing in {reasons}");
    }
}

#[tokio::test]
async fn disabled_config_refuses_plan() {
    let (home, _) = home().await;
    std::fs::write(
        home.path().join("config.toml"),
        "[memory]\nforget_source = false\n",
    )
    .unwrap();
    assert!(
        op(home.path(), plan_cmd(AGENT, SESSION, None))
            .await
            .is_err()
    );
    std::fs::write(
        home.path().join("config.toml"),
        "[memory]\nforget_source = \"yes\"\n",
    )
    .unwrap();
    assert!(
        !forget_source_enabled(home.path()),
        "non-boolean fails closed"
    );
    std::fs::write(home.path().join("config.toml"), "[memory]\n").unwrap();
    assert!(forget_source_enabled(home.path()));
}

#[tokio::test]
async fn plan_then_apply_forgets_one_message_and_hides_it() {
    let (home, ids) = home().await;
    let planned = op(home.path(), plan_cmd(AGENT, SESSION, Some("1")))
        .await
        .unwrap();
    assert!(planned.text.contains("將刪除 1 筆記憶"), "{}", planned.text);
    assert!(!planned.text.contains(SECRET), "no snippets unless asked");
    let pid = plan_id_of(&planned.text);

    // Without --confirm nothing happens.
    let preview = op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: pid.clone(),
            confirm: false,
        },
    )
    .await
    .unwrap();
    assert!(preview.text.contains("--confirm"));
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    assert!(engine.get_by_id(AGENT, &ids[0]).await.unwrap().is_some());

    approve(home.path(), &pid).await;
    let applied = op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: pid.clone(),
            confirm: true,
        },
    )
    .await
    .unwrap();
    assert!(applied.complete, "{}", applied.text);
    assert!(applied.text.contains("COMPLETE"));
    assert!(engine.get_by_id(AGENT, &ids[0]).await.unwrap().is_none());
    assert!(engine.get_by_id(AGENT, &ids[1]).await.unwrap().is_some());
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let left: Vec<String> = mgr
        .get_messages(SESSION)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(left, vec!["second thing".to_string()]);

    // A re-apply only re-runs steps.
    let again = op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: pid,
            confirm: true,
        },
    )
    .await
    .unwrap();
    assert!(again.text.contains("先前已套用"));

    // Privacy: no content and no raw session id in any forget audit row.
    let audit = audit(home.path());
    let ours: Vec<&str> = audit
        .lines()
        .filter(|l| l.contains("memory_source_forget"))
        .collect();
    assert!(ours.iter().any(|l| l.contains(AUDIT_PLANNED)));
    assert!(ours.iter().any(|l| l.contains(AUDIT_APPLIED)));
    assert!(
        ours.iter()
            .all(|l| !l.contains("jasmine") && !l.contains(SESSION)),
        "{ours:?}"
    );
}

#[tokio::test]
async fn show_snippets_prints_content_without_storing_it() {
    let (home, _) = home().await;
    let mut cmd = plan_cmd(AGENT, SESSION, Some("1"));
    if let ForgetSourceCommands::Plan { show_snippets, .. } = &mut cmd {
        *show_snippets = true;
    }
    let out = op(home.path(), cmd).await.unwrap();
    assert!(out.text.contains(SECRET));
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let plan = engine
        .get_forget_plan(&plan_id_of(&out.text))
        .await
        .unwrap()
        .unwrap();
    assert!(!plan.document.canonical_json().unwrap().contains("jasmine"));
}

#[tokio::test]
async fn expired_and_stale_plans_are_refused_without_deleting() {
    let (home, ids) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();

    // Expired.
    let pid = plan_id_of(
        &op(home.path(), plan_cmd(AGENT, SESSION, Some("1")))
            .await
            .unwrap()
            .text,
    );
    engine
        .conn_for_maintenance()
        .await
        .execute(
            "UPDATE memory_forget_plans
             SET plan_json = json_set(plan_json, '$.expires_at', '2000-01-01T00:00:00.000000Z')
             WHERE plan_id = ?1",
            [&pid],
        )
        .unwrap();
    approve(home.path(), &pid).await;
    assert!(
        op(
            home.path(),
            ForgetSourceCommands::Apply {
                plan: pid,
                confirm: true
            }
        )
        .await
        .is_err()
    );

    // Stale: a new row from the same message after the plan.
    let pid = plan_id_of(
        &op(home.path(), plan_cmd(AGENT, SESSION, Some("1")))
            .await
            .unwrap()
            .text,
    );
    store(&engine, "another from message 1", 1).await;
    approve(home.path(), &pid).await;
    let r = op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: pid,
            confirm: true,
        },
    )
    .await;
    assert!(r.is_err(), "{r:?}");
    assert!(
        engine.get_by_id(AGENT, &ids[0]).await.unwrap().is_some(),
        "nothing deleted"
    );

    let reasons = refusal_reasons(home.path()).join("\n");
    assert!(
        reasons.contains("expired") && reasons.contains("stale"),
        "{reasons}"
    );
}

#[tokio::test]
async fn whole_session_forget_uses_the_current_watermark() {
    let (home, ids) = home().await;
    let pid = plan_id_of(
        &op(home.path(), plan_cmd(AGENT, SESSION, None))
            .await
            .unwrap()
            .text,
    );
    approve(home.path(), &pid).await;
    let out = op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: pid,
            confirm: true,
        },
    )
    .await
    .unwrap();
    assert!(out.complete);
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    assert!(engine.get_by_id(AGENT, &ids[1]).await.unwrap().is_none());
    // A message after the watermark is a new source.
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let seq = mgr
        .append_message_with_id(SESSION, "user", "later", 1)
        .await
        .unwrap();
    store(&engine, "later fact", seq).await;
}

#[tokio::test]
async fn nothing_to_forget_records_no_plan() {
    let (home, _) = home().await;
    let out = op(home.path(), plan_cmd(AGENT, SESSION, Some("99")))
        .await
        .unwrap();
    assert!(out.text.contains("查無"));
    assert!(!audit(home.path()).contains(AUDIT_PLANNED));
}

#[test]
fn message_keys_accept_numbers_and_known_prefixes_only() {
    assert_eq!(
        message_keys(&["812".into(), "run:abc".into()]).unwrap(),
        vec!["m:812".to_string(), "run:abc".to_string()]
    );
    assert!(message_keys(&["-1".into()]).is_err());
    assert!(message_keys(&["DROP TABLE".into()]).is_err());
}

#[test]
fn namespaces_are_validated() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/agnes")).unwrap();
    assert!(valid_namespace(home.path(), "agnes"));
    assert!(valid_namespace(home.path(), "external/client-1"));
    assert!(valid_namespace(home.path(), "internal/gateway-internal"));
    assert!(!valid_namespace(home.path(), "ghost"));
    assert!(!valid_namespace(home.path(), "external/../x"));
    assert!(!valid_namespace(home.path(), "../agents"));
}

pub(super) fn turn_env(
    turn: &str,
    user_seq: Option<&str>,
    user_at: Option<&str>,
) -> crate::mcp_memory_handlers::McpTurnEnv {
    crate::mcp_memory_handlers::McpTurnEnv {
        session: Some(SESSION.into()),
        turn: Some(turn.into()),
        user_seq: user_seq.map(str::to_string),
        user_at: user_at.map(str::to_string),
        ..Default::default()
    }
}

pub(super) async fn mcp_write(
    engine: &SqliteMemoryEngine,
    sources: Vec<SourceRef>,
) -> duduclaw_memory::TemporalWriteOutcome {
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: AGENT.into(),
        content: "stored by the employee".into(),
        timestamp: chrono::Utc::now(),
        tags: vec![],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "mcp_internal".into(),
    };
    engine
        .store_temporal_outcome(
            AGENT,
            entry,
            Default::default(),
            duduclaw_memory::lineage::Provenance::Sources(sources),
        )
        .await
        .unwrap()
}

pub(super) async fn forget_keys(home: &Path, keys: &[&str]) -> ForgetPlan {
    let mut cmd = plan_cmd(AGENT, SESSION, None);
    if let ForgetSourceCommands::Plan { message, .. } = &mut cmd {
        *message = Some(keys.iter().map(|k| k.to_string()).collect());
    }
    let pid = plan_id_of(&op(home, cmd).await.unwrap().text);
    let engine = SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    let plan = engine.get_forget_plan(&pid).await.unwrap().unwrap();
    approve(home, &pid).await;
    assert!(
        op(
            home,
            ForgetSourceCommands::Apply {
                plan: pid,
                confirm: true
            }
        )
        .await
        .unwrap()
        .complete
    );
    plan
}

/// C1/C2: sources of an MCP write — turn identity only for an employee with
/// a host turn, never for an external client; a malformed user-message pair
/// is dropped (the write still has its turn source).
#[test]
fn mcp_write_sources_follow_the_host_env() {
    use crate::mcp_memory_handlers::{McpTurnEnv, mcp_write_sources_with};
    let at = "2026-10-05T01:02:03.000000Z";
    let s = mcp_write_sources_with(AGENT, &turn_env("t-9", Some("12"), Some(at))).unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].kind, duduclaw_memory::SourceKind::McpTurn);
    assert_eq!(
        (s[0].session.as_str(), s[0].message.as_str()),
        (SESSION, "turn:t-9")
    );
    assert_eq!(s[1].kind, duduclaw_memory::SourceKind::ChannelMessage);
    assert_eq!((s[1].message.as_str(), s[1].seq), ("m:12", Some(12)));
    // Dispatch-shaped turn (no user message): the turn source only.
    let s = mcp_write_sources_with(AGENT, &turn_env("t-9", None, None)).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].kind, duduclaw_memory::SourceKind::McpTurn);
    // A malformed or half user-message pair on the process's own turn is
    // refused (`.mcp.json` platform fix, item 3: fail closed; was ignored).
    for (seq, when) in [
        (Some("x"), Some(at)),
        (Some("-1"), Some(at)),
        (Some("3"), Some("yesterday")),
        (Some("3"), None),
        (Some(""), Some(at)),
    ] {
        assert!(
            mcp_write_sources_with(AGENT, &turn_env("t-9", seq, when)).is_err(),
            "{seq:?} {when:?}"
        );
    }
    let ext =
        mcp_write_sources_with("external/c1", &turn_env("t-9", Some("12"), Some(at))).unwrap();
    assert_eq!(
        (ext.len(), ext[0].kind),
        (1, duduclaw_memory::SourceKind::McpExternal)
    );
    assert_eq!(ext[0].session, "mcp:c1");
    let none = mcp_write_sources_with(AGENT, &McpTurnEnv::default()).unwrap();
    assert_eq!(none[0].kind, duduclaw_memory::SourceKind::McpExternal);
}

/// Revision A: what the employee stored with `memory_store` during the turn
/// of a user message is in the plan for that message, goes at apply, and a
/// later write from the same turn is fenced.
#[tokio::test]
async fn forgetting_a_message_reaches_what_the_employee_stored_during_its_turn() {
    let (home, ids) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let at = duduclaw_memory::format_ts(chrono::Utc::now());
    let env = turn_env("t-1", Some("1"), Some(&at));
    let stored = mcp_write(
        &engine,
        crate::mcp_memory_handlers::mcp_write_sources_with(AGENT, &env).unwrap(),
    )
    .await
    .stored_id()
    .unwrap()
    .to_string();
    // The same employee's dispatch-path write (no user message) is unrelated.
    let dispatch = mcp_write(
        &engine,
        crate::mcp_memory_handlers::mcp_write_sources_with(AGENT, &turn_env("t-2", None, None))
            .unwrap(),
    )
    .await
    .stored_id()
    .unwrap()
    .to_string();

    let plan = forget_keys(home.path(), &["m:1"]).await;
    let targets: Vec<&str> = plan
        .document
        .body
        .targets
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert!(targets.contains(&stored.as_str()), "{targets:?}");
    assert!(targets.contains(&ids[0].as_str()));
    assert!(!targets.contains(&dispatch.as_str()));
    assert!(engine.get_by_id(AGENT, &stored).await.unwrap().is_none());
    assert!(engine.get_by_id(AGENT, &dispatch).await.unwrap().is_some());
    assert!(matches!(
        mcp_write(
            &engine,
            crate::mcp_memory_handlers::mcp_write_sources_with(AGENT, &env).unwrap()
        )
        .await,
        duduclaw_memory::TemporalWriteOutcome::Fenced(_)
    ));
}

/// Forgetting a turn by its key fences a later write from that turn; the
/// next turn writes normally.
#[tokio::test]
async fn mcp_turn_source_is_fenced_once_its_turn_is_forgotten() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let src = |t: &str| {
        crate::mcp_memory_handlers::mcp_write_sources_with(AGENT, &turn_env(t, None, None)).unwrap()
    };
    assert!(mcp_write(&engine, src("t-9")).await.stored_id().is_some());
    forget_keys(home.path(), &["turn:t-9"]).await;
    assert!(matches!(
        mcp_write(&engine, src("t-9")).await,
        duduclaw_memory::TemporalWriteOutcome::Fenced(_)
    ));
    assert!(mcp_write(&engine, src("t-10")).await.stored_id().is_some());
}
