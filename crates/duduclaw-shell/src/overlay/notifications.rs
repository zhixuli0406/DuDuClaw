// Notifications overlay content — Shell-S0 round 2, dark theme in Shell-S1,
// real gateway data in Shell-S4 (2026-08-22, WP-S4-notif).
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Notifications.dc.html`
// (light) / `commercial/design/duduclaw-os-home-dark/Notifications.dc.html`
// (dark) — a right-docked floating panel (`top:40px; right:12px;
// bottom:16px; width:390px`, no explicit height: CSS-style top+bottom
// anchoring stretches it to fill, reproduced the same way with gpui's own
// `.top()`/`.right()`/`.bottom()` absolute setters and no `.h(...)` call).
//
// ── Shell-S4: approval cards are the "審批第一公民" against REAL data ────
// The two approval cards' 核准/駁回 buttons used to write into a fake,
// index-aligned `overlay::ApprovalDecision` toggle. They now drive the real
// `gateway_client::decide_approval` RPC through `overlay::notifications_feed
// ::NotificationsFeed` — a dynamic, id-keyed list from `approvals.list`, not
// two hardcoded design-board examples. Because a click here now hits
// `approvals.db` for good (unlike the old in-memory-only toggle), every
// approve/reject is a TWO-STEP confirm (first click arms `Confirming`,
// shows "確定"/"取消"; the confirm click actually fires the RPC) —
// `notifications_feed::RowDecision`'s own doc comment has the full state
// machine. This is a deliberate addition beyond the design board (which
// only ever shows the single-click Pending/Approved/Rejected states) to
// guard against an accidental click on a real, hard-to-undo decision; the
// research doc for this round (`research/native-os-2026-08/
// shell-s4-surfaces-2026-08.md` §1.3) confirms button-based (not swipe)
// interaction is right but doesn't itself prescribe a confirm step — this
// module adds one anyway given what a click now actually does.
//
// ── Three connectivity states ─────────────────────────────────────────
// `NotificationsFeed::status` (Idle/Loading/Ready/Offline) drives the
// panel's top region: a spinner-ish loading line, an honest "無法連線"
// banner (task brief: "誠實橫幅照 S3 示範模式前例" — same
// `Key::NetworkDemoModeNotice`-style honesty this crate already established
// for OOBE's demo Wi-Fi backend), or the real row list (which itself may be
// empty — a fourth, distinct rendering, not folded into any of the other
// three). `content()` below picks exactly one of these.
//
// ── Panel-open triggers a refresh; a periodic poll keeps it warm ────────
// `open_and_refresh` (called from `home.rs`'s two "open Notifications"
// click sites, replacing their old bare `view.surface.open(...)`) kicks off
// a session bootstrap + list fetch the first time, or a fresh list fetch
// when the last one is older than `notifications_feed::REFRESH_STALE_AFTER`
// (task brief cadence: "30s＋開面板即刷"). `render()` below additionally
// makes sure the ONE periodic checker is running (`schedule_stale_check`)
// so the pending list and the home menu-bar ticker both stay live without
// the operator needing to close and reopen the panel.
//
// ── WP-A4-4 (2026-08-22): the appliance VM's 429 storm ──────────────────
// On the real 值班機, `journalctl -u duduclaw-kiosk` showed six-plus
// identical `approvals RPC failed: ... 429 Too Many Requests` lines PER
// SECOND, and `cage` (whose only client is this shell) sat at ~100% CPU
// with a static screen. Three defects compounded, all fixed this round:
//   1. `schedule_stale_check` armed a NEW one-shot timer on every render
//      pass while relying on repaints for continuity, and the refresh it
//      triggers itself causes repaints — so the pending-timer count only
//      ever grew. Now: one claimed slot + a self-re-arming loop.
//   2. `NotificationsFeed::apply_list_err` left `last_refreshed_at`
//      untouched, so `is_stale()` was permanently true after the first
//      failure and every timer that landed while no fetch was in flight
//      fired another one immediately. Now: `overlay::notifications_backoff`
//      (exponential + jitter, 429 gets a much longer base, 60s ceiling).
//   3. Every failure printed a line. Now: first-of-streak, kind changes,
//      and powers of two, with the suppressed count carried forward.
// A fourth, smaller change removes the repaints that a background poll
// caused on surfaces that draw nothing affected by it — see
// `trigger_refresh_if_stale`'s own comment on the busy dot.
//
// `cx: &mut Context<ShellView>` is threaded through via a plain `for` loop
// when building approval cards, NOT `.iter().map(...)` — this crate's own
// `duduclaw-native-gui` sibling already hit and documented this exact wall
// (`main.rs`'s gotcha list: "A `&mut Context<V>` is NOT `Copy`, so it cannot
// be captured into a closure that runs more than once ... use a plain `for`
// loop instead"), so the workaround is applied here from the start rather
// than rediscovered.
//
// ── Dark theme (Shell-S1) ─────────────────────────────────────────
// Every color below now resolves through `palette: ShellPalette` — see
// `crate::palette`'s own header comment. Same dark-only panel-root
// `.text_color(...)` fallback `overlay/launcher.rs`'s own header comment
// documents (this panel's root never set one either — verified against the
// original code) applies here too, for the identical reason.

use std::sync::mpsc;
use std::time::Duration;

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, App, ClickEvent, Context, Div, FontWeight, Rgba, Stateful, Window};

use duduclaw_native_gui::theme;

use super::notifications_backoff::{FailureKind, LogVerdict};
use super::notifications_feed::{ApprovalRow, FeedStatus, NotificationsFeed, RowDecision};
use super::OverlayUiState;
use crate::gateway_client::{self, ApprovalItem};
use crate::i18n::{t, Key, Locale};
use crate::icons;
use crate::palette::ShellPalette;
use crate::surface::Overlay;
use crate::{fake_data, ShellView};

/// Poll cadence for the background thread <-> `cx.spawn` bridge — same
/// value `oobe/steps/account.rs`'s own one-shot poll loop uses.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `home.rs`'s two "open Notifications" click sites call this INSTEAD of a
/// bare `view.surface.open(Overlay::Notifications)` — opens the overlay and
/// (if warranted — see `trigger_refresh_if_stale`) kicks off the real fetch,
/// satisfying the task brief's "開面板即刷". `pub(crate)` since `home.rs` is
/// a sibling module of `overlay`, not a descendant of it.
pub(crate) fn open_and_refresh(view: &mut ShellView, cx: &mut Context<ShellView>) {
    view.surface.open(Overlay::Notifications);
    trigger_refresh_if_stale(view, cx);
}

/// Dispatches a background-thread fetch (session bootstrap, if needed, then
/// `approvals.list`) when the feed is stale and nothing is already in
/// flight. Called from `open_and_refresh` above AND from `content()`'s own
/// periodic timer while the panel stays open — both call sites share this
/// one function so "just opened" and "been open a while" behave identically.
/// `pub(crate)` (Shell-S4-lock, 2026-08-22): the lockscreen surface
/// (`lockscreen/render.rs`, a SIBLING module of `overlay`) reads the SAME
/// `NotificationsFeed` for its own "N 件等你決定" summary card — see that
/// struct's own header comment on why the two surfaces share one model
/// rather than each polling independently — and needs to kick off the exact
/// same refresh-if-stale dispatch while locked, since Home (and therefore
/// this panel's own `content()`) isn't rendered at all during a lock.
pub(crate) fn trigger_refresh_if_stale(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let feed = &mut view.overlay_ui.notifications;
    if !feed.is_stale() {
        return;
    }
    let existing_jwt = feed.session_jwt().map(str::to_string);
    let started = match &existing_jwt {
        Some(_) => feed.begin_refresh(),
        None => feed.begin_bootstrap(),
    };
    if !started {
        // Already busy (a fetch or a decide is in flight) — the task
        // brief's 30s cadence will catch the next window; no need to queue
        // a second attempt behind this one.
        return;
    }
    // WP-A4-4 "無變化不 notify": the ONLY thing this state change draws is
    // the panel header's busy dot (`header`'s `is_busy` argument), and the
    // panel is the only surface that draws it. Home's menu-bar ticker and
    // the lockscreen summary card both read `pending_count()`, which a
    // dispatch cannot have changed yet. Repainting them here was one of the
    // two renders per poll cycle that fed the appliance VM's timer pile-up
    // (see `schedule_stale_check` below) — with the panel closed, this
    // whole cycle is now repaint-free unless the DATA actually moves.
    if notifications_panel_open(view) {
        cx.notify();
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = fetch_once(existing_jwt);
        let _ = tx.send(outcome);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    let changed = apply_fetch_outcome(&mut view.overlay_ui.notifications, outcome);
                    // Same rule as the dispatch-side notify above, plus the
                    // panel's own busy dot going back OFF (which only the
                    // panel draws, and only while it is open).
                    if changed || notifications_panel_open(view) {
                        cx.notify();
                    }
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    })
    .detach();
}

/// Is the Notifications panel the overlay currently on screen? `ShellView.
/// surface` is a private field, but this module is a DESCENDANT of the
/// crate root where `ShellView` is defined, so it is in scope here — the
/// same access `open_and_refresh` above already relies on.
fn notifications_panel_open(view: &ShellView) -> bool {
    view.surface.overlay() == Some(Overlay::Notifications)
}

/// Runs entirely on a background `std::thread` — never called from gpui's
/// own executor. Blocking calls only (`gateway_client`'s own contract).
/// `Ok` carries `new_jwt` (`Some` only when THIS call bootstrapped a fresh
/// session — `None` means an existing one was reused) alongside the fetched
/// list, so `apply_fetch_outcome` below can persist the session before
/// applying the list. A list failure right after a fresh bootstrap
/// deliberately discards that new token rather than persisting it on a
/// partial success — bootstrap is a single cheap POST, and re-bootstrapping
/// on the next stale check is simpler than a three-way partial-success
/// state to reconcile. Diagnostics are logged once, by the caller
/// (`apply_fetch_outcome`), not here — this fn stays a plain data-in/
/// data-out function so it doesn't have to know it's running off-thread.
fn fetch_once(existing_jwt: Option<String>) -> Result<(Option<String>, Vec<ApprovalItem>), gateway_client::GatewayError> {
    let (jwt, new_jwt) = match existing_jwt {
        Some(jwt) => (jwt, None),
        None => {
            let jwt = gateway_client::bootstrap_local_session()?;
            (jwt.clone(), Some(jwt))
        }
    };
    let items = gateway_client::list_approvals(&jwt)?;
    Ok((new_jwt, items))
}

/// Applies one settled fetch and emits (at most) one stderr line for it.
/// Returns whether anything a surface actually DRAWS changed — see
/// `NotificationsFeed::apply_list_ok`'s own doc comment for why `busy`
/// deliberately isn't part of that answer.
///
/// ── Log denoise (WP-A4-4) ──────────────────────────────────────────────
/// The appliance VM's journal carried six-plus IDENTICAL failure lines per
/// second for minutes on end. The frequency itself is fixed by the backoff
/// plus the timer-arm guard, but a wedged gateway would still produce one
/// line per retry forever, so `notifications_backoff::RefreshBackoff::
/// record_failure`'s verdict gates the line too: first failure of a streak,
/// any change of failure kind, then powers of two. Suppressed failures are
/// COUNTED and reported by the next emitted line — this is "stop repeating
/// yourself", never "swallow it" (5.誠實回報). Recovery gets exactly one
/// line, and only when there was a streak to recover from.
fn apply_fetch_outcome(feed: &mut NotificationsFeed, outcome: Result<(Option<String>, Vec<ApprovalItem>), gateway_client::GatewayError>) -> bool {
    match outcome {
        Ok((new_jwt, items)) => {
            if let Some(jwt) = new_jwt {
                feed.apply_session_ok(jwt);
            }
            let ok = feed.apply_list_ok(items);
            if let Some(streak) = ok.recovered {
                eprintln!("[notifications] gateway reachable again after {streak} consecutive failure(s)");
            }
            ok.changed
        }
        // Distinguishes "never got a session at all" from "had one, but the
        // RPC itself failed" only for the stderr diagnostic's own wording —
        // both still land the panel on the same Offline banner either way
        // (`NotificationsFeed::apply_session_err`/`::apply_list_err` — see
        // `gateway_client::GatewayError`'s own doc comment for why the UI
        // layer doesn't need the distinction beyond that).
        Err(err) => {
            let kind = FailureKind::classify(&err);
            let outcome = match &err {
                gateway_client::GatewayError::Session(_) => feed.apply_session_err(kind),
                gateway_client::GatewayError::Rpc(_) => feed.apply_list_err(kind),
            };
            if let LogVerdict::Emit { suppressed } = outcome.log {
                let what = match &err {
                    gateway_client::GatewayError::Session(e) => format!("local session unavailable: {e:?}"),
                    gateway_client::GatewayError::Rpc(e) => format!("approvals RPC failed: {e:?}"),
                };
                let quiet = if suppressed > 0 { format!(", {suppressed} identical failure(s) not logged") } else { String::new() };
                eprintln!("[notifications] {what} (failure #{}{quiet}; next retry in ~{:?})", outcome.failures, feed.next_check_delay());
            }
            outcome.changed
        }
    }
}

pub(super) fn render(
    ui: &OverlayUiState,
    // D6 (2026-08-23): third-party app notifications delivered over
    // `org.freedesktop.Notifications` (`crate::notifyd`). A SECOND feed in
    // this one panel, deliberately: the whole reason the shell serves that
    // bus name itself rather than shipping mako/dunst is that a browser's
    // notification and an agent's approval must land in the same place, in
    // the same visual language — see `notifyd`'s own module doc.
    notify_center: &crate::notifyd::center::NotificationCenter,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    // Notifications.dc.html: bg `rgba(255,255,255,0.96)` light / `rgba(30,
    // 30,33,0.96)` dark — `surface_raised` in both. Border: opaque
    // `border()` light / `rgba(255,255,255,0.12)` dark.
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };

    // Keep the feed from going stale on its own. Idempotent by
    // construction as of WP-A4-4 — the FIRST call arms the one periodic
    // checker, every subsequent call (including one per render pass, which
    // is what this is) fails the claim and returns immediately. See
    // `schedule_stale_check`'s own doc comment for the appliance-VM
    // pile-up that made the previous "arm a fresh one-shot timer every
    // render" shape untenable.
    schedule_stale_check(cx);

    let mut panel = div()
        .id("overlay-notifications-panel")
        .absolute()
        .top(px(40.))
        .right(px(12.))
        .bottom(px(16.))
        .w(px(390.))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.96))
        .border_1()
        .border_color(border_color)
        .shadow(palette.floating_shadow());
    if palette.is_dark() {
        // See this file's header comment on why this is dark-only.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }
    panel
        .child(header(palette, ui.notifications.is_busy()))
        .child(tabs(palette))
        .child(content(&ui.notifications, &ui.task_progress, notify_center, palette, cx))
        .child(footer(palette))
}

/// The ONE periodic stale-check timer — WP-A4-4 (2026-08-22).
///
/// ── What this replaced, and why ────────────────────────────────────────
/// Until this round, every call armed a fresh one-shot 30s timer and simply
/// relied on "a new one is armed every render pass" for continuity. That is
/// a pile-up generator, and the appliance VM proved it: an armed timer
/// fires, `trigger_refresh_if_stale` calls `cx.notify()`, gpui repaints,
/// the repaint arms ANOTHER timer — and, critically, `lockscreen::render`
/// armed a second one of these plus a clock tick on the same pass, so each
/// poll cycle's two repaints left MORE pending timers behind than the one
/// that fired. The pending count grew for the whole 5h48m uptime; by the
/// time the machine was inspected, `cage` (whose only client is this shell)
/// was at ~100% CPU with a static screen, and every one of those timers
/// that landed while no fetch happened to be in flight started another
/// `approvals.list` — hence six-plus 429s per second in `journalctl`.
///
/// The shape now: a SINGLE task that claims `NotificationsFeed::
/// try_arm_stale_timer`'s one slot before doing anything, then re-arms
/// itself internally (`spawn_idle_watchdog`'s established loop shape in
/// `lockscreen/render.rs`) instead of relying on repaints to re-arm it.
/// Callers may therefore call this from a render body as often as they like
/// — every call after the first is a claim that fails and returns
/// immediately. It exits (releasing the slot for a later render to re-arm)
/// when OOBE takes over the screen or the window is gone.
///
/// Sleep length comes from `NotificationsFeed::next_check_delay`, so an
/// open backoff window is waited out exactly rather than being polled
/// through and refused — see that fn's own doc comment.
///
/// `pub(crate)`: `lockscreen::render` calls this SAME function (it used to
/// carry a near-identical private copy of its own — a second timer against
/// the same feed, which is exactly how two surfaces between them rebuilt
/// the pile-up this fn now prevents).
///
/// ── One deliberate behaviour change worth naming ───────────────────────
/// Because the loop now outlives the render pass that armed it, a checker
/// started from the lock screen (or from opening the panel) keeps running
/// after an unlock (or a close), at one attempt per `REFRESH_STALE_AFTER`.
/// Before, it stopped as soon as neither surface was rendering. That is a
/// net improvement rather than a leak: Home's own menu-bar approval ticker
/// (`home::menu_bar_ticker`) reads this exact feed and previously showed
/// whatever count happened to be left over from the last time the panel or
/// the lock screen was on screen. Nothing on Home arms it in the first
/// place, though — so a machine that has never locked and never opened the
/// panel still has an un-polled ticker. Fixing THAT means arming from
/// `home::render` too, which is a scope call for a later round, not
/// something to sneak in with a CPU fix.
pub(crate) fn schedule_stale_check(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| {
        // Claim the single slot. `weak.update` runs on the foreground
        // executor, so N tasks spawned by N repaints in the same frame are
        // serialised here and exactly one of them wins.
        let claimed = weak.update(cx, |view, _cx| view.overlay_ui.notifications.try_arm_stale_timer()).unwrap_or(false);
        if !claimed {
            return;
        }
        loop {
            let delay = match weak.update(cx, |view, _cx| view.overlay_ui.notifications.next_check_delay()) {
                Ok(d) => d,
                // View gone (window closed) — nothing left to release.
                Err(_) => return,
            };
            cx.background_executor().timer(delay).await;
            let keep_polling = weak.update(cx, |view, cx| {
                // Nothing on screen wants this data during first-run setup,
                // and OOBE has no gateway session yet either — release the
                // slot rather than poll into a void; the first Home/lock
                // render after OOBE finishes re-arms it.
                if view.oobe.is_some() {
                    view.overlay_ui.notifications.disarm_stale_timer();
                    return false;
                }
                trigger_refresh_if_stale(view, cx);
                // A4 (2026-08-24): rides this SAME already-armed timer —
                // see `notifications_tasks::trigger_task_refresh_if_stale`'s
                // own doc comment for why a second independent timer is
                // exactly the WP-A4-4 regression this file's own header
                // comment documents.
                super::notifications_tasks::trigger_task_refresh_if_stale(view, cx);
                true
            });
            match keep_polling {
                Ok(true) => {}
                // `Ok(false)` already disarmed above; `Err` means the view
                // is gone and the slot went with it.
                Ok(false) | Err(_) => return,
            }
        }
    })
    .detach();
}

/// `is_busy` (Shell-S4, `NotificationsFeed::is_busy`) drives a tiny
/// title-adjacent indicator — a subtle "cheap insurance" use of that flag
/// beyond the internal single-flight guard it's primarily for: the panel
/// has no other visible sign a silent background poll (`schedule_stale_
/// check`) is in flight while `status` stays `Ready` (see that variant's
/// own doc comment on why a background refresh does NOT flip back to
/// `Loading`), so this is the one place the operator can tell "still
/// talking to the gateway" apart from "done, nothing changed".
fn header(palette: ShellPalette, is_busy: bool) -> Div {
    // Notifications.dc.html: "全部標為已讀" is `brand` light / `brand_bright`
    // dark (`#2171cc`/`#59a6ff`).
    let link_text = if palette.is_dark() { palette.brand_bright } else { palette.brand };
    let mut title_row = div().flex().items_center().gap(px(6.)).child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child(fake_data::NOTIF_HEADER_TITLE));
    if is_busy {
        title_row = title_row.child(div().w(px(5.)).h(px(5.)).rounded(px(5.)).bg(theme::alpha(palette.brand, 0.7)));
    }
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(18.))
        .pt(px(16.))
        .pb(px(10.))
        .child(title_row)
        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(link_text, 1.0)).child(fake_data::NOTIF_MARK_ALL_READ))
}

fn tabs(palette: ShellPalette) -> Div {
    // Notifications.dc.html: the ACTIVE tab is an INVERTED chip in both
    // themes (`bg=#09090b/text=#fafafa` light, `bg=#fafafa/text=#18181b`
    // dark) — `active_bg` is `palette.foreground` in both cases; `active_
    // text` has no single field that holds both values (light's `0xfafafa`
    // isn't `surface`/`surface_raised` in light, which is `0xffffff`), so
    // it's a bespoke per-theme pair. The INACTIVE tab is a clean token pair
    // in both themes (`SECONDARY`/`MUTED`/`SURFACE_HOVER` all share one hex
    // per theme already, per `theme.rs`'s own token table, so `surface_
    // hover` covers it) and its text is ladder rank 2 (`text_secondary`).
    let active_bg = palette.foreground;
    let active_text = if palette.is_dark() { palette.surface } else { 0xfafafa };

    let mut row = div().flex().gap(px(6.)).px(px(18.)).pb(px(12.));
    for (i, tab) in fake_data::NOTIF_TABS.iter().enumerate() {
        let active = i == 0; // Notifications.dc.html: "全部" is the selected tab
        let (bg_hex, text_hex) = if active { (active_bg, active_text) } else { (palette.surface_hover, palette.text_secondary) };
        row = row.child(
            div()
                .rounded(px(999.))
                .px(px(12.))
                .py(px(4.))
                .text_size(px(12.))
                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .bg(theme::alpha(bg_hex, 1.0))
                .text_color(theme::alpha(text_hex, 1.0))
                .child(*tab),
        );
    }
    row
}

fn content(
    feed: &NotificationsFeed,
    // A4 (2026-08-24): the "進行中任務" section's own feed — see
    // `overlay::notifications_tasks`'s own header comment for why it is a
    // sibling read here rather than folded into `feed` above (a different
    // gateway query, a different, much simpler state machine).
    task_progress: &super::task_progress_feed::TaskProgressFeed,
    notify_center: &crate::notifyd::center::NotificationCenter,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let body = div().flex_1().overflow_hidden().flex().flex_col().gap(px(10.)).px(px(14.));

    // D6: the approval feed's three non-Ready states are about the GATEWAY,
    // and say nothing about whether third-party app notifications work. They
    // used to `return` the whole body; now they render their banner and then
    // fall through to the app-notification section, so a gateway outage can
    // never also hide a LINE message that arrived perfectly well.
    let (body, show_approvals) = match feed.status {
        FeedStatus::Offline => (body.child(status_banner(t(Locale::ZhTw, Key::NotifOfflineBanner), palette, true)), false),
        FeedStatus::Idle => (body.child(status_banner(t(Locale::ZhTw, Key::NotifLoadingLabel), palette, false)), false),
        FeedStatus::Loading if feed.rows().is_empty() && feed.decided().is_empty() => {
            (body.child(status_banner(t(Locale::ZhTw, Key::NotifLoadingLabel), palette, false)), false)
        }
        FeedStatus::Loading | FeedStatus::Ready => (body, true),
    };

    let mut body = body;
    if show_approvals {
        let mut cards = Vec::with_capacity(feed.rows().len() + feed.decided().len());
        for row in feed.rows() {
            cards.push(approval_card(row, palette, cx));
        }
        for row in feed.decided() {
            cards.push(approval_card(row, palette, cx));
        }
        if cards.is_empty() {
            body = body.child(status_banner(t(Locale::ZhTw, Key::NotifEmptyLabel), palette, false));
        } else {
            body = body.children(cards);
        }
    }

    body = super::notifications_apps::app_notifications_section(body, notify_center, palette, cx);
    body = super::notifications_tasks::task_progress_section(body, task_progress, palette);

    body.child(
        div()
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::alpha(palette.text_faint, 1.0))
            .px(px(4.))
            .child(fake_data::NOTIF_TODAY_LABEL),
    )
    .children(fake_data::TODAY_ACTIVITY.iter().map(move |row| activity_row(row, palette)))
}

/// One connectivity/state line — the offline banner uses `destructive` text
/// on a faint destructive tint (an honest error, same treatment
/// `oobe::steps::network`'s own `NetworkScanFailedStatus` line gets);
/// loading/empty use plain muted text, no alarming color (task brief's own
/// "誠實橫幅" only asks for honesty, not alarm, on the non-error states).
pub(super) fn status_banner(text: &str, palette: ShellPalette, is_error: bool) -> Div {
    let mut el = div().px(px(4.)).py(px(10.)).text_size(px(12.)).text_color(theme::alpha(if is_error { palette.destructive } else { palette.text_faint }, 1.0));
    if is_error {
        el = el.bg(theme::alpha(palette.destructive, if palette.is_dark() { 0.12 } else { 0.08 })).rounded(px(8.)).px(px(10.));
    }
    el.child(text.to_string())
}

fn approval_card(row: &ApprovalRow, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // Notifications.dc.html: pending cards get an amber border — `#f3d9a4`
    // light (opaque) / `rgba(203,148,0,0.45)` dark (`warning` at 45%
    // alpha). Resolved (approved/rejected) cards have no board precedent in
    // EITHER theme (the board only ever renders pending cards) — light
    // settles to the neutral panel-border hex, same as the original
    // light-only code; dark's analog uses this file's own neutral
    // panel-border convention (`rgba(255,255,255,0.12)`, matching the panel
    // root's own border elsewhere in this file) rather than inventing a
    // fourth color.
    let is_awaiting_input = matches!(row.decision, RowDecision::Pending | RowDecision::Confirming { .. } | RowDecision::Failed { .. });
    let border_color: Rgba = match (is_awaiting_input, palette.is_dark()) {
        (true, true) => theme::alpha(palette.warning, 0.45),
        (true, false) => theme::alpha(0xf3d9a4, 1.0),
        (false, true) => theme::alpha(0xffffff, 0.12),
        (false, false) => theme::alpha(0xececef, 1.0),
    };

    // agent_id has no design-board avatar color/initial mapping (that
    // struct's `agent_initial`/`agent_bg_hex` fields were per-card DESIGN
    // data, not something the real gateway response carries) — derived
    // deterministically instead: the first character (CJK-safe — `chars()`,
    // not a byte slice) and a stable hash-picked color from the same
    // small palette `home_dock.rs`'s own dock agents already use, so a
    // given agent always renders the same color across refreshes without
    // needing a lookup table this round doesn't have data for.
    let initial = row.agent_id.chars().next().map(String::from).unwrap_or_else(|| "?".to_string());
    let bg_hex = agent_color_for(&row.agent_id);

    let mut root = div()
        .id(format!("notif-approval-{}", row.id))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(12.))
        .px(px(14.))
        .py(px(13.))
        .shadow(palette.surface_shadow())
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(avatar(initial, bg_hex, palette))
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(row.summary.clone()))
                .when(is_awaiting_input, |el| el.child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(palette.brand, 1.0)))),
        )
        .child(div().text_size(px(12.)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(row.detail.clone()));

    root = root.child(match row.decision {
        RowDecision::Pending => approval_actions(&row.id, palette, cx),
        RowDecision::Confirming { approve } => confirm_actions(&row.id, approve, palette, cx),
        RowDecision::InFlight { .. } => status_pill(t(Locale::ZhTw, Key::NotifDecidingLabel), palette.text_secondary, palette.surface_hover),
        RowDecision::Failed { approve } => failed_actions(&row.id, approve, palette, cx),
        RowDecision::Settled { approved: true } => {
            decision_badge(fake_data::NOTIF_APPROVED_LABEL, palette.badge_text(fake_data::BadgeKind::Success), palette.badge_bg(fake_data::BadgeKind::Success))
        }
        RowDecision::Settled { approved: false } => decision_badge(
            fake_data::NOTIF_REJECTED_LABEL,
            theme::alpha(palette.destructive, 1.0),
            // No board precedent for a rejected state (the board only ever
            // shows pending cards) — approximated as a light tint of the
            // same destructive token; dark's tint bumps to 0.16 to match
            // the family convention the OTHER three badge kinds use in
            // dark (see `crate::palette::ShellPalette::badge_bg`'s own doc
            // comment), rather than reusing light's 0.12 verbatim.
            theme::alpha(palette.destructive, if palette.is_dark() { 0.16 } else { 0.12 }),
        ),
    });
    root
}

/// Small fixed palette of avatar background hues, picked deterministically
/// per `agent_id` (a simple additive hash over its bytes — no crypto
/// property needed, just visual stability across refreshes) — reuses the
/// exact two hex values `fake_data::DOCK_AGENTS` already established for
/// this shell's two example agents, plus one more from the goal-card badge
/// family, so a real agent id never introduces a brand-new, uncoordinated
/// hue.
pub(super) fn agent_color_for(agent_id: &str) -> u32 {
    const PALETTE: [u32; 3] = [0x2171cc, 0x0f766e, 0x8b5cf6];
    let sum: u32 = agent_id.bytes().map(u32::from).sum();
    PALETTE[(sum as usize) % PALETTE.len()]
}

fn approval_actions(id: &str, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let id_approve = id.to_string();
    let approve_click = cx.listener(move |view, _ev, _window, cx| {
        view.overlay_ui.notifications.begin_confirm(&id_approve, true);
        cx.notify();
    });
    let id_reject = id.to_string();
    let reject_click = cx.listener(move |view, _ev, _window, cx| {
        view.overlay_ui.notifications.begin_confirm(&id_reject, false);
        cx.notify();
    });

    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(action_button(fake_data::NOTIF_APPROVE_LABEL, format!("notif-approve-{id}"), true, palette, approve_click))
        .child(action_button(fake_data::NOTIF_REJECT_LABEL, format!("notif-reject-{id}"), false, palette, reject_click))
        .child(div().flex_1().text_size(px(11.5)).text_color(theme::alpha(palette.text_faint, 1.0)).child(fake_data::NOTIF_NOTE_PLACEHOLDER))
}

/// The armed "Confirming" row — real decision, one more click to fire it.
/// Task brief: "要二段確認防誤觸" — see this file's own header comment for
/// why a two-step confirm was added beyond what the design board itself
/// shows.
fn confirm_actions(id: &str, approve: bool, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let id_confirm = id.to_string();
    let confirm_click = cx.listener(move |view, _ev, _window, cx| {
        let Some(approve) = view.overlay_ui.notifications.begin_decide(&id_confirm) else {
            return;
        };
        cx.notify();
        dispatch_decide(id_confirm.clone(), approve, view, cx);
    });
    let id_cancel = id.to_string();
    let cancel_click = cx.listener(move |view, _ev, _window, cx| {
        view.overlay_ui.notifications.cancel_confirm(&id_cancel);
        cx.notify();
    });

    let prompt_color = if approve { palette.brand } else { palette.destructive };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(prompt_color, 1.0))
                .child(if approve { fake_data::NOTIF_APPROVE_LABEL } else { fake_data::NOTIF_REJECT_LABEL }),
        )
        .child(action_button(t(Locale::ZhTw, Key::NotifConfirmButton), format!("notif-confirm-{id}"), approve, palette, confirm_click))
        .child(cancel_button(format!("notif-cancel-{id}"), palette, cancel_click))
}

/// A `Failed` row — the confirm/cancel affordance from `confirm_actions`
/// already collapsed; this offers a retry (re-arms `Confirming`, same as a
/// first click on a `Pending` row) plus the honest failure line.
fn failed_actions(id: &str, approve: bool, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let id_retry = id.to_string();
    let retry_click = cx.listener(move |view, _ev, _window, cx| {
        view.overlay_ui.notifications.begin_confirm(&id_retry, approve);
        cx.notify();
    });
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .text_size(px(11.5))
                .text_color(theme::alpha(palette.destructive, 1.0))
                .child(t(Locale::ZhTw, Key::NotifDecideFailedLabel)),
        )
        .child(action_button(t(Locale::ZhTw, Key::NotifRetryButton), format!("notif-retry-{id}"), approve, palette, retry_click))
}

/// Dispatches `gateway_client::decide_approval` on a background thread and
/// bridges the result back via the same `mpsc` + `cx.spawn` poll pattern
/// `trigger_refresh_if_stale` above and `oobe/steps/account.rs` both use.
/// Split out of `confirm_actions`'s click closure only to keep that closure
/// body short — not reused elsewhere.
fn dispatch_decide(id: String, approve: bool, view: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(jwt) = view.overlay_ui.notifications.session_jwt().map(str::to_string) else {
        // Shouldn't happen — a row only exists after a successful list,
        // which requires a session — but fail closed rather than dispatch
        // a request with no credential.
        view.overlay_ui.notifications.apply_decide_err(&id, approve);
        return;
    };

    let (tx, rx) = mpsc::channel();
    let id_for_thread = id.clone();
    std::thread::spawn(move || {
        // stderr trail records the EVENT (which id, approve/reject,
        // outcome) — never the jwt or the approval's own content/payload;
        // the durable, content-bearing audit record is the gateway's own
        // `approvals.db` + audit log (task brief: "決策動作有稽核可循——
        // gateway 端本就有").
        let ok = match gateway_client::decide_approval(&jwt, &id_for_thread, approve) {
            Ok(()) => {
                eprintln!("[notifications] approvals.decide id={id_for_thread} approve={approve} ok");
                true
            }
            Err(e) => {
                eprintln!("[notifications] approvals.decide id={id_for_thread} approve={approve} failed: {e:?}");
                false
            }
        };
        let _ = tx.send(ok);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(ok) => {
                let _ = weak.update(cx, |view, cx| {
                    if ok {
                        view.overlay_ui.notifications.apply_decide_ok(&id, approve);
                    } else {
                        view.overlay_ui.notifications.apply_decide_err(&id, approve);
                    }
                    cx.notify();
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    })
    .detach();
}

pub(super) fn action_button(
    label: &str,
    id: impl Into<gpui::ElementId>,
    primary: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    if primary {
        div()
            .id(id)
            .cursor_pointer()
            .bg(theme::alpha(palette.brand, 1.0))
            .text_color(theme::alpha(palette.brand_foreground, 1.0))
            .rounded(px(8.))
            .px(px(14.))
            .py(px(5.))
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(label.to_string())
            .on_click(on_click)
    } else {
        // Notifications.dc.html: bg `#ffffff` light / `#27272a` dark — dark
        // deliberately picks `surface_hover`, NOT `surface_raised`, so the
        // button visibly stands out from the card it sits on (light leaves
        // the card/button bg identical, distinguishing the button by border
        // alone — see this file's header comment for why the two themes
        // diverge here). Border: `border()` light (opaque) /
        // `rgba(255,255,255,0.14)` dark (bespoke, not `border()`'s own dark
        // 0.06 alpha).
        let bg = if palette.is_dark() { palette.surface_hover } else { palette.surface_raised };
        let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.14).into() } else { palette.border() };
        div()
            .id(id)
            .cursor_pointer()
            .bg(theme::alpha(bg, 1.0))
            .text_color(theme::alpha(palette.text_secondary, 1.0))
            .border_1()
            .border_color(border_color)
            .rounded(px(8.))
            .px(px(14.))
            .py(px(5.))
            .text_size(px(12.))
            .child(label.to_string())
            .on_click(on_click)
    }
}

/// The "取消" (cancel confirm) button — visually the same neutral secondary
/// treatment as `action_button`'s non-primary branch, kept as its own small
/// fn since it has no `primary`/`approve` distinction to carry.
fn cancel_button(id: impl Into<gpui::ElementId>, palette: ShellPalette, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    // Reuses `NetworkCancelButton` ("取消") rather than adding a dedicated
    // Notifications key — see `crate::i18n::Key::NotifOfflineBanner`'s own
    // doc comment for why this one action is shared instead of duplicated.
    action_button(t(Locale::ZhTw, Key::NetworkCancelButton), id, false, palette, on_click)
}

/// A neutral (non-badge-colored) status pill — used for the `InFlight`
/// "處理中…" line, visually distinct from the success/destructive
/// `decision_badge`s below it.
fn status_pill(label: &str, text_hex: u32, bg_hex: u32) -> Div {
    decision_badge_owned(label.to_string(), theme::alpha(text_hex, 1.0), theme::alpha(bg_hex, 1.0))
}

/// Shared pill badge — resolved approval outcomes (已核准/已駁回) AND the
/// activity rows' own status pills (完成/等你決定) are visually the same
/// component in the design board, just different label/color pairs.
fn decision_badge(label: &'static str, text: Rgba, bg: Rgba) -> Div {
    decision_badge_owned(label.to_string(), text, bg)
}

pub(super) fn decision_badge_owned(label: String, text: Rgba, bg: Rgba) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(text).bg(bg).rounded(px(999.)).px(px(8.)).py(px(2.)).child(label)
}

pub(super) fn avatar(initial: String, bg_hex: u32, palette: ShellPalette) -> Div {
    div()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(13.))
        .bg(theme::alpha(bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(palette.brand_foreground, 1.0)).child(initial))
}

fn system_icon(palette: ShellPalette) -> Div {
    // Same neutral gray "settings"/"system update" gradient
    // `home/home_dock.rs::dock_settings` uses — see `crate::palette`'s own
    // header comment (`settings_gradient_top`/`bottom`). Glyph stroke:
    // `#ffffff` light (kept literal, byte-identical to the original) /
    // `text_secondary` (`#d4d4d8`) dark, matching the board's own dark
    // stroke exactly.
    // ICON-1 (2026-08-22): `palette.icon_on_neutral_gradient()` resolves to
    // the SAME pair this line used to spell out (`#ffffff` light /
    // `text_secondary` dark) — it is the shared tint for icons sitting on
    // this neutral gray gradient, which the dock's settings gear uses too.
    let glyph_color = palette.icon_on_neutral_gradient();
    div()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(7.))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(palette.settings_gradient_top), 0.0),
            linear_color_stop(rgb(palette.settings_gradient_bottom), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .shadow(palette.icon_shadow(0.14, 0.30))
        // Update-available system row. ICON-1 (2026-08-22) restored the
        // board's own download arrow here; the "更" initial that stood in
        // for it is now only the fallback.
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(glyph_color, 1.0))
                .child(icons::icon_or_glyph(&[(icons::DOWNLOAD, glyph_color)], 14., "更")),
        )
}

fn activity_row(row: &fake_data::ActivityRow, palette: ShellPalette) -> Stateful<Div> {
    let avatar_el = match row.avatar {
        fake_data::RowAvatar::Agent { initial, bg_hex } => avatar(initial.to_string(), bg_hex, palette),
        fake_data::RowAvatar::System => system_icon(palette),
    };

    let mut el = div()
        .id(row.id)
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(6.))
        .py(px(8.))
        .rounded(px(10.))
        .child(avatar_el)
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().text_size(px(12.5)).child(row.line1))
                .child(div().text_size(px(11.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(row.line2)),
        );
    if let Some((label, kind)) = row.badge {
        el = el.child(decision_badge(label, palette.badge_text(kind), palette.badge_bg(kind)));
    }
    el
}

fn footer(palette: ShellPalette) -> Div {
    // Notifications.dc.html: border-top `#f0f0f2` light / `rgba(255,255,
    // 255,0.08)` dark.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    div()
        .px(px(18.))
        .py(px(10.))
        .border_t_1()
        .border_color(border_color)
        .text_size(px(11.))
        .text_color(theme::alpha(palette.text_faint, 1.0))
        .child(fake_data::NOTIF_FOOTER)
}
