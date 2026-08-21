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
//
// ── `inbox_data.rs` split (WP-NG-debt, 2026-08-21) ───────────────────────
// The pure feed model (`FeedCategory`/`FilterKind`/`FeedBadge`/
// `ApprovalExpand`/`FeedItem`, `build_feed`/`filter_feed`/
// `build_decide_params`, and their unit tests) moved to `inbox_data.rs` —
// same "types + pure parsing/filtering, zero dependency on state/fetch/
// render" split `goals_data.rs` already established for `goals.rs`, done
// here purely to bring this file back under this crate's own <800-line
// convention. This file keeps the state/fetch/render layer: `InboxState`,
// `ensure_state`, `maybe_fetch`, `spawn_call`, `dispatch_decide`, `render`.
// No behavior differs from an unsplit version.

use std::collections::HashSet;

use gpui::{div, prelude::*, px, App, Context, Div, Entity, Global, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::screens::inbox_data::{build_decide_params, build_feed, filter_feed, FilterKind};
use crate::screens::inbox_rows as rows;
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

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
    ///
    /// Neither this constructor nor [`InboxState::request_refresh`] is
    /// unit-tested — unlike `dashboard::DashboardState::new()` (no `cx`
    /// parameter, directly constructible in a plain `#[test]`), this `new`
    /// requires a live `gpui::App` to mint the shared `note_field` entity,
    /// and this crate has no headless-`App` test harness anywhere yet (grep
    /// confirms: zero `#[test]` in the whole crate constructs a `gpui::App`
    /// or an `Entity<T>`). `request_refresh`'s own three-line body is simple
    /// enough to read-verify directly; the smoke test (`DUDUCLAW_NATIVE_GUI_
    /// DEBUG_PAGE=inbox`) is what actually exercises this constructor, not a
    /// unit test — an honest gap, not a silently-skipped one.
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
