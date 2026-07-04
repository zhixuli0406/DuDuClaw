//! SandboxRunner — execute agent tasks inside isolated Docker containers.
//!
//! [A-1a] Creates a one-shot container with:
//! - Agent directory mounted read-only
//! - Workspace as tmpfs
//! - Network disabled by default (configurable)
//! - Timeout-based auto-kill
//! - API key passed via container env var (isolated per container, not visible to host)

use std::path::Path;
use std::time::Duration;

use duduclaw_core::types::RuntimeType;
use tempfile::NamedTempFile;

use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures_util::StreamExt;
use tracing::{info, warn};

const SANDBOX_IMAGE: &str = "duduclaw-agent:latest";

/// Result of a sandboxed agent execution.
pub struct SandboxResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub timed_out: bool,
}

/// Execute a prompt inside an isolated Docker container for a specific agent.
///
/// # Arguments
/// - `agent_dir`: Path to the agent directory (mounted read-only)
/// - `prompt`: The task/prompt to execute
/// - `model`: LLM model ID
/// - `system_prompt`: Agent system prompt
/// - `api_key`: API key (injected via container env, isolated per container)
/// - `timeout`: Maximum execution time
/// - `network_access`: Whether to allow network inside the container
pub async fn run_sandboxed(
    agent_dir: &Path,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    api_key: &str,
    timeout: Duration,
    network_access: bool,
) -> Result<SandboxResult, String> {
    run_sandboxed_with_env(agent_dir, prompt, model, system_prompt, api_key, timeout, network_access, &[], &[]).await
}

/// Like [`run_sandboxed`] but accepts additional environment variables
/// (e.g., delegation depth tracking) and tool restrictions.
///
/// Runs the Claude CLI (legacy default). Non-Claude agents should use
/// [`run_sandboxed_for_runtime`].
#[allow(clippy::too_many_arguments)]
pub async fn run_sandboxed_with_env(
    agent_dir: &Path,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    api_key: &str,
    timeout: Duration,
    network_access: bool,
    extra_env: &[(String, String)],
    disallowed_tools: &[String],
) -> Result<SandboxResult, String> {
    run_sandboxed_for_runtime(
        RuntimeType::Claude,
        agent_dir,
        prompt,
        model,
        system_prompt,
        api_key,
        timeout,
        network_access,
        extra_env,
        disallowed_tools,
    )
    .await
}

/// Runtime-parameterized sandbox execution: the in-container argv follows the
/// agent's `[runtime] provider` flag dialect (Claude default; Codex / Gemini /
/// Antigravity per [`build_agent_cmd`]). Capability posture stays
/// deny-by-default — the container itself (no network, read-only rootfs,
/// tmpfs workspace) is the outer wall; `disallowed_tools` narrows further.
#[allow(clippy::too_many_arguments)]
pub async fn run_sandboxed_for_runtime(
    runtime: RuntimeType,
    agent_dir: &Path,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    api_key: &str,
    timeout: Duration,
    network_access: bool,
    extra_env: &[(String, String)],
    disallowed_tools: &[String],
) -> Result<SandboxResult, String> {
    let client = Docker::connect_with_local_defaults()
        .map_err(|e| format!("Docker connect failed: {e}"))?;

    let container_name = format!("duduclaw-sandbox-{}", uuid::Uuid::new_v4());

    // Write API key to a secure temp file (O_EXCL creation).
    // Convert to TempPath so we control exactly when deletion happens — this
    // prevents async cancellation from dropping the file while the container
    // still needs to read it (R4-M3).
    let key_file = NamedTempFile::new()
        .map_err(|e| format!("Failed to create temp key file: {e}"))?;
    let key_path = key_file.into_temp_path();
    std::fs::write(&key_path, api_key)
        .map_err(|e| format!("Failed to write API key to temp file: {e}"))?;
    duduclaw_core::platform::set_owner_only(&key_path).ok();
    let key_path_str = key_path.to_string_lossy().to_string();

    // Build mount: agent dir as read-only, key file as read-only bind mount
    let agent_dir_str = agent_dir.to_string_lossy().to_string();
    let binds = vec![
        format!("{agent_dir_str}:/agent:ro"),
        format!("{key_path_str}:/run/secrets/api_key:ro"),
    ];

    // Sanitize system_prompt: remove newlines and prevent CLI flag injection (R3-H5)
    let safe_prompt = system_prompt
        .replace(['\n', '\r'], " ");
    let safe_prompt = if safe_prompt.starts_with('-') {
        format!(" {safe_prompt}")
    } else {
        safe_prompt
    };

    // Build the in-container argv following the runtime's flag dialect.
    let cmd = build_agent_cmd(runtime, prompt, model, &safe_prompt, disallowed_tools);
    if runtime != RuntimeType::Claude && !disallowed_tools.is_empty() {
        warn!(
            runtime = runtime.as_str(),
            "capability enforcement is best-effort on this runtime — per-tool deny \
             lists collapse to coarse sandbox flags (container isolation still applies)"
        );
    }

    // Environment variables: API key file + any extra env (e.g., delegation context).
    // SAFETY: Keys and values are assumed pre-validated (agent IDs pass
    // is_valid_agent_id: [a-zA-Z0-9_-]; depth is u8). If adding arbitrary
    // user input here in the future, escape or reject `=` and newlines in keys.
    //
    // Claude reads the key through ANTHROPIC_API_KEY_FILE (never surfaces in
    // `docker inspect`). The other CLIs have no *_FILE variant, so their key
    // rides the container env directly — acceptable because the env is scoped
    // to this one-shot container, but the file indirection is preferred where
    // the CLI supports it.
    let mut env = vec!["ANTHROPIC_API_KEY_FILE=/run/secrets/api_key".to_string()];
    match runtime {
        RuntimeType::Claude => {}
        RuntimeType::Codex => env.push(format!("OPENAI_API_KEY={api_key}")),
        RuntimeType::Gemini => env.push(format!("GEMINI_API_KEY={api_key}")),
        RuntimeType::Antigravity => env.push(format!("ANTIGRAVITY_API_KEY={api_key}")),
        // No CLI binary for OpenAI-compat — treated as Claude-shaped fallback
        // upstream; nothing extra to inject here.
        RuntimeType::OpenAiCompat => {}
    }
    for (k, v) in extra_env {
        env.push(format!("{k}={v}"));
    }

    let network_mode = if network_access {
        None // Use default bridge network
    } else {
        Some("none".to_string()) // Complete network isolation
    };

    let host_config = bollard::models::HostConfig {
        binds: Some(binds),
        network_mode,
        // tmpfs for workspace
        tmpfs: Some(
            [("/workspace".to_string(), "rw,noexec,nosuid,size=256m".to_string())]
                .into_iter()
                .collect(),
        ),
        // Memory limit: 512MB
        memory: Some(512 * 1024 * 1024),
        // No privileged
        privileged: Some(false),
        // Read-only root filesystem
        readonly_rootfs: Some(true),
        ..Default::default()
    };

    let mut labels = std::collections::HashMap::new();
    labels.insert("managed-by".to_string(), "duduclaw-sandbox".to_string());

    let config = Config {
        image: Some(SANDBOX_IMAGE.to_string()),
        cmd: Some(cmd),
        env: Some(env),
        working_dir: Some("/workspace".to_string()),
        labels: Some(labels),
        host_config: Some(host_config),
        ..Default::default()
    };

    let options = CreateContainerOptions {
        name: container_name.as_str(),
        platform: None,
    };

    // Create container
    let response = client
        .create_container(Some(options), config)
        .await
        .map_err(|e| format!("Create container failed: {e}"))?;

    let container_id = response.id;
    info!(id = %container_id, "Sandbox container created");

    // Start container
    client
        .start_container(&container_id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Start container failed: {e}"))?;

    // Wait for completion with timeout
    let wait_opts = WaitContainerOptions {
        condition: "not-running",
    };

    let timed_out;
    let exit_code;

    match tokio::time::timeout(timeout, async {
        let mut stream = client.wait_container(&container_id, Some(wait_opts));
        if let Some(Ok(result)) = stream.next().await {
            result.status_code
        } else {
            -1
        }
    })
    .await
    {
        Ok(code) => {
            timed_out = false;
            exit_code = code;
        }
        Err(_) => {
            timed_out = true;
            exit_code = -1;
            warn!(id = %container_id, "Sandbox execution timed out, killing container");
            let _ = client
                .stop_container(
                    &container_id,
                    Some(bollard::container::StopContainerOptions { t: 5 }),
                )
                .await;
        }
    }

    // Collect logs
    let log_opts = LogsOptions::<String> {
        stdout: true,
        stderr: true,
        follow: false,
        ..Default::default()
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut stream = client.logs(&container_id, Some(log_opts));

    while let Some(Ok(chunk)) = stream.next().await {
        match chunk {
            bollard::container::LogOutput::StdOut { message } => {
                stdout.push_str(&String::from_utf8_lossy(&message));
            }
            bollard::container::LogOutput::StdErr { message } => {
                stderr.push_str(&String::from_utf8_lossy(&message));
            }
            _ => {}
        }
    }

    // Cleanup: remove container
    let remove_opts = RemoveContainerOptions {
        force: true,
        ..Default::default()
    };
    if let Err(e) = client
        .remove_container(&container_id, Some(remove_opts))
        .await
    {
        warn!(id = %container_id, "Failed to remove sandbox container: {e}");
    } else {
        info!(id = %container_id, "Sandbox container cleaned up");
    }

    // Explicitly drop key_path AFTER container removal so the API key file
    // is never deleted while the container is still running (R4-M3).
    drop(key_path);

    Ok(SandboxResult {
        stdout,
        stderr,
        exit_code,
        timed_out,
    })
}

/// Check if Docker is available for sandbox operations.
pub async fn is_sandbox_available() -> bool {
    match Docker::connect_with_local_defaults() {
        Ok(client) => client.ping().await.is_ok(),
        Err(_) => false,
    }
}

// ── Runtime-parameterized argv ──────────────────────────────────

/// Write-capable tools used to detect a "read-only intent" deny list.
const WRITE_TOOLS: [&str; 5] = ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"];

/// True when the deny list bare-denies EVERY write-capable tool (exact
/// case-insensitive token match — never substring, per the 2026-06 review
/// conventions). Used to map Claude-style deny lists onto the coarse
/// read-only sandbox mode of Codex / Gemini.
fn all_write_tools_denied(disallowed_tools: &[String]) -> bool {
    WRITE_TOOLS.iter().all(|tool| {
        disallowed_tools
            .iter()
            .any(|d| d.trim().eq_ignore_ascii_case(tool))
    })
}

/// Neutralize a leading `-` so a prompt can never be parsed as a CLI flag.
fn flag_safe(prompt: &str) -> String {
    if prompt.starts_with('-') {
        format!(" {prompt}")
    } else {
        prompt.to_string()
    }
}

/// Build the in-container argv for `runtime`.
///
/// Deny-by-default posture:
/// - Claude: `--disallowedTools` passthrough (unchanged legacy behaviour).
/// - Codex: `--ask-for-approval never` + `--sandbox read-only|workspace-write`
///   (NEVER `danger-full-access` inside the sandbox — the container grants no
///   full-host semantics to escalate to). System prompt is embedded in the
///   prompt argument (codex exec has no `--system-prompt`; AGENTS.md is not
///   in the tmpfs workdir).
/// - Gemini: `--approval-mode auto_edit` (+ `--sandbox` on read-only intent);
///   system prompt embedded (no GEMINI_SYSTEM_MD temp file inside the container).
/// - Antigravity: `agy --dangerously-skip-permissions` (the CLI exposes no
///   alternative; the container is the enforcement boundary), `-p` last.
pub(crate) fn build_agent_cmd(
    runtime: RuntimeType,
    prompt: &str,
    model: &str,
    safe_system_prompt: &str,
    disallowed_tools: &[String],
) -> Vec<String> {
    match runtime {
        RuntimeType::Claude | RuntimeType::OpenAiCompat => {
            let mut cmd = vec![
                "claude".to_string(),
                "-p".to_string(),
                prompt.to_string(),
                "--model".to_string(),
                model.to_string(),
                "--output-format".to_string(),
                "text".to_string(),
                "--system-prompt".to_string(),
                safe_system_prompt.to_string(),
            ];
            if !disallowed_tools.is_empty() {
                cmd.push("--disallowedTools".to_string());
                cmd.push(disallowed_tools.join(","));
            }
            cmd
        }
        RuntimeType::Codex => {
            let sandbox = if all_write_tools_denied(disallowed_tools) {
                "read-only"
            } else {
                "workspace-write"
            };
            let mut cmd = vec![
                "codex".to_string(),
                "exec".to_string(),
                "--ask-for-approval".to_string(),
                "never".to_string(),
                "--sandbox".to_string(),
                sandbox.to_string(),
            ];
            if !model.is_empty() {
                cmd.push("-m".to_string());
                cmd.push(model.to_string());
            }
            cmd.push(flag_safe(&embed_system_prompt(safe_system_prompt, prompt)));
            cmd
        }
        RuntimeType::Gemini => {
            let mut cmd = vec![
                "gemini".to_string(),
                "-p".to_string(),
                "--approval-mode".to_string(),
                "auto_edit".to_string(),
            ];
            if all_write_tools_denied(disallowed_tools) {
                cmd.push("--sandbox".to_string());
            }
            if !model.is_empty() {
                cmd.push("-m".to_string());
                cmd.push(model.to_string());
            }
            cmd.push(flag_safe(&embed_system_prompt(safe_system_prompt, prompt)));
            cmd
        }
        RuntimeType::Antigravity => {
            let mut cmd = vec![
                "agy".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--print-timeout".to_string(),
                "300s".to_string(),
            ];
            if !model.is_empty() {
                cmd.push("--model".to_string());
                cmd.push(model.to_string());
            }
            // `-p` consumes the NEXT argv token as the prompt — it must be last.
            cmd.push("-p".to_string());
            cmd.push(flag_safe(&embed_system_prompt(safe_system_prompt, prompt)));
            cmd
        }
    }
}

/// Embed the system prompt into the user prompt for CLIs without a
/// `--system-prompt` flag. XML-delimited with closing-tag escaping so
/// untrusted content cannot break the frame.
fn embed_system_prompt(system_prompt: &str, prompt: &str) -> String {
    if system_prompt.trim().is_empty() {
        return prompt.to_string();
    }
    let safe_system =
        system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
    format!("<system_instructions>\n{safe_system}\n</system_instructions>\n\n{prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn claude_cmd_keeps_legacy_shape_with_denied_tools() {
        let cmd = build_agent_cmd(
            RuntimeType::Claude,
            "do the task",
            "claude-sonnet-4-6",
            "be safe",
            &s(&["computer"]),
        );
        assert_eq!(cmd[0], "claude");
        assert!(cmd.windows(2).any(|w| w[0] == "--disallowedTools" && w[1] == "computer"));
    }

    #[test]
    fn codex_cmd_never_full_access_and_derives_read_only() {
        let all_denied = s(&["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"]);
        let cmd = build_agent_cmd(RuntimeType::Codex, "task", "gpt-5", "sys", &all_denied);
        assert_eq!(cmd[0], "codex");
        assert!(cmd.windows(2).any(|w| w[0] == "--sandbox" && w[1] == "read-only"));
        assert!(!cmd.iter().any(|a| a == "danger-full-access"));

        let cmd = build_agent_cmd(RuntimeType::Codex, "task", "gpt-5", "sys", &[]);
        assert!(cmd.windows(2).any(|w| w[0] == "--sandbox" && w[1] == "workspace-write"));
    }

    #[test]
    fn gemini_cmd_uses_auto_edit_and_sandbox_on_read_only_intent() {
        let all_denied = s(&["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"]);
        let cmd = build_agent_cmd(RuntimeType::Gemini, "task", "", "sys", &all_denied);
        assert!(cmd.windows(2).any(|w| w[0] == "--approval-mode" && w[1] == "auto_edit"));
        assert!(cmd.iter().any(|a| a == "--sandbox"));
        assert!(!cmd.iter().any(|a| a == "yolo"));
    }

    #[test]
    fn antigravity_cmd_puts_prompt_flag_last() {
        let cmd = build_agent_cmd(RuntimeType::Antigravity, "task", "gemini-3-pro", "sys", &[]);
        assert_eq!(cmd[0], "agy");
        let p = cmd.iter().position(|a| a == "-p").expect("-p present");
        assert_eq!(p, cmd.len() - 2, "-p must immediately precede the final prompt token");
        assert!(cmd[p + 1].contains("task"));
        assert!(cmd[p + 1].contains("<system_instructions>"));
    }

    #[test]
    fn embedded_prompt_neutralizes_leading_dash() {
        let cmd = build_agent_cmd(RuntimeType::Codex, "--help", "", "", &[]);
        let last = cmd.last().unwrap();
        assert!(last.starts_with(' '), "leading dash must be neutralized: {last:?}");
    }

    #[test]
    fn partial_deny_list_is_not_read_only() {
        // Denying only Bash must not collapse to read-only (Write/Edit remain).
        assert!(!all_write_tools_denied(&s(&["Bash"])));
        // Qualified denies do not fully deny the base tool.
        assert!(!all_write_tools_denied(&s(&[
            "Bash(rm:*)",
            "Write",
            "Edit",
            "MultiEdit",
            "NotebookEdit"
        ])));
    }
}
