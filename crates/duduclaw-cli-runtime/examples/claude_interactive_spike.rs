//! Phase 3.C.2 end-to-end probe — drives a real `claude` interactive session
//! through the `PtySession` infrastructure (boot dance + sentinel protocol +
//! ANSI strip + chrome filter) and asserts that 1-3 turns return clean payload.
//!
//! Run with:
//!   cargo run -p duduclaw-cli-runtime --example claude_interactive_spike
//!
//! Set `DUDUCLAW_SPIKE_PRE_TRUSTED=1` if the cwd has already been accepted via
//! `claude project trust` (skips the trust-dialog handling in the boot dance).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use duduclaw_cli_runtime::session::{PtySession, SpawnOpts};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pipe tracing output to stderr so we see the diagnostic dump on invoke
    // timeout. Run with `RUST_LOG=warn` (or `info`/`debug`) for more detail.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let claude_path =
        which_claude().ok_or_else(|| Box::<dyn std::error::Error>::from("claude CLI not found in PATH"))?;
    println!("[spike] using claude at {claude_path}");

    let pre_trusted = std::env::var("DUDUCLAW_SPIKE_PRE_TRUSTED")
        .map(|v| v == "1")
        .unwrap_or(false);

    let mut opts = SpawnOpts::claude_interactive("spike-agent", &claude_path);
    // Add a small extra-args slice — we go through PtySession's
    // inject_protocol_args internally so we don't have to specify the
    // `--append-system-prompt` here. Just pick a fast model + dump.
    opts.extra_args = vec![
        "--model".to_string(),
        "claude-haiku-4-5".to_string(),
    ];
    opts.pre_trusted = pre_trusted;
    opts.cwd = Some(PathBuf::from("/tmp"));
    opts.boot_timeout = Duration::from_secs(60);
    opts.default_invoke_timeout = Duration::from_secs(120);

    let spawn_started = Instant::now();
    println!("[spike] PtySession::spawn (interactive=true, pre_trusted={pre_trusted})...");
    let session = match PtySession::spawn(opts).await {
        Ok(s) => {
            println!(
                "[spike] ✅ spawn complete in {:.1}s (pid={:?})",
                spawn_started.elapsed().as_secs_f64(),
                s.pid(),
            );
            s
        }
        Err(e) => {
            eprintln!("[spike] ❌ spawn failed: {e}");
            return Err(format!("spawn: {e}").into());
        }
    };

    let prompts = [
        "Say hi in one short sentence.",
        "What is 7 times 6? Reply with just the number.",
        "Reply with exactly the literal text: PROBE_TURN_3",
    ];

    let mut all_ok = true;
    for (i, prompt) in prompts.iter().enumerate() {
        let turn_started = Instant::now();
        println!("[spike] === turn {} ===", i + 1);
        println!("[spike] prompt: {prompt:?}");
        match session.invoke(prompt, Some(Duration::from_secs(90))).await {
            Ok(answer) => {
                println!(
                    "[spike] ✅ turn {} ok in {:.1}s, answer ({} chars):\n----\n{}\n----",
                    i + 1,
                    turn_started.elapsed().as_secs_f64(),
                    answer.chars().count(),
                    answer
                );
            }
            Err(e) => {
                all_ok = false;
                eprintln!(
                    "[spike] ❌ turn {} failed after {:.1}s: {e}",
                    i + 1,
                    turn_started.elapsed().as_secs_f64()
                );
            }
        }
    }

    println!("[spike] alive after 3 turns? {}", session.is_healthy());
    println!("[spike] shutting down");
    session.shutdown().await;

    if !all_ok {
        return Err("one or more turns failed".into());
    }
    Ok(())
}

fn which_claude() -> Option<String> {
    // **Review fix (LOW)**: dropped the hardcoded personal path
    // `/Users/lizhixu/.local/bin/claude`. Operators get `~/.local/bin`
    // via the dirs crate fallback + `which` lookup.
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
    if path.is_empty() { None } else { Some(path) }
}
