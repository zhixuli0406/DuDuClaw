// S5b (first wave) — 整合總覽. This file adds the `screens::integrations`
// module and its `render()` entry point only — `nav.rs`/`shell_sidebar.rs`
// (the sidebar wiring for a real `integrations` nav id) are a parallel
// package's scope this pass (task brief: "側欄/導覽不歸你動"); until that
// lands, this page is reachable via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=integrations` (`main.rs`'s debug-page boot
// override sets `active_page` directly, bypassing `nav.rs` entirely) and
// `screens/shell.rs`'s Column-3 `active_id == "integrations"` branch, added
// alongside this file.
//
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/
// Integrations.dc.html` — header (title/subtitle only, no header actions)
// then FOUR non-peer status cards (task brief B6: "四張整合狀態卡非平權
// tabs" — deliberately NOT the web `IntegrationsPage.tsx`'s `<Tabs>` layout,
// which this page's own visual authority overrides for this surface) — 工具
// 伺服器（MCP）／Google 工作區／Odoo ERP／身分解析 — each a clickable summary
// row with an icon tile, a one-line purpose blurb + live status detail, a
// status dot+label, and a chevron.
//
// ── Drill-down navigation contract (shared with the parallel C package) ──
// Clicking a card sets `RootView::active_page` to one of `"mcp"` / `"odoo"`
// / `"googleIntegration"` / `"identity"` — the exact four ids this task's
// brief names ("本頁以 active_id 約定跳轉…C 包同約定"). None of those four
// pages exist in `shell.rs`'s Column-3 match yet; navigating to one before
// the C package lands just falls through to that match's existing generic
// placeholder branch (raw id as heading, no crash) — `shell.rs`'s own
// pre-existing fallback IS the "route 存在與否守護（不 panic）" this task
// asks for, not something this file needs to reimplement.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `mcp.list` (`handle_mcp_list`, handlers.rs:27821) → `{ "agents": [
//     { "agent_id", "servers": [ { "name", "command", "args", "env" } ] } ],
//     "catalog": [...] }`. This card's "N 台已連接" counts DISTINCT server
//     names across every agent's `servers` array (a server two agents both
//     use counts once, matching "how many distinct tool servers are
//     reachable" rather than double-counting); "M 位員工使用中" counts
//     agents whose own `servers` array is non-empty.
//   `odoo.status` (`handle_odoo_status`, handlers.rs:24461) → `{
//     "connected": bool, "edition"?, "version"?, "uid"? }` when configured,
//     or `{ "connected": false, "error"? }` when not configured / the
//     credential is missing / the connect attempt failed — `error` is only
//     ever `"No credential configured"` or `"Connection failed"` (both
//     hardcoded strings at that handler, never structured).
//   `odoo.config` (`handle_odoo_config`, handlers.rs:24517) → `null` when
//     unconfigured, else `{ "url", "db", ... }` (no secrets) — used ONLY for
//     this card's live-detail subtitle (the connected host), never for the
//     dot/status itself (that stays `odoo.status`'s job — see
//     `odoo_card_view`'s own doc comment on why the two Loadables are kept
//     independent rather than merged into one).
//   `google.credentials.get` (`handle_google_credentials_get`,
//     handlers.rs:28230) → `{ "integration_enabled": bool, "effective":
//     "direct"|"apps_script"|"none", "service_account": {...}, "apps_script":
//     {...}, "required_scopes": [...] }`. `"effective"` is already resolved
//     "the same way a real tool call would" (that handler's own comment) —
//     the single authoritative signal this card needs, not a guess derived
//     from the two credential sub-objects.
//   `identity.config_get` (`handle_identity_config_get`, handlers.rs:9892,
//     `identity_table_to_response` at handlers.rs:2934) → `{ "provider":
//     "wiki_cache"|"notion"|"chained", "notion": {...}, "wiki_cache": {...} }`
//     — `provider` always resolves to one of `IDENTITY_PROVIDERS`
//     (handlers.rs:2910; unrecognized/missing config defaults to
//     `"wiki_cache"`), so this card's dot is always green/"使用中" — there is
//     no "identity resolution is off" state server-side (a `wiki_cache`
//     fallback is always live), matching the canvas's own single-state card.

use std::collections::HashSet;

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
use crate::rpc::CallError;
use crate::screens::dashboard::{self, Loadable};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Data model (pure — unit tested without a live `App`) ────────────────

#[derive(Clone, Default, PartialEq, Eq)]
pub struct McpCard {
    pub connected_servers: usize,
    pub agents_using: usize,
}

pub fn parse_mcp_card(v: &Value) -> McpCard {
    let agents = v.get("agents").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut servers: HashSet<String> = HashSet::new();
    let mut agents_using = 0usize;
    for agent in &agents {
        let list = agent.get("servers").and_then(Value::as_array).cloned().unwrap_or_default();
        if !list.is_empty() {
            agents_using += 1;
        }
        for s in &list {
            if let Some(name) = s.get("name").and_then(Value::as_str) {
                servers.insert(name.to_string());
            }
        }
    }
    McpCard { connected_servers: servers.len(), agents_using }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct OdooStatusCard {
    pub connected: bool,
    pub error: Option<String>,
}

pub fn parse_odoo_status_card(v: &Value) -> OdooStatusCard {
    OdooStatusCard {
        connected: v.get("connected").and_then(Value::as_bool).unwrap_or(false),
        error: v.get("error").and_then(Value::as_str).map(str::to_string),
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OdooConfigCard {
    pub url: String,
}

/// `null` (unconfigured) → `None`; a configured-but-blank `url` also
/// degrades to `None` rather than surfacing an empty subtitle fragment.
pub fn parse_odoo_config_card(v: &Value) -> Option<OdooConfigCard> {
    if v.is_null() {
        return None;
    }
    v.get("url").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| OdooConfigCard { url: s.to_string() })
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct GoogleCard {
    pub integration_enabled: bool,
    pub effective: String,
}

pub fn parse_google_card(v: &Value) -> GoogleCard {
    GoogleCard {
        integration_enabled: v.get("integration_enabled").and_then(Value::as_bool).unwrap_or(false),
        effective: v.get("effective").and_then(Value::as_str).unwrap_or("none").to_string(),
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct IdentityCard {
    pub provider: String,
}

pub fn parse_identity_card(v: &Value) -> IdentityCard {
    IdentityCard { provider: v.get("provider").and_then(Value::as_str).unwrap_or("wiki_cache").to_string() }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct IntegrationsState {
    requested: bool,
    pub mcp: Loadable<McpCard>,
    pub odoo_status: Loadable<OdooStatusCard>,
    /// Kept as an INDEPENDENT `Loadable` from `odoo_status` rather than
    /// merged into one struct — mirrors `inbox.rs`'s two-source-per-feed
    /// pattern: `odoo.config`'s only job is the connected-host subtitle
    /// fragment, and a failure/slow-response on it must never blank out
    /// `odoo.status`'s own (authoritative) connected/dot signal. See
    /// `odoo_card_view` for how the two are recombined at render time.
    pub odoo_config: Loadable<Option<OdooConfigCard>>,
    pub google: Loadable<GoogleCard>,
    pub identity: Loadable<IdentityCard>,
}

impl Default for IntegrationsState {
    fn default() -> Self {
        Self::new()
    }
}

impl Global for IntegrationsState {}

impl IntegrationsState {
    pub fn new() -> Self {
        Self {
            requested: false,
            mcp: Loadable::Loading,
            odoo_status: Loadable::Loading,
            odoo_config: Loadable::Loading,
            google: Loadable::Loading,
            identity: Loadable::Loading,
        }
    }
}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<IntegrationsState>() {
        cx.set_global(IntegrationsState::new());
    }
}

// ── Fetch orchestration (mirrors `dashboard.rs`'s multi-card Global-state
// shape — fired from the top of `render`) ─────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<IntegrationsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<IntegrationsState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "mcp.list", json!({}), |cx, result| {
        cx.global_mut::<IntegrationsState>().mcp = result.map(|v| parse_mcp_card(&v)).into();
    });
    spawn_call(cx, tx.clone(), "odoo.status", json!({}), |cx, result| {
        cx.global_mut::<IntegrationsState>().odoo_status = result.map(|v| parse_odoo_status_card(&v)).into();
    });
    spawn_call(cx, tx.clone(), "odoo.config", json!({}), |cx, result| {
        cx.global_mut::<IntegrationsState>().odoo_config = result.map(|v| parse_odoo_config_card(&v)).into();
    });
    spawn_call(cx, tx.clone(), "google.credentials.get", json!({}), |cx, result| {
        cx.global_mut::<IntegrationsState>().google = result.map(|v| parse_google_card(&v)).into();
    });
    spawn_call(cx, tx, "identity.config_get", json!({}), |cx, result| {
        cx.global_mut::<IntegrationsState>().identity = result.map(|v| parse_identity_card(&v)).into();
    });
}

/// Duplicated from `channels.rs`/`inbox.rs`/`dashboard.rs` rather than
/// importing — this crate's own established convention for this ~15-line
/// shape.
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

// ── Per-card view model ───────────────────────────────────────────────

enum CardBody {
    Loading,
    Failed(String),
    Ready { subtitle: SharedString, dot_color: u32, status_label: SharedString },
}

struct CardView {
    /// Single-letter placeholder glyph — this crate has no icon set yet,
    /// same convention `nav.rs`'s `badge_letter` already establishes.
    glyph: &'static str,
    hue: u32,
    title: SharedString,
    body: CardBody,
    /// `RootView::active_page` value this row navigates to on click — one
    /// of the four ids this task's brief hands to the parallel C package.
    target: &'static str,
}

fn mcp_card_view(loadable: &Loadable<McpCard>, locale: Locale) -> CardView {
    let body = match loadable {
        Loadable::Loading => CardBody::Loading,
        Loadable::Failed(e) => CardBody::Failed(e.clone()),
        Loadable::Ready(card) if card.connected_servers > 0 => {
            let servers = card.connected_servers.to_string();
            let agents = card.agents_using.to_string();
            CardBody::Ready {
                subtitle: i18n::tn(locale, "native.integrations.mcp.detail", &[("servers", &servers), ("agents", &agents)]),
                dot_color: theme::SUCCESS,
                status_label: i18n::t(locale, "native.integrations.mcp.status.connected"),
            }
        }
        Loadable::Ready(_) => CardBody::Ready {
            subtitle: i18n::t(locale, "native.integrations.mcp.detailEmpty"),
            dot_color: theme::MUTED_FOREGROUND,
            status_label: i18n::t(locale, "native.integrations.mcp.status.empty"),
        },
    };
    CardView { glyph: "M", hue: theme::BRAND, title: i18n::t(locale, "native.integrations.mcp.title"), body, target: "mcp" }
}

/// Recombines the two independent Loadables — see `IntegrationsState::
/// odoo_config`'s doc comment. `odoo_status` alone decides connected/dot/
/// failed; `odoo_config` (when it happens to already be `Ready`) only ever
/// enriches the subtitle with the connected host, and is never allowed to
/// block or override the status card's own state (a slow/failed
/// `odoo.config` degrades that one subtitle fragment, not the whole card).
fn odoo_card_view(
    status: &Loadable<OdooStatusCard>,
    config: &Loadable<Option<OdooConfigCard>>,
    locale: Locale,
) -> CardView {
    let body = match status {
        Loadable::Loading => CardBody::Loading,
        Loadable::Failed(e) => CardBody::Failed(e.clone()),
        Loadable::Ready(st) if st.connected => {
            let subtitle = match config {
                Loadable::Ready(Some(c)) => i18n::tn(locale, "native.integrations.odoo.detail", &[("url", &c.url)]),
                _ => i18n::t(locale, "native.integrations.odoo.purpose"),
            };
            CardBody::Ready { subtitle, dot_color: theme::SUCCESS, status_label: i18n::t(locale, "native.integrations.odoo.status.connected") }
        }
        Loadable::Ready(st) => {
            // `odoo.config` resolving to `Ready(None)` is the one case that
            // distinguishes "never set up" from "set up but currently
            // failing" — `odoo.config` still `Loading`/`Failed` falls back
            // to the generic not-connected wording rather than blocking.
            if matches!(config, Loadable::Ready(None)) {
                CardBody::Ready {
                    subtitle: i18n::t(locale, "native.integrations.odoo.purpose"),
                    dot_color: theme::MUTED_FOREGROUND,
                    status_label: i18n::t(locale, "native.integrations.odoo.status.notConfigured"),
                }
            } else {
                let subtitle = st
                    .error
                    .as_deref()
                    .map(SharedString::from)
                    .unwrap_or_else(|| i18n::t(locale, "native.integrations.odoo.purpose"));
                CardBody::Ready { subtitle, dot_color: theme::DESTRUCTIVE, status_label: i18n::t(locale, "native.integrations.odoo.status.failed") }
            }
        }
    };
    CardView { glyph: "O", hue: theme::BRAND, title: i18n::t(locale, "native.integrations.odoo.title"), body, target: "odoo" }
}

fn google_card_view(loadable: &Loadable<GoogleCard>, locale: Locale) -> CardView {
    let body = match loadable {
        Loadable::Loading => CardBody::Loading,
        Loadable::Failed(e) => CardBody::Failed(e.clone()),
        Loadable::Ready(g) if !g.integration_enabled => CardBody::Ready {
            subtitle: i18n::t(locale, "native.integrations.google.purpose"),
            dot_color: theme::MUTED_FOREGROUND,
            status_label: i18n::t(locale, "native.integrations.google.status.disabled"),
        },
        Loadable::Ready(g) => {
            let (detail_key, dot, status_key) = match g.effective.as_str() {
                "direct" => (
                    "native.integrations.google.detail.direct",
                    theme::SUCCESS,
                    "native.integrations.google.status.connected",
                ),
                "apps_script" => (
                    "native.integrations.google.detail.appsScript",
                    theme::SUCCESS,
                    "native.integrations.google.status.connected",
                ),
                _ => (
                    "native.integrations.google.detail.none",
                    theme::MUTED_FOREGROUND,
                    "native.integrations.google.status.notConnected",
                ),
            };
            CardBody::Ready { subtitle: i18n::t(locale, detail_key), dot_color: dot, status_label: i18n::t(locale, status_key) }
        }
    };
    CardView {
        glyph: "G",
        hue: theme::WARNING,
        title: i18n::t(locale, "native.integrations.google.title"),
        body,
        target: "googleIntegration",
    }
}

fn identity_card_view(loadable: &Loadable<IdentityCard>, locale: Locale) -> CardView {
    let body = match loadable {
        Loadable::Loading => CardBody::Loading,
        Loadable::Failed(e) => CardBody::Failed(e.clone()),
        Loadable::Ready(id) => {
            let provider_key = match id.provider.as_str() {
                "notion" => "native.integrations.identity.provider.notion",
                "chained" => "native.integrations.identity.provider.chained",
                _ => "native.integrations.identity.provider.wikiCache",
            };
            let provider_label = i18n::t(locale, provider_key).to_string();
            CardBody::Ready {
                subtitle: i18n::tn(locale, "native.integrations.identity.detail", &[("provider", &provider_label)]),
                dot_color: theme::SUCCESS,
                status_label: i18n::t(locale, "native.integrations.identity.status.active"),
            }
        }
    };
    CardView {
        glyph: "I",
        hue: theme::MUTED_FOREGROUND,
        title: i18n::t(locale, "native.integrations.identity.title"),
        body,
        target: "identity",
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("integrations-page")
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

    let (mcp, odoo_status, odoo_config, google, identity) = {
        let g = cx.global::<IntegrationsState>();
        (g.mcp.clone(), g.odoo_status.clone(), g.odoo_config.clone(), g.google.clone(), g.identity.clone())
    };

    let views = vec![
        mcp_card_view(&mcp, locale),
        odoo_card_view(&odoo_status, &odoo_config, locale),
        google_card_view(&google, locale),
        identity_card_view(&identity, locale),
    ];

    let mut list = div().flex().flex_col().gap_2();
    for view in views {
        list = list.child(integration_row(view, locale, cx));
    }

    div()
        .id("integrations-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        // ── Header: title/subtitle only — no header actions in the canvas ─
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
                        .child(i18n::t(locale, "native.integrations.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.integrations.subtitle")),
                ),
        )
        // ── Four non-peer status cards ───────────────────────────────
        .child(list)
        // ── Footer hint (canvas's own static caption) ────────────────
        .child(
            div()
                .w_full()
                .text_center()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                .child(i18n::t(locale, "native.integrations.footerHint")),
        )
}

fn integration_row(view: CardView, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let target = view.target;

    let (subtitle_el, dot_color, status_label): (Div, u32, SharedString) = match view.body {
        CardBody::Loading => (
            div().pt_1().child(skeleton(px(200.), px(12.))),
            theme::MUTED_FOREGROUND,
            i18n::t(locale, "native.integrations.loading"),
        ),
        CardBody::Failed(msg) => {
            (div().child(dashboard::error_row(locale, &msg)), theme::DESTRUCTIVE, i18n::t(locale, "native.integrations.loadFailed"))
        }
        CardBody::Ready { subtitle, dot_color, status_label } => (
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(subtitle),
            dot_color,
            status_label,
        ),
    };

    div()
        .id(target)
        .flex()
        .items_center()
        .gap_3p5()
        .px(px(18.))
        .py(px(15.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .cursor_pointer()
        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(
            div()
                .size(px(42.))
                .flex_shrink_0()
                .rounded(px(theme::RADIUS_LG))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::alpha(view.hue, 0.14))
                .text_color(theme::alpha(view.hue, 1.0))
                .text_size(px(15.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(view.glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_BASE))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(view.title),
                )
                .child(subtitle_el),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .flex_shrink_0()
                .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(status_label)),
        )
        .child(div().flex_shrink_0().text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.6)).child(">"))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.active_page = target;
            cx.notify();
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mcp_card_counts_distinct_servers_and_agents_using() {
        let v = json!({ "agents": [
            { "agent_id": "a", "servers": [ { "name": "playwright" }, { "name": "notion" } ] },
            { "agent_id": "b", "servers": [ { "name": "playwright" } ] },
            { "agent_id": "c", "servers": [] },
        ]});
        let card = parse_mcp_card(&v);
        assert_eq!(card.connected_servers, 2); // playwright + notion, deduped
        assert_eq!(card.agents_using, 2); // a and b, not c
    }

    #[test]
    fn parse_mcp_card_missing_agents_is_zero_not_a_panic() {
        let card = parse_mcp_card(&json!({}));
        assert_eq!(card.connected_servers, 0);
        assert_eq!(card.agents_using, 0);
    }

    #[test]
    fn parse_odoo_status_card_reads_connected_and_error() {
        let c = parse_odoo_status_card(&json!({ "connected": true }));
        assert!(c.connected);
        assert_eq!(c.error, None);
        let c = parse_odoo_status_card(&json!({ "connected": false, "error": "Connection failed" }));
        assert!(!c.connected);
        assert_eq!(c.error.as_deref(), Some("Connection failed"));
    }

    #[test]
    fn parse_odoo_config_card_null_is_none() {
        assert!(parse_odoo_config_card(&json!(null)).is_none());
    }

    #[test]
    fn parse_odoo_config_card_reads_url() {
        let c = parse_odoo_config_card(&json!({ "url": "https://mycompany.odoo.com", "db": "x" })).unwrap();
        assert_eq!(c.url, "https://mycompany.odoo.com");
    }

    #[test]
    fn parse_odoo_config_card_blank_url_degrades_to_none() {
        assert!(parse_odoo_config_card(&json!({ "url": "" })).is_none());
    }

    #[test]
    fn parse_google_card_reads_effective_and_enabled() {
        let g = parse_google_card(&json!({ "integration_enabled": true, "effective": "apps_script" }));
        assert!(g.integration_enabled);
        assert_eq!(g.effective, "apps_script");
    }

    #[test]
    fn parse_google_card_missing_effective_defaults_to_none() {
        let g = parse_google_card(&json!({}));
        assert_eq!(g.effective, "none");
        assert!(!g.integration_enabled);
    }

    #[test]
    fn parse_identity_card_defaults_to_wiki_cache() {
        assert_eq!(parse_identity_card(&json!({})).provider, "wiki_cache");
        assert_eq!(parse_identity_card(&json!({ "provider": "notion" })).provider, "notion");
    }
}
