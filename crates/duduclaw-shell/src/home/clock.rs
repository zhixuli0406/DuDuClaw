// Menu-bar clock — WP-fix-QEMU-a (2026-09-05), a QEMU walkthrough defect
// fix. The menu bar's "22:58" used to be `fake_data::CLOCK`, a literal
// constant that never advanced — and on the walkthrough boot it read a
// completely different time from the SAME machine's lock screen
// (`lockscreen::render::now_local_strings`, which already reads
// `chrono::Local::now()`). This module gives Home's own menu bar the
// identical real clock source, so the two can never disagree again.

use std::time::Duration;

use chrono::Timelike;
use gpui::Context;

use crate::ShellView;

/// "HH:MM" in the system's local timezone — the exact same
/// `chrono::Local::now()` source `lockscreen::render::now_local_strings`
/// already uses for the lock screen's own clock (that fn's own doc comment
/// has the "already a resolved dependency, promoted to direct" provenance;
/// this reads the identical clock, no new dependency).
pub(crate) fn menu_bar_clock_text() -> String {
    let now = chrono::Local::now();
    format!("{:02}:{:02}", now.hour(), now.minute())
}

/// Same 20s cadence `lockscreen::render::CLOCK_TICK_INTERVAL` uses (that
/// constant is private to a module this file must not edit, so this is its
/// own copy, not a shared one) — close enough to a minute boundary that the
/// displayed "HH:MM" never visibly lags, cheap enough (a plain repaint, no
/// I/O) to run for as long as this binary is alive.
const CLOCK_TICK_INTERVAL: Duration = Duration::from_secs(20);

/// Started exactly ONCE, from `main()` — same "runs continuously from boot,
/// no per-render re-arming" shape `lockscreen::render::spawn_idle_watchdog`
/// already establishes for the identical reason (see that fn's own doc
/// comment): Home's menu bar renders for as long as the shell is not in
/// OOBE/live-install/locked, which is nearly always, so there is no
/// meaningfully cheaper "only tick while X" condition to gate this on the
/// way the lock screen's own clock gates on `is_locked()` (that timer stops
/// paying for itself the instant the screen unlocks; this one has no
/// equivalent idle state worth stopping for).
///
/// A pure repaint, exactly like `schedule_clock_tick`'s own: `menu_bar_
/// clock_text()` recomputes the actual displayed value fresh on the very
/// next render pass this triggers, nothing is cached here. Exits once the
/// view is gone (window closed) — `weak.update` starts returning `Err` and
/// the loop stops re-arming itself, same exit condition `spawn_idle_
/// watchdog`'s own loop uses.
pub(crate) fn spawn_menu_bar_clock_tick(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| loop {
        cx.background_executor().timer(CLOCK_TICK_INTERVAL).await;
        if weak.update(cx, |_view, cx| cx.notify()).is_err() {
            break;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_bar_clock_text_is_always_five_characters_hh_colon_mm() {
        // Not a claim about WHICH time — that would be a flaky test racing
        // the real clock — just the honest "HH:MM" shape, unlike the old
        // `fake_data::CLOCK` literal this replaces which was a fixed string
        // that could drift from reality forever.
        let text = menu_bar_clock_text();
        assert_eq!(text.len(), 5, "expected HH:MM, got {text:?}");
        assert_eq!(text.chars().nth(2), Some(':'), "expected a colon separator, got {text:?}");
        assert!(text.chars().filter(|c| *c != ':').all(|c| c.is_ascii_digit()), "expected only digits and ':', got {text:?}");
    }
}
