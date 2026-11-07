use super::*;

struct TempHome(std::path::PathBuf);
impl TempHome {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("duduclaw-office-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Materialise `<home>/agents/<agent>/SKILLS/<skill>/scripts/<name>.py`.
fn install_script(home: &std::path::Path, agent: &str, skill: &str, name: &str, body: &str) {
    let dir = home
        .join("agents")
        .join(agent)
        .join("SKILLS")
        .join(skill)
        .join("scripts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.parent().unwrap().join("SKILL.md"), "# Office skill").unwrap();
    std::fs::write(dir.join(format!("{name}.py")), body).unwrap();
}

fn text_of(v: &Value) -> String {
    v.get("content")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string()
}
fn is_err(v: &Value) -> bool {
    v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false)
}

// ── Handler fail-closed input validation ────────────────────────────
#[tokio::test]
async fn rejects_unknown_skill() {
    let home = TempHome::new();
    let out = handle_office_script(
        &serde_json::json!({ "skill": "keynote", "script": "create" }),
        home.path(),
        "bot",
    )
    .await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("Invalid skill"));
}

#[tokio::test]
async fn rejects_script_with_path_separator() {
    let home = TempHome::new();
    let out = handle_office_script(
        &serde_json::json!({ "skill": "pptx", "script": "../../evil" }),
        home.path(),
        "bot",
    )
    .await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("Invalid script name"));
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_script_outside_platform_manifest() {
    let home = TempHome::new();
    // Skill valid, stem valid, but no such script installed.
    let out = handle_office_script(
        &serde_json::json!({ "skill": "pptx", "script": "untrusted" }),
        home.path(),
        "bot",
    )
    .await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("Script not found"));
}

#[tokio::test]
async fn rejects_arg_escaping_sandbox() {
    let home = TempHome::new();
    install_script(home.path(), "bot", "pptx", "create", "print('x')\n");
    let out = handle_office_script(
        &serde_json::json!({
            "skill": "pptx",
            "script": "create",
            "args": ["/etc/passwd", "--out", "out.pptx"]
        }),
        home.path(),
        "bot",
    )
    .await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("office_script refused"));
}

#[tokio::test]
async fn office_script_preserves_disabled_and_unreadable_skill_gates() {
    let home = TempHome::new();
    let state = home.path().join("skill_state");
    std::fs::create_dir(&state).unwrap();
    for content in [r#"{"version":1,"disabled":["pdf"]}"#, "broken"] {
        std::fs::write(state.join("bot.json"), content).unwrap();
        let out = handle_office_script(
            &serde_json::json!({ "skill": "pdf", "script": "create", "args": ["in.md", "--out", "out.pdf"] }),
            home.path(), "bot",
        ).await;
        assert!(is_err(&out));
        assert!(text_of(&out).contains("switched off"));
        assert!(!home.path().join(".office-execution").exists());
    }
}

#[tokio::test]
async fn rejects_non_string_arg() {
    let home = TempHome::new();
    install_script(home.path(), "bot", "pptx", "create", "print('x')\n");
    let out = handle_office_script(
        &serde_json::json!({ "skill": "pptx", "script": "create", "args": [42] }),
        home.path(),
        "bot",
    )
    .await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("must be a string"));
}

// The executable and filesystem regressions run in duduclaw-agent's
// office_execution::tests so they can be tested without compiling the gateway.
#[cfg(unix)]
#[tokio::test]
async fn office_script_rejects_inline_output_flag_before_spawn() {
    let home = TempHome::new();
    install_script(home.path(), "bot", "pdf", "create", "raise Exception('employee code')");
    let out = handle_office_script(
        &serde_json::json!({ "skill": "pdf", "script": "create", "args": ["in.md", "--out=/abs/out.pdf"] }),
        home.path(), "bot",
    ).await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("--key=value"));
}
