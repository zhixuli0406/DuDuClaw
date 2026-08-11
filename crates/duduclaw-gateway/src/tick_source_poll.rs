//! Resident sensing — the three polling tick sources (D5).
//!
//! Split out of [`crate::tick_source`] (which owns the shared payload
//! pipeline: `emit_payload`, [`crate::tick_source::TickHub`], `DropReason`,
//! `SourceState` and the `run_source` dispatch) purely to keep both files
//! inside the project's file-size convention. Like
//! [`crate::tick_source_ws`], this module answers exactly one question —
//! *how do I obtain the next payload?* — and never touches what happens to a
//! payload afterwards.
//!
//! Three answers live here, one per pollable kind:
//!
//! - **`http_poll`** — a GET through a shared, redirect-refusing client, with
//!   the SSRF gate re-checked against the URL actually dialed and the body
//!   accumulated under a hard byte cap.
//! - **`command`** — an argv vector executed directly (never through a
//!   shell), stdout taken as the payload, bounded by a timeout.
//! - **`file_tail`** — newly-appended complete lines since a byte cursor that
//!   survives rotation, truncation, invalid UTF-8 and over-cap lines.
//!
//! The push-based `websocket` kind is [`crate::tick_source_ws`]; it never
//! reaches [`poll_once`].

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, warn};

use duduclaw_core::truncate_bytes;

use crate::tick_config::{TickKind, TickSourceConfig};
use crate::tick_source::{MAX_TICK_PAYLOAD_BYTES, SourceState};

/// Upper bound on one `command` / `http_poll` fetch, independent of the poll
/// interval. `min(interval, this)` is what actually applies, so a 1 s source
/// can never queue overlapping fetches.
const MAX_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared HTTP client for `http_poll` sources — one warm connection pool
/// instead of a fresh TCP+TLS handshake per poll (same rationale as
/// `autopilot_engine::notify_http_client`). Redirects are refused outright:
/// a validated, non-internal URL that 302s elsewhere is exactly the SSRF
/// bypass the initial check is meant to stop.
fn tick_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(2)
            .build()
            .expect("reqwest client build (tick sources)")
    })
}

/// Where the next `file_tail` read should start, given the current cursor and
/// the file's length.
///
/// Rotation/truncation (the file got shorter than where we were reading) resets
/// to the beginning — the alternative would be seeking past EOF and going
/// permanently silent after the first `logrotate`.
pub fn tail_start_offset(cursor: u64, len: u64) -> u64 {
    if len < cursor { 0 } else { cursor }
}

/// Fetch this source's pending payload(s). `http_poll` / `command` produce at
/// most one; `file_tail` produces one per newly-appended line.
pub(crate) async fn poll_once(
    cfg: &TickSourceConfig,
    state: &mut SourceState,
    interval: Duration,
) -> Result<Vec<String>, String> {
    let timeout = interval.min(MAX_FETCH_TIMEOUT);
    match cfg.kind {
        TickKind::HttpPoll => {
            let url = cfg.url.as_deref().ok_or("missing url")?;
            // Fail-closed re-check on the URL actually dialed — the config was
            // validated at load time, but re-validating here keeps the gate
            // adjacent to the request (same convention as the Odoo connector).
            crate::web_fetch::validate_url(url).map_err(|e| format!("url rejected: {e}"))?;
            let mut response = tick_http_client()
                .get(url)
                .timeout(timeout)
                .header("User-Agent", "DuDuClaw/1.0")
                // Refuses a GCP metadata server that only answers requests
                // without this header (same defence as `web_fetch`).
                .header("Metadata-Flavor", "none")
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("HTTP {status}"));
            }
            if let Some(len) = response.content_length() {
                if len > MAX_TICK_PAYLOAD_BYTES as u64 {
                    return Err(format!("response too large: {len} bytes"));
                }
            }
            // Accumulated chunk-by-chunk with a hard cap rather than
            // `response.text()`: a chunked reply advertises no Content-Length,
            // so an endpoint that starts streaming gigabytes would otherwise be
            // fully buffered before any size check could run — on a loop that
            // may poll every second.
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| format!("read body failed: {e}"))?
            {
                if body.len() + chunk.len() > MAX_TICK_PAYLOAD_BYTES {
                    return Err(format!(
                        "response exceeded the {MAX_TICK_PAYLOAD_BYTES}-byte cap"
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(vec![String::from_utf8_lossy(&body).into_owned()])
        }
        TickKind::Command => {
            let argv = cfg.command.as_ref().ok_or("missing command")?;
            let (program, args) = argv.split_first().ok_or("empty command")?;
            // argv is executed directly — never through a shell — so a value
            // in the config can't be reinterpreted as shell syntax.
            //
            // Unlike the HTTP branch above, stdout is buffered whole: the
            // command is an operator-authored argv behind the fail-closed
            // `allow_command_sources` switch, so its output volume is bounded
            // by `timeout` rather than by a byte cap. Anything over
            // `MAX_TICK_PAYLOAD_BYTES` is then refused in `emit_payload`.
            let mut command = tokio::process::Command::new(program);
            command.args(args).kill_on_drop(true);
            let output = tokio::time::timeout(timeout, command.output())
                .await
                .map_err(|_| format!("command timed out after {}s", timeout.as_secs()))?
                .map_err(|e| format!("spawn failed: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "command exited {:?}: {}",
                    output.status.code(),
                    truncate_bytes(stderr.trim(), 200)
                ));
            }
            Ok(vec![String::from_utf8_lossy(&output.stdout).into_owned()])
        }
        TickKind::FileTail => {
            let path = cfg.path.as_ref().ok_or("missing path")?;
            read_new_lines(path, &mut state.file_offset).await
        }
        // Unreachable in practice (`run_source` routes websocket sources to
        // their own loop before ever calling this), but an explicit refusal
        // beats a `_ => unreachable!()`: a future caller gets a counted
        // fetch_error, not a panic in a resident task.
        TickKind::Websocket => Err("websocket sources are stream-driven, not polled".into()),
    }
}

/// Read whatever was appended to `path` since `cursor`, returning one entry
/// per complete line. `cursor` advances past the bytes consumed; a shorter
/// file (rotation / truncation) resets it to zero first.
async fn read_new_lines(path: &Path, cursor: &mut u64) -> Result<Vec<String>, String> {
    let len = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("stat {} failed: {e}", path.display()))?
        .len();
    let start = tail_start_offset(*cursor, len);
    if start != *cursor {
        debug!(path = %path.display(), "tick file_tail: file shrank — cursor reset");
        *cursor = start;
    }
    if len <= *cursor {
        return Ok(Vec::new());
    }

    // Bound one read so a file that grew by gigabytes between polls can't be
    // slurped into memory in a single tick.
    let to_read = (len - *cursor).min(MAX_TICK_PAYLOAD_BYTES as u64);
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {} failed: {e}", path.display()))?;
    file.seek(std::io::SeekFrom::Start(*cursor))
        .await
        .map_err(|e| format!("seek failed: {e}"))?;
    let mut buffer = vec![0u8; to_read as usize];
    file.read_exact(&mut buffer)
        .await
        .map_err(|e| format!("read failed: {e}"))?;

    // Line splitting happens on RAW BYTES, not on a lossy-decoded string: an
    // invalid UTF-8 byte decodes to a 3-byte replacement char, so measuring
    // "bytes consumed" on the decoded text would drift the file cursor and
    // permanently desynchronize the tail. Each complete line is decoded
    // individually afterwards.
    //
    // Only complete lines are consumed; a trailing partial line stays for the
    // next poll (the writer may still be mid-append).
    let mut consumed = 0usize;
    let mut lines = Vec::new();
    for line in buffer.split_inclusive(|b| *b == b'\n') {
        if line.last() != Some(&b'\n') {
            break;
        }
        consumed += line.len();
        let text = String::from_utf8_lossy(line);
        let trimmed = text.trim_end_matches(['\n', '\r']);
        if !trimmed.is_empty() {
            lines.push(trimmed.to_string());
        }
    }

    if consumed == 0 && to_read == MAX_TICK_PAYLOAD_BYTES as u64 {
        // A single line longer than the payload cap would otherwise re-read
        // the same window forever and the source would go permanently silent.
        // Skip the over-cap chunk instead — loudly, never silently.
        warn!(
            path = %path.display(),
            cap = MAX_TICK_PAYLOAD_BYTES,
            "tick file_tail: no line terminator within the payload cap — skipping the chunk"
        );
        *cursor += to_read;
        return Ok(Vec::new());
    }

    *cursor += consumed as u64;
    Ok(lines)
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot_engine::AutopilotEvent;
    use crate::tick_source::{DropReason, TickHub, run_source};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    // ── file_tail rotation ───────────────────────────────────

    #[test]
    fn tail_offset_resets_on_rotation() {
        assert_eq!(
            tail_start_offset(100, 500),
            100,
            "normal growth keeps cursor"
        );
        assert_eq!(tail_start_offset(100, 100), 100, "no new bytes");
        assert_eq!(
            tail_start_offset(500, 100),
            0,
            "file shrank ⇒ re-read from 0"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_tail_reads_new_lines_and_survives_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feed.jsonl");
        std::fs::write(&path, "{\"p\":1}\n{\"p\":2}\n").unwrap();

        let mut cursor = 0u64;
        let lines = read_new_lines(&path, &mut cursor).await.unwrap();
        assert_eq!(lines, vec!["{\"p\":1}", "{\"p\":2}"]);

        // Nothing new → nothing emitted.
        assert!(read_new_lines(&path, &mut cursor).await.unwrap().is_empty());

        // Append → only the new line comes back.
        std::fs::write(&path, "{\"p\":1}\n{\"p\":2}\n{\"p\":3}\n").unwrap();
        assert_eq!(
            read_new_lines(&path, &mut cursor).await.unwrap(),
            vec!["{\"p\":3}"]
        );

        // Rotation: the file is replaced by a shorter one. The cursor must
        // reset to 0 instead of seeking past EOF forever.
        std::fs::write(&path, "{\"p\":9}\n").unwrap();
        assert_eq!(
            read_new_lines(&path, &mut cursor).await.unwrap(),
            vec!["{\"p\":9}"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_tail_cursor_survives_invalid_utf8() {
        // A lossy decode turns one bad byte into a 3-byte replacement char.
        // Measuring consumed bytes on the decoded text would drift the cursor
        // and silently corrupt every later read.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feed.log");
        let mut bytes = b"good\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\nafter\n");
        std::fs::write(&path, &bytes).unwrap();

        let mut cursor = 0u64;
        let lines = read_new_lines(&path, &mut cursor).await.unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "good");
        assert_eq!(lines[2], "after");
        assert_eq!(
            cursor,
            bytes.len() as u64,
            "cursor must count FILE bytes, not decoded-string bytes"
        );
        assert!(read_new_lines(&path, &mut cursor).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_tail_skips_a_line_longer_than_the_payload_cap() {
        // Without the skip, a terminator-less chunk at the cap would be
        // re-read every poll and the source would go permanently silent.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feed.log");
        let mut blob = vec![b'x'; MAX_TICK_PAYLOAD_BYTES + 16];
        blob.push(b'\n');
        blob.extend_from_slice(b"recovered\n");
        std::fs::write(&path, &blob).unwrap();

        let mut cursor = 0u64;
        assert!(read_new_lines(&path, &mut cursor).await.unwrap().is_empty());
        assert_eq!(
            cursor, MAX_TICK_PAYLOAD_BYTES as u64,
            "cursor moved past the chunk"
        );
        // The tail recovers on the following polls instead of stalling.
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..3 {
            seen.extend(read_new_lines(&path, &mut cursor).await.unwrap());
        }
        assert!(seen.iter().any(|l| l == "recovered"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_tail_holds_back_a_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feed.jsonl");
        std::fs::write(&path, "{\"p\":1}\n{\"p\":2").unwrap();

        let mut cursor = 0u64;
        let lines = read_new_lines(&path, &mut cursor).await.unwrap();
        assert_eq!(
            lines,
            vec!["{\"p\":1}"],
            "incomplete line waits for its newline"
        );

        std::fs::write(&path, "{\"p\":1}\n{\"p\":2}\n").unwrap();
        assert_eq!(
            read_new_lines(&path, &mut cursor).await.unwrap(),
            vec!["{\"p\":2}"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_source_counts_a_fetch_failure() {
        let hub = Arc::new(TickHub::new());
        let (tx, _rx) = broadcast::channel(16);
        let cfg = TickSourceConfig {
            id: "failing-cmd".into(),
            kind: TickKind::Command,
            enabled: true,
            interval_secs: 1,
            url: None,
            command: Some(vec!["sh".into(), "-c".into(), "exit 1".into()]),
            path: None,
            subscribe: Vec::new(),
            json_fields: BTreeMap::new(),
            emit_unchanged: false,
            max_events_per_minute: 120,
            persist_every_n: 0,
        };
        let hub2 = hub.clone();
        let handle = tokio::spawn(async move {
            run_source(cfg, tx, hub2, None).await;
        });
        // `run_source` sleeps `interval_secs` (1s) before its first poll —
        // real time, not paused (this crate's tests don't depend on the
        // tokio `test-util` feature). Give it enough margin for one full
        // iteration on a loaded CI box, then stop the loop.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        handle.abort();
        let snap = hub.counters_snapshot("failing-cmd").await;
        assert!(
            snap.dropped_fetch_error >= 1,
            "expected at least one fetch_error drop, got {snap:?}"
        );
    }
}
