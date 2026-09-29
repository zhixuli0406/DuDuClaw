//! RFC-26 §4.1 (P6.1): Plan Mode — a clarify-first planner tool.
//!
//! `plan_start` turns an ambiguous task into a structured planning scaffold: up to
//! 3 clarifying questions for the agent to ask the user *before* executing, then a
//! decomposition instruction. The calling agent (itself an LLM) does the actual
//! questioning + `tasks_create` decomposition — this tool supplies the structure
//! and honours the per-agent `[planner] clarify_first` toggle.

use std::path::Path;

use serde_json::{Value, json};

/// `[planner]` settings from `agent.toml`. Fail-safe default: clarify first.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannerSettings {
    pub clarify_first: bool,
    pub max_questions: usize,
}

impl Default for PlannerSettings {
    fn default() -> Self {
        PlannerSettings {
            clarify_first: true,
            max_questions: 3,
        }
    }
}

/// Parse `[planner]` from an `agent.toml` string (pure + fail-safe).
pub fn parse_planner_settings(toml_str: &str) -> PlannerSettings {
    let def = PlannerSettings::default();
    let parsed: toml::Value = match toml_str.parse() {
        Ok(v) => v,
        Err(_) => return def,
    };
    let p = match parsed.get("planner").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return def,
    };
    PlannerSettings {
        clarify_first: p
            .get("clarify_first")
            .and_then(|v| v.as_bool())
            .unwrap_or(def.clarify_first),
        max_questions: p
            .get("max_questions")
            .and_then(|v| v.as_integer())
            .filter(|n| (1..=5).contains(n))
            .map(|n| n as usize)
            .unwrap_or(def.max_questions),
    }
}

/// Read `[planner]` for the calling agent.
///
/// Resolved through [`crate::mcp::caller_agent_dir`] rather than a bare
/// `agents/<id>` join: an `eph-*` team role member lives at
/// `<home>/agents/.ephemeral/<id>/`, so the bare join silently missed its
/// `agent.toml` and every role member fell back to the default settings. An
/// ordinary registry id resolves byte-identically to the old join.
pub fn load_planner_settings(home_dir: &Path, agent_id: &str) -> PlannerSettings {
    let path = crate::mcp::caller_agent_dir(home_dir, agent_id).join("agent.toml");
    match std::fs::read_to_string(&path) {
        Ok(s) => parse_planner_settings(&s),
        Err(_) => PlannerSettings::default(),
    }
}

fn ok_json(value: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": value.to_string() }] })
}

fn err(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("Error: {text}") }], "isError": true })
}

/// `plan_start` — return a clarify-first planning scaffold for a task.
pub async fn handle_plan_start(args: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let task = match args.get("task").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim(),
        _ => return err("task is required"),
    };
    let settings = load_planner_settings(home_dir, agent_id);

    if !settings.clarify_first {
        return ok_json(json!({
            "task": task,
            "clarify_first": false,
            "scaffold": format!(
                "Planner is in direct mode. Decompose '{task}' into subtasks now and create them \
                 with tasks_create, wiring dependencies via depends_on where order matters. Do not \
                 ask clarifying questions first."
            ),
        }));
    }

    ok_json(json!({
        "task": task,
        "clarify_first": true,
        "max_questions": settings.max_questions,
        "scaffold": format!(
            "Before executing '{task}', ask the user up to {n} clarifying questions that would \
             change the plan — focus on scope (what's in/out), constraints (deadline, tools, \
             budget), and the acceptance criteria (how we know it's done). Wait for answers (or \
             proceed best-effort if none come), THEN decompose into subtasks with tasks_create, \
             setting depends_on for steps that must run in order.",
            n = settings.max_questions
        ),
        "suggested_dimensions": ["scope", "constraints", "acceptance_criteria"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &Value) -> String {
        v["content"][0]["text"].as_str().unwrap_or("").to_string()
    }
    fn is_error(v: &Value) -> bool {
        v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false)
    }

    #[test]
    fn settings_default_clarify_first() {
        assert!(PlannerSettings::default().clarify_first);
        assert_eq!(PlannerSettings::default().max_questions, 3);
    }

    #[test]
    fn parse_missing_section_default() {
        assert_eq!(
            parse_planner_settings("[agent]\nname='x'"),
            PlannerSettings::default()
        );
    }

    #[test]
    fn parse_reads_planner_section() {
        let s = parse_planner_settings("[planner]\nclarify_first = false\nmax_questions = 2\n");
        assert!(!s.clarify_first);
        assert_eq!(s.max_questions, 2);
    }

    #[test]
    fn parse_clamps_out_of_range_questions() {
        let s = parse_planner_settings("[planner]\nmax_questions = 99\n");
        assert_eq!(s.max_questions, 3); // out of [1,5] → default
    }

    #[tokio::test]
    async fn plan_start_requires_task() {
        let home = tempfile::tempdir().unwrap();
        assert!(is_error(
            &handle_plan_start(&json!({}), home.path(), "a1").await
        ));
    }

    #[tokio::test]
    async fn plan_start_clarify_first_default() {
        let home = tempfile::tempdir().unwrap();
        let v =
            handle_plan_start(&json!({"task": "ship the billing page"}), home.path(), "a1").await;
        assert!(!is_error(&v));
        let p: Value = serde_json::from_str(&text(&v)).unwrap();
        assert_eq!(p["clarify_first"], true);
        assert_eq!(p["max_questions"], 3);
    }

    #[tokio::test]
    async fn plan_start_direct_mode_when_disabled() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("a1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[planner]\nclarify_first = false\n",
        )
        .unwrap();
        let v = handle_plan_start(&json!({"task": "x"}), home.path(), "a1").await;
        let p: Value = serde_json::from_str(&text(&v)).unwrap();
        assert_eq!(p["clarify_first"], false);
        assert!(text(&v).contains("direct mode"));
    }

    /// Regression (2026-09-28 audit, debt #9 same-family): a team role member
    /// is an `eph-*` agent scaffolded at `<home>/agents/.ephemeral/<id>/`. The
    /// bare `agents/<id>` join never saw its `agent.toml`, so a role member
    /// that had turned clarify-first off was still handed the clarify-first
    /// scaffold every round.
    #[tokio::test]
    async fn eph_caller_planner_settings_resolve_inside_the_ephemeral_scaffold() {
        let home = tempfile::tempdir().unwrap();
        let eph = "eph-agnes-r1-planner-ab12";
        let scaffold = home
            .path()
            .join("agents")
            .join(duduclaw_gateway::ephemeral::EPHEMERAL_DIR_NAME)
            .join(eph);
        std::fs::create_dir_all(&scaffold).unwrap();
        std::fs::write(
            scaffold.join("agent.toml"),
            "[planner]\nclarify_first = false\nmax_questions = 2\n",
        )
        .unwrap();

        let s = load_planner_settings(home.path(), eph);
        assert!(!s.clarify_first);
        assert_eq!(s.max_questions, 2);

        let v = handle_plan_start(&json!({"task": "x"}), home.path(), eph).await;
        assert!(text(&v).contains("direct mode"));
        // Nothing was minted in the registry on the way there.
        assert!(!home.path().join("agents").join(eph).exists());
    }

    /// The companion half: an ordinary registry id is byte-identical to the
    /// pre-fix `agents/<id>/agent.toml` join.
    #[test]
    fn ordinary_caller_planner_settings_still_read_the_registry_path() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("agnes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[planner]\nmax_questions = 5\n").unwrap();
        let s = load_planner_settings(home.path(), "agnes");
        assert!(s.clarify_first);
        assert_eq!(s.max_questions, 5);
    }
}
