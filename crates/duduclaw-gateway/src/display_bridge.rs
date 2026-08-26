//! A7c: agent→display gateway bridge — the "gateway forwarding layer" that
//! makes A7a's `display` group (comp's `shell_control` socket: cursor
//! size/source, theme, output scale) reachable from an agent identity.
//!
//! Design authority: `commercial/docs/DESIGN-os-self-drive-2026-08.md` (A7a,
//! §7 — the uid-boundary finding this module closes) and this crate's
//! `mcp_os_ops.rs`-style "same capability, two front doors, same gate set"
//! convention (see that file's module doc in `duduclaw-cli`).
//!
//! ## What actually closes the uid gap
//! The boundary itself is fixed on the COMP side
//! (`duduclaw-comp/src/shell_control/listener.rs::classify_peer` — a new
//! `PeerAuthority::Agent` tier, unconditionally trusting root plus an
//! optionally configured second uid, restricted to
//! `ShellControlRequest::agent_allowed`'s closed verb subset: cursor size/
//! source, theme, output scale, and their read-only counterpart). THIS
//! module is only the client half: a plain, stateless, one-shot Unix-socket
//! JSON-line caller speaking the exact same wire protocol
//! `duduclaw-cli/src/os_drive/display.rs` already speaks. It is a SECOND,
//! independent hand-rolled client of that published shape — not a shared
//! implementation — for the same reason `os_drive/display.rs`'s own module
//! doc gives for not depending on `duduclaw-comp` directly: that crate pulls
//! in smithay (Linux-only), which would break this workspace's macOS dev
//! build, and comp's own `shell_control` module doc already frames the wire
//! JSON as "reuse only the pure data, never a shared type".
//!
//! ## Why this lives in `duduclaw-gateway`, not only in `os_drive`
//! `os_drive::display` resolves comp's socket via the CALLING process's own
//! `$XDG_RUNTIME_DIR` — correct for a human operator logged in AS the kiosk
//! session, structurally wrong for an agent CLI subprocess or the
//! standalone `duduclaw mcp-server` process (neither inherits the kiosk
//! session's runtime dir; A7a's design doc §7 documents exactly this gap).
//! This module instead tries the FIXED, deployment-known path
//! (`/run/duduclaw-kiosk/duduclaw-shell.sock` — the literal both
//! `appliance/mkosi.extra/etc/systemd/system/duduclaw-kiosk.service` and
//! the Yocto `meta-duduclaw/recipes-duduclaw/duduclaw-shell/files/
//! duduclaw-kiosk.service` pin via `Environment=XDG_RUNTIME_DIR=
//! /run/duduclaw-kiosk`), so it works from ANY process on the box
//! regardless of that process's own environment — exactly the "pure/
//! stateless, callable from any process" shape `device_about`/`device_ops`/
//! `network` already established for A7a's other two groups. `duduclaw-cli`
//! already depends on `duduclaw-gateway` (the `duduclaw run` subcommand IS
//! the gateway, "one binary wearing two hats"), so both `os_drive::display`
//! (as a fallback layered on top of its existing `$XDG_RUNTIME_DIR` path —
//! see that module for the exact ordering) and the new
//! `os_display_get`/`os_display_set` MCP tools (`mcp_os_ops.rs`) call the
//! SAME functions here with zero duplication of the fixed-path logic.
//!
//! ## Not appliance-gated internally
//! Unlike the `device.*`-backed O-0 tools, the functions here do NOT check
//! `duduclaw_core::is_appliance()` themselves — matching `os_drive::display`'s
//! existing behaviour, where an off-appliance caller simply gets an
//! honest "socket not found" error (there is no kiosk session, so the fixed
//! path never exists) rather than a redundant explicit gate. Callers that
//! want a faster, clearer refusal before ever touching a socket (the MCP
//! tool front door does) check `is_appliance()` themselves first — same
//! split responsibility `mcp_os_ops.rs`'s handlers already use for other
//! tools that layer a gate ON TOP OF a pure/stateless function rather than
//! inside it.
//!
//! ## Scope (matches A7a design doc §5 exactly — `requires_approval=false`)
//! Cursor size/source, comp's own decoration theme, and output scale (the
//! real backend "把字放大" drives — WP-comp-shell-display D4b-3). No
//! approval/confirm gate: these are the same appearance preferences A7a's
//! design doc already rated reversible and low-risk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// Same literal `duduclaw-comp/src/shell_control/protocol.rs::
/// SOCKET_FILE_NAME` uses — hand-duplicated for the same "don't depend on
/// duduclaw-comp" reason this module's own doc gives.
const SOCKET_FILE_NAME: &str = "duduclaw-shell.sock";

/// The kiosk session's fixed `$XDG_RUNTIME_DIR` on every shipping image —
/// see this module's doc for the two systemd units that pin this exact
/// value. A caller-supplied override exists only for tests
/// (`call_at`/`kiosk_socket_path_override` below); production callers
/// always go through [`display_get`]/[`display_set`], which use the
/// hardcoded default.
const KIOSK_RUNTIME_DIR: &str = "/run/duduclaw-kiosk";

/// Same bound `os_drive/display.rs::MAX_RESPONSE_LINE_BYTES` uses, same
/// reasoning: a sane upper bound for a reply this process reads into
/// memory, matching the wire protocol's own server-side request-line cap.
const MAX_RESPONSE_LINE_BYTES: usize = 4096;

/// Same reasoning as `os_drive/display.rs::RESPONSE_TIMEOUT`: every real
/// reply is a synchronous, in-process answer on comp's side, so this is
/// generous headroom over "instant", not a real operation timeout.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

fn kiosk_socket_path() -> PathBuf {
    Path::new(KIOSK_RUNTIME_DIR).join(SOCKET_FILE_NAME)
}

#[cfg(unix)]
async fn call_at(path: &Path, req: Value) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    if !path.exists() {
        return Err(format!(
            "shell socket 不存在：{}。comp/殼可能未啟動，或這台不是值班機（appliance）——A7c 的固定路徑\
             只在真正的 kiosk session 上存在，見 commercial/docs/DESIGN-os-self-drive-2026-08.md §7。",
            path.display()
        ));
    }

    let mut stream = tokio::time::timeout(RESPONSE_TIMEOUT, UnixStream::connect(path))
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
            "連線已建立但未收到任何回應（連線在寫入回應前就被關閉）。".to_string(),
        );
    }
    // Defense-in-depth: a well-behaved server never sends a line anywhere
    // near this size — same bound the server enforces on OUR request.
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
                "shell socket 拒絕連線（unauthorized）——comp 尚未認得這個呼叫者的 uid。見 A7c 設計：\
                 comp 端需要以 root 執行（Yocto 值班機開機階段的現況），或設定 \
                 DUDUCLAW_SHELL_CONTROL_AGENT_UID 明確信任 gateway 的 uid。"
                    .to_string(),
            );
        }
        if err == "forbidden_for_agent" {
            return Err(
                "shell socket 拒絕這個動作（forbidden_for_agent）——這個 op 不在 A7c 的 agent 白名單內\
                 （只開放 cursor/theme/output-scale 這幾個外觀偏好）。"
                    .to_string(),
            );
        }
        return Err(format!("shell_control 回報錯誤：{err}"));
    }

    Ok(resp)
}

#[cfg(not(unix))]
async fn call_at(_path: &Path, _req: Value) -> Result<Value, String> {
    Err("display 組僅支援 Unix（Linux/macOS）——這個平台沒有 shell_control socket。".to_string())
}

async fn call(req: Value) -> Result<Value, String> {
    call_at(&kiosk_socket_path(), req).await
}

// ── Granular ops (mirror `os_drive/display.rs`'s wire calls exactly) ──────

pub async fn cursor_size_set(size: i64) -> Result<Value, String> {
    let resp = call(json!({ "op": "set_cursor_size", "params": { "size": size } })).await?;
    Ok(resp.get("cursor").cloned().unwrap_or(Value::Null))
}

pub async fn cursor_source_get() -> Result<Value, String> {
    let resp = call(json!({ "op": "get_cursor_source" })).await?;
    Ok(resp.get("cursor").cloned().unwrap_or(Value::Null))
}

pub async fn cursor_source_set(source: &str) -> Result<Value, String> {
    let resp = call(json!({ "op": "set_cursor_source", "params": { "source": source } })).await?;
    Ok(resp.get("cursor").cloned().unwrap_or(Value::Null))
}

pub async fn theme_set(theme: &str) -> Result<Value, String> {
    call(json!({ "op": "set_theme", "params": { "theme": theme } })).await?;
    Ok(json!({ "theme": theme }))
}

async fn outputs_get() -> Result<Vec<Value>, String> {
    let resp = call(json!({ "op": "get_outputs" })).await?;
    Ok(resp.get("outputs").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// The primary (first non-shadow) output's name — comp's `get_outputs`
/// already excludes the CD-2 shadow workspace, so the first entry is always
/// a real, human-visible screen. `None` only if comp reports zero outputs
/// (unreachable in practice on a running compositor, but handled honestly
/// rather than assumed).
async fn primary_output_name() -> Result<String, String> {
    let outputs = outputs_get().await?;
    outputs
        .first()
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "comp 回報零個輸出（output）——沒有可調整縮放的螢幕。".to_string())
}

async fn output_scale_set_primary(scale_pct: i64) -> Result<Value, String> {
    let output = primary_output_name().await?;
    let resp = call(json!({
        "op": "set_output_scale",
        "params": { "output": output, "scale_pct": scale_pct }
    }))
    .await?;
    Ok(resp.get("outputs").cloned().unwrap_or(Value::Null))
}

// ── The `os_display_get`/`os_display_set` surface (MCP tool + CLI shape) ──

/// Closed set of fields `display_set` accepts — mirrors comp's own
/// `agent_allowed` allowlist exactly (`cursor_size`/`cursor_source`/
/// `theme`/`output_scale`), never a free-form op name.
pub const DISPLAY_SET_FIELDS: [&str; 4] = ["cursor_size", "cursor_source", "theme", "output_scale"];

/// `os_display_get` — a snapshot of everything `display_set` can change:
/// cursor size/source (+ effective size, theme, persistence), and the
/// primary output's scale — what "現況：字級 100%" needs to answer.
pub async fn display_get() -> Result<Value, String> {
    let cursor = cursor_source_get().await?;
    let outputs = outputs_get().await?;
    let primary_scale_pct = outputs.first().and_then(|o| o.get("scale_pct")).cloned();
    let primary_output = outputs.first().and_then(|o| o.get("name")).cloned();
    Ok(json!({
        "cursor": cursor,
        "output": primary_output,
        "output_scale_pct": primary_scale_pct,
    }))
}

/// `os_display_set` — dispatches on `field` (one of [`DISPLAY_SET_FIELDS`])
/// against `value` (always a string on the wire; parsed per-field). Unknown
/// fields and unparseable values are refused HERE, before any socket call —
/// comp's own `agent_allowed`/`validate` are the second, authoritative gate,
/// never trusted to be the only one (defense in depth, coding convention
/// #4: fail closed on the caller-visible side too).
pub async fn display_set(field: &str, value: &str) -> Result<Value, String> {
    match field {
        "cursor_size" => {
            let size: i64 = value
                .trim()
                .parse()
                .map_err(|_| format!("cursor_size 必須是整數，收到：{value:?}"))?;
            cursor_size_set(size).await
        }
        "cursor_source" => cursor_source_set(value).await,
        "theme" => theme_set(value).await,
        "output_scale" => {
            let scale_pct: i64 = value
                .trim()
                .parse()
                .map_err(|_| format!("output_scale 必須是整數（百分比），收到：{value:?}"))?;
            output_scale_set_primary(scale_pct).await
        }
        other => Err(format!(
            "未知的 display 欄位：{other:?}（合法值：{}）",
            DISPLAY_SET_FIELDS.join(", ")
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// Spin up a stub `shell_control`-shaped server on a temp socket and
    /// call `call_at` directly against it — proves the request/response
    /// wire round trip works without needing a real compositor or the
    /// `$XDG_RUNTIME_DIR`-based env-var plumbing `os_drive/display.rs`'s own
    /// equivalent test relies on (this module never reads that env var at
    /// all, so no cross-test env-lock is needed here).
    async fn run_stub(path: &std::path::Path, respond: impl FnOnce(Value) -> Value + Send + 'static) {
        let listener = UnixListener::bind(path).expect("bind stub socket");
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

    fn temp_sock(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("duduclaw-display-bridge-test-{tag}-{}.sock", std::process::id()))
    }

    #[tokio::test]
    async fn missing_socket_is_an_honest_not_found_error_not_a_hang() {
        let path = temp_sock("missing");
        let _ = std::fs::remove_file(&path);
        let err = call_at(&path, json!({ "op": "get_outputs" })).await.unwrap_err();
        assert!(err.contains("不存在"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn cursor_size_set_round_trips_against_a_stub_server() {
        let path = temp_sock("cursorsize");
        let _ = std::fs::remove_file(&path);
        run_stub(&path, |req| {
            assert_eq!(req["op"], "set_cursor_size");
            assert_eq!(req["params"]["size"], 48);
            json!({ "ok": true, "cursor": { "size": 48, "effective_size": 48 } })
        })
        .await;

        let resp = call_at(&path, json!({ "op": "set_cursor_size", "params": { "size": 48 } }))
            .await
            .expect("stub round trip must succeed");
        assert_eq!(resp["cursor"]["size"], 48);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn unauthorized_error_is_translated_with_a7c_specific_guidance() {
        let path = temp_sock("unauth");
        let _ = std::fs::remove_file(&path);
        run_stub(&path, |_req| json!({ "ok": false, "error": "unauthorized" })).await;

        let err = call_at(&path, json!({ "op": "set_theme", "params": { "theme": "dark" } }))
            .await
            .unwrap_err();
        assert!(err.contains("DUDUCLAW_SHELL_CONTROL_AGENT_UID"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn forbidden_for_agent_error_is_translated_honestly() {
        let path = temp_sock("forbidden");
        let _ = std::fs::remove_file(&path);
        run_stub(&path, |_req| json!({ "ok": false, "error": "forbidden_for_agent" })).await;

        let err = call_at(&path, json!({ "op": "list_windows" })).await.unwrap_err();
        assert!(err.contains("forbidden_for_agent"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn display_set_rejects_an_unknown_field_before_touching_any_socket() {
        // No stub server bound at all — if this reached `call`, it would
        // fail with a connection error instead of the field-validation
        // error asserted below.
        let err = display_set("not_a_real_field", "1").await.unwrap_err();
        assert!(err.contains("未知的 display 欄位"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn display_set_rejects_a_non_integer_cursor_size_before_touching_any_socket() {
        let err = display_set("cursor_size", "big").await.unwrap_err();
        assert!(err.contains("cursor_size"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn display_set_rejects_a_non_integer_output_scale_before_touching_any_socket() {
        let err = display_set("output_scale", "huge").await.unwrap_err();
        assert!(err.contains("output_scale"), "unexpected: {err}");
    }
}
