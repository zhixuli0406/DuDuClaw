//! One connection of `duduclaw-maint-helper`: peer gate → token → either
//! filtered journal lines or one fixed restart.
//!
//! systemd (`Accept=yes`, `StandardInput=socket`) starts one instance per
//! connection with the connected socket on fd 0. [`serve_connection`] is the
//! whole life of that instance; the journal reader and the restart are
//! injected so tests can substitute fixtures.

use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::TimeZone;

use crate::logs_filter::{MAX_LINES_NON_FOLLOW, StreamStats, stream_records};
use crate::peer::{PeerRejection, authorize_peer};
use crate::protocol::{HelperRequest, LogRequest, MAX_TOKEN_LINE_BYTES, RestartTarget, TokenError};
use crate::restart::RestartResult;
use crate::tools::{CHILD_ENV, Tool, resolve_tool};

/// How long a client may take, in total, to send its token line.
pub const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one write to the client may block. A client that stays connected
/// but stops reading is treated as gone once this expires, so it cannot pin
/// a `journalctl -f` and one of the socket's connection slots.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Total time spent draining unread input before closing a refusal.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// A reader over a unix stream with one overall deadline: before every read
/// the socket's read timeout is set to the time left, so a client trickling
/// one byte at a time cannot stretch a per-read timeout indefinitely.
struct DeadlineReader<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline passed"));
        }
        self.stream.set_read_timeout(Some(left))?;
        (&*self.stream).read(buf)
    }
}

/// Fixed replies. Nothing from the request or from the system is echoed.
pub const REPLY_NOT_PERMITTED: &str = "error: not permitted\n";
pub const REPLY_BAD_REQUEST: &str = "error: unknown request\n";
pub const REPLY_UNAVAILABLE: &str = "error: journal unavailable\n";

/// How a connection ended.
#[derive(Debug)]
pub enum ServeOutcome {
    /// Peer uid check failed before any client input was parsed (pending
    /// input is only drained and discarded so the refusal reaches the client).
    Refused(PeerRejection),
    /// The request line was not one of the six tokens (or timed out).
    BadRequest(TokenReadError),
    /// `journalctl` could not be started.
    Unavailable(io::Error),
    /// A restart was requested; `result` was sent as one status line.
    Restart {
        target: RestartTarget,
        result: RestartResult,
    },
    /// Records were streamed. `client_gone` when the client hung up first.
    Streamed {
        request: LogRequest,
        stats: StreamStats,
        client_gone: bool,
        exit_code: Option<i32>,
    },
}

/// Why no valid token was read.
#[derive(Debug)]
pub enum TokenReadError {
    Token(TokenError),
    /// End of input before a newline.
    NoNewline,
    Io(io::Error),
}

/// Read one request line, byte by byte so nothing past the newline is
/// consumed, never more than [`MAX_TOKEN_LINE_BYTES`] + 1 bytes.
pub fn read_token_line<R: Read + ?Sized>(r: &mut R) -> Result<HelperRequest, TokenReadError> {
    let mut line = Vec::with_capacity(MAX_TOKEN_LINE_BYTES + 1);
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return Err(TokenReadError::NoNewline),
            Ok(_) if byte[0] == b'\n' => {
                return HelperRequest::parse_token(&line).map_err(TokenReadError::Token);
            }
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > MAX_TOKEN_LINE_BYTES {
                    return Err(TokenReadError::Token(TokenError::TooLong));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(TokenReadError::Io(e)),
        }
    }
}

/// Start `journalctl` for `req` with the fixed argv from
/// [`LogRequest::journalctl_args`], a cleared environment, stdout piped and
/// stderr discarded (its text would otherwise reach the client unfiltered).
pub fn spawn_journalctl(req: LogRequest) -> io::Result<Child> {
    let path = resolve_tool(Tool::Journalctl, |p| p.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "journalctl not found"))?;
    Command::new(path)
        .args(req.journalctl_args())
        .env_clear()
        .envs(CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

/// Serve one connection. In production `expected_uid` is
/// [`crate::protocol::MAINT_UID`], `spawn` is [`spawn_journalctl`] and
/// `restart` is [`crate::restart::restart_with_limit`] with the production
/// limiter.
pub fn serve_connection<Tz, F, R>(
    stream: &UnixStream,
    expected_uid: u32,
    spawn: F,
    restart: R,
    tz: &Tz,
) -> ServeOutcome
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
    F: FnOnce(LogRequest) -> io::Result<Child>,
    R: FnOnce(RestartTarget) -> RestartResult,
{
    let mut writer = stream;

    // 1. Who is calling — before reading a single byte from them.
    if let Err(rejection) = authorize_peer(stream, expected_uid) {
        finish_with_reply(stream, REPLY_NOT_PERMITTED);
        return ServeOutcome::Refused(rejection);
    }

    // 2. What they want: one of six tokens.
    let request = read_token_line(&mut DeadlineReader {
        stream,
        deadline: Instant::now() + TOKEN_TIMEOUT,
    })
    .and_then(|r| {
        stream
            .set_write_timeout(Some(WRITE_TIMEOUT))
            .map(|()| r)
            .map_err(TokenReadError::Io)
    });
    let request = match request {
        Ok(HelperRequest::Logs(r)) => r,
        Ok(HelperRequest::Restart(target)) => {
            // Same peer gate as the logs above; the unit comes from the
            // closed token set, never from the client's text.
            let result = restart(target);
            let mut line = result.reply(target);
            line.push('\n');
            finish_with_reply(stream, &line);
            return ServeOutcome::Restart { target, result };
        }
        Err(e) => {
            finish_with_reply(stream, REPLY_BAD_REQUEST);
            return ServeOutcome::BadRequest(e);
        }
    };

    // 3. Start the journal reader.
    let mut child = match spawn(request) {
        Ok(c) => c,
        Err(e) => {
            finish_with_reply(stream, REPLY_UNAVAILABLE);
            return ServeOutcome::Unavailable(e);
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        finish_with_reply(stream, REPLY_UNAVAILABLE);
        return ServeOutcome::Unavailable(io::Error::other("journal reader has no stdout"));
    };
    let child = Arc::new(Mutex::new(child));
    let client_gone = Arc::new(AtomicBool::new(false));

    // 4. Hang-up watcher: the client never sends anything after the token,
    //    so end-of-input (or an error) means it left. Stop the reader so a
    //    quiet `journalctl -f` does not hold one of the socket's four
    //    connection slots until RuntimeMaxSec.
    //    If the watcher cannot be set up, refuse rather than run a follow
    //    nobody can stop (fail closed).
    let watch = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
            }
            let _ = wait_child(&child);
            finish_with_reply(stream, REPLY_UNAVAILABLE);
            return ServeOutcome::Unavailable(e);
        }
    };
    {
        let child = Arc::clone(&child);
        let gone = Arc::clone(&client_gone);
        thread::spawn(move || {
            let _ = watch.set_read_timeout(None);
            let mut sink = [0u8; 256];
            loop {
                match (&watch).read(&mut sink) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            gone.store(true, Ordering::SeqCst);
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
            }
        });
    }

    // 5. Stream, bounded.
    let cap = if request.follow {
        None
    } else {
        Some(MAX_LINES_NON_FOLLOW)
    };
    let mut reader = BufReader::new(stdout);
    let result = stream_records(&mut reader, &mut writer, tz, cap);
    let stopped_early = !matches!(&result, Ok(s) if !s.capped);
    drop(reader);
    if stopped_early && let Ok(mut c) = child.lock() {
        let _ = c.kill();
    }
    let exit_code = wait_child(&child);
    let gone = client_gone.load(Ordering::SeqCst) || result.is_err();

    let stats = result.unwrap_or_default();
    if !gone && !stopped_early && exit_code != Some(0) {
        let code = exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string());
        let _ = writer
            .write_all(format!("[maint-helper] journalctl exited with status {code}\n").as_bytes());
    }
    // Close both directions now: the client sees end-of-output, and the
    // hang-up watcher's read returns, so nothing outlives this connection
    // (in production the process exits right after; in tests it does not).
    let _ = stream.shutdown(Shutdown::Both);
    ServeOutcome::Streamed {
        request,
        stats,
        client_gone: gone,
        exit_code,
    }
}

/// Send a fixed refusal and close so the client actually receives it.
///
/// Linux resets a unix stream connection that is closed while unread input
/// is still queued, and the client may already have sent its token line, so
/// a plain close can turn the reply into `ECONNRESET`. After the reply the
/// write half is shut down and up to 4 KiB of pending input is read and
/// thrown away, unparsed, for at most [`DRAIN_TIMEOUT`] in total.
fn finish_with_reply(stream: &UnixStream, reply: &str) {
    let mut writer = stream;
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let _ = writer.write_all(reply.as_bytes());
    let _ = stream.shutdown(Shutdown::Write);
    let mut drain = DeadlineReader {
        stream,
        deadline: Instant::now() + DRAIN_TIMEOUT,
    };
    let mut sink = [0u8; 512];
    let mut left = 4096usize;
    while left > 0 {
        match drain.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

/// Reap the child without holding the lock across a blocking wait, so the
/// hang-up watcher can still kill it.
fn wait_child(child: &Arc<Mutex<Child>>) -> Option<i32> {
    loop {
        match child.lock() {
            Ok(mut c) => match c.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) => {}
                Err(_) => return None,
            },
            Err(_) => return None,
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LogSource, MAINT_UID};
    use chrono::Utc;
    use std::io::{BufRead, Cursor};
    use std::time::Instant;

    fn my_uid() -> u32 {
        // SAFETY: getuid has no preconditions.
        #[allow(unsafe_code)]
        unsafe {
            libc::getuid()
        }
    }

    fn fixture(lines: &[String]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    fn no_restart(_: RestartTarget) -> RestartResult {
        panic!("no restart expected in this test")
    }

    fn rec(msg: &str) -> String {
        serde_json::json!({ "__REALTIME_TIMESTAMP": "1792134000000000", "MESSAGE": msg })
            .to_string()
    }

    fn cat(path: std::path::PathBuf) -> impl FnOnce(LogRequest) -> io::Result<Child> {
        move |_req| Command::new("cat").arg(path).stdout(Stdio::piped()).spawn()
    }

    #[test]
    fn token_line_reader() {
        assert!(
            matches!(read_token_line(&mut Cursor::new(b"model\n")), Ok(HelperRequest::Logs(r)) if r == LogRequest::ALL[2])
        );
        assert!(matches!(
            read_token_line(&mut Cursor::new(b"model")),
            Err(TokenReadError::NoNewline)
        ));
        assert!(matches!(
            read_token_line(&mut Cursor::new(b"model --all\n")),
            Err(TokenReadError::Token(TokenError::Unknown))
        ));
        let long = vec![b'a'; 4096];
        assert!(matches!(
            read_token_line(&mut Cursor::new(long)),
            Err(TokenReadError::Token(TokenError::TooLong))
        ));
        // Nothing past the newline is consumed.
        let mut c = Cursor::new(b"gateway\nEXTRA".to_vec());
        read_token_line(&mut c).unwrap();
        assert_eq!(c.position(), 8);
    }

    #[test]
    fn wrong_peer_is_refused_before_reading_or_spawning() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(b"gateway\n").unwrap();
        let mut spawned = false;
        let outcome = serve_connection(
            &server,
            MAINT_UID.wrapping_add(if my_uid() == MAINT_UID { 1 } else { 0 }),
            |_| {
                spawned = true;
                Err(io::Error::other("must not spawn"))
            },
            no_restart,
            &Utc,
        );
        assert!(matches!(
            outcome,
            ServeOutcome::Refused(PeerRejection::WrongUid(_))
        ));
        assert!(!spawned);
        drop(server);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, REPLY_NOT_PERMITTED);
    }

    #[test]
    fn unknown_token_is_refused_without_spawning() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(b"sysd\n").unwrap();
        let outcome = serve_connection(
            &server,
            my_uid(),
            |_| Err(io::Error::other("must not spawn")),
            no_restart,
            &Utc,
        );
        assert!(matches!(
            outcome,
            ServeOutcome::BadRequest(TokenReadError::Token(TokenError::Unknown))
        ));
        drop(server);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, REPLY_BAD_REQUEST);
    }

    #[test]
    fn streams_filtered_lines_and_withholds_the_password() {
        let f = fixture(&[
            rec("gateway listening"),
            rec("  🔑 First-run admin — log in with this, you'll be asked to change it:"),
            rec("     Email:    admin@duduclaw.local"),
            rec("     Password: Tr0ub4dor-and-3"),
            rec(&["token ", "AAExampleExampleExampleExampleXYZ12"].concat()),
        ]);
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(b"gateway\n").unwrap();
        let mut seen = None;
        let path = f.path().to_path_buf();
        let outcome = serve_connection(
            &server,
            my_uid(),
            |req| {
                seen = Some(req);
                cat(path)(req)
            },
            no_restart,
            &Utc,
        );
        assert_eq!(
            seen,
            Some(LogRequest {
                source: LogSource::Gateway,
                follow: false
            })
        );
        match outcome {
            ServeOutcome::Streamed {
                stats,
                client_gone,
                exit_code,
                ..
            } => {
                assert_eq!(stats.emitted, 4);
                assert_eq!(stats.withheld, 1);
                assert!(!client_gone);
                assert_eq!(exit_code, Some(0));
            }
            other => panic!("unexpected {other:?}"),
        }
        drop(server);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert!(!reply.contains("Tr0ub4dor"), "{reply}");
        assert!(
            !reply.contains("AAExampleExampleExampleExampleXYZ12"),
            "{reply}"
        );
        assert!(reply.contains("2026-10-16 07:00:00+00:00 gateway listening"));
        assert!(reply.contains("Email:"));
    }

    #[test]
    fn non_follow_output_is_capped() {
        let lines: Vec<String> = (0..MAX_LINES_NON_FOLLOW + 50)
            .map(|i| rec(&format!("l{i}")))
            .collect();
        let f = fixture(&lines);
        let (server, client) = UnixStream::pair().unwrap();
        (&client).write_all(b"model\n").unwrap();
        let reader = thread::spawn(move || {
            let mut n = 0;
            for l in io::BufReader::new(&client).lines() {
                l.unwrap();
                n += 1;
            }
            n
        });
        let outcome = serve_connection(
            &server,
            my_uid(),
            cat(f.path().to_path_buf()),
            no_restart,
            &Utc,
        );
        match outcome {
            ServeOutcome::Streamed { stats, .. } => {
                assert_eq!(stats.emitted, MAX_LINES_NON_FOLLOW);
                assert!(stats.capped);
            }
            other => panic!("unexpected {other:?}"),
        }
        drop(server);
        assert_eq!(reader.join().unwrap(), MAX_LINES_NON_FOLLOW);
    }

    #[test]
    fn follow_stops_when_the_client_hangs_up() {
        let f = fixture(&[rec("first")]);
        let path = f.path().to_path_buf();
        let (server, client) = UnixStream::pair().unwrap();
        (&client).write_all(b"gateway-follow\n").unwrap();
        let hang_up = thread::spawn(move || {
            let mut r = io::BufReader::new(&client);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            assert!(line.contains("first"));
            // Leaving: dropping the client closes the connection.
        });
        let started = Instant::now();
        // `tail -f` never ends on its own, like `journalctl -f`.
        let outcome = serve_connection(
            &server,
            my_uid(),
            move |_| {
                Command::new("tail")
                    .args(["-n", "+1", "-f"])
                    .arg(path)
                    .stdout(Stdio::piped())
                    .spawn()
            },
            no_restart,
            &Utc,
        );
        hang_up.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "follow did not stop"
        );
        assert!(
            matches!(
                outcome,
                ServeOutcome::Streamed {
                    client_gone: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    fn restart_request(token: &[u8], uid: u32) -> (ServeOutcome, String, Vec<RestartTarget>) {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(token).unwrap();
        let mut called = Vec::new();
        let outcome = serve_connection(
            &server,
            uid,
            |_| Err(io::Error::other("must not spawn")),
            |t| {
                called.push(t);
                RestartResult::Done
            },
            &Utc,
        );
        drop(server);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        (outcome, reply, called)
    }

    #[test]
    fn restart_tokens_restart_exactly_their_unit() {
        let (outcome, reply, called) = restart_request(b"restart-model\n", my_uid());
        assert!(matches!(
            outcome,
            ServeOutcome::Restart {
                target: RestartTarget::Model,
                result: RestartResult::Done
            }
        ));
        assert_eq!(called, [RestartTarget::Model]);
        assert_eq!(reply, "ok: restarted duduclaw-llama-server.service\n");
        let (_, reply, called) = restart_request(b"restart-gateway\n", my_uid());
        assert_eq!(called, [RestartTarget::Gateway]);
        assert_eq!(reply, "ok: restarted duduclaw-gateway.service\n");
    }

    #[test]
    fn restart_from_wrong_peer_or_unknown_token_runs_nothing() {
        let other = if my_uid() == MAINT_UID {
            MAINT_UID + 1
        } else {
            MAINT_UID
        };
        let (outcome, reply, called) = restart_request(b"restart-gateway\n", other);
        assert!(matches!(outcome, ServeOutcome::Refused(_)));
        assert!(called.is_empty());
        assert_eq!(reply, REPLY_NOT_PERMITTED);
        for bad in [
            &b"restart-sysd\n"[..],
            b"restart gateway\n",
            b"restart-gateway --now\n",
        ] {
            let (outcome, reply, called) = restart_request(bad, my_uid());
            assert!(matches!(outcome, ServeOutcome::BadRequest(_)));
            assert!(called.is_empty());
            assert_eq!(reply, REPLY_BAD_REQUEST);
        }
    }

    #[test]
    fn restart_refusal_is_sent_as_one_line() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(b"restart-model\n").unwrap();
        let outcome = serve_connection(
            &server,
            my_uid(),
            |_| Err(io::Error::other("must not spawn")),
            |_| RestartResult::RateLimited {
                retry_after_secs: 37,
            },
            &Utc,
        );
        assert!(matches!(outcome, ServeOutcome::Restart { .. }));
        drop(server);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "error: too soon, try again in 37 s\n");
    }

    #[test]
    fn trickled_token_hits_the_overall_deadline() {
        let (server, client) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        // One byte every 200 ms, never a newline: a per-read timeout would
        // never fire, the overall deadline must.
        let trickler = thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                if (&client).write_all(b"g").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
        });
        let started = Instant::now();
        let r = read_token_line(&mut DeadlineReader {
            stream: &server,
            deadline: Instant::now() + Duration::from_millis(800),
        });
        stop.store(true, Ordering::SeqCst);
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(server);
        trickler.join().unwrap();
    }

    #[test]
    fn journal_failure_is_reported_with_a_fixed_line() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(b"gateway\n").unwrap();
        let outcome = serve_connection(
            &server,
            my_uid(),
            |_| {
                Command::new("sh")
                    .args(["-c", "echo secret-stderr >&2; exit 3"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
            },
            no_restart,
            &Utc,
        );
        assert!(matches!(
            outcome,
            ServeOutcome::Streamed {
                exit_code: Some(3),
                ..
            }
        ));
        drop(server);
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "[maint-helper] journalctl exited with status 3\n");
    }
}
