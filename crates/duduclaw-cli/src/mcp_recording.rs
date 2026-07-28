//! Recording capture layer (WP3.3 R1/R3) — browser + desktop recording MCP
//! tool handlers.
//!
//! Browser recordings drive a real Playwright context (tracing + HAR + an
//! injected action recorder) through a small Node driver script materialized
//! into the recording directory. Desktop recordings run a detached
//! `duduclaw desktop-record-worker` subprocess (1 fps screenshots + foreground
//! window titles). Artifacts land under `~/.duduclaw/recordings/<id>/` with
//! 700 directory permissions; the HAR is redacted in place at stop time via
//! [`crate::mcp_recording_distill::redact_har`].
//!
//! Security posture: every tool here is deny-by-default — the dispatch gate
//! (`mcp_dispatch.rs`) requires the calling agent's
//! `[capabilities] recording = true` AND `Scope::Recording`, fail-closed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

// ── Constants ────────────────────────────────────────────────────────────────

/// Hard runaway cap for a recording session (circuit breaker — a forgotten
/// recording must not run unbounded).
const DEFAULT_MAX_SECONDS: u64 = 1800;
const MAX_MAX_SECONDS: u64 = 7200;

/// How long `*_record_stop` waits for the worker/driver to flush artifacts.
const STOP_WAIT: Duration = Duration::from_secs(30);
const STOP_POLL: Duration = Duration::from_millis(500);

/// HAR files above this size are not parsed in-process (protects the MCP
/// server from OOM on pathological recordings).
pub(crate) const MAX_HAR_BYTES: u64 = 50 * 1024 * 1024;

// ── Paths / ids ──────────────────────────────────────────────────────────────

pub(crate) fn recordings_root(home_dir: &Path) -> PathBuf {
    home_dir.join("recordings")
}

pub(crate) fn recording_dir(home_dir: &Path, id: &str) -> PathBuf {
    recordings_root(home_dir).join(id)
}

/// Recording ids are gateway-generated: `rec-<14 digit timestamp>-<6 hex>`.
/// Validation is strict (fail-closed) because the id is used as a path
/// component — anything else is rejected before touching the filesystem.
pub(crate) fn is_valid_recording_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("rec-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    if parts.len() != 2 {
        return false;
    }
    parts[0].len() == 14
        && parts[0].chars().all(|c| c.is_ascii_digit())
        && parts[1].len() == 6
        && parts[1].chars().all(|c| c.is_ascii_hexdigit())
}

fn new_recording_id() -> String {
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let hex: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect();
    format!("rec-{ts}-{hex}")
}

/// Restrict a path to owner-only access (700 dirs / 600 files). No-op off
/// Unix — Windows ACLs are inherited from the profile directory.
pub(crate) fn set_owner_only(path: &Path, is_dir: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_dir { 0o700 } else { 0o600 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, is_dir);
    }
}

// ── Recording metadata ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RecordingMeta {
    pub id: String,
    /// "browser" | "desktop"
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    pub agent: String,
    #[serde(default)]
    pub pid: Option<u32>,
    pub started_at: String,
}

pub(crate) fn read_meta(dir: &Path) -> Result<RecordingMeta, String> {
    let raw = std::fs::read_to_string(dir.join("meta.json"))
        .map_err(|e| format!("讀取 meta.json 失敗：{e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("meta.json 格式錯誤：{e}"))
}

// ── Tool result helpers (same JSON shape as mcp.rs) ─────────────────────────

fn rec_text(text: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": text}] })
}

fn rec_error(msg: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": msg}], "isError": true })
}

// ── Detached child registry (reap opportunistically on stop) ────────────────

fn child_registry() -> &'static Mutex<HashMap<String, std::process::Child>> {
    static REG: OnceLock<Mutex<HashMap<String, std::process::Child>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_child(id: &str, child: std::process::Child) {
    if let Ok(mut reg) = child_registry().lock() {
        reg.insert(id.to_string(), child);
    }
}

/// Reap the child if this process spawned it (avoids zombies). Best-effort.
fn reap_child(id: &str) {
    let child = child_registry().lock().ok().and_then(|mut r| r.remove(id));
    if let Some(mut c) = child {
        // The driver exits right after writing done.json; give it a moment.
        for _ in 0..10 {
            match c.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(_) => return,
            }
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

// ── Node / Playwright resolution ─────────────────────────────────────────────

/// Pick the newest `node_modules` root (from `roots`) that contains a
/// `playwright` package directory. Pure — unit-testable with temp dirs.
pub(crate) fn find_playwright_in_roots(roots: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for root in roots {
        let pw = root.join("playwright");
        if pw.is_dir() {
            let mtime = std::fs::metadata(&pw)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, root.clone()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Resolve a `node_modules` directory that contains the `playwright` package.
/// Order: explicit `DUDUCLAW_PLAYWRIGHT_NODE_PATH` env → `npm root -g` →
/// cached `~/.npm/_npx/*/node_modules` installs (newest wins).
async fn resolve_playwright_module_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("DUDUCLAW_PLAYWRIGHT_NODE_PATH") {
        let p = duduclaw_core::expand_tilde(raw.trim());
        if p.join("playwright").is_dir() {
            return Some(p);
        }
        warn!(path = %p.display(), "DUDUCLAW_PLAYWRIGHT_NODE_PATH set but contains no playwright/ — ignoring");
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(out) = tokio::process::Command::new("npm")
        .args(["root", "-g"])
        .output()
        .await
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                roots.push(PathBuf::from(p));
            }
        }
    }
    let npx_cache = duduclaw_core::expand_tilde("~/.npm/_npx");
    if let Ok(rd) = std::fs::read_dir(&npx_cache) {
        for entry in rd.flatten() {
            roots.push(entry.path().join("node_modules"));
        }
    }
    find_playwright_in_roots(&roots)
}

/// Locate a `node` binary. Launchd-launched gateways don't inherit a shell
/// PATH, so probe the common install locations too (same spirit as
/// `which_claude`).
fn resolve_node_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DUDUCLAW_NODE") {
        let pb = duduclaw_core::expand_tilde(p.trim());
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("node");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    // NVM: newest version dir wins.
    let nvm = duduclaw_core::expand_tilde("~/.nvm/versions/node");
    if let Ok(rd) = std::fs::read_dir(&nvm) {
        let mut vers: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        vers.sort();
        for v in vers.iter().rev() {
            let cand = v.join("bin").join("node");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for fixed in ["/opt/homebrew/bin/node", "/usr/local/bin/node"] {
        let p = PathBuf::from(fixed);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

// ── Browser driver (CommonJS so NODE_PATH-based resolution works) ───────────

/// The Node driver script materialized into each browser recording directory.
/// CJS on purpose: `require()` honours `NODE_PATH`, ES `import` does not.
const BROWSER_DRIVER_CJS: &str = r##"'use strict';
// DuDuClaw browser recording driver (WP3.3 R1). Auto-generated — do not edit.
const fs = require('fs');
const cfg = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
let chromium;
try { ({ chromium } = require('playwright')); }
catch (e) {
  fs.writeFileSync(cfg.done_path, JSON.stringify({ error: 'playwright module not found: ' + String(e).slice(0, 300) }));
  process.exit(3);
}
const actions = [];
const record = (a) => { if (actions.length < 5000) actions.push(Object.assign({ ts: new Date().toISOString() }, a)); };
let finishing = false;
async function main() {
  const browser = await chromium.launch({ headless: cfg.headless === true });
  const context = await browser.newContext({ recordHar: { path: cfg.har_path } });
  await context.tracing.start({ screenshots: true, snapshots: true });
  const finish = async (reason) => {
    if (finishing) return;
    finishing = true;
    try { await context.tracing.stop({ path: cfg.trace_path }); } catch (e) {}
    try { await context.close(); } catch (e) {}
    try { await browser.close(); } catch (e) {}
    try { fs.writeFileSync(cfg.actions_path, JSON.stringify({ actions: actions }, null, 2)); } catch (e) {}
    try { fs.writeFileSync(cfg.done_path, JSON.stringify({ ended_at: new Date().toISOString(), reason: reason, actions: actions.length })); } catch (e) {}
    process.exit(0);
  };
  await context.exposeBinding('__ddcRecord', (_src, a) => { if (a && typeof a === 'object') record(a); });
  await context.addInitScript(() => {
    const selOf = (el) => {
      try {
        if (!el) return '?';
        if (el.id) return '#' + el.id;
        const tag = (el.tagName || '?').toLowerCase();
        const nm = el.getAttribute && el.getAttribute('name');
        return nm ? tag + '[name=' + nm + ']' : tag;
      } catch (e) { return '?'; }
    };
    const txtOf = (el) => { try { return String(el.innerText || '').trim().slice(0, 80); } catch (e) { return ''; } };
    // Typed VALUES are masked by default — the redacted HAR carries the data
    // plane; the action log only needs "a text field was filled".
    window.addEventListener('click', (e) => { try { window.__ddcRecord({ kind: 'click', selector: selOf(e.target), text: txtOf(e.target) }); } catch (err) {} }, true);
    window.addEventListener('change', (e) => { try { window.__ddcRecord({ kind: 'fill', selector: selOf(e.target), value: '<masked>' }); } catch (err) {} }, true);
    window.addEventListener('submit', (e) => { try { window.__ddcRecord({ kind: 'submit', selector: selOf(e.target) }); } catch (err) {} }, true);
  });
  context.on('page', (page) => {
    page.on('framenavigated', (frame) => {
      try { if (frame === page.mainFrame()) record({ kind: 'goto', url: frame.url() }); } catch (e) {}
    });
    page.on('close', () => {
      setTimeout(() => {
        try { if ((context.pages() || []).length === 0) finish('all_pages_closed'); } catch (e) { finish('page_closed'); }
      }, 300);
    });
  });
  browser.on('disconnected', () => {
    if (finishing) return;
    try { fs.writeFileSync(cfg.actions_path, JSON.stringify({ actions: actions }, null, 2)); } catch (e) {}
    try { fs.writeFileSync(cfg.done_path, JSON.stringify({ ended_at: new Date().toISOString(), reason: 'browser_disconnected', actions: actions.length })); } catch (e) {}
    process.exit(0);
  });
  const page = await context.newPage();
  try { await page.goto(cfg.url, { waitUntil: 'domcontentloaded', timeout: 60000 }); }
  catch (e) { record({ kind: 'error', message: String(e).slice(0, 300) }); }
  const poll = setInterval(() => {
    try { if (fs.existsSync(cfg.stop_signal)) { clearInterval(poll); finish('stop_signal'); } } catch (e) {}
  }, 500);
  setTimeout(() => { clearInterval(poll); finish('max_seconds'); }, Math.max(30, cfg.max_seconds | 0) * 1000);
}
main().catch((e) => {
  try { fs.writeFileSync(cfg.done_path, JSON.stringify({ error: String(e).slice(0, 500) })); } catch (err) {}
  process.exit(2);
});
"##;

// ── browser_record_start ─────────────────────────────────────────────────────

pub(crate) async fn handle_browser_record_start(
    args: &Value,
    home_dir: &Path,
    agent: &str,
) -> Value {
    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("").trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return rec_error("browser_record_start 需要 http(s):// 開頭的 `url`。");
    }
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("recording")
        .trim();
    let headless = args
        .get("headless")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_seconds = args
        .get("max_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_SECONDS)
        .clamp(30, MAX_MAX_SECONDS);

    let Some(node) = resolve_node_binary() else {
        return rec_error(
            "找不到 node 執行檔。瀏覽器錄製需要 Node.js（可設環境變數 DUDUCLAW_NODE 指定路徑）。",
        );
    };
    let Some(module_root) = resolve_playwright_module_root().await else {
        return rec_error(
            "找不到 playwright Node 模組。請先安裝：`npm install -g playwright && npx playwright install chromium`，\
             或設 DUDUCLAW_PLAYWRIGHT_NODE_PATH 指向含 playwright 的 node_modules 目錄。",
        );
    };

    let id = new_recording_id();
    let dir = recording_dir(home_dir, &id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return rec_error(&format!("建立錄製目錄失敗：{e}"));
    }
    set_owner_only(&recordings_root(home_dir), true);
    set_owner_only(&dir, true);

    let driver_path = dir.join("driver.cjs");
    if let Err(e) = std::fs::write(&driver_path, BROWSER_DRIVER_CJS) {
        return rec_error(&format!("寫入 driver 失敗：{e}"));
    }
    let config = serde_json::json!({
        "url": url,
        "har_path": dir.join("session.har"),
        "trace_path": dir.join("trace.zip"),
        "actions_path": dir.join("actions.json"),
        "stop_signal": dir.join("stop.signal"),
        "done_path": dir.join("done.json"),
        "headless": headless,
        "max_seconds": max_seconds,
    });
    let config_path = dir.join("driver-config.json");
    if let Err(e) = std::fs::write(&config_path, config.to_string()) {
        return rec_error(&format!("寫入 driver 設定失敗：{e}"));
    }

    let log_path = dir.join("driver.log");
    let log = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => return rec_error(&format!("建立 driver.log 失敗：{e}")),
    };
    let log_err = match log.try_clone() {
        Ok(f) => f,
        Err(e) => return rec_error(&format!("建立 driver.log 失敗：{e}")),
    };

    let mut cmd = std::process::Command::new(&node);
    cmd.arg(&driver_path)
        .arg(&config_path)
        .env("NODE_PATH", &module_root)
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return rec_error(&format!("啟動錄製 driver 失敗：{e}")),
    };
    let pid = child.id();
    register_child(&id, child);

    let meta = RecordingMeta {
        id: id.clone(),
        kind: "browser".to_string(),
        name: name.to_string(),
        url: Some(url.to_string()),
        agent: agent.to_string(),
        pid: Some(pid),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let meta_path = dir.join("meta.json");
    if let Err(e) = std::fs::write(
        &meta_path,
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    ) {
        return rec_error(&format!("寫入 meta.json 失敗：{e}"));
    }
    set_owner_only(&meta_path, false);

    info!(recording = %id, url = %url, agent = %agent, "browser recording started");
    rec_text(&format!(
        "🔴 已開始瀏覽器錄製（id：{id}）。\n\
         已開啟{}瀏覽器並帶 tracing + HAR 錄製，請在視窗中操作要示範的流程。\n\
         完成後呼叫 browser_record_stop(id=\"{id}\")，或直接關閉瀏覽器視窗。\n\
         安全上限：{max_seconds} 秒後自動停止。錄製檔會落在 ~/.duduclaw/recordings/{id}/（權限 700，HAR 停止時自動脫敏）。",
        if headless { "無頭" } else { "" }
    ))
}

// ── stop shared helper ───────────────────────────────────────────────────────

/// Signal the recorder to stop and wait for `done.json`. Returns the parsed
/// done payload (or an error after killing a hung recorder).
async fn request_stop_and_wait(dir: &Path, meta: &RecordingMeta) -> Result<Value, String> {
    let done_path = dir.join("done.json");
    if !done_path.exists() {
        let _ = std::fs::write(dir.join("stop.signal"), b"stop");
        let deadline = std::time::Instant::now() + STOP_WAIT;
        while !done_path.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(STOP_POLL).await;
        }
        if !done_path.exists() {
            // Hung recorder: terminate the process group leader.
            #[cfg(unix)]
            if let Some(pid) = meta.pid {
                let _ = tokio::process::Command::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .output()
                    .await;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    reap_child(&meta.id);
    if done_path.exists() {
        let raw = std::fs::read_to_string(&done_path)
            .map_err(|e| format!("讀取 done.json 失敗：{e}"))?;
        Ok(serde_json::from_str(&raw).unwrap_or(Value::Null))
    } else {
        Err("錄製程序未在時限內結束（已強制終止）。部分成品可能未寫出。".to_string())
    }
}

/// Validate an id argument and resolve the recording directory (must exist).
fn resolve_existing_recording(args: &Value, home_dir: &Path) -> Result<(String, PathBuf), Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !is_valid_recording_id(&id) {
        return Err(rec_error("無效的錄製 id（格式：rec-<時間戳>-<hex>）。"));
    }
    let dir = recording_dir(home_dir, &id);
    if !dir.is_dir() {
        return Err(rec_error(&format!("找不到錄製「{id}」。")));
    }
    Ok((id, dir))
}

// ── browser_record_stop ──────────────────────────────────────────────────────

pub(crate) async fn handle_browser_record_stop(args: &Value, home_dir: &Path) -> Value {
    let (id, dir) = match resolve_existing_recording(args, home_dir) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let meta = match read_meta(&dir) {
        Ok(m) => m,
        Err(e) => return rec_error(&e),
    };
    if meta.kind != "browser" {
        return rec_error(&format!(
            "錄製「{id}」不是瀏覽器錄製（kind={}），請改用 desktop_record_stop。",
            meta.kind
        ));
    }

    let done = match request_stop_and_wait(&dir, &meta).await {
        Ok(v) => v,
        Err(e) => return rec_error(&e),
    };
    if let Some(err) = done.get("error").and_then(|v| v.as_str()) {
        return rec_error(&format!("錄製 driver 回報錯誤：{err}"));
    }

    // ── HAR redaction (in place, atomic) ────────────────────────────────────
    let har_path = dir.join("session.har");
    let mut redaction_note = String::from("HAR：不存在（可能未產生任何流量）");
    if har_path.is_file() {
        let size = std::fs::metadata(&har_path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_HAR_BYTES {
            redaction_note = format!(
                "HAR：{size} bytes 超過 {MAX_HAR_BYTES} 上限，未脫敏——請勿直接分享此檔"
            );
        } else {
            match std::fs::read_to_string(&har_path) {
                Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                    Ok(mut har) => {
                        let summary = crate::mcp_recording_distill::redact_har(&mut har);
                        let tmp = dir.join("session.har.tmp");
                        let write_ok = std::fs::write(&tmp, har.to_string())
                            .and_then(|()| std::fs::rename(&tmp, &har_path));
                        match write_ok {
                            Ok(()) => {
                                set_owner_only(&har_path, false);
                                redaction_note = format!(
                                    "HAR 已脫敏（headers {}、cookies {}、query {}、body 欄位 {} 處已替換為 <env:VAR>）",
                                    summary.headers, summary.cookies, summary.query_params, summary.body_fields
                                );
                            }
                            Err(e) => {
                                redaction_note =
                                    format!("HAR 脫敏後寫回失敗：{e}——請勿直接分享此檔");
                            }
                        }
                    }
                    Err(e) => redaction_note = format!("HAR 解析失敗（{e}），未脫敏"),
                },
                Err(e) => redaction_note = format!("HAR 讀取失敗（{e}）"),
            }
        }
    }

    let has = |f: &str| dir.join(f).is_file();
    let actions_count = std::fs::read_to_string(dir.join("actions.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("actions").and_then(|a| a.as_array()).map(|a| a.len()))
        .unwrap_or(0);

    info!(recording = %id, "browser recording stopped");
    rec_text(&format!(
        "⏹ 瀏覽器錄製「{id}」已停止。\n\
         成品：trace.zip {}、session.har {}、actions.json {}（{actions_count} 個操作事件）。\n\
         {redaction_note}。\n\
         下一步：呼叫 skill_from_recording(id=\"{id}\") 蒸餾成 SKILL.md 草稿（需管理員審批後才會安裝）。",
        if has("trace.zip") { "✅" } else { "❌" },
        if has("session.har") { "✅" } else { "❌" },
        if has("actions.json") { "✅" } else { "❌" },
    ))
}

// ── desktop_record_start / stop (R3, degraded: screenshots + window titles) ──

pub(crate) async fn handle_desktop_record_start(
    args: &Value,
    home_dir: &Path,
    agent: &str,
) -> Value {
    if !cfg!(target_os = "macos") {
        return rec_error(
            "桌面錄製目前僅支援 macOS（使用系統 screencapture）。Linux/Windows 版本尚未提供。",
        );
    }
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("desktop-recording")
        .trim();
    let max_seconds = args
        .get("max_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_SECONDS)
        .clamp(30, MAX_MAX_SECONDS);

    let id = new_recording_id();
    let dir = recording_dir(home_dir, &id);
    let desktop_dir = dir.join("desktop");
    if let Err(e) = std::fs::create_dir_all(desktop_dir.join("frames")) {
        return rec_error(&format!("建立錄製目錄失敗：{e}"));
    }
    set_owner_only(&recordings_root(home_dir), true);
    set_owner_only(&dir, true);
    set_owner_only(&desktop_dir, true);

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return rec_error(&format!("找不到 duduclaw 執行檔：{e}")),
    };
    let log_path = dir.join("worker.log");
    let log = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => return rec_error(&format!("建立 worker.log 失敗：{e}")),
    };
    let log_err = match log.try_clone() {
        Ok(f) => f,
        Err(e) => return rec_error(&format!("建立 worker.log 失敗：{e}")),
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("desktop-record-worker")
        .arg("--dir")
        .arg(&dir)
        .arg("--max-seconds")
        .arg(max_seconds.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return rec_error(&format!("啟動桌面錄製 worker 失敗：{e}")),
    };
    let pid = child.id();
    register_child(&id, child);

    let meta = RecordingMeta {
        id: id.clone(),
        kind: "desktop".to_string(),
        name: name.to_string(),
        url: None,
        agent: agent.to_string(),
        pid: Some(pid),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    ) {
        return rec_error(&format!("寫入 meta.json 失敗：{e}"));
    }

    // Explicit start signal (design §3: never record silently). The desktop-pet
    // sign-holding hookup is a listed TODO; for now the signal is this log +
    // the returned message the agent relays to the user.
    info!(recording = %id, agent = %agent, "desktop recording STARTED — visible-signal hookup (pet placard) is a TODO");
    rec_text(&format!(
        "🔴 已開始桌面錄製（id：{id}）——每秒截圖＋前景視窗標題（輸入內容一律不記錄）。\n\
         請直接示範要教的操作流程；完成後呼叫 desktop_record_stop(id=\"{id}\")。\n\
         安全上限：{max_seconds} 秒後自動停止。若截圖持續失敗，多半是 macOS「螢幕錄製」權限未授予，\n\
         請到 系統設定 → 隱私權與安全性 → 螢幕錄製 允許 duduclaw。"
    ))
}

pub(crate) async fn handle_desktop_record_stop(args: &Value, home_dir: &Path) -> Value {
    let (id, dir) = match resolve_existing_recording(args, home_dir) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let meta = match read_meta(&dir) {
        Ok(m) => m,
        Err(e) => return rec_error(&e),
    };
    if meta.kind != "desktop" {
        return rec_error(&format!(
            "錄製「{id}」不是桌面錄製（kind={}），請改用 browser_record_stop。",
            meta.kind
        ));
    }
    let done = match request_stop_and_wait(&dir, &meta).await {
        Ok(v) => v,
        Err(e) => return rec_error(&e),
    };
    if let Some(err) = done.get("error").and_then(|v| v.as_str()) {
        return rec_error(&format!("桌面錄製 worker 回報錯誤：{err}"));
    }
    let frames = done.get("frames").and_then(|v| v.as_u64()).unwrap_or(0);
    let events = std::fs::read_to_string(dir.join("desktop").join("events.jsonl"))
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    info!(recording = %id, frames, "desktop recording stopped");
    rec_text(&format!(
        "⏹ 桌面錄製「{id}」已停止：{frames} 張截圖、{events} 筆事件（desktop/events.jsonl）。\n\
         下一步：呼叫 skill_from_recording(id=\"{id}\") 蒸餾成 desktop-sop SKILL.md 草稿（需管理員審批後才會安裝）。"
    ))
}

// ── Desktop record worker loop (runs as `duduclaw desktop-record-worker`) ────

#[derive(Serialize)]
struct DesktopEvent<'a> {
    seq: u64,
    ts: String,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame: Option<String>,
    app: &'a str,
    window_title: &'a str,
}

/// The worker body for `duduclaw desktop-record-worker --dir <recording dir>`.
/// 1 fps screenshots (macOS `screencapture`) + foreground window titles into
/// `<dir>/desktop/`. Input-event capture (rdev) is deliberately deferred —
/// see the WP3.3 design addendum (DEGRADED scope). Returns a process exit
/// code.
pub async fn run_desktop_record_worker(dir: PathBuf, interval_ms: u64, max_seconds: u64) -> i32 {
    let desktop_dir = dir.join("desktop");
    let frames_dir = desktop_dir.join("frames");
    if std::fs::create_dir_all(&frames_dir).is_err() {
        return 2;
    }
    let events_path = desktop_dir.join("events.jsonl");
    let done_path = dir.join("done.json");
    let stop_signal = dir.join("stop.signal");
    let started = std::time::Instant::now();
    let interval = Duration::from_millis(interval_ms.clamp(200, 10_000));
    let mut seq: u64 = 0;
    let mut frames: u64 = 0;
    let mut consecutive_failures = 0u32;
    let mut last_window = String::new();
    let reason;

    let mut events = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)
    {
        Ok(f) => f,
        Err(_) => return 2,
    };
    set_owner_only(&events_path, false);

    loop {
        if stop_signal.exists() {
            reason = "stop_signal";
            break;
        }
        if started.elapsed().as_secs() >= max_seconds {
            reason = "max_seconds";
            break;
        }
        seq += 1;
        let ts = chrono::Utc::now().to_rfc3339();

        // Screenshot (macOS only — the start handler already gated the OS).
        let frame_rel = format!("frames/{seq:06}.jpg");
        let frame_path = desktop_dir.join(&frame_rel);
        let shot_ok = tokio::process::Command::new("screencapture")
            .args(["-x", "-t", "jpg"])
            .arg(&frame_path)
            .output()
            .await
            .map(|o| o.status.success() && frame_path.is_file())
            .unwrap_or(false);
        if shot_ok {
            frames += 1;
            consecutive_failures = 0;
            set_owner_only(&frame_path, false);
        } else {
            consecutive_failures += 1;
            if consecutive_failures >= 3 {
                reason = "screenshot_failed";
                break;
            }
        }

        // Foreground app / window title.
        let (app, title) = match duduclaw_os::frontmost_info().await {
            Ok(i) => (i.app, i.window_title),
            Err(_) => (String::new(), String::new()),
        };
        let window_key = format!("{app}\u{1}{title}");
        let kind = if window_key != last_window {
            last_window = window_key;
            "window_change"
        } else {
            "frame"
        };
        let ev = DesktopEvent {
            seq,
            ts,
            kind,
            frame: shot_ok.then(|| frame_rel.clone()),
            app: &app,
            window_title: &title,
        };
        if let Ok(line) = serde_json::to_string(&ev) {
            use std::io::Write;
            let _ = writeln!(events, "{line}");
        }

        tokio::time::sleep(interval).await;
    }

    let done = serde_json::json!({
        "ended_at": chrono::Utc::now().to_rfc3339(),
        "reason": reason,
        "frames": frames,
        "error": if reason == "screenshot_failed" {
            Value::String(
                "連續截圖失敗——請確認 macOS 螢幕錄製權限（系統設定 → 隱私權與安全性 → 螢幕錄製）。"
                    .to_string(),
            )
        } else {
            Value::Null
        },
    });
    let _ = std::fs::write(&done_path, done.to_string());
    if reason == "screenshot_failed" {
        1
    } else {
        0
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_id_roundtrip_valid() {
        let id = new_recording_id();
        assert!(is_valid_recording_id(&id), "generated id must validate: {id}");
    }

    #[test]
    fn recording_id_rejects_path_traversal_and_garbage() {
        for bad in [
            "",
            "rec-",
            "../etc",
            "rec-20260728000000-..%2f",
            "rec-20260728000000-a1b2c3/..",
            "rec-2026-a1b2c3",
            "rec-20260728000000-a1b2c3-extra",
            "rec-20260728000000-A1B2G3x", // 7 chars + non-hex
            "rec-2026072800000x-a1b2c3",  // non-digit timestamp
            "notrec-20260728000000-a1b2c3",
        ] {
            assert!(!is_valid_recording_id(bad), "must reject: {bad}");
        }
        assert!(is_valid_recording_id("rec-20260728061500-a1b2c3"));
    }

    #[test]
    fn find_playwright_picks_a_root_that_has_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a/node_modules");
        let b = tmp.path().join("b/node_modules");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(b.join("playwright")).unwrap();
        let got = find_playwright_in_roots(&[a, b.clone()]);
        assert_eq!(got, Some(b));
    }

    #[test]
    fn find_playwright_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("node_modules");
        std::fs::create_dir_all(&a).unwrap();
        assert_eq!(find_playwright_in_roots(&[a]), None);
    }

    #[cfg(unix)]
    #[test]
    fn set_owner_only_applies_700_to_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let d = tmp.path().join("rec");
        std::fs::create_dir_all(&d).unwrap();
        set_owner_only(&d, true);
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[tokio::test]
    async fn stop_on_unknown_recording_is_tool_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let args = serde_json::json!({ "id": "rec-20260728000000-abc123" });
        let out = handle_browser_record_stop(&args, tmp.path()).await;
        assert_eq!(out["isError"], true);
    }

    #[tokio::test]
    async fn start_rejects_non_http_url() {
        let tmp = tempfile::TempDir::new().unwrap();
        let args = serde_json::json!({ "url": "file:///etc/passwd", "name": "x" });
        let out = handle_browser_record_start(&args, tmp.path(), "tester").await;
        assert_eq!(out["isError"], true);
    }

    /// Live spike (R4): records a real headless Playwright session against
    /// https://example.com through the actual start/stop handlers and asserts
    /// the artifact layout + HAR redaction. Requires a local `playwright` npm
    /// module + installed chromium, so it is `#[ignore]`d for CI; run with:
    /// `cargo test -p duduclaw-cli --lib -- --ignored live_browser_record`
    #[tokio::test]
    #[ignore]
    async fn live_browser_record_example_com() {
        let tmp = tempfile::TempDir::new().unwrap();
        let args = serde_json::json!({
            "url": "https://example.com",
            "name": "live-spike",
            "headless": true,
        });
        let out = handle_browser_record_start(&args, tmp.path(), "tester").await;
        assert_ne!(out["isError"], true, "start failed: {out}");
        let text = out["content"][0]["text"].as_str().unwrap();
        let id = text
            .split("id：")
            .nth(1)
            .and_then(|s| s.split(['）', '\n']).next())
            .expect("start reply must carry the recording id")
            .trim()
            .to_string();
        assert!(is_valid_recording_id(&id), "bad id in reply: {id}");

        // Give the driver time to launch chromium and load the page.
        tokio::time::sleep(Duration::from_secs(10)).await;

        let stop = handle_browser_record_stop(&serde_json::json!({ "id": id }), tmp.path()).await;
        let stop_text = stop["content"][0]["text"].as_str().unwrap_or("");
        assert_ne!(stop["isError"], true, "stop failed: {stop}");

        let dir = recording_dir(tmp.path(), &id);
        assert!(dir.join("trace.zip").is_file(), "trace.zip missing; log: {:?}; stop: {stop_text}",
            std::fs::read_to_string(dir.join("driver.log")).unwrap_or_default());
        assert!(dir.join("session.har").is_file(), "session.har missing");
        assert!(dir.join("actions.json").is_file(), "actions.json missing");
        let har: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("session.har")).unwrap(),
        )
        .unwrap();
        assert!(har.get("log").is_some(), "HAR must parse with a log root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "recording dir must be 700");
        }
    }

    #[tokio::test]
    async fn desktop_worker_writes_done_on_stop_signal() {
        // The worker must exit promptly on stop.signal and write done.json —
        // headless-verifiable even where screenshots fail (failure budget 3).
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("rec-20260728000000-abc123");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stop.signal"), b"stop").unwrap();
        let code = run_desktop_record_worker(dir.clone(), 200, 5).await;
        assert_eq!(code, 0);
        let done: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("done.json")).unwrap())
                .unwrap();
        assert_eq!(done["reason"], "stop_signal");
    }
}
