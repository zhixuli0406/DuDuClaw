//! The eight `computer_*` MCP tools as a thin client of the gateway route
//! `POST /api/internal/computer-use` (design `DESIGN-computer-use-mcp-bridge`
//! §3.6). The session, the container and every safety check live in the
//! gateway (`duduclaw_gateway::computer_use_sessions`); this side only turns
//! tool arguments into a request and the answer into an MCP result.
//!
//! Authentication: a request signature, never the secrets themselves.
//! Headers `X-Duduclaw-Agent-Id` (the calling employee),
//! `X-Duduclaw-Timestamp` (unix seconds), `X-Duduclaw-Nonce` (16 random
//! bytes, hex) and `X-Duduclaw-Signature`
//! (`duduclaw_core::internal_request_signature`, keyed by
//! `$DUDUCLAW_MCP_API_KEY` over the id, `$DUDUCLAW_AGENT_TOKEN`, the
//! timestamp, the nonce and the body hash). A process that binds the port
//! while the gateway is down learns nothing reusable. The gateway port comes
//! from `duduclaw_core::gateway_port_for_home`.
//!
//! The only conversation hint sent is the opaque `$DUDUCLAW_TURN_ID`; the
//! gateway decides from its own record of live turns where a high-risk
//! confirmation may be asked.
//!
//! Every failure (gateway down, refused, no session) is an `isError: true`
//! result with a readable zh-TW reason; nothing is ever reported as started
//! or executed unless the gateway says so.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

/// Per-op client timeouts. Each exceeds the gateway's own bound for that op
/// (`computer_use_sessions::http::*_BUDGET`): any op but `status` may first
/// wait up to 300 s for an approval when the tool is in an approval list,
/// and an action may then wait up to 60 s for a confirmation.
pub(crate) const START_TIMEOUT: Duration = Duration::from_secs(420);
pub(crate) const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(360);
pub(crate) const ACTION_TIMEOUT: Duration = Duration::from_secs(400);
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(360);
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest gateway answer read (a screenshot is the big one).
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// The route path (mirrors `duduclaw_gateway::computer_use_sessions::http::ROUTE`).
const ROUTE: &str = "/api/internal/computer-use";
/// Longest gateway message passed through to the agent.
const MAX_MESSAGE_CHARS: usize = 500;

// ── argument parsing ─────────────────────────────────────────────────────

/// An integer argument given as a JSON number or a numeric string. `Ok(None)`
/// when absent or null.
pub(crate) fn int_arg(args: &Value, name: &str) -> Result<Option<i64>, String> {
    let bad = || format!("參數 {name} 必須是整數。");
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(Some(i))
            } else {
                match n.as_f64() {
                    Some(f) if f.fract() == 0.0 && f.abs() < 1e12 => Ok(Some(f as i64)),
                    _ => Err(bad()),
                }
            }
        }
        Some(Value::String(s)) => s.trim().parse::<i64>().map(Some).map_err(|_| bad()),
        Some(_) => Err(bad()),
    }
}

/// A boolean argument given as a JSON bool or `"true"` / `"false"`.
pub(crate) fn bool_arg(args: &Value, name: &str) -> Result<Option<bool>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(format!("參數 {name} 必須是 true 或 false。")),
        },
        Some(_) => Err(format!("參數 {name} 必須是 true 或 false。")),
    }
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

fn required_int(args: &Value, name: &str) -> Result<i64, String> {
    int_arg(args, name)?.ok_or_else(|| format!("缺少必要參數 {name}。"))
}

/// The request body for one tool call, or the argument error.
pub(crate) fn build_request(tool: &str, args: &Value, turn_id: Option<String>) -> Result<Value, String> {
    let session_id = str_arg(args, "session_id");
    match tool {
        "computer_session_start" => {
            let width = int_arg(args, "width")?;
            let height = int_arg(args, "height")?;
            for (name, v) in [("width", width), ("height", height)] {
                if v.is_some_and(|v| !(0..=u32::MAX as i64).contains(&v)) {
                    return Err(format!("參數 {name} 超出範圍。"));
                }
            }
            let mut body = json!({
                "op": "start",
                "width": width,
                "height": height,
                "task": str_arg(args, "task"),
                "turn_id": turn_id,
            });
            // Only when asked for: a start without it is byte-identical.
            if let Some(ws) = str_arg(args, "workspace") {
                body["workspace"] = json!(ws);
            }
            Ok(body)
        }
        "computer_screenshot" => {
            if str_arg(args, "display").is_some_and(|d| d != "container") {
                return Err("computer_screenshot 只支援容器畫面（display = container）。".to_string());
            }
            Ok(json!({"op": "screenshot", "session_id": session_id}))
        }
        "computer_click" => Ok(json!({
            "op": "action",
            "session_id": session_id,
            "turn_id": turn_id,
            "action": {
                "type": "click",
                "x": required_int(args, "x")?,
                "y": required_int(args, "y")?,
                "button": str_arg(args, "button"),
                "double": bool_arg(args, "double")?,
            }
        })),
        "computer_type" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({"op": "action", "session_id": session_id, "turn_id": turn_id, "action": {"type": "type", "text": text}}))
        }
        "computer_key" => {
            let key = str_arg(args, "key").unwrap_or("");
            Ok(json!({"op": "action", "session_id": session_id, "turn_id": turn_id, "action": {"type": "key", "key": key}}))
        }
        "computer_scroll" => Ok(json!({
            "op": "action",
            "session_id": session_id,
            "turn_id": turn_id,
            "action": {
                "type": "scroll",
                "x": required_int(args, "x")?,
                "y": required_int(args, "y")?,
                "direction": str_arg(args, "direction"),
                "amount": int_arg(args, "amount")?,
            }
        })),
        "computer_navigate" => {
            // Passed through untrimmed: the gateway validates exactly what
            // will be opened.
            let url = args.get("url").and_then(|v| v.as_str()).filter(|u| !u.is_empty());
            let url = url.ok_or_else(|| "缺少必要參數 url（https:// 開頭的完整網址）。".to_string())?;
            Ok(json!({"op": "action", "session_id": session_id, "turn_id": turn_id, "action": {"type": "navigate", "url": url}}))
        }
        "computer_session_stop" => Ok(json!({"op": "stop", "session_id": session_id})),
        "computer_workspace_list" => Ok(json!({"op": "workspace_list"})),
        "computer_workspace_read" => Ok(json!({
            "op": "workspace_read",
            "workspace_id": str_arg(args, "workspace_id").ok_or("缺少必要參數 workspace_id。")?,
            "path": str_arg(args, "path").ok_or("缺少必要參數 path。")?,
        })),
        "computer_workspace_write" => {
            // `content` is passed through untrimmed: the file gets exactly it.
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("缺少必要參數 content（UTF-8 文字）。")?;
            Ok(json!({
                "op": "workspace_write",
                "workspace_id": str_arg(args, "workspace_id").ok_or("缺少必要參數 workspace_id。")?,
                "path": str_arg(args, "path").ok_or("缺少必要參數 path。")?,
                "content": content,
                "expected_revision": int_arg(args, "expected_revision")?,
            }))
        }
        _ => Err(format!("未知的電腦操作工具：{tool}")),
    }
}

fn timeout_for(tool: &str) -> Duration {
    match tool {
        "computer_session_start" => START_TIMEOUT,
        "computer_screenshot" => SCREENSHOT_TIMEOUT,
        "computer_session_stop" => STOP_TIMEOUT,
        // The gateway bound is the stop budget (an approval may come first).
        "computer_workspace_list" | "computer_workspace_read" | "computer_workspace_write" => STOP_TIMEOUT,
        "computer_click" | "computer_type" | "computer_key" | "computer_scroll" | "computer_navigate" => {
            ACTION_TIMEOUT
        }
        _ => STATUS_TIMEOUT,
    }
}

// ── transport ────────────────────────────────────────────────────────────

/// Why a call did not produce a gateway answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportFailure {
    Connect,
    Timeout,
    Other,
}

pub(crate) fn transport_message(failure: TransportFailure, port: u16, timeout: Duration) -> String {
    match failure {
        TransportFailure::Connect => format!(
            "無法連線到 DuDuClaw gateway（127.0.0.1:{port}）。電腦操作工具需要 gateway 正在執行，請確認後再試。"
        ),
        TransportFailure::Timeout => format!(
            "DuDuClaw gateway 在 {} 秒內沒有回應這個電腦操作請求，請稍後再試。",
            timeout.as_secs()
        ),
        TransportFailure::Other => "呼叫 DuDuClaw gateway 時發生連線錯誤，請稍後再試。".to_string(),
    }
}

/// Turn the gateway's HTTP answer into the body (`ok: true`) or the reason
/// to show the agent.
pub(crate) fn interpret(status: u16, body: &[u8]) -> Result<Value, String> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Err(format!("DuDuClaw gateway 的回應無法辨識（HTTP {status}）。"));
    };
    if status == 200 && value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(value);
    }
    match value.get("message").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => Err(duduclaw_core::truncate_chars(m.trim(), MAX_MESSAGE_CHARS)),
        _ => Err(format!("DuDuClaw gateway 拒絕了這個電腦操作請求（HTTP {status}）。")),
    }
}

/// Credentials the request is signed with, read from this process's
/// environment. Neither secret is ever sent.
struct Caller {
    key: String,
    agent_id: String,
    token: String,
}

fn caller(agent_id: &str) -> Result<Caller, String> {
    let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let key = env(duduclaw_core::ENV_MCP_API_KEY).ok_or_else(|| {
        "電腦操作工具無法使用：這個 MCP 程序沒有 gateway 內部金鑰，只有由 DuDuClaw gateway 啟動的 AI 員工能使用。".to_string()
    })?;
    let token = env(duduclaw_core::ENV_AGENT_TOKEN).ok_or_else(|| {
        "電腦操作工具無法使用：缺少員工身分權杖，請由 DuDuClaw gateway 重新啟動此員工。".to_string()
    })?;
    Ok(Caller { key, agent_id: agent_id.to_string(), token })
}

/// The four authentication headers for `body` (see the module doc).
pub(crate) fn signed_headers(
    internal_key: &str,
    agent_id: &str,
    agent_token: &str,
    timestamp: u64,
    nonce: &[u8; 16],
    body: &[u8],
) -> [(&'static str, String); 4] {
    let timestamp = timestamp.to_string();
    let nonce = hex::encode(nonce);
    let signature = duduclaw_core::internal_request_signature(
        internal_key.as_bytes(),
        agent_id,
        agent_token,
        &timestamp,
        &nonce,
        body,
    );
    [
        ("X-Duduclaw-Agent-Id", agent_id.to_string()),
        ("X-Duduclaw-Timestamp", timestamp),
        ("X-Duduclaw-Nonce", nonce),
        ("X-Duduclaw-Signature", signature),
    ]
}

/// Read a response body; `Ok(None)` as soon as it exceeds `limit` bytes
/// (the rest is never read).
async fn read_capped(mut resp: reqwest::Response, limit: usize) -> Result<Option<Vec<u8>>, reqwest::Error> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > limit {
            return Ok(None);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Some(out))
}

async fn post(home: &Path, agent_id: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let who = caller(agent_id)?;
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| transport_message(TransportFailure::Other, port, timeout))?;
    let bytes = serde_json::to_vec(body).map_err(|_| transport_message(TransportFailure::Other, port, timeout))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nonce: [u8; 16] = rand::random();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}{ROUTE}"))
        .header("Content-Type", "application/json");
    for (name, value) in signed_headers(&who.key, &who.agent_id, &who.token, timestamp, &nonce, &bytes) {
        request = request.header(name, value);
    }
    let resp = request.body(bytes).send().await.map_err(|e| {
        let failure = if e.is_timeout() {
            TransportFailure::Timeout
        } else if e.is_connect() {
            TransportFailure::Connect
        } else {
            TransportFailure::Other
        };
        transport_message(failure, port, timeout)
    })?;
    let status = resp.status().as_u16();
    let bytes = read_capped(resp, MAX_RESPONSE_BYTES).await.map_err(|e| {
        let failure = if e.is_timeout() { TransportFailure::Timeout } else { TransportFailure::Other };
        transport_message(failure, port, timeout)
    })?;
    match bytes {
        Some(bytes) => interpret(status, &bytes),
        None => Err("DuDuClaw gateway 的回應超過 16 MiB，已捨棄。".to_string()),
    }
}

// ── results ──────────────────────────────────────────────────────────────

fn error(msg: &str) -> Value {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}

fn text(msg: String) -> Value {
    json!({"content": [{"type": "text", "text": msg}]})
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn counters_line(v: &Value) -> String {
    format!(
        "已用 {}/{} 個動作，session 剩約 {} 秒。",
        u(v, "actions_used"),
        u(v, "max_actions"),
        u(v, "seconds_left")
    )
}

/// Why the whole screenshot was hidden and what to do next, for a fully
/// masked screenshot (`fully_masked: true` or any `mask_reason`); `None`
/// for a partially masked or unmasked one. An unknown code gets the generic
/// "try again" advice.
fn full_mask_text(v: &Value) -> Option<&'static str> {
    let reason = v.get("mask_reason").and_then(Value::as_str);
    let fully = v.get("fully_masked").and_then(Value::as_bool) == Some(true);
    if !fully && reason.is_none() {
        return None;
    }
    Some(match reason {
        Some("several_pages") => {
            "畫面上同時開著好幾個網頁視窗，無法判斷哪一個在最前面。請呼叫 computer_navigate 重新開啟網頁，它會關掉多出來的視窗。"
        }
        Some("title_sensitive") => {
            "目前最前面的視窗看起來是密碼或憑證相關的畫面，這個視窗在最前面時無法顯示畫面。"
        }
        Some("title_unreadable") => {
            "無法確認目前最前面的是哪個視窗，這個視窗在最前面時無法顯示畫面。"
        }
        _ => {
            "暫時無法檢查畫面上有沒有敏感資料。請再呼叫一次 computer_screenshot；如果一直這樣，請用 computer_session_stop 結束 session，再用 computer_session_start 重新開始。"
        }
    })
}

/// The MCP result for a successful gateway answer.
pub(crate) fn render(tool: &str, v: &Value) -> Value {
    let id = v.get("session_id").and_then(Value::as_str).unwrap_or("");
    match tool {
        "computer_session_start" => {
            let mut msg = format!(
                "電腦操作 session 已啟動（{id}）：螢幕 {}x{}，最多 {} 個動作，約 {} 秒內有效，閒置 2 分鐘會自動結束。用 computer_screenshot 看畫面，座標以截圖的像素為準。",
                u(v, "width"),
                u(v, "height"),
                u(v, "max_actions"),
                u(v, "seconds_left")
            );
            if v.get("confirmation_channel").and_then(Value::as_bool) != Some(true) {
                msg.push_str("這次沒有可詢問的對話通道，高風險操作會直接被拒絕。");
            }
            if let Some(network) = v.get("network_message").and_then(Value::as_str) {
                msg.push_str(&duduclaw_core::truncate_chars(network, MAX_MESSAGE_CHARS));
            }
            if let Some(ws) = v.get("workspace_id").and_then(Value::as_str) {
                msg.push_str(&format!(
                    "已掛載工作區 {ws}（容器內唯讀路徑 /workspace/files，僅 root 可讀，版本 {}，{} 個檔案）。寫檔請用 computer_workspace_write。",
                    u(v, "data_revision"),
                    u(v, "files_used")
                ));
            }
            text(msg)
        }
        "computer_workspace_list" | "computer_workspace_read" | "computer_workspace_write" => {
            let mut out = v.clone();
            if let Some(map) = out.as_object_mut() {
                map.remove("ok");
                // The write answer is audited as result text: no path in it.
                if tool == "computer_workspace_write" {
                    map.remove("path");
                }
            }
            text(serde_json::to_string_pretty(&out).unwrap_or_default())
        }
        "computer_screenshot" => {
            let data = v.get("png_base64").and_then(Value::as_str).unwrap_or("");
            if data.is_empty() {
                return error("DuDuClaw gateway 沒有回傳截圖影像。");
            }
            let caption = match full_mask_text(v) {
                Some(why) => format!(
                    "截圖 {}x{}：為了安全，整張畫面都已遮蔽（全黑）。{}{}",
                    u(v, "width"),
                    u(v, "height"),
                    why,
                    counters_line(v)
                ),
                None => format!(
                    "截圖 {}x{}（敏感區域已遮蔽）。{}",
                    u(v, "width"),
                    u(v, "height"),
                    counters_line(v)
                ),
            };
            json!({
                "content": [
                    {"type": "image", "data": data, "mimeType": "image/png"},
                    {"type": "text", "text": caption}
                ]
            })
        }
        "computer_session_stop" => text(format!(
            "電腦操作 session 已結束（{id}）：共執行 {} 個動作，歷時 {} 秒，容器已移除。",
            u(v, "actions_used"),
            u(v, "duration_secs")
        )),
        _ => {
            let message = v.get("message").and_then(Value::as_str).unwrap_or("已執行");
            text(format!(
                "{}。{}要確認結果請呼叫 computer_screenshot。",
                duduclaw_core::truncate_chars(message, MAX_MESSAGE_CHARS),
                counters_line(v)
            ))
        }
    }
}

/// The `DUDUCLAW_TURN_ID` of this MCP process: the opaque id of the turn
/// the employee is answering, if any. The gateway maps it (under the
/// verified employee id) to the chat a confirmation may be asked in.
fn turn_id() -> Option<String> {
    std::env::var(duduclaw_core::ENV_TRUST_TURN_ID)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
}

/// Handle one `computer_*` tool call.
pub(crate) async fn handle_computer_use_tool(
    tool: &str,
    args: &Value,
    home: &Path,
    agent_id: &str,
) -> Value {
    let body = match build_request(tool, args, turn_id()) {
        Ok(body) => body,
        Err(msg) => return error(&msg),
    };
    match post(home, agent_id, &body, timeout_for(tool)).await {
        Ok(v) if workspace_missing(&body, &v) => error(WORKSPACE_NOT_ATTACHED),
        Ok(v) => render(tool, &v),
        Err(msg) => error(&msg),
    }
}

/// Fail closed against an older gateway that ignores `workspace` and starts
/// a session without one (design §7.4).
const WORKSPACE_NOT_ATTACHED: &str = "要求掛載工作區，但 gateway 開出的 session 沒有工作區（gateway 版本可能較舊）。請呼叫 computer_session_stop 結束這個 session，並請管理員更新 DuDuClaw。";

pub(crate) fn workspace_missing(request: &Value, answer: &Value) -> bool {
    request.get("op").and_then(Value::as_str) == Some("start")
        && request.get("workspace").is_some()
        && answer.get("workspace_id").and_then(Value::as_str).is_none_or(str::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_numeric_strings_are_both_accepted() {
        let args = json!({"x": 12, "y": "34", "a": 5.0, "b": "1.5", "c": true, "d": " 7 "});
        assert_eq!(int_arg(&args, "x"), Ok(Some(12)));
        assert_eq!(int_arg(&args, "y"), Ok(Some(34)));
        assert_eq!(int_arg(&args, "a"), Ok(Some(5)));
        assert_eq!(int_arg(&args, "d"), Ok(Some(7)));
        assert!(int_arg(&args, "b").is_err());
        assert!(int_arg(&args, "c").is_err());
        assert_eq!(int_arg(&args, "missing"), Ok(None));
        let flags = json!({"t": true, "s": "false", "n": 1});
        assert_eq!(bool_arg(&flags, "t"), Ok(Some(true)));
        assert_eq!(bool_arg(&flags, "s"), Ok(Some(false)));
        assert!(bool_arg(&flags, "n").is_err());
    }

    #[test]
    fn requests_carry_typed_values() {
        let click = build_request("computer_click", &json!({"x": "10", "y": 20, "double": "true"}), None).unwrap();
        assert_eq!(click["action"], json!({"type": "click", "x": 10, "y": 20, "button": null, "double": true}));
        let scroll = build_request("computer_scroll", &json!({"x": 1, "y": 2, "amount": "4"}), None).unwrap();
        assert_eq!(scroll["action"]["amount"], 4);
        let start = build_request("computer_session_start", &json!({"width": "800"}), Some("turn-1".into())).unwrap();
        assert_eq!(start["op"], "start");
        assert_eq!(start["width"], 800);
        assert_eq!(start["turn_id"], "turn-1");
        assert!(start.get("reply_channel").is_none(), "no chat is ever named by the caller");
        let typed = build_request("computer_type", &json!({"text": "hi"}), Some("turn-1".into())).unwrap();
        assert_eq!(typed["turn_id"], "turn-1");
        assert!(build_request("computer_click", &json!({"y": 1}), None).is_err(), "x is required");
        assert!(build_request("computer_screenshot", &json!({"display": "native"}), None).is_err());
        assert_eq!(build_request("computer_session_stop", &json!({}), None).unwrap()["session_id"], Value::Null);
        let nav = build_request("computer_navigate", &json!({"url": "https://example.com/a"}), Some("t".into())).unwrap();
        assert_eq!(nav["op"], "action");
        assert_eq!(nav["action"], json!({"type": "navigate", "url": "https://example.com/a"}));
        assert!(build_request("computer_navigate", &json!({}), None).is_err(), "url is required");
        assert!(build_request("computer_navigate", &json!({"url": ""}), None).is_err());
        assert_eq!(timeout_for("computer_navigate"), ACTION_TIMEOUT);
    }

    #[test]
    fn start_and_navigate_results_carry_the_network_line() {
        let start = render(
            "computer_session_start",
            &json!({"ok": true, "session_id": "cu-1", "width": 1280, "height": 800, "max_actions": 50,
                    "seconds_left": 600, "confirmation_channel": true,
                    "network_message": "可用 computer_navigate 開啟的網站：example.com。"}),
        );
        assert!(start["content"][0]["text"].as_str().unwrap().ends_with("可用 computer_navigate 開啟的網站：example.com。"));
        let nav = render(
            "computer_navigate",
            &json!({"ok": true, "message": "已開啟網頁（example.com）", "actions_used": 1, "max_actions": 50}),
        );
        assert!(nav["content"][0]["text"].as_str().unwrap().starts_with("已開啟網頁（example.com）。"));
    }

    #[test]
    fn gateway_answers_map_to_results_or_readable_errors() {
        assert_eq!(interpret(200, br#"{"ok":true,"x":1}"#).unwrap()["x"], 1);
        assert_eq!(
            interpret(404, "{\"ok\":false,\"code\":\"not_found\",\"message\":\"目前沒有進行中的電腦操作 session\"}".as_bytes()),
            Err("目前沒有進行中的電腦操作 session".to_string())
        );
        assert!(interpret(502, b"<html>bad gateway</html>").unwrap_err().contains("HTTP 502"));
        assert!(interpret(403, br#"{"ok":false}"#).unwrap_err().contains("HTTP 403"));
        // `ok` must be true on a 200 too.
        assert!(interpret(200, br#"{"ok":false,"message":"x"}"#).is_err());
        assert!(transport_message(TransportFailure::Connect, 18789, START_TIMEOUT).contains("127.0.0.1:18789"));
        assert!(transport_message(TransportFailure::Timeout, 1, ACTION_TIMEOUT).contains("400 秒"));
    }

    #[test]
    fn screenshot_is_an_image_block_followed_by_text() {
        let v = json!({"ok": true, "png_base64": "iVBORw0KGgo=", "width": 1280, "height": 800,
                       "actions_used": 3, "max_actions": 50, "seconds_left": 540});
        let r = render("computer_screenshot", &v);
        assert_eq!(r["content"][0], json!({"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"}));
        assert_eq!(r["content"][1]["type"], "text");
        let t = r["content"][1]["text"].as_str().unwrap();
        assert!(t.contains("3/50") && t.contains("540"), "{t}");
        assert!(!t.contains("iVBOR"), "the text block never carries the image");
        assert!(r.get("isError").is_none());
        assert_eq!(render("computer_screenshot", &json!({"ok": true}))["isError"], true);
        let action = render("computer_click", &json!({"ok": true, "message": "已執行：左鍵點擊 (1, 2)", "actions_used": 1, "max_actions": 50}));
        assert!(action["content"][0]["text"].as_str().unwrap().starts_with("已執行：左鍵點擊"));
    }

    #[test]
    fn a_fully_masked_screenshot_says_why_and_what_to_do() {
        let base = json!({"ok": true, "png_base64": "iVBORw0KGgo=", "width": 1280, "height": 800,
                          "actions_used": 3, "max_actions": 50, "seconds_left": 540});
        let text_for = |extra: Value| {
            let mut v = base.clone();
            for (k, val) in extra.as_object().unwrap() {
                v[k] = val.clone();
            }
            let r = render("computer_screenshot", &v);
            assert!(r.get("isError").is_none());
            assert_eq!(r["content"][0]["type"], "image");
            r["content"][1]["text"].as_str().unwrap().to_string()
        };
        let whole = "整張畫面都已遮蔽";
        let several = text_for(json!({"fully_masked": true, "mask_reason": "several_pages"}));
        assert!(several.contains(whole) && several.contains("好幾個網頁視窗"), "{several}");
        assert!(several.contains("computer_navigate") && several.contains("關掉多出來的視窗"), "{several}");
        let sensitive = text_for(json!({"fully_masked": true, "mask_reason": "title_sensitive"}));
        assert!(sensitive.contains(whole) && sensitive.contains("密碼或憑證"), "{sensitive}");
        assert!(sensitive.contains("在最前面時無法顯示畫面"), "{sensitive}");
        let unreadable = text_for(json!({"fully_masked": true, "mask_reason": "title_unreadable"}));
        assert!(unreadable.contains(whole) && unreadable.contains("無法確認"), "{unreadable}");
        assert!(unreadable.contains("在最前面時無法顯示畫面"), "{unreadable}");
        for extra in [
            json!({"fully_masked": true, "mask_reason": "helper_failed"}),
            json!({"fully_masked": true, "mask_reason": "something_new"}),
            json!({"fully_masked": true}),
        ] {
            let t = text_for(extra);
            assert!(t.contains(whole) && t.contains("再呼叫一次 computer_screenshot"), "{t}");
            assert!(t.contains("computer_session_stop") && t.contains("computer_session_start"), "{t}");
        }
        for t in [&several, &sensitive, &unreadable] {
            assert!(t.contains("3/50") && t.contains("540"), "{t}");
            assert!(!t.contains("敏感區域已遮蔽"), "{t}");
            for code in ["several_pages", "title_sensitive", "title_unreadable", "helper_failed", "eval-dom"] {
                assert!(!t.contains(code), "no codes or file names in the text: {t}");
            }
        }
        // Not fully masked: today's text.
        for extra in [json!({}), json!({"fully_masked": false, "mask_reason": null})] {
            let t = text_for(extra);
            assert!(t.starts_with("截圖 1280x800（敏感區域已遮蔽）。"), "{t}");
            assert!(!t.contains(whole), "{t}");
        }
    }

    #[test]
    fn requests_are_signed_and_carry_no_secret() {
        let key = "ddc_prod_0123456789abcdef0123456789abcdef";
        let token = "a".repeat(64);
        let body = br#"{"op":"status"}"#;
        let headers = signed_headers(key, "alice", &token, 1_700_000_000, &[7u8; 16], body);
        let get = |n: &str| headers.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("X-Duduclaw-Agent-Id"), "alice");
        assert_eq!(get("X-Duduclaw-Timestamp"), "1700000000");
        assert_eq!(get("X-Duduclaw-Nonce"), "07".repeat(16));
        let sig = get("X-Duduclaw-Signature");
        assert!(duduclaw_core::verify_internal_request_signature(
            key.as_bytes(), "alice", &token, "1700000000", &"07".repeat(16), body, &sig
        ));
        for (name, value) in &headers {
            assert!(!value.contains(key) && !value.contains(&token), "{name} leaks a secret");
            assert!(!name.eq_ignore_ascii_case("authorization"));
        }
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_large_answers_are_capped() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for answer in [
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/x\r\nContent-Length: 0\r\n\r\n".to_string(),
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}", 64, "x".repeat(64)),
            ] {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                sock.write_all(answer.as_bytes()).await.unwrap();
                let _ = sock.shutdown().await;
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = format!("http://{addr}/");
        let resp = client.post(&url).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 302, "the redirect is returned, not followed");
        let resp = client.post(&url).send().await.unwrap();
        assert_eq!(read_capped(resp, 16).await.unwrap(), None, "over the cap is refused");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_gateway_is_an_error_never_a_fake_success() {
        // A port nothing listens on (bound then dropped).
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        // `post` reads the key/token from the environment, which a test must
        // not touch; drive the same transport and failure mapping directly.
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let err = client.post(format!("http://127.0.0.1:{port}{ROUTE}")).send().await.unwrap_err();
        assert!(err.is_connect());
        let msg = transport_message(TransportFailure::Connect, port, START_TIMEOUT);
        assert!(msg.contains(&port.to_string()));
    }

    #[test]
    fn workspace_requests_and_the_old_gateway_fail_closed() {
        let plain = build_request("computer_session_start", &json!({}), None).unwrap();
        assert!(plain.get("workspace").is_none(), "no workspace key unless asked for");
        let ws = build_request("computer_session_start", &json!({"workspace": "new"}), None).unwrap();
        assert_eq!(ws["workspace"], "new");
        assert!(workspace_missing(&ws, &json!({"ok": true, "session_id": "cu-1"})));
        assert!(!workspace_missing(&ws, &json!({"ok": true, "workspace_id": "ws-x"})));
        assert!(!workspace_missing(&plain, &json!({"ok": true})));
        let w = build_request(
            "computer_workspace_write",
            &json!({"workspace_id": "ws-a", "path": "a.md", "content": " x ", "expected_revision": "3"}),
            None,
        )
        .unwrap();
        assert_eq!(w, json!({"op": "workspace_write", "workspace_id": "ws-a", "path": "a.md", "content": " x ", "expected_revision": 3}));
        assert!(build_request("computer_workspace_write", &json!({"workspace_id": "ws-a", "path": "a"}), None).is_err());
        assert!(build_request("computer_workspace_read", &json!({"path": "a"}), None).is_err());
        assert_eq!(build_request("computer_workspace_list", &json!({}), None).unwrap(), json!({"op": "workspace_list"}));
        assert_eq!(timeout_for("computer_workspace_write"), STOP_TIMEOUT);
    }
}
