//! MCP startup observations and bounded, gated install probes.
use std::{collections::HashMap, path::{Path, PathBuf}, sync::{LazyLock, Mutex}};
use duduclaw_core::mcp_start_status::{ReasonCode, StartStatus, safe_label};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// Home is part of the key to keep isolated gateways/tests independent.
static LAST: LazyLock<Mutex<HashMap<(PathBuf, String, String), StartStatus>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn last(home: &Path, agent: &str, server: &str) -> Option<StartStatus> {
    let current = LAST.lock().ok()?.get(&(home.into(), agent.into(), server.into())).cloned();
    // Proxy subprocesses report through the shared locked anomaly sink. Read
    // only a bounded tail; this is diagnostic state, never an authority source.
    let disk = latest_refusal(home, agent, server);
    match (current, disk) {
        (Some(a), Some(b)) => Some(if a.time >= b.time { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn latest_refusal(home: &Path, agent: &str, server: &str) -> Option<StartStatus> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(home.join("channel_failures.jsonl")).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(65536))).ok()?;
    let mut tail = String::new();
    // A truncated UTF-8 boundary is harmless: skip its partial first record.
    let mut bytes = Vec::new();
    file.take(65536).read_to_end(&mut bytes).ok()?;
    tail.push_str(&String::from_utf8_lossy(&bytes));
    tail.lines().rev().filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v["classification"] == "mcp_start" && v["agent_id"] == safe_label(agent)
            && v["server"] == safe_label(server) && v["reason_code"] != "connection_failed")
        .and_then(|v| serde_json::from_value(v).ok())
}

fn remember(home: &Path, agent: &str, server: &str, status: StartStatus) {
    if let Ok(mut map) = LAST.lock() {
        // Diagnostic state is bounded and disposable, never authorization state.
        if map.len() >= 4096 { map.clear(); }
        map.insert((home.into(), agent.into(), server.into()), status);
    }
}

pub(crate) fn observe_init(home: &Path, agent: &str, event: &Value) {
    if event["type"] != "system" || event["subtype"] != "init" { return; }
    let Some(servers) = event["mcp_servers"].as_array() else { return; };
    for server in servers {
        let Some(name) = server["name"].as_str() else { continue; };
        let status = server["status"].as_str().unwrap_or("unknown");
        let ok = status == "connected";
        let refusal = (!ok).then(|| latest_refusal(home, agent, name)).flatten()
            .filter(|s| chrono::DateTime::parse_from_rfc3339(&s.time).ok().is_some_and(|t|
                (chrono::Utc::now().signed_duration_since(t).num_seconds()).abs() <= 60));
        let result = StartStatus { ok, reason_code: if ok { None } else {
            refusal.and_then(|s| s.reason_code).or(Some(ReasonCode::ConnectionFailed))
        },
            time: chrono::Utc::now().to_rfc3339(), tool_count: None };
        remember(home, agent, name, result.clone());
        if !ok {
            let agent = safe_label(agent);
            let name = safe_label(name);
            let status = safe_label(status);
            tracing::warn!(employee = %agent, server = %name, status = %status, "MCP server did not connect");
            if let Err(e) = crate::trajectory_guard::append_anomaly(home, &json!({
                "classification":"mcp_start", "agent_id":agent, "server":name,
                "status":status, "ok":false, "reason_code":result.reason_code, "time":result.time,
            })) {
                tracing::warn!(error = %e, "MCP startup anomaly append failed");
            }
        }
    }
}

/// Use exactly the preparation/rewrite/identity attachment used by Claude.
/// The TempPath must stay alive until the probe child is reaped.
fn prepare(home: &Path, agent: &str, server: &str) -> Result<(Value, tempfile::TempPath), ReasonCode> {
    let dir = home.join("agents").join(agent);
    duduclaw_agent::mcp_template::prepare_mcp_config_for_spawn(&dir)
        .map_err(|_| ReasonCode::PolicyDenied)?;
    let guard = crate::redaction_proxy::maybe_proxy_mcp_config_in_lane(home, &dir.join(".mcp.json"), false)
        .map_err(|_| ReasonCode::PolicyDenied)?.ok_or(ReasonCode::PolicyDenied)?;
    let config: Value = serde_json::from_slice(&std::fs::read(&guard).map_err(|_| ReasonCode::PolicyDenied)?)
        .map_err(|_| ReasonCode::PolicyDenied)?;
    let entry = config["mcpServers"].get(server).cloned().ok_or(ReasonCode::PolicyDenied)?;
    Ok((entry, guard))
}

pub(crate) async fn probe(home: &Path, agent: &str, server: &str) -> StartStatus {
    let result = match prepare(home, agent, server) {
        Ok((entry, _guard)) => probe_entry(&entry, std::time::Duration::from_secs(10)).await,
        Err(reason) => Err(reason),
    };
    let status = StartStatus { ok: result.is_ok(), tool_count: result.as_ref().ok().copied(),
        reason_code: result.err(), time: chrono::Utc::now().to_rfc3339() };
    remember(home, agent, server, status.clone());
    status
}

async fn read_response<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R, id: u64) -> Result<Value, ReasonCode> {
    use tokio::io::AsyncReadExt;
    loop {
        // Bound a single hostile upstream frame before allocating it.
        let mut bytes = Vec::new();
        let n = (&mut *reader).take(1024 * 1024).read_until(b'\n', &mut bytes).await
            .map_err(|_| ReasonCode::ProtocolError)?;
        if n == 0 || bytes.last() != Some(&b'\n') { return Err(ReasonCode::ProtocolError); }
        let frame: Value = match serde_json::from_slice(&bytes) { Ok(v) => v, Err(_) => continue };
        if frame["id"] != id { continue; }
        if let Some(error) = frame.get("error") {
            let reason = serde_json::from_value(error["data"]["reason_code"].clone())
                .unwrap_or(ReasonCode::ProtocolError);
            return Err(reason);
        }
        return frame.get("result").cloned().ok_or(ReasonCode::ProtocolError);
    }
}

async fn probe_entry(entry: &Value, deadline: std::time::Duration) -> Result<usize, ReasonCode> {
    let command = entry["command"].as_str().ok_or(ReasonCode::PolicyDenied)?;
    let args: Vec<String> = serde_json::from_value(entry["args"].clone()).map_err(|_| ReasonCode::PolicyDenied)?;
    let env: HashMap<String, String> = serde_json::from_value(entry["env"].clone()).map_err(|_| ReasonCode::PolicyDenied)?;
    let mut cmd = duduclaw_core::platform::async_command_for(command);
    cmd.args(args).env_clear().envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs())
        .envs(env).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let end = tokio::time::Instant::now() + deadline;
    let mut child = tokio::time::timeout_at(end, duduclaw_core::execution_gate::with_runtime(
        duduclaw_core::execution_gate::RuntimeKind::Registered(
            duduclaw_core::execution_gate::Text::new("third-party-mcp".into()).expect("constant runtime")),
        crate::process_group::BackendChild::spawn_class(&mut cmd,
            duduclaw_core::execution_gate::ExecutionClass::ThirdPartyMcp)))
        .await.map_err(|_| ReasonCode::Timeout)?.map_err(|_| ReasonCode::UpstreamSpawnFailed)?;
    let result = tokio::time::timeout_at(end, async {
        let mut input = child.child().stdin.take().ok_or(ReasonCode::UpstreamIoUnavailable)?;
        let mut reader = BufReader::new(child.child().stdout.take().ok_or(ReasonCode::UpstreamIoUnavailable)?);
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"duduclaw-install-probe\",\"version\":\"1\"}}}\n")
            .await.map_err(|_| ReasonCode::ProtocolError)?;
        let init = read_response(&mut reader, 1).await?;
        if !init["protocolVersion"].is_string() || !init["capabilities"].is_object() {
            return Err(ReasonCode::ProtocolError);
        }
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
            .await.map_err(|_| ReasonCode::ProtocolError)?;
        let result = read_response(&mut reader, 2).await?;
        result["tools"].as_array().map(Vec::len).ok_or(ReasonCode::ProtocolError)
    }).await.unwrap_or(Err(ReasonCode::Timeout));
    child.kill_and_reap().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_observes_failed_server_without_failing_turn_or_logging_raw_error() {
        let home = tempfile::tempdir().unwrap();
        observe_init(home.path(), "employee", &json!({"type":"system", "subtype":"init",
            "mcp_servers":[{"name":"good","status":"connected"},
                {"name":"bad","status":"failed","error":"secret-never-log"}]}));
        assert!(last(home.path(), "employee", "good").unwrap().ok);
        assert_eq!(last(home.path(), "employee", "bad").unwrap().reason_code, Some(ReasonCode::ConnectionFailed));
        let log = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(!log.contains("secret-never-log"));
        let record: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(record["status"], "failed");
    }

    #[test]
    #[ignore = "Rust stdio child for probe tests"]
    fn fixture() {
        // Separate libtest progress text from protocol frames.
        println!();
        use std::io::{BufRead, Write};
        let mode = std::env::var("MCP_FIXTURE_MODE").unwrap_or_default();
        if mode == "timeout" { std::thread::sleep(std::time::Duration::from_secs(30)); return; }
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break; };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue; };
            if v.get("id").is_none() { continue; }
            let frame = if mode == "failure" {
                json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32001,
                    "message":"fixed","data":{"reason_code":"identity_rejected"}}})
            } else {
                let result = if v["method"] == "initialize" {
                    json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                } else { json!({"tools":[{"name":"echo_text"}]}) };
                json!({"jsonrpc":"2.0","id":v["id"],"result":result})
            };
            println!("{frame}");
            std::io::stdout().flush().unwrap();
        }
    }

    #[tokio::test]
    async fn install_probe_success_failure_and_timeout() {
        let home = tempfile::tempdir().unwrap();
        for (mode, expected) in [("success", Ok(1)), ("failure", Err(ReasonCode::IdentityRejected)),
            ("timeout", Err(ReasonCode::Timeout))] {
            let entry = json!({"command":std::env::current_exe().unwrap(),
                "args":["--exact","mcp_start::tests::fixture","--ignored","--nocapture"],
                "env":{"MCP_FIXTURE_MODE":mode,"HOME":home.path()}});
            let result = probe_entry(&entry, std::time::Duration::from_millis(500)).await;
            assert_eq!(result, expected, "{mode}");
        }
    }
}
