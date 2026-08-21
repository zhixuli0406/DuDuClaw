// WP-S6b1-J (2026-08-21) — "經銷夥伴入口" (B19), the LicenseShell's second
// tab (`manage_advanced_common::license_shell_tabs`, active id
// `"partnerPortal"`) — embedded within the 授權 breadcrumb, per the canvas's
// own `Main.dc.html` cover sheet: "PartnerPortalPage／DistributorsPage 語彙
// 皆含「經銷」但服務對象相反（夥伴自助視角 vs. 總部核發視角）". No own sidebar
// row. Reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=partnerPortal` today, or
// via the tab strip from `screens::license` (same batch, real navigation).
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/
// PartnerPortalPage.dc.html` (B19) — breadcrumb → LicenseShell tabs → header
// (+已認證 badge) → 夥伴編號/加入時間 boxed-list → 4 KPI tiles → 客戶清單
// table (+新增客戶) → 產生授權 card → 銷售物料 card. Functional reference:
// `web/src/pages/PartnerPortalPage.tsx` (its full CRUD dialogs — add/edit/
// delete customer, onboarding form — are NOT ported; this drill-down is
// read-only display, matching this task's "決策類組裝不真按" brief).
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs` + `partner_store.rs`, not guessed; gated Enterprise-only via
// `is_enterprise_only_method`, NOT a `require_*!()` dispatch-match gate —
// see `handlers.rs`'s own `enterprise_gates_every_multi_user_surface` test
// listing `partner.profile`/`partner.stats`/`partner.customers`; a
// Community-edition caller sees this surface fail with an edition-denial
// error, which this page renders through the same generic `Loadable::
// Failed` path as any other RPC rejection) ────────────────────────────────
//   `partner.profile {}` (`handle_partner_profile` ~L17409) → the serialized
//     `PartnerProfile` (`partner_store.rs` ~L23): `{"company","tier",
//     "partner_id","certified_at","created_at","updated_at"}`.
//   `partner.stats {}` (`handle_partner_stats` ~L17418) → the serialized
//     `PartnerStats` (~L104): `{"total_sold","active_customers",
//     "this_month_commission_cents","lifetime_commission_cents"}`.
//   `partner.customers {status?,limit?}` (`handle_partner_customers`
//     ~L17427) → `{"customers":[{"id","name","tier","activated_at","status",
//     "commission_cents","notes","created_at"}]}` (`PartnerCustomer`, ~L59).
//     Called with no params (server default `limit: 100`).
//
// ── Honest deviations from the design canvas (documented, not silent) ────
// 1. KPI row: the canvas's 4 tiles are 總銷售數／使用中客戶／月營收／本月分潤
//    — two DIFFERENT commission-shaped numbers. The real store only computes
//    ONE "this month" figure (`this_month_commission_cents`) plus one
//    lifetime figure (`lifetime_commission_cents`); there is no separate
//    gross-revenue number anywhere in `partner_store.rs`'s `compute_stats`.
//    Renamed to 本月分潤／累計分潤 (matching `this_month_commission_cents`/
//    `lifetime_commission_cents` respectively) rather than inventing a
//    "月營收" figure the backend cannot produce — same naming
//    `web/src/pages/PartnerPortalPage.tsx`'s own `StatTile` labels already
//    settle on (`partner.monthlyRevenue`/`partner.commission`, i.e. it made
//    the identical call).
// 2. 客戶清單 table drops the canvas's "狀態" column — the canvas's own 4
//    header cells (客戶名稱／授權方案／啟用時間／操作) never include one, so
//    this is fidelity TO the canvas, a deviation from the (5-column) web
//    reference instead.
// 3. 銷售物料 card drops the canvas's fabricated file sizes ("PDF, 4.2 MB" /
//    "DOCX, 1.8 MB") — `web/src/pages/PartnerPortalPage.tsx`'s own
//    `MaterialCard` doc comment already states no real asset ships in the
//    repo (`marketing/slide-decks/` checked empty) and renders an honest
//    "即將推出" affordance instead of a specific size; this page follows
//    that verified-real behavior rather than the canvas's illustrative
//    numbers.
// 4. "產生授權" card's CLI command line is reproduced verbatim from
//    `web/src/pages/PartnerPortalPage.tsx` (`duduclaw license generate
//    --tier <pro|enterprise> --customer <name> --months <n>`) — a real,
//    grep-verified CLI invocation shape, not canvas-authored copy.

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

// `company`/`tier` (real fields on the RPC's `PartnerProfile`, per this
// file's own header comment) are deliberately NOT captured here — the
// canvas's compact 夥伴狀態 boxed-list only ever shows 夥伴編號/加入時間,
// never a company name or profile tier, so parsing fields this page never
// renders would just be dead weight (and dead-code warnings) rather than
// honest completeness.
#[derive(Clone, Default)]
pub struct PartnerProfile {
    pub partner_id: Option<String>,
    pub certified_at: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Copy, Default)]
pub struct PartnerStats {
    pub total_sold: i64,
    pub active_customers: i64,
    pub this_month_commission_cents: i64,
    pub lifetime_commission_cents: i64,
}

// `id` (real field, used by the store to key updates/deletes — out of this
// read-only drill-down's scope) is deliberately NOT captured for the same
// "don't parse what nothing renders" reason `PartnerProfile` above states;
// its presence is still used as the row-validity check in `parse_customers`.
#[derive(Clone)]
pub struct PartnerCustomer {
    pub name: String,
    pub tier: String,
    pub activated_at: String,
}

// ── State ──────────────────────────────────────────────────────────────

pub struct PartnerPortalState {
    requested: bool,
    pub profile: Loadable<PartnerProfile>,
    pub stats: Loadable<PartnerStats>,
    pub customers: Loadable<Vec<PartnerCustomer>>,
}

impl Default for PartnerPortalState {
    fn default() -> Self {
        Self { requested: false, profile: Loadable::Loading, stats: Loadable::Loading, customers: Loadable::Loading }
    }
}

impl Global for PartnerPortalState {}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_profile(v: &Value) -> PartnerProfile {
    PartnerProfile {
        partner_id: v.get("partner_id").and_then(Value::as_str).map(str::to_string),
        certified_at: v.get("certified_at").and_then(Value::as_str).map(str::to_string),
        created_at: v.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

fn parse_stats(v: &Value) -> PartnerStats {
    PartnerStats {
        total_sold: v.get("total_sold").and_then(Value::as_i64).unwrap_or(0),
        active_customers: v.get("active_customers").and_then(Value::as_i64).unwrap_or(0),
        this_month_commission_cents: v.get("this_month_commission_cents").and_then(Value::as_i64).unwrap_or(0),
        lifetime_commission_cents: v.get("lifetime_commission_cents").and_then(Value::as_i64).unwrap_or(0),
    }
}

fn parse_customers(v: &Value) -> Vec<PartnerCustomer> {
    v.get("customers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let _id = c.get("id")?.as_str()?; // presence check only — see struct doc comment
            Some(PartnerCustomer {
                name: c.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                tier: c.get("tier").and_then(Value::as_str).unwrap_or("standard").to_string(),
                activated_at: c.get("activated_at").and_then(Value::as_str).unwrap_or("").to_string(),
            })
        })
        .collect()
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "partnerPortal" || cx.default_global::<PartnerPortalState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<PartnerPortalState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "partner.profile", json!({}), |cx, result| {
        cx.default_global::<PartnerPortalState>().profile = result.map(|v| parse_profile(&v)).into();
    });
    spawn_call(cx, tx.clone(), "partner.stats", json!({}), |cx, result| {
        cx.default_global::<PartnerPortalState>().stats = result.map(|v| parse_stats(&v)).into();
    });
    spawn_call(cx, tx, "partner.customers", json!({}), |cx, result| {
        cx.default_global::<PartnerPortalState>().customers = result.map(|v| parse_customers(&v)).into();
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

fn format_dollars(cents: i64) -> String {
    let dollars = cents / 100;
    let sign = if dollars < 0 { "-" } else { "" };
    let digits = dollars.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{}", grouped.chars().rev().collect::<String>())
}

fn format_date(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.format("%Y-%m-%d").to_string()).unwrap_or_else(|_| "—".to_string())
}

fn customer_tier_label(locale: Locale, tier: &str) -> SharedString {
    let key = match tier {
        "standard" => "partnerPortal.customerTier.standard",
        "pro" => "partnerPortal.customerTier.pro",
        "enterprise" => "partnerPortal.customerTier.enterprise",
        _ => return SharedString::from(tier.to_string()),
    };
    i18n::t(locale, key)
}

// ── Section: header + status badge ────────────────────────────────────

fn section_label(text: SharedString) -> Div {
    div()
        .px_0p5()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text)
}

fn stat_tile(label: SharedString, value: String, tone: u32) -> Div {
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
        .child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(tone, 1.0)).child(value))
}

// ── Section: 客戶清單 ──────────────────────────────────────────────────

fn customer_row(locale: Locale, c: &PartnerCustomer, is_last: bool) -> Div {
    let tier_badge = badge(
        customer_tier_label(locale, &c.tier),
        if c.tier == "enterprise" { BadgeVariant::Info } else if c.tier == "pro" { BadgeVariant::Warning } else { BadgeVariant::Secondary },
    );
    let row = div()
        .grid()
        .grid_cols(4)
        .gap_2()
        .px_4()
        .py_2p5()
        .items_center()
        .text_size(px(12.5))
        .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).font_weight(gpui::FontWeight::MEDIUM).child(SharedString::from(c.name.clone())))
        .child(tier_badge)
        .child(div().text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format_date(&c.activated_at)))
        .child(div().text_color(theme::alpha(theme::BRAND, 1.0)).font_weight(gpui::FontWeight::MEDIUM).child(i18n::t(locale, "partnerPortal.customer.edit")));
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn customers_section(locale: Locale, state: &Loadable<Vec<PartnerCustomer>>) -> Div {
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(section_label(i18n::t(locale, "partnerPortal.section.customers")))
        .child(
            div()
                .id("partnerportal-add-customer")
                .px_3()
                .h(px(28.))
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::BRAND, 1.0))
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(i18n::t(locale, "partnerPortal.addCustomer")),
        );

    let body: Div = match state {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_2();
            for _ in 0..3 {
                wrap = wrap.child(skeleton(px(700.), px(40.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div().child(empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>)),
        Loadable::Ready(rows) if rows.is_empty() => div().child(empty_state("🤝", i18n::t(locale, "partnerPortal.customers.empty"), None, None::<Div>)),
        Loadable::Ready(rows) => {
            let col_header = div()
                .grid()
                .grid_cols(4)
                .gap_2()
                .px_4()
                .py_2()
                .bg(theme::alpha(theme::MUTED, 0.35))
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "partnerPortal.col.customerName"))
                .child(i18n::t(locale, "partnerPortal.col.licenseTier"))
                .child(i18n::t(locale, "partnerPortal.col.activated"))
                .child(i18n::t(locale, "partnerPortal.col.actions"));
            let n = rows.len();
            let mut card = div()
                .w_full()
                .flex()
                .flex_col()
                .rounded(px(theme::RADIUS_XL))
                .overflow_hidden()
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .shadow(theme::surface_shadow())
                .child(col_header);
            for (i, c) in rows.iter().enumerate() {
                card = card.child(customer_row(locale, c, i + 1 == n));
            }
            card
        }
    };

    div().flex().flex_col().gap_1p5().child(header).child(body)
}

// ── Section: 產生授權 + 銷售物料 ───────────────────────────────────────

fn generate_license_card(locale: Locale) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(i18n::t(locale, "partnerPortal.section.generateLicense")))
        .child(
            div()
                .rounded(px(theme::RADIUS_XL))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .shadow(theme::surface_shadow())
                .p(px(13.))
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_size(px(12.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "partnerPortal.license.cliOnly")))
                .child(
                    div()
                        .rounded(px(theme::RADIUS_MD))
                        .bg(theme::alpha(theme::MUTED, 0.6))
                        .px_3()
                        .py_2()
                        .font_family("SF Mono")
                        .text_size(px(11.))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("duduclaw license generate --tier <pro|enterprise> --customer <name> --months <n>"),
                ),
        )
}

fn material_row(label: SharedString, coming_soon: SharedString, is_last: bool) -> Div {
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2p5()
        .child(div().text_size(px(12.5)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(label))
        .child(badge(coming_soon, BadgeVariant::Outline));
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn materials_card(locale: Locale) -> Div {
    let coming_soon = i18n::t(locale, "partnerPortal.materials.comingSoon");
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(section_label(i18n::t(locale, "partnerPortal.section.materials")))
        .child(
            div()
                .rounded(px(theme::RADIUS_XL))
                .overflow_hidden()
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .shadow(theme::surface_shadow())
                .child(material_row(i18n::t(locale, "partnerPortal.materials.slides"), coming_soon.clone(), false))
                .child(material_row(i18n::t(locale, "partnerPortal.materials.dmTemplate"), coming_soon, true)),
        )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<PartnerPortalState>();
    let profile = g.profile.clone();
    let stats = g.stats.clone();
    let customers = g.customers.clone();

    let crumb = breadcrumb("partnerportal-breadcrumb", locale, i18n::t(locale, "license.title"), cx);
    let tabs = license_shell_tabs(locale, "partnerPortal", cx);

    let certified_badge = match &profile {
        Loadable::Ready(p) if p.certified_at.is_some() => badge(i18n::t(locale, "partnerPortal.certified"), BadgeVariant::Success),
        Loadable::Ready(_) => badge(i18n::t(locale, "partnerPortal.pending"), BadgeVariant::Secondary),
        _ => badge("—", BadgeVariant::Secondary),
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "partnerPortal.title")))
                .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "partnerPortal.subtitle"))),
        )
        .child(certified_badge);

    let profile_box: Div = match &profile {
        Loadable::Loading => skeleton(px(760.), px(72.)),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>),
        Loadable::Ready(p) => boxed_group(vec![
            kv_row(
                i18n::t(locale, "partnerPortal.partnerId"),
                div().font_family("SF Mono").text_size(px(12.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(p.partner_id.clone().unwrap_or_else(|| "—".to_string()))),
                false,
            ),
            kv_row(
                i18n::t(locale, "partnerPortal.since"),
                div().text_size(px(12.5)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(format_date(&p.created_at)),
                true,
            ),
        ]),
    };

    let kpi_row: Div = match &stats {
        Loadable::Loading => {
            let mut row = div().flex().gap_2p5();
            for _ in 0..4 {
                row = row.child(skeleton(px(160.), px(70.)));
            }
            row
        }
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", msg), None, None::<Div>),
        Loadable::Ready(s) => div()
            .flex()
            .gap_2p5()
            .child(stat_tile(i18n::t(locale, "partnerPortal.totalSold"), s.total_sold.to_string(), theme::FOREGROUND))
            .child(stat_tile(i18n::t(locale, "partnerPortal.activeCustomers"), s.active_customers.to_string(), theme::SUCCESS))
            .child(stat_tile(i18n::t(locale, "partnerPortal.monthCommission"), format!("NT$ {}", format_dollars(s.this_month_commission_cents)), theme::WARNING))
            .child(stat_tile(i18n::t(locale, "partnerPortal.lifetimeCommission"), format!("NT$ {}", format_dollars(s.lifetime_commission_cents)), theme::BRAND)),
    };

    div()
        .id("partner-portal-page")
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
                .child(profile_box)
                .child(kpi_row)
                .child(customers_section(locale, &customers))
                .child(div().flex().gap_2p5().child(generate_license_card(locale)).child(materials_card(locale))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_reads_the_real_payload_shape() {
        let v = json!({
            "company": "晴天餐飲集團", "tier": "gold", "partner_id": "PTR-2214",
            "certified_at": "2026-02-18T00:00:00+00:00", "created_at": "2026-02-01T00:00:00+00:00",
            "updated_at": "2026-08-01T00:00:00+00:00",
        });
        let p = parse_profile(&v);
        assert_eq!(p.partner_id.as_deref(), Some("PTR-2214"));
        assert!(p.certified_at.is_some());
        assert_eq!(p.created_at, "2026-02-01T00:00:00+00:00");
    }

    #[test]
    fn parse_profile_missing_fields_default_to_uncertified() {
        let p = parse_profile(&json!({}));
        assert!(p.partner_id.is_none());
        assert!(p.certified_at.is_none());
        assert_eq!(p.created_at, "");
    }

    #[test]
    fn parse_stats_reads_every_field() {
        let v = json!({ "total_sold": 23, "active_customers": 18, "this_month_commission_cents": 5610000, "lifetime_commission_cents": 18700000 });
        let s = parse_stats(&v);
        assert_eq!(s.total_sold, 23);
        assert_eq!(s.active_customers, 18);
        assert_eq!(s.this_month_commission_cents, 5610000);
        assert_eq!(s.lifetime_commission_cents, 18700000);
    }

    #[test]
    fn parse_customers_reads_the_real_payload_shape() {
        let v = json!({ "customers": [
            { "id": "c1", "name": "晴天餐飲集團", "tier": "enterprise", "activated_at": "2026-03-02T00:00:00Z", "status": "active", "commission_cents": 0, "notes": null, "created_at": "2026-03-02T00:00:00Z" },
        ]});
        let rows = parse_customers(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "晴天餐飲集團");
        assert_eq!(rows[0].tier, "enterprise");
    }

    #[test]
    fn parse_customers_skips_entries_without_id() {
        let v = json!({ "customers": [{ "name": "x" }] });
        assert!(parse_customers(&v).is_empty());
    }

    #[test]
    fn parse_customers_missing_array_is_empty_not_a_panic() {
        assert!(parse_customers(&json!({})).is_empty());
    }

    #[test]
    fn customer_tier_label_covers_the_three_known_values_and_falls_back() {
        assert_eq!(customer_tier_label(Locale::En, "standard"), i18n::t(Locale::En, "partnerPortal.customerTier.standard"));
        assert_eq!(customer_tier_label(Locale::En, "pro"), i18n::t(Locale::En, "partnerPortal.customerTier.pro"));
        assert_eq!(customer_tier_label(Locale::En, "enterprise"), i18n::t(Locale::En, "partnerPortal.customerTier.enterprise"));
        assert_eq!(customer_tier_label(Locale::En, "mystery").to_string(), "mystery");
    }

    #[test]
    fn format_dollars_groups_thousands() {
        assert_eq!(format_dollars(5610000), "56,100");
        assert_eq!(format_dollars(0), "0");
    }

    #[test]
    fn format_date_is_date_only_or_dash() {
        assert_eq!(format_date("2026-03-02T00:00:00Z"), "2026-03-02");
        assert_eq!(format_date("garbage"), "—");
    }

    #[test]
    fn describe_call_error_prefers_structured_message_over_bare_string() {
        let msg = describe_call_error(&CallError::Rejected(json!({"code": "denied", "message": "權限不足"})));
        assert_eq!(msg, "權限不足");
    }
}
