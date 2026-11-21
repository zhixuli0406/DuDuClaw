//! RM-175: cheap, advisory "has my licence changed?" probe.
//!
//! `POST {control_url}/v1/license/rev` with `{"subscription_id",
//! "machine_fingerprint"}` — the identification the distributor-notice fetch
//! sends, through the same pinned transport and timeout. The answer
//! `200 {"rev": "<opaque, 1..=64 bytes>"}` is NOT signed and carries no
//! licence data: it is only a hint. A rev that differs from the one persisted
//! after the last successful sync triggers the ordinary signed refresh
//! ([`LicenseRuntime::sync_now`]); nothing else is derived from it, so a
//! forged or replayed rev can at worst cause one extra verified refresh.
//!
//! - `404` = the control plane does not offer the probe (or does not know
//!   this licence): remembered in memory, probed again once a day.
//! - `429`, `5xx`, other statuses, network errors and any malformed body =
//!   transient: the stored rev is kept, exponential backoff capped at 6 h.
//!
//! The loop runs only in the process holding the gateway instance lock and
//! does nothing without an installed licence. It logs only on state changes
//! and never logs the fingerprint, the rev or licence contents.

use std::path::Path;
use std::time::Duration;

use tracing::{debug, info};

use crate::license_runtime::{LicenseRuntime, SyncOutcome};

/// Persisted last-synced rev, next to `license.json`.
pub const REV_FILENAME: &str = "license_rev.json";
/// Longest rev accepted, in bytes.
pub const MAX_REV_BYTES: usize = 64;
/// Same as the distributor-notice fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest response body read; a valid answer is well under 100 bytes.
const MAX_RESPONSE_BYTES: usize = 4 * 1024;
/// Largest persisted file read.
const MAX_REV_FILE_BYTES: u64 = 4 * 1024;

const FIRST_PROBE_MIN: Duration = Duration::from_secs(2 * 60);
const FIRST_PROBE_MAX: Duration = Duration::from_secs(5 * 60);
pub const PROBE_INTERVAL: Duration = Duration::from_secs(30 * 60);
pub const PROBE_JITTER: Duration = Duration::from_secs(5 * 60);
/// Retry cadence once the control plane answered `404`.
pub const UNSUPPORTED_RETRY: Duration = Duration::from_secs(24 * 60 * 60);
/// First transient backoff; doubles per consecutive failure.
const BACKOFF_BASE: Duration = Duration::from_secs(60 * 60);
pub const MAX_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);

/// One probe answer, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevAnswer {
    /// A well-formed rev.
    Rev(String),
    /// `404`: probe not offered for this licence.
    Unsupported,
    /// Anything else; the reason is a fixed code for debug logs.
    Transient(&'static str),
}

fn valid_rev(rev: &str) -> bool {
    !rev.is_empty() && rev.len() <= MAX_REV_BYTES
}

/// Interpret a status + body strictly. Only `200` with a JSON object whose
/// `rev` is a non-empty string of at most [`MAX_REV_BYTES`] bytes is a rev.
pub fn parse_rev_response(status: u16, body: &[u8]) -> RevAnswer {
    match status {
        200 => {}
        404 => return RevAnswer::Unsupported,
        _ => return RevAnswer::Transient("http_status"),
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return RevAnswer::Transient("malformed");
    };
    match value.as_object().and_then(|object| object.get("rev")) {
        Some(serde_json::Value::String(rev)) if valid_rev(rev) => RevAnswer::Rev(rev.clone()),
        _ => RevAnswer::Transient("malformed"),
    }
}

/// One probe against the control plane. Request headers are reqwest's
/// defaults plus `content-type: application/json` (from `.json`).
pub async fn fetch_rev(control_url: &str, subscription_id: &str, machine_fingerprint: &str) -> RevAnswer {
    let endpoint = format!("{}/v1/license/rev", control_url.trim_end_matches('/'));
    let Ok(request) = crate::license_transport::request(reqwest::Method::POST, &endpoint, FETCH_TIMEOUT).await else {
        return RevAnswer::Transient("network");
    };
    let response = request
        .json(&serde_json::json!({
            "subscription_id": subscription_id,
            "machine_fingerprint": machine_fingerprint,
        }))
        .send()
        .await;
    let Ok(mut response) = response else {
        return RevAnswer::Transient("network");
    };
    let status = response.status().as_u16();
    if status != 200 {
        return parse_rev_response(status, &[]);
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return RevAnswer::Transient("malformed");
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return RevAnswer::Transient("malformed");
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => return RevAnswer::Transient("network"),
        }
    }
    parse_rev_response(status, &bytes)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredRev {
    subscription_id: String,
    rev: String,
}

/// The rev persisted for `subscription_id`, if any. A file written for
/// another subscription, oversized, malformed or holding an invalid rev is
/// ignored (= no rev, so the next probe syncs).
pub fn read_stored_rev(home: &Path, subscription_id: &str) -> Option<String> {
    let path = home.join(REV_FILENAME);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_REV_FILE_BYTES {
        return None;
    }
    let raw = std::fs::read(&path).ok()?;
    let stored: StoredRev = serde_json::from_slice(&raw).ok()?;
    (stored.subscription_id == subscription_id && valid_rev(&stored.rev)).then_some(stored.rev)
}

/// Persist `rev` for `subscription_id`: write-to-temp in the same directory,
/// fsync, owner-only (0600 on Unix), atomic rename — the same write style as
/// `duduclaw_license::storage`. Only the instance-lock holder writes it.
pub fn write_stored_rev(home: &Path, subscription_id: &str, rev: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let body = serde_json::to_vec(&StoredRev {
        subscription_id: subscription_id.to_string(),
        rev: rev.to_string(),
    })
    .map_err(std::io::Error::other)?;
    let mut temporary = tempfile::NamedTempFile::new_in(home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(&body)?;
    temporary.as_file().sync_all()?;
    temporary.persist(home.join(REV_FILENAME)).map_err(|e| e.error)?;
    Ok(())
}

/// `base` ± `jitter`, uniformly.
fn jittered(base: Duration, jitter: Duration) -> Duration {
    use rand::Rng as _;
    let spread = jitter.as_secs().saturating_mul(2);
    let offset = rand::thread_rng().gen_range(0..=spread);
    base.saturating_sub(jitter) + Duration::from_secs(offset)
}

fn first_probe_delay() -> Duration {
    use rand::Rng as _;
    let secs = rand::thread_rng().gen_range(FIRST_PROBE_MIN.as_secs()..=FIRST_PROBE_MAX.as_secs());
    Duration::from_secs(secs)
}

/// Backoff after `failures` consecutive transient failures (1 h, 2 h, 4 h,
/// then the 6 h cap), before jitter.
pub fn backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(8);
    BACKOFF_BASE.saturating_mul(1u32 << shift).min(MAX_BACKOFF)
}

fn normal_interval() -> Duration {
    jittered(PROBE_INTERVAL, PROBE_JITTER)
}

/// In-memory probe state. The only durable state is [`REV_FILENAME`].
#[derive(Debug, Default)]
pub struct RevProbeState {
    /// Subscription the control plane answered `404` for.
    unsupported_for: Option<String>,
    /// Consecutive transient failures (probe or sync).
    failures: u32,
}

impl RevProbeState {
    pub fn is_unsupported(&self) -> bool {
        self.unsupported_for.is_some()
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// One probe cycle; returns how long to sleep before the next one.
    /// `probe` performs the request, `sync` the signed refresh — injected so
    /// the state machine is testable without a control plane.
    pub async fn run_once<P, PF, S, SF>(
        &mut self,
        home: &Path,
        subscription_id: &str,
        probe: P,
        sync: S,
    ) -> Duration
    where
        P: FnOnce() -> PF,
        PF: std::future::Future<Output = RevAnswer>,
        S: FnOnce() -> SF,
        SF: std::future::Future<Output = SyncOutcome>,
    {
        if self.unsupported_for.as_deref().is_some_and(|sub| sub != subscription_id) {
            // A different licence may well be known to the control plane.
            self.unsupported_for = None;
        }
        let rev = match probe().await {
            RevAnswer::Rev(rev) => rev,
            RevAnswer::Unsupported => {
                if self.unsupported_for.is_none() {
                    info!("license change probe not offered by the control plane; checking again daily");
                }
                self.unsupported_for = Some(subscription_id.to_string());
                self.failures = 0;
                return UNSUPPORTED_RETRY;
            }
            RevAnswer::Transient(reason) => return self.transient(reason),
        };
        if self.unsupported_for.take().is_some() {
            info!("license change probe offered by the control plane again");
        }
        if read_stored_rev(home, subscription_id).as_deref() == Some(rev.as_str()) {
            self.recovered();
            return normal_interval();
        }
        // The rev observed BEFORE the sync is the one persisted: a change that
        // races with the sync differs from it and is caught next time.
        let outcome = sync().await;
        match outcome {
            SyncOutcome::Updated | SyncOutcome::Unchanged | SyncOutcome::Revoked | SyncOutcome::PendingRebind => {
                info!(outcome = outcome.as_str(), "license change probe triggered a license sync");
                if let Err(error) = write_stored_rev(home, subscription_id, &rev) {
                    debug!(%error, "could not persist the license change marker; the next probe syncs again");
                }
                self.recovered();
                normal_interval()
            }
            SyncOutcome::NoLicense => {
                self.recovered();
                normal_interval()
            }
            // Another sync ran moments ago; try again on the normal cadence.
            SyncOutcome::Busy { .. } => normal_interval(),
            SyncOutcome::Offline { reason } | SyncOutcome::Failed { reason } => self.transient(reason),
        }
    }

    fn transient(&mut self, reason: &'static str) -> Duration {
        self.failures = self.failures.saturating_add(1);
        if self.failures == 1 {
            debug!(reason, "license change probe failed; backing off");
        }
        jittered(backoff(self.failures), PROBE_JITTER).min(MAX_BACKOFF)
    }

    fn recovered(&mut self) {
        if self.failures > 0 {
            debug!("license change probe recovered");
        }
        self.failures = 0;
    }
}

/// Background loop, spawned with the other licence tasks.
pub async fn rev_probe_loop(runtime: LicenseRuntime) {
    tokio::time::sleep(first_probe_delay()).await;
    let mut state = RevProbeState::default();
    loop {
        let next = if !duduclaw_core::gateway_instance::held(runtime.home_dir()) {
            // Not the gateway holding this data directory: stay idle.
            normal_interval()
        } else if let Some((subscription_id, fingerprint)) = runtime.notice_identity().await {
            let home = runtime.home_dir().to_path_buf();
            let control_url = runtime.control_url().to_string();
            let (url, sub, fp, rt) = (control_url.as_str(), subscription_id.as_str(), fingerprint.as_str(), &runtime);
            state
                .run_once(
                    &home,
                    sub,
                    move || fetch_rev(url, sub, fp),
                    move || rt.sync_now(),
                )
                .await
        } else {
            // No licence: nothing to probe.
            normal_interval()
        };
        tokio::time::sleep(next).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn in_normal_window(d: Duration) -> bool {
        d >= PROBE_INTERVAL - PROBE_JITTER && d <= PROBE_INTERVAL + PROBE_JITTER
    }

    /// A probe returning `answer`, and a sync counting calls and returning `outcome`.
    async fn cycle(
        state: &mut RevProbeState,
        home: &Path,
        subscription_id: &str,
        answer: RevAnswer,
        outcome: SyncOutcome,
        syncs: &AtomicUsize,
    ) -> Duration {
        state
            .run_once(
                home,
                subscription_id,
                move || async move { answer },
                move || async move {
                    syncs.fetch_add(1, Ordering::SeqCst);
                    outcome
                },
            )
            .await
    }

    #[test]
    fn response_is_validated_strictly() {
        assert_eq!(parse_rev_response(200, br#"{"rev":"abc"}"#), RevAnswer::Rev("abc".into()));
        let max = "r".repeat(MAX_REV_BYTES);
        assert_eq!(parse_rev_response(200, format!(r#"{{"rev":"{max}"}}"#).as_bytes()), RevAnswer::Rev(max));
        // 65 bytes, or a 64-char string of multi-byte characters, is too long.
        let long = "r".repeat(MAX_REV_BYTES + 1);
        assert!(matches!(parse_rev_response(200, format!(r#"{{"rev":"{long}"}}"#).as_bytes()), RevAnswer::Transient(_)));
        let wide = "版".repeat(MAX_REV_BYTES);
        assert!(matches!(parse_rev_response(200, format!(r#"{{"rev":"{wide}"}}"#).as_bytes()), RevAnswer::Transient(_)));
        let bodies: &[&[u8]] = &[
            b"", b"not json", b"[]", b"\"abc\"", b"null", br#"{}"#, br#"{"rev":""}"#,
            br#"{"rev":42}"#, br#"{"rev":null}"#, br#"{"rev":["a"]}"#, br#"{"revision":"a"}"#,
        ];
        for &body in bodies {
            assert!(matches!(parse_rev_response(200, body), RevAnswer::Transient(_)), "{body:?}");
        }
        assert_eq!(parse_rev_response(404, b""), RevAnswer::Unsupported);
        for status in [201, 204, 301, 400, 401, 403, 429, 500, 502, 503] {
            assert!(matches!(parse_rev_response(status, br#"{"rev":"abc"}"#), RevAnswer::Transient(_)), "{status}");
        }
    }

    #[test]
    fn stored_rev_round_trips_owner_only_and_is_bound_to_the_subscription() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read_stored_rev(home.path(), "sub_a"), None);
        write_stored_rev(home.path(), "sub_a", "rev-1").unwrap();
        assert_eq!(read_stored_rev(home.path(), "sub_a").as_deref(), Some("rev-1"));
        assert_eq!(read_stored_rev(home.path(), "sub_b"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(home.path().join(REV_FILENAME)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join(REV_FILENAME)).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({"subscription_id": "sub_a", "rev": "rev-1"}));
        // Hand-edited junk or an oversized file is ignored, not trusted.
        std::fs::write(home.path().join(REV_FILENAME), b"{not json").unwrap();
        assert_eq!(read_stored_rev(home.path(), "sub_a"), None);
        std::fs::write(home.path().join(REV_FILENAME), br#"{"subscription_id":"sub_a","rev":""}"#).unwrap();
        assert_eq!(read_stored_rev(home.path(), "sub_a"), None);
        std::fs::write(home.path().join(REV_FILENAME), vec![b' '; 8 * 1024]).unwrap();
        assert_eq!(read_stored_rev(home.path(), "sub_a"), None);
    }

    #[tokio::test]
    async fn unchanged_rev_does_not_sync() {
        let home = tempfile::tempdir().unwrap();
        write_stored_rev(home.path(), "sub_a", "rev-1").unwrap();
        let syncs = AtomicUsize::new(0);
        let mut state = RevProbeState::default();
        let next = cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-1".into()),
            SyncOutcome::Updated, &syncs).await;
        assert_eq!(syncs.load(Ordering::SeqCst), 0);
        assert!(in_normal_window(next), "{next:?}");
        assert_eq!(read_stored_rev(home.path(), "sub_a").as_deref(), Some("rev-1"));
    }

    #[tokio::test]
    async fn changed_or_absent_rev_syncs_once_and_persists_the_observed_rev() {
        for outcome in [SyncOutcome::Updated, SyncOutcome::Unchanged, SyncOutcome::Revoked, SyncOutcome::PendingRebind] {
            let home = tempfile::tempdir().unwrap();
            let syncs = AtomicUsize::new(0);
            let mut state = RevProbeState::default();
            // Absent → sync.
            cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-1".into()), outcome.clone(), &syncs).await;
            assert_eq!(syncs.load(Ordering::SeqCst), 1, "{outcome:?}");
            assert_eq!(read_stored_rev(home.path(), "sub_a").as_deref(), Some("rev-1"));
            // Same rev → no further sync.
            cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-1".into()), outcome.clone(), &syncs).await;
            assert_eq!(syncs.load(Ordering::SeqCst), 1);
            // Different rev → exactly one more sync, new rev persisted.
            let next = cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-2".into()), outcome.clone(), &syncs).await;
            assert_eq!(syncs.load(Ordering::SeqCst), 2);
            assert_eq!(read_stored_rev(home.path(), "sub_a").as_deref(), Some("rev-2"));
            assert!(in_normal_window(next));
            assert_eq!(state.failures(), 0);
        }
    }

    #[tokio::test]
    async fn a_sync_that_did_not_complete_keeps_the_old_rev() {
        for outcome in [SyncOutcome::Offline { reason: "network" }, SyncOutcome::Failed { reason: "storage" },
            SyncOutcome::Busy { retry_after_secs: 10 }, SyncOutcome::NoLicense] {
            let home = tempfile::tempdir().unwrap();
            write_stored_rev(home.path(), "sub_a", "rev-1").unwrap();
            let syncs = AtomicUsize::new(0);
            let mut state = RevProbeState::default();
            let next = cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-2".into()), outcome.clone(), &syncs).await;
            assert_eq!(syncs.load(Ordering::SeqCst), 1);
            assert_eq!(read_stored_rev(home.path(), "sub_a").as_deref(), Some("rev-1"), "{outcome:?}");
            match outcome {
                SyncOutcome::Offline { .. } | SyncOutcome::Failed { .. } => {
                    assert_eq!(state.failures(), 1);
                    assert!(next >= backoff(1) - PROBE_JITTER, "{next:?}");
                }
                _ => {
                    assert_eq!(state.failures(), 0);
                    assert!(in_normal_window(next), "{outcome:?}: {next:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn not_found_means_unsupported_and_daily_retry() {
        let home = tempfile::tempdir().unwrap();
        let syncs = AtomicUsize::new(0);
        let mut state = RevProbeState::default();
        let next = cycle(&mut state, home.path(), "sub_a", RevAnswer::Unsupported, SyncOutcome::Updated, &syncs).await;
        assert_eq!(next, UNSUPPORTED_RETRY);
        assert!(state.is_unsupported());
        assert_eq!(syncs.load(Ordering::SeqCst), 0);
        assert!(!home.path().join(REV_FILENAME).exists());
        // Still 404 a day later: still daily.
        assert_eq!(cycle(&mut state, home.path(), "sub_a", RevAnswer::Unsupported, SyncOutcome::Updated, &syncs).await,
            UNSUPPORTED_RETRY);
        // The endpoint appears: back to the normal cadence and a first sync.
        let next = cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-1".into()), SyncOutcome::Unchanged, &syncs).await;
        assert!(!state.is_unsupported());
        assert!(in_normal_window(next));
        assert_eq!(syncs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transient_errors_back_off_exponentially_to_six_hours_and_keep_the_rev() {
        let home = tempfile::tempdir().unwrap();
        write_stored_rev(home.path(), "sub_a", "rev-1").unwrap();
        let syncs = AtomicUsize::new(0);
        let mut state = RevProbeState::default();
        let mut previous = Duration::ZERO;
        for round in 1..=6u32 {
            let next = cycle(&mut state, home.path(), "sub_a", RevAnswer::Transient("http_status"),
                SyncOutcome::Updated, &syncs).await;
            assert_eq!(state.failures(), round);
            assert!(next <= MAX_BACKOFF, "{next:?}");
            assert!(next + PROBE_JITTER >= backoff(round), "{round}: {next:?}");
            if round <= 3 {
                assert!(next > previous, "backoff grows: {round}");
            }
            previous = next;
        }
        assert_eq!(backoff(1), Duration::from_secs(3600));
        assert_eq!(backoff(2), Duration::from_secs(7200));
        assert_eq!(backoff(3), Duration::from_secs(4 * 3600));
        assert_eq!(backoff(4), MAX_BACKOFF);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
        assert_eq!(syncs.load(Ordering::SeqCst), 0);
        assert_eq!(read_stored_rev(home.path(), "sub_a").as_deref(), Some("rev-1"));
        // Recovery resets the counter.
        cycle(&mut state, home.path(), "sub_a", RevAnswer::Rev("rev-1".into()), SyncOutcome::Updated, &syncs).await;
        assert_eq!(state.failures(), 0);
    }

    #[tokio::test]
    async fn malformed_body_is_transient_and_never_syncs() {
        let home = tempfile::tempdir().unwrap();
        let syncs = AtomicUsize::new(0);
        let mut state = RevProbeState::default();
        let answer = parse_rev_response(200, br#"{"rev":{"nested":true}}"#);
        let next = cycle(&mut state, home.path(), "sub_a", answer, SyncOutcome::Updated, &syncs).await;
        assert_eq!(syncs.load(Ordering::SeqCst), 0);
        assert_eq!(state.failures(), 1);
        assert!(next + PROBE_JITTER >= backoff(1));
        assert!(!home.path().join(REV_FILENAME).exists());
    }

    #[tokio::test]
    async fn a_new_subscription_ignores_the_stored_rev_and_clears_unsupported() {
        let home = tempfile::tempdir().unwrap();
        write_stored_rev(home.path(), "sub_a", "rev-1").unwrap();
        let syncs = AtomicUsize::new(0);
        let mut state = RevProbeState::default();
        cycle(&mut state, home.path(), "sub_a", RevAnswer::Unsupported, SyncOutcome::Updated, &syncs).await;
        assert!(state.is_unsupported());
        // Same rev string, other subscription: not "unchanged".
        cycle(&mut state, home.path(), "sub_b", RevAnswer::Rev("rev-1".into()), SyncOutcome::Updated, &syncs).await;
        assert_eq!(syncs.load(Ordering::SeqCst), 1);
        assert!(!state.is_unsupported());
        assert_eq!(read_stored_rev(home.path(), "sub_b").as_deref(), Some("rev-1"));
        assert_eq!(read_stored_rev(home.path(), "sub_a"), None);
    }

    #[test]
    fn jitter_and_first_delay_stay_in_range() {
        for _ in 0..200 {
            assert!(in_normal_window(normal_interval()));
            let first = first_probe_delay();
            assert!(first >= FIRST_PROBE_MIN && first <= FIRST_PROBE_MAX);
        }
    }
}
