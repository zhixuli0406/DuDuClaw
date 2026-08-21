// WP-S6b2-N (S6b 第二波, 2026-08-21) — "編輯員工" (EditAgent), the largest
// of this wave's three form-page ports. Sibling of `edit_agent_data.rs`
// (types + pure parsing), `edit_agent_tabs_a.rs` (技能/工具/整合/一般),
// `edit_agent_tabs_b.rs` (大腦/預算/自動化/進階) — split into four files for
// the same file-size reason `tasks.rs`/`tasks_data.rs`/`tasks_detail.rs`/
// `tasks_detail_data.rs`/`tasks_quickview.rs` are split (that five-file
// group is this crate's own closest size precedent for a page this large).
//
// Visual authority: `commercial/design/duduclaw-s6-form-pages/EditAgent.dc.
// html`. Breadcrumb ("員工總覽 › {name} › 編輯") → header ("編輯員工 ·
// {name}" + "變更即時生效，離開頁面前不需要另外儲存" + a green "已儲存"
// status pill + "返回") → two-column layout: a 176px-wide LEFT rail (two
// groups — 能力: 技能/工具/整合；設定: 一般/大腦/預算/自動化/進階, 8
// categories) and a RIGHT content pane of boxed-list sections. The canvas
// draws ONLY 大腦's content in full (see `edit_agent_tabs_b.rs::brain_tab`)
// — the other 7 categories have no canvas artwork and were designed here
// using the SAME kv_row/boxed_group vocabulary, populated from real fields
// on `web/src/pages/agent-form/EditAgentPage.tsx`'s own tab structure (see
// `edit_agent_data.rs`'s header comment for the RPC-field citations, and
// each tab render function in `edit_agent_tabs_a.rs`/`edit_agent_tabs_b.rs`
// for the per-tab "誠實偏差" comment at every web field this page
// deliberately omits).
//
// ── The scope decision this whole page follows (precedent, not a fresh
// call) ───────────────────────────────────────────────────────────────
// `web/src/pages/agent-form/EditAgentPage.tsx` has NO manual save button —
// every field autosaves via debounced `agents.update`/`contract.update`
// calls. Porting live-editing + autosave for 8 tabs × 60+ fields to gpui
// this round is out of scope; this page instead follows `agents_detail.
// rs`'s OWN precedent for its capabilities box (see that file's header
// comment, quoted here): "a toggle that silently does nothing on click
// would be a worse lie than a badge that honestly can't be clicked. Only a
// curated, REAL subset ... is shown." Applied to this entire page:
//   - The LEFT RAIL is real/interactive — clicking a category switches the
//     visible content pane. This is client-local UI state (`EditAgentState::
//     active_tab`), not an RPC, so there is nothing dishonest about it being
//     "just a click handler that flips an enum".
//   - Every field VALUE in a boxed-list row is a REAL value read from the
//     real `agents.inspect`/`contract.get` RPCs (never fabricated, never a
//     static default pretending to be live data) — but every row is
//     READ-ONLY display this round: plain text/badge (`plain_value`/
//     `bool_badge` below, same shape as `agents_detail.rs`'s own
//     `plain_value`/`bool_badge`), no toggle switches that silently no-op,
//     no live-typeable fields, no autosave.
//   - The "已儲存" pill is static decorative text matching the canvas
//     (there is nothing to save on this read-only page, so it's always the
//     resting state — not a fabricated save-in-progress state machine with
//     nothing driving it). "返回" IS real — it navigates back to the p12
//     agent-detail page (`active_page = "agents"`, `AgentsState::view`
//     already stays `Detail` for the whole EditAgent visit — see this
//     file's `header_row`/`breadcrumb`).
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed — see `edit_agent_data.rs`'s own header
// comment for the full field-by-field citation) ──────────────────────────
//   `agents.inspect {"agent_id"}` — dispatch L5462, handler
//   `handle_agents_inspect` L11494 (`json!` block L11536-11659). Fetched
//   once per selected agent id, on first render (mirrors `agents.rs::
//   maybe_fetch_detail`'s "only when needed, latch by id" shape).
//   `contract.get {"agent_id"}` — dispatch L5495, handler
//   `handle_contract_get` L9740. Fetched LAZILY, only the first time the
//   使用者 opens the 工具 tab (mirrors the web's own lazy-load-on-tab-open
//   behavior, `EditAgentPage.tsx` line ~544) — never on initial page load.
//   `channels.status {}` — dispatch L5597 (admin-gated). Fetched lazily,
//   only the first time 整合 tab opens; a `Rejected` (non-admin) outcome
//   latches `Loadable::Failed`, rendered as an honest "未提供" by
//   `edit_agent_tabs_a.rs::integration_tab` — same pattern `agents_detail.
//   rs::channels_group` already establishes, reusing `agents_data::
//   parse_channel_statuses`/`channels_for_agent`/`channel_platform_label`
//   directly rather than re-deriving that parsing.
//
// ── Why state lives in a `gpui::Global`, not a `RootView` field ─────────
// Same constraint `agents.rs`/`goals.rs`'s own module doc comments document:
// this pass may not touch `main.rs` (parallel waves own its wiring), so
// `EditAgentState` is a `gpui::Global` — `Context<RootView>` derefs to
// `&mut App`, which carries `global`/`global_mut`/`default_global`.

use gpui::{div, prelude::*, px, Context, Div, IntoElement, SharedString, Stateful};
use serde_json::json;

use crate::i18n;
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::agents::{avatar_with_status, AgentsState};
use crate::screens::agents_data::{parse_channel_statuses, ChannelStatusItem};
use crate::screens::dashboard::Loadable;
use crate::screens::edit_agent_data::{parse_contract, parse_edit_agent_detail, ContractData, EditAgentDetail};
use crate::screens::goals::spawn_goal_call;
use crate::screens::{edit_agent_tabs_a, edit_agent_tabs_b};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

const CONTENT_WIDTH: f32 = 860.0;

/// The 8 categories the left rail switches between — two groups (能力/設定)
/// matching the canvas's own grouping exactly. Pure client-local UI state,
/// never persisted, never an RPC param (see this file's header comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTab {
    Skills,
    Tools,
    Integration,
    General,
    Brain,
    Budget,
    Automation,
    Advanced,
}

impl EditTab {
    fn label_key(self) -> &'static str {
        match self {
            EditTab::Skills => "native.editAgent.tab.skills",
            EditTab::Tools => "native.editAgent.tab.tools",
            EditTab::Integration => "native.editAgent.tab.integration",
            EditTab::General => "native.editAgent.tab.general",
            EditTab::Brain => "native.editAgent.tab.brain",
            EditTab::Budget => "native.editAgent.tab.budget",
            EditTab::Automation => "native.editAgent.tab.automation",
            EditTab::Advanced => "native.editAgent.tab.advanced",
        }
    }

    /// Stable, fully-qualified `gpui::ElementId` string for this rail row —
    /// a fixed `&'static str` (not `(&str, &str)`; `ElementId` has no
    /// `From<(&str, &str)>` impl in this crate's pinned gpui rev) rather
    /// than `format!("{:?}", self)` so the id never depends on `Debug`'s
    /// output shape.
    fn rail_id(self) -> &'static str {
        match self {
            EditTab::Skills => "edit-agent-rail-skills",
            EditTab::Tools => "edit-agent-rail-tools",
            EditTab::Integration => "edit-agent-rail-integration",
            EditTab::General => "edit-agent-rail-general",
            EditTab::Brain => "edit-agent-rail-brain",
            EditTab::Budget => "edit-agent-rail-budget",
            EditTab::Automation => "edit-agent-rail-automation",
            EditTab::Advanced => "edit-agent-rail-advanced",
        }
    }
}

// ── Global state ───────────────────────────────────────────────────────

pub struct EditAgentState {
    pub target_id: Option<String>,
    pub active_tab: EditTab,

    pub detail: Loadable<EditAgentDetail>,
    detail_loaded_for: Option<String>,

    /// Lazily fetched only once the 工具 tab is opened (see this file's
    /// header comment) — `contract_requested` latches per selected agent,
    /// reset by `select_agent`.
    pub contract: Loadable<ContractData>,
    contract_requested: bool,

    /// Lazily fetched only once the 整合 tab is opened; global (not
    /// per-agent) the same way `AgentsState::channels` is — see that
    /// field's own doc comment.
    pub channels: Loadable<Vec<ChannelStatusItem>>,
    channels_requested: bool,
}

impl Default for EditAgentState {
    fn default() -> Self {
        Self {
            target_id: None,
            active_tab: EditTab::Brain, // canvas's own default-selected category
            detail: Loadable::Loading,
            detail_loaded_for: None,
            contract: Loadable::Loading,
            contract_requested: false,
            channels: Loadable::Loading,
            channels_requested: false,
        }
    }
}

impl gpui::Global for EditAgentState {}

impl EditAgentState {
    /// Sets the target agent, triggering a fresh fetch — mirrors
    /// `AgentsState::select_agent`'s "a different id resets every
    /// Loadable" pattern (re-selecting the same id is a no-op so an
    /// already-loaded detail isn't discarded, e.g. re-clicking 編輯 from
    /// the same agent's detail page).
    pub fn select_agent(&mut self, id: String) {
        if self.target_id.as_deref() == Some(id.as_str()) {
            return;
        }
        self.target_id = Some(id);
        self.active_tab = EditTab::Brain;
        self.detail = Loadable::Loading;
        self.detail_loaded_for = None;
        self.contract = Loadable::Loading;
        self.contract_requested = false;
        self.channels = Loadable::Loading;
        self.channels_requested = false;
    }

    pub fn select_tab(&mut self, tab: EditTab) {
        self.active_tab = tab;
    }
}

// ── Fetch orchestration (mirrors `agents.rs`'s `maybe_fetch_*` shape) ────

fn maybe_fetch_detail(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<EditAgentState>();
    let Some(agent_id) = g.target_id.clone() else { return };
    if g.detail_loaded_for.as_deref() == Some(agent_id.as_str()) {
        return;
    }
    cx.global_mut::<EditAgentState>().detail_loaded_for = Some(agent_id.clone());
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "agents.inspect", json!({"agent_id": agent_id}), |cx, result| {
        cx.default_global::<EditAgentState>().detail = result.map(|v| parse_edit_agent_detail(&v)).map(|opt| opt.unwrap_or_default()).into();
    });
}

/// `contract.get` — only fired once `active_tab == Tools`, latched by
/// `contract_requested` (see this file's header comment for why this is
/// lazy rather than bundled into `maybe_fetch_detail`).
fn maybe_fetch_contract(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<EditAgentState>();
    if g.active_tab != EditTab::Tools || g.contract_requested {
        return;
    }
    let Some(agent_id) = g.target_id.clone() else { return };
    cx.global_mut::<EditAgentState>().contract_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "contract.get", json!({"agent_id": agent_id}), |cx, result| {
        cx.default_global::<EditAgentState>().contract = result.map(|v| parse_contract(&v)).into();
    });
}

/// `channels.status` — only fired once `active_tab == Integration`, latched
/// by `channels_requested`. Admin-gated; a `Rejected` outcome latches
/// `Loadable::Failed` and is rendered as an honest "未提供", never retried
/// in a loop — same contract `agents.rs::maybe_fetch_channels` establishes.
fn maybe_fetch_channels(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<EditAgentState>();
    if g.active_tab != EditTab::Integration || g.channels_requested {
        return;
    }
    cx.global_mut::<EditAgentState>().channels_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "channels.status", json!({}), |cx, result| {
        cx.default_global::<EditAgentState>().channels = result.map(|v| parse_channel_statuses(&v)).into();
    });
}

// ── Shared "boxed-list" primitives — local copies of `agents_detail.rs`'s
// own `kv_row`/`boxed_group`/`meta_label`/`plain_value`/`bool_badge`
// recipe (same "local copy over widened visibility" precedent that file's
// own header comment documents for its S4a design-gallery sibling), `pub
// (super)` so `edit_agent_tabs_a.rs`/`edit_agent_tabs_b.rs` — sibling
// modules within `crate::screens`, not children of this one — can reach
// them. `kv_row_desc` is new: the canvas's own 大腦 example pairs several
// rows with a small muted description line under the label (主要模型 /
// "日常對話與任務執行使用的模型"), which `agents_detail.rs`'s label-only
// `kv_row` has no slot for. ─────────────────────────────────────────────

pub(super) fn meta_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

pub(super) fn kv_row(label: impl Into<SharedString>, value: impl IntoElement) -> Div {
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

pub(super) fn kv_row_desc(label: impl Into<SharedString>, desc: impl Into<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .min_h(px(46.))
        .px_3p5()
        .py_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.75)).child(desc.into())),
        )
        .child(value)
}

pub(super) fn boxed_group(rows: Vec<Div>) -> Div {
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

pub(super) fn plain_value(text: SharedString) -> Div {
    div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(text)
}

pub(super) fn bool_badge(locale: i18n::Locale, on: bool) -> Div {
    if on {
        badge(i18n::t(locale, "native.agents.detail.capOn"), BadgeVariant::Success)
    } else {
        badge(i18n::t(locale, "native.agents.detail.capOff"), BadgeVariant::Outline)
    }
}

/// `meta_label(title)` above a body element, `gap_1p5` between — the
/// section-header + boxed-list pairing every tab in `edit_agent_tabs_a.rs`/
/// `edit_agent_tabs_b.rs` repeats (matches the canvas's own 大腦 example:
/// "模型" label above its boxed group, "執行引擎" above its own, ...).
pub(super) fn section(title: impl Into<SharedString>, body: impl IntoElement) -> Div {
    div().flex().flex_col().gap_1p5().child(meta_label(title)).child(body)
}

// ── Breadcrumb + header ───────────────────────────────────────────────

fn breadcrumb(locale: i18n::Locale, display_name: &str, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("edit-agent-breadcrumb-overview")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.agents.title"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "agents";
                    cx.global_mut::<AgentsState>().back_to_list();
                    cx.notify();
                })),
        )
        .child(div().child("›"))
        .child(
            div()
                .id("edit-agent-breadcrumb-agent")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(SharedString::from(display_name.to_string()))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    // `AgentsState::view` already stays `Detail` for the
                    // whole EditAgent visit (nothing on this page ever
                    // calls `back_to_list`), so simply switching pages
                    // lands back on the same agent's p12 detail canvas.
                    this.active_page = "agents";
                    cx.notify();
                })),
        )
        .child(div().child("›"))
        .child(
            div()
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(i18n::t(locale, "native.editAgent.breadcrumbEdit")),
        )
}

fn saved_pill(locale: i18n::Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::SUCCESS, 1.0))
        .child("✓")
        .child(i18n::t(locale, "native.editAgent.saved"))
}

fn header_row(locale: i18n::Locale, detail: &EditAgentDetail, cx: &mut Context<RootView>) -> Div {
    let title = i18n::t1(locale, "native.editAgent.title", "name", &detail.display_name);
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(avatar_with_status(&detail.id, &detail.display_name, detail.icon.as_deref(), &detail.status, px(34.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(px(17.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(i18n::t(locale, "native.editAgent.subtitle")),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(saved_pill(locale))
                .child(button(
                    "edit-agent-back",
                    i18n::t(locale, "native.editAgent.back"),
                    ButtonVariant::Secondary,
                    false,
                    None,
                    cx.listener(|this, _ev, _window, cx| {
                        this.active_page = "agents";
                        cx.notify();
                    }),
                )),
        )
}

// ── Left rail ──────────────────────────────────────────────────────────

fn rail_group_label(text: SharedString) -> Div {
    div()
        .px_2()
        .pt_2()
        .pb_1()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
        .child(text)
}

fn rail_item(tab: EditTab, active: EditTab, locale: i18n::Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let selected = tab == active;
    let label = i18n::t(locale, tab.label_key());
    let mut el = div()
        .id(tab.rail_id())
        .h(px(30.))
        .px_2()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .child(label);
    el = if selected {
        el.bg(theme::alpha(theme::BRAND, 0.10)).text_color(theme::alpha(theme::BRAND, 1.0)).font_weight(gpui::FontWeight::SEMIBOLD)
    } else {
        el.text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).hover(|s| s.bg(theme::alpha(theme::MUTED, 0.5)))
    };
    el.on_click(cx.listener(move |_this, _ev, _window, cx| {
        cx.global_mut::<EditAgentState>().select_tab(tab);
        cx.notify();
    }))
}

fn rail(locale: i18n::Locale, active: EditTab, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("edit-agent-rail")
        .w(px(176.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_2()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(rail_group_label(i18n::t(locale, "native.editAgent.groupCapability")))
        .child(rail_item(EditTab::Skills, active, locale, cx))
        .child(rail_item(EditTab::Tools, active, locale, cx))
        .child(rail_item(EditTab::Integration, active, locale, cx))
        .child(rail_group_label(i18n::t(locale, "native.editAgent.groupSettings")))
        .child(rail_item(EditTab::General, active, locale, cx))
        .child(rail_item(EditTab::Brain, active, locale, cx))
        .child(rail_item(EditTab::Budget, active, locale, cx))
        .child(rail_item(EditTab::Automation, active, locale, cx))
        .child(rail_item(EditTab::Advanced, active, locale, cx))
}

// ── Entry point ────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch_detail(state, cx);
    maybe_fetch_contract(state, cx);
    maybe_fetch_channels(state, cx);

    let locale = state.locale;
    let outer = div().id("edit-agent-page").size_full().overflow_y_scroll().flex().flex_col().items_center().p_4();

    let target_id = cx.default_global::<EditAgentState>().target_id.clone();
    if target_id.is_none() {
        // No agent selected yet — reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_
        // PAGE=editAgent` with no prior `agents_detail.rs` "編輯" click, or
        // (in principle) a stale global. Honest empty state, not a panic.
        return outer.child(div().w(px(CONTENT_WIDTH)).pt_16().child(empty_state(
            "🐾",
            i18n::t(locale, "native.editAgent.noAgentSelected"),
            None,
            None::<Div>,
        )));
    }

    let g = cx.default_global::<EditAgentState>();
    let active_tab = g.active_tab;
    let detail = match &g.detail {
        Loadable::Ready(d) => d.clone(),
        Loadable::Loading => {
            return outer.child(
                div()
                    .w(px(CONTENT_WIDTH))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .pt_8()
                    .child(skeleton(px(CONTENT_WIDTH), px(64.)))
                    .child(skeleton(px(CONTENT_WIDTH), px(320.))),
            );
        }
        Loadable::Failed(msg) => {
            return outer.child(
                div().w(px(CONTENT_WIDTH)).pt_16().child(empty_state(
                    "⚠️",
                    i18n::t1(locale, "native.home.card.errorPrefix", "message", msg),
                    None,
                    None::<Div>,
                )),
            );
        }
    };
    let contract = g.contract.clone();
    let channels = g.channels.clone();

    let body: Div = match active_tab {
        EditTab::Skills => edit_agent_tabs_a::skills_tab(locale, &detail),
        EditTab::Tools => edit_agent_tabs_a::tools_tab(locale, &detail, &contract),
        EditTab::Integration => edit_agent_tabs_a::integration_tab(locale, &detail, &channels),
        EditTab::General => edit_agent_tabs_a::general_tab(locale, &detail),
        EditTab::Brain => edit_agent_tabs_b::brain_tab(locale, &detail),
        EditTab::Budget => edit_agent_tabs_b::budget_tab(locale, &detail),
        EditTab::Automation => edit_agent_tabs_b::automation_tab(locale, &detail),
        EditTab::Advanced => edit_agent_tabs_b::advanced_tab(locale, &detail),
    };

    outer.child(
        div()
            .w(px(CONTENT_WIDTH))
            .flex()
            .flex_col()
            .gap_4()
            .pb_8()
            .child(breadcrumb(locale, &detail.display_name, cx))
            .child(header_row(locale, &detail, cx))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_5()
                    .child(rail(locale, active_tab, cx))
                    // Each tab body already returns a self-contained
                    // `.flex().flex_col().gap_3p5()` stack of sections (see
                    // `edit_agent_tabs_a.rs`/`edit_agent_tabs_b.rs`) — this
                    // wrapper only supplies the `flex_1`/`min_w_0` sizing.
                    .child(div().flex_1().min_w_0().child(body)),
            ),
    )
}
