// WP-S6b2-N (S6b 第二波, 2026-08-21) — 自建技能詳情 (SkillCustomDetail), the
// smallest of this wave's three pages: one record, no wizard, no template
// gallery.
//
// Visual authority: `commercial/design/duduclaw-s6-form-pages/
// SkillCustomDetail.dc.html` — breadcrumb ("技能庫 › {name}") → header
// (icon + name + status badge) → a status banner card → a "欄位" boxed-list
// (顯示名稱/slug/說明/預估節省時間/標籤) → a "累計節省時數" stat card → a
// "建立與審核紀錄" boxed-list → a "封存這個技能" button. The canvas draws
// the `approved` ("已上線") lifecycle state specifically.
//
// Functional ground truth: `web/src/components/skills/CustomSkillDetail.tsx`
// (488 lines) — this port keeps its data model and field list, but per this
// crate's "show real data honestly, not just the one state a static mockup
// drew" philosophy, ALL SIX `CustomSkillStatus` values (`draft`/
// `generating`/`pending_approval`/`approved`/`rejected`/`retired`) get their
// own banner (`status_banner` below; wording for the four the web draws
// mirrors `CustomSkillDetail.tsx` lines 280-318, plus two new honest
// banners this port adds for `draft`/`retired`). Draft/rejected are NOT
// rendered as an editable form — per this crate's "decision-class actions
// assembled, not wired" convention (see below), every status renders the
// SAME read-only field list; a live-editable save path this pass cannot
// verify against a real gateway session would be a bigger fabrication than
// a read-only preview.
//
// ── RPC shapes (verified against `crates/duduclaw-gateway/src/handlers.rs`,
// not guessed) ────────────────────────────────────────────────────────────
//   `skills.custom_list {}` (dispatch handlers.rs:6852, handler
//     `handle_skills_custom_list` ~L16696) → `{ "custom_skills": [ {
//     "id","slug","display_name","description_human","time_saved_value",
//     "time_saved_unit","tags" (comma-separated string, NOT an array),
//     "created_by_user","built_by_agent","status","approval_id",
//     "rejection_reason","created_at","updated_at","approved_at",
//     "usage_count","saved_hours_estimate" } ], "count" }` — field shapes
//     read directly from `Self::custom_skill_to_json` (handlers.rs:16261).
//     There is NO dedicated get-by-id RPC: the real web page
//     (`CustomSkillDetail.tsx` ~L135-136) fetches the whole list and finds
//     the target row client-side (`res.custom_skills.find((s) => s.id ===
//     id)`); this page does the same (`find_by_id` below).
//   `skills.custom_retire {"id"}` (dispatch handlers.rs:6853, handler
//     `handle_skills_custom_retire` ~L16743) → `{ "success","id",
//     "status": "retired" }` — see "assembled-not-wired" below.
//
// ── Assembled-not-wired: "封存這個技能" ───────────────────────────────────
// Renders with the exact canvas copy/placement as a real-looking, NOT
// `disabled: true`, button — but its `on_click` is an empty closure, same
// "looks clickable, does nothing" shape `mail.rs`'s "確認寄出"/"不要寄"
// buttons establish (rather than `mcp_keys.rs`'s dimmed `disabled: true`
// shape). This pass has no way to live-verify `skills.custom_retire`
// against a real authenticated session whose custom skill genuinely exists
// and is eligible to retire (no live entry point sets `id` yet — see
// below), so a half-tested write path would be a worse lie than an honest
// stub. Only rendered when `record.status != "retired"` (mirrors the web's
// own `canRetire`).
//
// ── No live entry point this round ────────────────────────────────────────
// Nothing in this crate yet has a "我的技能" list row to wire a real
// navigation click from — `screens::skills.rs`'s own header comment says
// the "市場" tab is the only one implemented this round; "我的技能" is an
// explicit stub even `skill_new.rs` (WP-S6b2-M) doesn't touch, its own
// scope stopping at the create/generate/submit wizard.
// `SkillCustomDetailState::id` therefore starts `None` — this page is
// reachable ONLY via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=skillCustom` this
// round, rendering an honest "尚未選擇技能" empty state plus a real
// breadcrumb back-link to 技能庫. `id` is a plain `pub` field a future
// "我的技能" row's click handler sets directly — the same "starts empty, a
// future caller sets it" shape this wave's sibling `EditAgentState` also
// uses.

use gpui::{div, prelude::*, px, Context, Div, Global, IntoElement, Rgba, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::dashboard::Loadable;
use crate::screens::goals::{relative_time, spawn_goal_call};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

const CONTENT_WIDTH: f32 = 660.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct CustomSkillRecord {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub description_human: String,
    pub time_saved_value: f64,
    pub time_saved_unit: String,
    /// Comma-separated tags, as the wire shape carries them — see
    /// `split_tags` for the display-time split.
    pub tags: String,
    pub created_by_user: String,
    pub built_by_agent: String,
    /// Canonical `CustomSkillStatus::as_str()` value — see `normalize_status`
    /// for the fail-safe mapping onto the six known tokens.
    pub status: String,
    pub approval_id: Option<String>,
    pub rejection_reason: Option<String>,
    pub created_at: String,
    pub approved_at: Option<String>,
    pub usage_count: i64,
    pub saved_hours_estimate: f64,
}

/// `skills.custom_list`'s `custom_skills` array → typed rows. Malformed
/// entries (missing `id`) are skipped, never panicking — same defensive
/// shape `mcp_keys.rs::parse_mcp_keys_list` uses.
pub fn parse_custom_skill_list(v: &Value) -> Vec<CustomSkillRecord> {
    v.get("custom_skills")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_one_record).collect())
        .unwrap_or_default()
}

fn parse_one_record(v: &Value) -> Option<CustomSkillRecord> {
    let id = v.get("id")?.as_str()?.to_string();
    Some(CustomSkillRecord {
        id,
        slug: v.get("slug").and_then(Value::as_str).unwrap_or("").to_string(),
        display_name: v.get("display_name").and_then(Value::as_str).unwrap_or("").to_string(),
        description_human: v.get("description_human").and_then(Value::as_str).unwrap_or("").to_string(),
        time_saved_value: v.get("time_saved_value").and_then(Value::as_f64).unwrap_or(0.0),
        time_saved_unit: v.get("time_saved_unit").and_then(Value::as_str).unwrap_or("minutes_per_use").to_string(),
        tags: v.get("tags").and_then(Value::as_str).unwrap_or("").to_string(),
        created_by_user: v.get("created_by_user").and_then(Value::as_str).unwrap_or("").to_string(),
        built_by_agent: v.get("built_by_agent").and_then(Value::as_str).unwrap_or("").to_string(),
        status: v.get("status").and_then(Value::as_str).unwrap_or("draft").to_string(),
        approval_id: v.get("approval_id").and_then(Value::as_str).map(str::to_string),
        rejection_reason: v.get("rejection_reason").and_then(Value::as_str).map(str::to_string),
        created_at: v.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
        approved_at: v.get("approved_at").and_then(Value::as_str).map(str::to_string),
        usage_count: v.get("usage_count").and_then(Value::as_i64).unwrap_or(0),
        saved_hours_estimate: v.get("saved_hours_estimate").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// Client-side "get by id" over the whole list — see this module's header
/// comment for why no dedicated RPC exists (mirrors `CustomSkillDetail.tsx`'s
/// own `res.custom_skills.find((s) => s.id === id)`).
pub fn find_by_id(records: &[CustomSkillRecord], id: &str) -> Option<CustomSkillRecord> {
    records.iter().find(|r| r.id == id).cloned()
}

/// Mirrors `CustomSkillDetail.tsx`'s own local `splitTags` — comma-separated
/// on the wire, trimmed + empty-filtered for display.
pub fn split_tags(tags: &str) -> Vec<String> {
    tags.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Fail-safe status normalization — mirrors `status-meta.ts`'s
/// `META[status] ?? { ...draft, neutral }` default: an unrecognized status
/// (should never happen against a real gateway, but the wire is untrusted
/// JSON) degrades to `draft`'s neutral treatment rather than panicking or
/// rendering nothing.
pub fn normalize_status(status: &str) -> &'static str {
    match status {
        "draft" => "draft",
        "generating" => "generating",
        "pending_approval" => "pending_approval",
        "approved" => "approved",
        "rejected" => "rejected",
        "retired" => "retired",
        _ => "draft",
    }
}

/// Retire is a valid transition from every status except `retired` itself —
/// mirrors `CustomSkillDetail.tsx`'s own `canRetire`.
pub fn can_retire(status: &str) -> bool {
    normalize_status(status) != "retired"
}

fn status_label_key(status: &str) -> &'static str {
    match normalize_status(status) {
        "draft" => "skillCustom.status.draft",
        "generating" => "skillCustom.status.generating",
        "pending_approval" => "skillCustom.status.pendingApproval",
        "approved" => "skillCustom.status.approved",
        "rejected" => "skillCustom.status.rejected",
        _ => "skillCustom.status.retired",
    }
}

pub fn status_badge_variant(status: &str) -> BadgeVariant {
    match normalize_status(status) {
        "approved" => BadgeVariant::Success,
        "pending_approval" => BadgeVariant::Warning,
        "generating" => BadgeVariant::Info,
        "rejected" => BadgeVariant::Destructive,
        _ => BadgeVariant::Outline, // draft / retired — neutral
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Draft,
    Generating,
    Pending,
    Approved,
    Rejected,
    Retired,
}

/// Which of the six status banners a record's status maps to — split out as
/// its own pure function so each mapping is independently testable (see
/// `tests::banner_kind_for_status_maps_each_status_to_its_own_banner`).
pub fn banner_kind_for_status(status: &str) -> BannerKind {
    match normalize_status(status) {
        "draft" => BannerKind::Draft,
        "generating" => BannerKind::Generating,
        "pending_approval" => BannerKind::Pending,
        "approved" => BannerKind::Approved,
        "rejected" => BannerKind::Rejected,
        _ => BannerKind::Retired,
    }
}

/// Trims a self-reported numeric estimate to how a person would type it —
/// `15.0` → `"15"`, `2.5` → `"2.5"` — rather than JSON's raw float
/// formatting. Never panics on a non-finite value (defensive against a
/// malformed wire payload).
pub fn format_trimmed_number(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// One decimal place, matching the canvas's "18.5 h" stat exactly.
pub fn format_saved_hours(v: f64) -> String {
    format!("{v:.1}")
}

/// Mirrors `status-meta.ts`'s own `formatTimeSaved`: the unit selects a
/// per-unit phrasing key, unrecognized/free-string units fall back to the
/// per-use phrasing (same default branch the web version takes).
pub fn format_time_saved(locale: Locale, value: f64, unit: &str) -> SharedString {
    let formatted = format_trimmed_number(value);
    if unit == "hours_per_month" {
        i18n::t1(locale, "skillCustom.timeSaved.hoursPerMonth", "value", &formatted)
    } else {
        i18n::t1(locale, "skillCustom.timeSaved.minutesPerUse", "value", &formatted)
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct SkillCustomDetailState {
    /// Target record id — see this module's header comment for why this
    /// starts `None` and who is expected to set it.
    pub id: Option<String>,
    /// Which id `skills.custom_list` has already been fired for — re-fires
    /// on a genuine id change, not on every render.
    requested_for: Option<String>,
    /// `Ready(None)` is a real, distinct state from `Loading`/`Failed`: the
    /// list fetched successfully but no row matched `id` (an honest
    /// "not found", never conflated with a network/auth failure).
    pub record: Loadable<Option<CustomSkillRecord>>,
}

impl Default for SkillCustomDetailState {
    fn default() -> Self {
        Self { id: None, requested_for: None, record: Loadable::Loading }
    }
}

impl Global for SkillCustomDetailState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    let Some(id) = cx.default_global::<SkillCustomDetailState>().id.clone() else {
        return;
    };
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<SkillCustomDetailState>().requested_for.as_deref() == Some(id.as_str()) {
        return;
    }
    {
        let g = cx.default_global::<SkillCustomDetailState>();
        g.requested_for = Some(id.clone());
        g.record = Loadable::Loading;
    }
    let tx = state.session_tx.clone();
    let target_id = id;
    spawn_goal_call(cx, tx, "skills.custom_list", json!({}), move |cx, result| {
        let loadable: Loadable<Option<CustomSkillRecord>> =
            result.map(|v| find_by_id(&parse_custom_skill_list(&v), &target_id)).into();
        cx.default_global::<SkillCustomDetailState>().record = loadable;
    });
}

// ── Shared "boxed-list" primitives (local to this file — same "duplicate
// the grammar, don't widen a shared module for one unrelated page"
// convention `agents_detail.rs`'s own header comment documents; see that
// file's lines 36-42 and `settings_common.rs`'s header comment for when
// sharing IS appropriate — a single standalone detail page like this one is
// not that case) ──────────────────────────────────────────────────────────

fn meta_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

fn kv_row(label: impl Into<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(38.))
        .px_3p5()
        .text_size(px(theme::TEXT_SM))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
        .child(value)
}

/// A wrapping variant of `kv_row` for the one field (說明) whose value can
/// run long enough to need multiple lines — matches the canvas's own
/// `align-items: flex-start` treatment for that row specifically, every
/// other row stays single-line-centered via plain `kv_row`.
fn kv_row_wrap(label: impl Into<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap_3()
        .px_3p5()
        .py_2p5()
        .text_size(px(theme::TEXT_SM))
        .child(div().flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
        .child(div().flex_1().min_w_0().text_right().child(value))
}

fn boxed_group(rows: Vec<Div>) -> Div {
    let n = rows.len();
    let mut container = div()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border());
    for (i, row) in rows.into_iter().enumerate() {
        container = container.child(if i + 1 < n { row.border_b_1().border_color(theme::border()) } else { row });
    }
    container
}

fn plain_value(text: SharedString) -> Div {
    div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(text)
}

fn mono_value(text: SharedString) -> Div {
    div().font_family("SF Mono").text_size(px(12.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(text)
}

fn dash_or(text: &str) -> SharedString {
    if text.trim().is_empty() {
        SharedString::from("—")
    } else {
        SharedString::from(text.to_string())
    }
}

fn tags_value(tags: Vec<String>) -> Div {
    let mut row = div().flex().flex_wrap().justify_end().gap_1();
    for t in tags {
        row = row.child(badge(SharedString::from(t), BadgeVariant::Secondary));
    }
    row
}

/// A small circle-with-initial avatar + name, matching the canvas's "C
/// Coder" row for 撰寫員工 — CJK-safe (`.chars().next()`, never a raw byte
/// slice, per this crate's coding conventions).
fn agent_avatar_row(name: &str) -> Div {
    if name.trim().is_empty() {
        return div().child(plain_value(SharedString::from("—")));
    }
    let initial = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .w(px(18.))
                .h(px(18.))
                .rounded_full()
                .bg(theme::alpha(theme::MUTED, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(9.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .child(initial),
        )
        .child(plain_value(SharedString::from(name.to_string())))
}

fn approved_value(locale: Locale, iso: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(theme::TEXT_SM))
        .text_color(theme::alpha(theme::SUCCESS, 1.0))
        .child("✓")
        .child(relative_time(locale, iso, chrono::Utc::now()))
}

fn section(title: SharedString, body: impl IntoElement) -> Div {
    div().flex().flex_col().gap_1p5().child(meta_label(title)).child(body)
}

/// Common "breadcrumb + single body" container shared by the three simple
/// states (`!has_id` / `Failed` / `Ready(None)`) — the `Ready(Some(record))`
/// branch has too many distinct children to fit this shape and builds its
/// own chain directly.
fn page_shell(bc: Div, body: impl IntoElement) -> Div {
    div().w(px(CONTENT_WIDTH)).flex().flex_col().gap_5().pb_8().child(bc).child(body)
}

// ── Breadcrumb ─────────────────────────────────────────────────────────

fn breadcrumb(locale: Locale, leaf: SharedString, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("skillcustom-breadcrumb-back")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.skills.title"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "skills";
                    cx.notify();
                })),
        )
        .child(div().child("›"))
        .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(leaf))
}

fn back_to_skills_button(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    button(
        "skillcustom-back-to-list",
        i18n::t(locale, "skillCustom.backToList"),
        ButtonVariant::Secondary,
        false,
        None,
        cx.listener(|this, _ev, _window, cx| {
            this.active_page = "skills";
            cx.notify();
        }),
    )
}

// ── Header + status banner ────────────────────────────────────────────

fn header_row(locale: Locale, record: &CustomSkillRecord) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(
                    div()
                        .w(px(34.))
                        .h(px(34.))
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(theme::MUTED, 1.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(16.))
                        .child("🧩"),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_BASE))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(SharedString::from(record.display_name.clone())),
                ),
        )
        .child(badge(i18n::t(locale, status_label_key(&record.status)), status_badge_variant(&record.status)))
}

fn banner_card(icon: &'static str, title: SharedString, title_color: Rgba, hint: Option<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .w(px(36.))
                .h(px(36.))
                .rounded_full()
                .bg(theme::alpha(theme::MUTED, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .text_size(px(16.))
                .child(icon),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(title_color).child(title))
                .children(hint.map(|h| div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(h))),
        )
}

/// Renders one of six honest banners per `banner_kind_for_status` — see
/// this module's header comment for why all six get a banner, not just the
/// `approved` state the canvas draws.
fn status_banner(locale: Locale, record: &CustomSkillRecord) -> Div {
    let muted = theme::alpha(theme::MUTED_FOREGROUND, 1.0);
    match banner_kind_for_status(&record.status) {
        BannerKind::Draft => banner_card("📝", i18n::t(locale, "skillCustom.banner.draftTitle"), muted, None),
        BannerKind::Generating => banner_card("⏳", i18n::t(locale, "skillCustom.banner.generatingTitle"), theme::alpha(theme::INFO, 1.0), None),
        BannerKind::Pending => banner_card("🕘", i18n::t(locale, "skillCustom.banner.pendingTitle"), theme::alpha(theme::WARNING, 1.0), None),
        BannerKind::Approved => {
            let hint = Some(i18n::t(locale, "skillCustom.banner.approvedHint"));
            banner_card("🎉", i18n::t(locale, "skillCustom.banner.approvedTitle"), theme::alpha(theme::SUCCESS, 1.0), hint)
        }
        BannerKind::Rejected => {
            let hint = match record.rejection_reason.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(reason) => SharedString::from(reason.to_string()),
                None => i18n::t(locale, "skillCustom.banner.rejectedNoReason"),
            };
            banner_card("↩️", i18n::t(locale, "skillCustom.banner.rejectedTitle"), theme::alpha(theme::DESTRUCTIVE, 1.0), Some(hint))
        }
        BannerKind::Retired => banner_card("📦", i18n::t(locale, "skillCustom.banner.retiredTitle"), muted, None),
    }
}

// ── Field / savings / history groups ──────────────────────────────────

fn fields_group(locale: Locale, record: &CustomSkillRecord) -> Div {
    let mut rows = vec![
        kv_row(i18n::t(locale, "skillCustom.field.displayName"), plain_value(SharedString::from(record.display_name.clone()))),
        kv_row(i18n::t(locale, "skillCustom.field.slug"), mono_value(SharedString::from(record.slug.clone()))),
        kv_row_wrap(i18n::t(locale, "skillCustom.field.description"), plain_value(dash_or(&record.description_human))),
        kv_row(
            i18n::t(locale, "skillCustom.field.timeSaved"),
            plain_value(format_time_saved(locale, record.time_saved_value, &record.time_saved_unit)),
        ),
    ];
    let tags = split_tags(&record.tags);
    if !tags.is_empty() {
        rows.push(kv_row(i18n::t(locale, "skillCustom.field.tags"), tags_value(tags)));
    }
    boxed_group(rows)
}

fn savings_card(locale: Locale, record: &CustomSkillRecord) -> Div {
    div()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .p_4()
        .flex()
        .flex_col()
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(theme::TEXT_2XL))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t1(locale, "skillCustom.savings.hoursValue", "value", &format_saved_hours(record.saved_hours_estimate))),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "skillCustom.savings.estimateNote")),
        )
        .child(
            div()
                .mt_2()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "skillCustom.savings.usedTimes", "count", &record.usage_count.to_string())),
        )
}

fn history_group(locale: Locale, record: &CustomSkillRecord) -> Div {
    let mut rows = vec![
        kv_row(i18n::t(locale, "skillCustom.field.builtBy"), agent_avatar_row(&record.built_by_agent)),
        kv_row(i18n::t(locale, "skillCustom.field.createdBy"), plain_value(dash_or(&record.created_by_user))),
        kv_row(i18n::t(locale, "skillCustom.field.createdAt"), plain_value(relative_time(locale, &record.created_at, chrono::Utc::now()))),
    ];
    if let Some(approval_id) = record.approval_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        rows.push(kv_row(i18n::t(locale, "skillCustom.field.approvalId"), mono_value(SharedString::from(approval_id.to_string()))));
    }
    if let Some(approved_at) = record.approved_at.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        rows.push(kv_row(i18n::t(locale, "skillCustom.field.approvedAt"), approved_value(locale, approved_at)));
    }
    if let Some(reason) = record.rejection_reason.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        rows.push(kv_row(i18n::t(locale, "skillCustom.field.rejectionReason"), plain_value(SharedString::from(reason.to_string()))));
    }
    boxed_group(rows)
}

/// The one decision-class action on this page — see this module's header
/// comment ("Assembled-not-wired") for why the button looks fully live
/// (canvas parity) but its click handler is empty. `None` when
/// `!can_retire(...)` (already `retired`).
fn actions_row(locale: Locale, record: &CustomSkillRecord) -> Option<Div> {
    if !can_retire(&record.status) {
        return None;
    }
    Some(
        div().flex().items_center().gap_2p5().pt_1().child(button(
            SharedString::from(format!("skillcustom-retire-{}", record.id)),
            i18n::t(locale, "skillCustom.retire"),
            ButtonVariant::Secondary,
            false, // looks clickable per canvas — NOT `disabled: true`, see header comment
            None,
            |_ev, _window, _app| {},
        )),
    )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);

    let locale = state.locale;
    let g = cx.default_global::<SkillCustomDetailState>();
    let has_id = g.id.is_some();
    let record_state = g.record.clone();

    let outer = div().id("skill-custom-detail-page").size_full().overflow_y_scroll().flex().flex_col().items_center().p_4();

    if !has_id {
        let bc = breadcrumb(locale, i18n::t(locale, "skillCustom.title"), cx);
        let cta = back_to_skills_button(locale, cx);
        let body = empty_state("🧩", i18n::t(locale, "skillCustom.noSelection.title"), Some(i18n::t(locale, "skillCustom.noSelection.desc")), Some(cta));
        return outer.child(page_shell(bc, body));
    }

    match record_state {
        Loadable::Loading => outer.child(
            div()
                .w(px(CONTENT_WIDTH))
                .flex()
                .flex_col()
                .gap_3()
                .pt_8()
                .child(skeleton(px(CONTENT_WIDTH), px(56.)))
                .child(skeleton(px(CONTENT_WIDTH), px(72.)))
                .child(skeleton(px(CONTENT_WIDTH), px(180.))),
        ),
        Loadable::Failed(msg) => {
            let bc = breadcrumb(locale, i18n::t(locale, "skillCustom.title"), cx);
            let body = empty_state("⚠️", i18n::t1(locale, "native.home.card.errorPrefix", "message", &msg), None, None::<Div>);
            outer.child(page_shell(bc, body))
        }
        Loadable::Ready(None) => {
            let bc = breadcrumb(locale, i18n::t(locale, "skillCustom.title"), cx);
            let cta = back_to_skills_button(locale, cx);
            let body = empty_state("🧩", i18n::t(locale, "skillCustom.notFound"), None, Some(cta));
            outer.child(page_shell(bc, body))
        }
        Loadable::Ready(Some(record)) => {
            let leaf = SharedString::from(record.display_name.clone());
            let mut content = div()
                .w(px(CONTENT_WIDTH))
                .flex()
                .flex_col()
                .gap_5()
                .pb_8()
                .child(breadcrumb(locale, leaf, cx))
                .child(header_row(locale, &record))
                .child(status_banner(locale, &record))
                .child(section(i18n::t(locale, "skillCustom.fieldsTitle"), fields_group(locale, &record)))
                .child(section(i18n::t(locale, "skillCustom.savingsTitle"), savings_card(locale, &record)))
                .child(section(i18n::t(locale, "skillCustom.historyTitle"), history_group(locale, &record)));
            if let Some(actions) = actions_row(locale, &record) {
                content = content.child(actions);
            }
            outer.child(content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_record_json() -> Value {
        json!({
            "id": "cs_1", "slug": "reply-digest-summarizer", "display_name": "回覆摘要生成器",
            "description_human": "彙整未讀信件重點", "time_saved_value": 15.0,
            "time_saved_unit": "minutes_per_use", "tags": "信箱, 摘要",
            "created_by_user": "louis.li", "built_by_agent": "coder", "status": "approved",
            "approval_id": "apr_8f21c0", "rejection_reason": null,
            "created_at": "2026-08-01T00:00:00Z", "updated_at": "2026-08-10T00:00:00Z",
            "approved_at": "2026-08-10T00:00:00Z", "usage_count": 47, "saved_hours_estimate": 18.5
        })
    }

    #[test]
    fn parse_custom_skill_list_reads_every_field() {
        let v = json!({ "custom_skills": [sample_record_json()], "count": 1 });
        let rows = parse_custom_skill_list(&v);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.id, "cs_1");
        assert_eq!(r.slug, "reply-digest-summarizer");
        assert_eq!(r.display_name, "回覆摘要生成器");
        assert_eq!(r.time_saved_value, 15.0);
        assert_eq!(r.time_saved_unit, "minutes_per_use");
        assert_eq!(r.tags, "信箱, 摘要");
        assert_eq!(r.built_by_agent, "coder");
        assert_eq!(r.status, "approved");
        assert_eq!(r.approval_id.as_deref(), Some("apr_8f21c0"));
        assert!(r.rejection_reason.is_none());
        assert_eq!(r.approved_at.as_deref(), Some("2026-08-10T00:00:00Z"));
        assert_eq!(r.usage_count, 47);
        assert_eq!(r.saved_hours_estimate, 18.5);
    }

    #[test]
    fn parse_custom_skill_list_missing_array_is_empty_not_panicking() {
        assert!(parse_custom_skill_list(&json!({})).is_empty());
        assert!(parse_custom_skill_list(&json!(null)).is_empty());
        assert!(parse_custom_skill_list(&json!({"custom_skills": "not-an-array"})).is_empty());
    }

    #[test]
    fn parse_custom_skill_list_skips_entries_without_id() {
        let v = json!({ "custom_skills": [{ "slug": "x" }, sample_record_json()] });
        let rows = parse_custom_skill_list(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "cs_1");
    }

    #[test]
    fn find_by_id_found_and_not_found() {
        let v = json!({ "custom_skills": [sample_record_json()] });
        let rows = parse_custom_skill_list(&v);
        assert!(find_by_id(&rows, "cs_1").is_some());
        assert!(find_by_id(&rows, "cs_missing").is_none());
        assert!(find_by_id(&[], "cs_1").is_none());
    }

    #[test]
    fn split_tags_handles_empty_whitespace_and_comma_heavy_strings() {
        assert_eq!(split_tags(""), Vec::<String>::new());
        assert_eq!(split_tags("   "), Vec::<String>::new());
        assert_eq!(split_tags("信箱, 摘要"), vec!["信箱".to_string(), "摘要".to_string()]);
        assert_eq!(split_tags(",,a,,b,,"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(split_tags("  a  ,  b  "), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn format_trimmed_number_whole_and_fractional() {
        assert_eq!(format_trimmed_number(15.0), "15");
        assert_eq!(format_trimmed_number(2.5), "2.5");
        assert_eq!(format_trimmed_number(18.50), "18.5");
        assert_eq!(format_trimmed_number(0.0), "0");
        assert_eq!(format_trimmed_number(f64::NAN), "0");
    }

    #[test]
    fn format_saved_hours_is_one_decimal() {
        assert_eq!(format_saved_hours(18.5), "18.5");
        assert_eq!(format_saved_hours(0.0), "0.0");
        assert_eq!(format_saved_hours(3.0), "3.0");
    }

    #[test]
    fn format_time_saved_routes_by_unit_and_falls_back_for_unknown_units() {
        let minutes = format_time_saved(Locale::ZhTw, 15.0, "minutes_per_use");
        assert!(minutes.contains("15"));
        assert!(minutes.contains('分'));
        let hours = format_time_saved(Locale::ZhTw, 2.5, "hours_per_month");
        assert!(hours.contains("2.5"));
        assert!(hours.contains('時'));
        // Unrecognized/free-string units fall back to the per-use phrasing,
        // mirroring `status-meta.ts::formatTimeSaved`'s own default branch.
        let unknown = format_time_saved(Locale::ZhTw, 5.0, "something_else");
        assert!(unknown.contains('分'));
    }

    #[test]
    fn normalize_status_maps_all_six_and_defaults_unknown_to_draft() {
        for s in ["draft", "generating", "pending_approval", "approved", "rejected", "retired"] {
            assert_eq!(normalize_status(s), s);
        }
        assert_eq!(normalize_status("bogus"), "draft");
        assert_eq!(normalize_status(""), "draft");
    }

    #[test]
    fn banner_kind_for_status_maps_each_status_to_its_own_banner() {
        assert_eq!(banner_kind_for_status("draft"), BannerKind::Draft);
        assert_eq!(banner_kind_for_status("generating"), BannerKind::Generating);
        assert_eq!(banner_kind_for_status("pending_approval"), BannerKind::Pending);
        assert_eq!(banner_kind_for_status("approved"), BannerKind::Approved);
        assert_eq!(banner_kind_for_status("rejected"), BannerKind::Rejected);
        assert_eq!(banner_kind_for_status("retired"), BannerKind::Retired);
        // normalize_status("bogus") fail-safes to "draft" — banner_kind
        // inherits the same fail-safe default.
        assert_eq!(banner_kind_for_status("bogus"), BannerKind::Draft);
    }

    #[test]
    fn status_badge_variant_matches_tone_per_status() {
        assert_eq!(status_badge_variant("approved"), BadgeVariant::Success);
        assert_eq!(status_badge_variant("pending_approval"), BadgeVariant::Warning);
        assert_eq!(status_badge_variant("generating"), BadgeVariant::Info);
        assert_eq!(status_badge_variant("rejected"), BadgeVariant::Destructive);
        assert_eq!(status_badge_variant("draft"), BadgeVariant::Outline);
        assert_eq!(status_badge_variant("retired"), BadgeVariant::Outline);
    }

    #[test]
    fn can_retire_is_false_only_for_retired() {
        for s in ["draft", "generating", "pending_approval", "approved", "rejected"] {
            assert!(can_retire(s));
        }
        assert!(!can_retire("retired"));
    }
}
