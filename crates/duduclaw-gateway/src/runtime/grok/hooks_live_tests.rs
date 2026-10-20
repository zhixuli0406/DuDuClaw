//! Live: which hooks the real Grok loads for an employee's spawn.
//!
//! `grok -p` needs a signed-in Grok, so the runtime is pointed at a stand-in
//! script that runs the real `grok inspect --json` with the spawn's own
//! working directory and environment. Everything lives under a temporary
//! HOME (never the developer's `~/.claude` or `~/.grok`). Run with:
//!   cargo test -p duduclaw-gateway --lib live_grok_inspect -- --ignored --nocapture

use std::path::Path;

use super::GrokRuntime;
use crate::runtime::{AgentRuntime, RuntimeContext};

#[tokio::test]
#[ignore = "needs the grok CLI"]
async fn live_grok_inspect_runs_only_the_duduclaw_guards() {
    let real = duduclaw_core::which_grok().expect("grok CLI not found");
    let tmp = tempfile::tempdir().unwrap();
    let user_home = tmp.path();
    let home = user_home.join(".duduclaw");
    let agent_dir = home.join("agents").join("trader");
    std::fs::create_dir_all(user_home.join(".claude")).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    // The OS user's own Claude hook: must not run in an employee's turn.
    std::fs::write(
        user_home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo OPERATOR_HOOK"}]}]}}"#,
    )
    .unwrap();
    // The employee's `.claude/settings.json`, as installed at boot.
    crate::agent_hook_installer::ensure_agent_hook_settings(
        &agent_dir,
        Path::new("/usr/local/bin/duduclaw"),
    )
    .await
    .unwrap();

    let out = tmp.path().join("inspect.json");
    let script = tmp.path().join("grok-stand-in.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ -s '{out}' ] || '{real}' inspect --json > '{out}'\n",
            out = out.display(),
            real = real,
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let rt = GrokRuntime {
        grok_path: script.to_string_lossy().to_string(),
    };
    let ctx = RuntimeContext {
        agent_dir: Some(agent_dir.clone()),
        system_prompt: String::new(),
        model: String::new(),
        max_tokens: 64,
        home_dir: home.clone(),
        agent_id: "trader".into(),
        preferred_provider: None,
        conversation_history: Vec::new(),
        capabilities: None,
        account_pool: Vec::new(),
        effort: None,
        allow_cross_family_failover: false,
    };
    // The stand-in prints nothing, so the turn itself errors; only the
    // discovery it recorded matters here.
    let _ = rt.execute("hi", &ctx).await;

    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    println!("{}", serde_json::to_string_pretty(&v["hooks"]).unwrap());
    let hooks = v["hooks"].as_array().expect("hooks array");
    let active: Vec<_> = hooks.iter().filter(|h| h["disabled"] != true).collect();
    assert_eq!(active.len(), 2, "{hooks:#?}");
    for h in &active {
        let target = h["target"].as_str().unwrap();
        assert!(
            target.contains(" hook agent-file-guard ") || target.ends_with(" hook data-file-guard"),
            "{target}"
        );
        let source = h["source"]["path"].as_str().unwrap();
        assert!(source.ends_with(".grok/hooks"), "{source}");
    }
    assert!(
        hooks
            .iter()
            .any(|h| h["target"] == "echo OPERATOR_HOOK" && h["disabled"] == true),
        "{hooks:#?}"
    );
}
