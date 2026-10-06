//! P2-A H-2a, narrowed in round 5 (M3-4, H3-1): during a round of a
//! continuous-responsibility run the parent of a task an employee creates is
//! decided by the host's round information ([`duduclaw_core::ENV_TASK_ID`]),
//! not by the model, and a model-given `parent_task_id` must lie inside that
//! round's tree. Any other round keeps the old rule. A present-but-damaged
//! round value is refused; only an absent one means "no round". The per-task
//! cap counts only unfinished sub-tasks.

use super::*;
use duduclaw_gateway::task_store::{MAX_CHILDREN_PER_TASK, TaskRow, TaskStore};
use serde_json::json;

struct Home(tempfile::TempDir);
impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        Self(dir)
    }
    fn path(&self) -> &std::path::Path {
        self.0.path()
    }
}

async fn task(home: &std::path::Path, assigned_to: &str, parent: Option<&str>) -> String {
    let store = TaskStore::open(home).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut row = TaskRow::new(
        id.clone(),
        "t".into(),
        "d".into(),
        "medium".into(),
        assigned_to.into(),
        "dashboard".into(),
    );
    row.parent_task_id = parent.map(str::to_string);
    store.insert_task(&row).await.unwrap();
    id
}

fn err_text(v: &Value) -> String {
    v["content"][0]["text"].as_str().unwrap_or("").to_string()
}

const EMP: RecordActor<'static> = RecordActor::Agent("worker");

/// Mark `task_id` as a responsibility run (an occurrence row).
fn make_occurrence(home: &std::path::Path, task_id: &str) {
    let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
    conn.execute(
        "INSERT INTO responsibility_occurrences (responsibility_id, occurrence_key, task_id, \
         contract_revision, control_epoch, period_key, reserved_cents, created_at) \
         VALUES ('r1', ?1, ?1, 1, 1, 'w', 50, '2026-10-06T00:00:00Z')",
        rusqlite::params![task_id],
    )
    .unwrap();
}

fn set_status(home: &std::path::Path, task_id: &str, status: &str) {
    let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE tasks SET status = ?2 WHERE id = ?1",
        rusqlite::params![task_id, status],
    )
    .unwrap();
}

async fn run_round(home: &std::path::Path) -> String {
    let round = task(home, "worker", None).await;
    make_occurrence(home, &round);
    round
}

#[tokio::test]
async fn in_a_run_the_round_task_becomes_the_parent_when_the_model_names_none() {
    let home = Home::new();
    let round = run_round(home.path()).await;
    let got = resolve_parent(home.path(), EMP, &json!({}), Some(&round))
        .await
        .unwrap();
    assert_eq!(got.as_deref(), Some(round.as_str()));
}

#[tokio::test]
async fn in_a_run_a_parent_inside_the_round_tree_is_kept() {
    let home = Home::new();
    let round = run_round(home.path()).await;
    let child = task(home.path(), "worker", Some(&round)).await;
    let grandchild = task(home.path(), "worker", Some(&child)).await;
    for p in [&round, &child, &grandchild] {
        let got = resolve_parent(
            home.path(),
            EMP,
            &json!({ "parent_task_id": p }),
            Some(&round),
        )
        .await
        .unwrap();
        assert_eq!(got.as_deref(), Some(p.as_str()));
    }
}

#[tokio::test]
async fn in_a_run_a_parent_outside_its_tree_is_refused() {
    let home = Home::new();
    let round = run_round(home.path()).await;
    // The caller's own unrelated task: still outside this run's tree.
    let other = task(home.path(), "worker", None).await;
    let err = resolve_parent(
        home.path(),
        EMP,
        &json!({ "parent_task_id": other }),
        Some(&round),
    )
    .await
    .unwrap_err();
    assert!(err_text(&err).contains(&round), "{}", err_text(&err));
    // A goal sub-task looping on its own id inside the run is held the same.
    let sub = task(home.path(), "worker", Some(&round)).await;
    assert!(
        resolve_parent(
            home.path(),
            EMP,
            &json!({ "parent_task_id": other }),
            Some(&sub)
        )
        .await
        .is_err()
    );
}

/// M3-4: an ordinary goal round (or a heartbeat wake-up of an ordinary task)
/// is not a run: no default parent, and a model-named parent gets the old
/// relationship check.
#[tokio::test]
async fn an_ordinary_round_keeps_the_old_rule() {
    let home = Home::new();
    let round = task(home.path(), "worker", None).await;
    let got = resolve_parent(home.path(), EMP, &json!({}), Some(&round))
        .await
        .unwrap();
    assert_eq!(got, None, "no default parent outside a run");
    let other = task(home.path(), "worker", None).await;
    let got = resolve_parent(
        home.path(),
        EMP,
        &json!({ "parent_task_id": other }),
        Some(&round),
    )
    .await
    .unwrap();
    assert_eq!(got.as_deref(), Some(other.as_str()));
}

/// Gemini / Grok / a Bash-started server pass no round information at all:
/// the tool keeps working as before. Nothing ties the new task to a run
/// unless the employee names it.
#[tokio::test]
async fn without_round_information_tasks_create_works_as_before() {
    let home = Home::new();
    let run = run_round(home.path()).await;
    assert_eq!(
        resolve_parent(home.path(), EMP, &json!({}), None)
            .await
            .unwrap(),
        None
    );
    let got = resolve_parent(home.path(), EMP, &json!({ "parent_task_id": run }), None)
        .await
        .unwrap();
    assert_eq!(
        got.as_deref(),
        Some(run.as_str()),
        "naming the run puts it in the tree"
    );
}

/// H3-1: a round value that is present but empty, blank or malformed is a
/// damaged value, refused; it is never read as "no round".
#[tokio::test]
async fn a_present_but_damaged_round_value_is_refused() {
    let home = Home::new();
    let _run = run_round(home.path()).await;
    let long = "a".repeat(129);
    for bad in ["", "  ", " x", "a b", "x;y", "\u{0}", long.as_str()] {
        let err = resolve_parent(home.path(), EMP, &json!({}), Some(bad))
            .await
            .unwrap_err();
        assert!(
            err_text(&err).contains("沒有建立"),
            "{bad:?}: {}",
            err_text(&err)
        );
    }
}

#[tokio::test]
async fn a_round_that_is_not_the_callers_is_ignored() {
    let home = Home::new();
    let foreign = run_round(home.path()).await;
    sql_assign(home.path(), &foreign, "someone-else");
    let got = resolve_parent(home.path(), EMP, &json!({}), Some(&foreign))
        .await
        .unwrap();
    assert_eq!(got, None);
}

fn sql_assign(home: &std::path::Path, task_id: &str, who: &str) {
    let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE tasks SET assigned_to = ?2 WHERE id = ?1",
        rusqlite::params![task_id, who],
    )
    .unwrap();
}

#[tokio::test]
async fn a_round_that_cannot_be_read_refuses_the_creation() {
    let home = Home::new();
    let _ = task(home.path(), "worker", None).await;
    let err = resolve_parent(home.path(), EMP, &json!({}), Some("no-such-task"))
        .await
        .unwrap_err();
    assert!(err_text(&err).contains("沒有建立"), "{}", err_text(&err));
}

#[tokio::test]
async fn the_operator_keeps_the_parent_it_asked_for() {
    let home = Home::new();
    let round = run_round(home.path()).await;
    let other = task(home.path(), "worker", None).await;
    let op = RecordActor::Operator("dudu");
    let got = resolve_parent(
        home.path(),
        op,
        &json!({ "parent_task_id": other }),
        Some(&round),
    )
    .await
    .unwrap();
    assert_eq!(got.as_deref(), Some(other.as_str()));
}

/// M3-4: the cap counts only unfinished sub-tasks.
#[tokio::test]
async fn the_sub_task_cap_counts_only_unfinished_children() {
    let home = Home::new();
    let round = run_round(home.path()).await;
    let mut kids = Vec::new();
    for _ in 0..MAX_CHILDREN_PER_TASK {
        kids.push(task(home.path(), "worker", Some(&round)).await);
    }
    let err = resolve_parent(home.path(), EMP, &json!({}), Some(&round))
        .await
        .unwrap_err();
    assert!(err_text(&err).contains("上限"), "{}", err_text(&err));
    set_status(home.path(), &kids[0], "done");
    let got = resolve_parent(home.path(), EMP, &json!({}), Some(&round))
        .await
        .unwrap();
    assert_eq!(
        got.as_deref(),
        Some(round.as_str()),
        "a finished child frees a place"
    );
}

/// M4-3 (b), through the process environment and the real `tasks_create`
/// handler: a `DUDUCLAW_TASK_ID` that is present but damaged refuses the
/// creation. The variable is process-global, so each value is tried in a
/// child run of this test binary (other tests in this process never see it).
#[test]
fn a_damaged_round_value_in_the_environment_refuses_tasks_create() {
    let exe = std::env::current_exe().unwrap();
    for bad in ["", "   ", "bad id", "x;y"] {
        let out = std::process::Command::new(&exe)
            .args([
                "--exact",
                "mcp::round_parent_tests::env_child_tasks_create_is_refused",
                "--ignored",
                "--test-threads=1",
            ])
            .env(duduclaw_core::ENV_TASK_ID, bad)
            .env_remove(duduclaw_core::ENV_AGENT_ID)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{bad:?}: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[tokio::test]
#[ignore = "run by a_damaged_round_value_in_the_environment_refuses_tasks_create"]
async fn env_child_tasks_create_is_refused() {
    assert!(
        std::env::var_os(duduclaw_core::ENV_TASK_ID).is_some(),
        "the parent test sets the variable"
    );
    let home = Home::new();
    let out = handle_tasks_create(&json!({ "title": "t" }), home.path(), EMP).await;
    assert!(
        out.get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "{out}"
    );
    assert!(err_text(&out).contains("沒有建立"), "{}", err_text(&out));
}
