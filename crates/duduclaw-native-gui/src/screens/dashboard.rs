// S4b — Screen 5: the "總覽" dashboard (`nav.rs` id `home`, already labeled
// "儀表板" / "今日重點一眼看懂" — this pass wires that existing nav slot to a
// real page instead of `shell.rs`'s generic placeholder heading).
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Main.dc.html`
// (p03 儀表板) — greeting → prompt bar → 3 suggestion chips → a ≤6-card grid
// (待審批/目標進度/任務看板/本月 API 預算/通道健康/AI 員工, each a status dot +
// one action button) → a "今日活動" shelf. Every color/spacing/radius below
// reads from `theme.rs` tokens, not the canvas's literal hex — see that
// file's own doc comment for the OKLCH→sRGB source of truth. One canvas/
// token mismatch is worth flagging: the canvas's 5 non-primary card buttons
// are "white bg + border + brand-colored text", which has no matching
// `mds_gpui::ButtonVariant` (the facade is deliberately scoped to the 4
// variants `button.rs` names) — `screens/prototypes/p03_dashboard.rs`
// already made the same call for the same reason, so this page follows that
// established precedent: `Secondary` for every non-primary card action.
//
// ── Data flow (first real consumer of `ws_status::call`) ─────────────────
// `ws_status.rs`'s S4 doc comment flagged its RPC layer as "no consumer
// yet" — this page is that consumer. `maybe_fetch` is driven from the top of
// `render` (the first tick where `active_page == "home"` AND the main `/ws`
// is `Authenticated` fires seven RPCs in parallel — each a `Context::spawn`
// async block awaiting the oneshot `ws_status::call` returns, per that
// function's own doc comment on the intended calling pattern — and flips
// `DashboardState.requested` so it never re-fires on its own). A visible
// "重新整理" button is the manual-refresh path this task brief asks for when
// no server-push event exists for these aggregates (there isn't one for
// tasks/approvals/budget/channels/agents as a group).
//
// Each of the 6 cards + the activity shelf tracks its OWN `Loadable<T>` —
// loading skeleton / error text / ready content — rather than one page-wide
// three-state switch: a manager-gated call (approvals/budget/channels)
// failing on a lower-privilege session must not blank the whole page when
// the agent-scoped calls (tasks/activity/agents) still succeeded. The one
// page-level state that DOES gate everything is the WS connection itself —
// see the `connection_error` block in `render` below.
//
// ── Why state lives in a `gpui::Global`, not a `RootView` field (WP-NG-
// debt, 2026-08-21) ───────────────────────────────────────────────────────
// `DashboardState` was originally a dedicated `RootView` field (this page
// was the FIRST S4b page, landing before any other page needed the same
// shape) — every S4b page after it (`goals.rs`, `agents.rs`, `console.rs`,
// `inbox.rs`, `tasks.rs`, `about.rs`) converged on `gpui::Global` instead
// (see `goals.rs`'s own module doc comment for the original "can't touch
// `main.rs` this pass" reasoning that started the convention). This page's
// residual `RootView` field was the one holdout; converging it here removes
// the special case with zero behavior change — `maybe_fetch` moves from the
// `main.rs` poll loop into the top of `render` (the same place every other
// page's own `maybe_fetch*` already runs from), and every `state.dashboard.*`
// read becomes a `cx.default_global::<DashboardState>()` read instead.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
use crate::screens::dashboard_cards as cards;
use crate::theme;
use crate::rpc::CallError;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

/// One data source's fetch state. Deliberately per-card, not per-page — see
/// this file's module doc comment.
#[derive(Clone)]
pub enum Loadable<T> {
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(Clone, Default)]
pub struct GoalHighlight {
    pub title: String,
    pub status: String,
    pub round: i64,
}

#[derive(Clone, Default)]
pub struct GoalCard {
    pub highlight: Option<GoalHighlight>,
    pub needs_human: usize,
}

#[derive(Clone, Default)]
pub struct TasksCard {
    pub active_count: usize,
    pub needs_human: usize,
}

#[derive(Clone, Default)]
pub struct BudgetCard {
    pub spent_cents: i64,
    pub total_cents: i64,
}

#[derive(Clone, Default)]
pub struct ChannelsCard {
    pub connected: usize,
    pub total: usize,
}

#[derive(Clone, Default)]
pub struct AgentsCard {
    pub active: usize,
    pub total: usize,
}

#[derive(Clone)]
pub struct ActivityItem {
    pub agent: String,
    pub summary: String,
    pub when: String,
}

pub struct DashboardState {
    /// Set once the first fetch has been dispatched — `maybe_fetch` is a
    /// no-op after this, until [`DashboardState::request_refresh`] clears it.
    requested: bool,
    pub approvals: Loadable<usize>,
    pub goal: Loadable<GoalCard>,
    pub tasks: Loadable<TasksCard>,
    pub budget: Loadable<BudgetCard>,
    pub channels: Loadable<ChannelsCard>,
    pub agents: Loadable<AgentsCard>,
    pub activity: Loadable<Vec<ActivityItem>>,
}

/// `cx.default_global::<DashboardState>()` needs `Default` (it lazily
/// installs one on first access) — see this file's module doc comment on
/// why this page's state moved behind `gpui::Global`.
impl Default for DashboardState {
    fn default() -> Self {
        Self::new()
    }
}

impl Global for DashboardState {}

impl DashboardState {
    pub fn new() -> Self {
        Self {
            requested: false,
            approvals: Loadable::Loading,
            goal: Loadable::Loading,
            tasks: Loadable::Loading,
            budget: Loadable::Loading,
            channels: Loadable::Loading,
            agents: Loadable::Loading,
            activity: Loadable::Loading,
        }
    }

    /// The manual "重新整理" button — resets every slice back to `Loading`
    /// and clears `requested` so the next poll tick (≤100ms) re-fires all
    /// seven calls. Public: `dashboard_cards.rs`'s refresh control calls
    /// this directly from a `cx.listener`.
    pub fn request_refresh(&mut self) {
        *self = Self::new();
    }

    /// M — approvals + needs_human tasks (goal AND non-goal), mirroring the
    /// web dashboard's `overviewCounts` shape (`web/src/lib/home-overview.ts`)
    /// closely enough for a one-line greeting subtitle. `None` until all
    /// three contributing sources have resolved — a partial sum would either
    /// under-report (silently wrong) or need its own tri-state, and the
    /// subtitle already has an honest "still loading" fallback for `None`.
    fn pending_decisions(&self) -> Option<usize> {
        match (&self.approvals, &self.goal, &self.tasks) {
            (Loadable::Ready(a), Loadable::Ready(g), Loadable::Ready(t)) => {
                Some(a + g.needs_human + t.needs_human)
            }
            _ => None,
        }
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

/// Called from the top of `render` on every pass (cheap to call when it's a
/// no-op: two field reads plus an enum compare) — same placement as every
/// other S4b page's own `maybe_fetch*` (see e.g. `agents.rs::
/// maybe_fetch_list`). `render` only ever runs while `active_page == "home"`
/// (`shell.rs` only calls `dashboard::render` for that nav id), so the
/// `active_page` check below is a defensive no-op in practice, kept for
/// parity with the pre-`Global` version rather than trimmed away.
pub fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "home" || cx.default_global::<DashboardState>().requested {
        return;
    }
    // Wait for the main `/ws` to actually be authenticated — firing while
    // still `Connecting` would just immediately resolve every call
    // `NotConnected` (`ws_status.rs`'s `call()` doc comment: calls made
    // while disconnected are never queued) and `requested` would latch
    // `true` on a page that then never gets real data until a manual
    // refresh. Re-checked every render until then, so the fetch still fires
    // the moment auth completes.
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<DashboardState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "approvals.list", json!({}), |cx, result| {
        cx.default_global::<DashboardState>().approvals = result.map(|v| {
            v.get("count").and_then(Value::as_u64).unwrap_or(0) as usize
        }).into();
    });
    spawn_call(cx, tx.clone(), "tasks.list", json!({"goal_mode": true}), |cx, result| {
        cx.default_global::<DashboardState>().goal = result.map(|v| parse_goal_card(&v)).into();
    });
    spawn_call(cx, tx.clone(), "tasks.list", json!({"goal_mode": false}), |cx, result| {
        cx.default_global::<DashboardState>().tasks = result.map(|v| parse_tasks_card(&v)).into();
    });
    spawn_call(cx, tx.clone(), "accounts.budget_summary", json!({}), |cx, result| {
        cx.default_global::<DashboardState>().budget = result.map(|v| BudgetCard {
            spent_cents: v.get("total_spent_cents").and_then(Value::as_i64).unwrap_or(0),
            total_cents: v.get("total_budget_cents").and_then(Value::as_i64).unwrap_or(0),
        }).into();
    });
    spawn_call(cx, tx.clone(), "channels.status", json!({}), |cx, result| {
        cx.default_global::<DashboardState>().channels = result.map(|v| parse_channels_card(&v)).into();
    });
    spawn_call(cx, tx.clone(), "agents.list", json!({}), |cx, result| {
        cx.default_global::<DashboardState>().agents = result.map(|v| parse_agents_card(&v)).into();
    });
    spawn_call(cx, tx, "activity.list", json!({"limit": 8}), |cx, result| {
        cx.default_global::<DashboardState>().activity = result.map(|v| parse_activity(&v)).into();
    });
}

impl<T> From<Result<T, String>> for Loadable<T> {
    fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(v) => Loadable::Ready(v),
            Err(e) => Loadable::Failed(e),
        }
    }
}

/// One RPC round trip: dispatch `method`, await the response, hand the
/// (payload | description) back to `apply` against `Context<RootView>`
/// (which is how a `Global`-backed `apply` reaches `cx.default_global::
/// <DashboardState>()` — mirrors `goals.rs::spawn_goal_call`'s identical
/// shape, kept as this page's own copy rather than a cross-module import so
/// this pass's diff stays scoped to the dashboard page). Generic over
/// `apply` (not boxed) — monomorphized per call site, matching this crate's
/// existing style of avoiding `dyn` where a concrete closure works.
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
            // The oneshot's sender was dropped without ever resolving —
            // only possible if the whole background manager thread died.
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
        // `error_response("", &msg)` (the server's own helper, see
        // `protocol.rs`) always wraps the message as a plain JSON string —
        // every RPC this page calls uses that helper, never a structured
        // `{code,message}` error object, so `as_str()` is the correct read,
        // not a lossy guess. Falls back to the raw JSON text for the one
        // hypothetical case a future handler ever changes that shape.
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Response parsing (`tasks.list` / `channels.status` / `agents.list` /
// `activity.list` shapes read directly from `duduclaw-gateway/src/
// handlers.rs` — see this crate's S4b report for the exact line numbers) ──

/// Goal-mode tasks (`tasks.list {goal_mode:true}` → `{"tasks":[...]}`,
/// `task_row_to_json` fields). Picks one task to headline the card: prefer
/// `needs_human` (matches the canvas's "卡在需要你" copy) over an
/// actively-looping task over a not-yet-started one over a stuck
/// blocked/failed one — `done` is never a highlight candidate. Ties within a
/// tier break on `updated_at` (ISO 8601 strings sort correctly
/// lexicographically), most recent first.
fn parse_goal_card(v: &Value) -> GoalCard {
    let tasks = v.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default();
    let needs_human = tasks.iter().filter(|t| status_of(t) == "needs_human").count();

    fn tier(status: &str) -> Option<u8> {
        match status {
            "needs_human" => Some(0),
            "in_progress" | "revising" | "review" => Some(1),
            "todo" | "pending" => Some(2),
            "blocked" | "failed" => Some(3),
            _ => None, // `done` and anything unrecognized never headline
        }
    }

    let mut best: Option<(u8, &str, &Value)> = None;
    for t in &tasks {
        let status = status_of(t);
        let Some(tier) = tier(status) else { continue };
        let updated = t.get("updated_at").and_then(Value::as_str).unwrap_or("");
        let better = match &best {
            None => true,
            Some((best_tier, best_updated, _)) => {
                tier < *best_tier || (tier == *best_tier && updated > *best_updated)
            }
        };
        if better {
            best = Some((tier, updated, t));
        }
    }

    let highlight = best.map(|(_, _, t)| GoalHighlight {
        title: t.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        status: status_of(t).to_string(),
        round: t.get("revision_round").and_then(Value::as_i64).unwrap_or(0),
    });

    GoalCard { highlight, needs_human }
}

/// Non-goal tasks (`tasks.list {goal_mode:false}`). No due-date field exists
/// anywhere in `TaskRow`/`task_row_to_json` (confirmed by reading both) —
/// the canvas's "今天到期 4 項" copy has no backing data, so this counts
/// everything still on the board (`status != "done"`) instead. Deliberate
/// deviation from the canvas's literal number, documented rather than
/// fabricated; the i18n string was written to match (`tasks.detail`: "{n}
/// 項進行中或待處理", not "到期").
fn parse_tasks_card(v: &Value) -> TasksCard {
    let tasks = v.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default();
    let active_count = tasks.iter().filter(|t| status_of(t) != "done").count();
    let needs_human = tasks.iter().filter(|t| status_of(t) == "needs_human").count();
    TasksCard { active_count, needs_human }
}

fn status_of(task: &Value) -> &str {
    task.get("status").and_then(Value::as_str).unwrap_or("")
}

fn parse_channels_card(v: &Value) -> ChannelsCard {
    let channels = v.get("channels").and_then(Value::as_array).cloned().unwrap_or_default();
    let total = channels.len();
    let connected = channels
        .iter()
        .filter(|c| c.get("connected").and_then(Value::as_bool).unwrap_or(false))
        .count();
    ChannelsCard { connected, total }
}

/// `agents.list` exposes each agent's config-level `status`
/// (active/paused/terminated/archived — `AgentStatus`, `duduclaw-core/src/
/// types.rs`), not a live heartbeat/online signal (no such field is
/// serialized). "在線/離線" below is therefore `status == "active"` vs
/// everything else the endpoint still lists (archived agents are already
/// excluded server-side by default) — an honest approximation from the only
/// data available, not a real presence check. Documented in the i18n
/// string's own surrounding card copy rather than silently overclaiming.
fn parse_agents_card(v: &Value) -> AgentsCard {
    let agents = v.get("agents").and_then(Value::as_array).cloned().unwrap_or_default();
    let total = agents.len();
    let active = agents
        .iter()
        .filter(|a| a.get("status").and_then(Value::as_str) == Some("active"))
        .count();
    AgentsCard { active, total }
}

fn parse_activity(v: &Value) -> Vec<ActivityItem> {
    let events = v.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    events
        .iter()
        .map(|e| ActivityItem {
            agent: e.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
            summary: e.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
            when: short_time(e.get("timestamp").and_then(Value::as_str).unwrap_or("")),
        })
        .collect()
}

/// `"2026-08-21T13:45:00Z"` → `"13:45"`. ISO 8601 is pure ASCII, so a byte
/// slice can never land mid-character — ordinary `&s[..n]` is safe here
/// specifically because of that (the crate-wide `truncate_bytes` convention
/// exists for the general "arbitrary string" case, which this is not).
fn short_time(ts: &str) -> String {
    if ts.len() >= 16 && ts.as_bytes().get(10) == Some(&b'T') {
        ts[11..16].to_string()
    } else {
        ts.to_string()
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

/// `web/src/pages/HomePage.tsx`'s `greetingBucket` thresholds, copied
/// verbatim (same product, same convention) — hour-of-day → greeting key.
fn greeting_key(hour: u32) -> &'static str {
    if (5..12).contains(&hour) {
        "native.home.greeting.morning"
    } else if (12..18).contains(&hour) {
        "native.home.greeting.afternoon"
    } else if (18..23).contains(&hour) {
        "native.home.greeting.evening"
    } else {
        "native.home.greeting.night"
    }
}

/// Local hour-of-day without pulling in a chrono dependency this crate
/// doesn't otherwise need — `SystemTime` since-epoch seconds mod 86400,
/// offset by the platform's UTC offset via `chrono`-free `libc`-free means
/// is not available portably, so this reads the OS local time the one way
/// `std` exposes: `std::time` has no timezone concept at all. Honest scope
/// cut: the greeting bucket uses UTC, not the user's local wall clock, on
/// any machine whose UTC offset crosses a bucket boundary differently than
/// local time would — flagged here rather than silently assumed correct;
/// low stakes (a wrong greeting bucket, not a wrong data value).
fn current_hour_utc() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ((secs / 3600) % 24) as u32
}

/// Web's `greetingName`: an empty/placeholder display name falls back to a
/// warm generic greeting rather than showing a raw DB seed value like
/// "Administrator" (`web/src/pages/HomePage.tsx`'s own doc comment records
/// the 2026-08-04 client report this guards against).
const PLACEHOLDER_DISPLAY_NAME: &str = "Administrator";

fn greeting_name(display_name: Option<&str>, locale: Locale) -> SharedString {
    let name = display_name.unwrap_or("").trim();
    if name.is_empty() || name == PLACEHOLDER_DISPLAY_NAME {
        i18n::t(locale, "native.home.greeting.fallbackName")
    } else {
        name.to_string().into()
    }
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    // Page-level error state: the main `/ws` has never authenticated. Every
    // card would otherwise sit on a `Loading` skeleton forever (`maybe_fetch`
    // deliberately waits for `Authenticated` before firing) — this is the
    // "斷線/失敗顯示可重試" state the task brief asks for; `ws_status.rs`
    // already auto-reconnects in the background, so there is nothing this
    // page needs to trigger itself, only something honest to show while it
    // waits.
    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("home-page")
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

    let greeting = i18n::t1(
        locale,
        greeting_key(current_hour_utc()),
        "name",
        &greeting_name(state.display_name.as_deref(), locale),
    );
    let subtitle = match cx.default_global::<DashboardState>().pending_decisions() {
        None => i18n::t(locale, "native.home.greeting.subtitleLoading"),
        Some(0) => i18n::t(locale, "native.home.greeting.subtitleAllClear"),
        Some(n) => i18n::t1(locale, "native.home.greeting.subtitlePending", "count", &n.to_string()),
    };

    let mut chip_rows = Vec::with_capacity(3);
    for (id, key) in [
        ("home-chip-todo", "native.home.chip.todo"),
        ("home-chip-report", "native.home.chip.report"),
        ("home-chip-goal", "native.home.chip.newGoal"),
    ] {
        chip_rows.push(cards::chip(id, i18n::t(locale, key), cx));
    }

    let card_rows = vec![
        cards::approvals_card(state, cx),
        cards::goal_card(state, cx),
        cards::tasks_card(state, cx),
        cards::budget_card(state, cx),
        cards::channels_card(state, cx),
        cards::agents_card(state, cx),
    ];

    div()
        .id("home-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .gap_5()
        .p_6()
        // ── Greeting + manual refresh ─────────────────────────────────
        .child(
            div()
                .w_full()
                .max_w(px(920.))
                .flex()
                .items_start()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .flex_1()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XL))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(greeting),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child(subtitle),
                        ),
                )
                .child(cards::refresh_button(locale, cx)),
        )
        // ── Prompt-first input (shares `state.chat`'s composer entity —
        // see `cards::prompt_bar`'s doc comment for why) ────────────────
        .child(cards::prompt_bar(state, locale, cx))
        .child(div().flex().flex_wrap().gap_2().justify_center().children(chip_rows))
        // ── ≤6 interactive cards ─────────────────────────────────────
        .child(div().w_full().max_w(px(920.)).grid().grid_cols(3).gap_3().children(card_rows))
        // ── Today's activity shelf ───────────────────────────────────
        .child(cards::activity_shelf(locale, cx))
}

/// Shared skeleton row for a card's value area while its `Loadable` is
/// still `Loading` — `dashboard_cards.rs` calls this for every card.
pub(super) fn loading_row() -> Div {
    div().flex().flex_col().gap_1p5().child(skeleton(px(120.), px(18.))).child(skeleton(px(70.), px(12.)))
}

pub(super) fn error_row(locale: Locale, message: &str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
        .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", message))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tasks.list {goal_mode:true}` shape, verbatim field names read from
    /// `handlers.rs::task_row_to_json` (`crates/duduclaw-gateway/src/
    /// handlers.rs`) — no live gateway session was available to this pass
    /// (see this crate's S4b report), so this is the honest substitute: a
    /// hand-built fixture matching the source-read shape exactly, guarding
    /// against a field-name typo silently degrading every card to "empty".
    fn goal_task(status: &str, updated_at: &str, title: &str, round: i64) -> Value {
        json!({ "status": status, "updated_at": updated_at, "title": title, "revision_round": round })
    }

    #[test]
    fn goal_card_prefers_needs_human_over_active_over_todo_over_blocked() {
        let v = json!({ "tasks": [
            goal_task("todo", "2026-08-20T10:00:00Z", "todo goal", 0),
            goal_task("blocked", "2026-08-20T12:00:00Z", "blocked goal", 1),
            goal_task("in_progress", "2026-08-20T09:00:00Z", "active goal", 2),
            goal_task("needs_human", "2026-08-20T08:00:00Z", "stuck goal", 3),
            goal_task("done", "2026-08-20T23:00:00Z", "finished goal", 9),
        ]});
        let card = parse_goal_card(&v);
        assert_eq!(card.needs_human, 1);
        let h = card.highlight.expect("expected a highlight");
        assert_eq!(h.title, "stuck goal");
        assert_eq!(h.status, "needs_human");
        assert_eq!(h.round, 3);
    }

    #[test]
    fn goal_card_ties_within_a_tier_break_on_most_recent_updated_at() {
        let v = json!({ "tasks": [
            goal_task("in_progress", "2026-08-20T09:00:00Z", "older", 0),
            goal_task("in_progress", "2026-08-20T15:00:00Z", "newer", 0),
        ]});
        let card = parse_goal_card(&v);
        assert_eq!(card.highlight.unwrap().title, "newer");
    }

    #[test]
    fn goal_card_done_only_never_highlights_and_reports_empty() {
        let v = json!({ "tasks": [goal_task("done", "2026-08-20T10:00:00Z", "finished", 0)] });
        let card = parse_goal_card(&v);
        assert!(card.highlight.is_none());
        assert_eq!(card.needs_human, 0);
    }

    #[test]
    fn goal_card_empty_tasks_array_is_empty_card() {
        let card = parse_goal_card(&json!({ "tasks": [] }));
        assert!(card.highlight.is_none());
        assert_eq!(card.needs_human, 0);
    }

    #[test]
    fn tasks_card_counts_everything_but_done_as_active() {
        let v = json!({ "tasks": [
            { "status": "todo" }, { "status": "in_progress" }, { "status": "blocked" },
            { "status": "needs_human" }, { "status": "done" }, { "status": "done" },
        ]});
        let card = parse_tasks_card(&v);
        assert_eq!(card.active_count, 4);
        assert_eq!(card.needs_human, 1);
    }

    #[test]
    fn channels_card_counts_connected_of_total() {
        let v = json!({ "channels": [
            { "name": "telegram", "connected": true },
            { "name": "discord", "connected": false },
            { "name": "line", "connected": true },
        ]});
        let card = parse_channels_card(&v);
        assert_eq!(card.connected, 2);
        assert_eq!(card.total, 3);
    }

    #[test]
    fn channels_card_missing_array_is_zero_not_a_panic() {
        let card = parse_channels_card(&json!({}));
        assert_eq!((card.connected, card.total), (0, 0));
    }

    #[test]
    fn agents_card_only_active_status_counts_as_online() {
        let v = json!({ "agents": [
            { "name": "a", "status": "active" },
            { "name": "b", "status": "paused" },
            { "name": "c", "status": "terminated" },
            { "name": "d", "status": "active" },
        ]});
        let card = parse_agents_card(&v);
        assert_eq!(card.active, 2);
        assert_eq!(card.total, 4);
    }

    #[test]
    fn activity_parses_agent_summary_and_short_time() {
        let v = json!({ "events": [
            { "agent_id": "customer-service", "summary": "回覆了 3 則客戶訊息", "timestamp": "2026-08-21T13:45:07Z" },
        ]});
        let items = parse_activity(&v);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].agent, "customer-service");
        assert_eq!(items[0].summary, "回覆了 3 則客戶訊息");
        assert_eq!(items[0].when, "13:45");
    }

    #[test]
    fn short_time_falls_back_to_the_raw_string_when_not_iso8601() {
        assert_eq!(short_time("not-a-timestamp"), "not-a-timestamp");
        assert_eq!(short_time(""), "");
    }

    #[test]
    fn describe_call_error_reads_the_plain_string_error_response_shape() {
        // `protocol.rs::WsFrame::error_response` always wraps the message as
        // a bare JSON string (verified by reading the source, see
        // `describe_call_error`'s own doc comment) — this pins that
        // assumption so a future protocol change trips a test, not a
        // silently-blank error card.
        let msg = describe_call_error(&CallError::Rejected(Value::String("權限不足".to_string())));
        assert_eq!(msg, "權限不足");
    }

    #[test]
    fn pending_decisions_is_none_until_all_three_sources_resolve() {
        let mut state = DashboardState::new();
        assert_eq!(state.pending_decisions(), None);
        state.approvals = Loadable::Ready(2);
        assert_eq!(state.pending_decisions(), None);
        state.goal = Loadable::Ready(GoalCard { highlight: None, needs_human: 1 });
        assert_eq!(state.pending_decisions(), None);
        state.tasks = Loadable::Ready(TasksCard { active_count: 5, needs_human: 3 });
        assert_eq!(state.pending_decisions(), Some(2 + 1 + 3));
    }

    #[test]
    fn pending_decisions_all_clear_is_explicitly_zero_not_none() {
        let mut state = DashboardState::new();
        state.approvals = Loadable::Ready(0);
        state.goal = Loadable::Ready(GoalCard { highlight: None, needs_human: 0 });
        state.tasks = Loadable::Ready(TasksCard { active_count: 0, needs_human: 0 });
        assert_eq!(state.pending_decisions(), Some(0));
    }

    #[test]
    fn request_refresh_resets_every_slice_and_the_requested_flag() {
        let mut state = DashboardState::new();
        state.requested = true;
        state.approvals = Loadable::Ready(3);
        state.request_refresh();
        assert!(!state.requested);
        assert!(matches!(state.approvals, Loadable::Loading));
    }

    #[test]
    fn greeting_bucket_thresholds_match_the_web_dashboard() {
        assert_eq!(greeting_key(5), "native.home.greeting.morning");
        assert_eq!(greeting_key(11), "native.home.greeting.morning");
        assert_eq!(greeting_key(12), "native.home.greeting.afternoon");
        assert_eq!(greeting_key(17), "native.home.greeting.afternoon");
        assert_eq!(greeting_key(18), "native.home.greeting.evening");
        assert_eq!(greeting_key(22), "native.home.greeting.evening");
        assert_eq!(greeting_key(23), "native.home.greeting.night");
        assert_eq!(greeting_key(4), "native.home.greeting.night");
    }

    #[test]
    fn greeting_name_falls_back_on_empty_and_on_the_db_placeholder() {
        assert_eq!(greeting_name(None, Locale::En), "there");
        assert_eq!(greeting_name(Some(""), Locale::En), "there");
        assert_eq!(greeting_name(Some("  "), Locale::En), "there");
        assert_eq!(greeting_name(Some("Administrator"), Locale::En), "there");
        assert_eq!(greeting_name(Some("Louis"), Locale::En), "Louis");
    }
}
