// S5b1-A (2026-08-21) — "系統更新" (`/app/system/updates`, `nav.rs` id
// `systemUpdates`). Visual authority: `commercial/design/duduclaw-s5-
// settings-pages/SystemUpdates.dc.html` — B7 system-update settings: header
// + 立即檢查 → 自動更新 toggle → 目前/最新版本 tiles → 版本紀錄 (release
// notes) → 立即安裝 → 更新歷史 → install-method footnote.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `system.version` (`handle_system_version` ~L19608, no role gate) →
//     `{ "version", "auto_update", "edition", "edition_profile" }`. Used
//     ONLY for the initial `auto_update` toggle state and the Pro/community
//     edition gate — NOT for the version tiles (those come from
//     `system.check_update`, the richer payload).
//   `system.check_update` (`handle_system_check_update` ~L19636, dispatch
//     gate `require_admin!()` ~L6127) → `{ "available", "current_version",
//     "latest_version", "release_notes", "published_at", "download_url",
//     "checksum_url", "install_method", "brew_formula", "auto_update",
//     "restart_pending_version", "update_channel"
//     ("control_plane"|"github"|"none"), "containerized" }`. Same RPC
//     `about.rs`'s update-status line and `device.rs`'s update summary row
//     already consume — this page is the one that reads the FULL payload
//     (release notes, install guidance, history-adjacent fields).
//   `system.apply_update` (`handle_system_apply_update` ~L19701, dispatch
//     gate `require_admin!()`) → `{ "success", ... }` (progress streamed via
//     the `system.update_progress` event — NOT subscribed to by this page
//     this round, see deviation below).
//   `system.update_config` (`handle_system_update_config`, dispatch gate
//     `require_admin!()`) → accepts `{ "auto_update": bool }`, used to
//     persist the toggle (never inferred, always an explicit click).
//
// ── Install-guidance gating (functional reference: `web/src/components/
// settings/sections/UpdateTab.tsx` — "版面禁抄 web" per this task's own
// rule, but the GATING LOGIC itself is copied faithfully since it encodes
// real product constraints, not visual layout) ───────────────────────────
// Priority order, highest first: containerized → homebrew → desktop →
// pro-no-channel → no-binary → up-to-date → installable. Exactly one of
// these renders per settled `check` state; see `install_action` below.
//
// ── Deliberate degradations, documented rather than faked ────────────────
// 1. "更新歷史" (更新歷史卡列) has NO backing RPC — nothing in this
//    workspace persists a version-history list (`system.check_update`
//    answers "what's latest", not "what shipped before"). The task brief's
//    own instruction is explicit here ("web 無歷史資料來源就以「無紀錄」空
//    態誠實呈現，不造假資料") — this section always renders the empty state,
//    never the canvas's three fabricated `v1.61.0`/`v1.60.0`/`v1.59.0` rows.
// 2. No live `system.update_progress` event subscription (retry-attempt
//    banner) — `about.rs`/`device.rs` don't either; wiring a new event
//    subscription channel for one page's progress bar is a bigger change
//    than this pass's scope. The install button still shows a plain
//    "安裝中…" busy state from the RPC's own eventual response.
// 3. Auto-update toggle is real and wired (`system.update_config`) — unlike
//    the destructive actions on `device.rs`'s danger zone, flipping a
//    boolean config flag is low-risk and reversible, so it dispatches
//    immediately on click (no arm-then-confirm step), matching `web`'s own
//    `handleAutoUpdateToggle` behavior exactly.
//
// State lives behind `gpui::Global` — same pattern every other S5b1-A page
// establishes (no `main.rs` field).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const CONTENT_MAX_WIDTH: f32 = 680.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct VersionInfo {
    pub auto_update: bool,
    pub is_pro: bool,
}

#[derive(Clone)]
pub struct UpdateCheckInfo {
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_notes: String,
    pub download_url: String,
    pub install_method: String,
    pub update_channel: String,
    pub containerized: bool,
    pub restart_pending_version: Option<String>,
}

pub struct SystemUpdatesState {
    requested: bool,
    pub version: Loadable<VersionInfo>,
    pub check: Loadable<UpdateCheckInfo>,
    pub checking: bool,
    pub auto_update_draft: bool,
    pub auto_update_saving: bool,
    pub auto_update_save_error: Option<String>,
    pub installing: bool,
    pub install_result: Option<Result<(), String>>,
}

impl Default for SystemUpdatesState {
    fn default() -> Self {
        Self {
            requested: false,
            version: Loadable::Loading,
            check: Loadable::Loading,
            checking: false,
            auto_update_draft: false,
            auto_update_saving: false,
            auto_update_save_error: None,
            installing: false,
            install_result: None,
        }
    }
}

impl Global for SystemUpdatesState {}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_version_info(v: &Value) -> VersionInfo {
    let edition = v.get("edition").and_then(Value::as_str).unwrap_or("community");
    VersionInfo {
        auto_update: v.get("auto_update").and_then(Value::as_bool).unwrap_or(false),
        is_pro: edition != "community",
    }
}

fn parse_check_info(v: &Value) -> UpdateCheckInfo {
    UpdateCheckInfo {
        available: v.get("available").and_then(Value::as_bool).unwrap_or(false),
        current_version: v.get("current_version").and_then(Value::as_str).unwrap_or("").to_string(),
        latest_version: v.get("latest_version").and_then(Value::as_str).unwrap_or("").to_string(),
        release_notes: v.get("release_notes").and_then(Value::as_str).unwrap_or("").to_string(),
        download_url: v.get("download_url").and_then(Value::as_str).unwrap_or("").to_string(),
        install_method: v.get("install_method").and_then(Value::as_str).unwrap_or("").to_string(),
        update_channel: v.get("update_channel").and_then(Value::as_str).unwrap_or("").to_string(),
        containerized: v.get("containerized").and_then(Value::as_bool).unwrap_or(false),
        restart_pending_version: v
            .get("restart_pending_version")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "systemUpdates" || cx.default_global::<SystemUpdatesState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<SystemUpdatesState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "system.version", json!({}), |cx, result| {
        let g = cx.default_global::<SystemUpdatesState>();
        match &result {
            Ok(v) => {
                let info = parse_version_info(v);
                g.auto_update_draft = info.auto_update;
                g.version = Loadable::Ready(info);
            }
            Err(e) => g.version = Loadable::Failed(e.clone()),
        }
    });
    dispatch_check(cx, tx);
}

fn dispatch_check(cx: &mut Context<RootView>, tx: tokio_mpsc::UnboundedSender<SessionCommand>) {
    cx.default_global::<SystemUpdatesState>().checking = true;
    spawn_call(cx, tx, "system.check_update", json!({}), |cx, result| {
        let g = cx.default_global::<SystemUpdatesState>();
        g.checking = false;
        g.check = result.map(|v| parse_check_info(&v)).into();
    });
}

fn dispatch_toggle_auto_update(cx: &mut Context<RootView>, state: &RootView, enabled: bool) {
    let tx = state.session_tx.clone();
    {
        let g = cx.default_global::<SystemUpdatesState>();
        g.auto_update_saving = true;
        g.auto_update_save_error = None;
    }
    spawn_call(cx, tx, "system.update_config", json!({ "auto_update": enabled }), move |cx, result| {
        let g = cx.default_global::<SystemUpdatesState>();
        g.auto_update_saving = false;
        match result {
            Ok(_) => g.auto_update_draft = enabled,
            Err(e) => g.auto_update_save_error = Some(e),
        }
    });
}

fn dispatch_install(cx: &mut Context<RootView>, state: &RootView) {
    let tx = state.session_tx.clone();
    {
        let g = cx.default_global::<SystemUpdatesState>();
        g.installing = true;
        g.install_result = None;
    }
    spawn_call(cx, tx, "system.apply_update", json!({}), |cx, result| {
        let g = cx.default_global::<SystemUpdatesState>();
        g.installing = false;
        match result {
            Ok(v) => {
                let success = v.get("success").and_then(Value::as_bool).unwrap_or(false);
                if success {
                    g.install_result = Some(Ok(()));
                } else {
                    let msg = v.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                    g.install_result = Some(Err(msg));
                }
            }
            Err(e) => g.install_result = Some(Err(e)),
        }
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

// ── Shared primitives ──────────────────────────────────────────────────

fn error_line(locale: Locale, msg: &str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
        .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg))
}

fn boxed(body: Div) -> Div {
    div()
        .w_full()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(body)
}

fn hint_box(locale: Locale, key: &str, tone: u32) -> Div {
    div()
        .p_3p5()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(tone, 0.10))
        .border_1()
        .border_color(theme::alpha(tone, 0.30))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(tone, 1.0))
        .child(i18n::t(locale, key))
}

// ── Section: 自動更新 ──────────────────────────────────────────────────

fn toggle_switch(id: &'static str, checked: bool, cx: &mut Context<RootView>) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .relative()
        .w(px(36.))
        .h(px(21.))
        .flex_shrink_0()
        .rounded_full()
        .cursor_pointer()
        .bg(theme::alpha(if checked { theme::BRAND } else { theme::MUTED }, 1.0))
        .child(div().absolute().top(px(2.)).left(if checked { px(17.) } else { px(2.) }).size(px(17.)).rounded_full().bg(theme::alpha(0xffffff, 1.0)))
        .on_click(cx.listener(|this, _ev, _window, cx| {
            let enabled = !cx.default_global::<SystemUpdatesState>().auto_update_draft;
            dispatch_toggle_auto_update(cx, this, enabled);
            cx.notify();
        }))
}

fn auto_update_section(locale: Locale, version: &Loadable<VersionInfo>, cx: &mut Context<RootView>) -> Option<Div> {
    let is_pro = match version {
        Loadable::Ready(v) => v.is_pro,
        _ => return None, // don't render a toggle whose real state we don't know yet
    };
    if !is_pro {
        return None;
    }
    let g = cx.default_global::<SystemUpdatesState>();
    let draft = g.auto_update_draft;
    let saving = g.auto_update_saving;
    let save_error = g.auto_update_save_error.clone();

    let toggle = toggle_switch("updates-auto-toggle", draft, cx);
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "updates.autoUpdate.title")))
        .child(toggle);
    let desc = div()
        .px_4()
        .pb_3()
        .text_size(px(11.5))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, "updates.autoUpdate.desc"));
    let mut wrap = div().flex().flex_col();
    wrap = wrap.child(row).child(desc);
    if saving {
        wrap = wrap.child(div().px_4().pb_2p5().child(skeleton(px(80.), px(12.))));
    }
    if let Some(msg) = save_error {
        wrap = wrap.child(div().px_4().pb_3().child(error_line(locale, &msg)));
    }
    Some(boxed(wrap))
}

// ── Section: 版本 tiles ────────────────────────────────────────────────

fn version_tiles(locale: Locale, info: &UpdateCheckInfo) -> Div {
    let current = div()
        .flex_1()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::MUTED, 1.0))
        .p_3p5()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "updates.current")))
        .child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(format!("v{}", info.current_version)));

    let latest_tone = if info.available { theme::WARNING } else { theme::SUCCESS };
    let latest = div()
        .flex_1()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(latest_tone, 0.10))
        .border_1()
        .border_color(theme::alpha(latest_tone, 0.30))
        .p_3p5()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "updates.latest")))
        .child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(latest_tone, 1.0)).child(format!("v{}", info.latest_version)));

    div().flex().gap_2p5().child(current).child(latest)
}

fn release_notes_box(locale: Locale, notes: &str) -> Option<Div> {
    if notes.trim().is_empty() {
        return None;
    }
    Some(
        boxed(
            div()
                .p_3p5()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "updates.releaseNotes")))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(notes.to_string())),
        ),
    )
}

// ── Install action / guidance gating (mirrors `UpdateTab.tsx`'s decision
// tree — see this file's header comment) ───────────────────────────────

/// Pure decision — no gpui dependency — so the priority order itself
/// (`web/src/components/settings/sections/UpdateTab.tsx`'s own gating
/// tree, copied faithfully per this file's header comment) is unit-
/// testable without constructing a `Context<RootView>`. `install_action`
/// below is the only caller; it just turns this into pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallGuidance {
    UpToDate,
    Containerized,
    Homebrew,
    Desktop,
    ProNoChannel,
    NoBinary,
    Installable,
}

fn install_guidance(info: &UpdateCheckInfo) -> InstallGuidance {
    if !info.available {
        return InstallGuidance::UpToDate;
    }
    let is_containerized = info.containerized;
    let is_homebrew = info.install_method == "homebrew";
    let is_desktop = info.install_method == "desktop";
    let is_pro_no_channel = info.install_method == "pro" && info.update_channel == "none" && !is_containerized;
    let uses_provider = info.update_channel == "control_plane";
    let no_binary = info.download_url.is_empty() && !is_pro_no_channel && !uses_provider && !is_containerized;

    if is_containerized {
        InstallGuidance::Containerized
    } else if is_homebrew {
        InstallGuidance::Homebrew
    } else if is_desktop {
        InstallGuidance::Desktop
    } else if is_pro_no_channel {
        InstallGuidance::ProNoChannel
    } else if no_binary {
        InstallGuidance::NoBinary
    } else {
        InstallGuidance::Installable
    }
}

fn install_action(locale: Locale, info: &UpdateCheckInfo, installing: bool, cx: &mut Context<RootView>) -> Div {
    match install_guidance(info) {
        InstallGuidance::UpToDate => {
            return div()
                .flex()
                .items_center()
                .gap_2()
                .p_3p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::SUCCESS, 0.10))
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::SUCCESS, 1.0))
                .child(i18n::t(locale, "updates.upToDate"));
        }
        InstallGuidance::Containerized => return hint_box(locale, "updates.containerHint", theme::MUTED_FOREGROUND),
        InstallGuidance::Homebrew => return hint_box(locale, "updates.homebrewHint", theme::WARNING),
        InstallGuidance::Desktop => return hint_box(locale, "updates.desktopManaged", theme::MUTED_FOREGROUND),
        InstallGuidance::ProNoChannel => return hint_box(locale, "updates.proNoChannel", theme::MUTED_FOREGROUND),
        InstallGuidance::NoBinary => return hint_box(locale, "updates.noBinary", theme::WARNING),
        InstallGuidance::Installable => {}
    }

    let label = if installing {
        i18n::t(locale, "updates.installing")
    } else {
        i18n::t1(locale, "updates.install", "version", &info.latest_version)
    };
    div().child(button("updates-install", label, ButtonVariant::Primary, installing, None, cx.listener(|this, _ev, _window, cx| {
        dispatch_install(cx, this);
    })))
}

// ── Section: 更新歷史（誠實空態，見檔頭 deviation #1） ────────────────────

fn history_section(locale: Locale) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(div().px_0p5().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "updates.history.title")))
        .child(boxed(div().py_6().child(empty_state("📭", i18n::t(locale, "updates.history.empty"), None, None::<Div>))))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<SystemUpdatesState>();
    let version = g.version.clone();
    let check = g.check.clone();
    let checking = g.checking;
    let installing = g.installing;
    let install_result = g.install_result.clone();

    let check_btn_label = if checking { i18n::t(locale, "updates.checking") } else { i18n::t(locale, "updates.check") };
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.systemUpdates")))
                .child(div().mt_1().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "nav.systemUpdates.desc"))),
        )
        .child(button("updates-check", check_btn_label, ButtonVariant::Secondary, checking, None, cx.listener(|this, _ev, _window, cx| {
            let tx = this.session_tx.clone();
            dispatch_check(cx, tx);
        })));

    let mut col = div().w_full().max_w(px(CONTENT_MAX_WIDTH)).p_6().flex().flex_col().gap_4().child(header);

    if let Some(section) = auto_update_section(locale, &version, cx) {
        col = col.child(section);
    }

    col = match &check {
        Loadable::Loading => col.child(skeleton(px(400.), px(80.))),
        Loadable::Failed(msg) => col.child(error_line(locale, msg)),
        Loadable::Ready(info) => {
            let mut inner = col.child(version_tiles(locale, info));
            if let Some(pending) = &info.restart_pending_version {
                inner = inner.child(hint_box(locale, "updates.restartPending", theme::WARNING).child(
                    // `hint_box` already rendered the static i18n text; append the
                    // version inline instead of a second, harder-to-localize key.
                    div().mt_1().text_size(px(11.)).font_family("SF Mono").child(SharedString::from(format!("v{pending}"))),
                ));
            }
            if let Some(notes) = release_notes_box(locale, &info.release_notes) {
                inner = inner.child(notes);
            }
            inner = inner.child(install_action(locale, info, installing, cx));
            if let Some(result) = &install_result {
                match result {
                    Ok(()) => inner = inner.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::SUCCESS, 1.0)).child(i18n::t(locale, "updates.installed"))),
                    Err(msg) => inner = inner.child(error_line(locale, msg)),
                }
            }
            inner
        }
    };

    col = col.child(history_section(locale));

    if let Loadable::Ready(info) = &check {
        if info.install_method == "desktop" {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .rounded(px(theme::RADIUS_LG))
                    .bg(theme::alpha(theme::MUTED, 1.0))
                    .text_size(px(11.5))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(i18n::t(locale, "updates.desktopManaged")),
            );
        }
    }

    div().id("system-updates-page").size_full().overflow_y_scroll().flex().flex_col().items_center().child(col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_info(available: bool) -> UpdateCheckInfo {
        UpdateCheckInfo {
            available,
            current_version: "1.61.0".to_string(),
            latest_version: "1.62.0".to_string(),
            release_notes: String::new(),
            download_url: "https://example.invalid/duduclaw-1.62.0.tar.gz".to_string(),
            install_method: "npm".to_string(),
            update_channel: "github".to_string(),
            containerized: false,
            restart_pending_version: None,
        }
    }

    #[test]
    fn parse_version_info_reads_edition_and_auto_update() {
        let v = json!({ "version": "1.61.0", "auto_update": true, "edition": "enterprise", "edition_profile": "enterprise" });
        let info = parse_version_info(&v);
        assert!(info.auto_update);
        assert!(info.is_pro);
    }

    #[test]
    fn parse_version_info_community_edition_is_not_pro() {
        let v = json!({ "edition": "community" });
        assert!(!parse_version_info(&v).is_pro);
    }

    #[test]
    fn parse_version_info_missing_edition_defaults_to_community_not_pro() {
        assert!(!parse_version_info(&json!({})).is_pro);
    }

    #[test]
    fn parse_check_info_reads_the_real_payload_shape() {
        let v = json!({
            "available": true, "current_version": "1.61.0", "latest_version": "1.62.0",
            "release_notes": "- fix x\n- fix y", "download_url": "https://x", "install_method": "desktop",
            "update_channel": "github", "containerized": false, "restart_pending_version": "1.62.0",
        });
        let info = parse_check_info(&v);
        assert!(info.available);
        assert_eq!(info.release_notes, "- fix x\n- fix y");
        assert_eq!(info.install_method, "desktop");
        assert_eq!(info.restart_pending_version, Some("1.62.0".to_string()));
    }

    #[test]
    fn parse_check_info_missing_fields_never_panics() {
        let info = parse_check_info(&json!({}));
        assert!(!info.available);
        assert!(info.restart_pending_version.is_none());
    }

    #[test]
    fn install_guidance_up_to_date_when_not_available() {
        assert_eq!(install_guidance(&base_info(false)), InstallGuidance::UpToDate);
    }

    #[test]
    fn install_guidance_containerized_wins_over_every_other_signal() {
        let mut info = base_info(true);
        info.containerized = true;
        info.install_method = "pro".to_string();
        info.update_channel = "none".to_string();
        assert_eq!(install_guidance(&info), InstallGuidance::Containerized);
    }

    #[test]
    fn install_guidance_homebrew() {
        let mut info = base_info(true);
        info.install_method = "homebrew".to_string();
        assert_eq!(install_guidance(&info), InstallGuidance::Homebrew);
    }

    #[test]
    fn install_guidance_desktop() {
        let mut info = base_info(true);
        info.install_method = "desktop".to_string();
        assert_eq!(install_guidance(&info), InstallGuidance::Desktop);
    }

    #[test]
    fn install_guidance_pro_no_channel() {
        let mut info = base_info(true);
        info.install_method = "pro".to_string();
        info.update_channel = "none".to_string();
        assert_eq!(install_guidance(&info), InstallGuidance::ProNoChannel);
    }

    #[test]
    fn install_guidance_no_binary_when_download_url_empty() {
        let mut info = base_info(true);
        info.download_url = String::new();
        assert_eq!(install_guidance(&info), InstallGuidance::NoBinary);
    }

    #[test]
    fn install_guidance_control_plane_provider_is_installable_despite_empty_download_url() {
        let mut info = base_info(true);
        info.download_url = String::new();
        info.update_channel = "control_plane".to_string();
        assert_eq!(install_guidance(&info), InstallGuidance::Installable);
    }

    #[test]
    fn install_guidance_ordinary_npm_install_with_a_url_is_installable() {
        assert_eq!(install_guidance(&base_info(true)), InstallGuidance::Installable);
    }

    #[test]
    fn describe_call_error_prefers_structured_message() {
        let msg = describe_call_error(&CallError::Rejected(json!({"code": "denied", "message": "權限不足"})));
        assert_eq!(msg, "權限不足");
    }
}
