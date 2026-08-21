// WP-S6b2-N (S6b 第二波, 2026-08-21) — "新增員工" (CreateAgent) page, reached
// from the "AI 員工" list's "新員工" button (`agents_list.rs`'s own header
// row — this pass flips that button from an honest disabled stub to a real
// `active_page = "createAgent"` navigation, see that file's own updated
// comment). No `nav.rs` entry of its own — same "D 先掛好分支就直接可達,未掛
// 就自己掛" precedent every prior wave's `shell.rs` comment block already
// establishes for a leaf page reached only via another page's own click, not
// the sidebar.
//
// Visual authority: `commercial/design/duduclaw-s6-form-pages/CreateAgent.dc.
// html` — ONE scrolling page (not a wizard): header (icon/title/subtitle +
// 取消/新增員工) → a 2-column template-picker card grid (空白開始 + role
// cards; a `created: true` role renders dimmed+disabled with a "已建立"
// badge; the selected card gets a blue ring) → boxed-list 基本資料 (識別碼/
// 顯示名稱[+已自訂 badge]/觸發詞) → boxed-list 組織位置 (直屬上級/部門) →
// a read-only SOUL.md preview block → a "進階：contract.toml／agent.toml
// 覆寫" disclosure row.
//
// This file holds `CreateAgentState` (the `gpui::Global`) + fetch
// orchestration + the top-level `render` entry point. All section-level
// rendering (template grid / basics / org / SOUL preview / advanced
// disclosure, plus their local `kv_row`/`boxed_group` primitives) lives in
// the sibling `create_agent_sections.rs` — split out purely to keep this
// file under this crate's own <800-line convention (the unsplit version ran
// to ~890 lines); see that file's own header comment.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ─────────────────────────────────────────────────────
//   `templates.roster {}` / `{"industry"?}` — dispatch handlers.rs:5481,
//   handler `handle_templates_roster` at handlers.rs:8017. Requires
//   `premium_dir_unlocked()` server-side; fails outright on a non-premium
//   install. TS mirror `TemplateRoster`, `web/src/lib/api.ts:4066`. Full
//   field mapping + honest-degrade rule in `create_agent_data.rs`'s own
//   header comment.
//   `templates.role {"role_id","industry"?}` — dispatch handlers.rs:5485,
//   handler `handle_templates_role` at handlers.rs:8070. TS mirror
//   `TemplateRoleDetail`, api.ts:4079.
//   `templates.create_agent {role_id,industry?,name?,display_name?,
//   trigger?,reports_to?,department?,soul_md?,contract_toml?,agent_toml?}`
//   — dispatch handlers.rs:5489, handler `handle_templates_create_agent` at
//   handlers.rs:8114 (`require_admin!()`). TS mirror
//   `TemplateCreateAgentResult`, api.ts:4144.
//   `agents.create {name,display_name,role?,trigger?,model_preferred?,
//   reports_to?,department?}` — dispatch handlers.rs:5412, handler
//   `handle_agents_create` at handlers.rs:7525 (`require_admin!()`).
//   Defaults `role` to `"specialist"`, `trigger` to `"@{display_name}"` when
//   empty, `model_preferred` to `"claude-sonnet-4-6"` when empty/absent;
//   validates `name` via `is_valid_agent_id`.
//   `agents.list {}` — dispatch handlers.rs:5396, handler
//   `handle_agents_list_filtered`. Reused via `agents_data::
//   parse_agents_list` (this page's own independent RPC call + Global
//   field — this crate's established "duplicate the call, don't couple two
//   pages' Global state" convention, see `agents.rs`'s own header comment)
//   for the 直屬上級 picker.
//   `departments.list {}` — dispatch handlers.rs:6379, handler
//   `handle_departments_list` at handlers.rs:26379 (`require_manager!()`).
//   TS mirror `DepartmentInfo`, api.ts:4136 — only `.name` is read.
//
// ── Assembled-not-wired boundary (this crate's established "decision-class
// actions are assembled, not clicked" convention — see `mail.rs`'s own
// header comment for the canonical wording) ───────────────────────────────
// "新增員工" (submit) is the ONLY assembled-not-wired control on this page.
// It renders as a real-looking enabled primary button (never `disabled:
// true` — the canvas draws it fully enabled) but its `on_click` listener is
// empty (`cx.listener(|_, _, _, _| {})`) — see that button's own doc comment
// (in `create_agent_sections.rs::header_row`) for exactly which RPC it would
// call and why creating a real AI employee (an admin-gated write with
// lasting side effects: a new agent directory, org.toml, an installed
// file-guard hook) is left off this pass, same reasoning `mail.rs`'s own
// unwired "確認寄出" cites. Every other interaction on this page is real:
// template card selection fires `templates.role` and repopulates the form;
// the three basics TextFields are live-typeable; 直屬上級/部門 selection is
// real local state; 取消 really navigates back to `active_page = "agents"`;
// the 進階 disclosure really toggles.

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, Stateful};
use serde_json::json;

use crate::i18n::{self, Locale};
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::create_agent_data::{self, DepartmentItem, TemplateRoleDetail, TemplateRosterData};
use crate::screens::create_agent_sections;
use crate::screens::dashboard::Loadable;
use crate::screens::goals::spawn_goal_call;
use crate::text_field::TextField;
use crate::ws_status::WsConnState;
use crate::RootView;

const CONTENT_WIDTH: f32 = 680.0;

// ── Global state ───────────────────────────────────────────────────────

/// Holds three `Entity<TextField>` fields, so — same reason `IdentityState`
/// (`identity.rs`) isn't `#[derive(Default)]` — this needs a real `cx: &mut
/// App` at construction time. `ensure_state` below is the lazy-init
/// entry point every render call goes through first, same shape
/// `identity.rs::ensure_state` establishes.
pub struct CreateAgentState {
    roster_requested: bool,
    /// `Loadable::Ready(None)` means "no templates available (RPC failed,
    /// or an honest-but-foreign payload) — fall back to the plain blank
    /// form", never `Loadable::Failed` — see `create_agent_data::
    /// parse_template_roster`'s own doc comment for why an RPC error and a
    /// malformed 200 both collapse to the same silent `None`.
    pub roster: Loadable<Option<TemplateRosterData>>,

    /// `None` = "空白開始". `Some(role_id)` drives `maybe_fetch_role_detail`.
    pub selected_role_id: Option<String>,
    pub role_detail: Loadable<TemplateRoleDetail>,
    role_detail_loaded_for: Option<String>,
    /// The template's own `display_name` at the moment it was applied to
    /// `display_name_field` — the comparison baseline `sync_display_name_
    /// touched` uses to detect a live edit. `None` until a template's
    /// `templates.role` response actually lands (deliberately NOT set the
    /// instant a card is clicked — see that function's own doc comment for
    /// why comparing against a not-yet-loaded baseline would misfire).
    pub template_display_name: Option<String>,
    /// Sticky once true (matches the task brief's own "flip on first user
    /// edit" instruction literally, not a live re-comparison every render —
    /// reverting the text back to the template's own value does NOT clear
    /// the "已自訂" badge, same "once a fact is recorded it doesn't silently
    /// un-happen" posture this crate's memory/audit layers apply elsewhere).
    pub display_name_touched: bool,

    pub id_field: Entity<TextField>,
    pub display_name_field: Entity<TextField>,
    pub trigger_field: Entity<TextField>,

    agents_requested: bool,
    pub agents_list: Loadable<Vec<AgentListItem>>,
    departments_requested: bool,
    pub departments: Loadable<Vec<DepartmentItem>>,

    /// Chosen agent id, or `None` to keep the template's own wiring / no
    /// supervisor on the blank path. Local UI state only — no RPC fires on
    /// selection, matching the web's own "only sent at submit time" shape.
    pub reports_to: Option<String>,
    pub department: Option<String>,

    pub advanced_open: bool,
}

impl CreateAgentState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            roster_requested: false,
            roster: Loadable::Loading,
            selected_role_id: None,
            role_detail: Loadable::Loading,
            role_detail_loaded_for: None,
            template_display_name: None,
            display_name_touched: false,
            // Placeholder text is baked in at construction time in
            // `Locale::ZhTw` — this Global is lazily created once (`ensure_
            // state`) before any per-render `locale` is available to it,
            // same "hardcode the construction-time locale" precedent
            // `runs.rs`'s own `search: TextField::new(cx, i18n::t(Locale::
            // ZhTw, "runs.search.placeholder"), false, "")` already
            // establishes for this exact constraint.
            id_field: TextField::new(cx, i18n::t(Locale::ZhTw, "createAgent.basics.idPlaceholder"), false, ""),
            display_name_field: TextField::new(cx, i18n::t(Locale::ZhTw, "createAgent.basics.displayNamePlaceholder"), false, ""),
            trigger_field: TextField::new(cx, i18n::t(Locale::ZhTw, "createAgent.basics.triggerPlaceholder"), false, ""),
            agents_requested: false,
            agents_list: Loadable::Loading,
            departments_requested: false,
            departments: Loadable::Loading,
            reports_to: None,
            department: None,
            advanced_open: false,
        }
    }
}

impl Global for CreateAgentState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<CreateAgentState>() {
        let state = CreateAgentState::new(cx);
        cx.set_global(state);
    }
}

/// Template card click (or "空白開始", `role_id: None`). Re-selecting the
/// current selection is a no-op. Selecting blank clears the three fields
/// immediately (there is no `templates.role` response coming to populate
/// them); selecting a template leaves the fields as-is until
/// `maybe_fetch_role_detail` below applies the fresh response — never
/// stale-populates from a previous role's leftover values. `pub(super)`:
/// called from `create_agent_sections.rs::template_card`'s own on_click.
pub(super) fn select_role(cx: &mut Context<RootView>, role_id: Option<String>) {
    let (id_field, display_name_field, trigger_field, unchanged) = {
        let g = cx.global::<CreateAgentState>();
        (g.id_field.clone(), g.display_name_field.clone(), g.trigger_field.clone(), g.selected_role_id == role_id)
    };
    if unchanged {
        return;
    }
    {
        let g = cx.global_mut::<CreateAgentState>();
        g.selected_role_id = role_id.clone();
        g.role_detail = Loadable::Loading;
        g.role_detail_loaded_for = None;
        g.template_display_name = None;
        g.display_name_touched = false;
        g.reports_to = None;
        g.department = None;
        g.advanced_open = false;
    }
    if role_id.is_none() {
        id_field.update(cx, |f, cx| {
            f.content.clear();
            cx.notify();
        });
        display_name_field.update(cx, |f, cx| {
            f.content.clear();
            cx.notify();
        });
        trigger_field.update(cx, |f, cx| {
            f.content.clear();
            cx.notify();
        });
    }
}

/// Compares `display_name_field`'s live content against the template's own
/// baseline once (and only once) that baseline is known — running this
/// comparison before `template_display_name` is populated (the window
/// between a card click and its `templates.role` response landing) would
/// compare live content against `None` and misfire a false "已自訂" the
/// instant any template is picked, before the user has typed anything.
fn sync_display_name_touched(cx: &mut Context<RootView>) {
    let g = cx.global::<CreateAgentState>();
    if g.display_name_touched {
        return;
    }
    let Some(baseline) = g.template_display_name.clone() else { return };
    let field = g.display_name_field.clone();
    let current = field.read(cx).content.clone();
    if current != baseline {
        cx.global_mut::<CreateAgentState>().display_name_touched = true;
    }
}

/// `pub(super)`: called from `create_agent_sections.rs::soul_and_advanced_
/// section`'s disclosure-row on_click.
pub(super) fn toggle_advanced(cx: &mut Context<RootView>) {
    let current = cx.global::<CreateAgentState>().advanced_open;
    cx.global_mut::<CreateAgentState>().advanced_open = !current;
}

// ── Fetch orchestration (mirrors `agents.rs`'s `maybe_fetch_*` shape) ────

fn maybe_fetch_roster(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.global::<CreateAgentState>().roster_requested {
        return;
    }
    cx.global_mut::<CreateAgentState>().roster_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "templates.roster", json!({}), |cx, result| {
        // RPC failure (non-premium install, network) AND a malformed-but-
        // successful payload both collapse to `None` — see this file's
        // header comment for why that's the intended, silent degrade.
        let roster = result.ok().and_then(|v| create_agent_data::parse_template_roster(&v));
        cx.global_mut::<CreateAgentState>().roster = Loadable::Ready(roster);
    });
}

fn maybe_fetch_role_detail(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let g = cx.global::<CreateAgentState>();
    let Some(role_id) = g.selected_role_id.clone() else { return };
    if g.role_detail_loaded_for.as_deref() == Some(role_id.as_str()) {
        return;
    }
    cx.global_mut::<CreateAgentState>().role_detail_loaded_for = Some(role_id.clone());
    let industry = match &cx.global::<CreateAgentState>().roster {
        Loadable::Ready(Some(r)) => r.industry.clone(),
        _ => None,
    };
    let tx = state.session_tx.clone();
    let mut params = json!({ "role_id": role_id });
    if let Some(ind) = industry {
        params["industry"] = json!(ind);
    }
    spawn_goal_call(cx, tx, "templates.role", params, |cx, result| {
        let detail = result.ok().and_then(|v| create_agent_data::parse_template_role_detail(&v));
        match detail {
            Some(d) => {
                let (id_field, display_name_field, trigger_field) = {
                    let g = cx.global::<CreateAgentState>();
                    (g.id_field.clone(), g.display_name_field.clone(), g.trigger_field.clone())
                };
                let id_val = d.name.clone();
                let name_val = d.display_name.clone();
                let trigger_val = d.trigger.clone();
                id_field.update(cx, |f, cx| {
                    f.content = id_val;
                    cx.notify();
                });
                display_name_field.update(cx, |f, cx| {
                    f.content = name_val;
                    cx.notify();
                });
                trigger_field.update(cx, |f, cx| {
                    f.content = trigger_val;
                    cx.notify();
                });
                let g = cx.global_mut::<CreateAgentState>();
                g.template_display_name = Some(d.display_name.clone());
                g.display_name_touched = false;
                g.role_detail = Loadable::Ready(d);
            }
            None => {
                cx.global_mut::<CreateAgentState>().role_detail = Loadable::Failed(i18n::t(Locale::ZhTw, "createAgent.roleDetail.loadError").to_string());
            }
        }
    });
}

fn maybe_fetch_agents(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.global::<CreateAgentState>().agents_requested {
        return;
    }
    cx.global_mut::<CreateAgentState>().agents_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "agents.list", json!({}), |cx, result| {
        cx.global_mut::<CreateAgentState>().agents_list = result.map(|v| agents_data::parse_agents_list(&v)).into();
    });
}

fn maybe_fetch_departments(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.global::<CreateAgentState>().departments_requested {
        return;
    }
    cx.global_mut::<CreateAgentState>().departments_requested = true;
    let tx = state.session_tx.clone();
    spawn_goal_call(cx, tx, "departments.list", json!({}), |cx, result| {
        cx.global_mut::<CreateAgentState>().departments = result.map(|v| create_agent_data::parse_departments(&v)).into();
    });
}

// ── Top-level render ──────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_roster(state, cx);
    maybe_fetch_role_detail(state, cx);
    maybe_fetch_agents(state, cx);
    maybe_fetch_departments(state, cx);
    sync_display_name_touched(cx);

    let locale = state.locale;
    let outer = div().id("create-agent-page").size_full().overflow_y_scroll().flex().flex_col().items_center().p_4();

    let roster = cx.global::<CreateAgentState>().roster.clone();
    let selected_role_id = cx.global::<CreateAgentState>().selected_role_id.clone();

    let template = create_agent_sections::template_section(locale, &roster, &selected_role_id, cx);

    // Snapshot every remaining field this render pass needs as an owned
    // clone BEFORE calling `org_section`/`soul_and_advanced_section` — both
    // also need `cx` itself (for their own `cx.listener` calls), so a live
    // `&CreateAgentState` borrow can't still be held when they're called.
    // See `create_agent_sections.rs::org_section`'s own doc comment for the
    // exact aliasing conflict this avoids.
    let g = cx.global::<CreateAgentState>();
    let basics = create_agent_sections::basics_section(locale, g);
    let role_detail_snapshot = g.role_detail.clone();
    let advanced_open = g.advanced_open;
    let agents_list = g.agents_list.clone();
    let departments = g.departments.clone();
    let reports_to = g.reports_to.clone();
    let department = g.department.clone();

    let template_reports_to = match &role_detail_snapshot {
        Loadable::Ready(d) if selected_role_id.is_some() => d.reports_to.clone(),
        _ => None,
    };

    let org = create_agent_sections::org_section(locale, &agents_list, &departments, &reports_to, &department, template_reports_to, cx);

    let mut body = div()
        .w(px(CONTENT_WIDTH))
        .flex()
        .flex_col()
        .gap_5()
        .pb_8()
        .child(create_agent_sections::breadcrumb(locale, cx))
        .child(create_agent_sections::header_row(locale, cx))
        .child(template)
        .child(basics)
        .child(org);

    if selected_role_id.is_some() {
        body = body.child(create_agent_sections::soul_and_advanced_section(locale, &role_detail_snapshot, advanced_open, cx));
    }

    outer.child(body)
}
