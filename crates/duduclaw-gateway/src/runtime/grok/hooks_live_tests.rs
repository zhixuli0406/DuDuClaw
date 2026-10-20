//! Live: what the real Grok loads for an employee's spawn (hooks, and the
//! OS user's Claude/Cursor instructions, rules and skills).
//!
//! `grok -p` needs a signed-in Grok, so the runtime is pointed at a stand-in
//! script that runs the real `grok inspect --json` with the spawn's own
//! working directory and environment. Everything lives under a temporary
//! HOME (never the developer's `~/.claude` or `~/.grok`). Run with:
//!   cargo test -p duduclaw-gateway --lib live_grok_inspect -- --ignored --nocapture

use std::path::Path;

use super::GrokRuntime;
use crate::runtime::{AgentRuntime, RuntimeContext};

/// Runs one `GrokRuntime` turn for the employee `trader` under `user_home`
/// and returns what `grok inspect --json` reported from inside that spawn.
async fn inspect_from_spawn(user_home: &Path) -> serde_json::Value {
    let real = duduclaw_core::which_grok().expect("grok CLI not found");
    let home = user_home.join(".duduclaw");
    let agent_dir = home.join("agents").join("trader");
    std::fs::create_dir_all(&agent_dir).unwrap();
    // The employee's `.claude/settings.json`, as installed at boot.
    crate::agent_hook_installer::ensure_agent_hook_settings(
        &agent_dir,
        Path::new("/usr/local/bin/duduclaw"),
    )
    .await
    .unwrap();

    let out = user_home.join("inspect.json");
    let script = user_home.join("grok-stand-in.sh");
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

    serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap()
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[tokio::test]
#[ignore = "needs the grok CLI"]
async fn live_grok_inspect_runs_only_the_duduclaw_guards() {
    let tmp = tempfile::tempdir().unwrap();
    // The OS user's own Claude hook: must not run in an employee's turn.
    write(
        &tmp.path().join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo OPERATOR_HOOK"}]}]}}"#,
    );

    let v = inspect_from_spawn(tmp.path()).await;
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

#[tokio::test]
#[ignore = "needs the grok CLI"]
async fn live_grok_inspect_leaves_out_the_operators_claude_context() {
    let tmp = tempfile::tempdir().unwrap();
    let user_home = tmp.path();
    let skill = |name: &str| format!("---\nname: {name}\ndescription: {name}\n---\n{name}\n");
    write(&user_home.join(".claude/CLAUDE.md"), "OPERATOR_CLAUDE_MD\n");
    write(&user_home.join(".claude/rules/house.md"), "OPERATOR_RULE\n");
    write(
        &user_home.join(".claude/skills/op-skill/SKILL.md"),
        &skill("op-skill"),
    );
    // Claude-format rules inside the employee directory are compat too.
    write(
        &user_home.join(".duduclaw/agents/trader/.claude/rules/extra.md"),
        "PROJECT_CLAUDE_RULE\n",
    );
    write(
        &user_home.join(".cursor/skills/cursor-skill/SKILL.md"),
        &skill("cursor-skill"),
    );
    // The employee's own instructions file must keep loading.
    write(
        &user_home.join(".duduclaw/agents/trader/AGENTS.md"),
        "EMPLOYEE_AGENTS_MD\n",
    );

    let v = inspect_from_spawn(user_home).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&v["projectInstructions"]).unwrap()
    );
    println!("{}", serde_json::to_string_pretty(&v["skills"]).unwrap());

    let enabled =
        |e: &serde_json::Value| e["disabled"] != true && e["compatibilityStatus"] != "disabled";
    let instructions = v["projectInstructions"]
        .as_array()
        .expect("instructions array");
    for e in instructions.iter().filter(|e| enabled(e)) {
        let path = e["path"].as_str().unwrap();
        assert!(
            !path.contains("/.claude/") && !path.contains("/.cursor/"),
            "operator file still loaded: {path}"
        );
    }
    assert!(
        instructions.iter().any(|e| enabled(e)
            && e["path"]
                .as_str()
                .is_some_and(|p| p.to_ascii_lowercase().ends_with("agents/trader/agents.md"))),
        "{instructions:#?}"
    );
    // The three Claude-format files are listed, and listed as disabled.
    let disabled_claude = instructions
        .iter()
        .filter(|e| !enabled(e) && e["path"].as_str().is_some_and(|p| p.contains("/.claude/")))
        .count();
    assert_eq!(disabled_claude, 3, "{instructions:#?}");
    let skills = v["skills"].as_array().expect("skills array");
    for name in ["op-skill", "cursor-skill"] {
        let s = skills.iter().find(|s| s["name"] == name).expect(name);
        assert!(!enabled(s), "{s:#}");
    }
}
