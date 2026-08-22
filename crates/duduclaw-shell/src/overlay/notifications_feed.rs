// Notifications overlay's real approval-card state — Shell-S4 (2026-08-22,
// WP-S4-notif). Replaces the Shell-S0/S1 fake `ApprovalDecision`/
// `approval_decisions` model (index-aligned against the static
// `fake_data::APPROVAL_CARDS` — see `overlay.rs`'s pre-S4 history) with a
// dynamic, id-keyed model backed by the real gateway (`gateway_client::
// list_approvals`/`decide_approval`).
//
// Deliberately pure `&mut self` mutation with no gpui types anywhere in this
// file — same "testable without a live window" discipline `surface.rs`'s
// own header comment establishes for `SurfaceState`, extended here. The
// actual gateway I/O (`gateway_client`) is dispatched from
// `overlay/notifications.rs`'s `cx.listener`s via a `std::thread::spawn` +
// `cx.spawn` poll loop (`oobe/steps/account.rs`'s established pattern); this
// struct only ever receives already-settled results through its `apply_*`
// methods, never performs I/O itself.
//
// ── Why the home menu-bar ticker and this panel share ONE model ─────────
// `research/native-os-2026-08/shell-s4-surfaces-2026-08.md` §1.4: "通知中心
// 的『今日活動』時間軸本質上就是『值班紀錄』...兩處不要各自維護一份資料模
// 型" — the same principle applies to the menu-bar approval ticker
// (`home.rs::menu_bar_ticker`) and this panel's approval cards: both are
// views over the identical pending-approvals list, so `ShellView` owns
// exactly one `NotificationsFeed` (nested in `overlay::OverlayUiState`,
// see that struct's own doc comment) and both call sites read it, rather
// than each polling the gateway on its own schedule.
//
// ── Settled rows stay visible after the server has already forgotten them
// ─────────────────────────────────────────────────────────────────────
// `approvals.list` (`handle_approvals_list`) only ever returns PENDING rows
// (`ApprovalBroker::list_pending`) — the instant a decision lands, the next
// refresh's response simply omits that id. The research doc's own KDE
// precedent (§1.3: "決策完成後徽章留在原地...讓使用者能回頭稽核") wants the
// ✓/✗ badge to stay put for a beat rather than the card vanishing the
// moment you click — so a row the operator just decided moves into
// `decided` (capped, most-recent-first) instead of being dropped, and stays
// there for the remainder of this panel-open session even once a later
// `apply_list_ok` no longer includes it. Matches this panel's own footer
// copy (`fake_data::NOTIF_FOOTER`: "已在儀表板處理過的項目會自動消失") —
// "會消失", not "立刻消失".

use std::time::{Duration, Instant};

use crate::gateway_client::ApprovalItem;

/// Bounds how many just-decided rows stay visible in `decided` — an
/// unbounded list would grow for as long as the panel stays open across
/// many decisions in one sitting; nothing in the design board's own
/// "今天" section shows more than a handful of rows at once either.
const MAX_DECIDED_HISTORY: usize = 5;

/// How stale `last_refreshed_at` must be before `overlay/notifications.rs`'s
/// panel-open handler triggers another `approvals.list` — task brief's own
/// cadence ("30s＋開面板即刷").
pub const REFRESH_STALE_AFTER: Duration = Duration::from_secs(30);

/// Per-row interaction state. `Confirming`/`Failed` both exist for the same
/// reason: a real decision now hits `approvals.db` for good (unlike the old
/// fake in-memory toggle), so a single accidental click must not be able to
/// fire it — see `overlay/notifications.rs`'s own header comment for the
/// two-step confirm this drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDecision {
    /// Awaiting the operator's first click.
    Pending,
    /// First click landed — armed, awaiting the SECOND (confirm) click or a
    /// cancel. `approve` is which button was clicked first.
    Confirming { approve: bool },
    /// The confirm click fired; `approvals.decide` is in flight.
    InFlight { approve: bool },
    /// A `Confirming`/`InFlight` attempt for this row failed (network/RPC
    /// error, not a server-side rejection of the decision itself — see
    /// `NotificationsFeed::apply_decide_err`'s own doc comment). Clicking
    /// either button again from here re-arms `Confirming`, same as from
    /// `Pending`.
    Failed { approve: bool },
    /// `approvals.decide` succeeded. Only ever assigned by
    /// `NotificationsFeed::apply_decide_ok`, which simultaneously moves the
    /// row from `rows` into `decided` — see that fn's own doc comment.
    /// `approved` names the outcome, not "which button was clicked", the
    /// same distinction `overlay::ApprovalDecision`'s pre-S4
    /// `Approved`/`Rejected` variants drew (kept here for the badge
    /// renderer's benefit).
    Settled { approved: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRow {
    pub id: String,
    pub agent_id: String,
    pub summary: String,
    /// Precomputed once at construction (`from_item`) from `kind` +
    /// `simulation` — see that fn's own doc comment. Plain zh-TW literal
    /// fragments ("風險："/"類型："), NOT routed through `crate::i18n`: this
    /// module inherits Home/overlay's existing "content data stays
    /// hardcoded zh-TW, only NEW chrome strings get i18n'd" line (see
    /// `crate::i18n`'s own header comment on why OOBE draws that same
    /// boundary around `oobe::fake_data`) — `summary`/`agent_id` themselves
    /// are equally un-i18n'able server-generated prose, this is just one
    /// more field in that same category, not a new inconsistency.
    pub detail: String,
    pub decision: RowDecision,
}

impl ApprovalRow {
    /// Builds a display row from a freshly-listed `ApprovalItem`, always
    /// starting `Pending` — a fresh `apply_list_ok` only ever constructs
    /// rows this way, never resurrects a prior `Confirming`/`InFlight`
    /// state for the same id (see `NotificationsFeed::apply_list_ok`'s own
    /// doc comment for why that race can't happen in practice).
    fn from_item(item: ApprovalItem) -> Self {
        let detail = build_detail(&item.kind, item.simulation.as_ref());
        Self { id: item.id, agent_id: item.agent_id, summary: item.summary, detail, decision: RowDecision::Pending }
    }
}

/// `simulation`'s JSON shape mirrors `duduclaw-gateway`'s own
/// `SimulationNarrative` (`{"world_state_change": "...", "risk_points":
/// [...]}}` — see `gateway_client::ApprovalItem`'s own doc comment). Falls
/// back to a bare kind label when no simulation ran (the overwhelming
/// majority of approvals, per `handle_approvals_list`'s own comment) or its
/// `world_state_change` is empty/absent.
fn build_detail(kind: &str, simulation: Option<&serde_json::Value>) -> String {
    if let Some(sim) = simulation {
        let world = sim.get("world_state_change").and_then(serde_json::Value::as_str).unwrap_or("").trim();
        if !world.is_empty() {
            let risks: Vec<&str> =
                sim.get("risk_points").and_then(serde_json::Value::as_array).map(|arr| arr.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
            return if risks.is_empty() { world.to_string() } else { format!("{world} · 風險：{}", risks.join("、")) };
        }
    }
    format!("類型：{kind}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedStatus {
    /// Never attempted a session bootstrap yet — the panel has never been
    /// opened this run.
    Idle,
    /// A session bootstrap or a list refresh is in flight with no prior
    /// successful list ever having landed (so there is nothing to keep
    /// showing underneath a spinner).
    Loading,
    /// At least one `approvals.list` has succeeded. `rows`/`decided` below
    /// hold the actual data — this status alone doesn't say whether the
    /// list is currently empty.
    Ready,
    /// Session bootstrap or an RPC call failed — see this struct's own
    /// `apply_session_err`/`apply_list_ok` (its `Err` counterpart)
    /// doc comments for exactly which failures land here. Distinguishes
    /// "the gateway is unreachable / this session isn't allowed to
    /// auto-login" from `Loading`/`Ready` so the panel can render the
    /// honest offline banner (task brief: "誠實橫幅照 S3 示範模式前例").
    Offline,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationsFeed {
    pub status: FeedStatus,
    session_jwt: Option<String>,
    /// True while EITHER a session bootstrap, a list refresh, or a decide
    /// call is in flight — a single shared guard, not three independent
    /// ones, because this module deliberately never lets more than one
    /// gateway operation run at once (see `begin_refresh`/`begin_decide`'s
    /// own doc comments) — which is also what makes `apply_list_ok`'s
    /// "just replace `rows` wholesale" simplicity safe: a decide can never
    /// be in flight while a list refresh is also in flight, so there's no
    /// race between them to reconcile.
    busy: bool,
    last_refreshed_at: Option<Instant>,
    rows: Vec<ApprovalRow>,
    /// Most-recently-decided first, capped at `MAX_DECIDED_HISTORY` — see
    /// this file's header comment.
    decided: Vec<ApprovalRow>,
}

impl Default for NotificationsFeed {
    fn default() -> Self {
        Self { status: FeedStatus::Idle, session_jwt: None, busy: false, last_refreshed_at: None, rows: Vec::new(), decided: Vec::new() }
    }
}

impl NotificationsFeed {
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn session_jwt(&self) -> Option<&str> {
        self.session_jwt.as_deref()
    }

    /// Boolean twin of `session_jwt()` — `overlay/notifications.rs` itself
    /// never needs it (its two call sites already branch on `session_jwt()`
    /// directly, since they also need the VALUE), but it's a natural public
    /// accessor to have alongside `session_jwt()`, and is exercised by this
    /// module's own test suite below. Same "kept for whichever future
    /// caller needs it" allowance `duduclaw-native-gui/src/rpc.rs`'s
    /// `CallError::Rejected` doc comment gives for the same situation.
    #[allow(dead_code)]
    pub fn has_session(&self) -> bool {
        self.session_jwt.is_some()
    }

    /// Pending rows only — `decided` rows are done, not part of "how many
    /// things are waiting on you" (the ticker/badge count).
    pub fn pending_count(&self) -> usize {
        self.rows.len()
    }

    pub fn rows(&self) -> &[ApprovalRow] {
        &self.rows
    }

    pub fn decided(&self) -> &[ApprovalRow] {
        &self.decided
    }

    /// Single-row lookup by id — `overlay/notifications.rs`'s render loop
    /// iterates `rows()` directly rather than looking rows up one at a time,
    /// so nothing in production code calls this yet; kept as a natural
    /// public accessor (same allowance as `has_session`'s own doc comment
    /// above) and exercised extensively by this module's own tests below.
    #[allow(dead_code)]
    pub fn row(&self, id: &str) -> Option<&ApprovalRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// Whether `overlay/notifications.rs`'s panel-open handler should kick
    /// off another `approvals.list` — true when never refreshed, or when
    /// the last successful refresh is older than [`REFRESH_STALE_AFTER`].
    /// Does NOT itself check `is_busy()` — callers already guard on that
    /// separately (same split `begin_refresh`'s own doc comment describes).
    pub fn is_stale(&self) -> bool {
        match self.last_refreshed_at {
            None => true,
            Some(t) => t.elapsed() >= REFRESH_STALE_AFTER,
        }
    }

    // ── Session bootstrap ────────────────────────────────────────────────

    /// Starts a session bootstrap. Returns `false` (no-op) if one is already
    /// in flight or a session already exists — the caller's cue to skip
    /// spawning a redundant background thread.
    pub fn begin_bootstrap(&mut self) -> bool {
        if self.busy || self.session_jwt.is_some() {
            return false;
        }
        self.busy = true;
        if self.status == FeedStatus::Idle {
            self.status = FeedStatus::Loading;
        }
        true
    }

    pub fn apply_session_ok(&mut self, jwt: String) {
        self.session_jwt = Some(jwt);
        self.busy = false;
        // Status stays whatever it was (`Loading` on first boot) — the
        // caller immediately chains into `begin_refresh` on success, which
        // will resolve it to `Ready`/`Offline` itself; there's no
        // "authenticated but nothing fetched yet" state worth rendering.
    }

    /// A bootstrap failed — `SessionError` collapses to `Offline`
    /// regardless of which specific reason (`Refused`/`Unreachable`/
    /// `Http`/`Malformed`/`NonLoopback`) it was; see `gateway_client::
    /// GatewayError`'s own doc comment for why the UI layer doesn't
    /// distinguish these.
    pub fn apply_session_err(&mut self) {
        self.busy = false;
        self.status = FeedStatus::Offline;
    }

    // ── List refresh ─────────────────────────────────────────────────────

    /// Starts a list refresh. Returns `false` (no-op) if already busy or no
    /// session exists yet (the caller should bootstrap one first).
    pub fn begin_refresh(&mut self) -> bool {
        if self.busy || self.session_jwt.is_none() {
            return false;
        }
        self.busy = true;
        if self.status != FeedStatus::Ready {
            self.status = FeedStatus::Loading;
        }
        // A refresh while already `Ready` deliberately does NOT revert to
        // `Loading` — the panel keeps showing the last-known rows during a
        // silent background poll rather than flashing a spinner over data
        // it already has (see `FeedStatus::Ready`'s own doc comment).
        true
    }

    /// A fresh `approvals.list` succeeded. Replaces `rows` wholesale — safe
    /// because `busy` is a single shared guard (see that field's own doc
    /// comment): nothing can be `Confirming`/`InFlight` in the OLD `rows`
    /// while a refresh is in flight, since starting a decide would have
    /// required `begin_decide`, which itself requires `!busy`.
    pub fn apply_list_ok(&mut self, items: Vec<ApprovalItem>) {
        self.rows = items.into_iter().map(ApprovalRow::from_item).collect();
        self.busy = false;
        self.status = FeedStatus::Ready;
        self.last_refreshed_at = Some(Instant::now());
    }

    pub fn apply_list_err(&mut self) {
        self.busy = false;
        self.status = FeedStatus::Offline;
        // `rows`/`decided` are deliberately left untouched — a transient
        // refresh failure while the panel is already showing real data
        // should not blank it out; the offline banner layers ON TOP (see
        // `overlay/notifications.rs`'s render logic), it doesn't replace
        // the list.
    }

    // ── Per-row confirm / decide ─────────────────────────────────────────

    /// First click on a pending (or previously-failed) row's approve/reject
    /// button — arms the two-step confirm. Returns `false` if the row
    /// doesn't exist or isn't in a state that can be armed (already
    /// `Confirming`/`InFlight` — a second click while already armed is
    /// handled by `overlay/notifications.rs` calling `begin_decide`
    /// instead, not by re-arming). `Settled` is listed only for
    /// exhaustiveness — unreachable in practice, since a settled row has
    /// already moved out of `self.rows` into `self.decided` by the time
    /// this runs (`apply_decide_ok`), so `self.rows.iter_mut().find(..)`
    /// above can never hand back one.
    pub fn begin_confirm(&mut self, id: &str, approve: bool) -> bool {
        let Some(row) = self.rows.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        match row.decision {
            RowDecision::Pending | RowDecision::Failed { .. } => {
                row.decision = RowDecision::Confirming { approve };
                true
            }
            RowDecision::Confirming { .. } | RowDecision::InFlight { .. } | RowDecision::Settled { .. } => false,
        }
    }

    /// The confirm row's "取消" button — reverts `Confirming` back to
    /// `Pending`. A no-op if the row isn't currently `Confirming` (e.g. the
    /// cancel click raced a background refresh that already moved the row
    /// elsewhere).
    pub fn cancel_confirm(&mut self, id: &str) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            if matches!(row.decision, RowDecision::Confirming { .. }) {
                row.decision = RowDecision::Pending;
            }
        }
    }

    /// The confirm row's "確定" button. Returns the `approve` value to send
    /// to `gateway_client::decide_approval` on success, or `None` if the
    /// row wasn't `Confirming` or another operation is already in flight
    /// (same shared `busy` guard as `begin_refresh`).
    pub fn begin_decide(&mut self, id: &str) -> Option<bool> {
        if self.busy {
            return None;
        }
        let row = self.rows.iter_mut().find(|r| r.id == id)?;
        let RowDecision::Confirming { approve } = row.decision else {
            return None;
        };
        row.decision = RowDecision::InFlight { approve };
        self.busy = true;
        Some(approve)
    }

    /// `approvals.decide` succeeded — moves the row out of `rows` into the
    /// front of `decided` (trimmed to [`MAX_DECIDED_HISTORY`]). A no-op if
    /// the row is already gone (e.g. a concurrent refresh somehow already
    /// dropped it — defensive only, shouldn't happen under the single
    /// shared `busy` guard).
    pub fn apply_decide_ok(&mut self, id: &str, approve: bool) {
        self.busy = false;
        let Some(pos) = self.rows.iter().position(|r| r.id == id) else {
            return;
        };
        let mut row = self.rows.remove(pos);
        row.decision = RowDecision::Settled { approved: approve };
        self.decided.insert(0, row);
        self.decided.truncate(MAX_DECIDED_HISTORY);
    }

    /// `approvals.decide` failed (network/RPC error — a server-side
    /// rejection of the DECISION ITSELF, e.g. a stale id, still lands here
    /// too: `gateway_client::decide_approval` only ever returns `Ok(())` on
    /// a genuine `ok:true`, so anything else is "the operator's click did
    /// not take effect", which is exactly what `Failed` communicates).
    /// Leaves the row in `rows` with `Failed { approve }` so the panel can
    /// offer to retry, rather than silently reverting to `Pending` and
    /// losing which button they'd clicked.
    pub fn apply_decide_err(&mut self, id: &str, approve: bool) {
        self.busy = false;
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.decision = RowDecision::Failed { approve };
        }
    }
}

impl RowDecision {
    #[cfg(test)]
    fn is_settled(&self) -> bool {
        matches!(self, RowDecision::Settled { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, agent: &str, summary: &str) -> ApprovalItem {
        ApprovalItem { id: id.to_string(), agent_id: agent.to_string(), kind: "tool_call".to_string(), summary: summary.to_string(), created_at: "now".to_string(), simulation: None }
    }

    #[test]
    fn starts_idle_with_no_session_and_empty_lists() {
        let feed = NotificationsFeed::default();
        assert_eq!(feed.status, FeedStatus::Idle);
        assert!(!feed.has_session());
        assert!(!feed.is_busy());
        assert_eq!(feed.pending_count(), 0);
        assert!(feed.decided().is_empty());
        assert!(feed.is_stale());
    }

    #[test]
    fn begin_bootstrap_then_apply_session_ok_yields_a_session() {
        let mut feed = NotificationsFeed::default();
        assert!(feed.begin_bootstrap());
        assert!(feed.is_busy());
        assert_eq!(feed.status, FeedStatus::Loading);
        feed.apply_session_ok("jwt-1".to_string());
        assert!(!feed.is_busy());
        assert_eq!(feed.session_jwt(), Some("jwt-1"));
    }

    #[test]
    fn begin_bootstrap_is_a_noop_when_already_busy_or_already_sessioned() {
        let mut feed = NotificationsFeed::default();
        assert!(feed.begin_bootstrap());
        assert!(!feed.begin_bootstrap(), "already busy — must not double-fire");
        feed.apply_session_ok("jwt-1".to_string());
        assert!(!feed.begin_bootstrap(), "already has a session — must not re-bootstrap");
    }

    #[test]
    fn apply_session_err_goes_offline() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_err();
        assert_eq!(feed.status, FeedStatus::Offline);
        assert!(!feed.is_busy());
        assert!(!feed.has_session());
    }

    #[test]
    fn begin_refresh_requires_a_session() {
        let mut feed = NotificationsFeed::default();
        assert!(!feed.begin_refresh(), "no session yet — must refuse");
    }

    #[test]
    fn refresh_round_trip_populates_rows_and_clears_staleness() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        assert!(feed.begin_refresh());
        assert!(feed.is_busy());
        feed.apply_list_ok(vec![item("a1", "finance", "要我核准嗎？")]);
        assert!(!feed.is_busy());
        assert_eq!(feed.status, FeedStatus::Ready);
        assert_eq!(feed.pending_count(), 1);
        assert!(!feed.is_stale());
        assert_eq!(feed.row("a1").unwrap().decision, RowDecision::Pending);
    }

    #[test]
    fn a_ready_feed_refreshing_again_does_not_revert_to_loading() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);
        assert!(feed.begin_refresh());
        assert_eq!(feed.status, FeedStatus::Ready, "silent background refresh must not flash a loading state over existing data");
        assert_eq!(feed.pending_count(), 1, "old rows must stay visible while the refresh is in flight");
    }

    #[test]
    fn list_err_goes_offline_but_keeps_existing_rows() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);
        feed.begin_refresh();
        feed.apply_list_err();
        assert_eq!(feed.status, FeedStatus::Offline);
        assert_eq!(feed.pending_count(), 1, "a transient refresh failure must not blank out already-shown data");
    }

    #[test]
    fn confirm_decide_cancel_flow() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);

        assert!(feed.begin_confirm("a1", true));
        assert_eq!(feed.row("a1").unwrap().decision, RowDecision::Confirming { approve: true });

        feed.cancel_confirm("a1");
        assert_eq!(feed.row("a1").unwrap().decision, RowDecision::Pending);
    }

    #[test]
    fn double_click_while_already_confirming_does_not_rearm() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);

        assert!(feed.begin_confirm("a1", true));
        assert!(!feed.begin_confirm("a1", false), "must not silently flip which button was armed");
    }

    #[test]
    fn decide_success_moves_the_row_into_decided_settled() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);
        feed.begin_confirm("a1", true);

        let approve = feed.begin_decide("a1").expect("row was Confirming, must yield Some");
        assert!(approve);
        assert!(feed.is_busy());
        assert_eq!(feed.row("a1").unwrap().decision, RowDecision::InFlight { approve: true });

        feed.apply_decide_ok("a1", true);
        assert!(!feed.is_busy());
        assert_eq!(feed.pending_count(), 0, "settled row must leave the pending list");
        assert_eq!(feed.decided().len(), 1);
        assert!(feed.decided()[0].decision.is_settled());
    }

    #[test]
    fn begin_decide_refuses_a_row_that_is_not_confirming() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);
        assert_eq!(feed.begin_decide("a1"), None, "row is still Pending, not Confirming — must refuse");
    }

    #[test]
    fn decide_failure_marks_the_row_failed_and_keeps_it_in_the_pending_list() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);
        feed.begin_confirm("a1", false);
        feed.begin_decide("a1");

        feed.apply_decide_err("a1", false);
        assert!(!feed.is_busy());
        assert_eq!(feed.pending_count(), 1, "a failed decide must not disappear the row");
        assert_eq!(feed.row("a1").unwrap().decision, RowDecision::Failed { approve: false });
    }

    #[test]
    fn a_failed_row_can_be_re_armed_exactly_like_a_pending_one() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        feed.begin_refresh();
        feed.apply_list_ok(vec![item("a1", "finance", "q")]);
        feed.begin_confirm("a1", true);
        feed.begin_decide("a1");
        feed.apply_decide_err("a1", true);

        assert!(feed.begin_confirm("a1", true), "a Failed row must be re-armable");
    }

    #[test]
    fn decided_history_is_capped_and_most_recent_first() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        for n in 0..(MAX_DECIDED_HISTORY + 2) {
            let id = format!("a{n}");
            feed.begin_refresh();
            feed.apply_list_ok(vec![item(&id, "finance", "q")]);
            feed.begin_confirm(&id, true);
            feed.begin_decide(&id);
            feed.apply_decide_ok(&id, true);
        }
        assert_eq!(feed.decided().len(), MAX_DECIDED_HISTORY);
        // Most recently decided (`a{n-1}`) is at the front.
        assert_eq!(feed.decided()[0].id, format!("a{}", MAX_DECIDED_HISTORY + 1));
    }

    #[test]
    fn begin_decide_refuses_while_another_operation_is_already_busy() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        assert_eq!(feed.begin_decide("anything"), None, "bootstrap is still in flight — must refuse");
    }

    #[test]
    fn unknown_row_id_operations_are_noops_not_panics() {
        let mut feed = NotificationsFeed::default();
        feed.begin_bootstrap();
        feed.apply_session_ok("jwt-1".to_string());
        assert!(!feed.begin_confirm("no-such-id", true));
        feed.cancel_confirm("no-such-id");
        assert_eq!(feed.begin_decide("no-such-id"), None);
        feed.apply_decide_ok("no-such-id", true);
        feed.apply_decide_err("no-such-id", true);
        assert_eq!(feed.pending_count(), 0);
    }

    #[test]
    fn build_detail_prefers_simulation_world_state_change() {
        let sim = serde_json::json!({"world_state_change": "會寄出退款信", "risk_points": ["金額可能算錯"]});
        let detail = build_detail("tool_call", Some(&sim));
        assert!(detail.contains("會寄出退款信"));
        assert!(detail.contains("金額可能算錯"));
    }

    #[test]
    fn build_detail_falls_back_to_kind_when_no_simulation() {
        assert_eq!(build_detail("goal_kickoff", None), "類型：goal_kickoff");
    }

    #[test]
    fn build_detail_falls_back_to_kind_when_simulation_has_no_world_state_change() {
        let sim = serde_json::json!({"risk_points": []});
        assert_eq!(build_detail("mcp_install", Some(&sim)), "類型：mcp_install");
    }
}
