// WP-S6b1-J (2026-08-21) — "授權" (B16+B18), a "進階設定" drill-down leaf: no
// own sidebar row, breadcrumb only (`進階設定 › 授權`). Also the
// "LicenseShell" tab-strip host (`license`/`partnerPortal` tabs,
// `manage_advanced_common::license_shell_tabs`) — see `screens::
// partner_portal`, this batch's other tab. Reachable via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=license` today (`manage_advanced.rs`'s own
// 授權 row wiring is a sibling package's scope, same as `screens::billing`'s
// own header comment already states).
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/LicensePage.dc.
// html` (B16+B18) — breadcrumb → LicenseShell tabs → header (+重新整理) → 3
// KPI tiles → 授權詳情 boxed-list → 模組能力 table → 續約提醒 banner →
// "替代情境·尚未啟用" step-wizard preview card (ALWAYS rendered alongside the
// enabled main view, per the canvas's own `Main.dc.html` cover sheet §2:
// "呈現啟用流程的另一狀態，不取代已啟用主視圖" — not gated on the real
// `installed` flag). Functional reference: `web/src/pages/LicensePage.tsx`
// (its `ActivateLicenseCard` write-form is NOT ported — this drill-down is
// read-only display, matching this task's own "決策類組裝不真按" brief for
// the CLI-driven activate/redeem flow that card exposes).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, not guessed; NO `require_*!()` gate — `license.status` is one of the
// explicit "personal_surfaces_are_untouched" test's own entries, i.e. the
// upgrade path itself must never be permission-gated) ────────────────────
//   `license.status {}` (`handle_license_status` ~L7133) → the serialized
//   `LicenseSnapshot` struct (`duduclaw-gateway/src/license_runtime.rs`
//   ~L486): `{"tier","mode","installed","customer_id","subscription_id",
//   "expires_at","days_until_expiry","last_phone_home",
//   "days_since_phone_home","fingerprint_match","branding_editable",
//   "max_agents","nfr"}`. `tier` is `LicenseTier`'s OWN `#[serde(rename_all
//   = "snake_case")]` derive output, verified with a throwaway
//   `serde_json::to_string` probe against every variant (not assumed):
//   `open_source / hobby / solo / studio / business / partner /
//   personal_pro_self_host / self_host_pro / oem`.
//
// ── A real bug this pass found, NOT reproduced here ───────────────────────
// `web/src/lib/license-labels.ts`'s `TIER_LABELS` map keys `LicenseTier::
// OpenSource` as `'opensource'` (no underscore) — that key can never match
// the wire value above (`"open_source"`), so the web dashboard's own tier
// label silently falls through to a raw string for every open-source
// install. `TIER_LABELS` below uses the VERIFIED `"open_source"` key
// instead — same label content, correct lookup. (Out of this task's scope
// to fix the web bug itself; flagged here so the native port doesn't
// silently copy it.)
//
// ── 模組能力 table: ported from `web/src/pages/LicensePage.tsx`'s
// `COMMERCIAL_FEATURES` client-side tier-membership table (NOT a backend
// field — the tier→capability mapping is a static UI fact, computed the
// same way here), not the canvas's 5 illustrative rows (多使用者與部門／白牌
// 經銷／治理與RBAC／值班機OS授權／進階可靠性監控) — those 5 labels have no
// backing constant anywhere in the codebase (grepped `duduclaw-license`,
// `duduclaw-gateway`: no match), so rendering them would be fabricated data.
// The 7 real feature keys/tier sets are reproduced verbatim from the web
// source of truth instead — an honest, flagged deviation from the canvas's
// literal row set, not a silent substitution.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::{breadcrumb, license_shell_tabs};
use crate::screens::settings_common::{boxed_group, kv_row};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const CONTENT_MAX_WIDTH: f32 = 800.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct LicenseSnapshot {
    pub tier: String,
    pub installed: bool,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub expires_at: Option<String>,
    pub days_until_expiry: Option<i64>,
    pub last_phone_home: Option<String>,
    pub days_since_phone_home: Option<i64>,
    pub fingerprint_match: Option<bool>,
}

// ── Tier label table (verified wire values — see this file's header
// comment's "real bug this pass found" note) ─────────────────────────────

const TIER_LABELS: &[(&str, &str)] = &[
    ("open_source", "Open Source"),
    ("hobby", "Hobby (Trial)"),
    ("solo", "Solo"),
    ("studio", "Studio"),
    ("business", "Business"),
    ("partner", "Partner (NFR)"),
    ("personal_pro_self_host", "Personal Pro"),
    ("self_host_pro", "Self-Host Pro"),
    ("oem", "OEM"),
];

fn tier_label(tier: &str) -> String {
    TIER_LABELS.iter().find(|(k, _)| *k == tier).map(|(_, v)| v.to_string()).unwrap_or_else(|| tier.to_string())
}

// ── Commercial-module feature matrix (ported from `web/src/pages/
// LicensePage.tsx`'s `COMMERCIAL_FEATURES` — see this file's header
// comment) ─────────────────────────────────────────────────────────────

struct CommercialFeature {
    label_key: &'static str,
    tiers: &'static [&'static str],
}

const COMMERCIAL_FEATURES: &[CommercialFeature] = &[
    CommercialFeature {
        label_key: "license.feature.premiumTemplates",
        tiers: &["studio", "business", "partner", "personal_pro_self_host", "self_host_pro", "oem"],
    },
    CommercialFeature {
        label_key: "license.feature.evolutionParams",
        tiers: &["business", "partner", "self_host_pro", "oem"],
    },
    CommercialFeature {
        label_key: "license.feature.dashboardEnterprise",
        tiers: &["business", "partner", "self_host_pro", "oem"],
    },
    CommercialFeature {
        label_key: "license.feature.prioritySecurityPatch",
        tiers: &["business", "partner", "personal_pro_self_host", "self_host_pro", "oem"],
    },
    CommercialFeature {
        label_key: "license.feature.privateDiscord",
        tiers: &["business", "partner", "personal_pro_self_host", "self_host_pro", "oem"],
    },
    CommercialFeature { label_key: "license.feature.odoo", tiers: &["business", "partner"] },
    CommercialFeature { label_key: "license.feature.whiteLabel", tiers: &["oem"] },
];

// ── State ──────────────────────────────────────────────────────────────

pub struct LicenseState {
    requested: bool,
    pub snapshot: Loadable<LicenseSnapshot>,
}

impl Default for LicenseState {
    fn default() -> Self {
        Self { requested: false, snapshot: Loadable::Loading }
    }
}

impl Global for LicenseState {}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_license_status(v: &Value) -> LicenseSnapshot {
    LicenseSnapshot {
        tier: v.get("tier").and_then(Value::as_str).unwrap_or("open_source").to_string(),
        installed: v.get("installed").and_then(Value::as_bool).unwrap_or(false),
        customer_id: v.get("customer_id").and_then(Value::as_str).map(str::to_string),
        subscription_id: v.get("subscription_id").and_then(Value::as_str).map(str::to_string),
        expires_at: v.get("expires_at").and_then(Value::as_str).map(str::to_string),
        days_until_expiry: v.get("days_until_expiry").and_then(Value::as_i64),
        last_phone_home: v.get("last_phone_home").and_then(Value::as_str).map(str::to_string),
        days_since_phone_home: v.get("days_since_phone_home").and_then(Value::as_i64),
        fingerprint_match: v.get("fingerprint_match").and_then(Value::as_bool),
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "license" || cx.default_global::<LicenseState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<LicenseState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "license.status", json!({}), |cx, result| {
        cx.default_global::<LicenseState>().snapshot = result.map(|v| parse_license_status(&v)).into();
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

// ── Display helpers ────────────────────────────────────────────────────

/// RFC3339 → "2026-08-15 09:30"; "—" when unparseable/empty/absent.
fn format_datetime(ts: Option<&str>) -> String {
    match ts {
        Some(s) if !s.is_empty() => chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|_| "—".to_string()),
        _ => "—".to_string(),
    }
}

// ── Section: KPI tiles ─────────────────────────────────────────────────

fn stat_tile(label: SharedString, value: Div, sub: Option<SharedString>) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(13.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(value)
        .children(sub.map(|s| div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(s)))
}

fn kpi_row(locale: Locale, snap: &LicenseSnapshot) -> Div {
    let tier_tile = stat_tile(
        i18n::t(locale, "license.kpi.tier"),
        div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(tier_label(&snap.tier)),
        Some(i18n::t(locale, if snap.installed { "license.mode.commercial" } else { "license.mode.opensource" })),
    );

    let (expiry_text, expiry_color) = match snap.days_until_expiry {
        None => (i18n::t(locale, "license.expiry.unknown"), theme::MUTED_FOREGROUND),
        Some(d) if d < 0 => (i18n::t1(locale, "license.expiry.expired", "days", &(-d).to_string()), theme::DESTRUCTIVE),
        Some(d) if d <= 7 => (i18n::t1(locale, "license.expiry.critical", "days", &d.to_string()), theme::DESTRUCTIVE),
        Some(d) if d <= 30 => (i18n::t1(locale, "license.expiry.warning", "days", &d.to_string()), theme::WARNING),
        Some(d) => (i18n::t1(locale, "license.expiry.ok", "days", &d.to_string()), theme::SUCCESS),
    };
    let expiry_tile = stat_tile(
        i18n::t(locale, "license.kpi.expiresAt"),
        div().text_size(px(15.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(expiry_color, 1.0)).child(expiry_text),
        Some(SharedString::from(format_datetime(snap.expires_at.as_deref()))),
    );

    let phone_home_tile = stat_tile(
        i18n::t(locale, "license.kpi.lastPhoneHome"),
        match snap.days_since_phone_home {
            Some(d) => badge(i18n::t1(locale, "license.phoneHome.daysAgo", "days", &d.to_string()), if d <= 7 { BadgeVariant::Success } else if d <= 30 { BadgeVariant::Warning } else { BadgeVariant::Destructive }),
            None => div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "license.phoneHome.notApplicable")),
        },
        Some(SharedString::from(format_datetime(snap.last_phone_home.as_deref()))),
    );

    div().flex().gap_2p5().child(tier_tile).child(expiry_tile).child(phone_home_tile)
}

// ── Section: 授權詳情 ──────────────────────────────────────────────────

fn detail_section(locale: Locale, snap: &LicenseSnapshot) -> Div {
    let opt_mono = |v: &Option<String>| -> Div {
        div()
            .font_family("SF Mono")
            .text_size(px(12.))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(SharedString::from(v.clone().unwrap_or_else(|| "—".to_string())))
    };
    let fingerprint_row = match snap.fingerprint_match {
        Some(true) => badge(i18n::t(locale, "license.fingerprintMatch.yes"), BadgeVariant::Success),
        Some(false) => badge(i18n::t(locale, "license.fingerprintMatch.no"), BadgeVariant::Destructive),
        None => div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("—"),
    };

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(i18n::t(locale, "license.section.details")))
        .child(boxed_group(vec![
            kv_row(i18n::t(locale, "license.customerId"), opt_mono(&snap.customer_id), false),
            kv_row(i18n::t(locale, "license.subscriptionId"), opt_mono(&snap.subscription_id), false),
            kv_row(i18n::t(locale, "license.fingerprintMatch"), fingerprint_row, true),
        ]))
}

fn section_label(text: SharedString) -> Div {
    div()
        .px_0p5()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text)
}

// ── Section: 模組能力 ──────────────────────────────────────────────────

fn modules_section(locale: Locale, snap: &LicenseSnapshot) -> Div {
    let header = div()
        .grid()
        .grid_cols(2)
        .gap_2()
        .px_4()
        .py_2()
        .bg(theme::alpha(theme::MUTED, 0.35))
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, "license.modules.col.module"))
        .child(i18n::t(locale, "license.modules.col.status"));

    let mut rows = vec![header];
    for f in COMMERCIAL_FEATURES {
        let unlocked = f.tiers.contains(&snap.tier.as_str());
        let status = if unlocked {
            badge(i18n::t(locale, "license.modules.unlocked"), BadgeVariant::Success)
        } else {
            badge(i18n::t(locale, "license.modules.locked"), BadgeVariant::Secondary)
        };
        let row = div()
            .grid()
            .grid_cols(2)
            .gap_2()
            .px_4()
            .py_2p5()
            .items_center()
            .text_size(px(12.5))
            .text_color(theme::alpha(if unlocked { theme::FOREGROUND } else { theme::MUTED_FOREGROUND }, 1.0))
            .border_t_1()
            .border_color(theme::border())
            .child(i18n::t(locale, f.label_key))
            .child(status);
        rows.push(row);
    }

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(i18n::t(locale, "license.section.modules")))
        .child(
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
                .children(rows),
        )
}

// ── Section: 續約提醒 banner ───────────────────────────────────────────

fn renewal_banner(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .py_2p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::WARNING, 0.10))
        .border_1()
        .border_color(theme::alpha(theme::WARNING, 0.3))
        .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(theme::alpha(theme::WARNING, 1.0)).child(i18n::t(locale, "license.renewal.note")))
        .child(
            div()
                .flex_shrink_0()
                .px_3p5()
                .h(px(30.))
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::BRAND, 1.0))
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(i18n::t(locale, "license.renewal.action")),
        )
}

// ── Section: 替代情境 · 尚未啟用（step wizard preview, static） ──────────

fn activation_wizard_preview(locale: Locale) -> Div {
    let step = |n: u32, label: SharedString, active: bool| -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
            .text_color(theme::alpha(if active { theme::BRAND } else { theme::MUTED_FOREGROUND }, 1.0))
            .child(
                div()
                    .size(px(16.))
                    .flex_shrink_0()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(9.5))
                    .when(active, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
                    .when(!active, |el| el.border_1().border_color(theme::alpha(theme::MUTED_FOREGROUND, 0.4)))
                    .child(n.to_string()),
            )
            .child(label)
    };

    div()
        .border_1()
        .border_color(theme::alpha(theme::MUTED_FOREGROUND, 0.3))
        .rounded(px(theme::RADIUS_XL))
        .p(px(13.))
        .flex()
        .flex_col()
        .gap_2p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(badge(i18n::t(locale, "license.wizard.badge"), BadgeVariant::Secondary))
                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "license.wizard.caption"))),
        )
        .child(
            div()
                .flex()
                .gap(px(22.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2p5()
                        .flex_shrink_0()
                        .w(px(150.))
                        .child(step(1, i18n::t(locale, "license.wizard.step1"), true))
                        .child(step(2, i18n::t(locale, "license.wizard.step2"), false))
                        .child(step(3, i18n::t(locale, "license.wizard.step3"), false)),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .justify_center()
                        .child(
                            div()
                                .border_1()
                                .border_color(theme::border())
                                .rounded(px(theme::RADIUS_LG))
                                .px_3()
                                .py_2()
                                .font_family("SF Mono")
                                .text_size(px(12.))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child("DUDU-XXXX-XXXX-XXXX"),
                        )
                        .child(
                            div()
                                .self_start()
                                .px_4()
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .rounded(px(theme::RADIUS_LG))
                                .bg(theme::alpha(theme::BRAND, 1.0))
                                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(i18n::t(locale, "license.wizard.next")),
                        ),
                ),
        )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let snapshot = cx.default_global::<LicenseState>().snapshot.clone();

    let crumb = breadcrumb("license-breadcrumb", locale, i18n::t(locale, "license.title"), cx);
    let tabs = license_shell_tabs(locale, "license", cx);

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "license.title")))
                .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "license.subtitle"))),
        )
        .child(
            div()
                .id("license-refresh")
                .cursor_pointer()
                .px_3()
                .h(px(30.))
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_size(px(12.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.5)))
                .child(i18n::t(locale, "license.refresh"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.default_global::<LicenseState>().requested = false;
                    cx.notify();
                })),
        );

    let body: Div = match &snapshot {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_3p5();
            for _ in 0..3 {
                wrap = wrap.child(skeleton(px(760.), px(60.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div().child(empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>)),
        Loadable::Ready(snap) => div()
            .flex()
            .flex_col()
            .gap_3p5()
            .child(kpi_row(locale, snap))
            .child(detail_section(locale, snap))
            .child(modules_section(locale, snap))
            .child(renewal_banner(locale))
            .child(activation_wizard_preview(locale)),
    };

    div()
        .id("license-page")
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
                .gap_3p5()
                .child(crumb)
                .child(tabs)
                .child(header)
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_license_status_reads_the_real_payload_shape() {
        let v = json!({
            "tier": "business", "mode": "commercial", "installed": true,
            "customer_id": "CUS-8842-TW", "subscription_id": "SUB-2026-0447",
            "expires_at": "2027-03-15T00:00:00+00:00", "days_until_expiry": 206,
            "last_phone_home": "2026-08-21T06:00:00+00:00", "days_since_phone_home": 0,
            "fingerprint_match": true,
        });
        let s = parse_license_status(&v);
        assert_eq!(s.tier, "business");
        assert!(s.installed);
        assert_eq!(s.customer_id.as_deref(), Some("CUS-8842-TW"));
        assert_eq!(s.days_until_expiry, Some(206));
        assert_eq!(s.fingerprint_match, Some(true));
    }

    #[test]
    fn parse_license_status_missing_fields_default_to_open_source() {
        let s = parse_license_status(&json!({}));
        assert_eq!(s.tier, "open_source");
        assert!(!s.installed);
        assert!(s.customer_id.is_none());
        assert!(s.fingerprint_match.is_none());
    }

    #[test]
    fn tier_label_uses_the_verified_open_source_key_not_the_web_bug_key() {
        assert_eq!(tier_label("open_source"), "Open Source");
        assert_eq!(tier_label("opensource"), "opensource"); // unknown key: falls through verbatim
    }

    #[test]
    fn tier_label_covers_every_known_wire_value() {
        for (k, _) in TIER_LABELS {
            assert_ne!(tier_label(k), *k, "tier {k} should resolve to a real label, not fall through");
        }
    }

    #[test]
    fn commercial_features_business_tier_matches_web_source_of_truth() {
        let unlocked: Vec<&str> = COMMERCIAL_FEATURES.iter().filter(|f| f.tiers.contains(&"business")).map(|f| f.label_key).collect();
        assert!(unlocked.contains(&"license.feature.premiumTemplates"));
        assert!(unlocked.contains(&"license.feature.odoo"));
        assert!(!unlocked.contains(&"license.feature.whiteLabel"));
    }

    #[test]
    fn commercial_features_oem_unlocks_white_label_only_oem() {
        let white_label = COMMERCIAL_FEATURES.iter().find(|f| f.label_key == "license.feature.whiteLabel").unwrap();
        assert_eq!(white_label.tiers, &["oem"]);
    }

    #[test]
    fn format_datetime_is_minute_resolution_or_dash() {
        assert_eq!(format_datetime(Some("2026-08-15T09:30:00Z")), "2026-08-15 09:30");
        assert_eq!(format_datetime(None), "—");
        assert_eq!(format_datetime(Some("")), "—");
        assert_eq!(format_datetime(Some("garbage")), "—");
    }

    #[test]
    fn describe_call_error_prefers_structured_message_over_bare_string() {
        let msg = describe_call_error(&CallError::Rejected(json!({"code": "denied", "message": "權限不足"})));
        assert_eq!(msg, "權限不足");
    }
}
