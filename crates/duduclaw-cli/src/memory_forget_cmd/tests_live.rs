//! Fixes from the 2026-10-05 live verification of forget by source: the
//! turn of a forgotten message (issues 1, 2), bus messages with half an
//! upstream identity (issue 3), RL trajectories in the not-covered list
//! (issue 4), the summary line of a run plan (issue 5), every stale reason
//! (issue 6), and the `list` / `show` / `resume` output (doc mismatches).

use super::tests::{
    AGENT, SESSION, approve, audit, forget_keys, home, mcp_write, op, plan_cmd, plan_id_of,
    turn_env,
};
use super::*;
use crate::mcp_memory_handlers::{McpTurnEnv, mcp_write_sources_with};
use duduclaw_memory::{SourceKind, TemporalWriteOutcome};

fn now_ts() -> String {
    duduclaw_memory::format_ts(chrono::Utc::now())
}

/// The live repro: forget message 1 after the employee stored something in
/// its turn; a later write naming only the turn (same turn and session, no
/// user message) is refused and audited; a new turn of the session writes.
#[tokio::test]
async fn a_turn_only_replay_after_forgetting_its_message_is_fenced_and_audited() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let at = now_ts();
    let stored = mcp_write(
        &engine,
        mcp_write_sources_with(AGENT, &turn_env("t-1", Some("1"), Some(&at))).unwrap(),
    )
    .await
    .stored_id()
    .unwrap()
    .to_string();

    let plan = forget_keys(home.path(), &["1"]).await;
    let target = plan
        .document
        .body
        .targets
        .iter()
        .find(|t| t.id == stored)
        .expect("the turn's write is a target");
    assert!(target.other_sources.is_empty(), "{target:?}");
    assert!(plan.document.body.collateral.is_empty());

    let replay = mcp_write_sources_with(AGENT, &turn_env("t-1", None, None)).unwrap();
    let TemporalWriteOutcome::Fenced(r) = mcp_write(&engine, replay).await else {
        panic!("a turn-only write from the forgotten turn must be fenced");
    };
    duduclaw_gateway::memory_provenance::record_fenced(home.path(), AGENT, "mcp_memory_store", &r);
    assert!(audit(home.path()).contains("memory_write_fenced"));

    let next = mcp_write_sources_with(AGENT, &turn_env("t-2", None, None)).unwrap();
    assert!(mcp_write(&engine, next).await.stored_id().is_some());
}

/// `--message 5` and `--message m:5` mean the same message.
#[test]
fn message_numbers_take_both_spellings() {
    assert_eq!(
        message_keys(&["5".into()]).unwrap(),
        message_keys(&["m:5".into()]).unwrap()
    );
}

/// Plan, `show` and the target lines: no "other source" for the same turn,
/// direct/derived in words, the same-turn reply in `show` too.
#[tokio::test]
async fn plan_and_show_print_the_same_turn_as_one_source() {
    let (home, _) = home().await;
    let mgr =
        duduclaw_gateway::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let reply = mgr
        .append_message_with_id(SESSION, "assistant", "noted", 1)
        .await
        .unwrap();
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let at = now_ts();
    mcp_write(
        &engine,
        mcp_write_sources_with(AGENT, &turn_env("t-2", Some("2"), Some(&at))).unwrap(),
    )
    .await;
    let out = op(home.path(), plan_cmd(AGENT, SESSION, Some("2")))
        .await
        .unwrap()
        .text;
    assert!(!out.contains("連帶影響"), "{out}");
    assert!(out.contains("／語意]  直接"), "{out}");
    assert!(!out.contains("origins"), "{out}");
    assert!(out.contains("1 個回合也一併設為不再學到"), "{out}");
    let line = format!("#2 的回覆 #{reply}");
    assert!(out.contains(&line), "{out}");
    let shown = op(
        home.path(),
        ForgetSourceCommands::Show {
            plan: plan_id_of(&out),
        },
    )
    .await
    .unwrap()
    .text;
    assert!(shown.contains(&line), "{shown}");
}

/// `list`: one line per conversation, each memory counted once; inside a
/// conversation the turn's own writes sit under its user message.
#[tokio::test]
async fn list_shows_one_line_per_conversation_and_turns_under_their_message() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let at = now_ts();
    mcp_write(
        &engine,
        mcp_write_sources_with(AGENT, &turn_env("t-1", Some("1"), Some(&at))).unwrap(),
    )
    .await;
    let all = op(
        home.path(),
        ForgetSourceCommands::List {
            agent: AGENT.into(),
            session: None,
        },
    )
    .await
    .unwrap()
    .text;
    let lines: Vec<&str> = all.lines().filter(|l| l.contains(SESSION)).collect();
    assert_eq!(lines.len(), 1, "{all}");
    // Two seeded memories plus the turn's write, each once.
    assert!(lines[0].contains("3 筆"), "{all}");
    assert!(lines[0].contains("聊天訊息、員工自行存入"), "{all}");

    let one = op(
        home.path(),
        ForgetSourceCommands::List {
            agent: AGENT.into(),
            session: Some(SESSION.into()),
        },
    )
    .await
    .unwrap()
    .text;
    assert!(!one.contains("turn:t-1"), "{one}");
    let first = one
        .lines()
        .find(|l| l.contains("→ --message 1"))
        .expect("message 1 line");
    assert!(first.contains("含員工在這一輪自行存入的記憶"), "{one}");
    assert!(first.contains("2 筆"), "{one}");
    assert!(one.contains("寫 5 或 m:5 都可以"), "{one}");
}

/// `resume` with nothing left to run does not list every step kind.
#[tokio::test]
async fn resume_with_nothing_left_says_so() {
    let (home, _) = home().await;
    let plan = forget_keys(home.path(), &["1"]).await;
    let out = op(
        home.path(),
        ForgetSourceCommands::Resume { plan: plan.plan_id },
    )
    .await
    .unwrap();
    assert!(out.complete);
    assert!(out.text.contains("沒有需要補跑"), "{}", out.text);
    assert!(!out.text.contains("壓縮摘要"), "{}", out.text);
}

/// Every stale reason that holds is named, not only the first.
#[tokio::test]
async fn a_stale_refusal_names_every_reason() {
    let (home, ids) = home().await;
    let p1 = plan_id_of(
        &op(home.path(), plan_cmd(AGENT, SESSION, Some("1")))
            .await
            .unwrap()
            .text,
    );
    forget_keys(home.path(), &["2"]).await;
    // A new memory from message 1 after the plan.
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let src = duduclaw_memory::SourceRef::channel_message(SESSION, 1, chrono::Utc::now(), None);
    mcp_write(&engine, vec![src]).await;
    approve(home.path(), &p1).await;
    let err = op(
        home.path(),
        ForgetSourceCommands::Apply {
            plan: p1,
            confirm: true,
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("另一次刪除"), "{err}");
    assert!(err.contains("資料有變"), "{err}");
    assert!(engine.get_by_id(AGENT, &ids[0]).await.unwrap().is_some());
}

/// A run plan explains why the whole conversation's summary is cleared.
#[tokio::test]
async fn a_run_plan_explains_the_whole_summary_clear() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let env = McpTurnEnv {
        run_session: Some("cron:agnes".into()),
        run: Some("k1".into()),
        ..Default::default()
    };
    mcp_write(&engine, mcp_write_sources_with(AGENT, &env).unwrap()).await;
    let out = op(home.path(), plan_cmd(AGENT, "cron:agnes", Some("run:k1")))
        .await
        .unwrap()
        .text;
    assert!(
        out.contains("壓縮摘要會清除整段 cron:agnes 的摘要"),
        "{out}"
    );
    assert!(out.contains("無法只拿掉這次執行的部分"), "{out}");
    assert!(out.contains("rl_trajectories"), "{out}");
}

/// Issue 3: a dispatched run's own identity versus the upstream turn its bus
/// message carried.
#[test]
fn dispatch_runs_keep_their_source_when_the_upstream_identity_is_incomplete() {
    let run = |turn: Option<&str>, session: Option<&str>| McpTurnEnv {
        run_session: Some("dispatch:agnes".into()),
        run: Some("r1".into()),
        turn: turn.map(str::to_string),
        session: session.map(str::to_string),
        ..Default::default()
    };
    let kinds = |env: &McpTurnEnv| -> Vec<SourceKind> {
        mcp_write_sources_with(AGENT, env)
            .unwrap()
            .into_iter()
            .map(|s| s.kind)
            .collect()
    };
    // Normal dispatch: the run only.
    assert_eq!(kinds(&run(None, None)), vec![SourceKind::DispatchRun]);
    // Complete upstream: the upstream turn and the run.
    assert_eq!(
        kinds(&run(Some("t-9"), Some(SESSION))),
        vec![SourceKind::McpTurn, SourceKind::DispatchRun]
    );
    // Incomplete upstream (either half): the run plus the unknown marker.
    for env in [run(Some("t-9"), None), run(None, Some(SESSION))] {
        assert_eq!(
            kinds(&env),
            vec![SourceKind::UpstreamUnknown, SourceKind::DispatchRun]
        );
    }
    // The process's own turn incomplete: still refused, naming the field,
    // without telling anyone to restart the gateway.
    let own = McpTurnEnv {
        turn: Some("t-1".into()),
        ..Default::default()
    };
    let e = mcp_write_sources_with(AGENT, &own).unwrap_err();
    assert!(e.contains("DUDUCLAW_SESSION_ID"), "{e}");
    let msg = crate::mcp_memory_handlers::malformed_host_env(&e);
    assert!(!msg.to_lowercase().contains("restart"), "{msg}");
}

/// The marker a write gets for an unknown upstream shows in the plan's
/// untracked count.
#[tokio::test]
async fn an_unknown_upstream_is_counted_as_untracked_in_a_plan() {
    let (home, _) = home().await;
    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let env = McpTurnEnv {
        run_session: Some("dispatch:agnes".into()),
        run: Some("r2".into()),
        turn: Some("t-9".into()),
        ..Default::default()
    };
    mcp_write(&engine, mcp_write_sources_with(AGENT, &env).unwrap()).await;
    let out = op(home.path(), plan_cmd(AGENT, SESSION, Some("1")))
        .await
        .unwrap()
        .text;
    assert!(out.contains("另有 1 筆沒有完整來源紀錄的記憶"), "{out}");
}

/// The sender attaches the upstream identity as a pair or not at all.
#[test]
fn the_sender_attaches_both_halves_or_neither() {
    use duduclaw_gateway::memory_provenance::complete_upstream_pair as pair;
    let s = |v: &str| Some(v.to_string());
    assert_eq!(pair(s("t"), s("x")), (s("t"), s("x"), None));
    assert_eq!(pair(None, None), (None, None, None));
    assert_eq!(pair(s("t"), None), (None, None, Some("session_id")));
    assert_eq!(pair(s(" "), s("x")), (None, None, Some("turn_id")));
}
