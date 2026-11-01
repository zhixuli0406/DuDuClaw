//! Notifications accept counts only. No remote string can become an OSC payload.
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Direct,
    Tmux,
    Skip,
}
pub struct Notifications {
    enabled: bool,
    route: Route,
    focused: bool,
    previous: usize,
    last: Option<Instant>,
}
impl Notifications {
    pub fn new(enabled: bool, route: Route) -> Self {
        // Terminals that never report focus are conservatively unfocused.
        Self {
            enabled,
            route,
            focused: false,
            previous: 0,
            last: None,
        }
    }
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    pub fn update(&mut self, count: Option<usize>, now: Instant) -> Option<String> {
        let count = count?;
        let rises = count > self.previous;
        self.previous = count;
        if !self.enabled
            || self.focused
            || !rises
            || self.route == Route::Skip
            || self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(60))
        {
            return None;
        }
        self.last = Some(now);
        let osc = format!("\x1b]9;{}\x07", crate::strings::notification(count));
        Some(match self.route {
            Route::Direct => osc,
            Route::Tmux => format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b")),
            Route::Skip => unreachable!(),
        })
    }
}
pub async fn route() -> Route {
    if std::env::var_os("TMUX").is_none() {
        return Route::Direct;
    }
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::process::Command::new("tmux")
            .args(["show-option", "-gv", "allow-passthrough"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match result {
        Ok(Ok(output))
            if output.status.success()
                && matches!(output.stdout.as_slice(), b"on\n" | b"all\n") =>
        {
            Route::Tmux
        }
        _ => Route::Skip,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_text_focus_rate_limit_and_stale() {
        let now = Instant::now();
        let mut n = Notifications::new(true, Route::Direct);
        assert_eq!(
            n.update(Some(2), now).unwrap(),
            "\x1b]9;DuDuClaw：2 項需要你處理\x07"
        );
        assert!(n.update(Some(3), now + Duration::from_secs(59)).is_none());
        assert!(n.update(None, now + Duration::from_secs(60)).is_none());
        assert!(n.update(Some(4), now + Duration::from_secs(60)).is_some());
        n.focus(true);
        assert!(n.update(Some(5), now + Duration::from_secs(120)).is_none());
        n.focus(false);
        assert!(n.update(Some(5), now + Duration::from_secs(121)).is_none());
        assert!(n.update(Some(6), now + Duration::from_secs(121)).is_some());
    }
    #[test]
    fn hostile_data_has_no_path_into_sequence() {
        let hostile =
            serde_json::json!({"name":"\u{1b}]52;c;bad\u{7}", "title":"\u{1b}]8;;evil\u{7}"});
        let inbox = [hostile];
        let mut n = Notifications::new(true, Route::Tmux);
        assert_eq!(
            n.update(Some(inbox.len()), Instant::now()).unwrap(),
            "\x1bPtmux;\x1b\x1b]9;DuDuClaw：1 項需要你處理\x07\x1b\\"
        );
        for (enabled, route) in [(false, Route::Direct), (true, Route::Skip)] {
            assert!(
                Notifications::new(enabled, route)
                    .update(Some(1), Instant::now())
                    .is_none()
            );
        }
    }
}
