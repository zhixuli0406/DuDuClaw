// WP-S6b1-L (S6b 第一波) — "經銷" (`DistributorsPage.dc.html`, B19). No
// `nav.rs` entry of its own this round — reached via `screens::
// manage_advanced`'s 經銷 row (wired this same pass, see that module's own
// header comment) or the debug-page override
// (`DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=distributors`); self-attached in
// `shell.rs` per the "D 先掛好分支就直接可達，未掛就自己掛" precedent that
// file's own comment blocks already establish for every S5b3-wave page.
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/
// DistributorsPage.dc.html` — breadcrumb (進階設定 › 經銷) → header (title/
// subtitle, no button here — the canvas puts it inline above the ledger
// instead) → 3 stat tiles → 已核發授權 boxed table (經銷商/機器指紋/版本/
// 核發日期/狀態/操作) → footer note. Per this task's own "版面禁抄 web" rule,
// `web/src/pages/DistributorsPage.tsx` (branding tab, sign-bundle dialog,
// per-field white-label grants) is a functional RPC cross-reference only,
// never a layout source — this page ports none of that chrome, only the
// distributor-ledger concept.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) — ONE call, not two ──────────────────────
//   `distributor.list` (dispatch match arm L6908, handler
//   `handle_distributor_list` L17870, `require_admin!()`) →
//   `{"distributors": [{"id","name","contact","note","status","created_at",
//   "updated_at","licenses": [{"id","distributor_id","subscription_id",
//   "customer_id","tier","machine_fingerprint","issued_at","expires_at",
//   "status","revoked_at","license_blob","last_refresh_at"}]}]}`
//   (`crates/duduclaw-gateway/src/distributor_store.rs::DistributorProfile`/
//   `IssuedLicense`). `distributor.status` (dispatch L6904, handler L17853)
//   ALSO exists and exposes a `stats` rollup, but every number that rollup
//   carries (`total_distributors`/`active_licenses`/`revoked_licenses`) is
//   already derivable from `distributor.list`'s own payload — and the two
//   numbers the canvas's stat tiles actually need beyond a plain count
//   (本月核發授權／即將到期 30 天內) have NO backing field on either RPC, so
//   they are computed client-side from the real `issued_at`/`expires_at`/
//   `status` values (see `compute_summary` below — a real derived stat from
//   real data, not a fabricated one; same "growth.rs computes streaks from
//   raw payloads" precedent). One RPC call covers the whole page.
//
// ── Deliberate deviations from the canvas, documented not silent ─────────
// 1. **版本 badge**: every distributor-issued license currently carries
//    `tier: "oem"` LITERALLY — `handle_distributor_issue` in handlers.rs
//    hardcodes the string. There is no code path that produces the
//    canvas's fictional 企業版/個人版 two-tier distinction (`max_agents`
//    decides "個人量/無限企業包" at issue time but is never persisted back
//    onto the `IssuedLicense` row, only baked into the opaque signed
//    `license_blob`). `tier_badge` below stays generic over the full
//    `LicenseTier` snake_case vocabulary (`duduclaw-license/src/tier.rs::
//    as_toml_key`) for forward compatibility, but today it only ever
//    renders the "oem" arm — an honest single real state, not the canvas's
//    two illustrative ones.
// 2. **機器指紋遮罩**: `first4…last4` (matching the canvas's own example
//    values, e.g. `8f2a…c910`), NOT `web/src/pages/DistributorsPage.tsx::
//    maskId`'s `first8…last4` — this task's brief calls for matching the
//    CANVAS's mono mask ("機器指紋 mono 遮罩照畫布"), and the canvas's own
//    rendered digits are 4+4.
// 3. **已核發授權／編輯／移除／檢視 are all decision-type actions —
//    assembled, not wired.** Same "disabled `mds_gpui::button`, zero click
//    handler" pattern `users.rs`'s header comment point (4) and
//    `mcp_keys.rs`'s create/revoke actions already establish for this
//    codebase — applied here even though this task's own brief only spells
//    that rule out explicitly for the 成員 page, for the same reason: a
//    real 核發新授權 needs a distributor picker + a machine-fingerprint
//    field collected from the distributor out-of-band, which is a full form
//    this "清單+徽章+機器指紋遮罩" skeleton pass does not build.

use chrono::{DateTime, Datelike, Utc};
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

const CONTENT_MAX_WIDTH: f32 = 860.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LicenseRow {
    pub distributor_name: String,
    pub machine_fingerprint: String,
    pub tier: String,
    pub issued_at: String,
    pub expires_at: String,
    pub status: String,
}

#[derive(Clone, Default)]
pub struct DistributorsPageData {
    pub total_distributors: i64,
    pub licenses: Vec<LicenseRow>,
}

pub fn parse_distributor_list(v: &Value) -> DistributorsPageData {
    let distributors = v.get("distributors").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut licenses = Vec::new();
    for d in &distributors {
        let name = d.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(lics) = d.get("licenses").and_then(Value::as_array) else { continue };
        for lic in lics {
            licenses.push(LicenseRow {
                distributor_name: name.clone(),
                machine_fingerprint: lic.get("machine_fingerprint").and_then(Value::as_str).unwrap_or("").to_string(),
                tier: lic.get("tier").and_then(Value::as_str).unwrap_or("").to_string(),
                issued_at: lic.get("issued_at").and_then(Value::as_str).unwrap_or("").to_string(),
                expires_at: lic.get("expires_at").and_then(Value::as_str).unwrap_or("").to_string(),
                status: lic.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
            });
        }
    }
    DistributorsPageData { total_distributors: distributors.len() as i64, licenses }
}

/// `(在案經銷商, 本月核發授權, 即將到期 30 天內)` — the last two have no
/// backing RPC field (see this module's own header comment); both are real
/// counts computed from the real `issued_at`/`expires_at`/`status` values,
/// never fabricated.
#[derive(Clone, Copy, Default)]
pub struct Summary {
    pub total_distributors: i64,
    pub issued_this_month: i64,
    pub expiring_30d: i64,
}

pub fn compute_summary(data: &DistributorsPageData, now: DateTime<Utc>) -> Summary {
    let mut issued_this_month = 0i64;
    let mut expiring_30d = 0i64;
    for lic in &data.licenses {
        if let Ok(issued) = DateTime::parse_from_rfc3339(&lic.issued_at) {
            let issued = issued.with_timezone(&Utc);
            if issued.year() == now.year() && issued.month() == now.month() {
                issued_this_month += 1;
            }
        }
        if lic.status == "active" {
            if let Ok(expires) = DateTime::parse_from_rfc3339(&lic.expires_at) {
                let expires = expires.with_timezone(&Utc);
                let delta_days = (expires - now).num_days();
                if (0..=30).contains(&delta_days) {
                    expiring_30d += 1;
                }
            }
        }
    }
    Summary { total_distributors: data.total_distributors, issued_this_month, expiring_30d }
}

/// `"8f2a…c910"` — first 4 + last 4 chars, matching the canvas's own mask
/// digits exactly (see this module's header comment point (2)). Short
/// inputs render whole rather than being truncated into something shorter
/// than the mask markers themselves.
pub fn mask_fingerprint(fp: &str) -> String {
    let chars: Vec<char> = fp.chars().collect();
    if chars.len() <= 10 {
        return fp.to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// `"2026-08-15"` from an RFC3339 timestamp — `"—"` when unparseable (never
/// a fabricated date). Local copy of `mcp_keys.rs::format_created_date`'s
/// same recipe — same "duplicate, don't widen a sibling module's
/// visibility" precedent that file's own header comment already applies.
pub fn format_date_only(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.format("%Y-%m-%d").to_string()).unwrap_or_else(|_| "—".to_string())
}

// ── State ──────────────────────────────────────────────────────────────

pub struct DistributorsState {
    requested: bool,
    pub data: Loadable<DistributorsPageData>,
}

impl Default for DistributorsState {
    fn default() -> Self {
        Self { requested: false, data: Loadable::Loading }
    }
}

impl Global for DistributorsState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "distributors" || cx.default_global::<DistributorsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<DistributorsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "distributor.list", json!({}), |cx, result| {
        cx.default_global::<DistributorsState>().data = result.map(|v| parse_distributor_list(&v)).into();
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

// ── Breadcrumb (2-segment: 進階設定 › 經銷 — same local-duplicate reasoning
// as `users.rs::breadcrumb`, this wave's sibling page). ───────────────────

fn breadcrumb(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("distributors-breadcrumb")
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("distributors-breadcrumb-root")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "nav.manageAdvanced"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "manageAdvanced";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(i18n::t(locale, "distributors.title")))
}

// ── Display helpers ────────────────────────────────────────────────────

/// See this module's header comment point (1) — only the "oem" arm is ever
/// real data today; the rest of `LicenseTier`'s vocabulary is covered for
/// forward compatibility, not because it's reachable now.
fn tier_badge(locale: Locale, tier: &str) -> Div {
    match tier {
        "oem" => badge(i18n::t(locale, "distributors.tier.oem"), BadgeVariant::Info),
        "self_host_pro" => badge(i18n::t(locale, "distributors.tier.selfHostPro"), BadgeVariant::Info),
        "personal_pro_self_host" => badge(i18n::t(locale, "distributors.tier.personalProSelfHost"), BadgeVariant::Secondary),
        "partner" => badge(i18n::t(locale, "distributors.tier.partner"), BadgeVariant::Secondary),
        other => badge(SharedString::from(other.to_string()), BadgeVariant::Outline),
    }
}

fn is_expiring_soon(expires_at: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(expires_at)
        .map(|dt| {
            let delta_days = (dt.with_timezone(&Utc) - now).num_days();
            (0..=30).contains(&delta_days)
        })
        .unwrap_or(false)
}

fn license_status_display(locale: Locale, lic: &LicenseRow, now: DateTime<Utc>) -> (u32, SharedString) {
    match lic.status.as_str() {
        "revoked" => (theme::MUTED_FOREGROUND, i18n::t(locale, "distributors.status.revoked")),
        "active" if is_expiring_soon(&lic.expires_at, now) => (theme::WARNING, i18n::t(locale, "distributors.status.expiringSoon")),
        "active" => (theme::SUCCESS, i18n::t(locale, "distributors.status.active")),
        other => (theme::MUTED_FOREGROUND, SharedString::from(other.to_string())),
    }
}

// ── Stat tiles ─────────────────────────────────────────────────────────

fn stat_tile(label: SharedString, value: SharedString, value_color: u32) -> Div {
    div()
        .flex_1()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .px_4()
        .py_3()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().mt_1().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(value_color, 1.0)).child(value))
}

fn stats_row(locale: Locale, summary: &Loadable<Summary>) -> Div {
    let (dist_v, month_v, exp_v): (SharedString, SharedString, SharedString) = match summary {
        Loadable::Loading | Loadable::Failed(_) => ("—".into(), "—".into(), "—".into()),
        Loadable::Ready(s) => (
            i18n::tn(locale, "distributors.stats.countUnit", &[("n", &s.total_distributors.to_string())]),
            i18n::tn(locale, "distributors.stats.licenseUnit", &[("n", &s.issued_this_month.to_string())]),
            i18n::tn(locale, "distributors.stats.licenseUnit", &[("n", &s.expiring_30d.to_string())]),
        ),
    };
    div()
        .flex()
        .gap_2p5()
        .child(stat_tile(i18n::t(locale, "distributors.stats.distributors"), dist_v, theme::FOREGROUND))
        .child(stat_tile(i18n::t(locale, "distributors.stats.issuedThisMonth"), month_v, theme::FOREGROUND))
        .child(stat_tile(i18n::t(locale, "distributors.stats.expiring30d"), exp_v, theme::WARNING))
}

// ── Table ──────────────────────────────────────────────────────────────

fn header_row(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(theme::alpha(theme::MUTED, 0.35))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(div().flex_1().child(i18n::t(locale, "distributors.col.distributor")))
        .child(div().w(px(120.)).flex_shrink_0().child(i18n::t(locale, "distributors.col.fingerprint")))
        .child(div().w(px(64.)).flex_shrink_0().child(i18n::t(locale, "distributors.col.tier")))
        .child(div().w(px(80.)).flex_shrink_0().child(i18n::t(locale, "distributors.col.issuedAt")))
        .child(div().w(px(96.)).flex_shrink_0().child(i18n::t(locale, "distributors.col.status")))
        .child(div().w(px(56.)).flex_shrink_0().child(""))
}

fn license_row(locale: Locale, lic: &LicenseRow, now: DateTime<Utc>, is_last: bool) -> Div {
    let (dot_color, status_label) = license_status_display(locale, lic, now);

    let status_col = div()
        .w(px(96.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(status_label));

    let fingerprint_col = div()
        .w(px(120.))
        .flex_shrink_0()
        .font_family("SF Mono")
        .text_size(px(11.5))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(SharedString::from(mask_fingerprint(&lic.machine_fingerprint)));

    let actions_col = div().w(px(56.)).flex_shrink_0().child(button(
        SharedString::from(format!("distributors-view-{}-{}", lic.distributor_name, lic.machine_fingerprint)),
        i18n::t(locale, "distributors.action.view"),
        ButtonVariant::Ghost,
        true, // decision-type action — assembled, not wired; see module header §3
        None,
        |_ev, _window, _cx| {},
    ));

    let mut row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(lic.distributor_name.clone())),
        )
        .child(fingerprint_col)
        .child(div().w(px(64.)).flex_shrink_0().child(tier_badge(locale, &lic.tier)))
        .child(div().w(px(80.)).flex_shrink_0().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format_date_only(&lic.issued_at)))
        .child(status_col)
        .child(actions_col);
    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let now = Utc::now();
    let data = cx.default_global::<DistributorsState>().data.clone();
    let summary: Loadable<Summary> = match &data {
        Loadable::Loading => Loadable::Loading,
        Loadable::Failed(e) => Loadable::Failed(e.clone()),
        Loadable::Ready(d) => Loadable::Ready(compute_summary(d, now)),
    };

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "distributors.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "distributors.subtitle")));

    let ledger_header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "distributors.ledger.title")))
        .child(button(
            "distributors-issue",
            i18n::t(locale, "distributors.issue"),
            ButtonVariant::Primary,
            true, // decision-type action — assembled, not wired; see module header §3
            None,
            |_ev, _window, _cx| {},
        ));

    let body = match &data {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(48.))).child(skeleton(px(760.), px(48.))),
        Loadable::Failed(err) => div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(d) if d.licenses.is_empty() => div().child(empty_state("🏢", i18n::t(locale, "distributors.empty"), None, None::<Div>)),
        Loadable::Ready(d) => {
            let n = d.licenses.len();
            boxed_group(
                std::iter::once(header_row(locale))
                    .chain(d.licenses.iter().enumerate().map(|(i, lic)| license_row(locale, lic, now, i + 1 == n)))
                    .collect(),
            )
        }
    };

    let footer = div()
        .text_size(px(11.5))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .text_center()
        .child(i18n::t(locale, "distributors.footerNote"));

    div()
        .id("distributors-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .p_6()
                .child(breadcrumb(locale, cx))
                .child(header)
                .child(stats_row(locale, &summary))
                .child(ledger_header)
                .child(body)
                .child(footer),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_distributor_list_flattens_licenses_with_parent_name() {
        let v = json!({ "distributors": [
            { "name": "安心科技有限公司", "licenses": [
                { "machine_fingerprint": "8f2a11220033c910", "tier": "oem", "issued_at": "2026-06-11T00:00:00Z", "expires_at": "2027-06-11T00:00:00Z", "status": "active" },
            ] },
            { "name": "橙果數位股份有限公司", "licenses": [] },
        ]});
        let data = parse_distributor_list(&v);
        assert_eq!(data.total_distributors, 2);
        assert_eq!(data.licenses.len(), 1);
        assert_eq!(data.licenses[0].distributor_name, "安心科技有限公司");
        assert_eq!(data.licenses[0].tier, "oem");
    }

    #[test]
    fn parse_distributor_list_missing_arrays_are_empty_not_panicking() {
        let data = parse_distributor_list(&json!({}));
        assert_eq!(data.total_distributors, 0);
        assert!(data.licenses.is_empty());
    }

    #[test]
    fn compute_summary_counts_this_month_issuances_and_30_day_expiries() {
        let now = DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z").unwrap().with_timezone(&Utc);
        let data = DistributorsPageData {
            total_distributors: 3,
            licenses: vec![
                // Issued this month, active, expires far out — counts toward
                // issued_this_month only.
                LicenseRow {
                    distributor_name: "a".into(), machine_fingerprint: "x".into(), tier: "oem".into(),
                    issued_at: "2026-08-02T00:00:00Z".into(), expires_at: "2027-08-02T00:00:00Z".into(), status: "active".into(),
                },
                // Issued last month — does NOT count toward issued_this_month.
                // Active and expiring in 10 days — counts toward expiring_30d.
                LicenseRow {
                    distributor_name: "b".into(), machine_fingerprint: "y".into(), tier: "oem".into(),
                    issued_at: "2026-07-24T00:00:00Z".into(), expires_at: "2026-08-31T00:00:00Z".into(), status: "active".into(),
                },
                // Revoked and expiring soon — must NOT count toward
                // expiring_30d (only active licenses can be "expiring soon").
                LicenseRow {
                    distributor_name: "c".into(), machine_fingerprint: "z".into(), tier: "oem".into(),
                    issued_at: "2026-08-05T00:00:00Z".into(), expires_at: "2026-08-25T00:00:00Z".into(), status: "revoked".into(),
                },
            ],
        };
        let summary = compute_summary(&data, now);
        assert_eq!(summary.total_distributors, 3);
        assert_eq!(summary.issued_this_month, 2); // a and c both issued 2026-08
        assert_eq!(summary.expiring_30d, 1); // only b
    }

    #[test]
    fn mask_fingerprint_is_four_plus_four_matching_the_canvas() {
        assert_eq!(mask_fingerprint("8f2a11220033c910"), "8f2a…c910");
        // Short values render whole, never truncated below the mask width.
        assert_eq!(mask_fingerprint("abc123"), "abc123");
    }

    #[test]
    fn format_date_only_is_date_only_or_dash() {
        assert_eq!(format_date_only("2026-06-11T09:30:00Z"), "2026-06-11");
        assert_eq!(format_date_only("not-a-date"), "—");
    }

    #[test]
    fn license_status_display_revoked_beats_expiry_and_active_checks_expiry_window() {
        let now = DateTime::parse_from_rfc3339("2026-08-21T00:00:00Z").unwrap().with_timezone(&Utc);
        let revoked = LicenseRow {
            distributor_name: "a".into(), machine_fingerprint: "x".into(), tier: "oem".into(),
            issued_at: "2026-01-01T00:00:00Z".into(), expires_at: "2026-08-25T00:00:00Z".into(), status: "revoked".into(),
        };
        assert_eq!(license_status_display(Locale::ZhTw, &revoked, now).0, theme::MUTED_FOREGROUND);

        let expiring = LicenseRow { status: "active".into(), expires_at: "2026-08-25T00:00:00Z".into(), ..revoked.clone() };
        assert_eq!(license_status_display(Locale::ZhTw, &expiring, now).0, theme::WARNING);

        let healthy = LicenseRow { status: "active".into(), expires_at: "2027-08-25T00:00:00Z".into(), ..revoked };
        assert_eq!(license_status_display(Locale::ZhTw, &healthy, now).0, theme::SUCCESS);
    }
}
