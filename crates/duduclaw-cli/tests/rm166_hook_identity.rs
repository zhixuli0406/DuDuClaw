//! RM-166: the `agent-file-guard` hook inherits the Claude CLI's env. These
//! tests run the real hook binary with exactly the env the gateway builds for
//! an employee's Claude spawn (spawn-env allowlist, then
//! `agent_hook_installer::apply_hook_identity_env`), in strict mode. Since
//! round 2 that env carries `DUDUCLAW_HOOK_TOKEN`, not `DUDUCLAW_AGENT_TOKEN`.
use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;

const CLI: &str = env!("CARGO_BIN_EXE_duduclaw");

fn strict_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\nrequire_identity_token = true\n",
    )
    .unwrap();
    duduclaw_core::ensure_identity_key(home.path()).unwrap();
    for id in ["alice", "bob"] {
        let dir = home.path().join("agents").join(id);
        std::fs::create_dir_all(dir.join("wiki")).unwrap();
        std::fs::create_dir_all(dir.join("deliverables")).unwrap();
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = \"{id}\"\n")).unwrap();
        std::fs::write(dir.join("MEMORY.md"), "# memory\n").unwrap();
    }
    home
}

struct HookRun {
    code: Option<i32>,
    stderr: String,
}

/// Run `duduclaw hook agent-file-guard --agent <claimed> --home <home>` (the
/// installed command) with the env of a Claude spawn for `identity_dir`.
async fn run_hook(home: &Path, claimed: &str, identity_dir: Option<&Path>, envelope: serde_json::Value) -> HookRun {
    let mut cmd = tokio::process::Command::new(CLI);
    // Same order as the production spawn sites: allowlist, then identity.
    let _granted = duduclaw_core::apply_agent_cli_env_allowlist_for(&mut cmd, None);
    duduclaw_gateway::agent_hook_installer::apply_hook_identity_env(&mut cmd, identity_dir);
    cmd.args(["hook", "agent-file-guard", "--agent", claimed, "--home"])
        .arg(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn duduclaw hook");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(envelope.to_string().as_bytes()).await.unwrap();
    drop(stdin);
    let out = child.wait_with_output().await.unwrap();
    HookRun { code: out.status.code(), stderr: String::from_utf8_lossy(&out.stderr).into_owned() }
}

fn write(path: &Path, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Write",
        "cwd": cwd.to_string_lossy(),
        "tool_input": {"file_path": path.to_string_lossy(), "content": "note\n"},
    })
}

fn bash(command: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": {"command": command},
    })
}

#[tokio::test]
async fn rm166_hook_gets_identity_from_cli_env() {
    let home = strict_home();
    let h = home.path();
    let alice = h.join("agents/alice");

    // The env the builder hands alice's spawn is exactly her verified pair.
    let env = duduclaw_gateway::agent_hook_installer::hook_identity_env(&alice);
    let id = env.iter().find(|(k, _)| k == duduclaw_core::ENV_AGENT_ID).map(|(_, v)| v.as_str());
    assert_eq!(id, Some("alice"));
    // RM-166 round 2: the hook-only token, never the full identity token.
    assert!(env.iter().all(|(k, _)| k != duduclaw_core::ENV_AGENT_TOKEN), "{env:?}");
    let token = env.iter().find(|(k, _)| k == duduclaw_core::ENV_HOOK_TOKEN).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(
        duduclaw_core::verify_hook_claim(h, "alice", &token, true),
        duduclaw_core::IdentityVerdict::Verified,
        "strict mode must resolve the hook caller as HookCaller::Agent"
    );
    assert_eq!(
        duduclaw_core::verify_identity_claim(h, "alice", &token, true),
        duduclaw_core::IdentityVerdict::Rejected,
        "the hook token is not an MCP identity"
    );

    // Own folder: memory, wiki page, deliverable (native Write and Bash).
    for target in ["MEMORY.md", "wiki/topic.md", "deliverables/report.md"] {
        let run = run_hook(h, "alice", Some(&alice), write(&alice.join(target), &alice)).await;
        assert_eq!(run.code, Some(0), "{target}: {}", run.stderr);
    }
    let cmd = format!("echo done >> '{}'", alice.join("deliverables/report.md").display());
    let run = run_hook(h, "alice", Some(&alice), bash(&cmd, &alice)).await;
    assert_eq!(run.code, Some(0), "own-folder Bash write: {}", run.stderr);

    // Another employee's folder is still refused, as a foreign write — not as
    // an identity failure.
    let run = run_hook(h, "alice", Some(&alice), write(&h.join("agents/bob/MEMORY.md"), &alice)).await;
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(!run.stderr.contains("身分驗證未通過"), "{}", run.stderr);
    // No identity refusal was recorded for any of the above.
    assert_eq!(
        duduclaw_security::audit::count_event_type_since(
            h,
            duduclaw_security::audit::EVENT_HOOK_UNTRUSTED_CALLER,
            chrono::Utc::now() - chrono::Duration::hours(1),
        ),
        0
    );
}

#[tokio::test]
async fn rm166_token_for_a_different_agent_is_refused() {
    let home = strict_home();
    let h = home.path();
    let alice = h.join("agents/alice");
    let bob = h.join("agents/bob");

    // Alice's hook (`--agent alice`) running with Bob's identity env.
    let run = run_hook(h, "alice", Some(&bob), write(&alice.join("MEMORY.md"), &alice)).await;
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("身分驗證未通過"), "{}", run.stderr);
    // The refusal is counted for the dashboard notice.
    assert_eq!(
        duduclaw_security::audit::count_event_type_since(
            h,
            duduclaw_security::audit::EVENT_HOOK_UNTRUSTED_CALLER,
            chrono::Utc::now() - chrono::Duration::hours(1),
        ),
        1
    );
}

#[tokio::test]
async fn rm166_spawn_without_identity_env_is_still_refused_in_strict_mode() {
    // Regression pin for the reported bug: no identity env (the pre-fix
    // spawn) ⇒ strict mode refuses the employee's own folder.
    let home = strict_home();
    let h = home.path();
    let alice = h.join("agents/alice");
    let run = run_hook(h, "alice", None, write(&alice.join("MEMORY.md"), &alice)).await;
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("身分驗證未通過"), "{}", run.stderr);
}
