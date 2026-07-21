//! Live verification for the Bug1 submit-watchdog fix (2026-07-21). Drives a
//! real `claude` interactive session with the exact production-shaped MULTI-LINE
//! prompt (`[sender_id: …]` header line + body) that stalled in production, and
//! checks the turn actually starts / answers instead of sitting in the composer.
//!
//! Run with RUST_LOG=duduclaw_cli_runtime=debug to see the watchdog resend line:
//!   RUST_LOG=duduclaw_cli_runtime=debug \
//!     cargo run -p duduclaw-cli-runtime --example bug1_submit_watchdog_spike
//!
//! Each fresh session gets ONE multi-line invoke. Repeat a few times: with the
//! fix, runs that would previously stall now log a resend and then answer.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use duduclaw_cli_runtime::session::{InvokeTimeout, PtySession, SpawnOpts};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("duduclaw_cli_runtime=debug")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let claude_path = which_claude()
        .ok_or_else(|| Box::<dyn std::error::Error>::from("claude CLI not found in PATH"))?;

    let runs: usize = std::env::var("RUNS").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
    let mut ok = 0usize;
    for i in 0..runs {
        let mut opts = SpawnOpts::claude_interactive("bug1-spike", &claude_path);
        opts.extra_args = vec!["--model".to_string(), "claude-haiku-4-5".to_string()];
        opts.cwd = Some(PathBuf::from("/tmp"));
        opts.boot_timeout = Duration::from_secs(60);
        let session = PtySession::spawn(opts).await.map_err(|e| format!("spawn: {e}"))?;

        // Exact production shape: header line + body (multi-line ⇒ triggers Bug1).
        let prompt = "[sender_id: webchat:test-user]\n用一句話回答：什麼是 AGI？";
        let t = Instant::now();
        // idle=30s: if the prompt never submits, the old code would stall at 30s;
        // the watchdog resubmits within ~1.5–4.5s so a working turn completes well
        // under that.
        let timeout = InvokeTimeout::with_idle(Duration::from_secs(120), Duration::from_secs(30));
        match session.invoke_with(prompt, timeout).await {
            Ok(ans) => {
                ok += 1;
                let head: String = ans.chars().take(50).collect();
                println!(
                    "[bug1-spike] run {} ✅ {:.1}s → {:?}",
                    i + 1,
                    t.elapsed().as_secs_f64(),
                    head
                );
            }
            Err(e) => println!(
                "[bug1-spike] run {} ❌ {:.1}s → {e}",
                i + 1,
                t.elapsed().as_secs_f64()
            ),
        }
        session.shutdown().await;
    }
    println!("[bug1-spike] {ok}/{runs} succeeded");
    Ok(())
}

fn which_claude() -> Option<String> {
    if let Some(home) = dirs::home_dir() {
        let c = home.join(".local/bin/claude");
        if c.exists() {
            return Some(c.to_string_lossy().to_string());
        }
    }
    let out = std::process::Command::new("which").arg("claude").output().ok()?;
    let p = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if p.is_empty() { None } else { Some(p) }
}
