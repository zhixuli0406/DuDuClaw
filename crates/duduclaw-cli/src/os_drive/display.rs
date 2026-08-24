//! A7a `display` group — a thin client for comp's `shell_control` socket
//! (`crates/duduclaw-comp/src/shell_control/protocol.rs`). See
//! `commercial/docs/DESIGN-os-self-drive-2026-08.md` §3/§7/§8 for why this
//! hand-rolls the wire JSON instead of depending on `duduclaw-comp` (a
//! smithay/Wayland crate that would break the macOS dev build, and whose own
//! module doc already frames this socket as "reuse only the pure JSON data,
//! never a shared type") and for the exact uid-boundary reasoning behind the
//! error messages below.
//!
//! Every function here does exactly one request/response round trip:
//! connect → write one JSON line → read one JSON line → close. No retries,
//! no persistent connection — matching the wire protocol's own "one-shot
//! RPC, not a session" contract.

use std::time::Duration;

use serde_json::{json, Value};

/// Same value the wire protocol's own `MAX_REQUEST_LINE_BYTES` uses on the
/// server side — a sane upper bound for a reply we read into memory.
const MAX_RESPONSE_LINE_BYTES: usize = 4096;

/// How long to wait for a reply before giving up. Every real reply is a
/// synchronous, in-process answer on comp's side (no network hop) — 5s is
/// generous headroom over "instant", not a real operation timeout.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve `$XDG_RUNTIME_DIR/duduclaw-shell.sock` — same file name the
/// server uses (`shell_control::protocol::SOCKET_FILE_NAME`, duplicated here
/// as a literal for the same "don't depend on duduclaw-comp" reason the
/// module doc gives).
fn socket_path() -> Result<std::path::PathBuf, String> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        "XDG_RUNTIME_DIR 未設定，無法定位 shell socket。這通常代表呼叫者不是在一個真正的桌面/\
         kiosk session 裡執行——appliance 上殼固定使用 XDG_RUNTIME_DIR=/run/duduclaw-kiosk。"
            .to_string()
    })?;
    Ok(std::path::PathBuf::from(runtime_dir).join("duduclaw-shell.sock"))
}

#[cfg(unix)]
async fn call(req: Value) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let path = socket_path()?;
    if !path.exists() {
        return Err(format!(
            "shell socket 不存在：{}。comp/殼可能未啟動，或這台呼叫者的 $XDG_RUNTIME_DIR 跟殼\
             實際監聽的 $XDG_RUNTIME_DIR 不是同一個——appliance 上殼固定用 /run/duduclaw-kiosk，\
             agent 身分（$XDG_RUNTIME_DIR 通常是 /run/duduclaw 或未設）結構上到不了這個 socket，\
             見設計文件 DESIGN-os-self-drive-2026-08.md §7。",
            path.display()
        ));
    }

    let mut stream = tokio::time::timeout(RESPONSE_TIMEOUT, UnixStream::connect(&path))
        .await
        .map_err(|_| format!("連線 {} 逾時。", path.display()))?
        .map_err(|e| format!("連線 shell socket 失敗（{}）：{e}", path.display()))?;

    let mut line = serde_json::to_string(&req).map_err(|e| format!("序列化請求失敗：{e}"))?;
    line.push('\n');
    tokio::time::timeout(RESPONSE_TIMEOUT, stream.write_all(line.as_bytes()))
        .await
        .map_err(|_| "寫入 shell socket 逾時。".to_string())?
        .map_err(|e| format!("寫入 shell socket 失敗：{e}"))?;

    let mut buf = String::new();
    let bytes_read = {
        let mut reader = BufReader::new(&mut stream);
        tokio::time::timeout(RESPONSE_TIMEOUT, reader.read_line(&mut buf))
            .await
            .map_err(|_| "等待 shell socket 回應逾時。".to_string())?
            .map_err(|e| format!("讀取 shell socket 回應失敗：{e}"))?
    };

    if bytes_read == 0 || buf.trim().is_empty() {
        return Err(
            "連線已建立但未收到任何回應（連線在寫入回應前就被關閉）。若這不是暫時性問題，\
             請確認呼叫者與 comp/殼行程是否為同一個系統帳號。"
                .to_string(),
        );
    }
    // Defense-in-depth: a well-behaved server never sends a line anywhere
    // near this size — same bound the server enforces on OUR request
    // (`MAX_REQUEST_LINE_BYTES`), applied symmetrically to what we accept.
    if buf.len() > MAX_RESPONSE_LINE_BYTES {
        return Err(format!(
            "shell socket 回應超過 {MAX_RESPONSE_LINE_BYTES} bytes 上限，已拒絕解析。"
        ));
    }

    let resp: Value = serde_json::from_str(buf.trim())
        .map_err(|e| format!("shell socket 回應不是合法 JSON：{e}（原始內容：{}）", buf.trim()))?;

    if resp.get("ok").and_then(Value::as_bool) == Some(false) {
        let err = resp.get("error").and_then(Value::as_str).unwrap_or("unknown_error");
        if err == "unauthorized" {
            return Err(
                "shell socket 拒絕連線（unauthorized）——same-uid SO_PEERCRED 邊界：呼叫者與 comp/\
                 殼行程不是同一個系統帳號。appliance 上殼固定以 duduclaw-kiosk 身分執行，agent 身分\
                 的呼叫者（duduclaw 帳號）結構上無法通過這個檢查，見設計文件 §7——這不是暫時性錯誤。"
                    .to_string(),
            );
        }
        return Err(format!("shell_control 回報錯誤：{err}"));
    }

    Ok(resp)
}

#[cfg(not(unix))]
async fn call(_req: Value) -> Result<Value, String> {
    Err("duduclaw os display 命令僅支援 Unix（Linux/macOS）——這個平台沒有 shell_control socket。"
        .to_string())
}

pub async fn cursor_size_get() -> Result<String, String> {
    let resp = call(json!({ "op": "get_cursor_source" })).await?;
    Ok(format!("{:#}", resp.get("cursor").cloned().unwrap_or(Value::Null)))
}

pub async fn cursor_size_set(size: i64) -> Result<String, String> {
    let resp = call(json!({ "op": "set_cursor_size", "params": { "size": size } })).await?;
    Ok(format!("{:#}", resp.get("cursor").cloned().unwrap_or(Value::Null)))
}

pub async fn cursor_source_get() -> Result<String, String> {
    let resp = call(json!({ "op": "get_cursor_source" })).await?;
    Ok(format!("{:#}", resp.get("cursor").cloned().unwrap_or(Value::Null)))
}

pub async fn cursor_source_set(source: &str) -> Result<String, String> {
    let resp = call(json!({ "op": "set_cursor_source", "params": { "source": source } })).await?;
    Ok(format!("{:#}", resp.get("cursor").cloned().unwrap_or(Value::Null)))
}

pub async fn theme_set(theme: &str) -> Result<String, String> {
    let _resp = call(json!({ "op": "set_theme", "params": { "theme": theme } })).await?;
    Ok(format!("theme 已切換為 {theme}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// Serializes every test below that touches the process-wide
    /// `XDG_RUNTIME_DIR` env var. `#[tokio::test(flavor = "current_thread")]`
    /// only isolates each test's ASYNC runtime — the Rust test harness still
    /// runs different `#[test]`/`#[tokio::test]` FUNCTIONS concurrently on
    /// separate OS threads by default, so two such tests setting/reading
    /// this env var at the same time race each other (found the hard way:
    /// without this lock, `cursor_size_set_round_trips_against_a_stub_server`
    /// intermittently observed a DIFFERENT test's stub server because both
    /// had briefly overlapping `XDG_RUNTIME_DIR` values). Held across
    /// `.await` on purpose — safe here because `current_thread` flavor does
    /// not require the test future to be `Send`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Spin up a stub `shell_control`-shaped server on a temp socket and
    /// point `call` at it via `XDG_RUNTIME_DIR` — proves the request/
    /// response wire round trip works without needing a real compositor.
    async fn run_stub(dir: &std::path::Path, respond: impl FnOnce(Value) -> Value + Send + 'static) {
        let sock_path = dir.join("duduclaw-shell.sock");
        let listener = UnixListener::bind(&sock_path).expect("bind stub socket");
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = String::new();
                {
                    let mut reader = BufReader::new(&mut stream);
                    let _ = reader.read_line(&mut buf).await;
                }
                let req: Value = serde_json::from_str(buf.trim()).unwrap_or(Value::Null);
                let resp = respond(req);
                let mut line = serde_json::to_string(&resp).unwrap();
                line.push('\n');
                let _ = stream.write_all(line.as_bytes()).await;
            }
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cursor_size_set_round_trips_against_a_stub_server() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        run_stub(dir.path(), |req| {
            assert_eq!(req["op"], "set_cursor_size");
            assert_eq!(req["params"]["size"], 48);
            json!({ "ok": true, "cursor": { "size": 48, "effective_size": 48 } })
        })
        .await;

        // SAFETY: single-threaded current_thread test, no concurrent env
        // readers within this test's lifetime.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let result = cursor_size_set(48).await;
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let out = result.expect("stub round trip must succeed");
        assert!(out.contains("48"), "unexpected: {out}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unauthorized_response_is_translated_to_the_uid_boundary_message() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        run_stub(dir.path(), |_req| json!({ "ok": false, "error": "unauthorized" })).await;

        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let result = theme_set("dark").await;
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let err = result.expect_err("unauthorized must surface as an Err");
        assert!(err.contains("SO_PEERCRED"), "unexpected message: {err}");
        assert!(err.contains("duduclaw-kiosk"), "unexpected message: {err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_socket_file_gives_an_honest_not_found_message_not_a_hang() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // Deliberately never bind a listener — the socket file never exists.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let result = cursor_size_get().await;
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let err = result.expect_err("missing socket must be a clean Err, never a hang");
        assert!(err.contains("不存在"), "unexpected message: {err}");
        assert!(err.contains("duduclaw-kiosk"), "unexpected message: {err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_xdg_runtime_dir_is_reported_honestly() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        let result = cursor_size_get().await;
        let err = result.expect_err("no XDG_RUNTIME_DIR must be an honest Err");
        assert!(err.contains("XDG_RUNTIME_DIR"), "unexpected message: {err}");
    }
}
