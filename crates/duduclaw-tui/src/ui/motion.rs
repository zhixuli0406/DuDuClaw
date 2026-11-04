//! Coalesce animation frames without redrawing an idle control room.
use super::app::App;
use crate::settings::Settings;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Motion { last: Option<Instant> }
impl Motion {
    pub(super) fn tick(&mut self, now: Instant, settings: &Settings, running: bool, spinner: &mut usize) -> bool {
        if !settings.motion || !running { self.last = None; *spinner = 0; return false; }
        if self.last.is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(80)) {
            self.last = Some(now); *spinner = spinner.wrapping_add(1); return true;
        }
        false
    }
}
pub(super) fn running(app: &App) -> bool {
    if app.list_view() || app.card.is_some() || app.control_card.is_some() || app.help || app.palette.is_some() || app.mailbox.is_some() { return false; }
    app.blocks.panes.iter().enumerate().any(|(index, pane)| {
        if pane.kind != crate::blocks::Block::Chat || (app.zoomed && index != app.blocks.focused) { return false; }
        let chat = if index == app.blocks.focused { &app.chat } else { &pane.chat_view.state };
        chat.running && chat.activity.has_running()
    })
}

#[derive(Default, PartialEq, Eq)]
struct TimerState {
    card: Option<(bool, bool)>,
    control_ready: Option<bool>,
    toasts: Vec<u64>,
    downstream: Vec<bool>,
}
#[derive(Default)]
pub(super) struct Timers { last: Option<Instant>, state: TimerState }
impl Timers {
    pub(super) fn tick(&mut self, now: Instant, app: &App) -> bool {
        if self.last.is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(1)) { return false; }
        self.last = Some(now);
        let state = TimerState {
            card: app.card.as_ref().map(|card| card.clock_state(now, crate::snooze::epoch_now())),
            control_ready: app.control_card.as_ref().map(|card| card.ready(now)),
            toasts: app.notices.visible(now).iter().map(|notice| notice.id).collect(),
            downstream: std::iter::once(app.downstream_prompt(now).is_some()).chain(app.blocks.panes.iter().enumerate()
                .filter(|(index, pane)| *index != app.blocks.focused && pane.kind == crate::blocks::Block::Chat)
                .map(|(_, pane)| pane.chat_view.state.downstream_reference(now).is_some())).collect(),
        };
        let changed = self.state != state;
        self.state = state;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_and_persistent_notices_do_not_schedule_repeated_redraws() {
        let now = Instant::now(); let mut app = App::default(); let mut timers = Timers::default();
        timers.tick(now, &app);
        assert!(!timers.tick(now + Duration::from_secs(1), &app));
        app.notices.push(super::super::notices::Kind::NeedYou, crate::sanitize::TerminalText::default(), None, None, now);
        assert!(timers.tick(now + Duration::from_secs(2), &app));
        assert!(!timers.tick(now + Duration::from_secs(3), &app));
        app.notices.push(super::super::notices::Kind::Done, crate::sanitize::TerminalText::default(), None, None, now);
        assert!(timers.tick(now + Duration::from_secs(4), &app));
        assert!(timers.tick(now + Duration::from_secs(7), &app));
        assert!(!timers.tick(now + Duration::from_secs(8), &app));
    }
    #[test]
    fn stops_when_idle_or_disabled_and_coalesces_eighty_ms() {
        let now = Instant::now(); let mut motion = Motion::default(); let mut tick = 0;
        let mut settings = Settings::test_defaults();
        assert!(!motion.tick(now, &settings, false, &mut tick));
        assert!(motion.tick(now, &settings, true, &mut tick));
        assert!(!motion.tick(now + Duration::from_millis(79), &settings, true, &mut tick));
        assert!(motion.tick(now + Duration::from_millis(80), &settings, true, &mut tick));
        assert!(!motion.tick(now + Duration::from_secs(1), &settings, false, &mut tick));
        assert_eq!(tick, 0);
        settings.motion = false;
        assert!(!motion.tick(now + Duration::from_secs(2), &settings, true, &mut tick));
        assert_eq!(tick, 0);
        for bad in ["true", "'auto'", "0"] { assert!(Settings::parse(&format!("motion = {bad}")).1.is_some()); }
        assert!(Settings::parse("").0.motion);
        assert!(!Settings::parse("motion = 'off'").0.motion);
        assert!(Settings::parse("motion = 'on'").0.motion);
    }
}
