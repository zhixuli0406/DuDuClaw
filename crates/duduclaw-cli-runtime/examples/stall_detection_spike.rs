//! Live verification for the PTY interactive stall-detection change
//! (2026-07-21). Drives a real `claude` interactive session and checks:
//!
//!   A. a SHORT task returns quickly under (idle=120s, hard=1800s);
//!   B. a LONG-but-working task (continuous streaming for longer than the idle
//!      window) survives — proving progress detection prevents the false-kill
//!      that the old fixed 180 s deadline caused. We deliberately set a SMALL
//!      idle window (idle=20s) and a long hard cap so a task that streams for
//!      40-70 s only completes if per-token progress keeps resetting the idle
//!      timer.
//!
//! Run with:
//!   cargo run -p duduclaw-cli-runtime --example stall_detection_spike
//!
//! Uses the local keychain OAuth account (already logged in). A couple of real
//! API calls; keep the model cheap.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use duduclaw_cli_runtime::session::{InvokeTimeout, PtySession, SpawnOpts};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let claude_path = which_claude()
        .ok_or_else(|| Box::<dyn std::error::Error>::from("claude CLI not found in PATH"))?;
    println!("[stall-spike] using claude at {claude_path}");

    let mut all_ok = true;

    // Fresh session per test to isolate timeout behaviour from the interactive
    // REPL's known cross-turn sentinel-residual fragility (which the production
    // fresh-spawn fallback handles separately).

    // ── Test A: short task, generous timeouts ────────────────────────────
    {
        let session = spawn(&claude_path).await?;
        let ta = Instant::now();
        let timeout_a =
            InvokeTimeout::with_idle(Duration::from_secs(1800), Duration::from_secs(120));
        println!("[stall-spike] A: short task (idle=120s, hard=1800s)…");
        match session
            .invoke_with("What is 2+2? Reply with just the number.", timeout_a)
            .await
        {
            Ok(ans) => println!(
                "[stall-spike] A ✅ {:.1}s → {:?}",
                ta.elapsed().as_secs_f64(),
                ans.trim()
            ),
            Err(e) => {
                all_ok = false;
                eprintln!("[stall-spike] A ❌ {:.1}s → {e}", ta.elapsed().as_secs_f64());
            }
        }
        session.shutdown().await;
    }

    // ── Test B: long-but-working task, SMALL idle window ─────────────────
    // idle=15s is much smaller than the task's streaming duration. The task
    // survives ONLY because per-token progress keeps resetting the idle timer —
    // proving stall detection does not false-kill a long-but-working task the
    // way the old fixed 180 s deadline would have for anything slower.
    {
        let session = spawn(&claude_path).await?;
        let tb = Instant::now();
        let timeout_b =
            InvokeTimeout::with_idle(Duration::from_secs(1800), Duration::from_secs(10));
        println!("[stall-spike] B: long streaming task (idle=10s, hard=1800s)…");
        let long_prompt = "Write a thorough, detailed essay of about 1500 words on the \
            complete history of lighthouses — ancient origins, the Pharos of Alexandria, \
            medieval developments, Fresnel lenses, automation, and their modern role. \
            Write the FULL essay now in flowing prose, many paragraphs.";
        match session.invoke_with(long_prompt, timeout_b).await {
            Ok(ans) => {
                let secs = tb.elapsed().as_secs_f64();
                println!(
                    "[stall-spike] B ✅ {:.1}s, {} chars (idle window was 10s: {})",
                    secs,
                    ans.chars().count(),
                    if secs > 10.0 {
                        "SURVIVED >10s — continuous progress kept it alive"
                    } else {
                        "finished <10s (inconclusive for the >idle-window claim)"
                    }
                );
            }
            Err(e) => {
                all_ok = false;
                eprintln!("[stall-spike] B ❌ {:.1}s → {e}", tb.elapsed().as_secs_f64());
            }
        }
        session.shutdown().await;
    }

    if !all_ok {
        return Err("one or more tests failed".into());
    }
    Ok(())
}

async fn spawn(claude_path: &str) -> Result<std::sync::Arc<PtySession>, Box<dyn std::error::Error>> {
    let mut opts = SpawnOpts::claude_interactive("stall-spike", claude_path);
    opts.extra_args = vec!["--model".to_string(), "claude-haiku-4-5".to_string()];
    opts.cwd = Some(PathBuf::from("/tmp"));
    opts.boot_timeout = Duration::from_secs(60);
    let t0 = Instant::now();
    let session = PtySession::spawn(opts).await.map_err(|e| format!("spawn: {e}"))?;
    println!("[stall-spike] boot ok in {:.1}s", t0.elapsed().as_secs_f64());
    Ok(session)
}

fn which_claude() -> Option<String> {
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".local/bin/claude");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    for c in ["/opt/homebrew/bin/claude", "/usr/local/bin/claude"] {
        if std::path::Path::new(c).exists() {
            return Some(c.to_string());
        }
    }
    let output = std::process::Command::new("which").arg("claude").output().ok()?;
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}
