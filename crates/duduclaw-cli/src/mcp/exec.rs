use super::*;

pub(crate) async fn handle_execute_program(args: &Value) -> Value {
    use crate::ptc::sandbox::{PtcRpcServer, PtcSandbox};
    use crate::ptc::types::{ScriptLanguage, ScriptRequest};

    let code = match args.get("code").and_then(|v| v.as_str()) {
        Some(c) => c.to_string(),
        None => return tool_error("Missing required parameter: code"),
    };
    let language = match args.get("language").and_then(|v| v.as_str()) {
        Some(l) => l.to_string(),
        None => return tool_error("Missing required parameter: language"),
    };
    let timeout_seconds = args
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .min(300);

    tracing::info!(language, timeout_seconds, "execute_program called");

    let script_language = match language.as_str() {
        "python" => ScriptLanguage::Python,
        "bash" => ScriptLanguage::Bash,
        "javascript" => ScriptLanguage::Bash, // node -e via bash wrapper below
        other => {
            return tool_error(&format!(
                "Unsupported language: '{}'. Supported: python, bash, javascript",
                other
            ));
        }
    };

    // For javascript, wrap the code in a bash invocation of node -e
    let script_code = if language == "javascript" {
        // Escape single quotes in the JS code for safe embedding in bash
        let escaped = code.replace('\'', "'\\''");
        format!("node -e '{escaped}'")
    } else {
        code
    };

    const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MB

    let req = ScriptRequest {
        script: script_code,
        language: script_language,
        timeout_ms: timeout_seconds * 1000,
        max_output_bytes: MAX_OUTPUT_BYTES,
    };

    // Create a temporary RPC server for the sandbox execution.
    // If a PTC socket is already set in the environment, reuse that path;
    // otherwise create a unique temporary socket path.
    let socket_path = std::env::var("DUDUCLAW_PTC_SOCKET")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("duduclaw_ptc_exec_{}.sock", std::process::id()))
        });
    let rpc_server = PtcRpcServer::new(socket_path);

    // Use PtcSandbox::execute_in_container which tries container isolation
    // first and falls back to direct subprocess execution.
    match PtcSandbox::execute_in_container(&req, &rpc_server).await {
        Ok(result) => {
            if result.exit_code == 0 {
                serde_json::json!({
                    "content": [{ "type": "text", "text": result.stdout }],
                })
            } else {
                serde_json::json!({
                    "content": [{ "type": "text", "text": format!(
                        "Program exited with code {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        result.exit_code, result.stdout, result.stderr
                    ) }],
                    "isError": true,
                })
            }
        }
        Err(e) => tool_error(&format!("Failed to execute {language}: {e}")),
    }
}

// ── office_script handler ───────────────────────────────────────
//
// Server-side execution of a bundled office skill's vetted `scripts/*.py`
// (docx/xlsx/pptx/pdf) so API-mode agents that have no Bash tool can still
// produce document files. Fail-closed on every input:
//   - `skill` must be one of the four bundled office skills.
//   - `script` must be a bare stem (alnum / `_` / `-`) resolving to an existing
//     `<agent>/SKILLS/<skill>/scripts/<script>.py`.
//   - every path-shaped argument must resolve inside the caller's agent dir or
//     the shared `{home}/attachments` root — the same sandbox the outbound
//     `📎DELIVER:` validator trusts.
// The agent id comes from the caller context (`default_agent`), never from
// arguments, so an agent can only ever run scripts in its own directory.

/// The four bundled office skills whose scripts `office_script` may run.
pub(crate) const OFFICE_SCRIPT_SKILLS: &[&str] = &["docx", "xlsx", "pptx", "pdf"];

/// Whether a single script argument is allowed. A bare flag/value with no path
/// separator (`--format`, `json`) always passes UNLESS its basename is a
/// protected agent-structure file (WP1.1 — see below); a path-shaped argument
/// must normalise to inside `agent_root` or `attach_root` and must not target
/// a protected basename either. Validation is lexical (no `canonicalize`)
/// because an `--out` file does not exist yet.
///
/// WP1.1 (SOUL.md 唯讀化) follow-up: `office_script` runs a bundled
/// docx/xlsx/pptx/pdf skill script with `cwd = agent_root` and previously only
/// checked that path-shaped arguments stayed *inside* the agent's own
/// directory — it never excluded the DuDuClaw-specific structure files that
/// live there. A bare `--out SOUL.md` (or `--out agent.toml`) argument passed
/// the old check trivially (no `/` at all) and would let the office script
/// overwrite a protected file with arbitrary bytes, sidestepping the C1–C4
/// gates entirely (none of which watch this tool). `AGENT_STRUCTURE_FILES` is
/// the exact same list `check_agent_file_write` protects on the Write/Edit/
/// Bash hook lane; this closes the same class of gap for `office_script`.
pub(crate) fn office_arg_within_sandbox(arg: &str, agent_root: &Path, attach_root: &Path) -> bool {
    let has_sep = arg.contains('/') || arg.contains('\\');
    let p = Path::new(arg);
    let abs = if !has_sep || p.is_relative() {
        agent_root.join(p)
    } else {
        p.to_path_buf()
    };
    let norm = duduclaw_core::agent_guard::lexical_normalize(&abs);
    if let Some(basename) = norm.file_name().and_then(|n| n.to_str()) {
        if duduclaw_core::AGENT_STRUCTURE_FILES
            .iter()
            .any(|protected| protected.eq_ignore_ascii_case(basename))
        {
            return false;
        }
    }
    if !has_sep {
        return true;
    }
    norm.starts_with(agent_root) || norm.starts_with(attach_root)
}

pub(crate) async fn handle_office_script(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    // 1. skill allowlist (fail-closed).
    let skill = args
        .get("skill")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if !OFFICE_SCRIPT_SKILLS.contains(&skill) {
        return tool_error(&format!(
            "Invalid skill '{skill}'. Must be one of: docx, xlsx, pptx, pdf"
        ));
    }

    // 2. script stem validation — a bare name, no path components.
    let raw_script = args
        .get("script")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let stem = raw_script.strip_suffix(".py").unwrap_or(raw_script);
    if stem.is_empty()
        || !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return tool_error(
            "Invalid script name. Use a bare stem like 'create', 'extract', or 'to_pdf' \
             (alphanumeric, '_' or '-'; no path separators).",
        );
    }

    // 3. resolve the script inside the CALLER's own agent dir (never from args).
    // W3-3b (a): `.ephemeral/` included — a role member's SKILLS live in its
    // own scaffold, and the bare registry path made every bundled office
    // script report "Script not found" for team members.
    let agent_dir = caller_agent_dir(home_dir, default_agent);
    let script_path = agent_dir
        .join("SKILLS")
        .join(skill)
        .join("scripts")
        .join(format!("{stem}.py"));
    if !script_path.is_file() {
        return tool_error(&format!(
            "Script not found: SKILLS/{skill}/scripts/{stem}.py (agent '{default_agent}'). \
             Available office scripts: create, extract, to_pdf."
        ));
    }

    // 4. validate every path-shaped argument stays inside the sandbox.
    let attach_root = home_dir.join("attachments");
    let script_args: Vec<String> = match args.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                let Some(s) = v.as_str() else {
                    return tool_error("Every element of 'args' must be a string.");
                };
                if !office_arg_within_sandbox(s, &agent_dir, &attach_root) {
                    return tool_error(&format!(
                        "Argument path escapes the agent sandbox: {s}. Inputs and --out must \
                         live under your agent directory or its attachments/."
                    ));
                }
                out.push(s.to_string());
            }
            out
        }
        Some(_) => return tool_error("'args' must be a JSON array of strings."),
    };

    // 5. WP-4G: resource ceilings on every INPUT document before a parser runs.
    //
    // `.docx` / `.xlsx` / `.pptx` are zip containers, and the file an agent
    // points at here is routinely something a stranger sent into a channel.
    // The skill scripts hand those bytes to python-docx / openpyxl /
    // python-pptx, none of which bound decompression or XML nesting: a zip
    // bomb takes the host's RAM with it, and a deeply-nested part crashes a
    // recursive-descent parser. Gate before spawning; fail-closed.
    //
    // Only *existing* files are inspected, so `--out` targets (not yet
    // created) and bare flags (`--format`) fall through untouched, and a
    // non-zip input (`.csv`, `.pdf`, legacy `.doc`) is out of scope by design.
    let limits = duduclaw_gateway::document_limits::DocumentLimits::from_home(home_dir);
    for arg in &script_args {
        let p = Path::new(arg);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            agent_dir.join(p)
        };
        if !abs.is_file() {
            continue;
        }
        if let Err(v) = duduclaw_gateway::document_limits::guard_document_path(&abs, &limits) {
            let shown = abs.file_name().and_then(|n| n.to_str()).unwrap_or("input");
            tracing::warn!(
                file = %shown,
                violation = v.kind(),
                "office_script: refused — input document exceeds resource limits"
            );
            return tool_error(&v.user_message(shown));
        }
    }

    // 6. run the script (cwd = agent dir so relative paths stay in the sandbox).
    run_office_script(&script_path, &script_args, &agent_dir).await
}

/// Spawn `uv run <script> <args...>` (PEP 723 deps auto-resolved), falling back
/// to the system python3 when `uv` is not installed. `kill_on_drop` guarantees a
/// timed-out child is reaped. Returns the script's stdout on success, or a
/// diagnostic error with stdout+stderr on non-zero exit.
pub(crate) async fn run_office_script(script_path: &Path, script_args: &[String], cwd: &Path) -> Value {
    use tokio::process::Command;
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
    const MAX_OUT: usize = 32_768;

    let build = |program: &str, prefix: &[&str]| {
        let mut cmd = Command::new(program);
        cmd.current_dir(cwd).kill_on_drop(true);
        for a in prefix {
            cmd.arg(a);
        }
        cmd.arg(script_path)
            .args(script_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd
    };

    let py = duduclaw_core::platform::python3_command();
    let (used, child) = match build("uv", &["run"]).spawn() {
        Ok(c) => ("uv run", c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match build(py, &[]).spawn() {
            Ok(c) => (py, c),
            Err(e2) => {
                return tool_error(&format!(
                    "Neither 'uv' nor '{py}' could be spawned to run the office script: {e2}. \
                     Install uv (https://docs.astral.sh/uv/) or the script's Python deps."
                ));
            }
        },
        Err(e) => return tool_error(&format!("Failed to spawn 'uv run': {e}")),
    };

    let output = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return tool_error(&format!("office script execution error ({used}): {e}")),
        // Timeout drops `child`; kill_on_drop reaps the subprocess.
        Err(_) => {
            return tool_error(&format!(
                "office script timed out after {}s ({used})",
                TIMEOUT.as_secs()
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        let body = duduclaw_core::truncate_bytes(stdout.trim(), MAX_OUT);
        tool_text(&format!(
            "office_script ok ({used}, exit 0).\n{body}\n\nIf a file was produced, end your \
             reply with a line `📎DELIVER:<absolute path>` so it reaches the user."
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        tool_error(&format!(
            "office_script failed ({used}, exit {code}).\n--- stdout ---\n{}\n--- stderr ---\n{}",
            duduclaw_core::truncate_bytes(&stdout, MAX_OUT),
            duduclaw_core::truncate_bytes(&stderr, MAX_OUT),
        ))
    }
}

// ── skill_bank_feedback handler ─────────────────────────────────
// (`skill_bank_search`'s own handler was removed by T5/O13: the tool is now a
// deprecated alias routing through `handle_skill_search` with source="bank",
// so there is one entry point and one result format. The bank half of that
// search lives in `skill_bank_hits`.)

pub(crate) async fn handle_session_restore_context(args: &Value) -> Value {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None => return tool_error("Missing required parameter: query"),
    };

    tracing::debug!(
        query = query.as_str(),
        "session_restore_context query content"
    );
    tracing::info!(query_len = query.len(), "session_restore_context called");

    // Search hidden messages in current session
    // In full implementation, this would use session_manager.search_hidden_messages()
    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::json!({
            "query": query,
            "results": [],
            "total": 0,
            "note": "Search for hidden/archived messages. Results returned when session context is available.",
        }).to_string() }],
    })
}
