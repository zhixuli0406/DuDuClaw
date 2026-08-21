// S5b1-A (2026-08-21) — "裝置" (`/app/system/device`, `nav.rs` id `device`).
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/Device.dc.
// html` — B5 boxed-list settings form: header → 狀態 tiles → 更新 summary
// row → 網路 table → 備份與還原 (own module, `device_backup.rs`) → 危險區
// (same module). This file owns the header/狀態/更新/網路 sections plus the
// shared `DeviceState` Global struct and fetch orchestration; `device_
// backup.rs` (a sibling, split for this crate's own <800-line file
// convention — same precedent `dashboard.rs`/`dashboard_cards.rs` and
// `goals.rs`/`goals_inspector.rs` already establish) owns 備份與還原 + 危險區.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `device.status` (`handle_device_status` ~L43978, dispatch gate
//     `require_admin!()` + `require_appliance!()` ~L6965) → the
//     `crate::device::collect_status` struct verbatim: `{ "cpu_cores",
//     "load_average": {"load1","load5","load15"}|null, "ram": {"total_mb",
//     "used_mb","available_mb"}|null, "disk": {"total_mb","used_mb",
//     "available_mb"}|null, "temperature_c"|null, "uptime_secs"|null,
//     "network_interfaces": [...] }`. Every optional field is a NULL
//     sensor reading, not an error — a missing row is dropped, never shown
//     as 0 (same rule `web/src/pages/DevicePage.tsx`'s own doc comment
//     states, functional reference only per this task's "版面禁抄 web").
//   `device.network` (`handle_device_network` ~L43991, same gates, ~L6970)
//     → `{ "interfaces": [ {"name","is_up","addresses":[...]} ] }`.
//   `device.factory_reset`/`device.power` (~L44023/44031, ~L7023/7029,
//     PLUS `require_confirm!()`) → `device_op_result_frame`'s shape:
//     `{ "success", "stdout", "stderr" }` on a normal `ok:true` response —
//     `success:false` is a real shell-command failure, not an RPC error, so
//     it must be read from the payload, never assumed from `ok:true` alone
//     (see `device_backup.rs`'s danger-zone dispatcher).
//   `system.check_update` (`handle_system_check_update` ~L19636, dispatch
//     gate `require_admin!()` ~L6127) → `{ "available", "current_version",
//     "latest_version", ... }` — same RPC `about.rs`'s own "更新中" line
//     already consumes; this page reads only the three fields above (the
//     full release-notes/install/history flow is `system_updates.rs`'s job,
//     not this page's — the canvas's own 更新 row is a one-line summary +
//     "前往系統更新" link, not a second update center).
//
// ── The `not_appliance` gate ──────────────────────────────────────────────
// Every `device.*` handler refuses with a structured `{"code":
// "not_appliance", "message": ...}` error off-appliance (`duduclaw_gateway::
// handlers::device_not_appliance_frame`, `DEVICE_NOT_APPLIANCE_ERROR_CODE`
// in `web/src/lib/api.ts`). `fetch_status` below is the ONE call whose raw
// `CallError` is inspected for that code (every other `device.*` call just
// degrades to a generic `Failed` row) — once `device.status` itself
// confirms "not an appliance", the whole page renders the honest refusal
// panel instead of firing four more calls that would only repeat the same
// answer, mirroring (not copying) the web page's own `notAppliance` gate.
//
// State lives behind `gpui::Global` — same pattern `dashboard.rs`/`goals.rs`
// already establish (no `main.rs` field).
//
// ── Deliberate degradations, documented rather than faked (see also
// `device_backup.rs`'s own header comment for the backup-section ones) ────
// Static-IP configuration (`device.network`'s write path) is refused
// server-side with `not_implemented` — this page renders that row as an
// inert "即將推出" label, matching the canvas exactly (it already shows the
// same wording as a static caption, not a live control).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

pub(super) const CONTENT_MAX_WIDTH: f32 = 700.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LoadAverage {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Clone)]
pub struct MemUsage {
    pub total_mb: u64,
    pub used_mb: u64,
}

#[derive(Clone)]
pub struct DeviceStatus {
    pub cpu_cores: u64,
    pub load_average: Option<LoadAverage>,
    pub ram: Option<MemUsage>,
    pub disk: Option<MemUsage>,
    pub temperature_c: Option<f64>,
    pub uptime_secs: Option<u64>,
}

#[derive(Clone)]
pub struct NetworkIface {
    pub name: String,
    pub is_up: bool,
    pub addresses: Vec<String>,
}

#[derive(Clone)]
pub struct DeviceUpdateSummary {
    pub current_version: String,
    pub available: bool,
    pub latest_version: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DangerAction {
    FactoryReset,
    Restart,
    Shutdown,
}

#[derive(Clone)]
pub(super) struct BackupSchedule {
    pub enabled: bool,
    pub interval_hours: u64,
    pub retention_count: u64,
}

#[derive(Clone)]
pub(super) struct BackupFile {
    pub name: String,
    pub size: u64,
    pub mtime_ms: i64,
}

/// Every field `device_backup.rs` needs lives here too — one `Global`, not
/// two, since both files render sections of the SAME page and several of
/// `device_backup.rs`'s actions (schedule save, backup create) need to
/// invalidate/refresh siblings this file owns (nothing does today, but
/// splitting the state would only add an extra `cx.global::<T>()` hop for
/// no isolation benefit — same one-struct-two-files precedent `goals.rs`/
/// `goals_inspector.rs` already establish for `GoalsState`).
pub struct DeviceState {
    requested: bool,
    pub not_appliance: bool,
    pub status: Loadable<DeviceStatus>,
    pub network: Loadable<Vec<NetworkIface>>,
    pub update: Loadable<DeviceUpdateSummary>,
    // `pub(super)`, not `pub`: `BackupSchedule`/`BackupFile` are themselves
    // `pub(super)` types (only ever consumed by `device.rs` + the sibling
    // `device_backup.rs`, both under `screens`) — a `pub` field of a wider-
    // visibility-than-its-type is a real rustc/clippy error
    // (`private_interfaces`), not a style nit.
    pub(super) schedule: Loadable<BackupSchedule>,
    pub(super) backups: Loadable<Vec<BackupFile>>,
    // ── Backup-schedule local draft (mirrors `web/src/pages/DevicePage.
    // tsx`'s own `draftEnabled`/`draftInterval`/`draftRetention` — nothing
    // is sent to the gateway until "儲存" is clicked). Plain integers, not a
    // `TextField` entity: the value only ever changes via bounded +/-
    // steppers (this page's own honest scope cut — see `device_backup.rs`'s
    // header comment on why free-text keyboard entry isn't wired here).
    pub draft_enabled: bool,
    pub draft_interval: u64,
    pub draft_retention: u64,
    pub schedule_saving: bool,
    pub schedule_save_result: Option<Result<(), String>>,
    // ── Backup create ──
    pub backup_creating: bool,
    pub backup_create_result: Option<Result<String, String>>,
    // ── Backup list row actions ──
    pub delete_armed: Option<String>,
    pub deleting_name: Option<String>,
    pub delete_result: Option<Result<(), String>>,
    // ── Danger zone (armed-then-confirm, no modal — see `device_backup.
    // rs`'s own doc comment on why) ── `pub(super)`: same private-type
    // reason as `schedule`/`backups` above (`DangerAction` is `pub(super)`).
    pub(super) danger_armed: Option<DangerAction>,
    pub danger_busy: bool,
    pub danger_result: Option<Result<(), String>>,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            requested: false,
            not_appliance: false,
            status: Loadable::Loading,
            network: Loadable::Loading,
            update: Loadable::Loading,
            schedule: Loadable::Loading,
            backups: Loadable::Loading,
            draft_enabled: false,
            draft_interval: 24,
            draft_retention: 7,
            schedule_saving: false,
            schedule_save_result: None,
            backup_creating: false,
            backup_create_result: None,
            delete_armed: None,
            deleting_name: None,
            delete_result: None,
            danger_armed: None,
            danger_busy: false,
            danger_result: None,
        }
    }
}

impl Global for DeviceState {}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_load_average(v: &Value) -> Option<LoadAverage> {
    let v = v.get("load_average")?;
    if v.is_null() {
        return None;
    }
    Some(LoadAverage {
        load1: v.get("load1").and_then(Value::as_f64).unwrap_or(0.0),
        load5: v.get("load5").and_then(Value::as_f64).unwrap_or(0.0),
        load15: v.get("load15").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

fn parse_mem_usage(v: &Value, key: &str) -> Option<MemUsage> {
    let v = v.get(key)?;
    if v.is_null() {
        return None;
    }
    Some(MemUsage {
        total_mb: v.get("total_mb").and_then(Value::as_u64).unwrap_or(0),
        used_mb: v.get("used_mb").and_then(Value::as_u64).unwrap_or(0),
    })
}

pub(super) fn parse_device_status(v: &Value) -> DeviceStatus {
    DeviceStatus {
        cpu_cores: v.get("cpu_cores").and_then(Value::as_u64).unwrap_or(0),
        load_average: parse_load_average(v),
        ram: parse_mem_usage(v, "ram"),
        disk: parse_mem_usage(v, "disk"),
        temperature_c: v.get("temperature_c").and_then(Value::as_f64),
        uptime_secs: v.get("uptime_secs").and_then(Value::as_u64),
    }
}

fn parse_network(v: &Value) -> Vec<NetworkIface> {
    v.get("interfaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|i| NetworkIface {
            name: i.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            is_up: i.get("is_up").and_then(Value::as_bool).unwrap_or(false),
            addresses: i
                .get("addresses")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        })
        .collect()
}

fn parse_update_summary(v: &Value) -> DeviceUpdateSummary {
    DeviceUpdateSummary {
        current_version: v.get("current_version").and_then(Value::as_str).unwrap_or("").to_string(),
        available: v.get("available").and_then(Value::as_bool).unwrap_or(false),
        latest_version: v.get("latest_version").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

// `parse_backup_schedule`/`parse_backup_files` live in `device_backup.rs`
// instead of here — they're backup-section-specific parsers, and moving
// them there (alongside the rendering code that's their only real
// consumer, plus `fetch_secondary` below which needs them too) keeps this
// file under this crate's own <800-line convention. `device_backup.rs`
// exposes them `pub(super)`, same visibility scope this file's own parsers
// use.
use crate::screens::device_backup::{parse_backup_files, parse_backup_schedule};

// ── Fetch orchestration ───────────────────────────────────────────────

pub(super) fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "device" || cx.default_global::<DeviceState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<DeviceState>().requested = true;
    fetch_status(cx, state.session_tx.clone());
}

/// `device.status` is fetched on its own (not via the generic [`spawn_call`]
/// below) because its raw `CallError` needs inspecting for the
/// `not_appliance` code — see this file's header comment. Every other
/// `device.*`/`system.check_update` call fires only once this one confirms
/// the gateway IS an appliance.
fn fetch_status(cx: &mut Context<RootView>, session_tx: tokio_mpsc::UnboundedSender<SessionCommand>) {
    let tx2 = session_tx.clone();
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, "device.status", json!({}));
        let result = rx.await;
        let _ = weak.update(cx, |_view, cx| {
            let is_appliance = match result {
                Ok(Ok(v)) => {
                    cx.default_global::<DeviceState>().status = Loadable::Ready(parse_device_status(&v));
                    true
                }
                Ok(Err(CallError::Rejected(err))) => {
                    if err.get("code").and_then(Value::as_str) == Some("not_appliance") {
                        cx.default_global::<DeviceState>().not_appliance = true;
                        false
                    } else {
                        cx.default_global::<DeviceState>().status = Loadable::Failed(describe_rejected(&err));
                        true
                    }
                }
                Ok(Err(e)) => {
                    cx.default_global::<DeviceState>().status = Loadable::Failed(describe_call_error(&e));
                    true
                }
                Err(_) => {
                    cx.default_global::<DeviceState>().status =
                        Loadable::Failed("背景連線執行緒已結束".to_string());
                    true
                }
            };
            if is_appliance {
                fetch_secondary(cx, tx2.clone());
            }
            cx.notify();
        });
    })
    .detach();
}

fn fetch_secondary(cx: &mut Context<RootView>, tx: tokio_mpsc::UnboundedSender<SessionCommand>) {
    spawn_call(cx, tx.clone(), "device.network", json!({}), |cx, result| {
        cx.default_global::<DeviceState>().network = result.map(|v| parse_network(&v)).into();
    });
    spawn_call(cx, tx.clone(), "system.check_update", json!({}), |cx, result| {
        cx.default_global::<DeviceState>().update = result.map(|v| parse_update_summary(&v)).into();
    });
    spawn_call(cx, tx.clone(), "device.backup_schedule_get", json!({}), |cx, result| {
        let g = cx.default_global::<DeviceState>();
        match &result {
            Ok(v) => {
                let sched = parse_backup_schedule(v);
                g.draft_enabled = sched.enabled;
                g.draft_interval = sched.interval_hours;
                g.draft_retention = sched.retention_count;
                g.schedule = Loadable::Ready(sched);
            }
            Err(e) => g.schedule = Loadable::Failed(e.clone()),
        }
    });
    spawn_call(cx, tx, "device.backup_list", json!({}), |cx, result| {
        cx.default_global::<DeviceState>().backups = result.map(|v| parse_backup_files(&v)).into();
    });
}

/// Generic device-page RPC round trip — pre-stringifies any error via
/// [`describe_call_error`] (never inspects `code`; only [`fetch_status`]
/// needs that). Shared with `device_backup.rs`'s mutating calls too.
pub(super) fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

pub(super) fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => describe_rejected(v),
    }
}

/// Every `device.*` failure this page has actually observed is the
/// structured `{code,message}` shape (`device_op_result_frame`,
/// `device_not_appliance_frame`, `device_confirm_required_frame` — all read
/// directly from `handlers.rs`); a bare-string fallback stays for parity
/// with every other page's `describe_call_error` (e.g. `dashboard.rs`'s
/// plain-string `WsFrame::error_response` convention), never a raw
/// `Value::to_string()` dump.
fn describe_rejected(v: &Value) -> String {
    v.get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| v.as_str().map(str::to_string))
        .unwrap_or_else(|| v.to_string())
}

// ── Display helpers ────────────────────────────────────────────────────

fn format_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        if hours > 0 { format!("{days}d {hours}h") } else { format!("{days}d") }
    } else if hours > 0 {
        if mins > 0 { format!("{hours}h {mins}m") } else { format!("{hours}h") }
    } else {
        format!("{mins}m")
    }
}

// ── Shared layout primitives (local — see `agents_detail.rs`'s "local copy
// over widened visibility" precedent) ────────────────────────────────────

pub(super) fn section_label(locale: Locale, key: &str) -> Div {
    div()
        .px_0p5()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, key))
}

pub(super) fn boxed_group(rows: Vec<Div>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .children(rows)
}

pub(super) fn error_line(locale: Locale, msg: &str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
        .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg))
}

// ── Section: 狀態 ─────────────────────────────────────────────────────

fn status_tile(label: SharedString, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .p_3()
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(value.into()),
        )
}

fn status_section(locale: Locale, status: &Loadable<DeviceStatus>) -> Div {
    let body: Div = match status {
        Loadable::Loading => {
            let mut grid = div().grid().grid_cols(3).gap_2p5();
            for _ in 0..6 {
                grid = grid.child(skeleton(px(180.), px(48.)));
            }
            grid
        }
        Loadable::Failed(msg) => error_line(locale, msg),
        Loadable::Ready(s) => {
            let mut grid = div().grid().grid_cols(3).gap_2p5();
            grid = grid.child(status_tile(i18n::t(locale, "device.status.cpuCores"), i18n::t1(locale, "device.status.cpuCoresValue", "n", &s.cpu_cores.to_string())));
            if let Some(load) = &s.load_average {
                grid = grid.child(status_tile(
                    i18n::t(locale, "device.status.load"),
                    format!("{:.2} / {:.2} / {:.2}", load.load1, load.load5, load.load15),
                ));
            }
            if let Some(ram) = &s.ram {
                grid = grid.child(status_tile(
                    i18n::t(locale, "device.status.ram"),
                    i18n::tn(locale, "device.status.usedOfTotal", &[("used", &format_mb(ram.used_mb)), ("total", &format_mb(ram.total_mb))]),
                ));
            }
            if let Some(disk) = &s.disk {
                grid = grid.child(status_tile(
                    i18n::t(locale, "device.status.disk"),
                    i18n::tn(locale, "device.status.usedOfTotal", &[("used", &format_mb(disk.used_mb)), ("total", &format_mb(disk.total_mb))]),
                ));
            }
            if let Some(temp) = s.temperature_c {
                grid = grid.child(status_tile(i18n::t(locale, "device.status.temperature"), format!("{temp:.1} °C")));
            }
            if let Some(uptime) = s.uptime_secs {
                grid = grid.child(status_tile(i18n::t(locale, "device.status.uptime"), format_uptime(uptime)));
            }
            grid
        }
    };
    div().flex().flex_col().gap_1p5().child(section_label(locale, "device.section.status")).child(body)
}

// ── Section: 更新（一行摘要 + 前往系統更新，非完整更新中心） ────────────

fn update_section(locale: Locale, update: &Loadable<DeviceUpdateSummary>, cx: &mut Context<RootView>) -> Div {
    let body: Div = match update {
        Loadable::Loading => skeleton(px(320.), px(20.)),
        Loadable::Failed(msg) => error_line(locale, msg),
        Loadable::Ready(u) => {
            let text = if u.available && !u.latest_version.is_empty() {
                i18n::tn(locale, "device.update.summary", &[("current", &u.current_version), ("latest", &u.latest_version)])
            } else {
                i18n::t1(locale, "device.update.upToDate", "current", &u.current_version)
            };
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size(px(7.)).rounded_full().bg(theme::alpha(if u.available { theme::WARNING } else { theme::SUCCESS }, 1.0)))
                        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(text)),
                )
                .child(
                    div()
                        .id("device-go-to-updates")
                        .px_3p5()
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .rounded(px(theme::RADIUS_LG))
                        .border_1()
                        .border_color(theme::border())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::BRAND, 1.0))
                        .child(i18n::t(locale, "device.update.goToUpdates"))
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.active_page = "systemUpdates";
                            cx.notify();
                        })),
                )
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(locale, "device.section.update"))
        .child(boxed_group(vec![div().p_3p5().child(body)]))
}

// ── Section: 網路 ─────────────────────────────────────────────────────

/// Always carries a bottom hairline — the static-IP row appended by
/// `network_section` below is always the visual last row of the group, so
/// no interface row is ever the group's final (border-less) child.
fn network_row(locale: Locale, iface: &NetworkIface) -> Div {
    let dot = div().size(px(7.)).rounded_full().bg(theme::alpha(if iface.is_up { theme::SUCCESS } else { theme::MUTED_FOREGROUND }, 1.0));
    let status_text = i18n::t(locale, if iface.is_up { "device.network.status.up" } else { "device.network.status.down" });
    let addresses = if iface.addresses.is_empty() { "—".to_string() } else { iface.addresses.join(", ") };
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2p5()
        .border_b_1()
        .border_color(theme::border())
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(iface.name.clone())))
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(div().flex().items_center().gap_1p5().child(dot).child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(status_text)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(addresses)),
        )
}

fn network_section(locale: Locale, network: &Loadable<Vec<NetworkIface>>) -> Div {
    let mut rows: Vec<Div> = Vec::new();
    match network {
        Loadable::Loading => rows.push(div().p_4().child(skeleton(px(300.), px(16.)))),
        Loadable::Failed(msg) => rows.push(div().p_4().child(error_line(locale, msg))),
        Loadable::Ready(ifaces) if ifaces.is_empty() => rows.push(
            div().p_4().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "device.network.empty")),
        ),
        Loadable::Ready(ifaces) => {
            for iface in ifaces {
                rows.push(network_row(locale, iface));
            }
        }
    }
    rows.push(
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_2p5()
            .border_t_1()
            .border_color(theme::border())
            .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "device.network.staticIp")))
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "device.network.staticIp.comingSoon"))),
    );
    div().flex().flex_col().gap_1p5().child(section_label(locale, "device.section.network")).child(boxed_group(rows))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<DeviceState>();
    let not_appliance = g.not_appliance;
    let status = g.status.clone();
    let update = g.update.clone();
    let network = g.network.clone();

    let header = div()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.device")))
        .child(div().mt_1().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "nav.device.desc")));

    if not_appliance {
        return div()
            .id("device-page")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(header)
                    .child(empty_state(
                        "🔌",
                        i18n::t(locale, "device.notAppliance.title"),
                        Some(i18n::t(locale, "device.notAppliance.desc")),
                        None::<Div>,
                    )),
            );
    }

    div()
        .id("device-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .p_6()
                .flex()
                .flex_col()
                .gap_5()
                .child(header)
                .child(status_section(locale, &status))
                .child(update_section(locale, &update, cx))
                .child(network_section(locale, &network))
                .child(crate::screens::device_backup::render_backup_section(state, cx))
                .child(crate::screens::device_backup::render_danger_section(state, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_device_status_reads_the_real_payload_shape() {
        let v = json!({ "cpu_cores": 8, "load_average": {"load1": 0.42, "load5": 0.38, "load15": 0.31},
            "ram": {"total_mb": 16000, "used_mb": 5600, "available_mb": 10400},
            "disk": {"total_mb": 240000, "used_mb": 88000, "available_mb": 152000}, "temperature_c": 52.0, "uptime_secs": 1054800 });
        let s = parse_device_status(&v);
        assert_eq!(s.cpu_cores, 8);
        let load = s.load_average.expect("load average present");
        assert_eq!((load.load1, load.load5, load.load15), (0.42, 0.38, 0.31));
        assert_eq!(s.ram.expect("ram present").used_mb, 5600);
        assert_eq!(s.disk.expect("disk present").total_mb, 240000);
        assert_eq!(s.temperature_c, Some(52.0));
        assert_eq!(s.uptime_secs, Some(1054800));
    }

    /// Covers both a JSON `null` sensor reading AND a wholly-absent key —
    /// both must degrade to `None`, never panic and never a fabricated 0.
    #[test]
    fn parse_device_status_null_or_missing_sensors_are_none_not_zero() {
        let v = json!({ "cpu_cores": 4, "load_average": null, "ram": null, "disk": null, "temperature_c": null, "uptime_secs": null });
        let s = parse_device_status(&v);
        assert!(s.load_average.is_none() && s.ram.is_none() && s.disk.is_none() && s.temperature_c.is_none() && s.uptime_secs.is_none());

        let empty = parse_device_status(&json!({}));
        assert_eq!(empty.cpu_cores, 0);
        assert!(empty.load_average.is_none());
    }

    #[test]
    fn parse_network_reads_interfaces_and_addresses() {
        let v = json!({ "interfaces": [
            {"name": "en0", "is_up": true, "addresses": ["192.168.1.42"]},
            {"name": "wlan0", "is_up": false, "addresses": []},
        ]});
        let ifaces = parse_network(&v);
        assert_eq!(ifaces.len(), 2);
        assert_eq!(ifaces[0].name, "en0");
        assert!(ifaces[0].is_up);
        assert_eq!(ifaces[0].addresses, vec!["192.168.1.42".to_string()]);
        assert!(!ifaces[1].is_up);
        assert!(ifaces[1].addresses.is_empty());
    }

    #[test]
    fn parse_network_missing_array_is_empty_not_panicking() {
        assert!(parse_network(&json!({})).is_empty());
    }

    #[test]
    fn parse_update_summary_reads_the_shared_check_update_shape() {
        let v = json!({ "available": true, "current_version": "1.61.0", "latest_version": "1.62.0" });
        let u = parse_update_summary(&v);
        assert!(u.available);
        assert_eq!(u.current_version, "1.61.0");
        assert_eq!(u.latest_version, "1.62.0");
    }

    // `parse_backup_schedule`/`parse_backup_files` tests moved to
    // `device_backup.rs` alongside the functions themselves (see this
    // file's `use crate::screens::device_backup::{...}` import above).

    #[test]
    fn describe_rejected_prefers_structured_message_over_bare_string() {
        assert_eq!(describe_rejected(&json!({"code": "not_appliance", "message": "此功能僅限值班機使用。"})), "此功能僅限值班機使用。");
        assert_eq!(describe_rejected(&json!("連線失敗")), "連線失敗");
    }

    #[test]
    fn format_mb_switches_to_gb_at_1024() {
        assert_eq!(format_mb(512), "512 MB");
        assert_eq!(format_mb(1024), "1.0 GB");
        assert_eq!(format_mb(5600), "5.5 GB");
    }

    #[test]
    fn format_uptime_picks_the_coarsest_two_units() {
        assert_eq!(format_uptime(59), "0m");
        assert_eq!(format_uptime(3600), "1h");
        assert_eq!(format_uptime(3660), "1h 1m");
        assert_eq!(format_uptime(90000), "1d 1h");
        assert_eq!(format_uptime(864000), "10d");
    }
}
