// S4b third wave — Screen "AI 員工" (p11 list / p12 detail, `nav.rs` id
// `agents`, already listed under the AI 員工 area's Column-2 page list —
// this pass wires that existing id to a real page instead of `shell.rs`'s
// generic placeholder).
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Agents.dc.html`
// (p11, master-detail: avatar+status-dot+two-line rows on the left, a mini
// profile + 3 stat cards + recent-activity shelf on the right — NOT a card
// grid) and `AgentDetail.dc.html` (p12: breadcrumb → hero header → boxed-
// list groups for 模型與 Runtime / 能力 / 通道 → an activity shelf). Both
// pages live inside ONE `nav.rs` page id — see `AgentsView` below for how
// "開啟完整檔案" switches between them without a second nav entry.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed — see `agents_data.rs`'s own header comment
// for the full field-by-field citation) ──────────────────────────────────
//   `agents.list {}`, `agents.inspect {"agent_id"}`, `tasks.list
//   {"agent_id"}`, `activity.list {"agent_id","limit"}`, `channels.status
//   {}` (admin-only — a non-admin session degrades to an honest "未提供"
//   channels box, never a blocked page).
//
// ── Why state lives in a `gpui::Global`, not a `RootView` field ─────────
// Same constraint `goals.rs`'s own module doc comment documents in detail:
// the task brief forbids touching `main.rs` this pass (parallel waves own
// its wiring), so `AgentsState` is a `gpui::Global` — zero `RootView`
// changes needed, `Context<RootView>` derefs to `&mut App` which carries
// `global`/`global_mut`/`default_global`.

use chrono::Local;
use gpui::{Context, Global, Stateful, Div};
use serde_json::json;

use crate::screens::agents_data::{
    agent_task_stats, parse_agent_detail, parse_agents_list, parse_channel_statuses, AgentDetailData,
    AgentListItem, AgentTaskStats, ChannelStatusItem,
};
use crate::screens::dashboard::{ActivityItem, Loadable};
use crate::screens::goals::spawn_goal_call;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// `agents_list`/`agents_summary`/`agents_detail`/`agents_data` are SIBLING
// modules (declared in `screens/mod.rs`, not `mod ...;` here) — same
// file-layout precedent `goals.rs`'s own module doc comment establishes for
// `goals_data`/`goals_inspector`: a `foo.rs` + `foo_bar.rs` pair living
// directly under `screens/`, avoiding the `foo/bar.rs` nested-directory
// shape Rust's 2018+ module resolution would otherwise require for a `mod
// bar;` declared INSIDE `foo.rs`.
use crate::screens::{agents_detail, agents_list};

/// Which of the two visual-authority canvases this page currently shows —
/// both render from the SAME `nav.rs` "agents" id (see this file's module
/// doc comment); this is purely an internal view state, not a second route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsView {
    List,
    Detail,
}

// ── Global state ───────────────────────────────────────────────────────

pub struct AgentsState {
    list_requested: bool,
    pub list: Loadable<Vec<AgentListItem>>,
    pub selected_id: Option<String>,
    pub view: AgentsView,

    /// Task-derived stats (今日完成/進行中/等你決定) for `selected_id` —
    /// shared by the list-mode mini summary AND the detail-mode hero stat
    /// row (both canvases show the same 3 numbers). Latched per agent id,
    /// same shape `goals.rs::GoalsState::iterations_loaded_for` documents.
    pub stats: Loadable<AgentTaskStats>,
    stats_loaded_for: Option<String>,

    pub activity: Loadable<Vec<ActivityItem>>,
    activity_loaded_for: Option<String>,

    /// Full `agents.inspect` payload — only needed once `view ==
    /// AgentsView::Detail`, so `maybe_fetch_detail` gates on that.
    pub detail: Loadable<AgentDetailData>,
    detail_loaded_for: Option<String>,

    /// Global (not per-agent) — `channels.status` takes no params and
    /// returns every configured channel across every agent; this page
    /// filters client-side per selected agent (`agents_data::
    /// channels_for_agent`). Fetched at most once (admin-gated; a
    /// permission failure latches `Loadable::Failed` rather than retrying
    /// forever — see `maybe_fetch_channels`).
    channels_requested: bool,
    pub channels: Loadable<Vec<ChannelStatusItem>>,
}

impl Default for AgentsState {
    fn default() -> Self {
        Self {
            list_requested: false,
            list: Loadable::Loading,
            selected_id: None,
            view: AgentsView::List,
            stats: Loadable::Loading,
            stats_loaded_for: None,
            activity: Loadable::Loading,
            activity_loaded_for: None,
            detail: Loadable::Loading,
            detail_loaded_for: None,
            channels_requested: false,
            channels: Loadable::Loading,
        }
    }
}

impl Global for AgentsState {}

impl AgentsState {
    /// Row click in the master list (List-mode only — Detail mode has no
    /// row list, only a breadcrumb back). Re-selecting the same id is a
    /// no-op so an already-loaded stats/activity pair isn't discarded.
    pub fn select_agent(&mut self, id: String) {
        if self.selected_id.as_deref() == Some(id.as_str()) {
            return;
        }
        self.selected_id = Some(id);
        self.stats = Loadable::Loading;
        self.stats_loaded_for = None;
        self.activity = Loadable::Loading;
        self.activity_loaded_for = None;
        self.detail = Loadable::Loading;
        self.detail_loaded_for = None;
    }

    /// "開啟完整檔案（模型、能力、通道）" — switches to the p12 canvas for
    /// whichever agent is currently selected.
    pub fn open_detail(&mut self) {
        self.view = AgentsView::Detail;
    }

    /// Breadcrumb "AI 員工" click in Detail mode — back to the p11 canvas,
    /// selection preserved (matches OmniFocus/Finder's own "going back
    /// keeps your place" convention `goals.rs` also follows for its own
    /// list⇄inspector relationship).
    pub fn back_to_list(&mut self) {
        self.view = AgentsView::List;
    }
}

// ── Fetch orchestration (mirrors `goals.rs`'s `spawn_goal_call` shape —
// reused directly, not duplicated: it's `pub(super)` inside `goals.rs`,
// i.e. `pub(in crate::screens)`, and this module lives inside `crate::
// screens` too) ───────────────────────────────────────────────────────

fn maybe_fetch_list(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<AgentsState>().list_requested {
        return;
    }
    cx.global_mut::<AgentsState>().list_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "agents.list", json!({}), |cx, result| {
        cx.default_global::<AgentsState>().list = result.map(|v| parse_agents_list(&v)).into();
    });
}

/// If nothing is selected yet, auto-select the first loaded agent —
/// matches `goals.rs::maybe_autoselect`'s "opening a page focuses its
/// first item" convention.
fn maybe_autoselect(cx: &mut Context<RootView>) {
    let g = cx.default_global::<AgentsState>();
    if g.selected_id.is_some() {
        return;
    }
    let Loadable::Ready(agents) = &g.list else { return };
    let Some(first) = agents.first() else { return };
    let id = first.id.clone();
    cx.global_mut::<AgentsState>().select_agent(id);
}

/// Re-fetches `tasks.list`/`activity.list` for `selected_id` whenever it
/// changes — feeds both the list-mode mini summary and the detail-mode
/// hero stat row + activity shelf.
fn maybe_fetch_side_data(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<AgentsState>();
    let Some(agent_id) = g.selected_id.clone() else { return };

    if g.stats_loaded_for.as_deref() != Some(agent_id.as_str()) {
        cx.global_mut::<AgentsState>().stats_loaded_for = Some(agent_id.clone());
        let tx = state.session_tx.clone();
        let id_for_call = agent_id.clone();
        spawn_goal_call(cx, tx, "tasks.list", json!({"agent_id": id_for_call}), |cx, result| {
            let today = Local::now().date_naive();
            cx.default_global::<AgentsState>().stats = result.map(|v| agent_task_stats(&v, today)).into();
        });
    }

    let g = cx.default_global::<AgentsState>();
    if g.activity_loaded_for.as_deref() != Some(agent_id.as_str()) {
        cx.global_mut::<AgentsState>().activity_loaded_for = Some(agent_id.clone());
        let tx = state.session_tx.clone();
        let locale = state.locale;
        spawn_goal_call(cx, tx, "activity.list", json!({"agent_id": agent_id, "limit": 6}), move |cx, result| {
            let now = chrono::Utc::now();
            cx.default_global::<AgentsState>().activity = result.map(|v| parse_agent_activity(&v, locale, now)).into();
        });
    }
}

fn maybe_fetch_detail(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<AgentsState>();
    if g.view != AgentsView::Detail {
        return;
    }
    let Some(agent_id) = g.selected_id.clone() else { return };
    if g.detail_loaded_for.as_deref() == Some(agent_id.as_str()) {
        return;
    }
    cx.global_mut::<AgentsState>().detail_loaded_for = Some(agent_id.clone());
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "agents.inspect", json!({"agent_id": agent_id}), |cx, result| {
        cx.default_global::<AgentsState>().detail = result.map(|v| parse_agent_detail(&v)).map(|opt| opt.unwrap_or_default()).into();
    });
}

/// `channels.status` is global (not scoped to one agent) and admin-only —
/// fetched at most once per app run, lazily on first Detail-view render.
/// A `Rejected` (non-admin) outcome latches `Loadable::Failed` and is
/// rendered as an honest "未提供" by `agents_detail`'s channels box, never
/// retried in a loop.
fn maybe_fetch_channels(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.default_global::<AgentsState>();
    if g.view != AgentsView::Detail || g.channels_requested {
        return;
    }
    cx.global_mut::<AgentsState>().channels_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "channels.status", json!({}), |cx, result| {
        cx.default_global::<AgentsState>().channels = result.map(|v| parse_channel_statuses(&v)).into();
    });
}

/// `activity.list` rows → `dashboard::ActivityItem`, reusing that existing
/// `pub` type (already shaped exactly `{agent, summary, when}`) rather than
/// inventing a near-duplicate. `when` uses `goals::relative_time` (the
/// "N 分鐘前" style already established there) instead of `dashboard.rs`'s
/// own private `short_time` (HH:MM-only, not reusable across modules).
fn parse_agent_activity(v: &serde_json::Value, locale: crate::i18n::Locale, now: chrono::DateTime<chrono::Utc>) -> Vec<ActivityItem> {
    v.get("events")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|e| {
                    let ts = e.get("timestamp").and_then(serde_json::Value::as_str).unwrap_or("");
                    ActivityItem {
                        agent: e.get("agent_id").and_then(serde_json::Value::as_str).unwrap_or("").to_string(),
                        summary: e.get("summary").and_then(serde_json::Value::as_str).unwrap_or("").to_string(),
                        when: super::goals::relative_time(locale, ts, now).to_string(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Shared labels (used by `agents_list`/`agents_summary`/`agents_detail`,
// all siblings within `screens` — same `pub(super)` visibility shape
// `goals.rs::status_label`/`relative_time` already establish) ───────────

/// `role` is the raw `format!("{:?}", AgentRole).to_lowercase()` wire token
/// (see `agents_data.rs`'s header comment) — every `AgentRole` variant is
/// covered so a legitimately new role never silently falls through; an
/// unrecognized string (future role added server-side before this client
/// catches up) degrades to the raw token itself, never a blank label.
pub(super) fn role_label(locale: crate::i18n::Locale, role: &str) -> gpui::SharedString {
    let key = match role {
        "main" => "native.agents.role.main",
        "specialist" => "native.agents.role.specialist",
        "worker" => "native.agents.role.worker",
        "developer" => "native.agents.role.developer",
        "qa" => "native.agents.role.qa",
        "planner" => "native.agents.role.planner",
        "teamleader" => "native.agents.role.teamLeader",
        "productmanager" => "native.agents.role.productManager",
        other => return other.to_string().into(),
    };
    crate::i18n::t(locale, key)
}

pub(super) fn status_label(locale: crate::i18n::Locale, status: &str) -> gpui::SharedString {
    let key = match status {
        "active" => "native.agents.status.active",
        "paused" => "native.agents.status.paused",
        "terminated" => "native.agents.status.terminated",
        other => return other.to_string().into(),
    };
    crate::i18n::t(locale, key)
}

/// Small filled status-dot hue for the avatar overlay (`avatar_with_status`
/// below) — active=brand, paused=warning, terminated=destructive.
pub(super) fn status_dot_color(status: &str) -> u32 {
    match status {
        "paused" => theme::WARNING,
        "terminated" => theme::DESTRUCTIVE,
        _ => theme::BRAND, // active, and any unrecognized status
    }
}

/// First character of a display name (CJK-safe — codepoint, never a byte
/// slice), matching `chat/agents_picker.rs::avatar_glyph`'s established
/// convention. That function is private to its own module, so this is a
/// second, deliberately identical, copy — same "local copy over widened
/// visibility" precedent `goals_inspector.rs::goals_status_dot`'s own doc
/// comment already sets for this crate. Falls back to the agent's own
/// emoji `icon` when set server-side.
pub(super) fn avatar_glyph(display_name: &str, icon: Option<&str>) -> String {
    if let Some(icon) = icon {
        return icon.to_string();
    }
    display_name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "?".to_string())
}

/// Deterministic (not random) color per agent id, so the same agent always
/// gets the same avatar color across renders/sessions — picked from a small
/// curated MDS palette. Explicitly NOT a real backend identity color (no
/// such field exists on `agents.list`/`agents.inspect`); this is a second,
/// deliberately identical copy of `chat/agents_picker::avatar_color`'s own
/// id-hash recipe — that function lives in a private (`mod agents_picker;`,
/// no `pub`) sibling module of `chat.rs`, unreachable from here without
/// widening its visibility, so this page duplicates the small hash instead
/// (same "local copy over widened visibility" precedent as `avatar_glyph`
/// above).
pub(super) fn avatar_color(id: &str) -> u32 {
    const PALETTE: [u32; 4] = [theme::BRAND, theme::SUCCESS, theme::INFO, theme::WARNING];
    let hash = id.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[hash as usize % PALETTE.len()]
}

/// Circular avatar + bottom-right status-dot overlay — the p11/p12 canvas's
/// own avatar recipe (`commercial/design/duduclaw-s4a-pages/Agents.dc.html`'s
/// `position: relative` avatar + `position: absolute; right: -1px; bottom:
/// -1px` dot, ported to gpui's `.relative()`/`.absolute()` — same technique
/// `screens/prototypes/p11_agents.rs::employee_row` already uses for its own
/// static sketch of this same page).
pub(super) fn avatar_with_status(
    id: &str,
    display_name: &str,
    icon: Option<&str>,
    status: &str,
    diameter: gpui::Pixels,
) -> gpui::Div {
    use gpui::{div, prelude::*, px};
    let color = avatar_color(id);
    let glyph = avatar_glyph(display_name, icon);
    let dot_size = (f32::from(diameter) * 0.3).max(10.0);
    div()
        .relative()
        .flex_shrink_0()
        .child(
            div()
                .size(diameter)
                .rounded_full()
                .bg(theme::alpha(color, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px((f32::from(diameter) * 0.4).max(10.0)))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .child(glyph),
        )
        .child(
            div()
                .absolute()
                .bottom(px(-1.))
                .right(px(-1.))
                .size(px(dot_size))
                .rounded_full()
                .border_2()
                .border_color(theme::alpha(theme::SURFACE, 1.0))
                .bg(theme::alpha(status_dot_color(status), 1.0)),
        )
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch_list(state, cx);
    maybe_autoselect(cx);
    maybe_fetch_side_data(state, cx);
    maybe_fetch_detail(state, cx);
    maybe_fetch_channels(state, cx);

    match cx.default_global::<AgentsState>().view {
        AgentsView::List => agents_list::render(state, cx),
        AgentsView::Detail => agents_detail::render(state, cx),
    }
}
