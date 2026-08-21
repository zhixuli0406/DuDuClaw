// WP-S5b1-C — Screen "Google 工作區" (`nav.rs` id `googleIntegration`, wired
// by the parallel A/B "整合" workstream — this pass adds the screen + its
// `screens/mod.rs` line only). An "整合" drill-down leaf: no own sidebar
// row, breadcrumb only.
//
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/
// GoogleIntegration.dc.html` — breadcrumb → header (title/subtitle +
// 已連接/未連線 badge) → "可解鎖的能力" grid → "OAuth 連接" boxed-list →
// "免 OAuth 用戶端的替代方式" two-card row. Functional reference:
// `web/src/pages/GoogleIntegrationPage.tsx` + its two child components
// `IntegrationConnectPanel`/`GoogleCredentialPaths` (58-line page composing
// two shared React components this drill-down re-implements natively).
//
// ── RPC shapes (verified against `crates/duduclaw-gateway/src/handlers.rs`
// + `crates/duduclaw-gateway/src/google_workspace.rs`, not guessed) ───────
//   `google.credentials.get` (dispatch ~L6470, handler ~L28230,
//     `require_admin!()`) → `{ integration_enabled, effective:
//     "direct"|"apps_script"|"none", service_account: { configured, key_
//     file, subject, error }, apps_script: { configured, url, error },
//     required_scopes: [...11 scope strings] }`. Never returns the Apps
//     Script bridge secret.
//   `mcp.oauth.providers` (dispatch ~L6449, handler ~L27978,
//     `require_admin!()`) filtered to `provider_id == "google"` — this is
//     what actually backs the canvas's "OAuth 連接" panel (Client ID/
//     Client Secret/已授權範圍/中斷連結): `google.credentials.get` covers
//     the service-account/Apps-Script paths, NOT the OAuth-client vault
//     entry, which lives in the same generic `mcp.oauth.*` surface every
//     other OAuth provider (Notion/GitHub/Slack) uses. `client_id` is
//     returned in full (public value); `client_secret_masked` is a 4-char
//     tail only.
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. "可解鎖的能力" (capabilities grid) — the canvas draws 8 illustrative
//    icon+label chips (搜尋郵件/讀取內容/…). No RPC enumerates "which
//    capabilities are active" (`google.credentials.get`'s `required_scopes`
//    is the closest live field, but it is 11 raw OAuth scope strings, not
//    curated capability copy). Rather than either fabricating new label
//    text or showing raw scope strings the canvas never intended, this page
//    reuses the SAME 8 curated capability strings the web dashboard already
//    ships and reviews (`web/src/i18n/zh-TW.json` `google.cap.*`, wired via
//    `GoogleIntegrationPage.tsx`'s `CAPABILITIES` constant) — real, shipped
//    product copy, not new invented text, translated into this crate's own
//    `googleInt.cap.*` keys.
// 2. No live "granted scopes" readback on the two alternative-path cards.
//    The canvas's OAuth panel scope chips ARE real (`mcp.oauth.providers`'s
//    `scopes` field for `provider_id == "google"`); the two "免 OAuth"
//    cards themselves carry no per-scope grant list in either RPC, so they
//    render as static informational cards (configured/not-configured badge
//    only) — same shape the canvas draws (it has no status badge on those
//    two cards either).
// 3. Connect/disconnect + both "設定教學 →" links: the connect/disconnect
//    button is rendered disabled (not wired) per this batch's "決策類...
//    不真按" convention (see `odoo.rs`'s header comment for the same call
//    across this page batch). The two "設定教學 →" links ARE real,
//    `cx.open_url`-backed — `docs/guides/google-no-oauth-client.md`, this
//    repo's own public doc for exactly these two paths (verified to exist
//    on disk), reached via the same GitHub blob URL pattern `about.rs::
//    DOCS_URL` already establishes — not a new, unverified link shape.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
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

/// Same GitHub blob URL shape `about.rs::DOCS_URL` establishes — this repo's
/// own public doc for the two no-OAuth-client credential paths (verified on
/// disk at `docs/guides/google-no-oauth-client.md`).
const NO_OAUTH_CLIENT_GUIDE_URL: &str =
    "https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/google-no-oauth-client.md";
const GOOGLE_CLOUD_CONSOLE_URL: &str = "https://console.cloud.google.com/apis/credentials";

/// The same 8 curated capability copy ids `web/src/pages/
/// GoogleIntegrationPage.tsx`'s `CAPABILITIES` constant ships — see header
/// comment §1 for why these are reused rather than invented or derived from
/// raw OAuth scope strings.
const CAPABILITY_KEYS: &[&str] = &[
    "native.googleInt.cap.search",
    "native.googleInt.cap.read",
    "native.googleInt.cap.draft",
    "native.googleInt.cap.calendar",
    "native.googleInt.cap.meet",
    "native.googleInt.cap.sheetsRead",
    "native.googleInt.cap.sheetsAppend",
    "native.googleInt.cap.status",
];

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct GoogleCredentialsInfo {
    pub integration_enabled: bool,
    /// Raw `"direct"|"apps_script"|"none"` from `google.credentials.get`.
    pub effective: String,
    pub sa_configured: bool,
    pub apps_script_configured: bool,
}

#[derive(Clone, Default)]
pub struct GoogleOAuthInfo {
    pub client_id: String,
    pub has_client_secret: bool,
    pub client_secret_masked: String,
    pub scopes: Vec<String>,
    /// Raw `"authenticated"|"expired"|"none"`.
    pub status: String,
}

pub struct GoogleIntegrationState {
    requested: bool,
    pub credentials: Loadable<GoogleCredentialsInfo>,
    pub oauth: Loadable<Option<GoogleOAuthInfo>>,
}

impl GoogleIntegrationState {
    fn new() -> Self {
        Self { requested: false, credentials: Loadable::Loading, oauth: Loadable::Loading }
    }
}

impl Global for GoogleIntegrationState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<GoogleIntegrationState>() {
        cx.set_global(GoogleIntegrationState::new());
    }
}

// ── Response parsing ──────────────────────────────────────────────────

pub fn parse_google_credentials(v: &Value) -> GoogleCredentialsInfo {
    GoogleCredentialsInfo {
        integration_enabled: v.get("integration_enabled").and_then(Value::as_bool).unwrap_or(false),
        effective: v.get("effective").and_then(Value::as_str).unwrap_or("none").to_string(),
        sa_configured: v.get("service_account").and_then(|sa| sa.get("configured")).and_then(Value::as_bool).unwrap_or(false),
        apps_script_configured: v.get("apps_script").and_then(|a| a.get("configured")).and_then(Value::as_bool).unwrap_or(false),
    }
}

/// Finds the `provider_id == "google"` entry inside `mcp.oauth.providers`'s
/// response — see header comment on why this (not `google.credentials.get`)
/// backs the OAuth-client panel.
pub fn parse_google_oauth_provider(v: &Value) -> Option<GoogleOAuthInfo> {
    let providers = v.get("providers").and_then(Value::as_array)?;
    let p = providers.iter().find(|p| p.get("provider_id").and_then(Value::as_str) == Some("google"))?;
    Some(GoogleOAuthInfo {
        client_id: p.get("client_id").and_then(Value::as_str).unwrap_or("").to_string(),
        has_client_secret: p.get("has_client_secret").and_then(Value::as_bool).unwrap_or(false),
        client_secret_masked: p.get("client_secret_masked").and_then(Value::as_str).unwrap_or("").to_string(),
        scopes: p
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        status: p.get("status").and_then(Value::as_str).unwrap_or("none").to_string(),
    })
}

// ── Fetch orchestration ──────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<GoogleIntegrationState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<GoogleIntegrationState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "google.credentials.get", json!({}), |cx, result| {
        cx.global_mut::<GoogleIntegrationState>().credentials = result.map(|v| parse_google_credentials(&v)).into();
    });
    spawn_call(cx, tx, "mcp.oauth.providers", json!({}), |cx, result| {
        cx.global_mut::<GoogleIntegrationState>().oauth = result.map(|v| parse_google_oauth_provider(&v)).into();
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

// ── Row / card builders ─────────────────────────────────────────────────

fn capability_chip(locale: Locale, key: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, key)))
}

fn alt_path_card(locale: Locale, title_key: &str, hint_key: &str, configured: bool) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).child(i18n::t(locale, title_key)))
                .child(badge(
                    i18n::t(locale, if configured { "native.googleInt.status.configured" } else { "native.googleInt.status.notConfigured" }),
                    if configured { BadgeVariant::Success } else { BadgeVariant::Secondary },
                )),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, hint_key)))
        .child(
            div()
                .id(SharedString::from(format!("googleint-guide-{title_key}")))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::PRIMARY, 1.0))
                .cursor_pointer()
                .hover(|s| s.underline())
                .on_click(|_ev, _window, cx| cx.open_url(NO_OAUTH_CLIENT_GUIDE_URL))
                .child(i18n::t(locale, "native.googleInt.setupGuide")),
        )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let (credentials, oauth) = {
        let s = cx.global::<GoogleIntegrationState>();
        (s.credentials.clone(), s.oauth.clone())
    };

    let status_badge = match &credentials {
        Loadable::Loading => badge(i18n::t(locale, "native.googleInt.status.checking"), BadgeVariant::Secondary),
        Loadable::Ready(info) if info.effective != "none" => badge(i18n::t(locale, "native.googleInt.status.connected"), BadgeVariant::Success),
        Loadable::Ready(_) => badge(i18n::t(locale, "native.googleInt.status.disconnected"), BadgeVariant::Secondary),
        Loadable::Failed(_) => badge(i18n::t(locale, "native.googleInt.status.unknown"), BadgeVariant::Secondary),
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
                        .child(i18n::t(locale, "native.googleInt.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.googleInt.subtitle")),
                ),
        )
        .child(status_badge);

    // `integration_enabled` (`config.toml [integrations] google_workspace`)
    // is the master gate — a credential can be fully configured AND this
    // still be off, in which case the tools never reach an AI employee at
    // all (mirrors `web/src/i18n/zh-TW.json`'s own `google.cred.gateOff`
    // copy). Shown only when it's actually off — never a false alarm on a
    // healthy, gated-on connection.
    let gate_off_note = matches!(&credentials, Loadable::Ready(info) if !info.integration_enabled).then(|| {
        div()
            .px_3p5()
            .py_2()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::WARNING, 0.08))
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::WARNING, 1.0))
            .child(i18n::t(locale, "native.googleInt.gateOff"))
    });

    let capabilities_section = div()
        .flex()
        .flex_col()
        .child(section_label(i18n::t(locale, "native.googleInt.capabilities")))
        .child(div().flex().flex_wrap().gap_2().children(CAPABILITY_KEYS.iter().map(|k| capability_chip(locale, k))));

    let oauth_section = {
        let body = match &oauth {
            Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(820.), px(140.))),
            Loadable::Failed(err) => div().p_4().child(
                div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone())),
            ),
            Loadable::Ready(None) => div().p_4().child(
                div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.googleInt.oauth.notConfigured")),
            ),
            Loadable::Ready(Some(info)) => {
                let mut rows = vec![
                    kv_row(
                        i18n::t(locale, "native.googleInt.oauth.clientId"),
                        div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(if info.client_id.is_empty() { SharedString::from("—") } else { SharedString::from(info.client_id.clone()) }),
                        false,
                    ),
                    kv_row(
                        i18n::t(locale, "native.googleInt.oauth.clientSecret"),
                        // `client_secret_masked` is a REAL 4-char tail
                        // `mcp.oauth.providers` returns (unlike Odoo's
                        // presence-only booleans) — shown verbatim when
                        // present rather than the generic placeholder chip,
                        // falling back to it only if the field is somehow
                        // empty despite `has_client_secret` being true.
                        if info.has_client_secret {
                            if info.client_secret_masked.is_empty() {
                                masked_value_chip()
                            } else {
                                div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(info.client_secret_masked.clone()))
                            }
                        } else {
                            div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.odoo.secret.notSet"))
                        },
                        false,
                    ),
                ];
                if !info.scopes.is_empty() {
                    rows.push(kv_row(
                        i18n::t(locale, "native.googleInt.oauth.grantedScopes"),
                        div().flex().flex_wrap().justify_end().gap_1().children(info.scopes.iter().map(|s| badge(SharedString::from(s.clone()), BadgeVariant::Secondary))),
                        false,
                    ));
                }
                // Not a plain label/value pair (the left side is itself a
                // clickable link, not text) — composed directly rather than
                // forced through `kv_row`'s `impl Into<SharedString>` label
                // slot.
                let manage_row = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(40.))
                    .px_4()
                    .py_2p5()
                    .child(
                        div()
                            .id("googleint-console-link")
                            .text_size(px(theme::TEXT_XS))
                            .text_color(theme::alpha(theme::PRIMARY, 1.0))
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .on_click(|_ev, _window, cx| cx.open_url(GOOGLE_CLOUD_CONSOLE_URL))
                            .child(i18n::t(locale, "native.googleInt.oauth.manageInConsole")),
                    )
                    .child(button(
                        "googleint-disconnect",
                        i18n::t(locale, if info.status == "authenticated" { "native.googleInt.oauth.disconnect" } else { "native.googleInt.oauth.connect" }),
                        ButtonVariant::Secondary,
                        true, // not wired this pass — see header comment §3
                        None,
                        |_ev, _window, _cx| {},
                    ));
                rows.push(manage_row);
                boxed_group(rows)
            }
        };
        div().flex().flex_col().child(section_label(i18n::t(locale, "native.googleInt.oauth.title"))).child(body)
    };

    let alt_paths_section = {
        let (sa_configured, gas_configured) = match &credentials {
            Loadable::Ready(info) => (info.sa_configured, info.apps_script_configured),
            _ => (false, false),
        };
        div()
            .flex()
            .flex_col()
            .child(section_label(i18n::t(locale, "native.googleInt.altPaths")))
            .child(
                div()
                    .flex()
                    .gap_2p5()
                    .child(alt_path_card(locale, "native.googleInt.alt.serviceAccount.title", "native.googleInt.alt.serviceAccount.hint", sa_configured))
                    .child(alt_path_card(locale, "native.googleInt.alt.appsScript.title", "native.googleInt.alt.appsScript.hint", gas_configured)),
            )
    };

    div()
        .id("google-integration-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(860.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(breadcrumb("googleint-breadcrumb", locale, i18n::t(locale, "native.googleInt.breadcrumb"), cx))
                .child(header)
                .children(gate_off_note)
                .child(capabilities_section)
                .child(oauth_section)
                .child(alt_paths_section),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_google_credentials_reads_effective_and_configured_flags() {
        let v = json!({
            "integration_enabled": true, "effective": "direct",
            "service_account": { "configured": true, "key_file": "sa.json", "subject": "", "error": "" },
            "apps_script": { "configured": false, "url": "", "error": "" },
            "required_scopes": ["gmail.readonly"],
        });
        let info = parse_google_credentials(&v);
        assert!(info.integration_enabled);
        assert_eq!(info.effective, "direct");
        assert!(info.sa_configured);
        assert!(!info.apps_script_configured);
    }

    #[test]
    fn parse_google_credentials_missing_fields_default_to_none() {
        let info = parse_google_credentials(&json!({}));
        assert_eq!(info.effective, "none");
        assert!(!info.sa_configured);
        assert!(!info.apps_script_configured);
    }

    #[test]
    fn parse_google_oauth_provider_finds_the_google_entry_only() {
        let v = json!({ "providers": [
            { "provider_id": "notion", "client_id": "x" },
            { "provider_id": "google", "client_id": "8291.apps.googleusercontent.com",
              "has_client_secret": true, "client_secret_masked": "••••9f3c",
              "scopes": ["gmail.readonly", "calendar"], "status": "authenticated" },
        ]});
        let info = parse_google_oauth_provider(&v).expect("google entry present");
        assert_eq!(info.client_id, "8291.apps.googleusercontent.com");
        assert!(info.has_client_secret);
        assert_eq!(info.scopes, vec!["gmail.readonly".to_string(), "calendar".to_string()]);
        assert_eq!(info.status, "authenticated");
    }

    #[test]
    fn parse_google_oauth_provider_absent_is_none_not_a_panic() {
        let v = json!({ "providers": [{ "provider_id": "notion", "client_id": "x" }] });
        assert!(parse_google_oauth_provider(&v).is_none());
    }

    #[test]
    fn parse_google_oauth_provider_missing_array_is_none() {
        assert!(parse_google_oauth_provider(&json!({})).is_none());
    }
}
