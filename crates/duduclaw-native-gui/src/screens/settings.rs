// WP-S6b3-P (S6b 第三波, 2026-08-22) — "系統設定" (`Settings.dc.html`, B5
// Tabs 索引 5 個 + boxed-list, only the 通用 tab carries real content — same
// "one real tab, the rest an honest stub" precedent `skills.rs`'s own
// header comment establishes for its 市場-only tab set). A "進階設定"
// drill-down leaf (`active_page == "settings"`, no `nav.rs` entry — wired
// from `manage_advanced.rs`'s 系統設定 row by this same pass). NOT the same
// page as `web/src/pages/SettingsPage.tsx` (a 15-tab shell covering
// container/heartbeat/autopilot/redaction/…) — this is the curated
// "跨系統整合設定總表" the canvas itself scopes to, per this task's own
// brief; `web` is a functional RPC cross-reference only, per "版面禁抄 web".
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `system.config {}` (dispatch L6082, handler `handle_system_config`
//   L19452, `require_admin!()`) → `{"config": <masked TOML string>,
//   "voice", "allowed_origins", "gap_digest_enabled", "novelty_gate_enabled",
//   "daily_digest_enabled", "daily_digest_at"}`. Only the last two structured
//   booleans/strings back this page's 每日摘要 row — the masked raw `config`
//   string is never string-scanned for other settings (this crate reads
//   structured JSON fields only, same convention every other page follows).
//   `system.version {}` (dispatch L6126, handler `handle_system_version`
//   L19609, no `require_*!()` gate) → `{"version", "auto_update", "edition",
//   "edition_profile"}`. Only `auto_update` is read here — this page reuses
//   the exact same field `screens::system_updates` already surfaces, rather
//   than a second parallel definition of "is auto-update on".
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **時區 row dropped.** No timezone field exists anywhere in
//    `system.config`/`system.version` — the canvas's "Asia/Taipei（UTC+8）"
//    is illustrative mockup text with nothing real behind it on this RPC
//    family. Dropped rather than fabricated, same "no backing field → drop
//    the line" precedent `users.rs`'s own header comment §1 establishes.
// 2. **"啟動時檢查健康狀態" row dropped** — same reasoning, no such flag
//    exists in either RPC.
// 3. **"需要人工決策時通知" row dropped** — no per-channel/global "notify on
//    needs_human" boolean exists in `system.config`; the real notification
//    fan-out for `needs_human` (`goal_notify.rs`) has no single on/off
//    switch surfaced through this RPC family. Dropped, not fabricated.
// 4. **自動更新 / 每日摘要 toggles are decision-class — assembled, not
//    wired**, same "disabled, no click handler" precedent every sibling
//    page in this pass establishes (`system.update_config` is a real
//    `require_admin!()` RPC, but writing is out of this page's read-only
//    scope this round).
// 5. **外觀/語言/隱私/進階 tabs render an honest stub**, no fabricated
//    content — this page's own `system.*` RPC family has no fields to back
//    them (theme/locale are local `RootView` state read by
//    `language_picker.rs`, a different page's own scope; 隱私/進階 have no
//    real settings surface named in this task's brief).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton, tabs, TabItem};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::screens::settings_common::{boxed_group, kv_row, section_label};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const TABS: [&str; 5] = ["general", "appearance", "language", "privacy", "advanced"];

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct SystemConfigInfo {
    pub daily_digest_enabled: bool,
    pub daily_digest_at: String,
}

pub fn parse_system_config(v: &Value) -> SystemConfigInfo {
    SystemConfigInfo {
        daily_digest_enabled: v.get("daily_digest_enabled").and_then(Value::as_bool).unwrap_or(false),
        daily_digest_at: v.get("daily_digest_at").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

pub fn parse_auto_update(v: &Value) -> bool {
    v.get("auto_update").and_then(Value::as_bool).unwrap_or(false)
}

// ── State ──────────────────────────────────────────────────────────────

pub struct SettingsState {
    requested: bool,
    pub tab: &'static str,
    pub config: Loadable<SystemConfigInfo>,
    pub auto_update: Loadable<bool>,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self { requested: false, tab: "general", config: Loadable::Loading, auto_update: Loadable::Loading }
    }
}

impl Global for SettingsState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "settings" || cx.default_global::<SettingsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<SettingsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "system.config", json!({}), |cx, result| {
        cx.default_global::<SettingsState>().config = result.map(|v| parse_system_config(&v)).into();
    });
    spawn_call(cx, tx, "system.version", json!({}), |cx, result| {
        cx.default_global::<SettingsState>().auto_update = result.map(|v| parse_auto_update(&v)).into();
    });
}

fn spawn_call(
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

fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.as_str().map(str::to_string))
            .unwrap_or_else(|| v.to_string()),
    }
}

// ── Toggle chip — assembled, no click handler; see module header §4 ──────

fn static_toggle(on: bool) -> Div {
    div()
        .relative()
        .w(px(34.))
        .h(px(20.))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::alpha(if on { theme::BRAND } else { theme::MUTED }, if on { 1.0 } else { 0.6 }))
        .child(div().absolute().top(px(2.)).left(if on { px(16.) } else { px(2.) }).size(px(16.)).rounded_full().bg(theme::alpha(0xffffff, 0.95)))
}

fn tab_label(locale: Locale, id: &str) -> SharedString {
    match id {
        "general" => i18n::t(locale, "settingsPage.tab.general"),
        "appearance" => i18n::t(locale, "settingsPage.tab.appearance"),
        "language" => i18n::t(locale, "settingsPage.tab.language"),
        "privacy" => i18n::t(locale, "settingsPage.tab.privacy"),
        _ => i18n::t(locale, "settingsPage.tab.advanced"),
    }
}

// ── 通用 tab content ───────────────────────────────────────────────────

fn general_tab(locale: Locale, config: &Loadable<SystemConfigInfo>, auto_update: &Loadable<bool>) -> Div {
    let general_row = match auto_update {
        Loadable::Loading => skeleton(px(700.), px(48.)),
        Loadable::Failed(err) => div().p_4().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(err.clone()),
        Loadable::Ready(on) => boxed_group(vec![kv_row(
            i18n::t(locale, "settingsPage.autoUpdate.label"),
            div().flex().items_center().gap_2().child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, if *on { "settingsPage.on" } else { "settingsPage.off" }))).child(static_toggle(*on)),
            true,
        )]),
    };

    let notify_row = match config {
        Loadable::Loading => skeleton(px(700.), px(48.)),
        Loadable::Failed(err) => div().p_4().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(err.clone()),
        Loadable::Ready(cfg) => {
            let detail: SharedString = if cfg.daily_digest_enabled && !cfg.daily_digest_at.is_empty() {
                i18n::t1(locale, "settingsPage.dailyDigest.at", "time", &cfg.daily_digest_at)
            } else {
                i18n::t(locale, "settingsPage.off")
            };
            boxed_group(vec![kv_row(
                i18n::t(locale, "settingsPage.dailyDigest.label"),
                div().flex().items_center().gap_2().child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(detail)).child(static_toggle(cfg.daily_digest_enabled)),
                true,
            )])
        }
    };

    div()
        .flex()
        .flex_col()
        .gap_5()
        .child(div().flex().flex_col().gap_1p5().child(section_label(i18n::t(locale, "settingsPage.section.general"))).child(general_row))
        .child(div().flex().flex_col().gap_1p5().child(section_label(i18n::t(locale, "settingsPage.section.notify"))).child(notify_row))
}

fn stub_tab(locale: Locale) -> Div {
    div().py_10().child(empty_state("🚧", i18n::t(locale, "settingsPage.stub"), None, None::<Div>))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<SettingsState>();
    let tab = g.tab;
    let config = g.config.clone();
    let auto_update = g.auto_update.clone();

    let crumb = breadcrumb("settings-breadcrumb", locale, i18n::t(locale, "settingsPage.title"), cx);
    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "settingsPage.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "settingsPage.subtitle")));

    let tab_items: Vec<TabItem> = TABS
        .iter()
        .map(|&id| {
            TabItem::new(
                id,
                tab_label(locale, id),
                cx.listener(move |_this, _ev, _window, cx| {
                    cx.default_global::<SettingsState>().tab = id;
                    cx.notify();
                }),
            )
        })
        .collect();
    let tab_row = tabs(tab_items, tab);

    let body = if tab == "general" { general_tab(locale, &config, &auto_update) } else { stub_tab(locale) };

    div()
        .id("settings-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(div().w_full().max_w(px(860.)).p_6().flex().flex_col().gap_3p5().child(crumb).child(header).child(tab_row).child(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_system_config_reads_digest_fields() {
        let v = json!({ "daily_digest_enabled": true, "daily_digest_at": "09:00" });
        let cfg = parse_system_config(&v);
        assert!(cfg.daily_digest_enabled);
        assert_eq!(cfg.daily_digest_at, "09:00");
    }

    #[test]
    fn parse_system_config_missing_fields_default_off() {
        let cfg = parse_system_config(&json!({}));
        assert!(!cfg.daily_digest_enabled);
        assert_eq!(cfg.daily_digest_at, "");
    }

    #[test]
    fn parse_auto_update_reads_the_real_field() {
        assert!(parse_auto_update(&json!({ "auto_update": true })));
        assert!(!parse_auto_update(&json!({ "auto_update": false })));
        assert!(!parse_auto_update(&json!({})));
    }

    #[test]
    fn tabs_constant_has_exactly_the_five_canvas_tabs() {
        assert_eq!(TABS, ["general", "appearance", "language", "privacy", "advanced"]);
    }
}
