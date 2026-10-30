//! CLI PreToolUse gate. Only exit 2 blocks; every strict-mode failure uses it.

use std::cell::Cell;
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use duduclaw_core::egress_guard::{Mode, RESPONSE_DOMAIN, RESPONSE_SIGNATURE_HEADER};
use futures_util::FutureExt;
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_BYTES: usize = 256 * 1024;
const HOOK_ROUTE: &str = "/api/internal/egress-guard/hook-check";
const UNAVAILABLE: &str = "外送防護無法確認此回合狀態，已暫停這次工具呼叫；請稍後重試，若持續失敗請管理者檢查 gateway 與 hook 設定。";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    allow: bool,
    message: String,
}

fn finish(mode: Mode, outcome: Result<Decision, String>) -> (i32, String, bool) {
    match outcome {
        Ok(decision) => {
            let blocked = !decision.allow && matches!(mode, Mode::Ask | Mode::Block);
            (if blocked { 2 } else { 0 }, if blocked { decision.message } else { String::new() }, false)
        }
        Err(_) => (
            if matches!(mode, Mode::Ask | Mode::Block) { 2 } else { 0 },
            if matches!(mode, Mode::Ask | Mode::Block) { UNAVAILABLE.into() } else { String::new() },
            true,
        ),
    }
}

async fn caught<F>(future: F) -> Result<Decision, String>
where F: std::future::Future<Output = Result<Decision, String>> {
    AssertUnwindSafe(future).catch_unwind().await.unwrap_or_else(|_| Err("hook panicked".into()))
}

pub(crate) async fn run(home: &Path, agent: &str) -> i32 {
    // Default is strict even when policy lookup itself panics or fails.
    let mode = Cell::new(Mode::Block);
    let result = caught(async {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(agent) {
            return Err("invalid hook identity".into());
        }
        std::fs::read_to_string(home.join("config.toml")).map_err(|_| "policy unavailable")?;
        let dir = duduclaw_gateway::ephemeral::resolve_agent_dir(home, agent)
            .unwrap_or_else(|| home.join("agents").join(agent));
        mode.set(duduclaw_core::egress_guard::effective_mode_checked(home, Some(&dir))?);
        if mode.get() == Mode::Off { return Ok(Decision { allow: true, message: String::new() }); }
        let mut input = Vec::new();
        std::io::stdin().take(MAX_BYTES as u64 + 1).read_to_end(&mut input).map_err(|_| "stdin unavailable")?;
        let Some((tool, args)) = classify(&input)? else {
            return Ok(Decision { allow: true, message: String::new() });
        };
        let turn = std::env::var(duduclaw_core::ENV_TRUST_TURN_ID).map_err(|_| "turn unavailable")?;
        exchange(home, agent, &turn, &tool, args).await
    }).await;
    let (code, message, failed) = finish(mode.get(), result);
    // Logging is best effort, and must never turn a block into exit 101.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if failed && mode.get() == Mode::Shadow {
            duduclaw_security::audit::append_tool_call_with_extras(
                home, agent, "cli_hook", "外送防護帳本無法確認；shadow 模式繼續執行", true,
                &[("egress_guard", json!("would_refuse_ledger_unavailable"))],
            );
        }
        if !message.is_empty() { eprintln!("{message}"); }
    }));
    code
}

fn classify(input: &[u8]) -> Result<Option<(String, Value)>, String> {
    if input.len() > MAX_BYTES { return Err("hook input too large".into()); }
    let envelope: Value = serde_json::from_slice(input).map_err(|_| "invalid hook JSON")?;
    for key in ["toolInputTruncated", "tool_input_truncated"] {
        if envelope.get(key).is_some_and(|value| value.as_bool() != Some(false)) {
            return Err("hook arguments were truncated".into());
        }
    }
    let name = envelope.get("tool_name").and_then(Value::as_str).ok_or("missing tool name")?;
    let tool = match name {
        "WebFetch" | "Fetch" | "web_fetch" => "WebFetch",
        "WebSearch" | "web_search" => "WebSearch",
        "Bash" | "Shell" | "run_terminal_command" => "Bash",
        _ => return Err("unexpected hook tool".into()),
    };
    let args = envelope.get("tool_input").filter(|v| v.is_object()).ok_or("missing tool input")?;
    let sink = match tool {
        "WebFetch" | "WebSearch" => true,
        "Bash" => {
            let command = args.get("command").and_then(Value::as_str).ok_or("missing Bash command")?;
            duduclaw_core::org_field_guard::bash_egress_command(command)
        }
        _ => return Err("unexpected hook tool".into()),
    };
    Ok(sink.then(|| (tool.to_string(), args.clone())))
}

fn port_from_config(home: &Path) -> Result<u16, String> {
    let text = std::fs::read_to_string(home.join("config.toml")).map_err(|_| "gateway config unavailable")?;
    let table = text.parse::<toml::Table>().map_err(|_| "invalid gateway config")?;
    let Some(gateway) = table.get("gateway") else { return Ok(duduclaw_core::DEFAULT_GATEWAY_PORT); };
    let gateway = gateway.as_table().ok_or("invalid gateway settings")?;
    let Some(port) = gateway.get("port") else { return Ok(duduclaw_core::DEFAULT_GATEWAY_PORT); };
    port.as_integer().and_then(|n| u16::try_from(n).ok()).filter(|n| *n != 0)
        .ok_or_else(|| "invalid gateway port".into())
}

async fn exchange(home: &Path, agent: &str, turn: &str, tool: &str, args: Value) -> Result<Decision, String> {
    let port = port_from_config(home)?;
    let key = duduclaw_gateway::mcp_internal_key::valid_internal_keys(home).into_iter().next()
        .ok_or("gateway key unavailable")?;
    let identity = duduclaw_core::load_identity_key(home).ok_or("identity key unavailable")?;
    let token = duduclaw_core::mint_identity_token(&identity, agent);
    let context = std::env::var("DUDUCLAW_PRINCIPAL_CONTEXT").ok();
    let body = serde_json::to_vec(&json!({"turn_id": turn, "tool": tool, "args": args, "principal_context": context})).map_err(|_| "bad hook request")?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "clock unavailable")?.as_secs().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let signature = duduclaw_core::internal_request_signature(key.as_bytes(), agent, &token, &timestamp, &nonce, &body);
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3)).build().map_err(|_| "hook client unavailable")?;
    let mut response = client.post(format!("http://127.0.0.1:{port}{HOOK_ROUTE}"))
        .header("Content-Type", "application/json").header("X-Duduclaw-Agent-Id", agent)
        .header("X-Duduclaw-Timestamp", timestamp).header("X-Duduclaw-Nonce", &nonce)
        .header("X-Duduclaw-Signature", signature).body(body).send().await.map_err(|_| "gateway unreachable")?;
    if !response.status().is_success() { return Err("gateway refused hook request".into()); }
    let signature = response.headers().get(RESPONSE_SIGNATURE_HEADER).and_then(|v| v.to_str().ok())
        .ok_or("unsigned hook response")?.to_string();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "hook response unreadable")? {
        if bytes.len() + chunk.len() > MAX_BYTES { return Err("hook response too large".into()); }
        bytes.extend_from_slice(&chunk);
    }
    interpret(&key, agent, &token, &nonce, &signature, &bytes)
}

fn interpret(key: &str, agent: &str, token: &str, nonce: &str, signature: &str, bytes: &[u8]) -> Result<Decision, String> {
    if !duduclaw_core::verify_internal_request_signature(key.as_bytes(), agent, token, RESPONSE_DOMAIN, nonce, bytes, signature) {
        return Err("unverifiable hook response".into());
    }
    let decision: Decision = serde_json::from_slice(bytes).map_err(|_| "invalid hook response")?;
    if !decision.allow && decision.message.trim().is_empty() { return Err("missing refusal explanation".into()); }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_non_network_bash_returns_before_gateway_work() {
        for command in ["pwd", "ls -la", "cargo test", "git status", "cat README.md"] {
            assert!(classify(&serde_json::to_vec(&json!({"tool_name":"Bash", "tool_input":{"command":command}})).unwrap()).unwrap().is_none());
        }
        for tool in ["WebFetch", "WebSearch"] {
            assert!(classify(&serde_json::to_vec(&json!({"tool_name":tool, "tool_input":{}})).unwrap()).unwrap().is_some());
        }
        assert!(classify(b"broken").is_err());
        assert!(classify(br#"{"tool_name":"Bash","tool_input":{}}"#).is_err());
    }

    #[test]
    fn hook_grok_native_names_use_canonical_tool_and_raw_arguments() {
        for (name, canonical, args) in [
            ("web_fetch", "WebFetch", json!({"url":"https://example.test/?q=private"})),
            ("Fetch", "WebFetch", json!({"url":"https://example.test/"})),
            ("web_search", "WebSearch", json!({"query":"private query"})),
            ("run_terminal_command", "Bash", json!({"command":"curl https://example.test"})),
            ("Shell", "Bash", json!({"command":"curl https://example.test"})),
        ] {
            let input = serde_json::to_vec(&json!({"tool_name":name,"tool_input":args})).unwrap();
            assert_eq!(classify(&input).unwrap(), Some((canonical.to_string(), args)));
        }
    }

    #[test]
    fn hook_truncated_grok_arguments_cannot_receive_approval() {
        for flag in [json!(true), json!("false"), Value::Null] {
            let input = serde_json::to_vec(&json!({"tool_name":"web_fetch", "tool_input":{"url":"https://example.test/"}, "toolInputTruncated":flag})).unwrap();
            assert!(classify(&input).is_err());
        }
        let input = serde_json::to_vec(&json!({"tool_name":"web_fetch", "tool_input":{"url":"https://example.test/"}, "toolInputTruncated":false})).unwrap();
        assert!(classify(&input).unwrap().is_some());
    }

    #[tokio::test]
    async fn hook_panics_and_errors_exit_two_only_in_strict_modes() {
        for mode in [Mode::Off, Mode::Shadow, Mode::Ask, Mode::Block] {
            let panic = caught(async { panic!("simulated hook panic"); }).await;
            let expected = if matches!(mode, Mode::Ask | Mode::Block) { 2 } else { 0 };
            assert_eq!(finish(mode, panic).0, expected);
            assert_eq!(finish(mode, Err("bad JSON".into())).0, expected);
            assert_eq!(finish(mode, Ok(Decision { allow: false, message: "請等待核准後重試".into() })).0, expected);
        }
    }

    #[test]
    fn hook_config_port_errors_do_not_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        assert!(port_from_config(temp.path()).is_err());
        for content in ["[", "gateway=3", "[gateway]\nport=0", "[gateway]\nport='18889'", "[gateway]\nport=65536"] {
            std::fs::write(temp.path().join("config.toml"), content).unwrap();
            assert!(port_from_config(temp.path()).is_err(), "{content}");
        }
        std::fs::write(temp.path().join("config.toml"), "[gateway]\nport=19999").unwrap();
        assert_eq!(port_from_config(temp.path()).unwrap(), 19999);
    }

    #[test]
    fn hook_response_must_be_signed_and_well_formed() {
        let bytes = br#"{"allow":true,"message":""}"#;
        let sig = duduclaw_core::internal_request_signature(b"key", "bot", "token", RESPONSE_DOMAIN, "nonce", bytes);
        assert!(interpret("key", "bot", "token", "nonce", &sig, bytes).unwrap().allow);
        assert!(interpret("key", "bot", "token", "wrong", &sig, bytes).is_err());
        let bad = br#"{"allow":false,"message":""}"#;
        let sig = duduclaw_core::internal_request_signature(b"key", "bot", "token", RESPONSE_DOMAIN, "nonce", bad);
        assert!(interpret("key", "bot", "token", "nonce", &sig, bad).is_err());
    }

    #[test]
    fn hook_turn_identity_reaches_child_by_environment_inheritance() {
        const CHILD: &str = "DUDUCLAW_EGRESS_HOOK_ENV_TEST";
        if std::env::var_os(CHILD).is_some() {
            #[cfg(unix)]
            {
                let output = std::process::Command::new("/bin/sh").args(["-c", "printf '%s' \"$DUDUCLAW_TURN_ID\""]).output().unwrap();
                assert!(output.status.success());
                assert_eq!(String::from_utf8(output.stdout).unwrap(), "hook-inherited-turn");
            }
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "egress_hook::tests::hook_turn_identity_reaches_child_by_environment_inheritance"])
            .env(CHILD, "1").env(duduclaw_core::ENV_TRUST_TURN_ID, "hook-inherited-turn")
            .status().unwrap();
        assert!(status.success());
    }
}
