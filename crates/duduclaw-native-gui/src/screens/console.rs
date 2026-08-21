// S4b (second wave) — Screen "主控台" (`nav.rs` id `console`, CHAT_ITEMS —
// already labeled "主控台" / "用自然語言操作整台機器"; this pass wires that
// existing nav slot to a real page, same shape the S4b dashboard pass used
// for `home`).
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Console.dc.html` —
// a "chat 骨架變體": left column = event queue (filter chips + status-dot
// rows), main column = one selected item's approval-context stream (header +
// narrative bubble + decision card), bottom = a reply composer. The static
// prototype at `screens/prototypes/p05_console.rs` is the S4a pencil sketch
// of the SAME idea (one hardcoded approval card); this file is the real,
// RPC-backed page that prototype was standing in for.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `approvals.list` (`handle_approvals_list`, ~line 31358) → `{ "approvals":
//     [ { "id", "agent_id", "kind", "summary", "payload", "created_at",
//         "ttl_seconds", "expires_at", "simulation", "channel",
//         "channel_link" } ], "count" }` — ALREADY pending-only
//     (`broker.list_pending`), so this page never needs its own status
//     filter for approvals.
//   `tasks.list` (`{"goal_mode": true}`) → `{ "tasks": [...] }`,
//     `task_row_to_json` fields (same shape `dashboard.rs::parse_goal_card`
//     already reads) — this page filters client-side to `status ==
//     "needs_human"` (mirrors the web `/goals` page's `WAITING_STATUSES`).
//   `approvals.decide` (`handle_approvals_decide`, ~line 31862) — DECISION
//     RPC, params `{ "id", "approve": bool, "reason"? }`.
//   `tasks.goal_decide` (`handle_tasks_goal_decide`, ~line 29784) — DECISION
//     RPC, params `{ "task_id", "action": "retry"|"abort"|..., "note"? }`
//     (`"continue"` instead needs `"message"`, non-empty — not used by this
//     page's 3-button subset, see below).
//
// ── Scope: 3-button goal-intervention subset, not the web's 4 ────────────
// The web `/goals` page's `InterventionButtons` (`web/src/pages/
// GoalsPage.tsx`) offers four actions: retry / done / abort / takeover. This
// task's brief asks for exactly THREE: 「核准繼續／附指示重試／中止」. Rather
// than inventing a new RPC action, this page maps both "核准繼續" and "附指示
//重試" onto the SAME real action (`"retry"`, whose `note` param is already
// optional server-side) — 核准繼續 sends an empty note (proceed as-is),
// 附指示重試 opens an inline field and sends the typed note. 中止 is a plain
// `"abort"`. `"done"`/`"takeover"` are deliberately not exposed on this
// compact card — an honest, documented scope cut (matching this crate's
// established "3 destructive intents max on a card" convention), not a
// missing feature: the full 4-action set stays reachable from `/goals`.
//
// ── Why per-page state lives behind `gpui::Global`, not a new `RootView`
// field ───────────────────────────────────────────────────────────────────
// Every prior S4b page (`chat.rs`'s `ChatState`, `dashboard.rs`'s
// `DashboardState`) hangs its state off a dedicated `RootView` field, wired
// in `main.rs`. This pass's task brief explicitly forbids touching
// `main.rs`/`nav.rs` this round (a parallel wave owns them for other pages
// landing the same day) — the same constraint `chat.rs`'s own module doc
// comment already documents for its S4b additions, and `sidebar_rpc.rs`'s
// doc comment for why it dials its own connection instead of reusing
// `ws_status.rs`. Both of those precedents solved "no `main.rs` edit
// available" by making the NEW state self-contained inside an ALREADY-
// `RootView`-attached parent (`ChatState`). This page has no such parent to
// attach to (nothing on `RootView` owns "console" state yet), so it reaches
// for gpui's own designed-for-this mechanism instead: `Global` — app-wide
// singleton storage keyed by Rust type (`cx.set_global`/`cx.global::<T>()`/
// `cx.global_mut::<T>()`, `crates/gpui/src/global.rs`), which needs zero
// `RootView`/`main.rs` changes and cannot collide with any other page's own
// distinct `Global` type. `ensure_state` below lazily installs it on first
// render, the same "construct once, cache" shape `conversations.rs`'s
// `search_focus` `RefCell` uses for its own lazily-created `FocusHandle`.
//
// `state.session_tx` (the main authenticated `/ws`'s command channel) IS
// already a `RootView` field (private, but — like every other private field
// on a type defined at the crate root — visible throughout this crate's
// module tree; `dashboard.rs` already relies on the exact same fact for
// `view.session_tx`). So unlike `sidebar_rpc.rs`, this page does NOT need
// its own dedicated connection: `ws_status::call` is reused directly,
// mirroring `dashboard.rs`'s `spawn_call` almost verbatim (the only
// difference is the target of mutation — `Context<RootView>`'s global slot
// instead of a `RootView` field).

use std::cell::RefCell;

use gpui::{div, prelude::*, Context, Div, FocusHandle, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::rpc::CallError;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

mod console_detail;
mod console_queue;

// ── Data model ─────────────────────────────────────────────────────────

pub use crate::screens::dashboard::Loadable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalItem {
    pub id: String,
    pub agent_id: String,
    pub kind: String,
    pub summary: String,
    pub payload: Value,
    pub created_at: String,
    pub channel: Option<String>,
    pub channel_link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalItem {
    pub id: String,
    pub title: String,
    pub assigned_to: String,
    pub updated_at: String,
    pub revision_round: i64,
    pub judge_feedback: Option<String>,
    /// Already a resolved token (`"no_progress"`/`"budget_exhausted"`/...)
    /// per `task_row_to_json`'s own doc comment — never the raw column.
    pub pause_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    WaitingApproval,
    Stuck,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Approval(String),
    Goal(String),
}

/// One combined, sorted row for the left queue — built fresh each render
/// from the two `Loadable`s, never stored (see `merged_queue`).
#[derive(Clone)]
pub enum QueueEntry {
    Approval(ApprovalItem),
    Goal(GoalItem),
}

impl QueueEntry {
    pub fn selection(&self) -> Selection {
        match self {
            QueueEntry::Approval(a) => Selection::Approval(a.id.clone()),
            QueueEntry::Goal(g) => Selection::Goal(g.id.clone()),
        }
    }

    fn recency_key(&self) -> &str {
        match self {
            QueueEntry::Approval(a) => a.created_at.as_str(),
            QueueEntry::Goal(g) => g.updated_at.as_str(),
        }
    }
}

/// App-wide singleton page state — see this module's header comment for why
/// `Global` instead of a `RootView` field.
pub struct ConsoleState {
    requested: bool,
    pub approvals: Loadable<Vec<ApprovalItem>>,
    pub goals: Loadable<Vec<GoalItem>>,
    pub filter: Filter,
    pub selected: Option<Selection>,
    /// Whether the goal card's "附指示重試" inline note field is open.
    pub goal_note_open: bool,
    pub goal_note: String,
    /// Id of the item currently awaiting a decision RPC's response — disables
    /// every decision button on the card while set (never both approval and
    /// goal decisions in flight at once; one card is visible at a time).
    pub decision_busy: Option<String>,
    pub decision_error: Option<String>,
    /// Lazily created on first render (needs `&App`, unavailable in
    /// `ConsoleState::new()`) — same pattern as `conversations.rs`'s
    /// `search_focus`. Deliberately excluded from `ConsoleSnapshot` (a
    /// `FocusHandle` is a live window resource, not view data).
    note_focus: RefCell<Option<FocusHandle>>,
}

impl ConsoleState {
    fn new() -> Self {
        Self {
            requested: false,
            approvals: Loadable::Loading,
            goals: Loadable::Loading,
            filter: Filter::All,
            selected: None,
            goal_note_open: false,
            goal_note: String::new(),
            decision_busy: None,
            decision_error: None,
            note_focus: RefCell::new(None),
        }
    }

    /// The manual refresh path — re-fetches both sources. Deliberately
    /// leaves `filter`/`selected` untouched (unlike `dashboard.rs`'s
    /// wholesale `request_refresh`): re-picking a filter or losing the
    /// currently-open detail pane on every refresh would be a worse UX for a
    /// page whose whole point is "look at ONE thing and decide" — a stale
    /// `selected` id simply falls out of the merged list once its source
    /// re-resolves (`resolve_selection` below degrades to "pick the first
    /// row" automatically).
    pub fn request_refresh(&mut self) {
        self.requested = false;
        self.approvals = Loadable::Loading;
        self.goals = Loadable::Loading;
    }
}

impl Global for ConsoleState {}

/// Read-only, owned snapshot of `ConsoleState` for rendering — cloned out of
/// the `Global` slot immediately so `console_cards.rs`'s rendering helpers
/// can freely call `cx.listener(...)` afterward without fighting the borrow
/// checker over a live `&ConsoleState` (which itself borrows `cx`). See this
/// module's header comment.
pub struct ConsoleSnapshot {
    pub approvals: Loadable<Vec<ApprovalItem>>,
    pub goals: Loadable<Vec<GoalItem>>,
    pub filter: Filter,
    pub selected: Option<Selection>,
    pub goal_note_open: bool,
    pub goal_note: String,
    pub decision_busy: Option<String>,
    pub decision_error: Option<String>,
}

fn snapshot(cx: &Context<RootView>) -> ConsoleSnapshot {
    let s = cx.global::<ConsoleState>();
    ConsoleSnapshot {
        approvals: s.approvals.clone(),
        goals: s.goals.clone(),
        filter: s.filter,
        selected: s.selected.clone(),
        goal_note_open: s.goal_note_open,
        goal_note: s.goal_note.clone(),
        decision_busy: s.decision_busy.clone(),
        decision_error: s.decision_error.clone(),
    }
}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<ConsoleState>() {
        cx.set_global(ConsoleState::new());
    }
}

fn ensure_focus_handle(cx: &gpui::App) -> FocusHandle {
    let state = cx.global::<ConsoleState>();
    if let Some(existing) = state.note_focus.borrow().clone() {
        return existing;
    }
    let handle = cx.focus_handle();
    *state.note_focus.borrow_mut() = Some(handle.clone());
    handle
}

/// Which queue entry the detail pane should actually show: the explicit
/// click-selection when it still resolves to a live row, else the first row
/// of the (already filtered+sorted) merged list, else `None` (empty queue).
/// Pure/display-only — never writes back to `ConsoleState::selected`.
pub fn resolve_selection(explicit: &Option<Selection>, merged: &[QueueEntry]) -> Option<Selection> {
    if let Some(sel) = explicit {
        if merged.iter().any(|e| &e.selection() == sel) {
            return Some(sel.clone());
        }
    }
    merged.first().map(QueueEntry::selection)
}

/// Combine + sort the two sources newest-first by each row's own recency
/// field (`created_at` for approvals, `updated_at` for tasks — both RFC3339,
/// so a lexicographic string comparison across the two fields is a valid
/// combined ordering, same reasoning `dashboard.rs::short_time`'s doc
/// comment already relies on for RFC3339 strings). `filter` narrows which
/// KIND of row is included; it never hides a row for any other reason (e.g.
/// no client-side re-implementation of "pending" — `approvals.list` is
/// already pending-only server-side).
pub fn merged_queue(approvals: &[ApprovalItem], goals: &[GoalItem], filter: Filter) -> Vec<QueueEntry> {
    let mut rows: Vec<QueueEntry> = Vec::with_capacity(approvals.len() + goals.len());
    if filter != Filter::Stuck {
        rows.extend(approvals.iter().cloned().map(QueueEntry::Approval));
    }
    if filter != Filter::WaitingApproval {
        rows.extend(goals.iter().cloned().map(QueueEntry::Goal));
    }
    rows.sort_by(|a, b| b.recency_key().cmp(a.recency_key()));
    rows
}

/// The queue's three-state read, derived once from a [`ConsoleSnapshot`] and
/// shared by BOTH `console_cards::queue_column` (the left list) and
/// `console_cards::detail_column` (the right pane) — a single source of
/// truth so the highlighted row and the shown detail can never disagree
/// about what "the current filtered list" is. `Loading` fires while EITHER
/// source hasn't resolved yet (never a half-populated list); once both have
/// resolved, `Ready` carries whichever data came back plus per-source error
/// text for anything that failed — a failed source degrades to "zero rows
/// from that source, error banner shown", never a blanked page (dashboard.rs's
/// per-card honesty philosophy, applied to a merged list instead of separate
/// cards since this page has only one list to show).
pub enum QueueLoad<'a> {
    Loading,
    Ready { entries: Vec<QueueEntry>, approvals_error: Option<&'a str>, goals_error: Option<&'a str> },
}

pub fn queue_load_state(snap: &ConsoleSnapshot) -> QueueLoad<'_> {
    if matches!(snap.approvals, Loadable::Loading) || matches!(snap.goals, Loadable::Loading) {
        return QueueLoad::Loading;
    }
    let (approvals_items, approvals_error): (&[ApprovalItem], Option<&str>) = match &snap.approvals {
        Loadable::Ready(v) => (v.as_slice(), None),
        Loadable::Failed(e) => (&[], Some(e.as_str())),
        Loadable::Loading => unreachable!("checked above"),
    };
    let (goals_items, goals_error): (&[GoalItem], Option<&str>) = match &snap.goals {
        Loadable::Ready(v) => (v.as_slice(), None),
        Loadable::Failed(e) => (&[], Some(e.as_str())),
        Loadable::Loading => unreachable!("checked above"),
    };
    let entries = merged_queue(approvals_items, goals_items, snap.filter);
    QueueLoad::Ready { entries, approvals_error, goals_error }
}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_approvals(v: &Value) -> Vec<ApprovalItem> {
    v.get("approvals")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|a| ApprovalItem {
                    id: str_field(a, "id"),
                    agent_id: str_field(a, "agent_id"),
                    kind: str_field(a, "kind"),
                    summary: str_field(a, "summary"),
                    payload: a.get("payload").cloned().unwrap_or(Value::Null),
                    created_at: str_field(a, "created_at"),
                    channel: opt_str_field(a, "channel"),
                    channel_link: opt_str_field(a, "channel_link"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `tasks.list {goal_mode:true}` → filtered client-side to `needs_human`
/// (the "卡住" chip's whole definition — see this module's header comment).
fn parse_goal_tasks(v: &Value) -> Vec<GoalItem> {
    v.get("tasks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|t| t.get("status").and_then(Value::as_str) == Some("needs_human"))
                .map(|t| GoalItem {
                    id: str_field(t, "id"),
                    title: str_field(t, "title"),
                    assigned_to: str_field(t, "assigned_to"),
                    updated_at: str_field(t, "updated_at"),
                    revision_round: t.get("revision_round").and_then(Value::as_i64).unwrap_or(0),
                    judge_feedback: opt_str_field(t, "judge_feedback").filter(|s| !s.is_empty()),
                    pause_reason: opt_str_field(t, "pause_reason"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Shared by both `console_queue.rs` (row title fallback) and
/// `console_detail.rs` (approval card header/badge) — hoisted here rather
/// than duplicated so the two split files don't drift on the "unresolved
/// kind → raw string, never a raw i18n key" fallback rule.
pub(super) fn agent_label(id: &str) -> String {
    if id.is_empty() {
        "—".to_string()
    } else {
        id.to_string()
    }
}

/// Mirrors `dashboard_cards.rs::status_label`'s shape: known kinds get a
/// localized label, anything else shows the raw server string verbatim
/// (never a raw, unresolved i18n key — see `web/src/components/console/
/// ApprovalRequestCard.tsx::kindLabel`'s own fallback-to-raw convention).
pub(super) fn kind_label(locale: Locale, kind: &str) -> SharedString {
    let key = match kind {
        "browser_action" => "native.console.kind.browserAction",
        "tool_call" => "native.console.kind.toolCall",
        "skill_activation" => "native.console.kind.skillActivation",
        "strategic_plan" => "native.console.kind.strategicPlan",
        "agent_hire" => "native.console.kind.agentHire",
        "wiki_ingest" => "native.console.kind.wikiIngest",
        "" => return i18n::t(locale, "native.console.kind.other"),
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn opt_str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `"2026-08-21T13:45:00Z"` → `"13:45"`. ISO 8601 is pure ASCII, so raw byte
/// slicing is safe here — same documented exception `dashboard.rs::
/// short_time` already establishes for the identical field shape.
pub fn short_time(ts: &str) -> String {
    if ts.len() >= 16 && ts.as_bytes().get(10) == Some(&b'T') {
        ts[11..16].to_string()
    } else {
        ts.to_string()
    }
}

/// Char-count truncation for arbitrary (possibly CJK) text — this crate has
/// no dependency on `duduclaw_core::truncate_chars` (deliberately detached
/// workspace, see `Cargo.toml`'s header comment on the D4 spike's `gpui` rev
/// pin), so this is a small local equivalent for the one place this page
/// needs it (the approval card's mono payload preview, which unlike an
/// RFC3339 timestamp is NOT provably ASCII-only). Never slices by raw byte
/// index — coding convention #1.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

// ── Fetch orchestration (read-only RPCs) ──────────────────────────────

/// Fired from the top of `render` (the only place this module has a live
/// `cx: &mut Context<RootView>`; `main.rs`'s poll loop is off-limits this
/// pass — see header comment). `requested` latches so re-renders (which
/// happen constantly) don't re-issue the fetch every frame.
fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<ConsoleState>().requested {
        return;
    }
    // Same "wait for Authenticated" gate `dashboard.rs::maybe_fetch` uses —
    // firing earlier would just immediately resolve `NotConnected` and latch
    // `requested` before real data ever arrives.
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<ConsoleState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "approvals.list", json!({}), |cx, result| {
        cx.global_mut::<ConsoleState>().approvals = result.map(|v| parse_approvals(&v)).into();
    });
    spawn_call(cx, tx, "tasks.list", json!({ "goal_mode": true }), |cx, result| {
        cx.global_mut::<ConsoleState>().goals = result.map(|v| parse_goal_tasks(&v)).into();
    });
}

/// One RPC round trip, mutating `ConsoleState` (the `Global` slot) through
/// `cx` instead of a `RootView` field — the only shape difference from
/// `dashboard.rs::spawn_call`, which this otherwise mirrors line for line.
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

// ── Decision RPCs (never invoked by this pass's own live smoke test — see
// this crate's S4b-console report) ─────────────────────────────────────

const METHOD_APPROVALS_DECIDE: &str = "approvals.decide";
const METHOD_GOAL_DECIDE: &str = "tasks.goal_decide";

/// `approvals.decide` params — `handle_approvals_decide` reads exactly
/// `id`/`approve`/`reason` (`crates/duduclaw-gateway/src/handlers.rs`,
/// ~line 31862); this page never collects a reason, so it's omitted rather
/// than sent empty (the handler already treats a missing `reason` as `""`).
pub fn build_approval_decide_params(id: &str, approve: bool) -> Value {
    json!({ "id": id, "approve": approve })
}

/// `tasks.goal_decide` params — `handle_tasks_goal_decide` (~line 29784).
/// `action` is always `"retry"` or `"abort"` from this card (see header
/// comment for why `"done"`/`"takeover"`/`"continue"` aren't offered here);
/// `note` is omitted entirely (not sent as `""`) when absent, matching the
/// handler's own `unwrap_or_default()` fallback exactly.
pub fn build_goal_decide_params(task_id: &str, action: &'static str, note: Option<&str>) -> Value {
    let mut obj = json!({ "task_id": task_id, "action": action });
    if let Some(n) = note {
        if let Value::Object(map) = &mut obj {
            map.insert("note".to_string(), json!(n));
        }
    }
    obj
}

/// Submit an approve/reject decision — called from the approval card's two
/// buttons. Optimistically drops the decided item out of the local list on
/// success (no separate re-fetch needed) and clears the busy/selection
/// state; a failure surfaces via `decision_error` and leaves the item in
/// place so the user can retry.
pub fn submit_approval_decision(view: &mut RootView, cx: &mut Context<RootView>, id: String, approve: bool) {
    {
        let g = cx.global_mut::<ConsoleState>();
        g.decision_busy = Some(id.clone());
        g.decision_error = None;
    }
    let tx = view.session_tx.clone();
    let params = build_approval_decide_params(&id, approve);
    let id_for_apply = id.clone();
    spawn_call(cx, tx, METHOD_APPROVALS_DECIDE, params, move |cx, result| {
        let g = cx.global_mut::<ConsoleState>();
        g.decision_busy = None;
        match result {
            Ok(_) => {
                if let Loadable::Ready(list) = &mut g.approvals {
                    list.retain(|a| a.id != id_for_apply);
                }
                if g.selected == Some(Selection::Approval(id_for_apply)) {
                    g.selected = None;
                }
            }
            Err(e) => g.decision_error = Some(e),
        }
    });
    cx.notify();
}

/// Submit a goal-loop intervention — called from the goal card's
/// 核准繼續/附指示重試/中止 buttons (see header comment for the action mapping).
pub fn submit_goal_decision(
    view: &mut RootView,
    cx: &mut Context<RootView>,
    task_id: String,
    action: &'static str,
    note: Option<String>,
) {
    {
        let g = cx.global_mut::<ConsoleState>();
        g.decision_busy = Some(task_id.clone());
        g.decision_error = None;
    }
    let tx = view.session_tx.clone();
    let params = build_goal_decide_params(&task_id, action, note.as_deref());
    let id_for_apply = task_id.clone();
    spawn_call(cx, tx, METHOD_GOAL_DECIDE, params, move |cx, result| {
        let g = cx.global_mut::<ConsoleState>();
        g.decision_busy = None;
        match result {
            Ok(_) => {
                if let Loadable::Ready(list) = &mut g.goals {
                    list.retain(|t| t.id != id_for_apply);
                }
                if g.selected == Some(Selection::Goal(id_for_apply)) {
                    g.selected = None;
                }
                g.goal_note_open = false;
                g.goal_note.clear();
            }
            Err(e) => g.decision_error = Some(e),
        }
    });
    cx.notify();
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("console-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                crate::i18n::t(locale, "native.home.connError.title"),
                Some(crate::i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    let snap = snapshot(cx);
    let left = console_queue::queue_column(&snap, locale, cx);
    let right = console_detail::detail_column(state, &snap, locale, cx);

    div().id("console-page").size_full().flex().child(left).child(right)
}

#[cfg(test)]
mod tests;
