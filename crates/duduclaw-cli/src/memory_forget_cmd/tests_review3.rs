//! Third review (2026-10-05): the printed other-namespace command works for
//! a dispatched employee (F1), the sender's "upstream dropped" marker (F4),
//! own session without a turn (F3), numeric message order in `list` (F7),
//! and how dispatch writes show in a forget of their upstream conversation.

use super::tests::{AGENT, SESSION, approve, home, mcp_write, op, plan_cmd, plan_id_of, turn_env};
use super::*;
use crate::mcp_memory_handlers::{McpTurnEnv, mcp_write_sources_with};
use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SourceKind, SourceRef, TemporalWriteOutcome};

const BOB: &str = "bob";

async fn write_in(
    engine: &SqliteMemoryEngine,
    ns: &str,
    sources: Vec<SourceRef>,
) -> TemporalWriteOutcome {
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: ns.into(),
        content: format!("stored by {ns}"),
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
            ns,
            entry,
            Default::default(),
            duduclaw_memory::lineage::Provenance::Sources(sources),
        )
        .await
        .unwrap()
}

/// Bob's MCP env while running work Agnes's turn `t` dispatched to him.
fn bob_dispatch(t: &str, run: &str) -> McpTurnEnv {
    McpTurnEnv {
        run_session: Some("dispatch:bob".into()),
        run: Some(run.into()),
        turn: Some(t.into()),
        session: Some(SESSION.into()),
        ..Default::default()
    }
}

async fn plan_and_apply(home: &Path, agent: &str, message: &str) -> ForgetPlan {
    let out = op(home, plan_cmd(agent, SESSION, Some(message)))
        .await
        .unwrap()
        .text;
    let pid = plan_id_of(&out);
    let engine = SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    let plan = engine.get_forget_plan(&pid).await.unwrap().unwrap();
    approve(home, &pid).await;
    let applied = op(
        home,
        ForgetSourceCommands::Apply {
            plan: pid,
            confirm: true,
        },
    )
    .await
    .unwrap();
    assert!(applied.complete, "{}", applied.text);
    plan
}

/// F1: Agnes's turn (message 1) dispatched work to Bob. The plan in Agnes's
/// namespace prints the command for Bob; running it after Agnes's forget
/// removes Bob's write and fences Bob's later writes from that turn.
#[tokio::test]
async fn the_printed_other_namespace_command_reaches_the_dispatched_write() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let at = duduclaw_memory::format_ts(chrono::Utc::now());
    mcp_write(
        &engine,
        mcp_write_sources_with(AGENT, &turn_env("t-9", Some("1"), Some(&at))).unwrap(),
    )
    .await;
    let bob_row = write_in(
        &engine,
        BOB,
        mcp_write_sources_with(BOB, &bob_dispatch("t-9", "r1")).unwrap(),
    )
    .await
    .stored_id()
    .unwrap()
    .to_string();

    let out = op(home.path(), plan_cmd(AGENT, SESSION, Some("1")))
        .await
        .unwrap()
        .text;
    let printed = format!(
        "duduclaw memory forget-source plan --agent {BOB} --session '{SESSION}' --message 1"
    );
    assert!(out.contains(&printed), "{out}");
    let pid = plan_id_of(&out);
    approve(home.path(), &pid).await;
    op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: pid,
            confirm: true,
        },
    )
    .await
    .unwrap();

    let plan = plan_and_apply(home.path(), BOB, "1").await;
    assert!(
        plan.document.body.targets.iter().any(|t| t.id == bob_row),
        "{:?}",
        plan.document.body.targets
    );
    assert!(engine.get_by_id(BOB, &bob_row).await.unwrap().is_none());
    let again = mcp_write_sources_with(BOB, &bob_dispatch("t-9", "r2")).unwrap();
    assert!(matches!(
        write_in(&engine, BOB, again).await,
        TemporalWriteOutcome::Fenced(_)
    ));
}

/// F4: the sender dropped a half pair and marked the message; the receiving
/// run records the upstream as unknown instead of as absent.
#[test]
fn a_dropped_upstream_marker_records_the_upstream_as_unknown() {
    let env = |flag: Option<&str>| McpTurnEnv {
        run_session: Some("dispatch:bob".into()),
        run: Some("r1".into()),
        upstream_unknown: flag.map(str::to_string),
        ..Default::default()
    };
    let kinds = |e: &McpTurnEnv| -> Vec<SourceKind> {
        mcp_write_sources_with(BOB, e)
            .unwrap()
            .into_iter()
            .map(|s| s.kind)
            .collect()
    };
    assert_eq!(
        kinds(&env(Some("1"))),
        vec![SourceKind::UpstreamUnknown, SourceKind::DispatchRun]
    );
    assert_eq!(kinds(&env(None)), vec![SourceKind::DispatchRun]);
    // A complete upstream wins over a stray marker.
    let mut full = env(Some("1"));
    full.turn = Some("t-1".into());
    full.session = Some(SESSION.into());
    assert_eq!(
        kinds(&full),
        vec![SourceKind::McpTurn, SourceKind::DispatchRun]
    );
}

/// F3, kept on purpose: an own session without a turn (the channel reply's
/// local-first inference tool loop runs that way) is recorded as an external
/// call, not refused; forgetting the conversation does not reach it.
#[test]
fn own_session_without_a_turn_falls_to_an_external_source_known_unreachable() {
    let env = McpTurnEnv {
        session: Some(SESSION.into()),
        ..Default::default()
    };
    let s = mcp_write_sources_with(AGENT, &env).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].kind, SourceKind::McpExternal);
    assert_eq!(s[0].session, format!("mcp:{AGENT}"));
}

/// F7: a turn recorded with two messages goes under the lower-numbered one
/// (`m:9` before `m:10`, not text order).
#[tokio::test]
async fn list_orders_message_numbers_numerically() {
    assert!(list::message_order("m:9") < list::message_order("m:10"));
    assert!(list::message_order("m:10") < list::message_order("run:a"));
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let now = chrono::Utc::now();
    mcp_write(
        &engine,
        vec![
            SourceRef::other(SourceKind::McpTurn, SESSION, "turn:t-2", now),
            SourceRef::channel_message(SESSION, 10, now, None),
            SourceRef::channel_message(SESSION, 9, now, None),
        ],
    )
    .await;
    let out = op(
        home.path(),
        ForgetSourceCommands::List {
            agent: AGENT.into(),
            session: Some(SESSION.into()),
        },
    )
    .await
    .unwrap()
    .text;
    let nine = out
        .lines()
        .find(|l| l.contains("→ --message 9"))
        .expect("message 9");
    assert!(nine.contains("含員工在這一輪自行存入"), "{out}");
}

/// How three dispatch writes show in a whole-conversation forget of their
/// upstream conversation: a complete upstream is a target; half an upstream
/// is not a target and is counted as untracked; no upstream at all is
/// neither (known unreachable: nothing ties it to the conversation).
#[tokio::test]
async fn dispatch_writes_in_an_upstream_forget_full_reached_half_untracked_none_unreachable() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let write = |env: McpTurnEnv| {
        let engine = &engine;
        async move {
            write_in(engine, AGENT, mcp_write_sources_with(AGENT, &env).unwrap())
                .await
                .stored_id()
                .unwrap()
                .to_string()
        }
    };
    let run = |k: &str| McpTurnEnv {
        run_session: Some("dispatch:agnes".into()),
        run: Some(k.into()),
        ..Default::default()
    };
    let none = write(run("r-none")).await;
    let half = write(McpTurnEnv {
        turn: Some("t-5".into()),
        ..run("r-half")
    })
    .await;
    let full = write(McpTurnEnv {
        turn: Some("t-6".into()),
        session: Some(SESSION.into()),
        ..run("r-full")
    })
    .await;
    let out = op(home.path(), plan_cmd(AGENT, SESSION, None))
        .await
        .unwrap()
        .text;
    let pid = plan_id_of(&out);
    let plan = engine.get_forget_plan(&pid).await.unwrap().unwrap();
    let ids: Vec<&str> = plan
        .document
        .body
        .targets
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert!(ids.contains(&full.as_str()), "{ids:?}");
    assert!(!ids.contains(&half.as_str()));
    assert!(!ids.contains(&none.as_str()), "known unreachable");
    assert_eq!(plan.document.body.untracked_in_namespace, 1);
}

