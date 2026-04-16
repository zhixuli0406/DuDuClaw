//! Evolution Engine — skill vetting utility.
//!
//! Legacy three-layer reflection (micro/meso/macro via Python subprocess)
//! has been removed. All evolution is now driven by the prediction engine
//! (Phase 1) and GVU self-play loop (Phase 2) in the Rust-native pipeline.
//! See `prediction/` and `gvu/` modules.

use std::path::Path;

use serde_json::Value;
use tracing::info;

/// Vet a skill file for security issues.
pub async fn vet_skill(
    home_dir: &Path,
    skill_name: &str,
    content: &str,
    skills_dir: Option<&Path>,
    quarantine_dir: Option<&Path>,
) -> Result<Value, String> {
    let python_path = find_python_path(home_dir);

    let mut cmd = tokio::process::Command::new(duduclaw_core::platform::python3_command());
    cmd.args(["-m", "duduclaw.evolution.run", "vet", "--skill-name", skill_name]);

    if let Some(sd) = skills_dir {
        cmd.args(["--skills-dir", &sd.to_string_lossy()]);
    }
    if let Some(qd) = quarantine_dir {
        cmd.args(["--quarantine-dir", &qd.to_string_lossy()]);
    }

    // SECURITY: Clear all inherited env vars (prevents API key leakage),
    // then whitelist only what the Python subprocess needs.
    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("HOME", std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default());
    cmd.env("USERPROFILE", std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default());
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
    info!("Skill vet for '{skill_name}' completed");
    serde_json::from_str(&stdout).map_err(|e| format!("Parse: {e}"))
}

// ── Internal ────────────────────────────────────────────────

fn find_python_path(home_dir: &Path) -> String {
    let mut candidates = vec![
        // Development: project root python/
        home_dir.parent().unwrap_or(home_dir).join("python").to_string_lossy().to_string(),
    ];
    #[cfg(not(windows))]
    {
        // Homebrew / source install
        candidates.push("/opt/duduclaw".to_string());
        // Homebrew Cellar (Apple Silicon)
        candidates.push("/opt/homebrew/opt/duduclaw-pro/libexec/python".to_string());
        // Homebrew Cellar (Intel Mac)
        candidates.push("/usr/local/opt/duduclaw-pro/libexec/python".to_string());
    }
    #[cfg(windows)]
    {
        // Windows: check common install locations
        if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
            candidates.push(format!("{appdata}\\Programs\\duduclaw\\python"));
        }
    }
    // User-local fallback
    candidates.push(home_dir.join(".duduclaw").join("python").to_string_lossy().to_string());

    for path in &candidates {
        if !path.is_empty() && Path::new(path).join("duduclaw").exists() {
            return path.clone();
        }
    }
    std::env::var("PYTHONPATH").unwrap_or_default()
}
