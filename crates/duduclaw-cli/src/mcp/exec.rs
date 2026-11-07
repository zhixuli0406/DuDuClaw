use super::*;

pub(crate) async fn handle_execute_program(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
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

    // The RPC bridge descriptor. Nothing serves this socket yet
    // (`PtcRpcServer` has no listener) and the container path never mounts
    // it. The path is always our own per-process name — never one taken
    // from the environment, since dropping the descriptor deletes the file.
    let socket_path =
        std::env::temp_dir().join(format!("duduclaw_ptc_exec_{}.sock", std::process::id()));
    let rpc_server = PtcRpcServer::new(socket_path);

    // Container isolation first; when it cannot run, `[container.sandbox]
    // script_when_unavailable` decides between refusing (default) and an
    // audited host run.
    match PtcSandbox::run_program(&req, &rpc_server, home_dir, default_agent).await {
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
// Skill layers override prompt text only. Executable bytes and argument schemas
// come exclusively from the binary's versioned, SHA-256-verified manifest.
pub(crate) const OFFICE_SCRIPT_SKILLS: &[&str] = &["docx", "xlsx", "pptx", "pdf"];

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

    // Keep the per-employee skill switch in addition to the central MCP gates.
    if duduclaw_agent::skill_state::read_disabled(home_dir, default_agent).is_disabled(skill) {
        return tool_error(&format!(
            "Skill '{skill}' is switched off for agent '{default_agent}'."
        ));
    }
    let agent_dir = caller_agent_dir(home_dir, default_agent);
    let script_args: Vec<String> = match args.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                let Some(s) = v.as_str() else {
                    return tool_error("Every element of 'args' must be a string.");
                };
                out.push(s.to_string());
            }
            out
        }
        Some(_) => return tool_error("'args' must be a JSON array of strings."),
    };

    let prepared = match duduclaw_agent::office_execution::PreparedOffice::new(
        home_dir, &agent_dir, skill, stem, &script_args,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return tool_error(&format!("office_script refused: {error}")),
    };
    // Apply the existing document resource gate to the exact private snapshot
    // that Python will read, never a mutable source pathname.
    let limits = duduclaw_gateway::document_limits::DocumentLimits::from_home(home_dir);
    if let Err(v) = duduclaw_gateway::document_limits::guard_document_path(prepared.input(), &limits) {
        tracing::warn!(violation = v.kind(), "office_script: input exceeds resource limits");
        return tool_error(&v.user_message("input"));
    }
    run_office_script(&prepared).await
}

/// Execute through the shared bounded runner; publication remains in Rust.
/// The runner's `ManagedChild::spawn` passes `execution_gate::check_local`, so a
/// protected Linux install refuses this local spawn (fail-closed).
pub(crate) async fn run_office_script(prepared: &duduclaw_agent::office_execution::PreparedOffice) -> Value {
    const MAX_OUT: usize = duduclaw_agent::office_execution::process::MAX_PIPE_BYTES;
    let result = match duduclaw_agent::office_execution::process::run(prepared).await {
        Ok(result) => result,
        Err(error) => return tool_error(&format!("office script execution refused: {error}")),
    };
    let used = result.used;
    let output = result.output;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        let published = match prepared.publish() {
            Ok(path) => path,
            Err(error) => return tool_error(&format!("office_script output refused: {error}")),
        };
        let body = if let Some(path) = published {
            format!("Created {}", path.display())
        } else {
            duduclaw_core::truncate_bytes(stdout.trim(), MAX_OUT).to_string()
        };
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
// (Searching the bank is `skill_search` with source="bank"; the bank half of
// that search lives in `skill_bank_hits`.)

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused script sandbox reaches the tool result the model (and the
    /// operator reading the transcript) sees: reason code and remedy, flagged
    /// as an error, never replaced by a generic label.
    #[tokio::test]
    #[cfg(unix)]
    async fn script_sandbox_refusal_reaches_the_tool_result() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[container.sandbox]\npids = 0\n").unwrap();
        let args = serde_json::json!({ "code": "echo hi", "language": "bash" });
        let result = handle_execute_program(&args, home.path(), "worker").await;
        assert_eq!(result["isError"], serde_json::json!(true), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("Script sandbox unavailable (invalid_config)"), "{text}");
        assert!(text.contains("The script was not run"), "{text}");
        assert!(text.contains("Fix the [container.sandbox] section of config.toml"), "{text}");
    }
}
