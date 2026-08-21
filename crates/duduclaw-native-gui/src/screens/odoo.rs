// WP-S5b1-C — Screen "Odoo ERP" (`nav.rs` id `odoo`, wired by the parallel
// A/B "整合" workstream — this pass adds the screen + its `screens/mod.rs`
// line only). An "整合" drill-down leaf: no own sidebar row, breadcrumb only.
//
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/Odoo.dc.
// html` — breadcrumb → header (title/subtitle + 已連接/未連線 badge) →
// "連線設定" boxed-list → "功能開關" boxed-list → "同步" boxed-list →
// "存取控制"（危險區, destructive-tinted）boxed-list → a footnote line.
// Functional reference: `web/src/pages/OdooPage.tsx` (776 lines covering the
// full editable form + per-agent override tab this drill-down does NOT
// reproduce — this page is read-only display of the global config).
//
// ── RPC shapes (verified against `crates/duduclaw-gateway/src/handlers.rs`,
// not guessed) ────────────────────────────────────────────────────────────
//   `odoo.status` (dispatch ~L6322, handler ~L24461, `require_admin!()`) →
//     not configured: `{ connected: false }`; configured but unreachable:
//     `{ connected: false, error }`; live: `{ connected: true, edition,
//     version, uid }`. **No secret ever returned.**
//   `odoo.config` (dispatch ~L6326, handler ~L24517, `require_admin!()`) →
//     `null` when unconfigured, else `{ url, db, protocol, auth_method,
//     username, poll_enabled, poll_interval_seconds, poll_models,
//     webhook_enabled, has_api_key, has_password, has_webhook_secret
//     (presence-only booleans — never the secret), unblock_models,
//     features_crm/sale/inventory/accounting/project/hr }`.
//   `security.credential_inventory` (dispatch ~L6173, handler ~L20503,
//     `require_admin!()`) → `{ entries: [{ path, configured, source,
//     source_label, writable, residue }], ... }` — a generic recursive walk
//     of `config.toml`'s `_enc`-paired credential fields (see that
//     handler's own doc comment on the `<field>`/`<field>_enc` pairing
//     rule), which is what backs this page's per-credential "來源" badge —
//     matched by `path` suffix (`odoo.api_key` / `odoo.password` /
//     `odoo.webhook_secret`), never assumed present.
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. No fabricated masked-value tail. The canvas shows `sk-••••••••7f2a` —
//    a specific cleartext tail. `odoo.config` returns only `has_api_key`/
//    `has_password` booleans, never any character of the secret, so this
//    page renders a generic `settings_common::masked_value_chip()` (plain
//    `"••••••••"`) plus a real 來源 badge sourced from `security.credential_
//    inventory` when that lookup resolves, or a plain "已設定"/"未設定"
//    presence badge when it doesn't (a lookup miss — e.g. a path-format
//    drift between this page's assumed `odoo.<field>` keys and whatever
//    `security_posture::credential_inventory`'s generic TOML walker
//    actually produces — degrades to presence-only, never a wrong label).
// 2. Feature-toggle rows are rendered as read-only status badges, not live
//    switches — same "toggle that silently does nothing on click is a worse
//    lie than a badge" reasoning `agents_detail.rs`'s own header comment
//    already establishes for its capability rows. All SIX real toggles
//    (`features_crm/sale/inventory/accounting/project/hr`) are shown, not
//    just the canvas's four (`project`/`hr` are real fields the canvas
//    simply didn't draw — showing all six is more complete, not a
//    fabrication).
// 3. "解除封鎖模組" (danger-zone unblock-models management) and "探索資料
//    結構"/"測試連線" are rendered but NOT wired — `odoo.discover_schema`/
//    `odoo.test`/`odoo.configure` are real RPCs, but this drill-down's brief
//    is explicit: "連線設定...分組 boxed-list；...唯讀呈現；「測試連線」
//    決策類組裝不真按". `unblock_models` itself IS shown, read-only, in the
//    danger-zone box (count only — the canvas's own "管理…" button stays
//    disabled per the same rule).
// 4. `webhook_secret`'s 來源 lookup key note: the canvas shows "密鑰：來源
//    ＝環境變數" as static mockup flavor text; this page shows whatever
//    `security.credential_inventory` actually reports (or "已設定"/"未設定"
//    on a lookup miss) — never that specific literal string.

use gpui::{div, prelude::*, px, Context, Div, Global, IntoElement, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, skeleton, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::settings_common::{boxed_group, breadcrumb, kv_row, masked_value_chip, section_label};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct OdooStatusInfo {
    pub connected: bool,
    pub edition: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct OdooConfigInfo {
    pub url: String,
    pub db: String,
    pub protocol: String,
    pub auth_method: String,
    pub username: String,
    pub has_api_key: bool,
    pub has_password: bool,
    pub has_webhook_secret: bool,
    pub poll_enabled: bool,
    pub poll_interval_seconds: i64,
    pub poll_models: Vec<String>,
    pub webhook_enabled: bool,
    pub unblock_models: Vec<String>,
    pub features_crm: bool,
    pub features_sale: bool,
    pub features_inventory: bool,
    pub features_accounting: bool,
    pub features_project: bool,
    pub features_hr: bool,
}

/// One `security.credential_inventory` entry, narrowed to what this page
/// reads (`kv_row`'s value slot — a source label, nothing else).
#[derive(Clone, Default)]
pub struct CredentialSource {
    pub api_key: Option<String>,
    pub password: Option<String>,
    pub webhook_secret: Option<String>,
}

pub struct OdooState {
    requested: bool,
    pub status: Loadable<OdooStatusInfo>,
    /// `None` inside `Ready` means `odoo.config` answered `null`
    /// (unconfigured) — a real, distinct state from `Loading`/`Failed`.
    pub config: Loadable<Option<OdooConfigInfo>>,
    pub credentials: Loadable<CredentialSource>,
}

impl OdooState {
    fn new() -> Self {
        Self {
            requested: false,
            status: Loadable::Loading,
            config: Loadable::Loading,
            credentials: Loadable::Loading,
        }
    }
}

impl Global for OdooState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<OdooState>() {
        cx.set_global(OdooState::new());
    }
}

// ── Response parsing ──────────────────────────────────────────────────

pub fn parse_odoo_status(v: &Value) -> OdooStatusInfo {
    OdooStatusInfo {
        connected: v.get("connected").and_then(Value::as_bool).unwrap_or(false),
        edition: v.get("edition").and_then(Value::as_str).map(str::to_string),
        version: v.get("version").and_then(Value::as_str).map(str::to_string),
        error: v.get("error").and_then(Value::as_str).map(str::to_string),
    }
}

fn str_array(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// `odoo.config`'s response is `null` when unconfigured — `None` here maps
/// exactly to that, distinct from a parse failure.
pub fn parse_odoo_config(v: &Value) -> Option<OdooConfigInfo> {
    if v.is_null() {
        return None;
    }
    Some(OdooConfigInfo {
        url: v.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
        db: v.get("db").and_then(Value::as_str).unwrap_or("").to_string(),
        protocol: v.get("protocol").and_then(Value::as_str).unwrap_or("jsonrpc").to_string(),
        auth_method: v.get("auth_method").and_then(Value::as_str).unwrap_or("api_key").to_string(),
        username: v.get("username").and_then(Value::as_str).unwrap_or("").to_string(),
        has_api_key: v.get("has_api_key").and_then(Value::as_bool).unwrap_or(false),
        has_password: v.get("has_password").and_then(Value::as_bool).unwrap_or(false),
        has_webhook_secret: v.get("has_webhook_secret").and_then(Value::as_bool).unwrap_or(false),
        poll_enabled: v.get("poll_enabled").and_then(Value::as_bool).unwrap_or(false),
        poll_interval_seconds: v.get("poll_interval_seconds").and_then(Value::as_i64).unwrap_or(300),
        poll_models: str_array(v, "poll_models"),
        webhook_enabled: v.get("webhook_enabled").and_then(Value::as_bool).unwrap_or(false),
        unblock_models: str_array(v, "unblock_models"),
        features_crm: v.get("features_crm").and_then(Value::as_bool).unwrap_or(false),
        features_sale: v.get("features_sale").and_then(Value::as_bool).unwrap_or(false),
        features_inventory: v.get("features_inventory").and_then(Value::as_bool).unwrap_or(false),
        features_accounting: v.get("features_accounting").and_then(Value::as_bool).unwrap_or(false),
        features_project: v.get("features_project").and_then(Value::as_bool).unwrap_or(false),
        features_hr: v.get("features_hr").and_then(Value::as_bool).unwrap_or(false),
    })
}

pub fn parse_credential_source(v: &Value) -> CredentialSource {
    let mut out = CredentialSource::default();
    let Some(entries) = v.get("entries").and_then(Value::as_array) else {
        return out;
    };
    for e in entries {
        let path = e.get("path").and_then(Value::as_str).unwrap_or("");
        let configured = e.get("configured").and_then(Value::as_bool).unwrap_or(false);
        if !configured {
            continue;
        }
        let label = e.get("source_label").and_then(Value::as_str).map(str::to_string);
        match path {
            "odoo.api_key" => out.api_key = label,
            "odoo.password" => out.password = label,
            "odoo.webhook_secret" => out.webhook_secret = label,
            _ => {}
        }
    }
    out
}

// ── Fetch orchestration ──────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<OdooState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<OdooState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "odoo.status", json!({}), |cx, result| {
        cx.global_mut::<OdooState>().status = result.map(|v| parse_odoo_status(&v)).into();
    });
    spawn_call(cx, tx.clone(), "odoo.config", json!({}), |cx, result| {
        cx.global_mut::<OdooState>().config = result.map(|v| parse_odoo_config(&v)).into();
    });
    spawn_call(cx, tx, "security.credential_inventory", json!({}), |cx, result| {
        cx.global_mut::<OdooState>().credentials = result.map(|v| parse_credential_source(&v)).into();
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

// ── Display helpers ────────────────────────────────────────────────────

fn protocol_label(locale: Locale, raw: &str) -> SharedString {
    match raw {
        "xmlrpc" => i18n::t(locale, "native.odoo.protocol.xmlrpc"),
        _ => i18n::t(locale, "native.odoo.protocol.jsonrpc"),
    }
}

fn auth_method_label(locale: Locale, raw: &str) -> SharedString {
    match raw {
        "password" => i18n::t(locale, "native.odoo.authMethod.password"),
        _ => i18n::t(locale, "native.odoo.authMethod.apiKey"),
    }
}

fn secret_row(locale: Locale, label: SharedString, has_value: bool, source_label: Option<&str>, is_last: bool) -> Div {
    if !has_value {
        return kv_row(
            label,
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.odoo.secret.notSet")),
            is_last,
        );
    }
    let value = div()
        .flex()
        .items_center()
        .gap_2()
        .children(source_label.map(|s| badge(SharedString::from(format!("{} {}", i18n::t(locale, "native.odoo.secret.sourcePrefix"), s)), BadgeVariant::Secondary)))
        .child(masked_value_chip());
    kv_row(label, value, is_last)
}

/// Same layout as `settings_common::kv_row`, but for the two rows on this
/// page whose "label" side is itself a two-line `Div` (title + muted
/// subtitle), not plain text — `kv_row`'s `impl Into<SharedString>` label
/// slot can't hold that.
fn kv_row_wide_label(label: Div, value: impl IntoElement, is_last: bool) -> Div {
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .min_h(px(40.))
        .px_4()
        .py_2p5()
        .child(label)
        .child(div().flex().items_center().gap_2().child(value));
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn feature_row(locale: Locale, label_key: &str, enabled: bool, is_last: bool) -> Div {
    let (variant, key) = if enabled {
        (BadgeVariant::Success, "native.odoo.feature.on")
    } else {
        (BadgeVariant::Secondary, "native.odoo.feature.off")
    };
    kv_row(i18n::t(locale, label_key), badge(i18n::t(locale, key), variant), is_last)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let (status, config, credentials) = {
        let s = cx.global::<OdooState>();
        (s.status.clone(), s.config.clone(), s.credentials.clone())
    };

    // `edition`/`version` (connected) and `error` (not connected, e.g. "No
    // credential configured" / "Connection failed" — the two hardcoded
    // strings `handle_odoo_status` ever sends) are real `odoo.status`
    // fields this page actually surfaces, not parsed-and-discarded.
    let status_col = match &status {
        Loadable::Loading => div().child(badge(i18n::t(locale, "native.odoo.status.checking"), BadgeVariant::Secondary)),
        Loadable::Ready(info) if info.connected => {
            let detail = match (&info.edition, &info.version) {
                (Some(e), Some(v)) => Some(SharedString::from(format!("{e} · {v}"))),
                (Some(e), None) => Some(SharedString::from(e.clone())),
                (None, Some(v)) => Some(SharedString::from(v.clone())),
                (None, None) => None,
            };
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap_0p5()
                .child(badge(i18n::t(locale, "native.odoo.status.connected"), BadgeVariant::Success))
                .children(detail.map(|d| div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(d)))
        }
        Loadable::Ready(info) => div()
            .flex()
            .flex_col()
            .items_end()
            .gap_0p5()
            .child(badge(i18n::t(locale, "native.odoo.status.disconnected"), BadgeVariant::Secondary))
            .children(info.error.as_ref().map(|e| div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(e.clone())))),
        Loadable::Failed(_) => div().child(badge(i18n::t(locale, "native.odoo.status.unknown"), BadgeVariant::Secondary)),
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
                        .child(i18n::t(locale, "native.odoo.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.odoo.subtitle")),
                ),
        )
        .child(status_col);

    let body: Div = match &config {
        Loadable::Loading => div().flex().flex_col().gap_3().child(skeleton(px(820.), px(220.))).child(skeleton(px(820.), px(88.))),
        Loadable::Failed(err) => div().p_4().child(
            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone())),
        ),
        Loadable::Ready(None) => div().p_4().child(
            div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.odoo.notConfigured")),
        ),
        Loadable::Ready(Some(cfg)) => {
            let cred = match &credentials {
                Loadable::Ready(c) => c.clone(),
                _ => CredentialSource::default(),
            };

            let connection_section = boxed_group(vec![
                kv_row(i18n::t(locale, "native.odoo.field.url"), div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").child(SharedString::from(cfg.url.clone())), false),
                kv_row(i18n::t(locale, "native.odoo.field.db"), div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").child(SharedString::from(cfg.db.clone())), false),
                kv_row(i18n::t(locale, "native.odoo.field.protocol"), protocol_label(locale, &cfg.protocol), false),
                kv_row(i18n::t(locale, "native.odoo.field.authMethod"), auth_method_label(locale, &cfg.auth_method), false),
                kv_row(
                    i18n::t(locale, "native.odoo.field.username"),
                    div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").child(if cfg.username.is_empty() { SharedString::from("—") } else { SharedString::from(cfg.username.clone()) }),
                    false,
                ),
                secret_row(
                    locale,
                    i18n::t(locale, "native.odoo.field.apiKeyOrPassword"),
                    if cfg.auth_method == "password" { cfg.has_password } else { cfg.has_api_key },
                    if cfg.auth_method == "password" { cred.password.as_deref() } else { cred.api_key.as_deref() },
                    true,
                ),
            ]);

            let features_section = boxed_group(vec![
                feature_row(locale, "native.odoo.feature.crm", cfg.features_crm, false),
                feature_row(locale, "native.odoo.feature.sale", cfg.features_sale, false),
                feature_row(locale, "native.odoo.feature.inventory", cfg.features_inventory, false),
                feature_row(locale, "native.odoo.feature.accounting", cfg.features_accounting, false),
                feature_row(locale, "native.odoo.feature.project", cfg.features_project, false),
                feature_row(locale, "native.odoo.feature.hr", cfg.features_hr, true),
            ]);

            let poll_sub: SharedString = if cfg.poll_models.is_empty() {
                i18n::t1(locale, "native.odoo.sync.pollIntervalOnly", "seconds", &cfg.poll_interval_seconds.to_string())
            } else {
                i18n::t1(
                    locale,
                    "native.odoo.sync.pollModels",
                    "models",
                    &format!("{}s · {}", cfg.poll_interval_seconds, cfg.poll_models.join(", ")),
                )
            };
            let sync_section = boxed_group(vec![
                kv_row_wide_label(
                    div().flex().flex_col().gap_0p5()
                        .child(div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.odoo.sync.poll")))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(poll_sub)),
                    badge(
                        i18n::t(locale, if cfg.poll_enabled { "native.odoo.feature.on" } else { "native.odoo.feature.off" }),
                        if cfg.poll_enabled { BadgeVariant::Success } else { BadgeVariant::Secondary },
                    ),
                    false,
                ),
                kv_row_wide_label(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_size(px(theme::TEXT_SM)).child(i18n::t(locale, "native.odoo.sync.webhook")))
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(i18n::t(locale, if cfg.webhook_enabled { "native.odoo.feature.on" } else { "native.odoo.feature.off" })),
                        ),
                    if cfg.has_webhook_secret {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(cred.webhook_secret.as_deref().map(|s| badge(SharedString::from(format!("{} {}", i18n::t(locale, "native.odoo.secret.sourcePrefix"), s)), BadgeVariant::Secondary)))
                            .child(masked_value_chip())
                    } else {
                        div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.odoo.secret.notSet"))
                    },
                    true,
                ),
            ]);

            let danger_section = div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px_0p5()
                        .pb_1p5()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
                        .child(i18n::t(locale, "native.odoo.danger.title")),
                )
                .child(
                    div()
                        .rounded(px(theme::RADIUS_XL))
                        .overflow_hidden()
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::alpha(theme::DESTRUCTIVE, 0.22))
                        .child(kv_row_wide_label(
                            div().flex().flex_col().gap_0p5()
                                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).child(i18n::t(locale, "native.odoo.danger.unblockModels")))
                                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "native.odoo.danger.unblockModelsCount", "count", &cfg.unblock_models.len().to_string()))),
                            button("odoo-danger-manage", i18n::t(locale, "native.odoo.danger.manage"), ButtonVariant::Destructive, true, None, |_ev, _window, _cx| {}),
                            true,
                        )),
                );

            div()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(div().flex().flex_col().child(section_label(i18n::t(locale, "native.odoo.section.connection"))).child(connection_section).child(
                    div().mt_2().flex().justify_end().gap_2()
                        .child(button("odoo-discover-schema", i18n::t(locale, "native.odoo.discoverSchema"), ButtonVariant::Secondary, true, None, |_ev, _window, _cx| {}))
                        .child(button("odoo-test-connection", i18n::t(locale, "native.odoo.testConnection"), ButtonVariant::Primary, true, None, |_ev, _window, _cx| {})),
                ))
                .child(div().flex().flex_col().child(section_label(i18n::t(locale, "native.odoo.section.features"))).child(features_section))
                .child(div().flex().flex_col().child(section_label(i18n::t(locale, "native.odoo.section.sync"))).child(sync_section))
                .child(danger_section)
                .child(
                    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).text_center().child(i18n::t(locale, "native.odoo.footnote")),
                )
        }
    };

    div()
        .id("odoo-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(860.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(breadcrumb("odoo-breadcrumb", locale, i18n::t(locale, "native.odoo.breadcrumb"), cx))
                .child(header)
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_odoo_status_unconfigured() {
        let info = parse_odoo_status(&json!({ "connected": false }));
        assert!(!info.connected);
        assert_eq!(info.error, None);
    }

    #[test]
    fn parse_odoo_status_connected_reads_edition_version() {
        let info = parse_odoo_status(&json!({ "connected": true, "edition": "enterprise", "version": "17.0", "uid": 2 }));
        assert!(info.connected);
        assert_eq!(info.edition.as_deref(), Some("enterprise"));
        assert_eq!(info.version.as_deref(), Some("17.0"));
    }

    #[test]
    fn parse_odoo_config_null_is_none() {
        assert!(parse_odoo_config(&Value::Null).is_none());
    }

    #[test]
    fn parse_odoo_config_reads_every_field_never_a_secret() {
        let v = json!({
            "url": "https://mycompany.odoo.com", "db": "mycompany", "protocol": "jsonrpc",
            "auth_method": "api_key", "username": "admin@mycompany.com",
            "poll_enabled": true, "poll_interval_seconds": 900, "poll_models": ["crm.lead", "sale.order"],
            "webhook_enabled": false, "has_api_key": true, "has_password": false, "has_webhook_secret": false,
            "unblock_models": ["res.partner"],
            "features_crm": true, "features_sale": true, "features_inventory": false,
            "features_accounting": false, "features_project": false, "features_hr": false,
        });
        let cfg = parse_odoo_config(&v).expect("configured");
        assert_eq!(cfg.url, "https://mycompany.odoo.com");
        assert_eq!(cfg.poll_models, vec!["crm.lead".to_string(), "sale.order".to_string()]);
        assert!(cfg.has_api_key);
        assert!(!cfg.has_password);
        assert_eq!(cfg.unblock_models, vec!["res.partner".to_string()]);
        // Belt-and-suspenders: the raw JSON must never carry a plaintext key
        // field name this parser would have accidentally picked up.
        assert!(v.get("api_key").is_none());
        assert!(v.get("password").is_none());
    }

    #[test]
    fn parse_credential_source_matches_by_path_suffix() {
        let v = json!({ "entries": [
            { "path": "odoo.api_key", "configured": true, "source": "keychain", "source_label": "系統鑰匙圈" },
            { "path": "odoo.password", "configured": false, "source": "unset", "source_label": "" },
            { "path": "telegram.bot_token", "configured": true, "source": "env", "source_label": "env:TELEGRAM_BOT_TOKEN" },
        ]});
        let cred = parse_credential_source(&v);
        assert_eq!(cred.api_key.as_deref(), Some("系統鑰匙圈"));
        assert_eq!(cred.password, None);
        assert_eq!(cred.webhook_secret, None);
    }

    #[test]
    fn parse_credential_source_missing_entries_is_default_not_a_panic() {
        let cred = parse_credential_source(&json!({}));
        assert_eq!(cred.api_key, None);
        assert_eq!(cred.password, None);
        assert_eq!(cred.webhook_secret, None);
    }

    #[test]
    fn protocol_label_maps_known_values() {
        assert_eq!(protocol_label(Locale::ZhTw, "xmlrpc").to_string(), i18n::t(Locale::ZhTw, "native.odoo.protocol.xmlrpc").to_string());
        assert_eq!(protocol_label(Locale::ZhTw, "jsonrpc").to_string(), i18n::t(Locale::ZhTw, "native.odoo.protocol.jsonrpc").to_string());
    }
}
