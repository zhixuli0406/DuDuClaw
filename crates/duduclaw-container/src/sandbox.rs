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
    let client = Docker::connect_with_local_defaults()
        .map_err(|e| format!("Docker connect failed: {e}"))?;

    let container_name = format!("duduclaw-sandbox-{}", uuid::Uuid::new_v4());

    // Build mount: agent dir as read-only
    let agent_dir_str = agent_dir.to_string_lossy().to_string();
    let binds = vec![format!("{agent_dir_str}:/agent:ro")];

    // Build command: call claude CLI with the prompt
    let cmd = vec![
        "claude".to_string(),
        "-p".to_string(),
        prompt.to_string(),
        "--model".to_string(),
        model.to_string(),
        "--output-format".to_string(),
        "text".to_string(),
        "--system-prompt".to_string(),
        system_prompt.to_string(),
    ];

    // Environment variables (only API key)
    let env = vec![format!("ANTHROPIC_API_KEY={api_key}")];

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
