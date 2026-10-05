//! P2-B review fixes: the dashboard approval gate (C-1), the shared AI-session
//! check, forgetting a source that left no memory (H-1), same-turn replies
//! (H-2), import paths (H-3), cross-namespace lineage (M-1), labels (M-3) and
//! malformed host env (L-4).

use super::tests::approve;
use super::*;
use duduclaw_memory::SourceRef;

const AGENT: &str = "agnes";
const SESSION: &str = "telegram:777";

async fn op(home: &Path, cmd: ForgetSourceCommands) -> Result<CmdOutput> {
    run_with_markers(home, cmd, &[]).await
}

fn plan_cmd(agent: &str, session: &str, message: Option<&str>) -> ForgetSourceCommands {
    ForgetSourceCommands::Plan {
        agent: agent.into(),
        session: session.into(),
        message: message.map(|m| m.split(',').map(str::to_string).collect()),
        show_snippets: false,
        max_rows: None,
        ttl_minutes: None,
    }
}

fn apply_cmd(pid: &str) -> ForgetSourceCommands {
    ForgetSourceCommands::Apply {
        plan: pid.into(),
        confirm: true,
    }
}

fn plan_id_of(text: &str) -> String {
    text.split("--plan ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .unwrap()
        .to_string()
}

async fn seed_outcome(
    engine: &SqliteMemoryEngine,
    ns: &str,
    content: &str,
    src: SourceRef,
) -> duduclaw_memory::TemporalWriteOutcome {
    let entry = duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: ns.into(),
        content: content.into(),
        timestamp: chrono::Utc::now(),
        tags: vec![],
        embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "test".into(),
    };
    engine
        .store_temporal_outcome(
            ns,
            entry,
            Default::default(),
            duduclaw_memory::lineage::Provenance::source(src),
        )
        .await
        .unwrap()
}

async fn seed(engine: &SqliteMemoryEngine, ns: &str, content: &str, src: SourceRef) -> String {
    seed_outcome(engine, ns, content, src)
        .await
        .stored_id()
        .unwrap()
        .to_string()
}

fn refusal_reasons(home: &Path) -> String {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(AUDIT_REFUSED))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A home whose session has a user message, the employee's reply to it and
/// a second user message; one memory from the user message.
async fn home() -> (tempfile::TempDir, Vec<i64>, String) {
    let home = tempfile::tempdir().unwrap();
    for a in [AGENT, "bob"] {
        std::fs::create_dir_all(home.path().join("agents").join(a)).unwrap();
    }
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    mgr.get_or_create(SESSION, AGENT).await.unwrap();
    mgr.get_or_create("telegram:bob", "bob").await.unwrap();
    let mut seqs = Vec::new();
    for (role, text) in [
        ("user", "I like jasmine tea"),
        ("assistant", "noted, jasmine tea"),
        ("user", "unrelated"),
    ] {
        seqs.push(
            mgr.append_message_with_id(SESSION, role, text, 1)
                .await
                .unwrap(),
        );
    }
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let id = seed(
        &engine,
        AGENT,
        "likes jasmine tea",
        SourceRef::channel_message(SESSION, seqs[0], chrono::Utc::now(), None),
    )
    .await;
    (home, seqs, id)
}

#[tokio::test]
async fn apply_needs_an_approval_for_this_exact_plan() {
    let (home, seqs, id) = home().await;
    let planned = op(
        home.path(),
        plan_cmd(AGENT, SESSION, Some(&seqs[0].to_string())),
    )
    .await
    .unwrap();
    assert!(
        planned.text.contains(gate::APPROVE_IN_DASHBOARD),
        "{}",
        planned.text
    );
    let pid = plan_id_of(&planned.text);
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();

    // Not approved yet.
    let err = op(home.path(), apply_cmd(&pid))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(gate::APPROVE_IN_DASHBOARD), "{err}");
    assert!(engine.get_by_id(AGENT, &id).await.unwrap().is_some());

    // Approved, then the stored plan hash changes: refused.
    approve(home.path(), &pid).await;
    engine
        .conn_for_maintenance()
        .await
        .execute(
            "UPDATE memory_forget_plans SET plan_hash = ?2 WHERE plan_id = ?1",
            rusqlite::params![pid, "f".repeat(64)],
        )
        .unwrap();
    let err = op(home.path(), apply_cmd(&pid))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(gate::APPROVE_IN_DASHBOARD), "{err}");
    assert!(engine.get_by_id(AGENT, &id).await.unwrap().is_some());
    let reasons = refusal_reasons(home.path());
    assert!(reasons.contains("approval_pending"), "{reasons}");
    assert!(reasons.contains("approval_hash_mismatch"), "{reasons}");
}

#[tokio::test]
async fn an_expired_approval_request_is_refused_and_an_approved_one_applies() {
    let (home, seqs, id) = home().await;
    let pid = plan_id_of(
        &op(
            home.path(),
            plan_cmd(AGENT, SESSION, Some(&seqs[0].to_string())),
        )
        .await
        .unwrap()
        .text,
    );
    let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
    conn.execute(
        "UPDATE approvals SET created_at = '2000-01-01T00:00:00+00:00'",
        [],
    )
    .unwrap();
    drop(conn);
    let err = op(home.path(), apply_cmd(&pid))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(gate::APPROVE_IN_DASHBOARD), "{err}");
    assert!(refusal_reasons(home.path()).contains("approval_expired"));

    // A fresh plan, approved: applies.
    let pid = plan_id_of(
        &op(
            home.path(),
            plan_cmd(AGENT, SESSION, Some(&seqs[0].to_string())),
        )
        .await
        .unwrap()
        .text,
    );
    approve(home.path(), &pid).await;
    let out = op(home.path(), apply_cmd(&pid)).await.unwrap();
    assert!(out.complete, "{}", out.text);
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    assert!(engine.get_by_id(AGENT, &id).await.unwrap().is_none());
}

#[tokio::test]
async fn any_single_turn_variable_marks_an_ai_session() {
    let (home, _, _) = home().await;
    for marker in [
        duduclaw_core::ENV_TRUST_TURN_ID,
        duduclaw_core::ENV_DISPATCH_RUN_ID,
    ] {
        let r = run_with_markers(home.path(), plan_cmd(AGENT, SESSION, None), &[marker]).await;
        assert!(r.is_err(), "{marker}");
    }
    assert!(refusal_reasons(home.path()).contains("ai_session"));
}

/// H-2 + H-1: forgetting the user message also forgets its reply; a message
/// that left no memory still gets a plan (tombstone, hidden).
#[tokio::test]
async fn reply_is_included_and_a_memoryless_message_is_still_planned() {
    let (home, seqs, _) = home().await;
    let planned = op(
        home.path(),
        plan_cmd(AGENT, SESSION, Some(&seqs[0].to_string())),
    )
    .await
    .unwrap();
    assert!(
        planned
            .text
            .contains(&format!("#{} 的回覆 #{}", seqs[0], seqs[1])),
        "{}",
        planned.text
    );
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let plan = engine
        .get_forget_plan(&plan_id_of(&planned.text))
        .await
        .unwrap()
        .unwrap();
    assert!(
        plan.document
            .selector
            .messages
            .contains(&format!("m:{}", seqs[1]))
    );

    // The unrelated message has no memory: planned anyway, then applied.
    let planned = op(
        home.path(),
        plan_cmd(AGENT, SESSION, Some(&seqs[2].to_string())),
    )
    .await
    .unwrap();
    assert!(planned.text.contains("沒有可刪的記憶"), "{}", planned.text);
    let pid = plan_id_of(&planned.text);
    approve(home.path(), &pid).await;
    let out = op(home.path(), apply_cmd(&pid)).await.unwrap();
    assert!(
        out.text.contains("已設下之後不再學到的紀錄"),
        "{}",
        out.text
    );
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let left: Vec<String> = mgr
        .get_messages(SESSION)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert!(!left.iter().any(|c| c == "unrelated"), "{left:?}");
    // A later write from that message is fenced.
    let fenced = seed_outcome(
        &engine,
        AGENT,
        "late fact",
        SourceRef::channel_message(SESSION, seqs[2], chrono::Utc::now(), None),
    )
    .await;
    assert!(matches!(
        fenced,
        duduclaw_memory::TemporalWriteOutcome::Fenced(_)
    ));
}

/// M-1: another employee's conversation is allowed when this namespace's
/// lineage recorded it; the plan lists the commands for the others.
#[tokio::test]
async fn cross_namespace_follows_lineage_and_lists_other_namespaces() {
    let (home, _, _) = home().await;
    // bob's session, never recorded by agnes: refused.
    assert!(
        op(home.path(), plan_cmd(AGENT, "telegram:bob", None))
            .await
            .is_err()
    );
    // agnes recorded bob's message 1, and so did bob.
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    for ns in [AGENT, "bob"] {
        seed(
            &engine,
            ns,
            "relayed fact",
            SourceRef::channel_message("telegram:bob", 1, chrono::Utc::now(), None),
        )
        .await;
    }
    let out = op(home.path(), plan_cmd(AGENT, "telegram:bob", Some("1")))
        .await
        .unwrap();
    assert!(
        out.text.contains(
            "duduclaw memory forget-source plan --agent bob --session 'telegram:bob' --message 1"
        ),
        "{}",
        out.text
    );
}

/// M-3: collateral is shown as a channel/time/message label, not a digest.
#[tokio::test]
async fn collateral_is_labelled_for_humans() {
    let (home, seqs, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let both = duduclaw_memory::lineage::Provenance::Sources(vec![
        SourceRef::channel_message(SESSION, seqs[0], chrono::Utc::now(), None),
        SourceRef::channel_message(SESSION, seqs[2], chrono::Utc::now(), None),
    ]);
    let entry = duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: AGENT.into(),
        content: "two-source fact".into(),
        timestamp: chrono::Utc::now(),
        tags: vec![],
        embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "test".into(),
    };
    engine
        .store_temporal(AGENT, entry, Default::default(), both)
        .await
        .unwrap();
    let out = op(
        home.path(),
        plan_cmd(AGENT, SESSION, Some(&seqs[0].to_string())),
    )
    .await
    .unwrap();
    let plan = engine
        .get_forget_plan(&plan_id_of(&out.text))
        .await
        .unwrap()
        .unwrap();
    let digest = &plan.document.body.collateral[0].source_digest;
    assert!(!out.text.contains(digest.as_str()), "{}", out.text);
    assert!(
        out.text.contains(&format!("的使用者訊息 #{}", seqs[2])),
        "{}",
        out.text
    );
}

#[test]
fn import_paths_name_the_hashed_session() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.csv");
    std::fs::write(&f, "x").unwrap();
    let direct = import_session_arg(&format!("import:{}", f.display()));
    let dotted = import_session_arg(&format!("import:{}/./notes.csv", dir.path().display()));
    assert_eq!(direct, dotted);
    assert!(direct.starts_with("import:") && direct.len() == "import:".len() + 32);
    assert_eq!(
        import_session_arg(&direct),
        direct,
        "a hashed session is kept"
    );
    assert_eq!(import_session_arg("telegram:1"), "telegram:1");
}

/// L-4: a host-spawned process with a malformed turn or run identity is
/// refused; dispatch runs carry their run as the source (M-6).
#[test]
fn malformed_host_env_is_refused_and_runs_are_sources() {
    use crate::mcp_memory_handlers::{McpTurnEnv, mcp_write_sources_with};
    let no_session = McpTurnEnv {
        turn: Some("t-1".into()),
        ..Default::default()
    };
    assert!(mcp_write_sources_with(AGENT, &no_session).is_err());
    let bad_session = McpTurnEnv {
        session: Some("bad\u{7}session".into()),
        turn: Some("t-1".into()),
        ..Default::default()
    };
    assert!(mcp_write_sources_with(AGENT, &bad_session).is_err());
    let run_no_session = McpTurnEnv {
        run: Some("abc".into()),
        ..Default::default()
    };
    assert!(mcp_write_sources_with(AGENT, &run_no_session).is_err());

    let run = McpTurnEnv {
        run_session: Some("cron:agnes".into()),
        run: Some("0123abcd".into()),
        ..Default::default()
    };
    let s = mcp_write_sources_with(AGENT, &run).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].kind, duduclaw_memory::SourceKind::DispatchRun);
    assert_eq!(
        (s[0].session.as_str(), s[0].message.as_str()),
        ("cron:agnes", "run:0123abcd")
    );
}

async fn requests(home: &Path) -> usize {
    duduclaw_gateway::approval::ApprovalBroker::open(home)
        .unwrap()
        .list_by_kind(duduclaw_gateway::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE)
        .await
        .unwrap()
        .len()
}

/// N5: planning an already-forgotten source again creates no plan and no
/// approval request; it says so.
#[tokio::test]
async fn replanning_a_forgotten_source_says_so_and_files_nothing() {
    let (home, seqs, _) = home().await;
    let m = seqs[0].to_string();
    let pid = plan_id_of(
        &op(home.path(), plan_cmd(AGENT, SESSION, Some(&m)))
            .await
            .unwrap()
            .text,
    );
    approve(home.path(), &pid).await;
    assert!(op(home.path(), apply_cmd(&pid)).await.unwrap().complete);
    let before = requests(home.path()).await;
    let again = op(home.path(), plan_cmd(AGENT, SESSION, Some(&m)))
        .await
        .unwrap();
    assert!(again.text.contains("已經忘記過"), "{}", again.text);
    assert!(!again.text.contains("--plan "), "{}", again.text);
    assert_eq!(requests(home.path()).await, before);
}

/// N9: a session with surrounding spaces plans and applies with one value.
#[tokio::test]
async fn a_session_with_spaces_plans_and_applies() {
    let (home, seqs, id) = home().await;
    let out = op(
        home.path(),
        plan_cmd(AGENT, &format!("  {SESSION} "), Some(&seqs[0].to_string())),
    )
    .await
    .unwrap();
    assert!(out.text.contains("將刪除 1 筆記憶"), "{}", out.text);
    let pid = plan_id_of(&out.text);
    approve(home.path(), &pid).await;
    let applied = op(home.path(), apply_cmd(&pid)).await.unwrap();
    assert!(applied.complete, "{}", applied.text);
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    assert!(engine.get_by_id(AGENT, &id).await.unwrap().is_none());
}

/// N3: the card names the namespace, source, counts and creation time, and
/// says that nothing proves who ran the command.
#[tokio::test]
async fn the_card_says_it_came_from_a_local_command_line() {
    let (home, seqs, _) = home().await;
    op(
        home.path(),
        plan_cmd(AGENT, SESSION, Some(&seqs[0].to_string())),
    )
    .await
    .unwrap();
    let rec = duduclaw_gateway::approval::ApprovalBroker::open(home.path())
        .unwrap()
        .list_by_kind(duduclaw_gateway::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE)
        .await
        .unwrap()
        .remove(0);
    for want in [gate::ORIGIN_NOTICE, AGENT, SESSION, "記憶 1 筆", "建立於"] {
        assert!(
            rec.summary.contains(want),
            "{want} missing: {}",
            rec.summary
        );
    }
    assert!(!rec.summary.contains("jasmine"));
}

/// N6: the user sent two messages before one reply; forgetting the first
/// also forgets that reply.
#[tokio::test]
async fn a_reply_after_several_user_messages_is_included() {
    let (home, seqs, _) = home().await;
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let u1 = mgr
        .append_message_with_id(SESSION, "user", "first", 1)
        .await
        .unwrap();
    let u2 = mgr
        .append_message_with_id(SESSION, "user", "second", 1)
        .await
        .unwrap();
    let a = mgr
        .append_message_with_id(SESSION, "assistant", "both noted", 1)
        .await
        .unwrap();
    let _ = seqs;
    let out = op(home.path(), plan_cmd(AGENT, SESSION, Some(&u1.to_string())))
        .await
        .unwrap();
    assert!(
        out.text.contains(&format!("#{u1} 的回覆 #{a}")),
        "{}",
        out.text
    );
    assert!(!out.text.contains(&format!("#{u1} 的回覆 #{u2}")));
}
