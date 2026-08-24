// The Notifications panel's "進行中任務" section, plus the fetch/apply glue
// for `overlay::task_progress_feed::TaskProgressFeed` — A4 (2026-08-24).
//
// Its own file, not more lines in `overlay/notifications.rs`: that file is
// already at this crate's ~900-line ceiling (see `notifications_apps.rs`'s
// own header comment for the same reasoning on the exact same file, applied
// there for D6's app-notification section). The 接點 into `notifications.rs`
// is the same shape D6 established: one extra field read in `content()`,
// one call to `task_progress_section` from it, and one extra trigger call
// from the ALREADY-existing `schedule_stale_check` timer loop — no second
// timer is armed (see `task_progress_feed`'s own header comment for why
// that specifically matters on this codebase).
//
// ── Why this reuses `NotificationsFeed`'s session instead of bootstrapping
// its own ─────────────────────────────────────────────────────────────
// A brand new local-session POST per feed would double the appliance's
// `/api/session/local` traffic for no reason — both feeds are the SAME
// `admin@local` operator session. `NotificationsFeed::session_jwt()` is
// already a `pub fn` accessor for exactly this kind of reuse. If no session
// exists yet (fresh boot, panel/lockscreen never rendered), this feed falls
// back to bootstrapping its own — it must not sit blocked forever waiting on
// a session `NotificationsFeed` may never be asked to create (e.g. a machine
// where only the dock, never the panel, has rendered since this round wires
// `schedule_stale_check` into `home_dock.rs` too — see that file's own
// comment on this call site).

use std::sync::mpsc;
use std::time::Duration;

use gpui::{div, prelude::*, px, Context, Div, FontWeight};

use duduclaw_native_gui::theme;

use super::notifications::agent_color_for;
use super::task_progress_feed::TaskProgressFeed;
use crate::gateway_client::{self, TaskProgressItem};
use crate::i18n::{t, Key, Locale};
use crate::palette::ShellPalette;
use crate::ShellView;

/// Same bridge cadence every other background-thread <-> `cx.spawn` poll in
/// this crate uses (`overlay/notifications.rs::POLL_INTERVAL`, `home_dock.rs
/// ::BRIDGE_POLL_INTERVAL`) — this is the "check the mpsc channel" tick, not
/// the fetch cadence itself (`task_progress_feed::REFRESH_STALE_AFTER`).
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Dispatches a background-thread `tasks.list(status="in_progress")` fetch
/// when the feed is stale and nothing is already in flight. Called from
/// `overlay/notifications.rs::schedule_stale_check`'s loop body (the SAME
/// already-armed 30s timer `trigger_refresh_if_stale` rides) and from
/// `home_dock.rs::dock_container` (see that file's own comment on why the
/// dock arms the shared timer at all this round) — never on its own timer.
pub(crate) fn trigger_task_refresh_if_stale(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.overlay_ui.task_progress.is_stale() {
        return;
    }
    if !view.overlay_ui.task_progress.begin_refresh() {
        return;
    }
    // Reuse the approvals feed's session if it already bootstrapped one —
    // see this file's header comment. A `None` here just means `fetch_once`
    // bootstraps its own; it does NOT mean "wait for the other feed".
    let existing_jwt = view.overlay_ui.notifications.session_jwt().map(str::to_string);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = fetch_once(existing_jwt);
        let _ = tx.send(outcome);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    let changed = apply_fetch_outcome(&mut view.overlay_ui.task_progress, outcome);
                    if changed {
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

/// Runs entirely on a background `std::thread` — never called from gpui's
/// own executor, same contract `overlay/notifications.rs::fetch_once`
/// documents for its own approvals fetch.
fn fetch_once(existing_jwt: Option<String>) -> Result<Vec<TaskProgressItem>, gateway_client::GatewayError> {
    let jwt = match existing_jwt {
        Some(jwt) => jwt,
        None => gateway_client::bootstrap_local_session()?,
    };
    Ok(gateway_client::list_in_progress_tasks(&jwt)?)
}

/// Applies one settled fetch. Returns whether the dock badge / panel section
/// actually changed. Failures are diag-gated (not unconditional stderr like
/// the approvals fetch's own `apply_fetch_outcome`) — a missing in-progress
/// task count is a soft, secondary signal, not the primary offline banner
/// this panel already owns via the approvals feed; a second unconditional
/// failure line for the same underlying outage would just double the noise
/// WP-A4-4 spent a whole round suppressing.
fn apply_fetch_outcome(feed: &mut TaskProgressFeed, outcome: Result<Vec<TaskProgressItem>, gateway_client::GatewayError>) -> bool {
    match outcome {
        Ok(items) => feed.apply_list_ok(items),
        Err(e) => {
            if crate::diag_enabled() {
                eprintln!("[tasks] in-progress task fetch failed: {e:?}");
            }
            feed.apply_list_err();
            false
        }
    }
}

/// Appends the section heading and one row per in-progress task. Renders
/// nothing at all when there are none — same "an empty heading over an
/// empty list is noise" rule `notifications_apps::app_notifications_section`
/// applies to its own section, and the approval feed already owns this
/// panel's "nothing here" line for the offline/loading/empty approvals
/// states.
pub(super) fn task_progress_section(body: Div, feed: &TaskProgressFeed, palette: ShellPalette) -> Div {
    if feed.rows().is_empty() {
        return body;
    }

    let mut section = body.child(
        div()
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::alpha(palette.text_faint, 1.0))
            .px(px(4.))
            .child(t(Locale::ZhTw, Key::NotifTaskSectionLabel)),
    );
    for row in feed.rows() {
        section = section.child(task_row(row, palette));
    }
    section
}

fn task_row(row: &TaskProgressItem, palette: ShellPalette) -> gpui::Stateful<Div> {
    // No design-board precedent for this row (same situation `notifications
    // .rs::approval_card`'s own comment documents for `agent_id`) — reuses
    // that exact same deterministic per-agent color picker rather than
    // inventing a second one, so an agent that shows up in both an approval
    // card and a task row renders with the SAME hue in both.
    let bg_hex = agent_color_for(&row.assigned_to);
    let initial = row.assigned_to.chars().next().map(String::from).unwrap_or_else(|| "?".to_string());

    div()
        .id(format!("notif-task-{}", row.id))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(0xececef, 1.0) })
        .rounded(px(12.))
        .px(px(14.))
        .py(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .w(px(22.))
                .h(px(22.))
                .rounded(px(11.))
                .bg(theme::alpha(bg_hex, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(palette.brand_foreground, 1.0))
                .child(initial),
        )
        .child(div().flex_1().text_size(px(13.)).text_color(theme::alpha(palette.foreground, 1.0)).child(row.title.clone()))
}
