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

// ── Path sandbox validation ─────────────────────────────────────────
#[test]
fn arg_sandbox_allows_bare_flags_and_values() {
    let root = std::path::Path::new("/home/agent");
    let attach = std::path::Path::new("/home/attachments");
    assert!(office_arg_within_sandbox("--format", root, attach));
    assert!(office_arg_within_sandbox("json", root, attach));
    assert!(office_arg_within_sandbox("outline.md", root, attach)); // relative, no sep
}

#[test]
fn arg_sandbox_allows_paths_inside_agent_and_attachments() {
    let root = std::path::Path::new("/home/agent");
    let attach = std::path::Path::new("/home/attachments");
    assert!(office_arg_within_sandbox(
        "/home/agent/attachments/out.pptx",
        root,
        attach
    ));
    assert!(office_arg_within_sandbox(
        "attachments/out.pptx",
        root,
        attach
    )); // relative → joins root
    assert!(office_arg_within_sandbox(
        "/home/attachments/in.pptx",
        root,
        attach
    ));
}

#[test]
fn arg_sandbox_rejects_escapes_and_absolute_outside() {
    let root = std::path::Path::new("/home/agent");
    let attach = std::path::Path::new("/home/attachments");
    assert!(!office_arg_within_sandbox("/etc/passwd", root, attach));
    assert!(!office_arg_within_sandbox(
        "../victim/secret.docx",
        root,
        attach
    ));
    assert!(!office_arg_within_sandbox(
        "/home/agent/../victim/x",
        root,
        attach
    ));
}

/// WP1.1 follow-up: a bare `--out SOUL.md` (no path separator at all)
/// used to pass trivially — the "no separator ⇒ always allowed" branch
/// ran before any containment check. This is the exact office_script
/// gap the WP1.1 SOUL write-path inventory found: none of the C1–C4
/// gates watch this tool, so it was a live way to overwrite a protected
/// agent-structure file with arbitrary script output.
#[test]
fn arg_sandbox_rejects_bare_protected_structure_filenames() {
    let root = std::path::Path::new("/home/agent");
    let attach = std::path::Path::new("/home/attachments");
    for bare in [
        "SOUL.md",
        "soul.md",
        "agent.toml",
        "CLAUDE.md",
        "MEMORY.md",
        "CONTRACT.toml",
        ".mcp.json",
    ] {
        assert!(
            !office_arg_within_sandbox(bare, root, attach),
            "bare '{bare}' must be rejected"
        );
    }
    // A full path to the same protected file, even though it resolves
    // inside the sandbox, must be rejected the same way.
    assert!(!office_arg_within_sandbox(
        "/home/agent/SOUL.md",
        root,
        attach
    ));
    assert!(
        !office_arg_within_sandbox("SOUL.MD", root, attach),
        "case-insensitive"
    );
    // A normal output filename that merely CONTAINS "soul" is untouched.
    assert!(office_arg_within_sandbox(
        "soul-searching-report.docx",
        root,
        attach
    ));
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

#[tokio::test]
async fn rejects_missing_script_file() {
    let home = TempHome::new();
    // Skill valid, stem valid, but no such script installed.
    let out = handle_office_script(
        &serde_json::json!({ "skill": "pptx", "script": "create" }),
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
            "args": ["/etc/passwd"]
        }),
        home.path(),
        "bot",
    )
    .await;
    assert!(is_err(&out));
    assert!(text_of(&out).contains("escapes the agent sandbox"));
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

// ── Live execution (skips when no Python runner is on PATH) ──────────
#[tokio::test]
async fn runs_a_real_script_and_returns_stdout() {
    // Only meaningful when `uv` or python3 exists — skip otherwise so CI
    // on a runner without a Python toolchain does not fail.
    let py = duduclaw_core::platform::python3_command();
    let has_uv = std::process::Command::new("uv")
        .arg("--version")
        .output()
        .is_ok();
    let has_py = std::process::Command::new(py)
        .arg("--version")
        .output()
        .is_ok();
    if !has_uv && !has_py {
        eprintln!("skipping: neither uv nor {py} available");
        return;
    }

    let home = TempHome::new();
    // A dependency-free script so `python3` fallback also works (uv would
    // resolve PEP 723 deps, but we assert only on stdout here). The `.stem`
    // is `extract` to exercise a real allowlisted name.
    install_script(
        home.path(),
        "bot",
        "pptx",
        "extract",
        "print('OFFICE_SCRIPT_OK')\n",
    );
    let out = handle_office_script(
        &serde_json::json!({ "skill": "pptx", "script": "extract" }),
        home.path(),
        "bot",
    )
    .await;
    assert!(!is_err(&out), "expected success, got: {}", text_of(&out));
    assert!(text_of(&out).contains("OFFICE_SCRIPT_OK"));
}
