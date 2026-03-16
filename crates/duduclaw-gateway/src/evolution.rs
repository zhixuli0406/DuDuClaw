//! Evolution Engine integration — calls Python subprocess for reflections.
//!
//! - Micro: triggered after each channel message
//! - Meso: triggered by heartbeat timer (hourly)
//! - Macro: triggered by daily timer

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Run micro reflection after a conversation.
pub async fn run_micro(
    home_dir: &Path,
    agent_id: &str,
    agent_dir: &Path,
    summary: &str,
) {
    info!("🔄 Micro reflection for {agent_id}");
    match call_evolution("micro", home_dir, agent_id, agent_dir, Some(summary)).await {
        Ok(result) => {
            info!("✅ Micro reflection done: {}", result.get("status").and_then(|v| v.as_str()).unwrap_or("?"));
        }
        Err(e) => warn!("Micro reflection failed: {e}"),
    }
}

/// Run meso reflection (heartbeat).
pub async fn run_meso(home_dir: &Path, agent_id: &str, agent_dir: &Path) {
    info!("🔄 Meso reflection for {agent_id}");
    match call_evolution("meso", home_dir, agent_id, agent_dir, None).await {
        Ok(result) => {
            let notes = result.get("notes_reviewed").and_then(|v| v.as_u64()).unwrap_or(0);
            info!("✅ Meso reflection done: reviewed {notes} notes");
        }
        Err(e) => warn!("Meso reflection failed: {e}"),
    }
}

/// Run macro reflection (daily).
pub async fn run_macro(home_dir: &Path, agent_id: &str, agent_dir: &Path) {
    info!("🔄 Macro reflection for {agent_id}");
    match call_evolution("macro", home_dir, agent_id, agent_dir, None).await {
        Ok(result) => {
            let skills = result.get("skills_reviewed").and_then(|v| v.as_u64()).unwrap_or(0);
            info!("✅ Macro reflection done: reviewed {skills} skills");
            // Log the report if present
            if let Some(report) = result.get("report").and_then(|v| v.as_str())
                && !report.is_empty()
            {
                info!("📊 Evolution report:\n{report}");
            }
        }
        Err(e) => warn!("Macro reflection failed: {e}"),
    }
}

/// Vet a skill file for security issues.
pub async fn vet_skill(
    home_dir: &Path,
    skill_name: &str,
    content: &str,
    skills_dir: Option<&Path>,
    quarantine_dir: Option<&Path>,
) -> Result<Value, String> {
    let python_path = find_python_path(home_dir);

    let mut cmd = tokio::process::Command::new("python3");
    cmd.args(["-m", "duduclaw.evolution.run", "vet", "--skill-name", skill_name]);

    if let Some(sd) = skills_dir {
        cmd.args(["--skills-dir", &sd.to_string_lossy()]);
    }
    if let Some(qd) = quarantine_dir {
        cmd.args(["--quarantine-dir", &qd.to_string_lossy()]);
    }

    cmd.env("PYTHONPATH", &python_path);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Spawn: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(content.as_bytes()).await;
        drop(stdin);
    }

    let output = child.wait_with_output().await.map_err(|e| format!("Wait: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|e| format!("Parse: {e}"))
}

/// Start the evolution heartbeat background tasks.
pub fn start_evolution_timers(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    // Meso: every hour
    let h = home_dir.clone();
    let r = registry.clone();
    handles.push(tokio::spawn(async move {
        // Wait 5 minutes before first meso reflection
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        loop {
            run_reflections_for_all_agents(&h, &r, "meso").await;
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }));

    // Macro: every 24 hours
    let h = home_dir;
    let r = registry;
    handles.push(tokio::spawn(async move {
        // Wait 1 hour before first macro reflection
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        loop {
            run_reflections_for_all_agents(&h, &r, "macro").await;
            tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
        }
    }));

    handles
}

async fn run_reflections_for_all_agents(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    reflection_type: &str,
) {
    let reg = registry.read().await;
    let agents: Vec<(String, PathBuf)> = reg.list().iter().map(|a| {
        (a.config.agent.name.clone(), a.dir.clone())
    }).collect();
    drop(reg);

    for (agent_id, agent_dir) in agents {
        match reflection_type {
            "meso" => run_meso(home_dir, &agent_id, &agent_dir).await,
            "macro" => run_macro(home_dir, &agent_id, &agent_dir).await,
            _ => {}
        }
    }
}

// ── Internal ────────────────────────────────────────────────

async fn call_evolution(
    command: &str,
    home_dir: &Path,
    agent_id: &str,
    agent_dir: &Path,
    summary: Option<&str>,
) -> Result<Value, String> {
    let python_path = find_python_path(home_dir);

    let mut cmd = tokio::process::Command::new("python3");
    cmd.args([
        "-m", "duduclaw.evolution.run",
        command,
        "--agent-id", agent_id,
        "--agent-dir", &agent_dir.to_string_lossy(),
    ]);

    if let Some(s) = summary {
        cmd.args(["--summary", s]);
    }

    cmd.env("PYTHONPATH", &python_path);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        cmd.output(),
    )
    .await
    .map_err(|_| "Evolution timeout (30s)".to_string())?
    .map_err(|e| format!("Spawn: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("exit {}: {}", output.status.code().unwrap_or(-1), &stderr[..stderr.len().min(200)]));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|e| format!("Parse: {e}, stdout: {}", &stdout[..stdout.len().min(100)]))
}

fn find_python_path(home_dir: &Path) -> String {
    let candidates = [
        home_dir.parent().unwrap_or(home_dir).join("python").to_string_lossy().to_string(),
        "/opt/duduclaw".to_string(),
    ];
    for path in &candidates {
        if !path.is_empty() && Path::new(path).join("duduclaw").exists() {
            return path.clone();
        }
    }
    std::env::var("PYTHONPATH").unwrap_or_default()
}
