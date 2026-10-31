use super::*;
use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers as M};
#[test]
fn input_escape_controls_application_cursor_and_paste() {
    assert!(escape(KeyEvent::new(K::Char('\\'), M::CONTROL)));
    for (key, expected) in [
        (KeyEvent::new(K::Esc, M::NONE), b"\x1b".as_slice()),
        (KeyEvent::new(K::Char('c'), M::CONTROL), b"\x03"),
        (KeyEvent::new(K::Char('p'), M::CONTROL), b"\x10"),
        (KeyEvent::new(K::PageUp, M::NONE), b"\x1b[5~"),
    ] {
        assert!(!escape(key));
        assert_eq!(key_bytes(key, false), expected);
    }
    assert_eq!(key_bytes(KeyEvent::new(K::Up, M::NONE), true), b"\x1bOA");
    assert_eq!(
        key_bytes(KeyEvent::new(K::Left, M::CONTROL), false),
        b"\x1b[1;5D"
    );
    assert_eq!(paste_bytes("繁體\n測試", false), "繁體\n測試".as_bytes());
    assert_eq!(
        paste_bytes("text\x1b[201~\x03", true),
        b"\x1b[200~text[201~\x1b[201~"
    );
}
#[test]
fn parser_hostile_streams_never_become_terminal_commands() {
    let mut parser = vt100::Parser::new(24, 80, SCROLLBACK);
    let mut filter = filter::Filter::default();
    let bytes = b"safe\x1b]52;c;CLIPBOARD_CANARY\x07\x1b]0;TITLE_CANARY\x07\x1b]2;TITLE2_CANARY\x1b\\\x1b]8;;https://evil.invalid\x1b\\link\x1b]8;;\x1b\\\x1bPqSIXEL_CANARY\x1b\\\x1bP1;2|DCS_CANARY\x1b\\\x1b[31;1mRED";
    for chunk in bytes.chunks(3) {
        filter.process(chunk, 24, 80, |safe| parser.process(safe));
    }
    assert_eq!(parser.screen().contents(), "safelinkRED");
    assert!(parser.screen().cell(0, 8).unwrap().bold());
}
#[test]
fn resize_and_scrollback_are_bounded() {
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    let mut session = Session::default();
    session.parser = Some(parser::ParserWorker::spawn(rx, 24, 80).unwrap());
    session.resize(6, 20);
    for _ in 0..60 { tx.send(b"line\r\n".repeat(100)).unwrap(); }
    drop(tx);
    session.scroll_offset = SCROLLBACK;
    session.parser.as_ref().unwrap().scroll(SCROLLBACK);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        session.poll();
        if session.screen().size() == (6, 20) && session.screen().scrollback() == SCROLLBACK { break; }
        // Reapply after all queued lines have entered scrollback.
        session.parser.as_ref().unwrap().scroll(0);
        std::thread::sleep(std::time::Duration::from_millis(20));
        session.parser.as_ref().unwrap().scroll(SCROLLBACK);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(std::time::Instant::now() < deadline);
    }
    session.scroll(false);
    assert_eq!(session.scroll_offset, SCROLLBACK - 5);
    session.resize(0, u16::MAX);
    assert_eq!((session.size().rows, session.size().cols), (1, 500));
}
#[test]
fn owner_environment_strips_all_identity_markers_and_has_no_session_token() {
    let cmd = command_with_env(
        [
            ("SHELL".into(), "/bin/sh".into()),
            ("COMSPEC".into(), "/bin/sh".into()),
            ("PATH".into(), "/usr/bin".into()),
            ("OWNER_VALUE".into(), "preserved".into()),
            ("DUDUCLAW_AGENT_TOKEN".into(), "agent-canary".into()),
            ("DUDUCLAW_TURN_ID".into(), "turn-canary".into()),
            ("duduclaw_future_marker".into(), "unknown-canary".into()),
        ],
    );
    assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("/bin/sh")]);
    let env: Vec<_> = cmd.iter_full_env_as_str().collect();
    assert!(env.contains(&("OWNER_VALUE", "preserved")));
    assert!(
        !env.iter()
            .any(|(k, _)| k.to_ascii_uppercase().starts_with("DUDUCLAW_"))
    );
    assert!(!env.iter().any(|(_, v)| v.contains("canary")));
}
#[test]
fn old_local_command_configuration_is_ignored_with_notice() {
    for input in [
        "[pty]\ncommand = ['claude', '--help']",
        "[pty]\ncommand = 'sh -c echo unsafe'",
        "[pty]\ncommand = []",
        "[pty]\ncommand = ['']",
        "[pty]\ncommand = [1]",
        "[pty]\ncommand = ['sh']\nenv = { TOKEN = 'x' }",
    ] {
        let (settings, notice) = crate::settings::Settings::parse(input);
        assert_eq!(notice, Some(strings::PTY_COMMAND_IGNORED));
        assert_eq!(settings.glyphs, crate::settings::Glyphs::Ascii);
    }
    let (settings, notice) = crate::settings::Settings::parse("glyphs = 'unicode'\n[pty]\ncommand = ['evil']");
    assert_eq!(notice, Some(strings::PTY_COMMAND_IGNORED));
    assert_eq!(settings.glyphs, crate::settings::Glyphs::Unicode);
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };
    fn wait_until(mut check: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(Instant::now() < until, "native PTY timeout");
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn child_exit_and_kernel_resize() {
        let mut session = Session::default();
        session.start_script_for_test("read line; stty size; printf '繁體é'; exit 7");
        assert!(session.running());
        session.resize(13, 41);
        session.send(b"go\n".to_vec());
        wait_until(|| {
            session.poll();
            !session.running()
        });
        wait_until(|| {
            session.poll();
            session.screen().contents().contains("13 41")
        });
        assert!(
            session
                .status
                .contains(&format!("{} 7", strings::PTY_EXITED))
        );
        assert!(session.screen().contents().contains("繁體é"));
    }
    #[test]
    fn close_kills_child_and_grandchild_in_owned_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pids");
        let script = format!("sleep 120 & echo $! > '{}' ; wait", path.display());
        let mut session = Session::default();
        session.start_script_for_test(&script);
        assert!(session.running());
        let pid = session.process.as_ref().unwrap().pid_for_test();
        wait_until(|| {
            std::fs::read_to_string(&path)
                .ok()
                .is_some_and(|s| s.trim().parse::<u32>().is_ok())
        });
        let grandchild: u32 = std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        session.stop();
        assert!(!session.cleanup_complete, "group cleanup cannot prove detached descendants stopped");
        wait_until(|| !duduclaw_core::platform::process_alive(pid));
        wait_until(|| !duduclaw_core::platform::process_alive(grandchild));
    }
    #[test]
    #[ignore = "sandbox denies /bin/ps session inventory; native tree acceptance NOT RUN"]
    fn close_kills_child_and_grandchild_in_separate_job_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pids");
        let script = format!(
            "set -m; sh -c 'sleep 120 & echo $! > \"$1\"; wait' fixture '{}' & wait",
            path.display()
        );
        let mut session = Session::default();
        session.start_script_for_test(&script);
        assert!(session.running());
        let pid = session.process.as_ref().unwrap().pid_for_test();
        wait_until(|| {
            std::fs::read_to_string(&path)
                .ok()
                .is_some_and(|s| s.trim().parse::<u32>().is_ok())
        });
        let grandchild: u32 = std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        session.stop();
        assert!(!session.running());
        assert!(!session.cleanup_complete);
        assert!(session.status.contains(strings::PTY_STOP_FAILED));
        wait_until(|| !duduclaw_core::platform::process_alive(pid));
        wait_until(|| !duduclaw_core::platform::process_alive(grandchild));
    }
}

#[test]
fn escape_accepts_legacy_enhanced_and_function_key_forms() {
    use crossterm::event::KeyEventKind;
    for key in [
        KeyEvent::new(K::Char('4'), M::CONTROL),
        KeyEvent::new(K::Char('\\'), M::CONTROL),
        KeyEvent::new(K::Char('\u{1c}'), M::CONTROL),
        KeyEvent::new(K::Char('\u{1c}'), M::NONE),
        KeyEvent::new(K::F(12), M::NONE),
    ] {
        assert!(escape(key), "{key:?}");
        assert!(!escape(KeyEvent { kind: KeyEventKind::Release, ..key }));
    }
    assert!(!escape(KeyEvent::new(K::Char('4'), M::NONE)));
    assert!(!escape(KeyEvent::new(K::Char('4'), M::ALT | M::CONTROL)));
}

#[test]
fn burst_of_5000_key_events_reaches_slow_child_in_order_as_one_batch() {
    use std::{io::{self, Write}, sync::{Arc, Mutex, mpsc}, time::Duration};
    struct SlowChild { bytes: Arc<Mutex<Vec<u8>>>, done: mpsc::Sender<()>, total: usize }
    impl Write for SlowChild {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_micros(50));
            let n = bytes.len().min(13); // Force partial writes.
            let mut received = self.bytes.lock().unwrap();
            received.extend_from_slice(&bytes[..n]);
            if received.len() == self.total { self.done.send(()).unwrap(); }
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let events: Vec<_> = "/private/tmp/claude-501/-Users-lizhixu-Project/繁體中文/"
        .chars().cycle().take(5000).map(|c| KeyEvent::new(K::Char(c), M::NONE)).collect();
    let expected: Vec<_> = events.iter().flat_map(|k| key_bytes(*k, false)).collect();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (done, completion) = mpsc::channel();
    let mut session = Session::default();
    session.input = Some(writer::InputWriter::spawn(SlowChild {
        bytes: bytes.clone(), done, total: expected.len(),
    }).unwrap());
    session.begin_input_batch();
    for key in events { session.send(key_bytes(key, false)); }
    assert_eq!(session.input_batch, expected);
    // Coalescing happens before the writer sees any of these 5,000 events.
    assert_eq!(session.input.as_ref().unwrap().pending(), 0);
    session.flush_input();
    completion.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(*bytes.lock().unwrap(), expected);
}

#[test]
fn byte_cap_counts_inflight_and_rejects_whole_chunks() {
    use std::{io::{self, Write}, sync::{Arc, Mutex, mpsc}, time::Duration};
    struct GatedChild { bytes: Arc<Mutex<Vec<u8>>>, gate: mpsc::Receiver<()>, started: mpsc::Sender<()> }
    impl Write for GatedChild {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.started.send(()).unwrap();
            self.gate.recv_timeout(Duration::from_secs(5)).unwrap();
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (release, gate) = mpsc::channel();
    let (started, writing) = mpsc::channel();
    let mut session = Session::default();
    session.input = Some(writer::InputWriter::spawn(GatedChild { bytes: bytes.clone(), gate, started }).unwrap());
    session.send(vec![b'a'; 64 * 1024]);
    writing.recv_timeout(Duration::from_secs(5)).unwrap();
    // Also exercise the unbounded-by-count queue while the first write is stalled.
    for _ in 0..5000 { session.send(vec![b'b']); }
    let pending = (64 * 1024) + 5000;
    assert_eq!(session.input.as_ref().unwrap().pending(), pending);
    session.send(vec![b'X'; MAX_INPUT - pending + 1]);
    assert_eq!(session.status, strings::PTY_INPUT_FULL);
    assert_eq!(session.input.as_ref().unwrap().pending(), pending);
    // Release every accepted chunk, then wait for the worker to drain.
    for _ in 0..5001 { release.send(()).unwrap(); }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while session.input.as_ref().unwrap().pending() != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let expected = [vec![b'a'; 64 * 1024], vec![b'b'; 5000]].concat();
    assert_eq!(*bytes.lock().unwrap(), expected);
    session.send(vec![b'X'; MAX_INPUT + 1]);
    assert_eq!(session.status, strings::PTY_INPUT_FULL);
    assert_eq!(session.input.as_ref().unwrap().pending(), 0);
}

#[test]
fn writer_failure_is_visible_without_another_keypress() {
    struct BrokenChild;
    impl std::io::Write for BrokenChild {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "test child stopped reading"))
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut session = Session::default();
    session.input = Some(writer::InputWriter::spawn(BrokenChild).unwrap());
    session.send(b"command".to_vec());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.input.as_ref().unwrap().failed() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    session.poll();
    assert_eq!(session.status, strings::PTY_INPUT_FAILED);
}
