// WP-S5b2-F (S5b 第二波) — Screen "存取金鑰" (McpKeysPage). No `nav.rs`
// entry of its own — reached only via `screens::mcp`'s "存取金鑰管理 →"
// row (wired this pass, see that module's own header comment §4 update
// below) or the debug-page override. A deeper "整合" drill-down leaf than
// `mcp`/`odoo`/`google_integration`/`identity` (WP-S5b1-C's four pages,
// `active_page == "mcp"` etc.) — those use `settings_common::breadcrumb`'s
// fixed two-segment "整合 › {page}" shape; this page needs FOUR segments
// (管理›整合›工具伺服器（MCP）›存取金鑰, per this task's brief), so it
// carries its own small local breadcrumb (`breadcrumb_trail` below) rather
// than widening that shared helper's signature for one caller — same
// "duplicate, don't couple two batches through a shared module" reasoning
// `settings_common.rs`'s own header comment already applies to itself.
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/McpKeys.dc.
// html` (B4) — breadcrumb → header (title/subtitle + 建立金鑰) → warning
// banner → 唯讀表格（用戶端 ID/金鑰/權限範圍/建立時間/外部/操作）.
// Functional reference: `web/src/pages/McpKeysPage.tsx`.
//
// ── RPC shapes (verified against `crates/duduclaw-gateway/src/handlers.rs`,
// not guessed) ────────────────────────────────────────────────────────────
//   `mcp_keys.list` (dispatch ~L5549, handler `handle_mcp_keys_list`
//     ~L10252, `require_admin!()`) → `{"keys": [{"masked","client_id",
//     "is_external","created_at","scopes":[...],"rotate_recommended"}]}`.
//     The cleartext key is NEVER returned here — only `masked` (e.g.
//     `"ddc-••••8f3a"`), matching the canvas exactly. A non-admin caller
//     gets a real error frame (surfaces as this page's `Loadable::Failed`),
//     not a silently-empty table.
//   `mcp_keys.create` (~L5553/~L10299) and `mcp_keys.revoke`
//     (~L5557/~L10417) both exist and are real, but this task's brief is
//     explicit — "建立/撤銷決策類組裝不真按": both actions render fully
//     assembled (correct copy, correct placement, the exact revoke-needs-
//     full-key warning banner from the canvas) as `disabled: true`
//     `mds_gpui::button`s, which per that primitive's own doc comment
//     attach NO click handler at all (not a half-wired one). Same
//     established pattern `screens::mcp`'s two header buttons and its
//     OAuth connect/disconnect actions already use.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::settings_common::boxed_group;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct McpKeyRow {
    pub masked: String,
    pub client_id: String,
    pub is_external: bool,
    /// RFC3339, rendered verbatim date-only (`chrono` parse; malformed ⇒
    /// "—", never a fabricated date).
    pub created_at: String,
    pub scopes: Vec<String>,
    pub rotate_recommended: bool,
}

pub fn parse_mcp_keys_list(v: &Value) -> Vec<McpKeyRow> {
    v.get("keys")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|k| {
                    let masked = k.get("masked")?.as_str()?.to_string();
                    let client_id = k.get("client_id").and_then(Value::as_str).unwrap_or("").to_string();
                    let is_external = k.get("is_external").and_then(Value::as_bool).unwrap_or(false);
                    let created_at = k.get("created_at").and_then(Value::as_str).unwrap_or("").to_string();
                    let scopes: Vec<String> = k
                        .get("scopes")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    let rotate_recommended = k.get("rotate_recommended").and_then(Value::as_bool).unwrap_or(false);
                    Some(McpKeyRow { masked, client_id, is_external, created_at, scopes, rotate_recommended })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `"2026-08-15"` from an RFC3339 timestamp — `"—"` when unparseable
/// (never a fabricated date).
pub fn format_created_date(created_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| "—".to_string())
}

// ── State ──────────────────────────────────────────────────────────────

pub struct McpKeysState {
    requested: bool,
    pub keys: Loadable<Vec<McpKeyRow>>,
}

impl McpKeysState {
    fn new() -> Self {
        Self { requested: false, keys: Loadable::Loading }
    }
}

impl Global for McpKeysState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<McpKeysState>() {
        cx.set_global(McpKeysState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<McpKeysState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<McpKeysState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "mcp_keys.list", json!({}), |cx, result| {
        cx.global_mut::<McpKeysState>().keys = result.map(|v| parse_mcp_keys_list(&v)).into();
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

// ── Breadcrumb (four segments — see this module's header comment) ────────

fn breadcrumb_trail(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let crumb = |id: &'static str, label: SharedString, target: &'static str, cx: &mut Context<RootView>| {
        div()
            .id(id)
            .cursor_pointer()
            .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
            .child(label)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.active_page = target;
                cx.notify();
            }))
    };
    let sep = || SharedString::from("›");
    div()
        .id("mcpkeys-breadcrumb")
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(crumb("mcpkeys-crumb-manage", i18n::t(locale, "nav.manage"), "manage", cx))
        .child(sep())
        .child(crumb("mcpkeys-crumb-integrations", i18n::t(locale, "nav.integrations"), "integrations", cx))
        .child(sep())
        .child(crumb("mcpkeys-crumb-mcp", i18n::t(locale, "native.mcp.breadcrumb"), "mcp", cx))
        .child(sep())
        .child(div().overflow_hidden().child(i18n::t(locale, "mcpKeys.breadcrumb")))
}

// ── Rows ───────────────────────────────────────────────────────────────

fn key_row(locale: Locale, k: &McpKeyRow, is_last: bool) -> Div {
    let mut left = div().flex_1().min_w_0().flex().flex_col().gap_0p5().child(
        div()
            .text_size(px(theme::TEXT_SM))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(SharedString::from(if k.client_id.is_empty() { "—".to_string() } else { k.client_id.clone() })),
    );
    left = left.child(
        div()
            .font_family("SF Mono")
            .text_size(px(11.5))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(SharedString::from(k.masked.clone())),
    );

    let mut scopes_row = div().flex().flex_wrap().gap_1();
    for s in &k.scopes {
        scopes_row = scopes_row.child(badge(SharedString::from(s.clone()), BadgeVariant::Secondary));
    }

    let external = if k.is_external {
        badge(i18n::t(locale, "mcpKeys.col.external"), BadgeVariant::Warning)
    } else {
        div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "mcpKeys.internal"))
    };

    let mut actions = div().flex().items_center().gap_2();
    if k.rotate_recommended {
        actions = actions.child(badge(i18n::t(locale, "mcpKeys.rotate"), BadgeVariant::Warning));
    }
    actions = actions.child(button(
        SharedString::from(format!("mcpkeys-revoke-{}", k.masked)),
        i18n::t(locale, "mcpKeys.revoke"),
        ButtonVariant::Destructive,
        true, // decision-type action — assembled, not wired; see module header §
        None,
        |_ev, _window, _cx| {},
    ));

    let mut row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(left)
        .child(scopes_row)
        .child(div().w(px(90.)).flex_shrink_0().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format_created_date(&k.created_at)))
        .child(div().w(px(70.)).flex_shrink_0().child(external))
        .child(actions);
    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let keys = cx.global::<McpKeysState>().keys.clone();

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "mcpKeys.breadcrumb")))
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "mcpKeys.desc"))),
        )
        .child(button(
            "mcpkeys-create",
            i18n::t(locale, "mcpKeys.create"),
            ButtonVariant::Primary,
            true, // decision-type action — assembled, not wired; see module header §
            None,
            |_ev, _window, _cx| {},
        ));

    let warning = div()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::WARNING, 0.10))
        .border_1()
        .border_color(theme::alpha(theme::WARNING, 0.3))
        .px_3p5()
        .py_2()
        .text_size(px(11.5))
        .text_color(theme::alpha(theme::WARNING, 1.0))
        .child(i18n::t(locale, "mcpKeys.revokeBanner"));

    let body = match &keys {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(44.))).child(skeleton(px(760.), px(44.))),
        Loadable::Failed(err) => div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(rows) if rows.is_empty() => div().child(empty_state("🔑", i18n::t(locale, "mcpKeys.empty"), None, None::<Div>)),
        Loadable::Ready(rows) => {
            let header_row = div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .bg(theme::alpha(theme::MUTED, 0.35))
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(div().flex_1().child(i18n::t(locale, "mcpKeys.col.clientId")))
                .child(div().flex_1().child(i18n::t(locale, "mcpKeys.col.scopes")))
                .child(div().w(px(90.)).flex_shrink_0().child(i18n::t(locale, "mcpKeys.col.created")))
                .child(div().w(px(70.)).flex_shrink_0().child(i18n::t(locale, "mcpKeys.col.external")))
                .child(div().flex_shrink_0().child(""));
            let n = rows.len();
            boxed_group(
                std::iter::once(header_row)
                    .chain(rows.iter().enumerate().map(|(i, k)| key_row(locale, k, i + 1 == n)))
                    .collect(),
            )
        }
    };

    div()
        .id("mcp-keys-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(900.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(breadcrumb_trail(locale, cx))
                .child(header)
                .child(warning)
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_mcp_keys_list_reads_every_field() {
        let v = json!({ "keys": [
            { "masked": "ddc-••••8f3a", "client_id": "odoo-sync", "is_external": false,
              "created_at": "2026-07-02T00:00:00Z", "scopes": ["memory:read", "memory:write"],
              "rotate_recommended": false },
            { "masked": "ddc-••••1c9e", "client_id": "n8n-workflow", "is_external": true,
              "created_at": "2026-08-01T00:00:00Z", "scopes": ["task:read"], "rotate_recommended": true },
        ]});
        let rows = parse_mcp_keys_list(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].client_id, "odoo-sync");
        assert_eq!(rows[0].scopes.len(), 2);
        assert!(!rows[0].rotate_recommended);
        assert!(rows[1].is_external);
        assert!(rows[1].rotate_recommended);
    }

    #[test]
    fn parse_mcp_keys_list_missing_array_is_empty_not_panicking() {
        assert!(parse_mcp_keys_list(&json!({})).is_empty());
        assert!(parse_mcp_keys_list(&json!(null)).is_empty());
    }

    #[test]
    fn parse_mcp_keys_list_skips_entries_without_masked() {
        let v = json!({ "keys": [{ "client_id": "x" }] });
        assert!(parse_mcp_keys_list(&v).is_empty());
    }

    #[test]
    fn format_created_date_is_date_only_or_dash() {
        assert_eq!(format_created_date("2026-08-15T09:30:00Z"), "2026-08-15");
        assert_eq!(format_created_date("not-a-date"), "—");
        assert_eq!(format_created_date(""), "—");
    }
}
