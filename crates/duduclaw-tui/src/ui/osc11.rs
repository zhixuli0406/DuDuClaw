//! Startup-only OSC 11 negotiation using the same reader as the event loop.
//!
//! Crossterm exposes an OSC response as key events. Keeping one reader avoids
//! competing stdin consumers and preserves unrelated keys, paste and resize
//! events. The reply filter survives the deadline to consume delayed fragments.
//! An incomplete prefix/reply recovers after 150 ms without response bytes;
//! standalone Esc becomes deliverable after 50 ms, on the next reader poll.
//! Fragments arriving after that recovery
//! window cannot be distinguished from normal keys and are no longer quarantined.
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::{collections::VecDeque, io::{self, IsTerminal}, time::{Duration, Instant}};

#[cfg(any(unix, test))]
const QUERY_TIMEOUT: Duration = Duration::from_millis(150);
const MAX_REPLY_BYTES: usize = 64;
const ESC_HOLD: Duration = Duration::from_millis(50);
const REPLY_IDLE_TIMEOUT: Duration = Duration::from_millis(150);
type Rgb = (u8, u8, u8);

/// Accept only one complete, bounded OSC 11 rgb reply (BEL or ST terminated).
fn parse_reply(reply: &[u8]) -> Option<Rgb> {
    if reply.len() > MAX_REPLY_BYTES { return None; }
    let payload = reply.strip_prefix(b"\x1b]11;rgb:")?;
    let payload = payload.strip_suffix(b"\x07")
        .or_else(|| payload.strip_suffix(b"\x1b\\"))?;
    let mut channels = payload.split(|b| *b == b'/');
    let mut component = || {
        let digits = channels.next()?;
        if digits.is_empty() || digits.len() > 4 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let value = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
        let maximum = (1u32 << (digits.len() * 4)) - 1;
        Some(((value * 255 + maximum / 2) / maximum) as u8)
    };
    let rgb = (component()?, component()?, component()?);
    channels.next().is_none().then_some(rgb)
}

pub(super) struct QueryResult {
    pub background: Option<Rgb>,
    pub filter: ReplyFilter,
}

/// Only OSC 11 is withheld. A mismatching prefix is replayed unchanged.
#[derive(Default)]
pub(super) struct ReplyFilter {
    enabled: bool,
    prefix: Vec<Event>,
    prefix_bytes: Vec<u8>,
    escape_started: Option<Instant>,
    last_activity: Option<Instant>,
    reply: Vec<u8>,
    receiving: bool,
    escape: bool,
    overflow: bool,
    background: Option<Rgb>,
    pending: VecDeque<Event>,
}
impl ReplyFilter {
    #[cfg(any(unix, test))]
    fn enabled() -> Self { Self { enabled: true, ..Self::default() } }

    fn bytes(event: &Event) -> Option<Vec<u8>> {
        let Event::Key(key) = event else { return None; };
        if key.kind == KeyEventKind::Release { return None; }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, KeyModifiers::NONE) => Some(vec![0x1b]),
            (KeyCode::Char('g'), KeyModifiers::CONTROL) => Some(vec![0x07]),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) if c.is_ascii() => Some(vec![c as u8]),
            (KeyCode::Char(c), KeyModifiers::ALT) if c.is_ascii() => Some(vec![0x1b, c as u8]),
            _ => None,
        }
    }

    #[cfg(test)]
    fn consume(&mut self, event: Event) {
        self.consume_at(event, Instant::now());
    }

    fn consume_at(&mut self, event: Event, now: Instant) {
        self.recover(now);
        if !self.enabled {
            self.pending.push_back(event);
            return;
        }
        let Some(bytes) = Self::bytes(&event) else {
            // Resize/paste/mouse events have no OSC bytes and retain their order.
            if !self.receiving && matches!(event, Event::Key(_)) { self.release_prefix(); }
            self.pending.push_back(event);
            return;
        };
        if self.receiving {
            self.last_activity = Some(now);
            for byte in bytes {
                if self.reply.len() < MAX_REPLY_BYTES { self.reply.push(byte); }
                else { self.overflow = true; }
                if byte == 0x07 || (self.escape && byte == b'\\') {
                    if !self.overflow && self.background.is_none() {
                        self.background = parse_reply(&self.reply);
                    }
                    self.receiving = false;
                    self.enabled = false;
                    self.last_activity = None;
                    self.escape = false;
                    self.reply.clear();
                    self.overflow = false;
                } else { self.escape = byte == 0x1b; }
            }
            return;
        }
        const PREFIX: &[u8] = b"\x1b]11;";
        self.prefix_bytes.extend_from_slice(&bytes);
        self.prefix.push(event);
        self.last_activity = Some(now);
        self.escape_started = if self.prefix_bytes == [0x1b] { Some(now) } else { None };
        if self.prefix_bytes == PREFIX {
            self.reply.extend_from_slice(PREFIX);
            self.prefix.clear();
            self.prefix_bytes.clear();
            self.receiving = true;
        } else if !PREFIX.starts_with(&self.prefix_bytes) {
            // A delayed response can follow an unrelated standalone Esc.
            // Replay the older candidate, then recognize a fresh OSC prefix.
            if self.prefix.len() > 1 && bytes.first() == Some(&0x1b) {
                let current = self.prefix.pop().expect("prefix contains current event");
                self.release_prefix();
                self.consume_at(current, now);
            } else { self.release_prefix(); }
        }
    }

    fn release_prefix(&mut self) {
        self.pending.extend(self.prefix.drain(..));
        self.prefix_bytes.clear();
        self.escape_started = None;
        self.last_activity = None;
    }

    fn release_escape_if_due(&mut self, now: Instant) {
        if self.escape_started.is_some_and(|started| now.saturating_duration_since(started) >= ESC_HOLD) {
            self.release_prefix();
        }
    }

    fn recover(&mut self, now: Instant) {
        self.release_escape_if_due(now);
        if self.last_activity.is_some_and(|last| now.saturating_duration_since(last) >= REPLY_IDLE_TIMEOUT) {
            if self.receiving {
                self.receiving = false;
                self.reply.clear();
                self.overflow = false;
                self.escape = false;
                self.last_activity = None;
            } else { self.release_prefix(); }
        }
    }

    /// Nonblocking event source for the regular loop. Up to 256 response events
    /// are discarded per call for fairness; partial replies have an idle deadline.
    pub(super) fn read(&mut self) -> io::Result<Option<Event>> {
        self.read_from(|| {
            if event::poll(Duration::ZERO)? { event::read().map(Some) } else { Ok(None) }
        }, Instant::now)
    }

    fn read_from(&mut self, mut next: impl FnMut() -> io::Result<Option<Event>>, now: impl Fn() -> Instant) -> io::Result<Option<Event>> {
        self.recover(now());
        if let Some(event) = self.pending.pop_front() { return Ok(Some(event)); }
        for _ in 0..256 {
            let Some(event) = next()? else { break; };
            self.consume_at(event, now());
            if let Some(event) = self.pending.pop_front() { return Ok(Some(event)); }
        }
        Ok(None)
    }
}

#[cfg(any(unix, test))]
fn negotiate(
    timeout: Duration,
    next: impl FnMut(Duration) -> io::Result<Option<Event>>,
) -> QueryResult {
    negotiate_with_clock(timeout, next, Instant::now)
}

#[cfg(any(unix, test))]
fn negotiate_with_clock(
    timeout: Duration,
    mut next: impl FnMut(Duration) -> io::Result<Option<Event>>,
    now: impl Fn() -> Instant,
) -> QueryResult {
    let deadline = now() + timeout;
    let mut filter = ReplyFilter::enabled();
    while let Some(remaining) = deadline.checked_duration_since(now()) {
        // Cap preserved startup input as well as reply storage. Any following
        // events remain in Crossterm and go through this same filter later.
        if filter.pending.len() >= 256 { break; }
        match next(remaining) {
            Ok(Some(event)) => {
                filter.consume_at(event, now());
                if !filter.enabled { break; }
            },
            Ok(None) | Err(_) => break,
        }
    }
    QueryResult { background: filter.background, filter }
}

/// Call after raw mode is enabled, before the first frame and regular event loop.
/// No second event reader or thread is created. Disabled/unsupported terminals
/// receive no query, so there is no reply to leak into keyboard input.
pub(super) fn query_background() -> QueryResult {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal()
        || !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
        || matches!(std::env::var("TERM").as_deref(), Ok("linux" | "dumb")) {
        return QueryResult { background: None, filter: ReplyFilter::default() };
    }
    // Windows native console input does not promise raw OSC response delivery.
    // Do not issue a query there: COLORFGBG and the dark default remain bounded.
    #[cfg(not(unix))]
    { QueryResult { background: None, filter: ReplyFilter::default() } }
    #[cfg(unix)]
    {
        if write_query().is_err() {
            // Even a partial output write may provoke a response: keep filtering.
            return QueryResult { background: None, filter: ReplyFilter::enabled() };
        }
        negotiate(QUERY_TIMEOUT, |remaining| {
            if event::poll(remaining)? { event::read().map(Some) } else { Ok(None) }
        })
    }
}

#[cfg(unix)]
#[allow(unsafe_code)] // A nonblocking output guard prevents a congested TTY hanging startup.
fn write_query() -> io::Result<()> {
    use std::io::Write;
    struct Restore(libc::c_int);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: stdout stays live; restore exactly its previous flags.
            unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, self.0); }
        }
    }
    // SAFETY: borrowed live stdout descriptor, flag operations only.
    let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
    if flags < 0 { return Err(io::Error::last_os_error()); }
    if unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let _restore = Restore(flags);
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"\x1b]11;?\x1b\\")?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(c: char) -> Event { Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)) }
    fn reply_events(payload: &str, st: bool) -> Vec<Event> {
        let mut events = vec![Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT))];
        events.extend(payload.chars().map(key));
        events.push(if st { Event::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT)) }
            else { Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)) });
        events
    }

    #[test]
    fn late_split_reply_with_startup_state_updates_keeps_chat_live() {
        use crate::{chat::{ChatRooms, Frame}, state::Model, strings};
        use super::super::{app::App, sync_chat_state};
        use std::cell::Cell;
        use tokio::sync::watch;

        // Logical time and scripted reads make every ordering reproducible,
        // independent of thread scheduling, sockets and the host terminal.
        let mut separated = vec![Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))];
        separated.extend("]11;rgb:1010/2b2b/4848".chars().map(key));
        separated.push(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        separated.push(key('\\'));
        for reply in [reply_events("11;rgb:1010/2b2b/4848", true), separated] {
            for split in 0..=reply.len() {
                let start = Instant::now();
                let now = Cell::new(start);
                let (chat_tx, mut chat_rx) = watch::channel(ChatRooms::default());
                let (feed_tx, feed_rx) = watch::channel(Model::default());
                let mut app = App::default();
                let mut query = negotiate_with_clock(QUERY_TIMEOUT, |remaining| {
                    assert_eq!(remaining, QUERY_TIMEOUT);
                    now.set(start + Duration::from_millis(75));
                    let mut rooms = ChatRooms::default();
                    rooms.apply(Frame::parse(r#"{"type":"session_info","session_id":"base"}"#).unwrap());
                    chat_tx.send_replace(rooms);
                    let mut model = Model::default();
                    model.employees.items = vec![serde_json::from_str(
                        r#"{"name":"adu","display_name":"阿嘟","status":"active","archived":false}"#,
                    ).unwrap()];
                    model.employees.stale = false;
                    model.employees.refetch_pending = false;
                    model.inbox.stale = false;
                    model.inbox.refetch_pending = false;
                    feed_tx.send_replace(model);
                    now.set(start + QUERY_TIMEOUT);
                    Ok(None)
                }, || now.get());
                assert!(query.background.is_none());
                assert!(chat_rx.has_changed().unwrap());
                assert!(feed_rx.has_changed().unwrap());
                // The feed snapshot here is a stress fixture: production starts its
                // StateFeed only after negotiation. The same initial projection and
                // subsequent refresh/sync handoff below are used in the real loop.
                // Production consumes the first chat snapshot before refresh has
                // created a chat pane. No further WebChat message is needed/sent.
                app.sync_chats(&chat_rx.borrow_and_update());
                let mut revision = app.blocks.revision;
                assert!(!chat_rx.has_changed().unwrap());
                for (index, chunk) in [&reply[..split], &reply[split..]].into_iter().enumerate() {
                    now.set(start + QUERY_TIMEOUT + Duration::from_millis(10 + index as u64 * 20));
                    let mut events = chunk.iter().cloned();
                    assert_eq!(query.filter.read_from(|| Ok(events.next()), || now.get()).unwrap(), None);
                    // The regular loop continues refreshing while a reply is partial.
                    app.refresh(&feed_rx.borrow());
                    sync_chat_state(&mut app, &mut chat_rx, &mut revision);
                    assert!(app.chat.connected, "split {split}, read {index}");
                    assert!(!app.chat.stale, "fresh chat must clear its stale marker");
                    assert_eq!(app.chat.status.visible(), strings::CONNECTED);
                }
                assert_eq!(query.filter.background, Some((16, 43, 72)));
                assert!(!chat_rx.has_changed().unwrap());
                assert!(!feed_rx.borrow().employees.stale);
                let mut keys = [key('q')].into_iter();
                assert_eq!(query.filter.read_from(|| Ok(keys.next()), || now.get()).unwrap(), Some(key('q')));
            }
        }
    }

    #[test]
    fn valid_bel_st_and_channel_precisions() {
        assert_eq!(parse_reply(b"\x1b]11;rgb:1010/2b2b/4848\x07"), Some((16, 43, 72)));
        assert_eq!(parse_reply(b"\x1b]11;rgb:f5/f4/f0\x1b\\"), Some((245, 244, 240)));
        assert_eq!(parse_reply(b"\x1b]11;rgb:f/0/8\x07"), Some((255, 0, 136)));
        assert_eq!(parse_reply(b"\x1b]11;rgb:FFF/000/888\x07"), Some((255, 0, 136)));
        for st in [true, false] {
            let mut filter = ReplyFilter::enabled();
            for event in reply_events("11;rgb:1010/2b2b/4848", st) { filter.consume(event); }
            assert_eq!(filter.background, Some((16, 43, 72)));
            assert!(filter.pending.is_empty());
            assert!(!filter.receiving);
        }
    }

    #[test]
    fn malformed_and_oversized_replies_are_refused_and_drained() {
        for reply in [b"rgb:ff/ff/ff\x07".as_slice(), b"\x1b]10;rgb:ff/ff/ff\x07", b"\x1b]11;rgb:ff//ff\x07", b"\x1b]11;rgb:gg/ff/ff\x07", b"\x1b]11;rgb:fffff/ff/ff\x07", b"\x1b]11;rgb:ff/ff/ff/ff\x07", b"\x1b]11;rgb:ff/ff/ff", b"\x1b]11;rgb:ff/ff/ff\x07x"] {
            assert_eq!(parse_reply(reply), None, "{reply:?}");
        }
        assert_eq!(parse_reply(&[b'x'; MAX_REPLY_BYTES + 1]), None);
        let mut filter = ReplyFilter::enabled();
        for event in reply_events(&format!("11;rgb:{}", "f".repeat(4096)), true) { filter.consume(event); }
        assert!(filter.background.is_none());
        assert!(filter.pending.is_empty());
        assert!(!filter.receiving);
        assert!(filter.reply.is_empty());
        filter.consume(key('q'));
        assert_eq!(filter.pending.pop_front(), Some(key('q')));
    }

    #[test]
    fn timeout_and_late_partial_reply_never_become_keys() {
        let started = Instant::now();
        let query = negotiate(Duration::from_millis(5), |remaining| {
            std::thread::sleep(remaining);
            Ok(None)
        });
        assert!(query.background.is_none());
        assert!(started.elapsed() < Duration::from_millis(250));
        let all = reply_events("11;rgb:ff/ff/ff", true);
        for split in 0..=all.len() {
            let mut first = all[..split].iter().cloned();
            let mut query = negotiate(QUERY_TIMEOUT, |_| Ok(first.next()));
            for event in &all[split..] { query.filter.consume(event.clone()); }
            assert_eq!(query.filter.background, Some((255, 255, 255)), "split {split}");
            assert!(query.filter.pending.is_empty(), "split {split}");
            query.filter.consume(key('q'));
            assert_eq!(query.filter.pending.pop_front(), Some(key('q')));
        }
    }

    #[test]
    fn unrelated_keys_paste_resize_and_unknown_osc_are_preserved() {
        let unrelated = [key('q'), Event::Paste("中文\nmessage".into()), Event::Resize(80, 24),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), key('x')];
        let mut filter = ReplyFilter::enabled();
        for event in unrelated.iter().cloned() { filter.consume(event); }
        assert_eq!(filter.pending.iter().cloned().collect::<Vec<_>>(), unrelated);
        filter.pending.clear();
        let other_osc = reply_events("10;rgb:ff/ff/ff", false);
        for event in other_osc.iter().cloned() { filter.consume(event); }
        assert_eq!(filter.pending.iter().cloned().collect::<Vec<_>>(), other_osc);
    }

    #[test]
    fn timeout_does_not_hold_an_ordinary_escape_forever() {
        let mut filter = ReplyFilter::enabled();
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        filter.consume(escape.clone());
        let started = filter.escape_started.unwrap();
        filter.release_escape_if_due(started + ESC_HOLD - Duration::from_millis(1));
        assert!(filter.pending.is_empty());
        filter.release_escape_if_due(started + ESC_HOLD);
        assert_eq!(filter.pending.pop_front(), Some(escape));
        for event in reply_events("11;rgb:0/0/0", true) { filter.consume(event); }
        assert_eq!(filter.background, Some((0, 0, 0)));
        assert!(filter.pending.is_empty());
    }

    #[test]
    fn unterminated_reply_and_prefix_recover_after_idle_window() {
        for input in ["11;rgb:ff/", "1"] {
            let mut filter = ReplyFilter::enabled();
            filter.consume(Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)));
            for c in input.chars() { filter.consume(key(c)); }
            let last = filter.last_activity.unwrap();
            filter.recover(last + REPLY_IDLE_TIMEOUT);
            assert!(!filter.receiving);
            assert!(filter.prefix.is_empty());
            assert!(filter.reply.is_empty());
            filter.pending.clear();
            filter.consume(key('q'));
            assert_eq!(filter.pending.pop_front(), Some(key('q')));
        }
        // The first ordinary key after the deadline triggers recovery itself.
        let mut filter = ReplyFilter::enabled();
        let mut partial = reply_events("11;rgb:ff/", true);
        partial.pop();
        for event in partial { filter.consume(event); }
        assert!(filter.receiving);
        filter.last_activity = Some(Instant::now() - REPLY_IDLE_TIMEOUT);
        filter.consume(key('i'));
        assert_eq!(filter.pending.pop_front(), Some(key('i')));
    }

    #[test]
    fn split_escape_and_st_are_drained() {
        let mut filter = ReplyFilter::enabled();
        filter.consume(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        for c in "]11;rgb:0/0/0".chars() { filter.consume(key(c)); }
        filter.consume(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        filter.consume(key('\\'));
        assert_eq!(filter.background, Some((0, 0, 0)));
        assert!(filter.pending.is_empty());
    }
}

#[cfg(all(test, unix))]
#[allow(unsafe_code)] // An isolated PTY child exercises Crossterm's actual parser.
mod pty_tests {
    use super::*;
    use std::{fs::File, io::{Read, Write}, os::fd::{AsRawFd, FromRawFd}, process::{Command, Stdio}};

    #[test]
    fn real_terminal_reply_and_normal_key_use_one_reader() {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: writable fd outputs; optional openpty arguments are null.
        assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) }, 0);
        // SAFETY: a successful openpty returned two newly owned descriptors.
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        // SAFETY: live master fd; nonblocking lets the parent enforce its deadline.
        assert_eq!(unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) }, 0);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ui::osc11::pty_tests::probe", "--ignored", "--nocapture"])
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut replied = false;
        let mut buffer = [0u8; 4096];
        loop {
            if let Ok(n) = master.read(&mut buffer) { output.extend_from_slice(&buffer[..n]); }
            if !replied && output.windows(8).any(|s| s == b"\x1b]11;?\x1b\\") {
                // Prefix crosses reads, including a separated ST terminator.
                master.write_all(b"q\x1b]11;rgb:1010/").unwrap();
                std::thread::sleep(Duration::from_millis(5));
                master.write_all(b"2b2b/4848\x1b").unwrap();
                std::thread::sleep(Duration::from_millis(5));
                master.write_all(b"\\").unwrap();
                replied = true;
            }
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{}", String::from_utf8_lossy(&output));
                assert!(replied, "child did not issue OSC 11");
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("OSC 11 child exceeded deadline: {}", String::from_utf8_lossy(&output));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    #[ignore = "run only by the isolated PTY parent test"]
    fn probe() {
        let _guard = super::super::terminal::TerminalGuard::enter().unwrap();
        let started = Instant::now();
        let mut result = query_background();
        assert_eq!(result.background, Some((16, 43, 72)));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(result.filter.read().unwrap(), Some(Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
        )));
        assert_eq!(result.filter.read().unwrap(), None);
    }
}
