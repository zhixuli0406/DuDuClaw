// S4b (second wave) — 收件匣 (p07). Visual authority: `commercial/design/
// duduclaw-s4a-pages/Inbox.dc.html` — one merged feed, type filter chips
// (全部/審批/通知/提及/系統), a persistent "全部已讀" action top-right, two-
// line rows with an unread dot, and an approval row's expanded state (amber-
// bordered card: context line + mono simulation summary + note input +
// approve/reject).
//
// ── Data flow ─────────────────────────────────────────────────────────
// Merges TWO independent RPCs into one feed: `approvals.list` (every
// pending approval, unconditionally "on top" per the task brief) and
// `activity.list` (the same feed `dashboard.rs`'s activity shelf already
// consumes, just with a higher limit here). Each keeps its OWN `Loadable`
// (see `dashboard::Loadable`, reused rather than redefined — same enum,
// same three states) so a failure on one source degrades to "show what the
// other source has, plus an inline error banner", never a blank page — the
// same per-source-independence principle `dashboard.rs`'s module doc
// comment lays out for its six cards, applied here to two feed sources
// instead of six cards.
//
// ── "提及" (mention) — honest scope cut, read before assuming a bug ──────
// The backend has NO mention signal anywhere in `activity.list`'s payload
// (grep-verified across `duduclaw-gateway/src/*.rs`: no `event_type`
// containing "mention", no `@mention`-derived activity row). The canvas's
// "財務助理 在「供應商比價」提到你" row has no real data source to back it.
// The 提及 filter chip is kept (canvas fidelity — the brief names it
// explicitly) but [`classify_activity_event`] can never route an item into
// [`FeedCategory::Mention`] — selecting that chip always shows the empty
// state. Documented here and in the crate's own report rather than silently
// wired to always-empty with no explanation.
//
// ── System vs. Notification — the OTHER honest judgment call ────────────
// `ActivityRow::agent_id` is never empty (grep-verified: platform/infra
// events stamp a SENTINEL agent id — `"autopilot"` in `autopilot_engine.rs`,
// `"channel_alert"` in `channel_alerts.rs` — not `""`), so agent-id
// presence can't distinguish "an AI staff member's own work" from
// "housekeeping". [`classify_activity_event`] instead matches a small,
// explicit `event_type` PREFIX table (mirrors `notify_digest.rs`'s own
// `is_learning_event`/`LEARNING_PREFIXES` pattern — `starts_with` over a
// closed prefix list, not an unanchored `contains`, per this crate's own
// coding convention #2). Deliberately not exhaustive: an event_type this
// table doesn't recognize defaults to Notification, which may misclassify
// a future infra event type — a documented judgment call, not a silent one.
//
// ── State pattern convergence (2026-08 consistency debt cleanup) ────────
// This page originally hung its fetch/UI state off a dedicated `RootView`
// field (`main.rs`'s `inbox: screens::inbox::InboxState`), the ONLY page in
// this crate to do so — every other RPC-backed page added since
// (`console.rs`'s `ConsoleState`, `goals.rs`'s `GoalsState`) uses gpui's own
// `Global` singleton mechanism instead, precisely to avoid needing a
// `main.rs` edit per new page (see `console.rs`'s own header comment,
// "Why per-page state lives behind `gpui::Global`, not a new `RootView`
// field", for the full reasoning — unchanged here, just applied to this
// page too). This pass converges `InboxState` onto that same `Global`
// pattern: `ensure_state` lazily installs it on first render (mirroring
// `console.rs::ensure_state`), `maybe_fetch` now fires from the top of
// `render` itself instead of `main.rs`'s poll loop (mirroring
// `console.rs`/`goals.rs`'s own `maybe_fetch` placement), and every
// `view.inbox.*` / `this.inbox.*` access throughout this file and
// `inbox_rows.rs` became `cx.global::<InboxState>()` /
// `cx.global_mut::<InboxState>()`. Behavior is unchanged — same three
// `Loadable`s, same fetch-once latch, same click-handler mutations — only
// WHERE the state lives changed. `main.rs`'s `inbox` field, its
// construction call, and its poll-loop `maybe_fetch` call were removed in
// the same pass (dashboard's own `DashboardState` field is a deliberately
// separate, not-yet-converged case — out of this pass's scope).

use std::collections::HashSet;

use gpui::{div, prelude::*, px, App, Context, Div, Entity, Global, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::screens::inbox_rows as rows;
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedCategory {
    Approval,
    Notification,
    Mention,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    All,
    Approval,
    Notification,
    Mention,
    System,
}

impl FilterKind {
    /// The 5 chips, in the canvas's own left-to-right order.
    pub const ALL: [FilterKind; 5] = [
        FilterKind::All,
        FilterKind::Approval,
        FilterKind::Notification,
        FilterKind::Mention,
        FilterKind::System,
    ];

    fn matches(self, category: FeedCategory) -> bool {
        match self {
            FilterKind::All => true,
            FilterKind::Approval => category == FeedCategory::Approval,
            FilterKind::Notification => category == FeedCategory::Notification,
            FilterKind::Mention => category == FeedCategory::Mention,
            FilterKind::System => category == FeedCategory::System,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            FilterKind::All => "native.inbox.filter.all",
            FilterKind::Approval => "native.inbox.filter.approval",
            FilterKind::Notification => "native.inbox.filter.notification",
            FilterKind::Mention => "native.inbox.filter.mention",
            FilterKind::System => "native.inbox.filter.system",
        }
    }

    fn empty_key(self) -> &'static str {
        match self {
            FilterKind::All => "native.inbox.empty.all",
            FilterKind::Approval => "native.inbox.empty.approval",
            FilterKind::Notification => "native.inbox.empty.notification",
            FilterKind::Mention => "native.inbox.empty.mention",
            FilterKind::System => "native.inbox.empty.system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedBadge {
    /// An approval, always pending (`approvals.list` only ever returns
    /// pending rows — see `handle_approvals_list`).
    Pending,
    /// `activity.list` row whose `type == "task_completed"`.
    Done,
}

/// Only populated for [`FeedCategory::Approval`] rows — the fields the
/// expanded card needs that a plain notification row never carries.
#[derive(Debug, Clone)]
pub struct ApprovalExpand {
    pub approval_id: String,
    pub kind: String,
    /// D1 ActionGuard forward-simulation narrative (`approval.rs`'s
    /// `SimulationNarrative`), parsed straight from the RPC's raw
    /// `simulation` JSON — `None` for the overwhelming majority of
    /// approvals that never ran that judge (see `handle_approvals_list`'s
    /// own comment on this field). The mono "will execute" block in the
    /// expanded card is omitted entirely when this is `None`, never
    /// fabricated.
    pub world_state_change: Option<String>,
    pub risk_points: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FeedItem {
    /// Stable across a single fetch cycle, namespaced by source
    /// (`"approval:<id>"` / `"activity:<id>"`) — the two id spaces are NOT
    /// guaranteed disjoint on their own (both are opaque server ids), so an
    /// unprefixed id could theoretically collide between an approval and an
    /// activity row.
    pub id: String,
    pub category: FeedCategory,
    pub agent_id: String,
    pub title: String,
    /// Raw RFC3339 (or empty) — formatted on demand by `inbox_rows.rs`, same
    /// "store raw, parse at render" convention `conversation_row.rs` already
    /// established for this crate's other timestamp field.
    pub timestamp: String,
    pub badge: Option<FeedBadge>,
    pub approval: Option<ApprovalExpand>,
}

/// Explicit prefix table — see this module's header comment ("System vs.
/// Notification") for why `agent_id` can't be the signal instead.
const SYSTEM_EVENT_PREFIXES: &[&str] = &[
    "channel_",
    "autopilot",
    "gvu_",
    "goal_loop.",
    "os_watch_",
    "playbook_rule_retired",
    "approval.channel_decision",
];

fn classify_activity_event(event_type: &str) -> FeedCategory {
    if SYSTEM_EVENT_PREFIXES.iter().any(|p| event_type.starts_with(p)) {
        FeedCategory::System
    } else {
        FeedCategory::Notification
    }
}

/// Pure merge: `approvals.list`'s `approvals` array + `activity.list`'s
/// `events` array → one ordered feed, approvals first (task brief: "審批
/// pending 置頂"). Field names read directly from `handlers.rs::
/// handle_approvals_list` / `activity_row_to_json` — see this crate's S4b
/// report for the exact line numbers, not guessed.
pub fn build_feed(approvals: &[Value], activities: &[Value]) -> Vec<FeedItem> {
    let mut items = Vec::with_capacity(approvals.len() + activities.len());

    for a in approvals {
        let Some(id) = a.get("id").and_then(Value::as_str) else { continue };
        let agent_id = a.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
        let title = a.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let kind = a.get("kind").and_then(Value::as_str).unwrap_or("other").to_string();
        let timestamp = a.get("created_at").and_then(Value::as_str).unwrap_or("").to_string();

        let sim = a.get("simulation").filter(|v| !v.is_null());
        let world_state_change = sim
            .and_then(|s| s.get("world_state_change"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let risk_points: Vec<String> = sim
            .and_then(|s| s.get("risk_points"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        items.push(FeedItem {
            id: format!("approval:{id}"),
            category: FeedCategory::Approval,
            agent_id,
            title,
            timestamp,
            badge: Some(FeedBadge::Pending),
            approval: Some(ApprovalExpand {
                approval_id: id.to_string(),
                kind,
                world_state_change,
                risk_points,
            }),
        });
    }

    for e in activities {
        let Some(id) = e.get("id").and_then(Value::as_str) else { continue };
        let event_type = e.get("type").and_then(Value::as_str).unwrap_or("");
        let agent_id = e.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
        let title = e.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let timestamp = e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
        items.push(FeedItem {
            id: format!("activity:{id}"),
            category: classify_activity_event(event_type),
            agent_id,
            title,
            timestamp,
            badge: (event_type == "task_completed").then_some(FeedBadge::Done),
            approval: None,
        });
    }

    items
}

pub fn filter_feed(items: &[FeedItem], filter: FilterKind) -> Vec<&FeedItem> {
    items.iter().filter(|it| filter.matches(it.category)).collect()
}

/// `approvals.decide` params — pure, unit-tested. The task brief is explicit
/// that a live approve/reject click must never actually happen during this
/// pass's own verification (it would mutate real approval state on
/// whichever gateway is running) — this function is the substitute: it pins
/// the exact shape the real button's `on_click` sends, verified against
/// `handle_approvals_decide`'s param reads (`id` / `approve` bool / `reason`
/// optional str) instead of guessed.
pub fn build_decide_params(approval_id: &str, approve: bool, note: &str) -> Value {
    json!({ "id": approval_id, "approve": approve, "reason": note })
}

// ── State ──────────────────────────────────────────────────────────────

pub struct InboxState {
    requested: bool,
    pub approvals: Loadable<Vec<Value>>,
    pub activity: Loadable<Vec<Value>>,
    pub filter: FilterKind,
    /// Which approval row (its `FeedItem::id`, `"approval:<id>"`) is showing
    /// its expanded card — at most one at a time, matching the canvas (only
    /// the amber card is ever open).
    pub expanded_id: Option<String>,
    /// One shared entity for whichever approval is currently expanded —
    /// same "reuse one entity across whatever's currently showing it"
    /// pattern `dashboard_cards.rs`'s `prompt_bar` already established for
    /// the chat composer, not a per-row entity (which would need an
    /// unbounded, ever-growing entity pool for a feed that can hold
    /// arbitrarily many approvals over a session).
    pub note_field: Entity<TextField>,
    /// "已看過" ids, this app session only (task brief: "未讀點＝client 本地
    /// 「本次 app session 未看過」語意") — never persisted to disk. An
    /// honest local approximation, same category of client-local
    /// approximation `conversation_row.rs`'s own "未讀點" comment already
    /// flags for its sidebar list.
    pub read_ids: HashSet<String>,
    /// The approval id (bare, not `"approval:"`-prefixed) currently mid-
    /// decide, if any — disables that row's buttons and shows a "送出中…"
    /// label while the RPC is in flight.
    pub deciding_id: Option<String>,
    pub decide_error: Option<String>,
}

impl InboxState {
    /// `locale` is whatever `state.locale` is at the moment `ensure_state`
    /// lazily constructs this `Global` (first render of the inbox page) —
    /// in practice always the app's boot-time locale, since Phase 1a has no
    /// in-app language switcher yet (`language_picker.rs` only ever runs
    /// once, before login) and this page can only be reached after that.
    /// The note field's placeholder is therefore effectively fixed for the
    /// session either way, same "not re-localized on a later in-app
    /// language switch" honest scope cut `main.rs`'s `chat_input`
    /// construction already carries for the identical reason.
    pub fn new(cx: &mut App, locale: Locale) -> Self {
        Self {
            requested: false,
            approvals: Loadable::Loading,
            activity: Loadable::Loading,
            filter: FilterKind::All,
            expanded_id: None,
            note_field: TextField::new(cx, i18n::t(locale, "native.inbox.notePlaceholder"), false, ""),
            read_ids: HashSet::new(),
            deciding_id: None,
            decide_error: None,
        }
    }

    /// Resets only the fetch-related slices (so a retry re-fires both
    /// calls) — deliberately preserves `read_ids`/`filter`/`expanded_id`,
    /// unlike `dashboard::DashboardState::request_refresh`'s full reset:
    /// those three are session-local UI state the user chose, not fetch
    /// state, and a retry-after-error shouldn't discard a filter selection
    /// or collapse an open approval card out from under the user.
    pub fn request_refresh(&mut self) {
        self.requested = false;
        self.approvals = Loadable::Loading;
        self.activity = Loadable::Loading;
    }

    fn approvals_slice(&self) -> &[Value] {
        match &self.approvals {
            Loadable::Ready(v) => v,
            _ => &[],
        }
    }

    fn activity_slice(&self) -> &[Value] {
        match &self.activity {
            Loadable::Ready(v) => v,
            _ => &[],
        }
    }

    fn pending_approval_count(&self) -> usize {
        self.approvals_slice().len()
    }
}

impl Global for InboxState {}

/// Lazily installs the `Global` on first render — mirrors
/// `console.rs::ensure_state` exactly, just needing `locale` too (this
/// state's constructor mints a `TextField` entity, unlike `ConsoleState::
/// new()`'s no-arg form).
fn ensure_state(cx: &mut Context<RootView>, locale: Locale) {
    if !cx.has_global::<InboxState>() {
        let state = InboxState::new(cx, locale);
        cx.set_global(state);
    }
}

// ── Fetch orchestration (mirrors `console.rs::maybe_fetch`'s Global-state
// shape — fired from the top of `render`, not `main.rs`'s poll loop; see
// this module's header comment) ──────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<InboxState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<InboxState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "approvals.list", json!({}), |cx, result| {
        cx.global_mut::<InboxState>().approvals = result
            .map(|v| v.get("approvals").and_then(Value::as_array).cloned().unwrap_or_default())
            .into();
    });
    // 30, not `dashboard.rs`'s 8 — this page's whole reason to exist is
    // being the deeper, browsable feed the home shelf only teases.
    spawn_call(cx, tx, "activity.list", json!({"limit": 30}), |cx, result| {
        cx.global_mut::<InboxState>().activity = result
            .map(|v| v.get("events").and_then(Value::as_array).cloned().unwrap_or_default())
            .into();
    });
}

/// Duplicated from `dashboard.rs`/`console.rs` rather than importing (both
/// are module-private where they live, and this crate's own established
/// convention — see `console.rs::spawn_call`'s doc comment — is to
/// duplicate this ~15-line shape per page rather than widen a sibling
/// file's visibility for one more caller). Byte-for-byte the same shape as
/// `console.rs::spawn_call` (the `Context<RootView>`-based `apply` variant,
/// not `dashboard.rs`'s `&mut RootView` one — this page no longer takes
/// that path).
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

/// Approve/reject dispatch — real RPC wiring (`approvals.decide`), never
/// exercised by this pass's own live smoke test (see this module's header
/// comment). On success, optimistically drops the decided approval out of
/// `approvals` (its own next `approvals.list` refresh would agree — the
/// broker already flipped it out of `Pending` — this just avoids waiting
/// for a refresh the page has no reason to trigger on its own) and closes
/// the expanded card; on failure, surfaces the error inline rather than
/// silently reverting to a state that looks like nothing happened.
pub(super) fn dispatch_decide(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    approval_id: String,
    approve: bool,
    note: String,
) {
    let params = build_decide_params(&approval_id, approve, &note);
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, "approvals.decide", params);
        let outcome = match rx.await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            let g = cx.global_mut::<InboxState>();
            g.deciding_id = None;
            match outcome {
                Ok(()) => {
                    if let Loadable::Ready(items) = &mut g.approvals {
                        items.retain(|a| a.get("id").and_then(Value::as_str) != Some(approval_id.as_str()));
                    }
                    g.expanded_id = None;
                    g.decide_error = None;
                }
                Err(e) => g.decide_error = Some(e),
            }
            cx.notify();
        });
    })
    .detach();
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx, state.locale);
    maybe_fetch(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        // Same page-level gate `dashboard.rs::render` uses, same reused
        // copy — an unauthenticated main `/ws` is exactly the same
        // situation for every RPC-backed page, not something this page
        // needs its own strings for.
        return div()
            .id("inbox-page")
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

    // Clone the three fetch-state fields out of the `Global` slot up front
    // — same "borrow ends immediately, rest of `render` is free to pass
    // `cx` into child functions" reasoning `console.rs::render`'s own doc
    // comment spells out (`filter`/`ChatFilter`... here `FilterKind` — is
    // `Copy`; the two `Loadable<Vec<Value>>`s are cheap enough at this
    // page's own fetch sizes — 30-row `activity.list` cap — to clone once
    // per render rather than restructure every row-rendering call site).
    let (approvals, activity, filter) = {
        let g = cx.global::<InboxState>();
        (g.approvals.clone(), g.activity.clone(), g.filter)
    };

    let still_loading = matches!(approvals, Loadable::Loading) || matches!(activity, Loadable::Loading);
    let approvals_failed = matches!(approvals, Loadable::Failed(_));
    let activity_failed = matches!(activity, Loadable::Failed(_));
    let both_failed = approvals_failed && activity_failed;

    let body: Div = if still_loading {
        div().flex().flex_col().gap_2().children((0..4).map(|_| rows::skeleton_row()).collect::<Vec<_>>())
    } else if both_failed {
        let msg = match (&approvals, &activity) {
            (Loadable::Failed(a), Loadable::Failed(b)) if a == b => a.clone(),
            (Loadable::Failed(a), Loadable::Failed(b)) => format!("{a}；{b}"),
            _ => String::new(), // unreachable given `both_failed`'s guard
        };
        div().flex().flex_col().items_center().gap_3().child(empty_state(
            "⚠️",
            i18n::t1(locale, "native.inbox.loadError", "message", &msg),
            None,
            None::<Div>,
        ))
        .child(rows::retry_button(locale, cx))
    } else {
        let approvals_slice: &[Value] = match &approvals {
            Loadable::Ready(v) => v,
            _ => &[],
        };
        let activity_slice: &[Value] = match &activity {
            Loadable::Ready(v) => v,
            _ => &[],
        };
        let all_items = build_feed(approvals_slice, activity_slice);
        let visible = filter_feed(&all_items, filter);

        let mut col = div().flex().flex_col().gap_2();
        if approvals_failed {
            col = col.child(rows::error_banner(i18n::t1(
                locale,
                "native.inbox.approvalsError",
                "message",
                match &approvals {
                    Loadable::Failed(m) => m,
                    _ => "",
                },
            )));
        }
        if activity_failed {
            col = col.child(rows::error_banner(i18n::t1(
                locale,
                "native.inbox.activityError",
                "message",
                match &activity {
                    Loadable::Failed(m) => m,
                    _ => "",
                },
            )));
        }

        if visible.is_empty() {
            col.child(empty_state("📭", i18n::t(locale, filter.empty_key()), None, None::<Div>))
        } else {
            for item in &visible {
                col = col.child(rows::feed_row(state, item, cx));
            }
            col
        }
    };

    // Reuses `InboxState::pending_approval_count` (a short, separate borrow
    // of the `Global` rather than re-deriving the same count from the local
    // `approvals` clone above) — one source of truth for "how many pending
    // approvals", and keeps that pre-existing method a live call site.
    let approval_count = cx.global::<InboxState>().pending_approval_count();
    let mut chip_row = Vec::with_capacity(FilterKind::ALL.len());
    for kind in FilterKind::ALL {
        let count = if kind == FilterKind::Approval && approval_count > 0 { Some(approval_count) } else { None };
        chip_row.push(rows::filter_chip(kind, kind.label_key(), count, filter == kind, locale, cx));
    }

    div()
        .id("inbox-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_6()
        // ── Header: title + "全部已讀" ───────────────────────────────
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.inbox.title")),
                )
                .child(mark_all_read_button(locale, cx)),
        )
        // ── Filter chips ──────────────────────────────────────────────
        .child(div().flex().flex_wrap().gap_1p5().children(chip_row))
        // ── Feed ──────────────────────────────────────────────────────
        .child(body)
        // ── Footer hint (literally true for the approval subset — a
        // decision made elsewhere really does drop out of the next
        // `approvals.list`; NOT true for activity rows, which have no
        // "already handled" concept at all. Kept as the canvas's own
        // static caption, not re-validated per row — same honesty
        // trade-off `dashboard.rs`'s footer/caption text makes elsewhere
        // for copy the underlying data can't fully back.) ──────────────
        .child(
            div()
                .w_full()
                .text_center()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                .child(i18n::t(locale, "native.inbox.footerHint")),
        )
}

fn mark_all_read_button(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    button(
        "inbox-mark-all-read",
        i18n::t(locale, "native.inbox.markAllRead"),
        ButtonVariant::Secondary,
        false,
        None,
        cx.listener(|_this, _ev, _window, cx| {
            let g = cx.global::<InboxState>();
            let all_items = build_feed(g.approvals_slice(), g.activity_slice());
            let g = cx.global_mut::<InboxState>();
            for item in &all_items {
                g.read_ids.insert(item.id.clone());
            }
            cx.notify();
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(id: &str, agent: &str, summary: &str, kind: &str, created: &str, sim: Value) -> Value {
        json!({
            "id": id, "agent_id": agent, "kind": kind, "summary": summary,
            "payload": {}, "created_at": created, "ttl_seconds": 3600,
            "expires_at": 0, "simulation": sim, "channel": null, "channel_link": null,
        })
    }

    fn activity(id: &str, event_type: &str, agent: &str, summary: &str, ts: &str) -> Value {
        json!({ "id": id, "type": event_type, "agent_id": agent, "task_id": null, "summary": summary, "timestamp": ts, "metadata": null })
    }

    #[test]
    fn build_feed_puts_all_approvals_before_all_activity() {
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", Value::Null)];
        let activities = vec![activity("e1", "task_completed", "duke", "done", "2026-08-21T09:00:00Z")];
        let items = build_feed(&approvals, &activities);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].category, FeedCategory::Approval);
        assert_eq!(items[1].category, FeedCategory::Notification);
    }

    #[test]
    fn approval_row_carries_the_simulation_narrative_when_present() {
        let sim = json!({ "world_state_change": "  會付款 NT$2,180  ", "risk_points": ["不可逆"] });
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", sim)];
        let items = build_feed(&approvals, &[]);
        let exp = items[0].approval.as_ref().expect("approval row must carry ApprovalExpand");
        assert_eq!(exp.world_state_change.as_deref(), Some("會付款 NT$2,180")); // trimmed
        assert_eq!(exp.risk_points, vec!["不可逆".to_string()]);
    }

    #[test]
    fn approval_row_has_no_simulation_block_when_null() {
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", Value::Null)];
        let items = build_feed(&approvals, &[]);
        let exp = items[0].approval.as_ref().unwrap();
        assert!(exp.world_state_change.is_none());
        assert!(exp.risk_points.is_empty());
    }

    #[test]
    fn approval_row_ignores_blank_world_state_change() {
        let sim = json!({ "world_state_change": "   ", "risk_points": [] });
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", sim)];
        let items = build_feed(&approvals, &[]);
        assert!(items[0].approval.as_ref().unwrap().world_state_change.is_none());
    }

    #[test]
    fn activity_row_missing_id_is_dropped_not_panicked() {
        let activities = vec![json!({ "type": "task_completed", "summary": "x" })];
        assert!(build_feed(&[], &activities).is_empty());
    }

    #[test]
    fn classify_activity_event_routes_known_system_prefixes() {
        for ty in ["channel_recovered", "autopilot_triggered", "gvu_consolidated", "goal_loop.created", "os_watch_goal_kickoff", "playbook_rule_retired", "approval.channel_decision"] {
            assert_eq!(classify_activity_event(ty), FeedCategory::System, "expected {ty} to classify as System");
        }
    }

    #[test]
    fn classify_activity_event_defaults_unknown_types_to_notification() {
        for ty in ["task_completed", "agent.progress", "goal_intent.outcome", "os_file", "something_never_seen_before"] {
            assert_eq!(classify_activity_event(ty), FeedCategory::Notification, "expected {ty} to classify as Notification");
        }
    }

    #[test]
    fn no_activity_event_type_ever_classifies_as_mention() {
        // Pins the header comment's honesty claim: whatever the classifier
        // does, Mention is unreachable from real activity data today.
        for ty in ["task_completed", "channel_recovered", "goal_loop.created", "totally_unknown"] {
            assert_ne!(classify_activity_event(ty), FeedCategory::Mention);
        }
    }

    #[test]
    fn task_completed_activity_gets_the_done_badge_others_do_not() {
        let activities = vec![
            activity("e1", "task_completed", "duke", "done", "2026-08-21T09:00:00Z"),
            activity("e2", "agent.progress", "duke", "still going", "2026-08-21T09:05:00Z"),
        ];
        let items = build_feed(&[], &activities);
        assert_eq!(items[0].badge, Some(FeedBadge::Done));
        assert_eq!(items[1].badge, None);
    }

    #[test]
    fn approval_row_always_gets_the_pending_badge() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "2026-08-21T10:00:00Z", Value::Null)];
        assert_eq!(build_feed(&approvals, &[])[0].badge, Some(FeedBadge::Pending));
    }

    #[test]
    fn filter_feed_all_returns_everything() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "t", Value::Null)];
        let activities = vec![activity("e1", "task_completed", "duke", "s", "t")];
        let items = build_feed(&approvals, &activities);
        assert_eq!(filter_feed(&items, FilterKind::All).len(), 2);
    }

    #[test]
    fn filter_feed_approval_excludes_activity_rows() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "t", Value::Null)];
        let activities = vec![activity("e1", "task_completed", "duke", "s", "t")];
        let items = build_feed(&approvals, &activities);
        let visible = filter_feed(&items, FilterKind::Approval);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].category, FeedCategory::Approval);
    }

    #[test]
    fn filter_feed_system_excludes_notification_rows() {
        let activities = vec![
            activity("e1", "channel_recovered", "autopilot", "s", "t"),
            activity("e2", "task_completed", "duke", "s", "t"),
        ];
        let items = build_feed(&[], &activities);
        let visible = filter_feed(&items, FilterKind::System);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "activity:e1");
    }

    #[test]
    fn filter_feed_mention_is_always_empty_given_todays_data() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "t", Value::Null)];
        let activities = vec![
            activity("e1", "channel_recovered", "autopilot", "s", "t"),
            activity("e2", "task_completed", "duke", "s", "t"),
        ];
        let items = build_feed(&approvals, &activities);
        assert!(filter_feed(&items, FilterKind::Mention).is_empty());
    }

    #[test]
    fn build_decide_params_matches_handle_approvals_decide_shape() {
        // `id` required str, `approve` bool, `reason` optional str — read
        // straight from `handle_approvals_decide`'s param parsing, not
        // guessed. See this function's own doc comment.
        let v = build_decide_params("appr-1", true, "看起來沒問題");
        assert_eq!(v["id"], json!("appr-1"));
        assert_eq!(v["approve"], json!(true));
        assert_eq!(v["reason"], json!("看起來沒問題"));
    }

    #[test]
    fn build_decide_params_reject_carries_approve_false() {
        let v = build_decide_params("appr-2", false, "");
        assert_eq!(v["approve"], json!(false));
    }

    // `InboxState::request_refresh`/`InboxState::new` are NOT unit-tested
    // here — unlike `dashboard::DashboardState::new()` (no `cx` parameter,
    // directly constructible in a plain `#[test]`), `InboxState::new`
    // requires a live `gpui::App` to mint the shared `note_field` entity,
    // and this crate has no headless-`App` test harness anywhere yet (grep
    // confirms: zero `#[test]` in the whole crate constructs a `gpui::App`
    // or an `Entity<T>`). `request_refresh`'s own three-line body is simple
    // enough to read-verify directly; the smoke test (`DUDUCLAW_NATIVE_GUI_
    // DEBUG_PAGE=inbox`) is what actually exercises `InboxState::new`, not
    // a unit test — an honest gap, not a silently-skipped one.

    #[test]
    fn filter_kind_all_chip_order_matches_the_canvas() {
        assert_eq!(
            FilterKind::ALL,
            [FilterKind::All, FilterKind::Approval, FilterKind::Notification, FilterKind::Mention, FilterKind::System]
        );
    }
}
