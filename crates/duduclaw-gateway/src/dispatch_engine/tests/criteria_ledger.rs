//! WP-G2: the criteria ledger through the settle path and the judge prompt.

use super::*;
use crate::goal_loop::criteria_ledger::{
    CRITERIA_STATUS_INVALID_EVENT, CriteriaLedger, CriteriaLedgerMode, CriterionStatus,
};

/// Records what the panel was asked (and through which entry point).
#[derive(Default)]
struct RecordingJudge {
    task: std::sync::Mutex<Option<String>>,
    result: std::sync::Mutex<Option<String>>,
    handles: std::sync::Mutex<Option<Vec<String>>>,
}

#[async_trait]
impl AcceptanceJudge for RecordingJudge {
    async fn judge(&self, _c: &str, task: &str, result: &str) -> Result<AcceptanceVerdict, String> {
        *self.task.lock().unwrap() = Some(task.to_string());
        *self.result.lock().unwrap() = Some(result.to_string());
        Ok(AcceptanceVerdict {
            passed: false,
            feedback: "再補一點".into(),
            aspects: None,
        })
    }

    async fn judge_with_criteria(
        &self,
        c: &str,
        task: &str,
        result: &str,
        handles: &[String],
    ) -> Result<AcceptanceVerdict, String> {
        *self.handles.lock().unwrap() = Some(handles.to_vec());
        self.judge(c, task, result).await
    }
}

fn write_goal_loop_config(home: &std::path::Path, mode: &str) {
    std::fs::write(
        home.join("config.toml"),
        format!("[goal_loop]\ncriteria_ledger = \"{mode}\"\n"),
    )
    .unwrap();
}

/// Seed a `review` goal with a two-criterion ledger (or none) and an open
/// iteration round, completed with `reply`.
async fn seed(store: &TaskStore, id: &str, with_ledger: bool, reply: &str) {
    let mut g = pending_goal(id);
    g.assigned_to = "probe".into();
    g.acceptance_criteria = Some("產出 hello.txt\n內容含「你好」".into());
    g.acceptance_criteria_baseline = g.acceptance_criteria.clone();
    if with_ledger {
        g.criteria_ledger = CriteriaLedger::new(
            id,
            "產出 hello.txt\n內容含「你好」",
            CriteriaLedgerMode::Report,
        )
        .map(|l| l.to_json());
    }
    store.insert_task(&g).await.unwrap();
    store
        .record_iteration_dispatch(id, 1, "2026-10-03T00:00:00Z")
        .await
        .unwrap();
    assert!(
        store
            .atomic_claim(id, "probe", "2026-10-03T00:00:00Z", "2026-10-03T00:05:00Z")
            .await
            .unwrap()
            .is_claimed()
    );
    store.complete_task(id, reply, "probe").await.unwrap();
}

const VALID: &str = "已建立 hello.txt\n<criteria_status>[{\"id\":\"C1\",\"status\":\"covered\",\"evidence\":[\"Write hello.txt\"]},{\"id\":\"C2\",\"status\":\"blocked\",\"unresolved\":[\"不確定編碼\"]}]</criteria_status>";

async fn run(
    home: &std::path::Path,
    with_ledger: bool,
    reply: &str,
) -> (Arc<TaskStore>, Arc<RecordingJudge>) {
    let store = Arc::new(TaskStore::open(home).unwrap());
    seed(&store, "g2", with_ledger, reply).await;
    let judge = Arc::new(RecordingJudge::default());
    let engine = DispatchEngine::new(
        store.clone(),
        Some(judge.clone() as Arc<dyn AcceptanceJudge>),
    )
    .with_home_dir(home.to_path_buf());
    engine.review_goal_tasks().await.unwrap();
    (store, judge)
}

fn audit_events(home: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v["event_type"] == CRITERIA_STATUS_INVALID_EVENT)
        .collect()
}

#[tokio::test]
async fn valid_report_updates_ledger_strips_tag_and_reaches_judge_as_reference() {
    let home = tempfile::tempdir().unwrap();
    let (store, judge) = run(home.path(), true, VALID).await;
    let t = store.get_task("g2").await.unwrap().unwrap();
    let ledger = CriteriaLedger::from_json(t.criteria_ledger.as_deref()).unwrap();
    assert_eq!(ledger.units[0].status, CriterionStatus::Covered);
    assert_eq!(ledger.units[1].status, CriterionStatus::Blocked);
    assert_eq!(ledger.units[1].unresolved, vec!["不確定編碼".to_string()]);
    assert_eq!(ledger.last_report_round, Some(1));
    assert_eq!(ledger.invalid_reports, 0);
    // Round snapshot written on the iteration row.
    let snaps = store.iteration_criteria_snapshots("g2").await.unwrap();
    assert_eq!(snaps.len(), 1);
    let snap = CriteriaLedger::from_json(snaps[0].1.as_deref()).unwrap();
    assert_eq!(snap.units, ledger.units);
    // Tag stripped from the text the judge sees.
    let result = judge.result.lock().unwrap().clone().unwrap();
    assert_eq!(result, "已建立 hello.txt");
    // Report mode: reference block present, panel contract unchanged.
    let task = judge.task.lock().unwrap().clone().unwrap();
    assert!(task.contains("<criteria_ledger_self_report>"), "{task}");
    assert!(task.contains("[C2] 內容含「你好」 — blocked"), "{task}");
    assert!(task.contains("不是證據"));
    assert!(
        judge.handles.lock().unwrap().is_none(),
        "report mode must not call the criteria contract"
    );
    assert!(audit_events(home.path()).is_empty());
}

#[tokio::test]
async fn invalid_report_leaves_ledger_counts_and_audits() {
    let home = tempfile::tempdir().unwrap();
    let reply = "done\n<criteria_status>[{\"id\":\"C1\",\"status\":\"covered\",\"evidence\":[\"x\"],\"note\":\"extra\"}]</criteria_status>";
    let (store, judge) = run(home.path(), true, reply).await;
    let t = store.get_task("g2").await.unwrap().unwrap();
    let ledger = CriteriaLedger::from_json(t.criteria_ledger.as_deref()).unwrap();
    assert!(
        ledger
            .units
            .iter()
            .all(|u| u.status == CriterionStatus::Planned)
    );
    assert_eq!(ledger.invalid_reports, 1);
    assert_eq!(ledger.last_report_round, None);
    let events = audit_events(home.path());
    assert_eq!(events.len(), 1);
    assert!(
        events[0]["details"]["violation"]
            .as_str()
            .unwrap()
            .contains("contract violation")
    );
    assert!(
        events[0]["details"]["body_head"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 200
    );
    // The invalid tag is still stripped before the judge.
    assert_eq!(judge.result.lock().unwrap().clone().unwrap(), "done");
}

#[tokio::test]
async fn absent_report_leaves_ledger_unchanged_and_uncounted() {
    let home = tempfile::tempdir().unwrap();
    let (store, _judge) = run(home.path(), true, "只有文字").await;
    let t = store.get_task("g2").await.unwrap().unwrap();
    let ledger = CriteriaLedger::from_json(t.criteria_ledger.as_deref()).unwrap();
    assert_eq!(ledger.invalid_reports, 0);
    assert!(
        ledger
            .units
            .iter()
            .all(|u| u.status == CriterionStatus::Planned)
    );
    assert!(audit_events(home.path()).is_empty());
}

#[tokio::test]
async fn enforce_mode_asks_the_panel_for_every_handle() {
    let home = tempfile::tempdir().unwrap();
    write_goal_loop_config(home.path(), "enforce");
    let (_store, judge) = run(home.path(), true, VALID).await;
    assert_eq!(
        judge.handles.lock().unwrap().clone(),
        Some(vec!["C1".to_string(), "C2".to_string()])
    );
}

#[tokio::test]
async fn off_mode_and_no_ledger_leave_the_judge_input_untouched() {
    // Off: ledger ignored, tag not stripped, ledger not written.
    let home = tempfile::tempdir().unwrap();
    write_goal_loop_config(home.path(), "off");
    let (store, judge) = run(home.path(), true, VALID).await;
    let off_task = judge.task.lock().unwrap().clone().unwrap();
    assert!(!off_task.contains("criteria_ledger_self_report"));
    assert_eq!(judge.result.lock().unwrap().clone().unwrap(), VALID);
    let t = store.get_task("g2").await.unwrap().unwrap();
    let ledger = CriteriaLedger::from_json(t.criteria_ledger.as_deref()).unwrap();
    assert!(
        ledger
            .units
            .iter()
            .all(|u| u.status == CriterionStatus::Planned)
    );
    assert!(
        store.iteration_criteria_snapshots("g2").await.unwrap()[0]
            .1
            .is_none()
    );

    // A goal without a ledger under the default mode sees exactly the same
    // judge input as one under `off`.
    let home2 = tempfile::tempdir().unwrap();
    let (_store, judge2) = run(home2.path(), false, VALID).await;
    let none_task = judge2.task.lock().unwrap().clone().unwrap();
    assert_eq!(
        none_task.replace(&home2.path().display().to_string(), "<HOME>"),
        off_task.replace(&home.path().display().to_string(), "<HOME>")
    );
    assert_eq!(judge2.result.lock().unwrap().clone().unwrap(), VALID);
}

#[tokio::test]
async fn judge_input_names_the_worker_working_directory_from_the_system() {
    let home = tempfile::tempdir().unwrap();
    let (_store, judge) = run(home.path(), false, "做完了").await;
    let task = judge.task.lock().unwrap().clone().unwrap();
    let dir = home.path().join("agents").join("probe");
    assert!(
        task.contains(&format!(
            "<worker_working_directory>{}</worker_working_directory>",
            dir.display()
        )),
        "{task}"
    );
    assert!(task.contains("provided by the system, not by the worker"));

    // No home dir ⇒ no agent directory ⇒ no line.
    let store = Arc::new(TaskStore::open(tempfile::tempdir().unwrap().path()).unwrap());
    seed(&store, "g3", false, "做完了").await;
    let judge = Arc::new(RecordingJudge::default());
    let engine = DispatchEngine::new(
        store.clone(),
        Some(judge.clone() as Arc<dyn AcceptanceJudge>),
    );
    engine.review_goal_tasks().await.unwrap();
    let task = judge.task.lock().unwrap().clone().unwrap();
    assert!(!task.contains("worker_working_directory"), "{task}");
}

#[test]
fn working_directory_block_is_escaped_and_optional() {
    assert!(worker_working_directory_block(None).is_none());
    let b = worker_working_directory_block(Some(std::path::Path::new("/h/agents/a<b>"))).unwrap();
    assert!(
        b.starts_with("<worker_working_directory>/h/agents/a&lt;b&gt;</worker_working_directory>")
    );
}

// ── judge prompt / schema / parser contract ─────────────────────────────

fn handles() -> Vec<String> {
    vec!["C1".to_string(), "C2".to_string()]
}

#[test]
fn prompt_without_handles_is_byte_identical_and_enforce_adds_the_contract() {
    for d in [Difficulty::Simple, Difficulty::Complex] {
        let plain = build_acceptance_prompt_for("crit", "task", "result", d);
        assert_eq!(
            build_acceptance_prompt_with_criteria("crit", "task", "result", d, &[]),
            plain
        );
        assert!(!plain.contains("\"criteria\""));
        let enforce =
            build_acceptance_prompt_with_criteria("crit", "task", "result", d, &handles());
        assert!(enforce.contains("acceptance ledger (C1, C2)"), "{enforce}");
        assert!(enforce.contains("a missing or duplicated handle counts as FAIL"));
        assert!(enforce.contains("\"criteria\": [{\"id\": \"C1\", \"pass\": true|false"));
        // Everything else is unchanged.
        assert!(enforce.contains("<worker_result>\nresult\n</worker_result>"));
    }
}

#[test]
fn schema_with_criteria_only_changes_with_handles() {
    let aspects = panel_aspects(Difficulty::Complex);
    assert_eq!(
        panel_output_schema_with_criteria(aspects, &[]),
        panel_output_schema(aspects)
    );
    let s = panel_output_schema_with_criteria(aspects, &handles());
    assert_eq!(
        s["properties"]["criteria"]["items"]["properties"]["id"]["enum"],
        serde_json::json!(["C1", "C2"])
    );
    assert!(
        s["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("criteria"))
    );
}

fn panel(criteria: serde_json::Value) -> String {
    serde_json::json!({
        "correctness": {"pass": true, "reason": "ok"},
        "completeness": {"pass": true, "reason": "ok"},
        "safety": {"pass": true, "reason": "ok"},
        "criteria": criteria,
    })
    .to_string()
}

#[test]
fn enforce_parser_requires_every_handle_exactly_once() {
    use crate::dispatch_engine::judge::parse_panel_verdict_contract_with_criteria;
    use crate::dispatch_engine::strict_shadow::StrictReplyParsing;
    let aspects = panel_aspects(Difficulty::Complex);
    let all_pass = panel(serde_json::json!([
        {"id": "C1", "pass": true, "reason": "file exists"},
        {"id": "C2", "pass": true, "reason": "contains 你好"},
    ]));
    let missing = panel(serde_json::json!([{"id": "C1", "pass": true, "reason": "ok"}]));
    let dup = panel(serde_json::json!([
        {"id": "C1", "pass": true, "reason": "ok"},
        {"id": "C1", "pass": true, "reason": "ok"},
        {"id": "C2", "pass": true, "reason": "ok"},
    ]));
    let one_fail = panel(serde_json::json!([
        {"id": "C1", "pass": true, "reason": "ok"},
        {"id": "C2", "pass": false, "reason": "缺少你好"},
    ]));
    for mode in [
        StrictReplyParsing::Off,
        StrictReplyParsing::Shadow,
        StrictReplyParsing::Enforce,
    ] {
        let m = crate::metrics::MetricsRegistry::new_isolated();
        let parse = |raw: &str| {
            parse_panel_verdict_contract_with_criteria(&m, raw, aspects, &handles(), mode, None)
        };
        let v = parse(&all_pass);
        assert!(v.passed, "{mode:?}: {v:?}");
        let rows = v.aspects.unwrap();
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|r| r["name"] == "C2" && r["criterion"] == true)
        );
        let v = parse(&missing);
        assert!(!v.passed && v.feedback.contains("C2"), "{mode:?}: {v:?}");
        let v = parse(&dup);
        assert!(!v.passed && v.feedback.contains("C1"), "{mode:?}: {v:?}");
        let v = parse(&one_fail);
        assert!(
            !v.passed && v.feedback.contains("[correctness]") && v.feedback.contains("缺少你好"),
            "{mode:?}: {v:?}"
        );
    }
    // Correctness's own FAIL still fails when every criterion passes.
    let own_fail = serde_json::json!({
        "correctness": {"pass": false, "reason": "wrong"},
        "completeness": {"pass": true, "reason": "ok"},
        "safety": {"pass": true, "reason": "ok"},
        "criteria": [{"id": "C1", "pass": true, "reason": "ok"}, {"id": "C2", "pass": true, "reason": "ok"}],
    })
    .to_string();
    assert!(!parse_panel_verdict_for_criteria(&own_fail, aspects, &handles()).passed);
    // A panel without criteria cannot pass under enforce; a legacy PASS neither.
    let no_criteria = serde_json::json!({
        "correctness": {"pass": true, "reason": "ok"},
        "completeness": {"pass": true, "reason": "ok"},
        "safety": {"pass": true, "reason": "ok"},
    })
    .to_string();
    assert!(!parse_panel_verdict_for_criteria(&no_criteria, aspects, &handles()).passed);
    assert!(parse_panel_verdict_for_criteria(&no_criteria, aspects, &[]).passed);
    assert!(!parse_panel_verdict_for_criteria("PASS\nlooks fine", aspects, &handles()).passed);
    assert!(parse_panel_verdict_for_criteria("PASS\nlooks fine", aspects, &[]).passed);
}

#[test]
fn strict_parser_refuses_criteria_outside_its_contract() {
    use crate::dispatch_engine::judge::parse_panel_verdict_contract_with_criteria;
    use crate::dispatch_engine::strict_shadow::StrictReplyParsing;
    let aspects = panel_aspects(Difficulty::Complex);
    let m = crate::metrics::MetricsRegistry::new_isolated();
    // Criteria present but not asked for ⇒ strict refuses (FAIL in enforce).
    let with = panel(serde_json::json!([{"id": "C1", "pass": true, "reason": "ok"}]));
    let v = parse_panel_verdict_contract_with_criteria(
        &m,
        &with,
        aspects,
        &[],
        StrictReplyParsing::Enforce,
        None,
    );
    assert!(!v.passed, "{v:?}");
    // Unknown handle ⇒ strict schema violation; lenient ignores it.
    let unknown = panel(serde_json::json!([
        {"id": "C1", "pass": true, "reason": "ok"},
        {"id": "C2", "pass": true, "reason": "ok"},
        {"id": "C9", "pass": true, "reason": "ok"},
    ]));
    let strict = parse_panel_verdict_contract_with_criteria(
        &m,
        &unknown,
        aspects,
        &handles(),
        StrictReplyParsing::Enforce,
        None,
    );
    assert!(!strict.passed);
    assert!(parse_panel_verdict_for_criteria(&unknown, aspects, &handles()).passed);
    // Extra field on a criterion ⇒ strict refuses.
    let extra = panel(serde_json::json!([
        {"id": "C1", "pass": true, "reason": "ok", "score": 1},
        {"id": "C2", "pass": true, "reason": "ok"},
    ]));
    let v = parse_panel_verdict_contract_with_criteria(
        &m,
        &extra,
        aspects,
        &handles(),
        StrictReplyParsing::Enforce,
        None,
    );
    assert!(!v.passed);
    // No handles: identical to the pre-WP-G2 contract parser.
    let plain = serde_json::json!({
        "correctness": {"pass": true, "reason": "ok"},
        "completeness": {"pass": true, "reason": "ok"},
        "safety": {"pass": true, "reason": "ok"},
    })
    .to_string();
    for mode in [
        StrictReplyParsing::Off,
        StrictReplyParsing::Shadow,
        StrictReplyParsing::Enforce,
    ] {
        assert_eq!(
            parse_panel_verdict_contract_with_criteria(&m, &plain, aspects, &[], mode, None),
            crate::dispatch_engine::judge::parse_panel_verdict_contract_with(
                &m, &plain, aspects, mode, None
            )
        );
    }
}

/// The real `LlmAcceptanceJudge` sends the per-criterion contract only via
/// `judge_with_criteria`, and parses it.
#[tokio::test]
async fn llm_judge_enforces_criteria_end_to_end() {
    let reply = panel(serde_json::json!([
        {"id": "C1", "pass": true, "reason": "ok"},
        {"id": "C2", "pass": false, "reason": "缺"},
    ]));
    let judge = LlmAcceptanceJudge::new(StubCaller(reply.clone()));
    let v = judge
        .judge_with_criteria(
            "產出 hello.txt\n內容含你好",
            "研究 hello",
            "done",
            &handles(),
        )
        .await
        .unwrap();
    assert!(!v.passed);
    // The same reply through the plain entry point ignores `criteria` in the
    // lenient (default, no home) path.
    let v = judge
        .judge("產出 hello.txt\n內容含你好", "研究 hello", "done")
        .await
        .unwrap();
    assert!(v.passed);
}

/// Accept-side twin of [`RecordingJudge`].
struct AcceptingJudge;

#[async_trait]
impl AcceptanceJudge for AcceptingJudge {
    async fn judge(&self, _c: &str, _t: &str, _r: &str) -> Result<AcceptanceVerdict, String> {
        Ok(AcceptanceVerdict { passed: true, feedback: "ok".into(), aspects: None })
    }
}

/// WP-G2: after settle no stored copy of the reply carries the raw tag —
/// neither the accepted task's `result_summary` (「最新產出摘要」) nor the
/// sealed round's `worker_excerpt` (rejected and accepted alike).
#[tokio::test]
async fn stored_reply_copies_carry_no_criteria_tag() {
    // Rejected round: `result_summary` is cleared; the excerpt survives.
    let home = tempfile::tempdir().unwrap();
    let (store, _judge) = run(home.path(), true, VALID).await;
    let iters = store.list_iterations("g2").await.unwrap();
    let excerpt = iters[0].worker_excerpt.clone().unwrap();
    assert_eq!(excerpt, "已建立 hello.txt");

    // Accepted round: the task keeps the stripped summary.
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(home.path()).unwrap());
    seed(&store, "g2", true, VALID).await;
    let engine = DispatchEngine::new(store.clone(), Some(Arc::new(AcceptingJudge) as Arc<dyn AcceptanceJudge>))
        .with_home_dir(home.path().to_path_buf());
    engine.review_goal_tasks().await.unwrap();
    let t = store.get_task("g2").await.unwrap().unwrap();
    assert_eq!(t.status, "done");
    assert_eq!(t.result_summary.as_deref(), Some("已建立 hello.txt"));
    let iters = store.list_iterations("g2").await.unwrap();
    assert!(!iters[0].worker_excerpt.clone().unwrap_or_default().contains("criteria_status"));
    // The parsed ledger still holds what the tag said.
    let ledger = CriteriaLedger::from_json(t.criteria_ledger.as_deref()).unwrap();
    assert_eq!(ledger.units[0].evidence, vec!["Write hello.txt".to_string()]);

    // No ledger: the stored reply is left exactly as submitted.
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open(home.path()).unwrap());
    seed(&store, "g2", false, VALID).await;
    let engine = DispatchEngine::new(store.clone(), Some(Arc::new(AcceptingJudge) as Arc<dyn AcceptanceJudge>))
        .with_home_dir(home.path().to_path_buf());
    engine.review_goal_tasks().await.unwrap();
    assert_eq!(store.get_task("g2").await.unwrap().unwrap().result_summary.as_deref(), Some(VALID));
}
