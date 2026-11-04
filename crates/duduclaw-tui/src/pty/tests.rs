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
        assert_eq!(settings.glyphs, crate::settings::Glyphs::default());
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

#[test]
fn mouse_encodings_modes_and_bounds_are_child_only() {
    use crossterm::event::{MouseEvent, MouseEventKind as K, MouseButton as B, KeyModifiers as M};
    use vt100::{MouseProtocolMode as Mode, MouseProtocolEncoding as Encoding};
    let event = |kind| MouseEvent { kind, column: 40, row: 20, modifiers: M::NONE };
    assert_eq!(mouse_bytes(event(K::Down(B::Left)), 3, 4, Mode::PressRelease, Encoding::Sgr), b"\x1b[<0;3;4M");
    assert_eq!(mouse_bytes(event(K::Up(B::Right)), 3, 4, Mode::PressRelease, Encoding::Sgr), b"\x1b[<2;3;4m");
    assert_eq!(mouse_bytes(event(K::Down(B::Left)), 3, 4, Mode::PressRelease, Encoding::Default), vec![27,91,77,32,35,36]);
    assert_eq!(mouse_bytes(event(K::Up(B::Right)), 3, 4, Mode::PressRelease, Encoding::Default), vec![27,91,77,35,35,36]);
    assert_eq!(mouse_bytes(event(K::Drag(B::Left)), 300, 4, Mode::ButtonMotion, Encoding::Utf8), "\x1b[M@Ō$".as_bytes());
    assert_eq!(mouse_bytes(event(K::Moved), 3, 4, Mode::AnyMotion, Encoding::Sgr), b"\x1b[<35;3;4M");
    for (kind, mode) in [(K::Moved, Mode::ButtonMotion), (K::Drag(B::Left), Mode::PressRelease), (K::Up(B::Left), Mode::Press), (K::Down(B::Left), Mode::None)] {
        assert!(mouse_bytes(event(kind), 3, 4, mode, Encoding::Sgr).is_empty());
    }
    assert!(mouse_bytes(event(K::Down(B::Left)), 224, 4, Mode::PressRelease, Encoding::Default).is_empty());
    assert!(mouse_bytes(event(K::Down(B::Left)), 0, 4, Mode::PressRelease, Encoding::Sgr).is_empty());
    let mut modified = event(K::ScrollDown); modified.modifiers = M::CONTROL | M::SHIFT;
    assert_eq!(mouse_bytes(modified, 3, 4, Mode::PressRelease, Encoding::Sgr), b"\x1b[<85;3;4M");
}

#[test]
fn waiting_signals_are_bounded_display_only_and_hostile_strings_never_escape() {
    for sequence in [b"\x1b]9;waiting\x07".as_slice(), b"\x1b]777;notify;child;needs input\x1b\\", b"\x1b]2;Waiting for you\x07"] {
        for split in 0..=sequence.len() {
            let mut filter = filter::Filter::default();
            let mut safe = Vec::new();
            filter.process(&sequence[..split], 24, 80, |b| safe.extend_from_slice(b));
            filter.process(&sequence[split..], 24, 80, |b| safe.extend_from_slice(b));
            assert_eq!(filter.waiting_seq, 1);
            // Preserve the existing harmless ST whitelist behavior; never OSC content.
            assert_eq!(safe, if sequence.ends_with(b"\x1b\\") { b"\x1b\\".to_vec() } else { Vec::new() });
        }
    }
    for sequence in [
        b"\x1b]52;c;bad\x07".to_vec(), b"\x1b]2;approve all\x07".to_vec(),
        b"\x1b]777;notify;bad\x07".to_vec(), b"\x1b]9;\x18cancel\x07".to_vec(),
        b"\x1b]9;bad\x1b]52;c;secret\x07".to_vec(), b"\x1b]2;Waiting for you\x1b[?1006h".to_vec(),
        [b"\x1b]9;".as_slice(), &vec![b'a'; 1024], b"\x07"].concat(),
    ] {
        let mut filter = filter::Filter::default();
        let mut safe = Vec::new();
        filter.process(&sequence, 24, 80, |b| safe.extend_from_slice(b));
        assert_eq!(filter.waiting_seq, 0);
        assert!(!safe.windows(2).any(|b| b == b"\x1b]"));
    }
    let mut filter = filter::Filter::default();
    let mut parser = vt100::Parser::new(24, 80, 0);
    filter.process(b"\x1b]52;c;\x1b[?1000;1006h\x1b]777;notify;child;question\x07", 24, 80, |b| parser.process(b));
    assert_eq!(parser.screen().mouse_protocol_mode(), vt100::MouseProtocolMode::PressRelease);
    assert_eq!(parser.screen().mouse_protocol_encoding(), vt100::MouseProtocolEncoding::Sgr);
    assert_eq!(filter.waiting_seq, 1);
}

#[test]
fn urxvt_and_sgr_pixels_exact_mouse_bytes() {
    use crossterm::event::{MouseEvent, MouseEventKind as K, MouseButton as B};
    use vt100::MouseProtocolMode as Mode;
    use MouseEncoding as E;
    let cases: &[(K, M, &[u8], &[u8])] = &[
        (K::Down(B::Left), M::NONE, b"\x1b[32;17;33M", b"\x1b[<0;17;33M"),
        (K::Down(B::Middle), M::NONE, b"\x1b[33;17;33M", b"\x1b[<1;17;33M"),
        (K::Down(B::Right), M::NONE, b"\x1b[34;17;33M", b"\x1b[<2;17;33M"),
        (K::Up(B::Left), M::NONE, b"\x1b[35;17;33M", b"\x1b[<0;17;33m"),
        (K::Up(B::Right), M::NONE, b"\x1b[35;17;33M", b"\x1b[<2;17;33m"),
        (K::ScrollUp, M::NONE, b"\x1b[96;17;33M", b"\x1b[<64;17;33M"),
        (K::ScrollDown, M::NONE, b"\x1b[97;17;33M", b"\x1b[<65;17;33M"),
        (K::ScrollLeft, M::NONE, b"\x1b[98;17;33M", b"\x1b[<66;17;33M"),
        (K::ScrollRight, M::NONE, b"\x1b[99;17;33M", b"\x1b[<67;17;33M"),
        (K::Down(B::Left), M::SHIFT, b"\x1b[36;17;33M", b"\x1b[<4;17;33M"),
        (K::Down(B::Left), M::ALT, b"\x1b[40;17;33M", b"\x1b[<8;17;33M"),
        (K::Down(B::Left), M::CONTROL, b"\x1b[48;17;33M", b"\x1b[<16;17;33M"),
        (K::Up(B::Right), M::SHIFT | M::ALT | M::CONTROL, b"\x1b[63;17;33M", b"\x1b[<30;17;33m"),
        (K::ScrollDown, M::SHIFT | M::ALT | M::CONTROL, b"\x1b[125;17;33M", b"\x1b[<93;17;33M"),
    ];
    for &(kind, modifiers, urxvt, pixels) in cases {
        let event = MouseEvent { kind, column: 65535, row: 65535, modifiers };
        assert_eq!(mouse_bytes(event, 17, 33, Mode::PressRelease, E::Urxvt), urxvt);
        assert_eq!(mouse_bytes(event, 17, 33, Mode::PressRelease, E::SgrPixels), pixels);
        for encoding in [E::Urxvt, E::SgrPixels] {
            assert!(mouse_bytes(event, 17, 33, Mode::None, encoding).is_empty());
            assert!(mouse_bytes(event, 0, 33, Mode::PressRelease, encoding).is_empty());
            assert!(mouse_bytes(event, 17, 0, Mode::PressRelease, encoding).is_empty());
        }
    }
    let event = |kind| MouseEvent { kind, column: 0, row: 0, modifiers: M::NONE };
    for encoding in [E::Urxvt, E::SgrPixels] {
        assert!(mouse_bytes(event(K::Up(B::Left)), 1, 1, Mode::Press, encoding).is_empty());
        assert!(mouse_bytes(event(K::Drag(B::Left)), 1, 1, Mode::PressRelease, encoding).is_empty());
        assert!(mouse_bytes(event(K::Moved), 1, 1, Mode::ButtonMotion, encoding).is_empty());
    }
    assert_eq!(mouse_bytes(event(K::Drag(B::Left)), 500, 200, Mode::ButtonMotion, E::Urxvt), b"\x1b[64;500;200M");
    assert_eq!(mouse_bytes(event(K::Moved), 4000, 3200, Mode::AnyMotion, E::SgrPixels), b"\x1b[<35;4000;3200M");
    assert_eq!(mouse_bytes(event(K::Down(B::Left)), 1, 1, Mode::PressRelease, E::Urxvt), b"\x1b[32;1;1M");
    assert_eq!(mouse_bytes(event(K::Down(B::Left)), u32::MAX, u32::MAX, Mode::PressRelease, E::SgrPixels), b"\x1b[<0;4294967295;4294967295M");
}

#[test]
fn pixel_coordinates_use_host_geometry_or_fallback_and_stay_inside_pane() {
    use crossterm::{event::{MouseEvent, MouseEventKind, MouseButton}, terminal::WindowSize};
    use ratatui::layout::Rect;
    let area = Rect::new(40, 10, 20, 4);
    let event = |column, row| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: M::NONE };
    for (column, row, expected) in [(40, 10, (1, 1)), (59, 10, (153, 1)), (40, 13, (1, 49)), (59, 13, (153, 49))] {
        let position = mouse_pixels(event(column, row), area, None).unwrap();
        assert_eq!(position, expected);
        assert!(position.0 <= 160 && position.1 <= 64);
    }
    let host = || Some(WindowSize { columns: 80, rows: 24, width: 800, height: 480 });
    assert_eq!(mouse_pixels(event(59, 13), area, host()), Some((191, 61)));
    assert_eq!(mouse_pixels(event(40, 10), area, host()), Some((1, 1)));
    let (x, y) = mouse_pixels(event(59, 13), area, host()).unwrap();
    assert_eq!(mouse_bytes(event(59, 13), x, y, vt100::MouseProtocolMode::PressRelease, MouseEncoding::SgrPixels), b"\x1b[<0;191;61M");
    for (column, row) in [(39, 10), (60, 10), (40, 9), (40, 14), (u16::MAX, u16::MAX)] {
        assert_eq!(mouse_pixels(event(column, row), area, host()), None);
    }
    assert_eq!(mouse_pixels(event(40, 10), Rect::new(40, 10, 0, 4), None), None);
    assert_eq!(mouse_pixels(event(40, 10), Rect::new(40, 10, 20, 0), None), None);
    for size in [
        WindowSize { columns: 0, rows: 0, width: 800, height: 480 },
        WindowSize { columns: 80, rows: 24, width: 0, height: 0 },
        WindowSize { columns: 80, rows: 24, width: 1, height: 1 },
        WindowSize { columns: 80, rows: 24, width: 800, height: 0 },
    ] {
        assert_eq!(mouse_pixels(event(59, 13), area, Some(size)), Some((153, 49)));
    }
    let huge = Some(WindowSize { columns: 1, rows: 1, width: u16::MAX, height: u16::MAX });
    assert_eq!(mouse_pixels(event(65534, 65534), Rect::new(0, 0, 65535, 65535), huge), Some((4294770691, 4294770691)));
}

#[test]
fn mouse_encoding_precedence_survives_every_mode_subset_and_reset_order() {
    use MouseEncoding as E;
    let modes = [1005, 1015, 1006, 1016];
    let encodings = [E::Utf8, E::Urxvt, E::Sgr, E::SgrPixels];
    for mask in 0..16 {
        for reverse in [false, true] {
            let mut filter = filter::Filter::default();
            for i in 0..4 {
                let i = if reverse { 3 - i } else { i };
                if mask & (1 << i) != 0 {
                    filter.process(format!("\x1b[?{}h", modes[i]).as_bytes(), 24, 80, |_| {});
                }
            }
            let mut remaining = mask;
            for i in (0..4).rev() {
                let expected = (0..4).rev().find(|bit| remaining & (1 << bit) != 0).map(|bit| encodings[bit]).unwrap_or(E::Default);
                assert_eq!(filter.mouse_encoding(), expected);
                filter.process(format!("\x1b[?{}l", modes[i]).as_bytes(), 24, 80, |_| {});
                remaining &= !(1 << i);
            }
            assert_eq!(filter.mouse_encoding(), E::Default);
        }
    }
}

#[test]
fn hostile_child_mouse_toggles_are_streaming_atomic_and_reporting_stays_required() {
    use crossterm::event::{MouseEvent, MouseEventKind, MouseButton};
    use vt100::MouseProtocolMode as Mode;
    use MouseEncoding as E;
    let mut filter = filter::Filter::default();
    let mut parser = vt100::Parser::new(24, 80, 0);
    let event = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 40, row: 10, modifiers: M::NONE };
    let steps: &[(&[u8], E, Mode, &[u8])] = &[
        (b"\x1b[?1016h", E::SgrPixels, Mode::None, b""),
        (b"\x1b[?1000h", E::SgrPixels, Mode::PressRelease, b"\x1b[<0;1;1M"),
        (b"\x1b[?1005;1015;1006h", E::SgrPixels, Mode::PressRelease, b"\x1b[<0;1;1M"),
        (b"\x1b[?1016", E::SgrPixels, Mode::PressRelease, b"\x1b[<0;1;1M"),
        (b"l", E::Sgr, Mode::PressRelease, b"\x1b[<0;1;1M"),
        (b"\x1b[?1006l", E::Urxvt, Mode::PressRelease, b"\x1b[32;1;1M"),
        (b"\x1b]2;\x1b[?1016\x18", E::Urxvt, Mode::PressRelease, b"\x1b[32;1;1M"),
        (b"\x1b[?1016:1h\x1b[?1016 h", E::Urxvt, Mode::PressRelease, b"\x1b[32;1;1M"),
        (b"\x1b[?0001016h", E::SgrPixels, Mode::PressRelease, b"\x1b[<0;1;1M"),
        (b"\x1b[?1016;1015l", E::Utf8, Mode::PressRelease, b"\x1b[M !!"),
        (b"\x1b[?1005l", E::Default, Mode::PressRelease, b"\x1b[M !!"),
        (b"\x1b[?1015h\x1b[?1000l", E::Urxvt, Mode::None, b""),
        (b"\x1bc", E::Default, Mode::None, b""),
    ];
    for &(bytes, encoding, mode, expected) in steps {
        for byte in bytes { filter.process(&[*byte], 24, 80, |safe| parser.process(safe)); }
        assert_eq!(filter.mouse_encoding(), encoding);
        assert_eq!(parser.screen().mouse_protocol_mode(), mode);
        assert_eq!(mouse_bytes(event, 1, 1, mode, encoding), expected);
    }
    // Overlong sequences, excess parameters and OSC text cannot set a mode.
    for bytes in [
        [b"\x1b[?".as_slice(), &[b'0'; 128], b"1016h"].concat(),
        [b"\x1b[?".as_slice(), b"1;".repeat(32).as_slice(), b"1016h"].concat(),
        b"\x1b]2;?1016h\x07".to_vec(),
        b"\x1b[?9999999999999999991016h".to_vec(),
        b"\x1b[?1006 ;1016h".to_vec(),
        b"\x1b[?1016;1 h".to_vec(),
        b"\x1b[?1006:1;1016h".to_vec(),
    ] {
        for split in 0..=bytes.len() {
            let mut filter = filter::Filter::default();
            filter.process(&bytes[..split], 24, 80, |_| {});
            filter.process(&bytes[split..], 24, 80, |_| {});
            assert_eq!(filter.mouse_encoding(), E::Default);
        }
    }
}

#[test]
fn idle_poll_does_not_request_redraw() {
    let mut session = super::Session::default();
    assert!(!session.poll());
    assert!(!session.poll());
}
