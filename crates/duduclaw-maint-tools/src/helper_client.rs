//! The shell's side of the helper socket: send one token, relay the answer
//! (log lines, or the single status line of a restart).

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::protocol::{LogRequest, RestartTarget};
use crate::sanitize::TerminalFilter;

/// How often a blocked read wakes up to check for Ctrl-C.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How a relay ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayEnd {
    /// The service closed the connection (all lines sent).
    Eof,
    /// Ctrl-C. The connection is dropped, which makes the service stop its
    /// `journalctl -f`.
    Interrupted,
}

/// Connect to the helper, ask for log request `req`, copy the answer to `out`.
pub fn fetch<W: Write + ?Sized>(
    socket: &Path,
    req: LogRequest,
    out: &mut W,
    interrupted: &dyn Fn() -> bool,
) -> io::Result<RelayEnd> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(req.token().as_bytes())?;
    stream.write_all(b"\n")?;
    // Do not shut down the write half: the service treats end-of-input from
    // the client as "client gone" and stops following.
    stream.set_read_timeout(Some(POLL_INTERVAL))?;
    relay(&mut stream, out, interrupted)
}

/// Longest restart status line read from the helper.
pub const MAX_REPLY_BYTES: usize = 512;

/// How long to wait for a restart's status line: the helper waits up to
/// [`crate::restart::RESTART_WAIT`] for systemctl, plus a margin.
pub const RESTART_REPLY_TIMEOUT: Duration = Duration::from_secs(150);

/// Ask the helper to restart `target` and return its status line, filtered
/// for the terminal (`ok: …` or `error: …`).
pub fn request_restart(socket: &Path, target: RestartTarget) -> io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(target.token().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.set_read_timeout(Some(RESTART_REPLY_TIMEOUT))?;
    read_reply_line(&mut stream)
}

/// Read one bounded line (up to `\n`, end of input or [`MAX_REPLY_BYTES`]),
/// filtered for the terminal, without the newline.
pub fn read_reply_line<R: Read + ?Sized>(input: &mut R) -> io::Result<String> {
    let mut raw = Vec::with_capacity(64);
    let mut buf = [0u8; 128];
    while raw.len() < MAX_REPLY_BYTES && !raw.contains(&b'\n') {
        match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    raw.truncate(MAX_REPLY_BYTES);
    if let Some(i) = raw.iter().position(|&b| b == b'\n') {
        raw.truncate(i);
    }
    let filtered = crate::sanitize::filter_terminal_bytes(&raw);
    Ok(String::from_utf8_lossy(&filtered).trim().to_string())
}

/// Copy `input` to `out` through [`TerminalFilter`], until end of input or
/// `interrupted()`. Read timeouts are only wake-ups to check the flag.
pub fn relay<R: Read + ?Sized, W: Write + ?Sized>(
    input: &mut R,
    out: &mut W,
    interrupted: &dyn Fn() -> bool,
) -> io::Result<RelayEnd> {
    let mut buf = [0u8; 8192];
    let mut filtered = Vec::with_capacity(buf.len());
    let mut filter = TerminalFilter::default();
    loop {
        if interrupted() {
            return Ok(RelayEnd::Interrupted);
        }
        match input.read(&mut buf) {
            Ok(0) => {
                filtered.clear();
                filter.finish(&mut filtered);
                out.write_all(&filtered)?;
                out.flush()?;
                return Ok(RelayEnd::Eof);
            }
            Ok(n) => {
                filtered.clear();
                filter.feed(&buf[..n], &mut filtered);
                out.write_all(&filtered)?;
                out.flush()?;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;
    use std::os::unix::net::UnixListener;

    #[test]
    fn relays_until_eof_and_strips_controls() {
        let mut out = Vec::new();
        let end = relay(
            &mut Cursor::new(b"a\x1b[2Jb\nc\n".to_vec()),
            &mut out,
            &|| false,
        )
        .unwrap();
        assert_eq!(end, RelayEnd::Eof);
        assert_eq!(out, b"a [2Jb\nc\n");
    }

    /// A reader that times out forever, like a quiet `journalctl -f`.
    struct Quiet;
    impl Read for Quiet {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "timeout"))
        }
    }

    #[test]
    fn interrupt_ends_a_quiet_follow() {
        let polls = Cell::new(0);
        let flag = || {
            polls.set(polls.get() + 1);
            polls.get() > 3
        };
        let mut out = Vec::new();
        assert_eq!(
            relay(&mut Quiet, &mut out, &flag).unwrap(),
            RelayEnd::Interrupted
        );
    }

    #[test]
    fn reply_line_is_bounded_and_filtered() {
        assert_eq!(
            read_reply_line(&mut Cursor::new(b"ok: restarted x\nextra".to_vec())).unwrap(),
            "ok: restarted x"
        );
        assert_eq!(
            read_reply_line(&mut Cursor::new(b"error: \x1b[2Jbad".to_vec())).unwrap(),
            "error:  [2Jbad"
        );
        let long = read_reply_line(&mut Cursor::new(vec![b'a'; 10_000])).unwrap();
        assert_eq!(long.len(), MAX_REPLY_BYTES);
        assert_eq!(read_reply_line(&mut Cursor::new(Vec::new())).unwrap(), "");
    }

    #[test]
    fn fetch_sends_exactly_one_token_line() {
        let dir = std::env::temp_dir().join(format!("maint-helper-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut got = [0u8; 15];
            conn.read_exact(&mut got).unwrap();
            conn.write_all(b"line one\n").unwrap();
            got
        });
        let mut out = Vec::new();
        let req = LogRequest::ALL[1];
        let end = fetch(&path, req, &mut out, &|| false).unwrap();
        assert_eq!(end, RelayEnd::Eof);
        assert_eq!(&server.join().unwrap(), b"gateway-follow\n");
        assert_eq!(out, b"line one\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
