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
//! - **`http_poll`** — a GET through a redirect-refusing client pinned to a
//!   freshly re-resolved, freshly screened address set (D5-W2 DNS re-pin),
//!   with the SSRF gate re-checked against the URL actually dialed, the
//!   operator's custom headers attached, and the body accumulated under a hard
//!   byte cap.
//! - **`command`** — an argv vector executed directly (never through a
//!   shell), stdout taken as the payload, bounded by a timeout.
//! - **`file_tail`** — newly-appended complete lines since a byte cursor that
//!   survives rotation, truncation, invalid UTF-8 and over-cap lines.
//!
//! The push-based `websocket` kind is [`crate::tick_source_ws`]; it never
//! reaches [`poll_once`].

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, warn};

use duduclaw_core::truncate_bytes;

use crate::tick_config::{TickKind, TickSourceConfig};
use crate::tick_source::{MAX_TICK_PAYLOAD_BYTES, SourceState};

/// Upper bound on one `command` / `http_poll` fetch, independent of the poll
/// interval. `min(interval, this)` is what actually applies, so a 1 s source
/// can never queue overlapping fetches.
const MAX_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// One source's HTTP client, pinned to the address set it was built for
/// (D5-W2 DNS re-pin).
///
/// Why per-source and not one shared client: pinning is a *client-level*
/// setting in reqwest (`resolve_to_addrs`), so a process-wide singleton cannot
/// carry a per-request pin. Rebuilding a client per poll would be correct but
/// would also throw away the connection pool — a fresh TCP+TLS handshake every
/// second for a 1 s source. So the client is cached against the exact address
/// set it was pinned to: DNS is still re-resolved and re-screened on **every**
/// poll, and the client is rebuilt only when that answer actually changes.
pub(crate) struct PinnedHttpClient {
    addrs: Vec<SocketAddr>,
    client: reqwest::Client,
}

/// Resolve + screen this poll's DNS answer and hand back a client pinned to it.
///
/// The resolution happens on every call — that is the point of a re-pin — but
/// the client behind it survives as long as the answer is stable.
async fn pinned_client_for(
    state: &mut SourceState,
    host: &str,
    port: u16,
) -> Result<reqwest::Client, String> {
    // R2 — inside the DNS TTL this returns the cached, already-screened
    // address set and never touches the resolver; a 1 s source stops issuing
    // 86k lookups a day for an answer that changes hourly at most.
    let key = format!("{host}:{port}");
    let owned_host = host.to_string();
    let mut addrs = crate::tick_source::resolve_with_cache(
        &mut state.dns,
        &key,
        std::time::Instant::now(),
        || async move {
            crate::web_fetch::resolve_public_addrs(&owned_host, port)
                .await
                .map_err(|e| e.to_string())
        },
    )
    .await?;
    // Sorted so a round-robin resolver returning the same addresses in a
    // different order counts as "unchanged" and keeps the warm pool.
    addrs.sort();

    if let Some(existing) = &state.http_client {
        if existing.addrs == addrs {
            return Ok(existing.client.clone());
        }
    }

    // Redirects are refused outright: a validated, non-internal URL that 302s
    // elsewhere is exactly the SSRF bypass the initial check is meant to stop
    // — and a followed redirect would leave the pinned address behind.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(2)
        .resolve_to_addrs(host, &addrs)
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;
    state.http_client = Some(PinnedHttpClient {
        addrs,
        client: client.clone(),
    });
    Ok(client)
}

/// D5-W2 — the header map for one `http_poll` request.
///
/// Precedence, tightest last:
///
/// 1. Operator-declared `headers` (already validated by
///    `tick_config::validate_headers`, so both conversions below are
///    infallible in practice).
/// 2. `User-Agent`, only if the operator did not set their own.
/// 3. `Metadata-Flavor: none`, **always** — it is a security defence (it stops
///    a GCP metadata server that only answers header-less requests), not a
///    convenience, so a custom header may not displace it.
///
/// Nothing here logs a value. A header value is a credential.
pub(crate) fn build_header_map(custom: &BTreeMap<String, String>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in custom {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(n), Ok(v)) => {
                map.insert(n, v);
            }
            // Unreachable given config validation; refusing to send a header
            // we cannot represent (rather than sending something else) keeps
            // the failure loud at the endpoint instead of subtle here.
            _ => warn!(header = %name, "tick http_poll: header could not be encoded — omitted"),
        }
    }
    if !map.contains_key(reqwest::header::USER_AGENT) {
        map.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static("DuDuClaw/1.0"),
        );
    }
    map.insert("metadata-flavor", HeaderValue::from_static("none"));
    map
}

/// WP-H1 P1 — this request's headers with any `secret://` value resolved.
///
/// Called per poll, not per task start: the doctrine's rule is that a call site
/// may never hold a resolved credential, and re-resolving is what makes a
/// rotated secret take effect without restarting the gateway. Local backends
/// (`env` / `keychain` / `file`) cost effectively nothing; a network backend
/// owns its own TTL. Sources with no reference short-circuit inside
/// `resolve_header_secrets` before any config read.
async fn resolved_headers(
    cfg: &TickSourceConfig,
    home_dir: &std::path::Path,
) -> BTreeMap<String, String> {
    if !cfg.headers.values().any(|v| v.starts_with("secret://")) {
        return cfg.headers.clone();
    }
    let sm_cfg = crate::tick_headers::load_secret_manager_config(home_dir).await;
    crate::tick_headers::resolve_header_secrets(&cfg.headers, &sm_cfg, home_dir, &cfg.id).await
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
    home_dir: &std::path::Path,
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
            let parsed =
                crate::web_fetch::validate_url(url).map_err(|e| format!("url rejected: {e}"))?;
            // D5-W2 DNS re-pin — resolve the host at request time, refuse the
            // whole answer if any address is internal, and dial only the
            // addresses just validated. Without this, a hostname that passed
            // the pattern check at config load could re-resolve to
            // 169.254.169.254 before the TCP handshake.
            let host = parsed
                .host_str()
                .ok_or_else(|| "url rejected: missing host".to_string())?;
            let port = parsed.port_or_known_default().unwrap_or(443);
            let client = pinned_client_for(state, host, port).await?;
            let mut response = client
                .get(url)
                .timeout(timeout)
                .headers(build_header_map(&resolved_headers(cfg, home_dir).await))
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
    use crate::tick_source::{TickHub, run_source};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    // ── D5-W2: custom headers on the request ─────────────────

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn header_value(map: &HeaderMap, name: &str) -> Option<String> {
        map.get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }

    #[test]
    fn configured_headers_reach_the_request() {
        let map = build_header_map(&headers(&[
            ("X-API-Key", "secret-value"),
            ("Accept", "application/json"),
        ]));
        assert_eq!(
            header_value(&map, "x-api-key").as_deref(),
            Some("secret-value")
        );
        assert_eq!(
            header_value(&map, "accept").as_deref(),
            Some("application/json")
        );
        // Exactly one value per name — `insert`, not `append`, so a repeated
        // declaration can never become a doubled header.
        assert_eq!(map.get_all("x-api-key").iter().count(), 1);
    }

    #[test]
    fn built_in_headers_are_present_by_default() {
        let map = build_header_map(&BTreeMap::new());
        assert_eq!(
            header_value(&map, "user-agent").as_deref(),
            Some("DuDuClaw/1.0")
        );
        assert_eq!(
            header_value(&map, "metadata-flavor").as_deref(),
            Some("none")
        );
    }

    #[test]
    fn a_custom_user_agent_wins_but_the_metadata_defence_does_not_budge() {
        let map = build_header_map(&headers(&[
            ("User-Agent", "my-feed-client/2"),
            ("Metadata-Flavor", "Google"),
        ]));
        assert_eq!(
            header_value(&map, "user-agent").as_deref(),
            Some("my-feed-client/2"),
            "the operator may identify their own client"
        );
        assert_eq!(
            header_value(&map, "metadata-flavor").as_deref(),
            Some("none"),
            "a custom header must not disable the GCP metadata defence"
        );
        assert_eq!(map.get_all("metadata-flavor").iter().count(), 1);
    }

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
            headers: BTreeMap::new(),
            ping_interval_secs: 0,
            idle_timeout_secs: 0,
            json_fields: BTreeMap::new(),
            emit_unchanged: false,
            max_events_per_minute: 120,
            persist_every_n: 0,
            baseline_max_age_secs: crate::tick_config::DEFAULT_BASELINE_MAX_AGE_SECS,
            dns_ttl_secs: 0,
        };
        let hub2 = hub.clone();
        let handle = tokio::spawn(async move {
            run_source(cfg, Arc::new(std::env::temp_dir()), tx, hub2, None).await;
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
