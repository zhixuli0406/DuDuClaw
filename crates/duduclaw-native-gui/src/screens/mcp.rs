// WP-S5b1-C — Screen "工具伺服器（MCP）" (`nav.rs` id `mcp`, once the parallel
// A/B workstream wires it — this pass adds the screen + its `screens/mod.rs`
// line only, per this task's own "側欄/導覽/shell.rs 不歸你動" boundary). An
// "整合" (Integrations) drill-down leaf: no own sidebar row, breadcrumb only.
//
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/Mcp.dc.
// html` — breadcrumb → header (title/subtitle + 從網址匯入/新增伺服器 buttons)
// → "已連接的伺服器" boxed-list → "已授權服務（OAuth）" boxed-list → a single
// "存取金鑰管理 →" link row. Functional reference: `web/src/pages/McpPage.tsx`
// (1454 lines covering add/import/marketplace flows this drill-down does NOT
// reproduce — this page is read-only display of the two lists + the link).
//
// ── RPC shapes (verified against `crates/duduclaw-gateway/src/handlers.rs`,
// not guessed) ────────────────────────────────────────────────────────────
//   `mcp.list` (dispatch ~L6430, handler `handle_mcp_list` ~L27821,
//     `require_admin!()`) → `{ agents: [{ agent_id, servers: [{ name,
//     command, args, env }] }], catalog: [...] }`. Read directly from each
//     agent's `.mcp.json` — **no live connection/health field exists** (this
//     is a static config read, not a health probe).
//   `mcp.oauth.providers` (dispatch ~L6449, handler ~L27978,
//     `require_admin!()`) → `{ providers: [{ provider_id, name, scopes,
//     configured, client_id, has_client_secret, client_secret_masked,
//     status ("authenticated"|"expired"|"none"), expires_at, ... }] }`.
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. No per-server green/grey connectivity dot. The canvas draws one — but
//    `mcp.list` has no live-health field to back it (see above); a colored
//    dot would silently claim liveness data this RPC does not provide. Each
//    row instead shows a neutral "transport" badge (`stdio`/`http`,
//    genuinely derived from whether `command` looks like a URL — real
//    inference from real data, not fabricated) and, when more than one
//    agent has servers configured, which agent owns the row.
// 2. No "顯示中的員工" (agent filter) selector. The canvas's dropdown implies
//    a working per-agent filter; this pass shows every agent's servers
//    flattened into one list with an agent tag per row instead — the same
//    information, without inventing a filter interaction out of scope for
//    this drill-down.
// 3. OAuth connect/disconnect and the two header buttons (從網址匯入/新增
//    伺服器) are rendered but NOT wired to their RPCs this pass (`mcp.oauth.
//    start`/`mcp.oauth.revoke`/`mcp.update` all exist and are real, but
//    wiring a working OAuth consent round-trip + polling loop, or a working
//    add-server form, is out of this drill-down's stated scope — "已連接...
//    boxed-list＋OAuth 已授權服務區塊＋…連結"). `mds_gpui::button`'s
//    `disabled: true` path omits the click handler entirely (see that
//    module's own doc comment) rather than attaching a handler that would
//    silently do nothing.
// 4. "存取金鑰管理 →" IS live (WP-S5b2-F, S5b 第二波) — the target page now
//    exists (`screens::mcp_keys`, id `mcpKeys`). Clicking the row sets
//    `active_page = "mcpKeys"`; the `shell.rs` match arm that actually
//    routes that id is a sibling package's scope per WP-S5b2-F's own "側欄
//    /nav/shell.rs 不歸你動" boundary — until it lands, `shell.rs`'s
//    generic fallback renders an honest placeholder for `mcpKeys` (same
//    "unmatched id degrades, never panics" property `settings_common::
//    breadcrumb`'s own doc comment already relies on for `"integrations"`).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::settings_common::{boxed_group, breadcrumb, kv_row, section_label};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct McpServerRow {
    pub agent_id: String,
    pub name: String,
    pub command: String,
}

#[derive(Clone)]
pub struct OAuthProviderRow {
    pub provider_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    /// Raw `"authenticated"|"expired"|"none"` from `mcp.oauth.providers`.
    pub status: String,
}

pub struct McpState {
    requested: bool,
    pub servers: Loadable<Vec<McpServerRow>>,
    pub oauth: Loadable<Vec<OAuthProviderRow>>,
}

impl McpState {
    fn new() -> Self {
        Self { requested: false, servers: Loadable::Loading, oauth: Loadable::Loading }
    }
}

impl Global for McpState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<McpState>() {
        cx.set_global(McpState::new());
    }
}

// ── Response parsing ──────────────────────────────────────────────────

pub fn parse_mcp_list(v: &Value) -> Vec<McpServerRow> {
    let mut out = Vec::new();
    let Some(agents) = v.get("agents").and_then(Value::as_array) else {
        return out;
    };
    for agent in agents {
        let agent_id = agent.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(servers) = agent.get("servers").and_then(Value::as_array) else {
            continue;
        };
        for s in servers {
            let name = s.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let command = s.get("command").and_then(Value::as_str).unwrap_or("").to_string();
            out.push(McpServerRow { agent_id: agent_id.clone(), name, command });
        }
    }
    out
}

pub fn parse_oauth_providers(v: &Value) -> Vec<OAuthProviderRow> {
    let mut out = Vec::new();
    let Some(providers) = v.get("providers").and_then(Value::as_array) else {
        return out;
    };
    for p in providers {
        let provider_id = p.get("provider_id").and_then(Value::as_str).unwrap_or("").to_string();
        if provider_id.is_empty() {
            continue;
        }
        let name = p.get("name").and_then(Value::as_str).unwrap_or(&provider_id).to_string();
        let scopes: Vec<String> = p
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let status = p.get("status").and_then(Value::as_str).unwrap_or("none").to_string();
        out.push(OAuthProviderRow { provider_id, name, scopes, status });
    }
    out
}

/// Real inference from the real `command` field — a stdio MCP server's
/// `command` is a shell command; an http/sse server's `command` is the
/// endpoint URL itself (the design canvas's `odoo-bridge` row shows exactly
/// this: `command == "https://mcp.duduclaw.local/odoo"`). No separate
/// transport field exists on `McpServerDef` to read instead.
pub fn transport_label(command: &str) -> &'static str {
    if command.starts_with("http://") || command.starts_with("https://") {
        "http"
    } else {
        "stdio"
    }
}

// ── Fetch orchestration ──────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<McpState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<McpState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "mcp.list", json!({}), |cx, result| {
        cx.global_mut::<McpState>().servers = result.map(|v| parse_mcp_list(&v)).into();
    });
    spawn_call(cx, tx, "mcp.oauth.providers", json!({}), |cx, result| {
        cx.global_mut::<McpState>().oauth = result.map(|v| parse_oauth_providers(&v)).into();
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
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Row builders ───────────────────────────────────────────────────────

fn server_row(row: &McpServerRow, multi_agent: bool, is_last: bool) -> Div {
    let subtitle = div()
        .text_size(px(theme::TEXT_XS))
        .font_family("SF Mono")
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .overflow_hidden()
        .child(SharedString::from(row.command.clone()));

    let mut left = div().flex_1().min_w_0().flex().flex_col().gap_0p5().child(
        div()
            .text_size(px(theme::TEXT_SM))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(SharedString::from(row.name.clone())),
    );
    left = left.child(subtitle);

    let mut r = div().flex().items_center().gap_3().px_4().py_2p5().child(left).child(badge(
        transport_label(&row.command),
        BadgeVariant::Secondary,
    ));
    if multi_agent {
        r = r.child(badge(SharedString::from(row.agent_id.clone()), BadgeVariant::Outline));
    }
    if is_last {
        r
    } else {
        r.border_b_1().border_color(theme::border())
    }
}

fn oauth_row(locale: Locale, row: &OAuthProviderRow, is_last: bool) -> Div {
    let (status_variant, status_key) = match row.status.as_str() {
        "authenticated" => (BadgeVariant::Success, "native.mcp.oauth.status.authenticated"),
        "expired" => (BadgeVariant::Warning, "native.mcp.oauth.status.expired"),
        _ => (BadgeVariant::Secondary, "native.mcp.oauth.status.none"),
    };
    let subtitle: SharedString = if row.scopes.is_empty() {
        i18n::t(locale, status_key)
    } else {
        i18n::t1(locale, "native.mcp.oauth.scopesLine", "scopes", &row.scopes.join(", "))
    };

    let left = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(row.name.clone())),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(subtitle));

    let action_key = if row.status == "authenticated" {
        "native.mcp.oauth.action.disconnect"
    } else {
        "native.mcp.oauth.action.connect"
    };

    let r = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(left)
        .child(badge(i18n::t(locale, status_key), status_variant))
        .child(button(
            SharedString::from(format!("mcp-oauth-action-{}", row.provider_id)),
            i18n::t(locale, action_key),
            ButtonVariant::Secondary,
            true, // see header comment §3 — not wired this pass
            None,
            |_ev, _window, _cx| {},
        ));
    if is_last {
        r
    } else {
        r.border_b_1().border_color(theme::border())
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let (servers, oauth) = {
        let s = cx.global::<McpState>();
        (s.servers.clone(), s.oauth.clone())
    };

    let header = div()
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
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.mcp.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.mcp.subtitle")),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(button(
                    "mcp-import-url",
                    i18n::t(locale, "native.mcp.importFromUrl"),
                    ButtonVariant::Secondary,
                    true,
                    None,
                    |_ev, _window, _cx| {},
                ))
                .child(button(
                    "mcp-add-server",
                    i18n::t(locale, "native.mcp.addServer"),
                    ButtonVariant::Primary,
                    true,
                    None,
                    |_ev, _window, _cx| {},
                )),
        );

    let servers_section = {
        let body = match &servers {
            Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(560.), px(44.))).child(skeleton(px(560.), px(44.))),
            Loadable::Failed(err) => div().p_4().child(
                div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone())),
            ),
            Loadable::Ready(rows) if rows.is_empty() => {
                div().child(empty_state("🔌", i18n::t(locale, "native.mcp.empty"), None, None::<Div>))
            }
            Loadable::Ready(rows) => {
                let multi_agent = rows.iter().map(|r| &r.agent_id).collect::<std::collections::HashSet<_>>().len() > 1;
                let n = rows.len();
                boxed_group(
                    rows.iter()
                        .enumerate()
                        .map(|(i, r)| server_row(r, multi_agent, i + 1 == n))
                        .collect(),
                )
            }
        };
        div().flex().flex_col().child(section_label(i18n::t(locale, "native.mcp.connectedServers"))).child(body)
    };

    let oauth_section = {
        let body = match &oauth {
            Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(560.), px(44.))).child(skeleton(px(560.), px(44.))),
            Loadable::Failed(err) => div().p_4().child(
                div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone())),
            ),
            Loadable::Ready(rows) if rows.is_empty() => {
                div().child(empty_state("🔑", i18n::t(locale, "native.mcp.oauth.empty"), None, None::<Div>))
            }
            Loadable::Ready(rows) => {
                let n = rows.len();
                boxed_group(rows.iter().enumerate().map(|(i, r)| oauth_row(locale, r, i + 1 == n)).collect())
            }
        };
        div().flex().flex_col().child(section_label(i18n::t(locale, "native.mcp.oauthServices"))).child(body)
    };

    // "存取金鑰管理 →" — live, see header comment §4 (WP-S5b2-F).
    let keys_link_row = boxed_group(vec![kv_row(
        i18n::t(locale, "native.mcp.keysManagement"),
        div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.mcp.keysManagementArrow")),
        true,
    )])
    .id("mcp-keys-link")
    .cursor_pointer()
    .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.3)))
    .on_click(cx.listener(|this, _ev, _window, cx| {
        this.active_page = "mcpKeys";
        cx.notify();
    }));

    div()
        .id("mcp-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(860.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(breadcrumb("mcp-breadcrumb", locale, i18n::t(locale, "native.mcp.breadcrumb"), cx))
                .child(header)
                .child(servers_section)
                .child(oauth_section)
                .child(keys_link_row),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mcp_list_flattens_every_agent() {
        let v = json!({
            "agents": [
                { "agent_id": "duduclaw", "servers": [
                    { "name": "wiki-search", "command": "npx duduclaw-wiki-mcp", "args": [], "env": {} },
                    { "name": "playwright", "command": "npx @playwright/mcp@latest", "args": [], "env": {} },
                ]},
                { "agent_id": "sales-bot", "servers": [
                    { "name": "odoo-bridge", "command": "https://mcp.duduclaw.local/odoo", "args": [], "env": {} },
                ]},
            ],
            "catalog": [],
        });
        let rows = parse_mcp_list(&v);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].agent_id, "duduclaw");
        assert_eq!(rows[2].name, "odoo-bridge");
    }

    #[test]
    fn parse_mcp_list_missing_agents_is_empty_not_a_panic() {
        assert!(parse_mcp_list(&json!({})).is_empty());
    }

    #[test]
    fn parse_mcp_list_skips_agents_with_no_servers_array() {
        let v = json!({ "agents": [{ "agent_id": "x" }] });
        assert!(parse_mcp_list(&v).is_empty());
    }

    #[test]
    fn transport_label_infers_http_from_url_command() {
        assert_eq!(transport_label("https://mcp.duduclaw.local/odoo"), "http");
        assert_eq!(transport_label("http://localhost:9999/mcp"), "http");
    }

    #[test]
    fn transport_label_defaults_to_stdio_for_shell_commands() {
        assert_eq!(transport_label("npx @playwright/mcp@latest"), "stdio");
    }

    #[test]
    fn parse_oauth_providers_reads_every_field() {
        let v = json!({ "providers": [
            { "provider_id": "notion", "name": "Notion", "scopes": ["read_content", "read_comments"],
              "status": "authenticated", "configured": true },
            { "provider_id": "github", "name": "GitHub", "scopes": [], "status": "none", "configured": false },
        ]});
        let rows = parse_oauth_providers(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].provider_id, "notion");
        assert_eq!(rows[0].scopes, vec!["read_content".to_string(), "read_comments".to_string()]);
        assert_eq!(rows[1].status, "none");
    }

    #[test]
    fn parse_oauth_providers_missing_array_is_empty_not_a_panic() {
        assert!(parse_oauth_providers(&json!({})).is_empty());
    }

    #[test]
    fn parse_oauth_providers_skips_entries_without_provider_id() {
        let v = json!({ "providers": [{ "name": "x" }] });
        assert!(parse_oauth_providers(&v).is_empty());
    }
}
