//! L1 (zero quota): a stub `claude` CLI that records its argv and prints a
//! partial-message stream-json fixture, driven through the real channel
//! spawn path with and without a thinking sink in scope.

use super::*;
use crate::thinking::policy::{SinkLevel, ThinkingPolicy};
use std::sync::{Arc, Mutex};

const CHILD_HOME: &str = "DUDU_TEST_THINKING_CLI_HOME";
const SECRET: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";

fn fixture() -> String {
    crate::thinking::claude_stream::tests::PARTIAL_FIXTURE
        .iter()
        .map(|l| format!("{l}\n"))
        .collect()
}

fn sink_at(
    level: SinkLevel,
    home: &Path,
) -> (
    Arc<crate::thinking::ThinkingSink>,
    Arc<Mutex<Vec<crate::thinking::ThinkingEvent>>>,
) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    let cb: ProgressCallback = Box::new(move |ev| {
        if let ProgressEvent::Thinking(t) = ev {
            g.lock().unwrap().push(t);
        }
    });
    let policy = ThinkingPolicy {
        level,
        max_chars: crate::thinking::policy::DEFAULT_MAX_CHARS,
        ..ThinkingPolicy::INACTIVE
    };
    let ctx = crate::thinking::SinkCtx {
        home: home.to_path_buf(),
        agent_id: "agnes".into(),
        redaction: None,
    };
    let (_inner, sink) = crate::thinking::install(Some(policy), Some(cb), ctx);
    (sink.expect("sink"), got)
}

async fn spawn(home: &Path) -> Result<String, String> {
    spawn_claude_cli_with_env(
        "hi",
        "claude-opus-5",
        "",
        home,
        None,
        None,
        None,
        &std::collections::HashMap::new(),
        None,
        None,
    )
    .await
}

fn argv(home: &Path, n: u32) -> Vec<String> {
    std::fs::read_to_string(home.join(format!("argv-{n}")))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn child(home: std::path::PathBuf) {
    assert_eq!(
        duduclaw_core::which_claude().as_deref(),
        home.join("bin/claude").to_str()
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // 1. No sink (cron / dispatch / any non-reply spawn): argv has no
        //    partial-messages flag, the answer is the plain text.
        assert_eq!(spawn(&home).await.unwrap(), "兩份報表一致。");
        let plain = argv(&home, 1);
        assert!(!plain.iter().any(|a| a == "--include-partial-messages"), "{plain:?}");

        // 2. A signed-in WebChat viewer (summary): flag added, thinking text
        //    arrives masked, signatures never, the answer is not doubled.
        let (sink, got) = sink_at(SinkLevel::Summary, &home);
        let out = crate::thinking::scope_turn(Some(Arc::clone(&sink)), spawn(&home))
            .await
            .unwrap();
        assert_eq!(out, "兩份報表一致。");
        let live = argv(&home, 2);
        assert!(live.iter().any(|a| a == "--include-partial-messages"), "{live:?}");
        let mut without_flag = live.clone();
        without_flag.retain(|a| a != "--include-partial-messages");
        assert_eq!(without_flag, plain, "the flag is the only difference");
        let events = got.lock().unwrap().clone();
        let texts: Vec<String> = events.iter().filter_map(|e| e.text.clone()).collect();
        let last = texts.last().expect("thinking text shown");
        assert!(last.contains("先比對兩份報表"), "{texts:?}");
        assert!(last.contains("〔已遮蔽·EMAIL〕"), "{texts:?}");
        assert!(texts.iter().all(|t| !t.contains(SECRET)), "{texts:?}");
        assert!(texts.iter().all(|t| !t.contains("SIG-NEVER-SHOWN")));
        assert!(events.iter().any(|e| e.is_turn_usage() && e.tokens == Some(31)));
        sink.finish("agnes", "webchat:u:1").await;
        let rows = crate::run_steps::shared_store(&home)
            .unwrap()
            .recent_for_session("webchat:u:1", 10)
            .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, crate::run_steps::KIND_THINKING);
        assert!(rows[0].payload_preview.contains("31 token"), "{rows:?}");
        assert!(!rows[0].payload_preview.contains("比對"), "no thinking text stored");

        // 3. An external channel (signal): argv unchanged, no text at all.
        let (sink, got) = sink_at(SinkLevel::Signal, &home);
        crate::thinking::scope_turn(Some(sink), spawn(&home))
            .await
            .unwrap();
        assert_eq!(argv(&home, 3), plain);
        let events = got.lock().unwrap().clone();
        assert!(!events.is_empty(), "the turn still knows it thought");
        assert!(events.iter().all(|e| e.text.is_none() && e.line.is_none()));

        // DUDUCLAW_STREAM_DEBUG=1 (set for this child): thinking_delta lines
        // are logged as a marker only.
        let log = std::fs::read_to_string(home.join("claude_stream.log")).unwrap();
        assert!(log.contains(r#""elided":"thinking_delta""#));
        assert!(!log.contains(SECRET), "debug log never holds thinking text");
    });
}

#[test]
fn l1_stub_claude_thinking_flows_by_policy() {
    if let Some(home) = std::env::var_os(CHILD_HOME) {
        child(std::path::PathBuf::from(home));
        return;
    }
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let bin = home.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(home.join("stream.jsonl"), fixture()).unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '99999.0.0\\n'; exit 0; fi\n\
         n=0\nif [ -f '{h}/count' ]; then n=$(/bin/cat '{h}/count'); fi\nn=$((n+1))\n\
         printf '%s' \"$n\" > '{h}/count'\n\
         for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{h}/argv-'\"$n\"\n\
         /bin/cat '{h}/stream.jsonl'\n",
        h = home.display()
    );
    crate::test_exec::write_executable(&bin.join("claude"), script, 0o755);
    let full = concat!(module_path!(), "::l1_stub_claude_thinking_flows_by_policy");
    let name = full.split_once("::").unwrap().1;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", home)
        .env(CHILD_HOME, home)
        .env("DUDUCLAW_STREAM_DEBUG", "1")
        .args(["--exact", name, "--nocapture"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let started = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            let _ = duduclaw_core::platform::kill_process_group(child.id());
            let _ = child.wait();
            panic!("isolated thinking CLI fixture exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "child must run the fixture test: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(home.join("count")).unwrap(), "3");
}
