//! Notifications accept counts only. No remote string can become an OSC payload.
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Direct,
    Tmux,
    Direct777,
    Tmux777,
    Skip,
}
pub struct Notifications {
    enabled: bool,
    route: Route,
    focused: bool,
    previous: usize,
    connection_error: bool,
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
            connection_error: false,
            last: None,
        }
    }
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    /// Callers supply only actionable approvals and tasks, excluding informational rows.
    pub fn update(&mut self, actionable_count: Option<usize>, now: Instant) -> Option<String> {
        let count = actionable_count?;
        let rises = count > self.previous;
        self.previous = count;
        if !rises { return None; }
        self.emit(&crate::strings::notification(count), now)
    }
    pub fn connection_error(&mut self, failed: bool, now: Instant) -> Option<String> {
        let rises = failed && !self.connection_error;
        self.connection_error = failed;
        if !rises { return None; }
        self.emit(crate::strings::NOTIFY_CONNECTION_ERROR, now)
    }
    fn emit(&mut self, fixed: &str, now: Instant) -> Option<String> {
        if !self.enabled
            || self.focused
            || self.route == Route::Skip
            || self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(60))
        {
            return None;
        }
        self.last = Some(now);
        let osc = if matches!(self.route, Route::Direct777 | Route::Tmux777) {
            format!("\x1b]777;notify;DuDuClaw;{fixed}\x07")
        } else { format!("\x1b]9;{fixed}\x07") };
        Some(match self.route {
            Route::Direct | Route::Direct777 => osc,
            Route::Tmux | Route::Tmux777 => format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b")),
            Route::Skip => unreachable!(),
        })
    }
}
pub async fn route() -> Route {
    let osc777 = std::env::var_os("VTE_VERSION").is_some()
        || matches!(std::env::var("TERM").as_deref(), Ok("rxvt-unicode" | "rxvt-unicode-256color"));
    if std::env::var_os("TMUX").is_none() {
        return if osc777 { Route::Direct777 } else { Route::Direct };
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
            if osc777 { Route::Tmux777 } else { Route::Tmux }
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

#[cfg(test)]
mod extended_tests {
    use super::*;
    #[test]
    fn osc777_and_connection_errors_share_fixed_text_focus_and_rate_limit() {
        let now = Instant::now();
        for route in [Route::Direct777, Route::Tmux777] {
            let mut n = Notifications::new(true, route);
            let sequence = n.connection_error(true, now).unwrap();
            assert!(sequence.contains("]777;notify;DuDuClaw;DuDuClaw：連線中斷，請檢查控制室"));
            assert!(!sequence.contains("private"));
            assert!(n.update(Some(1), now).is_none());
            assert!(n.connection_error(true, now + Duration::from_secs(61)).is_none());
            n.connection_error(false, now + Duration::from_secs(61));
            n.focus(true);
            assert!(n.connection_error(true, now + Duration::from_secs(61)).is_none());
            n.focus(false);
            let hostile = serde_json::json!({"name":"private\u{1b}]52;c;bad\u{7}","error":"private password"});
            let rows = [hostile.clone(), hostile];
            let sequence = n.update(Some(rows.len()), now + Duration::from_secs(62)).unwrap();
            assert!(sequence.contains("DuDuClaw：2 項需要你處理"));
            assert!(!sequence.contains("private") && !sequence.contains("]52"));
        }
        assert!(Notifications::new(false, Route::Direct777).connection_error(true, now).is_none());
    }
}
