// S5b (first wave) — 通道. This file adds the `screens::channels` module and
// its `render()` entry point only — `nav.rs`/`shell_sidebar.rs` (the sidebar
// wiring for a real `channels` nav id) are a parallel package's scope this
// pass (task brief: "側欄/導覽不歸你動"); until that lands, this page is
// reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=channels` (`main.rs`'s
// debug-page boot override sets `active_page` directly, bypassing `nav.rs`
// entirely) and `screens/shell.rs`'s Column-3 `active_id == "channels"`
// branch, added alongside this file.
//
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/
// Channels.dc.html` — header (title/subtitle + "LINE 好友 QR" + "新增通道"
// buttons), a card-list of channel rows (brand icon tile · name · status
// dot+label · last-connected/row-action), and a static footer hint naming
// the channels not shown their own row in the canvas's 6-row example.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `channels.status` (`handle_channels_status`, handlers.rs:11668) →
//   `{ "channels": [ { "name", "connected": bool, "last_connected":
//       string|null (RFC3339), "error": string|null } ] }`.
//   `name` is either a bare platform id ("line"/"telegram"/"discord" — the
//   three global-config-driven channels, handlers.rs:11677's `token_map`)
//   or `"platform:agent_name"` for a per-agent bound channel
//   (discord/telegram/slack only, handlers.rs:11720's `pairs` table) —
//   never any other shape. `error` defaults to `Some("connecting")` for any
//   channel the runtime hasn't reported state for yet (handlers.rs:11701).
//
// ── Header buttons + row actions — assembled, not wired ("決策類只組裝不
// 真按", per this WP's own brief) ─────────────────────────────────────────
// "LINE 好友 QR" / "新增通道" (header) and "重新授權" / "查看錯誤" (row,
// shown only for a non-transitional error) render as inert placeholders —
// `web/src/pages/ChannelsPage.tsx`'s full add/edit/QR/test/remove dialogs
// are out of this WP's 2-page scope. The backend also carries no structured
// "needs reauth" vs "generic failure" signal (`error` is free text set by
// whichever connector task failed) — [`looks_like_auth_error`] below is a
// COSMETIC label choice only (which of the canvas's two row-action labels
// to print), never a real classification gating real behavior, so a plain
// case-insensitive substring scan is enough here (this crate's coding
// convention #2 word-boundary helpers are for authorization/routing
// decisions, which this cosmetic label pick is not — and this crate has no
// dependency on `duduclaw-core` to reach for `word_contains_ci` anyway, see
// `console.rs::truncate_chars`'s doc comment on the detached-workspace
// constraint).
//
// ── Brand icon tint — the canvas's approved exception to theme tokens ────
// The task brief explicitly carves brand tint colors out of the normal
// theme-token discipline ("LINE/TG/DC/Slack/WA/Feishu 等 brand tint 是畫布
// 放行的例外色"). The canvas itself is a light-theme mock (pale pastel tile
// backgrounds, e.g. LINE's `#dcfce7`); this crate renders dark-only
// (`theme.rs`'s module doc comment), so pasting that pastel literally as a
// tile background would look washed-out against the dark surface. This file
// keeps the canvas's brand HUE verbatim (the icon glyph's solid stroke
// color — LINE `#15803d`, Telegram `#1d4ed8`, Discord `#6d28d9`, Slack
// `#be123c`, WhatsApp `#047857`, Feishu `#0369a1`, read straight off the
// canvas's SVG `stroke` attributes) and derives the tile BACKGROUND as
// `theme::alpha(hue, 0.16)` — the same alpha-wash recipe `badge.rs`'s
// Success/Warning/Info variants already use for status pills — instead of
// inventing an unrelated second color.
//
// The canvas only shows 6 platforms (LINE/Telegram/Discord/Slack/WhatsApp/
// Feishu). This crate's own platform list additionally covers WeCom/
// DingTalk/Google Chat/Microsoft Teams/WebChat (`CLAUDE.md`'s "eleven
// channels") — those 5 hues in [`brand_tint`] are THIS FILE'S OWN
// extrapolation (same one-hue-per-platform idea, not sourced from any
// canvas pixel), flagged here so a future design pass knows which colors
// are canvas-authoritative and which were filled in to cover the full
// platform roster.

use chrono::Utc;
use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::screens::goals::relative_time;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Data model (pure — unit tested without a live `App`) ────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
    pub name: String,
    pub connected: bool,
    pub last_connected: Option<String>,
    pub error: Option<String>,
}

/// `channels.status` response → rows. Skips any entry missing/empty `name`
/// (defensive against a malformed payload) rather than panicking or
/// producing a blank row nothing can act on.
pub fn parse_channels(v: &Value) -> Vec<ChannelRow> {
    v.get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let name = c.get("name").and_then(Value::as_str)?.to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(ChannelRow {
                        name,
                        connected: c.get("connected").and_then(Value::as_bool).unwrap_or(false),
                        last_connected: c.get("last_connected").and_then(Value::as_str).map(str::to_string),
                        error: c.get("error").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Splits `"platform"` / `"platform:agent"` — never panics on an empty or
/// malformed name (a pathological `":"` name still yields `""`, never an
/// index panic).
fn platform_of(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

fn agent_of(name: &str) -> Option<&str> {
    name.split_once(':').map(|(_, a)| a).filter(|a| !a.is_empty())
}

fn platform_display_label(platform: &str) -> &str {
    match platform {
        "line" => "LINE",
        "telegram" => "Telegram",
        "discord" => "Discord",
        "slack" => "Slack",
        "whatsapp" => "WhatsApp",
        "feishu" => "Feishu",
        "wecom" => "WeCom",
        "dingtalk" => "DingTalk",
        "googlechat" => "Google Chat",
        "teams" => "Microsoft Teams",
        "webchat" => "WebChat",
        // Unrecognized platform id — show it verbatim rather than blank.
        other => other,
    }
}

fn display_title(name: &str) -> String {
    let label = platform_display_label(platform_of(name));
    match agent_of(name) {
        Some(agent) => format!("{label} · {agent}"),
        None => label.to_string(),
    }
}

/// (glyph, hue) — see module doc comment "Brand icon tint" for which hues
/// are canvas-verbatim vs this file's own extrapolation.
fn brand_tint(platform: &str) -> (&'static str, u32) {
    match platform {
        "line" => ("LN", 0x15803d),
        "telegram" => ("TG", 0x1d4ed8),
        "discord" => ("DC", 0x6d28d9),
        "slack" => ("SK", 0xbe123c),
        "whatsapp" => ("WA", 0x047857),
        "feishu" => ("FS", 0x0369a1),
        "wecom" => ("WM", 0x0e7490),
        "dingtalk" => ("DT", 0x4338ca),
        "googlechat" => ("GC", 0x0f766e),
        "teams" => ("TM", 0x7e22ce),
        "webchat" => ("WB", 0x52525c),
        _ => ("?", 0x71717b),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    Connected,
    /// `error == Some("connecting"|"reconnecting")` — the two markers
    /// `handle_channels_status` itself recognizes as transitional.
    Transitional,
    /// `error` present and NOT one of the two transitional markers.
    Failed,
    /// `connected == false` and no `error` at all — theoretically reachable
    /// (a channel entry with an explicitly-cleared error) even though every
    /// live code path in `handle_channels_status` defaults `error` to
    /// `Some("connecting")` when runtime state is absent.
    Unknown,
}

fn row_status(row: &ChannelRow) -> RowStatus {
    if row.connected {
        return RowStatus::Connected;
    }
    match row.error.as_deref() {
        Some("connecting") | Some("reconnecting") => RowStatus::Transitional,
        Some(_) => RowStatus::Failed,
        None => RowStatus::Unknown,
    }
}

/// Cosmetic-only label heuristic — see module doc comment. Never gates any
/// real behavior; both row-action buttons are inert placeholders either way.
fn looks_like_auth_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    ["auth", "unauthor", "token", "credential", "expired", "invalid_grant", "401"]
        .iter()
        .any(|kw| m.contains(kw))
}

// ── State ──────────────────────────────────────────────────────────────

pub struct ChannelsState {
    requested: bool,
    pub channels: Loadable<Vec<ChannelRow>>,
}

impl Default for ChannelsState {
    fn default() -> Self {
        Self::new()
    }
}

impl Global for ChannelsState {}

impl ChannelsState {
    pub fn new() -> Self {
        Self { requested: false, channels: Loadable::Loading }
    }

    pub fn request_refresh(&mut self) {
        *self = Self::new();
    }
}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<ChannelsState>() {
        cx.set_global(ChannelsState::new());
    }
}

// ── Fetch orchestration (mirrors `inbox.rs`/`console.rs`'s Global-state
// shape — fired from the top of `render`) ─────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<ChannelsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<ChannelsState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx, "channels.status", json!({}), |cx, result| {
        cx.global_mut::<ChannelsState>().channels = result.map(|v| parse_channels(&v)).into();
    });
}

/// Duplicated from `inbox.rs`/`console.rs`/`dashboard.rs` rather than
/// importing — this crate's own established convention for this ~15-line
/// shape (see `inbox.rs::spawn_call`'s own doc comment).
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
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        // Same page-level gate every other RPC-backed S4b/S5b page uses.
        return div()
            .id("channels-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                i18n::t(locale, "native.home.connError.title"),
                Some(i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    let channels = cx.global::<ChannelsState>().channels.clone();

    let body: Div = match &channels {
        Loadable::Loading => {
            div().flex().flex_col().gap_2().children((0..5).map(|_| channel_skeleton_row()).collect::<Vec<_>>())
        }
        Loadable::Failed(msg) => div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(empty_state("⚠️", i18n::t1(locale, "native.channels.loadError", "message", msg), None, None::<Div>))
            .child(retry_button(locale, cx)),
        Loadable::Ready(rows) if rows.is_empty() => {
            div().child(empty_state("📡", i18n::t(locale, "native.channels.empty"), None, None::<Div>))
        }
        Loadable::Ready(rows) => {
            let mut col = div().flex().flex_col().gap_2();
            for row in rows {
                col = col.child(channel_row(locale, row));
            }
            col
        }
    };

    div()
        .id("channels-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_6()
        // ── Header: title/subtitle + two assembled-only action buttons ───
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XL))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(i18n::t(locale, "native.channels.title")),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(i18n::t(locale, "native.channels.subtitle")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        // Assembled, not wired — see module doc comment.
                        .child(button(
                            "channels-line-qr",
                            i18n::t(locale, "native.channels.lineQr"),
                            ButtonVariant::Secondary,
                            false,
                            None,
                            |_ev, _window, _app| {},
                        ))
                        .child(button(
                            "channels-add",
                            i18n::t(locale, "native.channels.add"),
                            ButtonVariant::Primary,
                            false,
                            None,
                            |_ev, _window, _app| {},
                        )),
                ),
        )
        // ── Channel list ──────────────────────────────────────────────
        .child(body)
        // ── Footer hint — the platform roster beyond the canvas's 6-row
        // example (see module doc comment) ───────────────────────────
        .child(
            div()
                .w_full()
                .text_center()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                .child(i18n::t(locale, "native.channels.footerHint")),
        )
}

fn channel_skeleton_row() -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(16.))
        .py(px(12.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(skeleton(px(34.), px(34.)))
        .child(div().flex_1().child(skeleton(px(140.), px(14.))))
        .child(skeleton(px(70.), px(12.)))
}

fn retry_button(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    button(
        "channels-retry",
        i18n::t(locale, "native.channels.retry"),
        ButtonVariant::Secondary,
        false,
        None,
        cx.listener(|_this, _ev, _window, cx| {
            cx.global_mut::<ChannelsState>().request_refresh();
            cx.notify();
        }),
    )
}

fn channel_row(locale: Locale, row: &ChannelRow) -> Div {
    let platform = platform_of(&row.name);
    let (glyph, hue) = brand_tint(platform);
    let status = row_status(row);

    let (dot_color, status_key) = match status {
        RowStatus::Connected => (theme::SUCCESS, "native.channels.status.connected"),
        RowStatus::Transitional => (
            theme::INFO,
            if row.error.as_deref() == Some("reconnecting") {
                "native.channels.status.reconnecting"
            } else {
                "native.channels.status.connecting"
            },
        ),
        RowStatus::Failed => (theme::DESTRUCTIVE, "native.channels.status.failed"),
        RowStatus::Unknown => (theme::MUTED_FOREGROUND, "native.channels.status.disconnected"),
    };

    let right_meta = if status == RowStatus::Failed {
        let label = if row.error.as_deref().map(looks_like_auth_error).unwrap_or(false) {
            i18n::t(locale, "native.channels.action.reauth")
        } else {
            i18n::t(locale, "native.channels.action.viewError")
        };
        // Assembled, not wired — see module doc comment.
        div()
            .flex()
            .justify_end()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .rounded(px(theme::RADIUS_LG))
                    .border_1()
                    .border_color(theme::surface_border())
                    .text_size(px(theme::TEXT_XS))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::alpha(theme::BRAND, 1.0))
                    .child(label),
            )
    } else {
        let when = row
            .last_connected
            .as_deref()
            .map(|iso| relative_time(locale, iso, Utc::now()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| SharedString::from("—"));
        div().flex().justify_end().child(
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(when),
        )
    };

    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(16.))
        .py(px(12.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(if status == RowStatus::Failed {
            gpui::Hsla::from(theme::alpha(theme::DESTRUCTIVE, 0.35))
        } else {
            theme::surface_border()
        })
        .when(status != RowStatus::Failed, |el| el.shadow(theme::surface_shadow()))
        .child(
            div()
                .size(px(34.))
                .flex_shrink_0()
                .rounded(px(theme::RADIUS_MD))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::alpha(hue, 0.16))
                .text_color(theme::alpha(hue, 1.0))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(display_title(&row.name)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, status_key)),
                ),
        )
        .child(div().w(px(96.)).child(right_meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channels_reads_real_handler_shape() {
        let v = json!({ "channels": [
            { "name": "line", "connected": true, "last_connected": "2026-08-21T12:00:00Z", "error": null },
            { "name": "discord:sam", "connected": false, "last_connected": null, "error": "connecting" },
        ]});
        let rows = parse_channels(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "line");
        assert!(rows[0].connected);
        assert_eq!(rows[0].last_connected.as_deref(), Some("2026-08-21T12:00:00Z"));
        assert_eq!(rows[1].name, "discord:sam");
        assert!(!rows[1].connected);
        assert_eq!(rows[1].error.as_deref(), Some("connecting"));
    }

    #[test]
    fn parse_channels_missing_array_is_empty_not_a_panic() {
        assert_eq!(parse_channels(&json!({})).len(), 0);
    }

    #[test]
    fn parse_channels_skips_entries_missing_name() {
        let v = json!({ "channels": [ { "connected": true } ] });
        assert_eq!(parse_channels(&v).len(), 0);
    }

    #[test]
    fn platform_of_splits_agent_scoped_name() {
        assert_eq!(platform_of("discord:sam"), "discord");
        assert_eq!(platform_of("line"), "line");
        assert_eq!(agent_of("discord:sam"), Some("sam"));
        assert_eq!(agent_of("line"), None);
    }

    #[test]
    fn platform_of_never_panics_on_pathological_input() {
        assert_eq!(platform_of(":"), "");
        assert_eq!(agent_of(":"), None); // agent half is empty → filtered out
        assert_eq!(platform_of(""), "");
    }

    #[test]
    fn display_title_appends_agent_suffix_only_when_present() {
        assert_eq!(display_title("line"), "LINE");
        assert_eq!(display_title("discord:sam"), "Discord · sam");
        assert_eq!(display_title("unknown_platform"), "unknown_platform");
    }

    #[test]
    fn brand_tint_falls_back_for_unknown_platform() {
        let (glyph, hue) = brand_tint("carrier_pigeon");
        assert_eq!(glyph, "?");
        assert_eq!(hue, 0x71717b);
        // A known platform never hits the fallback arm.
        assert_ne!(brand_tint("line").1, 0x71717b);
    }

    #[test]
    fn row_status_classifies_all_four_states() {
        let base =
            ChannelRow { name: "x".into(), connected: false, last_connected: None, error: None };
        assert_eq!(
            row_status(&ChannelRow { connected: true, ..base.clone() }),
            RowStatus::Connected
        );
        assert_eq!(
            row_status(&ChannelRow { error: Some("connecting".into()), ..base.clone() }),
            RowStatus::Transitional
        );
        assert_eq!(
            row_status(&ChannelRow { error: Some("reconnecting".into()), ..base.clone() }),
            RowStatus::Transitional
        );
        assert_eq!(
            row_status(&ChannelRow { error: Some("401 Unauthorized".into()), ..base.clone() }),
            RowStatus::Failed
        );
        assert_eq!(row_status(&base), RowStatus::Unknown);
    }

    #[test]
    fn looks_like_auth_error_is_case_insensitive_keyword_scan() {
        assert!(looks_like_auth_error("Token expired"));
        assert!(looks_like_auth_error("401 Unauthorized"));
        assert!(looks_like_auth_error("INVALID_GRANT"));
        assert!(!looks_like_auth_error("connection refused"));
        assert!(!looks_like_auth_error("timeout after 30s"));
    }
}
