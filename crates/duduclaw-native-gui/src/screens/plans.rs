// WP-S5b2-D (2026-08-21) — 共同計畫 (`Plans.dc.html`). B1 "清單+詳情"
// two-column layout, same processing recipe `routines.rs` uses — see that
// module's doc comment for the cover-sheet legend quote. Reached via
// `nav.rs` id `plans` (`TASKS_ITEMS`, wired by this same pass).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Plans.dc.html`
// — left column: header ("共同計畫 · N 件" + assembled "新增計畫" button) + a
// scrollable list of plan rows (title, relative time, agent tag, progress
// fraction). Right column: the selected plan's title + progress badge +
// description placeholder, a 步驟 card (status circle · text · assignee ·
// reorder chevrons), and a "下一步要做什麼？" input row.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `plans.list` (`handle_plans_list`, ~L30018) → `{ "plans": [ { "id",
//     "title", "description", "agent_id", "goal_id", "status", "created_by",
//     "created_at", "updated_at", "steps_total", "steps_done" } ] }` — the
//     two progress counters ride along per-plan (handler's own comment:
//     "so the list view renders without N follow-up `plans.get` calls").
//     No participant-avatar list is included in this shape (only the owning
//     `agent_id`), so this page renders an agent tag instead of the
//     canvas's avatar stack — see [`PlanSummary`]'s doc comment.
//   `plans.get {"plan_id"}` (`handle_plans_get`, ~L30052) → `{ "plan": {...
//     same shape as above minus steps_total/steps_done }, "steps": [ { "id",
//     "plan_id", "text", "assignee_kind", "assignee", "status"
//     (`task_store.rs` L548: `todo | doing | done | skipped`,
//     `PLAN_STEP_STATUSES` L570 — the authoritative 4-state order this
//     page's local status-cycle reuses verbatim), "step_order",
//     "created_at", "updated_at" } ] }` — fired once per selection change
//     (see `maybe_fetch_detail`), the second read RPC this page calls.
//   `plans.create`/`plans.update`/`plans.remove`/`plans.add_step`/
//     `plans.update_step`/`plans.remove_step` (handlers.rs L30080-30413) are
//     the write-side family the canvas's "新增計畫"/"下一步要做什麼？＋新增步驟"
//     input stand in for. Per this WP's brief ("寫路徑組裝不真按"): the step
//     status circle IS genuinely clickable and cycles — see
//     [`cycle_step_status`] — but the click handler mutates ONLY this page's
//     own in-memory `Loadable<PlanDetail>` copy, never calling
//     `plans.update_step`; a refresh (or reselecting the plan) reverts to
//     the server's real state. The "新增步驟" text field is a real, typeable
//     input (`new_step_text`, same manual `on_key_down` capture
//     `console_detail.rs::goal_note_field` establishes) but its button is a
//     plain inert placeholder like every other write action on this page —
//     no `build_plans_*_params` functions are written since nothing here
//     ever calls out to the network for a write.
//
// ── Why state lives behind `gpui::Global`, not a `RootView` field ────────
// Same reasoning `routines.rs`/`console.rs`/`goals.rs` already document —
// this pass keeps `main.rs` off-limits, so `PlansState` is a `Global`
// singleton lazily installed by `ensure_state`.

use gpui::{div, prelude::*, Context, Div, Global, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n;
use crate::mds_gpui::empty_state;
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

mod plans_detail;
mod plans_rows;

// ── Data model ─────────────────────────────────────────────────────────

/// One `plans.list` row. No participant-avatar list is part of this shape
/// (see this module's header comment) — `agent_id` is the only "who" field
/// available at list time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSummary {
    pub id: String,
    pub title: String,
    pub agent_id: String,
    pub status: String,
    pub updated_at: String,
    pub steps_total: usize,
    pub steps_done: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanMeta {
    pub id: String,
    pub title: String,
    pub description: String,
    pub agent_id: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub id: String,
    pub plan_id: String,
    pub text: String,
    pub assignee_kind: String,
    pub assignee: String,
    /// One of [`PLAN_STEP_STATUSES`] — never assumed to be otherwise, see
    /// [`cycle_step_status`].
    pub status: String,
    pub step_order: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDetail {
    pub plan: PlanMeta,
    pub steps: Vec<PlanStep>,
}

/// `task_store.rs::PLAN_STEP_STATUSES` (L570) — the exact backend-defined
/// cycle order [`cycle_step_status`] advances through locally.
pub const PLAN_STEP_STATUSES: &[&str] = &["todo", "doing", "done", "skipped"];

/// Local, non-persisted status advance — see this module's header comment
/// on why this never calls `plans.update_step`. An unrecognized current
/// status (never emitted by the server, but never assumed impossible either)
/// falls back to the cycle's first entry rather than panicking on an
/// `.unwrap()` of a missing position.
pub fn cycle_step_status(current: &str) -> &'static str {
    let pos = PLAN_STEP_STATUSES.iter().position(|s| *s == current).unwrap_or(0);
    PLAN_STEP_STATUSES[(pos + 1) % PLAN_STEP_STATUSES.len()]
}

pub fn parse_plans(v: &Value) -> Vec<PlanSummary> {
    v.get("plans")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(Value::as_str)?.to_string();
                    let title = p.get("title").and_then(Value::as_str)?.to_string();
                    if id.is_empty() {
                        return None;
                    }
                    Some(PlanSummary {
                        id,
                        title,
                        agent_id: p.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        status: p.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
                        updated_at: p.get("updated_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        steps_total: p.get("steps_total").and_then(Value::as_u64).unwrap_or(0) as usize,
                        steps_done: p.get("steps_done").and_then(Value::as_u64).unwrap_or(0) as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_plan_detail(v: &Value) -> Option<PlanDetail> {
    let p = v.get("plan")?;
    let id = p.get("id").and_then(Value::as_str)?.to_string();
    let plan = PlanMeta {
        id,
        title: p.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        description: p.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
        agent_id: p.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
        status: p.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
        created_at: p.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
        updated_at: p.get("updated_at").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    let steps = v
        .get("steps")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let id = s.get("id").and_then(Value::as_str)?.to_string();
                    Some(PlanStep {
                        id,
                        plan_id: s.get("plan_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        text: s.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                        assignee_kind: s.get("assignee_kind").and_then(Value::as_str).unwrap_or("").to_string(),
                        assignee: s.get("assignee").and_then(Value::as_str).unwrap_or("").to_string(),
                        status: s.get("status").and_then(Value::as_str).unwrap_or("todo").to_string(),
                        step_order: s.get("step_order").and_then(Value::as_i64).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(PlanDetail { plan, steps })
}

// ── Global state ───────────────────────────────────────────────────────

pub struct PlansState {
    requested: bool,
    pub plans: Loadable<Vec<PlanSummary>>,
    pub selected: Option<String>,
    pub detail: Loadable<PlanDetail>,
    /// Which plan id `detail` was fetched for — `maybe_fetch_detail`'s latch,
    /// mirrors `requested`'s role for the list fetch but keyed by id since
    /// the detail source changes every time the selection changes.
    detail_for: Option<String>,
    /// Typed text in the "新增步驟" input — see this module's header comment
    /// on why the button next to it stays inert.
    pub new_step_text: String,
}

impl PlansState {
    fn new() -> Self {
        Self {
            requested: false,
            plans: Loadable::Loading,
            selected: None,
            detail: Loadable::Loading,
            detail_for: None,
            new_step_text: String::new(),
        }
    }

    pub fn request_refresh(&mut self) {
        self.requested = false;
        self.plans = Loadable::Loading;
        self.detail = Loadable::Loading;
        self.detail_for = None;
    }
}

impl Global for PlansState {}

pub struct PlansSnapshot {
    pub plans: Loadable<Vec<PlanSummary>>,
    pub selected: Option<String>,
    pub detail: Loadable<PlanDetail>,
    pub new_step_text: String,
}

fn snapshot(cx: &Context<RootView>) -> PlansSnapshot {
    let s = cx.global::<PlansState>();
    PlansSnapshot {
        plans: s.plans.clone(),
        selected: s.selected.clone(),
        detail: s.detail.clone(),
        new_step_text: s.new_step_text.clone(),
    }
}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<PlansState>() {
        cx.set_global(PlansState::new());
    }
}

/// Which plan id should be selected — the explicit click-selection when it
/// still resolves to a live row, else the first row's id, else `None`.
/// Mirrors `routines.rs::resolve_selection`'s exact shape (list, not
/// detail — see `plans_detail`'s own resolution over `PlansSnapshot::
/// detail` for the right column).
pub fn resolve_selected_id(explicit: &Option<String>, rows: &[PlanSummary]) -> Option<String> {
    if let Some(id) = explicit {
        if rows.iter().any(|r| &r.id == id) {
            return Some(id.clone());
        }
    }
    rows.first().map(|r| r.id.clone())
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch_list(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<PlansState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<PlansState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "plans.list", json!({}), |cx, result| {
        cx.global_mut::<PlansState>().plans = result.map(|v| parse_plans(&v)).into();
    });
}

/// Fires `plans.get` once per distinct effective selection — `render`'s own
/// top-of-pass call site (the only place with a live `cx`, same placement
/// `maybe_fetch_list`/every other S5b page's `maybe_fetch` uses).
fn maybe_fetch_detail(state: &RootView, cx: &mut Context<RootView>, plan_id: &str) {
    if cx.global::<PlansState>().detail_for.as_deref() == Some(plan_id) {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    {
        let g = cx.global_mut::<PlansState>();
        g.detail_for = Some(plan_id.to_string());
        g.detail = Loadable::Loading;
    }
    let tx = state.session_tx.clone();
    let id_for_apply = plan_id.to_string();
    spawn_call(cx, tx, "plans.get", json!({ "plan_id": plan_id }), move |cx, result| {
        // A stale response (selection changed again while this was
        // in-flight) is dropped rather than overwriting a newer fetch.
        if cx.global::<PlansState>().detail_for.as_deref() != Some(id_for_apply.as_str()) {
            return;
        }
        cx.global_mut::<PlansState>().detail = result
            .and_then(|v| parse_plan_detail(&v).ok_or_else(|| "malformed plans.get response".to_string()))
            .into();
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

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_list(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("plans-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                i18n::t(locale, "native.home.connError.title"),
                Some(i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    // Resolve the effective selection BEFORE fetching detail — the plan
    // list may still be `Loading`/`Failed`, in which case there is nothing
    // to select yet and `maybe_fetch_detail` is simply not called this pass
    // (it will be once the list resolves and a re-render happens).
    if let Loadable::Ready(rows) = &cx.global::<PlansState>().plans {
        if let Some(id) = resolve_selected_id(&cx.global::<PlansState>().selected, rows) {
            maybe_fetch_detail(state, cx, &id);
        }
    }

    let snap = snapshot(cx);
    let left = plans_rows::list_column(&snap, locale, cx);
    let right = plans_detail::detail_column(&snap, locale, cx);

    div().id("plans-page").size_full().flex().child(left).child(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plans_skips_malformed_rows() {
        let v = json!({ "plans": [
            { "id": "p1", "title": "新產品上線準備", "agent_id": "dudu", "status": "active", "updated_at": "t", "steps_total": 5, "steps_done": 3 },
            { "id": "", "title": "no id" },
            { "title": "no id field" },
        ] });
        let rows = parse_plans(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "p1");
        assert_eq!(rows[0].steps_total, 5);
        assert_eq!(rows[0].steps_done, 3);
    }

    #[test]
    fn parse_plan_detail_reads_plan_and_steps() {
        let v = json!({
            "plan": { "id": "p1", "title": "T", "description": "D", "agent_id": "dudu", "status": "active", "created_at": "c", "updated_at": "u" },
            "steps": [
                { "id": "s1", "plan_id": "p1", "text": "撰寫產品文案", "assignee_kind": "agent", "assignee": "dudu", "status": "done", "step_order": 1024 },
                { "id": "s2", "plan_id": "p1", "text": "準備定價方案", "assignee_kind": "agent", "assignee": "財務助理", "status": "doing", "step_order": 2048 },
            ],
        });
        let detail = parse_plan_detail(&v).expect("should parse");
        assert_eq!(detail.plan.title, "T");
        assert_eq!(detail.steps.len(), 2);
        assert_eq!(detail.steps[1].status, "doing");
    }

    #[test]
    fn parse_plan_detail_missing_plan_is_none() {
        assert!(parse_plan_detail(&json!({})).is_none());
    }

    #[test]
    fn parse_plan_detail_step_status_defaults_to_todo() {
        let v = json!({
            "plan": { "id": "p1", "title": "T" },
            "steps": [ { "id": "s1" } ],
        });
        let detail = parse_plan_detail(&v).expect("should parse");
        assert_eq!(detail.steps[0].status, "todo");
    }

    #[test]
    fn cycle_step_status_advances_through_the_four_states() {
        assert_eq!(cycle_step_status("todo"), "doing");
        assert_eq!(cycle_step_status("doing"), "done");
        assert_eq!(cycle_step_status("done"), "skipped");
        assert_eq!(cycle_step_status("skipped"), "todo");
    }

    #[test]
    fn cycle_step_status_unknown_falls_back_to_first_state() {
        assert_eq!(cycle_step_status("bogus"), "doing");
    }

    #[test]
    fn resolve_selected_id_falls_back_to_first_row() {
        let rows = vec![
            PlanSummary {
                id: "a".into(),
                title: "A".into(),
                agent_id: "".into(),
                status: "".into(),
                updated_at: "".into(),
                steps_total: 0,
                steps_done: 0,
            },
        ];
        assert_eq!(resolve_selected_id(&None, &rows), Some("a".to_string()));
        assert_eq!(resolve_selected_id(&Some("missing".to_string()), &rows), Some("a".to_string()));
        assert_eq!(resolve_selected_id(&Some("a".to_string()), &rows), Some("a".to_string()));
        assert_eq!(resolve_selected_id(&None, &[]), None);
    }
}
