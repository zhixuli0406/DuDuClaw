//! Gateway-side license runtime.
//!
//! Loads `~/.duduclaw/license.json` at startup, verifies the Ed25519
//! signature against the binary's embedded [`PublicKeyRegistry`], and
//! exposes the resulting tier + feature gate to the rest of the gateway.
//!
//! Spawns two long-running background tasks:
//!
//! - **phone-home** — refreshes the license on the cadence dictated by
//!   `features.toml` for the active tier (default 7 days for paid tiers,
//!   disabled for OpenSource). Failure modes are absorbed: a refresh that
//!   cannot reach the control-plane leaves the local license alone until
//!   the grace period expires, at which point the runtime downgrades to
//!   OpenSource and exposes an admin-facing validation warning.
//!
//! - **CRL poll** — every 24 hours fetches `GET /v1/license/crl`,
//!   verifies the issuer signature, and downgrades immediately if the
//!   active subscription appears in the revoked list.
//!
//! - **change probe** (RM-175, [`crate::license_rev`]) — every ~30 minutes
//!   asks the control plane for an unsigned revision hint and, when it
//!   differs from the last one seen, runs the ordinary signed refresh
//!   through [`LicenseRuntime::sync_now`].
//!
//! Both tasks are completely best-effort: any unhandled error is logged
//! and the loop continues. We never panic on a license problem — the
//! Apache 2.0 core must keep running.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Utc;
use duduclaw_license::{
    EMBEDDED_FEATURES_TOML, FeatureGate, License, LicenseError, LicenseTier, PublicKeyRegistry,
    crl::SignedCrl, fingerprint_candidates, fingerprint_is_hostname_only, generate_fingerprint,
    FingerprintCandidate, storage,
};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default fall-back when `DUDUCLAW_CONTROL_URL` is unset.
use duduclaw_license::control_url::DEFAULT_CONTROL_URL;

/// Process-wide cache for the strong fingerprint. The license crate retains
/// live hardware observations in memory; no editable disk cache is trusted.
static STRONG_FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Same, for the full candidate list (strong + v1.66.1 legacy compatibility).
static FINGERPRINT_CANDIDATES: std::sync::OnceLock<Vec<FingerprintCandidate>> = std::sync::OnceLock::new();

/// The fingerprint that actually identifies this machine to the control-plane:
/// the candidate the installed license validated against. Falls back to the
/// strong fingerprint when no license is installed or none matched, so a fresh
/// issuance always binds to the strong value.
///
/// Deliberately a `RwLock`, not a `OnceLock`: the effective value is only known
/// after `load_and_validate` runs, and a `OnceLock` read that happened to fire
/// first would pin the wrong value for the process lifetime.
static FINGERPRINT_CACHE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// One-shot guard so the "re-issue recommended" warning is logged once per
/// process rather than on every phone-home / dashboard poll.
static LEGACY_BINDING_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// This machine's strong fingerprint — what `duduclaw license fingerprint`
/// prints and the only value a *new* license should be issued against.
pub(crate) fn strong_fingerprint() -> &'static str {
    STRONG_FINGERPRINT.get_or_init(|| {
        if fingerprint_is_hostname_only() {
            // Reported, not silent: neither a platform UUID nor a usable MAC
            // could be read, so the fingerprint has no hardware component at
            // all. A weak binding must never be mistaken for a strong one.
            warn!(
                "no hardware identity readable on this host (no platform UUID, no \
                 usable MAC address) — the machine fingerprint binds to the hostname \
                 alone and provides no hardware binding"
            );
        }
        generate_fingerprint()
    })
}

/// Every fingerprint this machine may legitimately present, strongest first.
fn cached_candidates() -> &'static [FingerprintCandidate] {
    #[cfg(test)]
    if let Ok(candidates) = tests::VALIDATION_CANDIDATES.try_with(|candidates| *candidates) {
        return candidates;
    }
    FINGERPRINT_CANDIDATES.get_or_init(fingerprint_candidates)
}

/// Preferred locally observed identity class for the handshake snapshot.
pub(crate) fn preferred_fingerprint_strength() -> Option<duduclaw_license::FingerprintStrength> {
    cached_candidates().first().map(FingerprintCandidate::strength)
}

/// Return the effective machine fingerprint (see [`FINGERPRINT_CACHE`]).
pub(crate) fn cached_fingerprint() -> String {
    FINGERPRINT_CACHE
        .read()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| strong_fingerprint().to_string())
}

/// Record which candidate the installed license validated against.
fn set_effective_fingerprint(fingerprint: &str) {
    if let Ok(mut guard) = FINGERPRINT_CACHE.write() {
        *guard = Some(fingerprint.to_string());
    }
}

/// Reset the effective fingerprint to the strong value (no license, or the
/// installed one did not validate).
fn clear_effective_fingerprint() {
    if let Ok(mut guard) = FINGERPRINT_CACHE.write() {
        *guard = None;
    }
}

/// Tell the operator — once — that the active license is bound to a
/// historical hostname-based fingerprint. The binding stays accepted; the *next*
/// issuance should use the strong value.
fn warn_legacy_binding_once() {
    if !LEGACY_BINDING_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        warn!(
            "license is bound to a legacy fingerprint (issued before this machine's \
             identity source changed). The binding stays accepted; re-issue \
             when convenient with `duduclaw license rebind` (which calls the control \
             plane's /v1/license/rebind) or by re-signing against \
             `duduclaw license fingerprint`"
        );
    }
}

/// Env-var prefix for trusted issuer public keys.
///
/// Operators set `DUDUCLAW_LICENSE_PUBKEY_<KEY_ID>=<hex>` for each key
/// they trust. For example a Year-1 deployment that issues licenses with
/// `public_key_id = "v1"` exports `DUDUCLAW_LICENSE_PUBKEY_V1=<64 hex chars>`.
///
/// When the binary ships pre-baked, the same registry can be assembled
/// at compile time via [`PublicKeyRegistry::new`] + `with_key`; the env
/// path is for development and self-hosters who issue their own
/// internal-use licenses.
pub const PUBKEY_ENV_PREFIX: &str = "DUDUCLAW_LICENSE_PUBKEY_";

/// Active production issuer key id claimed by real licenses
/// (`license.public_key_id`).
///
/// **v1 was formally RETIRED on 2026-07-09** — its private counterpart was
/// unaccounted-for on the issuing side (no `license-signing-v1.key` under the
/// operator's control), so trusting it as a permanently-baked issuer was a
/// liability. It is intentionally NOT in the registry anymore; do not re-add v1.
/// The replacement v2 keypair is generated on a secure offline machine
/// (`license-keygen keygen --key-id v2`); only its public half is baked below.
const PROD_ISSUER_KEY_ID: &str = "v2";

/// The production issuer's Ed25519 **public** key (hex), baked into every stock
/// `duduclaw` binary so the commercial upgrade needs no separate build and no
/// environment variable: an enterprise drops its signed `license.json` into
/// `~/.duduclaw/`, restarts, and the paid tier unlocks — no `duduclaw-pro`.
///
/// Embedding a *public* key is safe and intended — the private signing key is
/// generated and held offline and never ships.
///
/// Empty would mean "no key baked (env-only, OpenSource)". Set below to the v2
/// issuer public key (id `v2`), generated offline via
/// `license-keygen keygen --key-id v2` — its private half is held offline and
/// never ships. This activates the single-binary upgrade path: a stock binary
/// verifies a v2-signed `license.json` with no env var and no `duduclaw-pro`.
const PROD_ISSUER_PUBKEY_HEX: &str =
    "942d6abae25dcd782c0586132143940003a10b1efd11e0a0b2f5a86d303476d0";

/// Build the trusted-issuer registry for a normal gateway boot.
///
/// Precedence: `DUDUCLAW_LICENSE_PUBKEY_<ID>` env vars are collected first and
/// win on id collision (self-hosters / self-issuers / emergency key rotation
/// can override), then the baked production key (`PROD_ISSUER_KEY_ID`) is
/// appended so a stock download trusts DuDuClaw-issued licenses with zero setup.
///
/// Fail-safe: an empty or malformed baked constant yields an env-only registry
/// (OpenSource unless an env key is supplied) rather than panicking. The worst
/// case is "no baked key" ⇒ OpenSource, never a crash of the Apache-2.0 core.
pub fn production_registry() -> PublicKeyRegistry {
    let registry = embedded_registry_from_env();
    if registry.get(PROD_ISSUER_KEY_ID).is_some() {
        // An operator-supplied key for this id takes precedence; don't shadow it.
        return registry;
    }
    if PROD_ISSUER_PUBKEY_HEX.is_empty() {
        // v1 retired; v2 not baked yet → env-only. Commercial licenses verify
        // only if an operator supplies a key via env; otherwise OpenSource.
        return registry;
    }
    match hex::decode(PROD_ISSUER_PUBKEY_HEX) {
        Ok(bytes) if bytes.len() == 32 => {
            info!(
                key_id = PROD_ISSUER_KEY_ID,
                "registered baked production issuer public key"
            );
            registry.with_key(PROD_ISSUER_KEY_ID, bytes)
        }
        _ => {
            warn!(
                "baked production issuer key is malformed — commercial licenses \
                 will not verify from this binary; running OpenSource unless a \
                 DUDUCLAW_LICENSE_PUBKEY_* env var supplies a valid key"
            );
            registry
        }
    }
}

/// Resolve the control-plane base URL with §10.5 precedence:
/// `DUDUCLAW_CONTROL_URL` env > `license.control_url` (self-carried) > the baked
/// default. A blank env value is treated as unset. This is only an untrusted
/// hint; license_transport validates the endpoint before every outbound call.
pub fn resolve_control_url(license: Option<&License>) -> String {
    if let Ok(env_url) = std::env::var("DUDUCLAW_CONTROL_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(lic) = license {
        if let Some(url) = lic.control_url.as_deref() {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    DEFAULT_CONTROL_URL.to_string()
}

/// How long we wait for a phone-home HTTP request before giving up and
/// trying again next cycle.
const PHONE_HOME_TIMEOUT: StdDuration = StdDuration::from_secs(20);

/// CRL fetch cadence — independent of tier, because CRL is the only way
/// to react to emergency revocations between phone-home cycles.
const CRL_POLL_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

/// Minimum sleep between phone-home attempts. Even Hobby (which defines
/// `license_phone_home_interval_days = 3` in features.toml) only sleeps
/// in days, but we floor at one hour so the task never spin-loops if
/// a tier accidentally defines a zero interval.
const MIN_PHONE_HOME_SLEEP: StdDuration = StdDuration::from_secs(60 * 60);

/// Read-only handle to the gateway's current licensing state.
///
/// Other services obtain a clone of this handle and call its accessor
/// methods. The interior is wrapped in [`tokio::sync::RwLock`] so the
/// phone-home and CRL background tasks can swap in a refreshed license
/// without blocking the rest of the gateway.
#[derive(Clone)]
pub struct LicenseRuntime {
    state: Arc<RwLock<LicenseRuntimeInner>>,
    gate: Arc<FeatureGate>,
    registry: Arc<PublicKeyRegistry>,
    home_dir: PathBuf,
    control_url: String,
    /// RM-175: single-flight / spacing for [`Self::sync_now`] and the cheap
    /// disk stamp for [`Self::reload_from_disk_if_changed`].
    sync: Arc<SyncGate>,
    /// Permits for blocking licence store operations: the process-wide
    /// [`STORE_OPS`] (tests substitute their own).
    store_ops: Arc<StoreOps>,
}

/// Minimum spacing between control-plane refreshes, scheduled or on demand
/// (the control plane admits only two refreshes at a time, globally).
pub const SYNC_MIN_SPACING: StdDuration = StdDuration::from_secs(30);

/// How long the gateway keeps trying `license.json.lock` before giving up
/// with a "busy" error that leaves the in-memory licence untouched.
const LICENSE_LOCK_DEADLINE: StdDuration = StdDuration::from_secs(5);

/// Boot waits longer so a slow CLI write does not select OpenSource at startup.
const BOOT_LICENSE_LOCK_DEADLINE: StdDuration = StdDuration::from_secs(30);

/// Overall bound of the boot-time licence load (lock wait plus reading two
/// files of at most 64 KiB). Past it the gateway starts as for an unreadable
/// licence and the 60-second watcher applies the licence later.
const BOOT_LOAD_DEADLINE: StdDuration = StdDuration::from_secs(45);

/// How long a caller waits for one store operation run on a blocking thread
/// (lock wait plus reading / writing two files of at most 64 KiB). Past it
/// the caller gives up; the operation keeps its permit until it really ends.
const STORE_OP_DEADLINE: StdDuration = StdDuration::from_secs(10);

/// Licence store operations that may run on blocking threads at once,
/// process-wide. Blocking file I/O cannot be cancelled: on a hung mount at
/// most this many threads are ever stuck, later callers get `Busy` at once.
const STORE_OP_PERMITS: usize = 2;

/// A store refused as unusable is not read again until its stamp changes or
/// this long has passed, whichever comes first.
const REJECTED_RETRY: StdDuration = StdDuration::from_secs(15 * 60);

/// Next scheduled phone-home attempt after a cycle that adopted no verified
/// answer (the sync that had just run was offline / failed): one hour, not
/// the day floor of [`LicenseRuntime::next_phone_home_sleep`].
const SCHEDULED_RETRY: StdDuration = StdDuration::from_secs(60 * 60);

/// Permits for blocking licence store operations ([`run_store_op`]).
struct StoreOps {
    permits: Arc<tokio::sync::Semaphore>,
    /// Blocking closures submitted through these permits.
    #[cfg(test)]
    spawned: std::sync::atomic::AtomicUsize,
}

impl StoreOps {
    /// A private set with the production limit (tests of the limit).
    #[cfg(test)]
    fn new() -> Self {
        Self::with_permits(STORE_OP_PERMITS)
    }

    fn with_permits(permits: usize) -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(permits)),
            #[cfg(test)]
            spawned: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// The one process-wide set of store permits, shared by every runtime and by
/// the boot-time load.
static STORE_OPS: std::sync::LazyLock<Arc<StoreOps>> = std::sync::LazyLock::new(|| {
    // Unit tests run in parallel in one process and some hold a permit for
    // a whole lock deadline; tests of the limit use their own `StoreOps::new()`.
    #[cfg(test)]
    let permits = 64;
    #[cfg(not(test))]
    let permits = STORE_OP_PERMITS;
    Arc::new(StoreOps::with_permits(permits))
});

/// How long a caller waits for a sync already in flight before skipping.
/// A flight holder is bounded by: DNS + request (each at most
/// `PHONE_HOME_TIMEOUT`, 20 s) and two locked store operations (snapshot
/// before, apply after; `STORE_OP_DEADLINE` each) — 60 s — plus slack.
const FLIGHT_WAIT: StdDuration = StdDuration::from_secs(75);

/// Who is asking for a sync. Both kinds share one flight and one spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncCaller {
    /// Dashboard `license.sync` or the change probe: inside the spacing
    /// window the answer is `Busy` (no vendor call).
    OnDemand,
    /// The phone-home loop: inside the spacing window the sync that just
    /// ran (a refresh) counts as this cycle's; the loop then re-computes its
    /// next sleep from the stored `last_phone_home`.
    Scheduled,
}

/// Serializes every control-plane refresh (on demand and scheduled). No
/// licence state lives here; the licence itself stays in
/// [`LicenseRuntimeInner`].
#[derive(Default)]
struct SyncGate {
    /// Held for the whole of one sync. The value is when the last vendor
    /// call (of either caller kind) started.
    flight: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    /// Bumped each time a sync finishes and records its outcome, so a
    /// caller that waited for an in-flight sync can take its result.
    generation: std::sync::atomic::AtomicU64,
    last: std::sync::Mutex<Option<SyncOutcome>>,
    /// `license.json` + revocation marker as last reconciled with memory.
    disk_stamp: std::sync::Mutex<Option<DiskStamp>>,
    /// A store refused as unusable (FIFO, oversized, malformed). Not read
    /// again by the watcher until the stamp changes or [`REJECTED_RETRY`]
    /// has passed; a forced sync ignores it.
    rejected: std::sync::Mutex<Option<RejectedStore>>,
}

/// Identity of a refused store, and when it was refused.
#[derive(Debug, Clone)]
struct RejectedStore {
    stamp: DiskStamp,
    /// SHA-256 of the licence bytes read, for a regular file within the
    /// size cap refused for its content; `None` for metadata-only refusals
    /// (FIFO, oversized, non-regular).
    digest: Option<[u8; 32]>,
    at: tokio::time::Instant,
}

impl RejectedStore {
    /// Still suppresses a re-read of a store with `stamp`.
    fn covers(&self, stamp: &DiskStamp) -> bool {
        self.stamp == *stamp && self.at.elapsed() < REJECTED_RETRY
    }
}

impl SyncGate {
    fn finish(&self, outcome: SyncOutcome) -> SyncOutcome {
        if let Ok(mut last) = self.last.lock() {
            *last = Some(outcome.clone());
        }
        self.generation.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        outcome
    }

    fn last_outcome(&self) -> Option<SyncOutcome> {
        self.last.lock().ok().and_then(|last| last.clone())
    }
}

/// Cheap identity of the files a CLI writer changes: type, length,
/// modification time and (Unix) status-change time and device + inode of
/// `license.json` and of the revocation marker. Licence links are resolved so
/// target replacement is noticed. The status-change time cannot be set back
/// by a writer, so an in-place rewrite that restores length and mtime still
/// changes the stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiskStamp {
    license: Option<FileStamp>,
    marker: Option<FileStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    regular: bool,
    len: u64,
    modified: Option<std::time::SystemTime>,
    /// Unix ctime (seconds, nanoseconds); `(0, 0)` elsewhere.
    changed: (i64, i64),
    device: u64,
    inode: u64,
}

impl DiskStamp {
    /// `None` when a file exists but its metadata cannot be read (the caller
    /// then falls back to the full digest comparison).
    fn read(home_dir: &Path) -> Option<Self> {
        Some(Self {
            license: file_stamp(&home_dir.join(storage::DEFAULT_LICENSE_FILENAME), true)?,
            marker: file_stamp(&home_dir.join(REVOKED_FILENAME), false)?,
        })
    }
}

/// Metadata only, never opens a FIFO or device. Resolve externally managed
/// licence links; internal markers retain their no-follow identity.
fn file_stamp(path: &Path, follow: bool) -> Option<Option<FileStamp>> {
    let metadata = if follow { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) };
    match metadata {
        Ok(meta) => {
            #[cfg(unix)]
            let (device, inode) = (std::os::unix::fs::MetadataExt::dev(&meta), std::os::unix::fs::MetadataExt::ino(&meta));
            #[cfg(unix)]
            let changed = (std::os::unix::fs::MetadataExt::ctime(&meta), std::os::unix::fs::MetadataExt::ctime_nsec(&meta));
            #[cfg(not(unix))]
            let (device, inode, changed) = (0, 0, (0, 0));
            Some(Some(FileStamp {
                regular: meta.file_type().is_file(),
                len: meta.len(),
                modified: meta.modified().ok(),
                changed,
                device,
                inode,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// Why the locked store could not be read this time.
#[derive(Debug)]
enum StoreReadError {
    /// `license.json.lock` held past [`LICENSE_LOCK_DEADLINE`], the whole
    /// operation past [`STORE_OP_DEADLINE`], or every store permit taken
    /// by operations still running.
    Busy,
    /// A store file is not regular, oversized, malformed, or an internal link:
    /// pointless to retry until it changes.
    Rejected(&'static str),
    /// Transient I/O failure.
    Io(String),
}

impl std::fmt::Display for StoreReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("license file is busy (locked by another process)"),
            Self::Rejected(reason) => write!(f, "license file refused: {reason}"),
            Self::Io(error) => write!(f, "license file could not be read: {error}"),
        }
    }
}

impl From<storage::StoreError> for StoreReadError {
    fn from(error: storage::StoreError) -> Self {
        match error {
            storage::StoreError::Busy => Self::Busy,
            storage::StoreError::Refused(reason) => Self::Rejected(reason),
            storage::StoreError::Io(error) => Self::Io(error.to_string()),
        }
    }
}

/// Run a store operation on a blocking thread: the lock is tried for at most
/// [`LICENSE_LOCK_DEADLINE`] and the caller stops waiting after
/// [`STORE_OP_DEADLINE`]. Never blocks an async worker.
async fn run_store_op<T, F>(ops: &StoreOps, op: F) -> Result<T, StoreReadError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreReadError> + Send + 'static,
{
    run_store_op_within(ops, STORE_OP_DEADLINE, op).await
}

/// [`run_store_op`] with its own wait bound. A permit of `ops` is taken
/// before anything is submitted and moves into the blocking closure, so it is
/// released only when the operation really ends (a read on a hung mount may
/// never end). Without a free permit the answer is `Busy` at once and no
/// blocking work is submitted.
async fn run_store_op_within<T, F>(ops: &StoreOps, deadline: StdDuration, op: F) -> Result<T, StoreReadError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreReadError> + Send + 'static,
{
    let Ok(permit) = ops.permits.clone().try_acquire_owned() else {
        debug!("license store operations still running; skipped this time");
        return Err(StoreReadError::Busy);
    };
    #[cfg(test)]
    ops.spawned.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        op()
    });
    match tokio::time::timeout(deadline, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(StoreReadError::Io("license store task failed".into())),
        Err(_) => Err(StoreReadError::Busy),
    }
}

/// Lock the store at `path` (bounded) and read it once.
async fn lock_and_read(ops: &StoreOps, path: PathBuf) -> Result<(storage::LockedLicenseStore, storage::StoreContents), StoreReadError> {
    run_store_op(ops, move || {
        let store = storage::LockedLicenseStore::acquire_within(&path, LICENSE_LOCK_DEADLINE)?;
        let contents = store.read_contents()?;
        Ok((store, contents))
    }).await
}

/// What a dashboard / probe triggered sync ended with. Reason codes are fixed
/// strings: never response bodies, licence contents or fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// A verified refresh changed what the licence grants (seats, expiry,
    /// tier, binding, …).
    Updated,
    /// A verified refresh returned the same entitlement.
    Unchanged,
    /// A signed revocation was applied; the gateway runs as OpenSource.
    Revoked,
    /// A rebind request awaits account-holder approval; licence unchanged.
    PendingRebind,
    /// No licence installed; nothing to sync.
    NoLicense,
    /// A licence file exists but is unusable; no vendor request was made.
    UnreadableLicenseFile { reason: &'static str },
    /// The control plane could not be reached or gave no acceptable answer;
    /// the installed licence is unchanged.
    Offline { reason: &'static str },
    /// The sync could not finish locally (disk write, or the licence changed
    /// while it was in flight); the installed licence is unchanged.
    Failed { reason: &'static str },
    /// A sync ran less than [`SYNC_MIN_SPACING`] ago; no vendor call made.
    Busy { retry_after_secs: u64 },
}

impl SyncOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::Revoked => "revoked",
            Self::PendingRebind => "pending_rebind",
            Self::NoLicense => "no_license",
            Self::UnreadableLicenseFile { .. } => "unreadable_license_file",
            Self::Offline { .. } => "offline",
            Self::Failed { .. } => "failed",
            Self::Busy { .. } => "busy",
        }
    }

    /// The control plane gave a verified answer (refreshed, pending rebind or
    /// revoked). Only such a result counts as a scheduled cycle's refresh.
    fn is_verified_answer(&self) -> bool {
        matches!(self, Self::Updated | Self::Unchanged | Self::PendingRebind | Self::Revoked)
    }

    /// Operator-facing zh-TW sentence (the dashboard localizes by `as_str`).
    pub fn message(&self) -> String {
        match self {
            Self::Updated => "已更新：已套用原廠最新的授權內容。".to_string(),
            Self::Unchanged => "已是最新：授權內容沒有變更。".to_string(),
            Self::Revoked => "授權已被撤銷，目前以開源模式執行。".to_string(),
            Self::PendingRebind => "等待原廠核准換機：帳號持有人在控制台核准後才會生效。".to_string(),
            Self::NoLicense => "尚未安裝商用授權，沒有需要同步的內容。".to_string(),
            Self::UnreadableLicenseFile { .. } => "主機上有授權檔，但內容無法讀取。請重新匯入授權，或請管理員檢查該檔案。".to_string(),
            Self::Offline { .. } => "目前連不上原廠，稍後會自動再試。".to_string(),
            Self::Failed { .. } => "同步沒有完成，稍後會自動再試。".to_string(),
            Self::Busy { retry_after_secs } => format!("剛剛已同步過，請 {retry_after_secs} 秒後再試。"),
        }
    }

    /// `{outcome, message}` plus `reason` / `retry_after_secs` when present.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = json!({ "outcome": self.as_str(), "message": self.message() });
        match self {
            Self::Offline { reason } | Self::Failed { reason } | Self::UnreadableLicenseFile { reason } => value["reason"] = json!(reason),
            Self::Busy { retry_after_secs } => value["retry_after_secs"] = json!(retry_after_secs),
            _ => {}
        }
        value
    }
}

/// The edition resolved at boot (`bootstrap_with_edition`). Parts of the
/// gateway are shaped by it until restart, so a licence change that resolves
/// a different edition recommends a restart.
static BOOT_EDITION: std::sync::OnceLock<duduclaw_core::EditionProfile> = std::sync::OnceLock::new();

/// The boot edition, when this process booted a gateway.
pub fn boot_edition_profile() -> Option<duduclaw_core::EditionProfile> {
    BOOT_EDITION.get().copied()
}

/// The only way the boot edition is written (from `bootstrap_with_edition`):
/// the first write wins, later ones are ignored. Returns whether it wrote.
fn record_boot_edition(
    cell: &std::sync::OnceLock<duduclaw_core::EditionProfile>,
    edition: duduclaw_core::EditionProfile,
) -> bool {
    cell.set(edition).is_ok()
}

struct LicenseRuntimeInner {
    revision: u64,
    /// `None` when no valid license is installed — gateway operates in
    /// OpenSource mode. The runtime exposes a stable `LicenseTier::OpenSource`
    /// to callers in this state.
    license: Option<License>,
    validation_warning: Option<&'static str>,

    /// HS13/D5: `generated_at` of the most recent *accepted* CRL. Used to
    /// reject replays of an older (e.g. pre-revocation) CRL. Loaded from disk
    /// at bootstrap so the freshness floor survives restarts.
    last_seen_crl_at: Option<chrono::DateTime<Utc>>,
}

/// File (under `home_dir`) that persists the last-seen CRL `generated_at`
/// timestamp so the monotonicity floor survives restarts.
const LAST_CRL_FILENAME: &str = "license_crl_last_seen.txt";

impl LicenseRuntime {
    /// Resolve the boot edition from the same verified license as live requests,
    /// before callers create users.db and consume the fresh-install signal.
    /// Background tasks remain the gateway caller's responsibility.
    pub async fn bootstrap_with_edition(
        home_dir: PathBuf,
        edition_override: Option<duduclaw_core::EditionProfile>,
    ) -> (Self, duduclaw_core::EditionProfile) {
        let _ = crate::license_seed::seed_license_if_absent(&home_dir);
        let runtime = Self::bootstrap(home_dir, production_registry()).await;
        let env = std::env::var("DUDUCLAW_EDITION").ok();
        let edition = runtime.boot_edition(env.as_deref(), edition_override).await;
        record_boot_edition(&BOOT_EDITION, edition);
        (runtime, edition)
    }

    async fn boot_edition(
        &self,
        env: Option<&str>,
        edition_override: Option<duduclaw_core::EditionProfile>,
    ) -> duduclaw_core::EditionProfile {
        let tier = self.snapshot().await.tier;
        duduclaw_core::EditionProfile::resolve(
            env, edition_override.map(|e| e.as_str()), Some(tier.as_toml_key()),
        )
    }

    /// Bootstrap the runtime from `home_dir`. Always succeeds — a missing
    /// or broken license file simply yields an OpenSource runtime.
    ///
    /// The returned runtime's background tasks have NOT been spawned yet —
    /// call [`Self::spawn_background_tasks`] after the rest of the gateway
    /// has finished initialising so the first phone-home doesn't race with
    /// startup logging.
    pub async fn bootstrap(home_dir: PathBuf, registry: PublicKeyRegistry) -> Self {
        let gate = Arc::new(
            FeatureGate::from_str(EMBEDDED_FEATURES_TOML)
                .expect("embedded features.toml is malformed"),
        );

        let (license, validation_warning) = load_and_validate(&home_dir, &registry, &gate).await;

        // §10.5: resolve the control-plane URL AFTER loading the license so a
        // self-carried `license.control_url` (baked in by the issuer) is honored
        // when the operator has not set `DUDUCLAW_CONTROL_URL`. Precedence:
        // env > license.control_url > DEFAULT.
        let control_url = resolve_control_url(license.as_ref());

        let tier = license
            .as_ref()
            .map(|l| l.tier)
            .unwrap_or(LicenseTier::OpenSource);
        info!(
            tier = %tier,
            customer = license
                .as_ref()
                .map(|l| l.customer_id.as_str())
                .unwrap_or("<none>"),
            // L22: the previous `then_some(0).unwrap_or(1)` could only ever log
            // 0 or 1 regardless of how many issuer keys were embedded. We report
            // the real count. NOTE for coordinator: `PublicKeyRegistry` currently
            // exposes only `is_empty()`/`get()`, so a `len()` accessor must be
            // added to `duduclaw-license/src/key.rs` for this to be exact; until
            // then we fall back to the env-derived count (the only construction
            // path that carries multiple keys in practice).
            registry_keys = count_embedded_issuer_keys(),
            "license runtime initialised"
        );

        // Proactive pre-expiry warning at boot (an already-expired license was
        // rejected above and logged separately; this covers the 1–30 day window).
        if let Some(lic) = license.as_ref() {
            log_expiry_warning(lic.tier, lic.days_until_expiry());
        }

        let last_seen_crl_at = read_last_seen_crl(&home_dir);
        crate::distributor_notice::runtime::load_cached(
            &home_dir,
            &registry,
            license.as_ref().map(|l| l.subscription_id.as_str()),
        );

        crate::distributor_brand::runtime::load_cached(
            &home_dir, &registry, license.as_ref().map(|l| l.subscription_id.as_str()),
        );

        Self {
            state: Arc::new(RwLock::new(LicenseRuntimeInner {
                revision: 0,
                license,
                validation_warning,
                last_seen_crl_at,
            })),
            gate,
            registry: Arc::new(registry),
            home_dir,
            control_url,
            sync: Arc::default(),
            store_ops: STORE_OPS.clone(),
        }
    }

    /// Spawn background phone-home and CRL polling tasks. Returns
    /// detached `JoinHandle`s so callers can hold them for graceful
    /// shutdown if they wish.
    pub fn spawn_background_tasks(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let phone_home = tokio::spawn(phone_home_loop(self.clone()));
        let crl_poll = tokio::spawn(crl_loop(self.clone()));
        let notice_poll = tokio::spawn(crate::distributor_notice::runtime::poll_loop(self.clone()));
        let brand_poll = tokio::spawn(crate::distributor_brand::runtime::poll_loop(self.clone()));
        // RM-175: idles unless this process holds the gateway instance lock
        // and a licence is installed (checked on every cycle).
        let rev_probe = tokio::spawn(crate::license_rev::rev_probe_loop(self.clone()));
        vec![phone_home, crl_poll, notice_poll, brand_poll, rev_probe]
    }

    /// `(subscription_id, machine_fingerprint)` of the installed license, for
    /// the distributor-notice fetch.
    pub(crate) async fn notice_identity(&self) -> Option<(String, String)> {
        let inner = self.state.read().await;
        inner
            .license
            .as_ref()
            .map(|l| (l.subscription_id.clone(), l.machine_fingerprint.clone()))
    }

    pub(crate) fn control_url(&self) -> &str {
        &self.control_url
    }

    pub(crate) fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub(crate) fn registry(&self) -> &PublicKeyRegistry {
        &self.registry
    }

    /// Currently-active tier. Always returns `OpenSource` when no license
    /// is installed or the installed license has been downgraded by the
    /// runtime (e.g. revoked, grace exceeded). Time-dependent validity is
    /// checked on every read, even if all background refreshes fail.
    pub async fn current_tier(&self) -> LicenseTier {
        let inner = self.state.read().await;
        self.live_tier(inner.license.as_ref())
    }

    fn live_tier(&self, license: Option<&License>) -> LicenseTier {
        let Some(license) = license else { return LicenseTier::OpenSource; };
        let grace = self.gate.grace_period_days(license.tier);
        // Match License::validate: zero grace disables the offline check.
        if license.is_expired() || (grace > 0 && license.grace_period_exceeded(grace)) {
            LicenseTier::OpenSource
        } else {
            license.tier
        }
    }

    /// Returns `true` if the active tier unlocks the named feature.
    /// Equivalent to `gate.check(current_tier(), feature)`.
    pub async fn check_feature(&self, feature: &str) -> bool {
        let tier = self.current_tier().await;
        self.gate.check(tier, feature)
    }

    /// Returns a clone of the embedded feature gate. Useful for callers
    /// that need to query multiple flags in a row without re-acquiring
    /// the read lock.
    pub fn feature_gate(&self) -> Arc<FeatureGate> {
        self.gate.clone()
    }


    /// Effective agent-count limit for the active license: the signed
    /// per-license `max_agents` override when present, else the tier default.
    /// `0` means unlimited. Falls back to the tier default when no license is
    /// installed (OpenSource). The single resolution point the agent-cap
    /// enforcement in `handlers.rs` must consult (P-License).
    pub async fn effective_max_agents(&self) -> usize {
        let inner = self.state.read().await;
        match inner.license.as_ref() {
            Some(lic) => self.gate.effective_max_agents(lic),
            None => self.gate.max_agents(LicenseTier::OpenSource),
        }
    }

    /// Effective Cloud memory storage quota in GB for the active tier
    /// (M1 moat-gate). `0` means unlimited — the value for OpenSource / free /
    /// self-host tiers, so memory-write enforcement is a no-op there. Only Cloud
    /// paid tiers with an explicit cap (Studio = 1, Business = 10) return
    /// nonzero. The single resolution point the memory-write path consults; the
    /// `duduclaw-memory` crate stays license-agnostic (quota is passed in as a
    /// parameter, not looked up).
    pub async fn effective_memory_quota_gb(&self) -> usize {
        let tier = self.current_tier().await;
        self.gate.effective_memory_quota_gb(tier)
    }

    /// Snapshot of the current license, if any. Returned by reference
    /// for read-only inspection (dashboard `license.status` RPC, etc.).
    pub async fn snapshot(&self) -> LicenseSnapshot {
        let inner = self.state.read().await;
        let tier = self.live_tier(inner.license.as_ref());
        let mut snapshot = LicenseSnapshot::from_state(inner.license.as_ref(), tier);
        snapshot.validation_warning = inner.validation_warning.or_else(|| {
            inner.license.as_ref().and_then(|license| {
                if license.is_expired() { Some("expired") }
                else if self.gate.grace_period_days(license.tier) > 0
                    && license.grace_period_exceeded(self.gate.grace_period_days(license.tier)) {
                    Some("grace_period_expired")
                } else { None }
            })
        });
        // An invalid installed file is different from a fresh personal install.
        snapshot.installed |= snapshot.validation_warning.is_some();
        if snapshot.validation_warning.is_some() { snapshot.mode = "opensource"; }
        snapshot
    }

    /// Verify a candidate license against this runtime's issuer registry +
    /// machine fingerprint + expiry, persist it as `license.json`, and
    /// hot-swap the in-memory license — the dashboard activation flow unlocks
    /// commercial features WITHOUT a gateway restart. Fail-closed: nothing is
    /// written unless signature, fingerprint, and expiry all pass; the final
    /// in-memory swap re-runs the full `load_and_validate` (M51 tier/deploy
    /// binding, phone-home grace, …) so the RPC path can never be more lenient
    /// than a restart. Error strings are operator-facing zh-TW.
    pub async fn install_and_reload(&self, license: License) -> Result<LicenseSnapshot, String> {
        if self.registry.is_empty() {
            return Err(
                "此版本未內建授權簽發公鑰，無法驗證授權（OSS 建置不支援商用授權）".to_string(),
            );
        }
        self.registry
            .verify(&license)
            .map_err(|e| format!("授權簽章驗證失敗：{e}"))?;
        let matched = license.validate_any(cached_candidates(),
            self.gate.phone_home_interval_days(license.tier), self.gate.grace_period_days(license.tier))
            .map_err(|e| format!("授權驗證失敗：{e}；換綁須由帳號持有人在控制台核准"))?;
        if license.is_expired() {
            return Err(format!("授權已於 {} 過期", license.expires_at));
        }
        // M51 pre-check: surface the tier ↔ deployment-mode mismatch BEFORE
        // writing anything — the post-save re-validation would reject it
        // anyway, but with an error that hides the actionable reason.
        let is_self_host = is_self_host_deployment();
        if license
            .validate_tier_deployment_binding(is_self_host)
            .is_err()
        {
            return Err(format!(
                "授權方案「{}」與本機部署型態不符：本機是{}部署。\
                 自架機器請簽發 self-host-pro / personal-pro-self-host / partner / oem；\
                 雲端託管才使用 solo / studio / business",
                license.tier,
                if is_self_host {
                    "自架（self-host）"
                } else {
                    "雲端（cloud）"
                },
            ));
        }
        // Serialize disk and memory changes with refresh CAS and revocation.
        let mut inner = self.state.write().await;
        // Lock, write and re-read off the async workers, the lock bounded.
        let path = self.home_dir.join(storage::DEFAULT_LICENSE_FILENAME);
        let to_save = license.clone();
        let (store, contents) = run_store_op(&self.store_ops, move || {
            let store = storage::LockedLicenseStore::acquire_within(&path, LICENSE_LOCK_DEADLINE)?;
            store.save(&to_save).map_err(|e| StoreReadError::Io(e.to_string()))?;
            let contents = store.read_contents();
            Ok((store, contents))
        }).await.map_err(|e| match e {
            StoreReadError::Busy => "授權檔正被其他程序使用，請稍後再試".to_string(),
            other => format!("寫入授權檔失敗：{other}"),
        })?;
        let validated = match contents {
            Ok(contents) => validate_store_contents(&self.registry, &self.gate, &contents),
            Err(_) => Err("unreadable"),
        };
        let (validated, validation_warning) = match validated {
            Ok(license) => (license, None),
            Err(reason) => (None, Some(reason)),
        };
        let ok = validated.is_some();
        // A notice cached for the previous license no longer applies; the
        // next fetch brings the new subscription's.
        crate::distributor_notice::runtime::reset(&self.home_dir);
        crate::distributor_brand::runtime::reset(&self.home_dir);
        inner.license = validated;
        inner.validation_warning = validation_warning;
        inner.revision += 1;
        if ok {
            if matched.legacy { warn_legacy_binding_once(); }
            set_effective_fingerprint(&matched.fingerprint);
        }
        drop(store);
        drop(inner);
        if !ok {
            return Err(
                "授權已寫入但完整驗證未通過（詳見 gateway 日誌），目前仍為 OpenSource 模式"
                    .to_string(),
            );
        }
        crate::distributor_notice::runtime::spawn_fetch_now(self.clone());
        crate::distributor_brand::runtime::spawn_fetch_now(self.clone());
        Ok(self.snapshot().await)
    }
}

/// Parse a dashboard-supplied license key: a base64-encoded license blob or
/// the raw license JSON. Deliberately does NOT accept filesystem paths (the
/// CLI does; a path coming from a browser form is a smell, not a feature).
pub fn parse_license_key(input: &str) -> Result<License, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("授權金鑰不可為空".to_string());
    }
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).map_err(|e| format!("授權 JSON 解析失敗：{e}"));
    }
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|_| "無法辨識的授權格式（需為 base64 金鑰或授權 JSON 全文）".to_string())?;
    serde_json::from_slice(&decoded).map_err(|e| format!("授權內容解析失敗：{e}"))
}

/// Read-only view of the runtime state for dashboards / metrics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LicenseSnapshot {
    pub tier: LicenseTier,
    pub mode: &'static str,
    pub installed: bool,
    /// Stable reason code, only exposed by license.status to Admin. No IDs or hashes.
    pub validation_warning: Option<&'static str>,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
    pub days_until_expiry: Option<i64>,
    pub last_phone_home: Option<chrono::DateTime<Utc>>,
    pub days_since_phone_home: Option<i64>,
    pub fingerprint_match: Option<bool>,
    /// Informational identity class, independent of paid-tier validation warnings.
    pub fingerprint_strength: Option<duduclaw_license::FingerprintStrength>,
    /// P-License: the active license's signed per-license agent-count quota, if
    /// any. `None` = no override → the tier default applies. Surfaced so the
    /// enforcement path can detect an explicit issuer-set limit (`is_some()`)
    /// even on a self-host tier — see `handlers::tier_limit_message`.
    pub max_agents: Option<u32>,
    /// NFR (Not-For-Resale) internal-test marker from the signed license. The
    /// dashboard MUST render this as a visible badge that operators
    /// cannot remove — it is the anti-resale watermark for free test licenses.
    pub nfr: bool,
}

impl LicenseSnapshot {
    pub(crate) fn for_admin(mut self, is_admin: bool) -> Self {
        if !is_admin { self.validation_warning = None; }
        self
    }

    fn from_state(license: Option<&License>, tier: LicenseTier) -> Self {
        let installed = license.is_some();
        // L7: reuse the cached fingerprint instead of re-enumerating host MACs.
        // This is the *effective* fingerprint (the candidate the active license
        // validated against), so a still-accepted legacy binding reports a
        // match rather than a scary false mismatch in the dashboard.
        let current_fp = cached_fingerprint();
        Self {
            tier,
            mode: if installed {
                "commercial"
            } else {
                "opensource"
            },
            installed,
            validation_warning: None,
            customer_id: license.map(|l| l.customer_id.clone()),
            subscription_id: license.map(|l| l.subscription_id.clone()),
            expires_at: license.map(|l| l.expires_at),
            days_until_expiry: license.map(|l| l.days_until_expiry()),
            last_phone_home: license.map(|l| l.last_phone_home),
            days_since_phone_home: license.map(|l| l.days_since_phone_home()),
            fingerprint_match: license.map(|l| l.is_valid_for_machine(&current_fp)),
            fingerprint_strength: cached_candidates().iter()
                .find(|c| c.fingerprint() == current_fp)
                .map(|c| c.strength()),
            max_agents: license.and_then(|l| l.max_agents),
            nfr: license.is_some_and(|l| l.nfr),
        }
    }
}

// ── Process-global accessor ───────────────────────────────────
//
// The gateway publishes its `LicenseRuntime` here at startup so other
// services (dashboard RPC handlers, channel-reply, evolution engine)
// can ask `current_tier()` without threading a handle through their
// initialisation paths. Mirrors the established pattern used by
// `cost_telemetry::init_telemetry` and `crate::log::LOG_TX`.

static GLOBAL_RUNTIME: std::sync::OnceLock<LicenseRuntime> = std::sync::OnceLock::new();

/// Publish a runtime as the process-global handle. Called once during
/// `start_gateway` bootstrap; subsequent calls are silently ignored.
pub fn set_global(runtime: LicenseRuntime) {
    let _ = GLOBAL_RUNTIME.set(runtime);
}

/// Returns the process-global runtime if one has been published. Returns
/// `None` for any subcommand that runs without `start_gateway` (e.g.
/// `duduclaw migrate`) so callers know to treat the absence as
/// "OpenSource mode" rather than a fatal error.
pub fn global() -> Option<&'static LicenseRuntime> {
    GLOBAL_RUNTIME.get()
}

/// Convenience: returns the current tier, or `OpenSource` when no
/// runtime has been published or no license is installed.
pub async fn current_tier_or_opensource() -> LicenseTier {
    match global() {
        Some(rt) => rt.current_tier().await,
        None => LicenseTier::OpenSource,
    }
}

/// Build a `PublicKeyRegistry` from `DUDUCLAW_LICENSE_PUBKEY_<ID>` env
/// vars. Each value is expected to be a 64-character hex string (the
/// Ed25519 public key as printed by `license-keygen keygen`).
///
/// Returns an empty registry when no matching env var is set — the
/// runtime treats this as "OpenSource mode" rather than failing.
pub fn embedded_registry_from_env() -> PublicKeyRegistry {
    let mut registry = PublicKeyRegistry::new();
    for (k, v) in std::env::vars() {
        let Some(suffix) = k.strip_prefix(PUBKEY_ENV_PREFIX) else {
            continue;
        };
        if suffix.is_empty() {
            warn!("ignoring {k} — missing key identifier suffix");
            continue;
        }
        let key_id = suffix.to_ascii_lowercase();
        let v_trim = v.trim();
        let bytes = match hex::decode(v_trim) {
            Ok(b) if b.len() == 32 => b,
            Ok(b) => {
                warn!(
                    key_id,
                    actual = b.len(),
                    "ignoring {k} — public key must decode to exactly 32 bytes"
                );
                continue;
            }
            Err(e) => {
                warn!(key_id, error = %e, "ignoring {k} — not valid hex");
                continue;
            }
        };
        registry = registry.with_key(&key_id, bytes);
        info!(key_id, "registered trusted issuer public key");
    }
    registry
}

/// L22: count the trusted issuer public keys derived from the environment.
///
/// Mirrors the validation in [`embedded_registry_from_env`] (suffix present +
/// 32-byte hex) so the logged count matches what was actually registered. This
/// exists because `PublicKeyRegistry` does not yet expose a `len()` accessor.
fn count_embedded_issuer_keys() -> usize {
    std::env::vars()
        .filter(|(k, v)| {
            k.strip_prefix(PUBKEY_ENV_PREFIX)
                .is_some_and(|suffix| !suffix.is_empty())
                && matches!(hex::decode(v.trim()), Ok(b) if b.len() == 32)
        })
        .count()
}

// ── Bootstrap helpers ─────────────────────────────────────────

/// Diagnose the on-disk license with the same checks as gateway bootstrap.
/// Returns only fixed reason codes; never license content, fingerprints or paths.
pub fn installed_license_warning(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate) -> Option<&'static str> {
    read_validated_license(home_dir, registry, gate).err()
}

fn validation_warning(error: &LicenseError) -> &'static str {
    match error {
        LicenseError::WeakFingerprint => "weak_binding",
        LicenseError::InvalidFingerprint if duduclaw_license::fingerprint_hardware_unavailable() => "hardware_identity_unavailable",
        LicenseError::InvalidFingerprint => "machine_mismatch",
        LicenseError::Expired => "expired",
        LicenseError::GracePeriodExceeded(_) => "grace_period_expired",
        LicenseError::InvalidSignature | LicenseError::UnknownPublicKeyId(_) => "invalid_signature",
        _ => "invalid_license",
    }
}

fn read_validated_license(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate) -> Result<Option<License>, &'static str> {
    let path = home_dir.join(storage::DEFAULT_LICENSE_FILENAME);
    // Preserve read-only diagnostics on an unused home. Returning no grant
    // without a lock is safe; any existing artifact is read under the lock.
    if !path.try_exists().map_err(|_| "unreadable")?
        && !home_dir.join(REVOKED_FILENAME).try_exists().map_err(|_| "unreadable")? {
        return Ok(None);
    }
    // Bounded: a lock held by a stuck process yields "unreadable" (OpenSource
    // for now) and the 60 s watcher applies the licence once it is free.
    let store = storage::LockedLicenseStore::acquire_within(&path, LICENSE_LOCK_DEADLINE).map_err(|_| "unreadable")?;
    read_validated_license_locked(registry, gate, &store)
}

fn read_validated_license_locked(registry: &PublicKeyRegistry, gate: &FeatureGate,
    store: &storage::LockedLicenseStore) -> Result<Option<License>, &'static str> {
    let contents = store.read_contents().map_err(|_| "unreadable")?;
    validate_store_contents(registry, gate, &contents)
}

/// The bootstrap validation, applied to a store already read under the lock.
fn validate_store_contents(registry: &PublicKeyRegistry, gate: &FeatureGate,
    contents: &storage::StoreContents) -> Result<Option<License>, &'static str> {
    if contents.revoked() {
        return Err("revoked");
    }
    let license = match contents.license() {
        Ok(Some(license)) => license,
        Ok(None) => return Ok(None),
        Err(_) => return Err("unreadable"),
    };
    registry.verify(&license).map_err(|e| validation_warning(&e))?;
    license.validate_any(cached_candidates(), gate.phone_home_interval_days(license.tier),
        gate.grace_period_days(license.tier)).map_err(|e| validation_warning(&e))?;
    license.validate_tier_deployment_binding(is_self_host_deployment())
        .map_err(|e| validation_warning(&e))?;
    Ok(Some(license))
}

/// Everything the relay client needs, produced only when the license is
/// eligible AND its signed `installation_pubkey` equals the local key.
/// No `Debug`: carries the installation signing key.
pub(crate) struct RelayLicense {
    /// Original JSON (including unknown fields and signature).
    pub artifact: serde_json::Value,
    pub fingerprint: String,
    pub subscription_id: String,
    pub key: crate::installation_identity::InstallationKey,
}

/// A fresh, validated relay artifact. Keep the original JSON (including unknown
/// fields and signature) rather than reconstructing it from the typed license.
/// The shared disk lock binds validation, fingerprint and artifact to one read.
pub(crate) fn mfa_relay_license(home: &Path) -> Result<RelayLicense, &'static str> {
    let unavailable = "no_smtp";
    let path = home.join(storage::DEFAULT_LICENSE_FILENAME);
    if !path.try_exists().map_err(|_| unavailable)? { return Err(unavailable); }
    let registry;
    let gate;
    let (registry_ref, gate_ref) = if let Some(runtime) = global() {
        (runtime.registry.as_ref(), runtime.gate.as_ref())
    } else {
        registry = production_registry();
        gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).map_err(|_| unavailable)?;
        (&registry, &gate)
    };
    mfa_relay_license_with(home, registry_ref, gate_ref)
}

fn mfa_relay_tier_eligibility(tier: LicenseTier, nfr: bool) -> Result<(), &'static str> {
    // Legacy Partner artifacts may omit the NFR bit; the tier is still free.
    if nfr || tier.is_partner() { return Err("relay_nfr"); }
    if !tier.is_paid() { return Err("no_smtp"); }
    Ok(())
}

fn mfa_relay_license_with(home: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate)
    -> Result<RelayLicense, &'static str> {
    let unavailable = "no_smtp";
    let path = home.join(storage::DEFAULT_LICENSE_FILENAME);
    if !path.try_exists().map_err(|_| unavailable)? { return Err(unavailable); }
    let store = storage::LockedLicenseStore::acquire_within(&path, LICENSE_LOCK_DEADLINE).map_err(|_| unavailable)?;
    let license = read_validated_license_locked(registry, gate, &store)
        .map_err(|_| unavailable)?.ok_or(unavailable)?;
    mfa_relay_tier_eligibility(license.tier, license.nfr)?;
    if license.machine_fingerprint.is_empty() { return Err("relay_unbound"); }
    let matched = license.validate_any(cached_candidates(), gate.phone_home_interval_days(license.tier),
        gate.grace_period_days(license.tier)).map_err(|_| unavailable)?;
    let raw_bytes = storage::read_store_file(&path).map_err(|_| unavailable)?.ok_or(unavailable)?;
    let raw: serde_json::Value = serde_json::from_slice(&raw_bytes).map_err(|_| unavailable)?;
    // Cooperative writers hold the same lock; also fail closed on a mismatched read.
    let parsed: License = serde_json::from_value(raw.clone()).map_err(|_| unavailable)?;
    if serde_json::to_value(&parsed).map_err(|_| unavailable)? != serde_json::to_value(&license).map_err(|_| unavailable)? {
        return Err(unavailable);
    }
    // Owner-approved installation key (design §2.1). The paid tier itself is
    // unaffected; only the relay is withheld until the key is bound and present.
    let key = crate::installation_identity::relay_key_status(license.installation_pubkey.as_deref(),
        crate::installation_identity::load(home))?;
    // Enrollment identity only. The relay transport owns its compile-time destination;
    // neither the license's unsigned control_url nor environment overrides select it.
    Ok(RelayLicense { artifact: raw, fingerprint: matched.fingerprint,
        subscription_id: license.subscription_id, key })
}

/// Relay key reason for a freshly refreshed license: `None` when the signed key
/// equals the local one. Never a reason to reject the license.
fn refreshed_relay_key_reason(license: &License, local_pubkey: Option<&str>) -> Option<&'static str> {
    match (license.installation_pubkey.as_deref(), local_pubkey) {
        (None, _) => Some("relay_key_unbound"),
        (Some(bound), Some(local)) if bound == local => None,
        _ => Some("relay_key_mismatch"),
    }
}

/// The boot-time disk read: `None` when neither the licence nor the
/// revocation marker exists.
fn boot_read_store(path: &Path) -> Result<Option<storage::StoreContents>, StoreReadError> {
    if !path.try_exists().map_err(|e| StoreReadError::Io(e.to_string()))?
        && !path.with_file_name(REVOKED_FILENAME).try_exists().map_err(|e| StoreReadError::Io(e.to_string()))? {
        return Ok(None);
    }
    let store = storage::LockedLicenseStore::acquire_within(path, BOOT_LICENSE_LOCK_DEADLINE)?;
    Ok(Some(store.read_contents()?))
}

async fn load_and_validate(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate) -> (Option<License>, Option<&'static str>) {
    let path = home_dir.join(storage::DEFAULT_LICENSE_FILENAME);
    load_and_validate_from(registry, gate, &STORE_OPS, BOOT_LOAD_DEADLINE, move || boot_read_store(&path)).await
}

/// [`load_and_validate`] with the disk read, its permits and its overall
/// bound injected (tests).
async fn load_and_validate_from<F>(registry: &PublicKeyRegistry, gate: &FeatureGate, ops: &StoreOps,
    deadline: StdDuration, read: F) -> (Option<License>, Option<&'static str>)
where
    F: FnOnce() -> Result<Option<storage::StoreContents>, StoreReadError> + Send + 'static,
{
    // Only disk work waits on a blocking thread; validation keeps the caller's
    // fingerprint context. The whole boot read (lock wait included) has its
    // own bound, not the running 5 s / 10 s bounds; past it the read keeps
    // its permit until it really ends and the watcher applies the licence.
    let contents = run_store_op_within(ops, deadline, read).await;
    let validated = match contents {
        Ok(Some(contents)) => validate_store_contents(registry, gate, &contents),
        Ok(None) => Ok(None),
        Err(StoreReadError::Busy) => {
            clear_effective_fingerprint();
            warn!("license file not readable within the boot deadline; running in OpenSource mode until the license watcher can read it");
            return (None, Some("unreadable"));
        }
        Err(_) => Err("unreadable"),
    };
    // One warning per boot: the deadline / lock timeout above, or the one below.
    match validated {
        Ok(Some(license)) => {
            let matched = license.validate_any(cached_candidates(), gate.phone_home_interval_days(license.tier),
                gate.grace_period_days(license.tier));
            // Recheck before publishing the effective identity; time may have advanced.
            match matched {
                Ok(matched) => {
                    if matched.legacy { warn_legacy_binding_once(); }
                    set_effective_fingerprint(&matched.fingerprint);
                    (Some(license), None)
                }
                Err(e) => {
                    clear_effective_fingerprint();
                    (None, Some(validation_warning(&e)))
                }
            }
        }
        Ok(None) => {
            clear_effective_fingerprint();
            debug!("no license file found; running in OpenSource mode");
            (None, None)
        }
        Err(reason) => {
            clear_effective_fingerprint();
            warn!(reason, "installed license validation failed; running in OpenSource mode");
            (None, Some(reason))
        }
    }
}

/// Resolve the deployment mode (M51 signal).
///
/// Read from `DUDUCLAW_DEPLOYMENT`:
///   - `cloud` → managed / Cloud control-plane deployment (`false`)
///   - `self_host` / `self-host` / `selfhost` / `on_prem` / `onprem` → `true`
///   - unset / anything else → **self-host** (`true`), the safe default for a
///     downloaded binary. The managed tenant image is responsible for setting
///     `DUDUCLAW_DEPLOYMENT=cloud` (injected by the tenant provisioner), so the
///     default never mis-classifies a legitimate self-host install.
pub fn is_self_host_deployment() -> bool {
    match std::env::var("DUDUCLAW_DEPLOYMENT") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "cloud" | "managed"),
        Err(_) => true,
    }
}

/// Proactive license-expiry urgency, mirroring the dashboard's `classifyExpiry`
/// buckets so the log warnings and the UI agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryUrgency {
    /// More than 30 days left — no warning.
    Ok,
    /// 8–30 days left — plan renewal.
    Warning,
    /// 0–7 days left — renew now to avoid a downgrade.
    Critical,
    /// Already past expiry.
    Expired,
}

/// Pure classification of days-until-expiry into a warning bucket. Thresholds
/// match the dashboard (`LicensePage.classifyExpiry`) and the OAuth-token
/// expiry-warning convention (30/7-day pre-warnings).
pub fn classify_expiry_urgency(days_until_expiry: i64) -> ExpiryUrgency {
    if days_until_expiry < 0 {
        ExpiryUrgency::Expired
    } else if days_until_expiry <= 7 {
        ExpiryUrgency::Critical
    } else if days_until_expiry <= 30 {
        ExpiryUrgency::Warning
    } else {
        ExpiryUrgency::Ok
    }
}

/// Emit a proactive expiry warning to the tracing log for a still-valid license
/// nearing its term end. An already-*expired* license never reaches this path
/// with `Some` (it is rejected during load and separately logged), so this only
/// fires for the 1–30 day pre-expiry window — the whole point of a proactive
/// warning is to fire *before* the downgrade.
fn log_expiry_warning(tier: LicenseTier, days_until_expiry: i64) {
    match classify_expiry_urgency(days_until_expiry) {
        ExpiryUrgency::Critical => warn!(
            tier = %tier,
            days_left = days_until_expiry,
            "license expires within 7 days — renew now to avoid an automatic downgrade to OpenSource"
        ),
        ExpiryUrgency::Warning => warn!(
            tier = %tier,
            days_left = days_until_expiry,
            "license expires within 30 days — plan renewal"
        ),
        ExpiryUrgency::Ok | ExpiryUrgency::Expired => {}
    }
}

/// Pure cap check used by the Cloud resource limits (agents / channels).
/// `max == 0` means unlimited (the features.toml convention), so it never
/// caps. Otherwise the cap is reached once `current >= max`.
pub fn cap_exceeded(max: usize, current: usize) -> bool {
    max != 0 && current >= max
}

/// P-License §7(b) enforcement gate for the agent cap. Self-host deployments are
/// normally exempt from resource caps (the Apache 2.0 promise), but a signed
/// per-license `max_agents` override is the issuer's EXPLICIT intent to cap this
/// seat count, so it overrides the exemption. Cloud deployments always enforce.
/// Pure so the decision is unit-testable without a live runtime / env.
pub fn agent_cap_enforced(is_self_host: bool, has_max_agents_override: bool) -> bool {
    !is_self_host || has_max_agents_override
}

/// How many agents may be registered as OS-native ("OS 原生員工") for the given
/// edition. This is a **quota** lock, not a capability lock — the `os_native`
/// capability itself is never removed or crippled on any edition; the free core
/// keeps every feature, we only cap *how many seats* may use it at once
/// (`edition-split-moat`: 鎖 quota 不鎖能力).
///
/// - **Personal** → `Some(1)`: exactly one AI employee may be the OS-native one.
/// - **Enterprise** → `None`: uncapped. The signed license schema carries no
///   os-native-specific field, so there is nothing to read here — paid tiers are
///   simply unlimited. (Do NOT invent a license field that doesn't exist; the
///   whole-fleet `max_agents` quota is a *separate* concern enforced elsewhere.)
///
/// `None` means unlimited; a `Some(n)` return is the number of agents that may
/// have `[capabilities] os_native = true` simultaneously. Pure so both the
/// write-time gate (`agents.update` / `os.settings.update`) and the startup
/// gate (`resolve_os_native_allowed`) consult one identical decision.
pub fn os_native_agent_quota(edition: duduclaw_core::EditionProfile) -> Option<u32> {
    if edition.is_personal() { Some(1) } else { None }
}

/// Agent-count cap check for processes OUTSIDE the gateway (the MCP server is
/// a separate `duduclaw mcp-server` process, so [`global()`] is never
/// initialised there and the dashboard's `tier_limit_message` gate cannot
/// run). Without this, an agent could `create_agent` its way past both the
/// Personal-edition cap and a signed P-License `max_agents` quota.
///
/// Bootstraps a throwaway runtime from disk and mirrors the gateway's agent
/// branch: Personal edition → `personal_max_agents()` (unlimited by default
/// per the 2026-07-16 B+C decision; `DUDUCLAW_PERSONAL_MAX_AGENTS` opts a
/// hosted deployment into a hard cap); otherwise the license tier /
/// signed-override path with the self-host exemption. Edition is resolved
/// the same way the gateway does at boot (env > license tier). The
/// `ServerConfig`-level edition override is gateway-internal and always
/// `None` in the open CLI, so it is not consulted here.
///
/// Returns the zh-TW refusal message when `current` agents already meet the
/// effective cap, else `None` (allowed).
pub async fn agent_cap_message_from_disk(
    home_dir: &std::path::Path,
    current: usize,
) -> Option<String> {
    let cap = agent_cap_from_disk(home_dir).await;
    if !cap_exceeded(cap.max, current) {
        return None;
    }
    Some(agent_cap_text(cap.max, cap.tier.as_deref()))
}

/// The zh-TW agent-cap refusal: `tier = None` is the Personal-edition cap.
/// One wording for the gateway and the processes outside it (RM-149).
pub fn agent_cap_text(max: usize, tier: Option<&str>) -> String {
    match tier {
        None => format!(
            "個人版最多可建立 {max} 個 AI 員工。\
             如需調整授權範圍，請聯絡總經銷：\
             https://futurecorp.tw"
        ),
        Some(tier) => format!(
            "您的方案（{tier}）最多可建立 {max} 個 Agent。\
             如需調整授權範圍，請聯絡總經銷：https://futurecorp.tw"
        ),
    }
}

/// The note appended to an agent-cap refusal: deleted employees no longer
/// count, archived ones still do (RM-149).
pub const AGENT_SEAT_NOTE: &str =
    "已刪除的 AI 員工不佔名額；已封存的 AI 員工仍佔名額，刪除後即釋出。";

/// Keep URLs separate from the seat explanation at every refusal call site.
pub fn with_agent_seat_note(message: &str, separator: &str) -> String {
    format!("{message}{separator}{AGENT_SEAT_NOTE}")
}

/// The agent-count cap that applies to this home, resolved from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCap {
    /// `0` = unlimited (or not enforced on this deployment).
    pub max: usize,
    /// The license tier when the cap comes from the license; `None` for
    /// the Personal-edition cap.
    pub tier: Option<String>,
}

/// [`agent_cap_message_from_disk`]'s decision without a count: the cap a
/// caller outside the gateway must hold to. Used by the pack importer,
/// which adds several employees at once and so has to compare
/// `current + adding` itself.
pub async fn agent_cap_from_disk(home_dir: &std::path::Path) -> AgentCap {
    let runtime = LicenseRuntime::bootstrap(home_dir.to_path_buf(), production_registry()).await;
    let snapshot = runtime.snapshot().await;
    let edition =
        duduclaw_core::EditionProfile::resolve_from_env(None, Some(snapshot.tier.as_toml_key()));

    if edition.is_personal() {
        return AgentCap { max: duduclaw_core::EditionProfile::personal_max_agents(), tier: None };
    }

    let agent_override = snapshot.max_agents.is_some();
    let is_self_host = is_self_host_deployment();
    if !agent_cap_enforced(is_self_host, agent_override) {
        return AgentCap { max: 0, tier: Some(snapshot.tier.to_string()) };
    }
    AgentCap {
        max: runtime.effective_max_agents().await,
        tier: Some(snapshot.tier.to_string()),
    }
}

// ── Phone-home loop ───────────────────────────────────────────

async fn phone_home_loop(runtime: LicenseRuntime) {
    let mut next_sleep = runtime.cadence_sleep().await;
    loop {
        tokio::time::sleep(next_sleep).await;

        // Same flight and 30 s spacing as `sync_now`: a verified dashboard /
        // probe sync that just ran counts as this cycle's refresh and the
        // next sleep is re-computed from the stored `last_phone_home`. A
        // cycle that made no vendor call and adopted no verified answer
        // (offline / failed sync just before, flight held too long) retries
        // in about an hour.
        next_sleep = runtime.scheduled_cycle(|| async {
            let result = runtime.do_phone_home_once().await;
            if let Err(e) = &result {
                // Non-fatal, re-evaluated next cycle, but logged at warn: a
                // refresh that keeps failing ends in the offline grace period
                // (until 2026-10-22 every refresh failed the nonce check at
                // debug level and nobody saw it).
                warn!(error = %e, "phone-home attempt failed; will retry next cycle");
            }
            result
        }).await;
    }
}

impl LicenseRuntime {
    /// One scheduled phone-home; returns how long the loop sleeps next.
    async fn scheduled_cycle<F, Fut>(&self, call: F) -> StdDuration
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<PhoneHomeOk, PhoneHomeError>>,
    {
        match self.scheduled_phone_home(call).await {
            SyncOutcome::Busy { .. } => {
                crate::license_rev::jittered(SCHEDULED_RETRY, crate::license_rev::PROBE_JITTER)
            }
            _ => self.cadence_sleep().await,
        }
    }

    /// The normal cadence: until the next refresh is due (at least a day,
    /// see [`Self::next_phone_home_sleep`]), or a day without a licence.
    async fn cadence_sleep(&self) -> StdDuration {
        // No license or OpenSource — wait a day and re-check (operator may
        // install one mid-flight).
        self.next_phone_home_sleep().await.unwrap_or(StdDuration::from_secs(24 * 60 * 60))
    }

    /// One scheduled phone-home through the shared flight
    /// ([`SyncCaller::Scheduled`]).
    async fn scheduled_phone_home<F, Fut>(&self, call: F) -> SyncOutcome
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<PhoneHomeOk, PhoneHomeError>>,
    {
        self.sync_gated_as(SyncCaller::Scheduled, call).await
    }

    async fn next_phone_home_sleep(&self) -> Option<StdDuration> {
        let inner = self.state.read().await;
        let license = inner.license.as_ref()?;
        let interval_days = self.gate.phone_home_interval_days(license.tier);
        if interval_days <= 0 {
            return None;
        }
        let days_since = license.days_since_phone_home().max(0);
        let days_until_due = (interval_days - days_since).max(1) as u64;
        let secs = days_until_due * 24 * 60 * 60;
        Some(StdDuration::from_secs(secs).max(MIN_PHONE_HOME_SLEEP))
    }

    async fn do_phone_home_once(&self) -> Result<PhoneHomeOk, PhoneHomeError> {
        // The store first: when it is busy (every store permit taken by
        // operations still running) no other blocking work is submitted.
        let mut request = self.refresh_snapshot().await?;
        // Generated only when absent; any other key-store failure omits the
        // field (the refresh still renews the tier, the relay stays unbound).
        let key_home = self.home_dir.clone();
        request.installation_pubkey = tokio::task::spawn_blocking(move ||
            crate::installation_identity::ensure_public_key_hex(&key_home)).await.ok().flatten();
        request.proposed_fingerprint = duduclaw_license::fingerprint::refresh_fingerprint(&request.machine_fingerprint)
            .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
        let subscription_id = &request.subscription_id;
        let machine_fingerprint = &request.machine_fingerprint;
        let endpoint = format!(
            "{}/v1/license/refresh",
            self.control_url.trim_end_matches('/')
        );

        // Every response authenticates this fresh nonce and full request context.
        let nonce = uuid::Uuid::new_v4().to_string();
        let proposed = &request.proposed_fingerprint;
        let mut request_body = json!({
            "subscription_id": subscription_id,
            "machine_fingerprint": machine_fingerprint,
            "current_version": env!("CARGO_PKG_VERSION"),
            "nonce": nonce,
            "telemetry": {},
        });
        // Existing issuers require exact equality with the installed binding.
        // A proposal is only effective after the account holder approves the
        // re-bind in the console. An old fingerprint is not authentication.
        if proposed != machine_fingerprint {
            request_body["new_fingerprint"] = json!(proposed);
        }
        // Capability signal (design §2.3): the issuer only adds the signed
        // `installation_pubkey` to licenses for requests that carry it. An
        // unknown key becomes an owner-approval request in the license console.
        if let Some(pubkey) = request.installation_pubkey.as_deref() {
            request_body["installation_pubkey"] = json!(pubkey);
        }

        let response = crate::license_transport::request(reqwest::Method::POST, &endpoint, PHONE_HOME_TIMEOUT)
            .await.map_err(PhoneHomeError::Http)?
            .json(&request_body)
            .send()
            .await
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?;

        let status_code = response.status();
        if !status_code.is_success() {
            return Err(PhoneHomeError::Http(format!(
                "control-plane returned HTTP {status_code}"
            )));
        }
        // Bounded before anything is parsed or verified.
        let body: serde_json::Value = crate::license_transport::read_bounded_json(
            response, crate::license_transport::MAX_LICENSE_RESPONSE_BYTES,
        ).await.map_err(PhoneHomeError::Parse)?;

        if status_code == reqwest::StatusCode::ACCEPTED
            || body.get("status").and_then(|v| v.as_str()) == Some("pending") {
            self.verify_status_response(&request, &nonce, &body, "pending")?;
            info!("license rebind pending account-holder console approval; installed license unchanged");
            return Ok(PhoneHomeOk::Pending);
        }
        match body.get("status").and_then(|v| v.as_str()) {
            Some("active") => {
                // Reject unbound responses before artifact validation.
                if !response_nonce_ok(&nonce, &body) {
                    return Err(PhoneHomeError::NonceMismatch);
                }

                let new_license_value = body
                    .get("license")
                    .ok_or_else(|| PhoneHomeError::Parse("missing 'license' field".into()))?;
                let new_license: License = serde_json::from_value(new_license_value.clone())
                    .map_err(|e| PhoneHomeError::Parse(e.to_string()))?;

                // C4 / C4.4: a valid signature is necessary but NOT sufficient.
                // The bootstrap path (`load_and_validate`) also enforces
                // expiry / machine-fingerprint / grace via `validate()`; the
                // refresh path previously skipped it, so a buggy/compromised
                // control plane (or a replayed signed response) could install
                // an expired or other-machine license. `accept_refreshed_license`
                // composes signature-trust + `validate()` so the acceptance
                // decision is pure and unit-testable.
                let phone_home = self.gate.phone_home_interval_days(new_license.tier);
                let grace = self.gate.grace_period_days(new_license.tier);
                let accepted = accept_refreshed_license(
                    &self.registry,
                    &new_license,
                    cached_candidates(),
                    phone_home,
                    grace,
                    &request, &nonce, &body,
                )?;
                let changed = self.apply_refresh(&request, new_license, &accepted.matched).await?;
                info!(changed, "phone-home succeeded; license refreshed");
                if let Some(reason) = accepted.relay_key_reason {
                    info!(reason,
                        "MFA email relay unavailable until the account owner approves this installation key in the license console");
                }
                Ok(PhoneHomeOk::Refreshed { changed })
            }
            Some("revoked") => {
                self.apply_revocation_response(&request, &nonce, &body).await?;
                Ok(PhoneHomeOk::Revoked)
            }
            other => Err(PhoneHomeError::Parse(format!(
                "unexpected refresh response status: {other:?}"
            ))),
        }
    }

    async fn refresh_snapshot(&self) -> Result<RefreshSnapshot, PhoneHomeError> {
        // Lock order is always memory then disk; release both before network I/O.
        let mut inner = self.state.write().await;
        let (store, contents) = match self.locked_store().await {
            Ok(locked) => locked,
            // A store that is no licence at all (FIFO, oversized) is
            // reconciled like an unreadable file, as before.
            Err(StoreReadError::Rejected(_)) => {
                self.install_unreadable(&mut inner);
                return Err(PhoneHomeError::NoLicense);
            }
            // Busy / transient: the in-memory licence is left untouched.
            Err(error) => return Err(PhoneHomeError::from_store(error)),
        };
        let same = match inner.license.as_ref() {
            Some(license) => contents.same_license(license).unwrap_or(false),
            None => false,
        };
        if !same {
            // A CLI writer cannot update our memory. Reconcile under the same
            // lock before rejecting the old request, including durable denial.
            let validated = validate_store_contents(&self.registry, &self.gate, &contents);
            self.install_reconciled(&mut inner, validated);
            drop(store);
            return Err(if inner.license.is_some() { PhoneHomeError::StaleResponse } else { PhoneHomeError::NoLicense });
        }
        drop(store);
        let license = inner.license.as_ref().ok_or(PhoneHomeError::NoLicense)?;
        Ok(RefreshSnapshot {
            subscription_id: license.subscription_id.clone(),
            machine_fingerprint: license.machine_fingerprint.clone(),
            proposed_fingerprint: license.machine_fingerprint.clone(),
            revision: inner.revision,
            disk: contents.snapshot(),
            is_self_host: is_self_host_deployment(),
            installation_pubkey: None,
        })
    }

    /// Replace memory with what the disk holds, as validated by
    /// `read_validated_license_locked` (the bootstrap path). The caller holds
    /// the state write lock and the disk lock, in that order. This is the
    /// reconcile the phone-home / CRL loops have always done; it was only
    /// moved here so [`Self::reload_from_disk_if_changed`] shares it.
    fn install_reconciled(&self, inner: &mut LicenseRuntimeInner,
        validated: Result<Option<License>, &'static str>) {
        let (license, warning) = match validated {
            Ok(license) => (license, None),
            Err(reason) => (None, Some(reason)),
        };
        inner.license = license;
        inner.validation_warning = warning;
        inner.revision += 1;
        clear_effective_fingerprint();
        if let Some(license) = inner.license.as_ref() {
            set_effective_fingerprint(&license.machine_fingerprint);
        }
        crate::distributor_notice::runtime::reset(&self.home_dir);
        crate::distributor_brand::runtime::reset(&self.home_dir);
    }

    /// Publish the same closed state as bootstrap while retaining the refusal
    /// memo. Do not cache the rejected stamp as successfully reconciled: the
    /// memo's retry deadline must still work, and a return to an earlier disk
    /// state (including absence) must not be skipped.
    fn install_unreadable(&self, inner: &mut LicenseRuntimeInner) -> bool {
        if let Ok(mut last) = self.sync.disk_stamp.lock() {
            *last = None;
        }
        let changed = inner.license.is_some() || inner.validation_warning != Some("unreadable");
        if changed {
            self.install_reconciled(inner, Err("unreadable"));
        }
        changed
    }

    /// RM-175: apply a `license.json` rewritten by another writer (the CLI's
    /// `duduclaw license refresh|activate|deactivate`) without a restart.
    ///
    /// Cheap first: when the length / modification time / inode of the
    /// licence file and the revocation marker equal the last reconciled
    /// state, nothing is read. Otherwise the existing digest comparison
    /// (`same_license`) decides, and a differing file goes through the same
    /// validation the phone-home / CRL reconcile uses. Returns `true` only
    /// when the licence in effect (or its validation state) changed.
    ///
    /// Fail closed: a lock, metadata or read error — including a file that is
    /// temporarily unreadable right now — leaves the in-memory licence untouched
    /// and is retried on the next call. A refused file drops to OpenSource
    /// with the same closed warning as bootstrap. All file and lock work runs
    /// on a blocking thread; a lock held elsewhere gives up after [`LICENSE_LOCK_DEADLINE`].
    /// A file refused as unusable (symbolic link, FIFO or other non-regular
    /// file, over 64 KiB, malformed JSON) is remembered by its stamp (plus
    /// the digest of the bytes read for malformed content), logged once and
    /// not read again until the stamp changes or [`REJECTED_RETRY`] passes.
    /// With every store permit taken by operations still running, nothing is
    /// submitted and the call returns `false` at once.
    pub async fn reload_from_disk_if_changed(&self) -> bool {
        self.reconcile_with_disk(false).await
    }

    async fn reconcile_with_disk(&self, force: bool) -> bool {
        let home = self.home_dir.clone();
        let stamp = match run_store_op(&self.store_ops, move || Ok(DiskStamp::read(&home))).await {
            Ok(stamp) => stamp,
            // Every store permit is taken (or the stamp read itself hung):
            // nothing else is submitted this time.
            Err(StoreReadError::Busy) => return false,
            Err(_) => None,
        };
        if !force {
            if let (Some(stamp), Ok(last)) = (stamp.as_ref(), self.sync.disk_stamp.lock()) {
                if last.as_ref() == Some(stamp) {
                    return false;
                }
            }
            // A store refused before is not read again by the watcher until
            // it changes or `REJECTED_RETRY` passes: no 60-second re-read of
            // a hostile file. A forced (admin) sync always reads once.
            if let (Some(stamp), Ok(rejected)) = (stamp.as_ref(), self.sync.rejected.lock()) {
                if rejected.as_ref().is_some_and(|rejected| rejected.covers(stamp)) {
                    return false;
                }
            }
        }
        if let Some(empty) = stamp.as_ref().filter(|s| s.license.is_none() && s.marker.is_none()) {
            // Unlicensed home, nothing on disk: do not create lock files.
            let unlicensed = {
                let inner = self.state.read().await;
                inner.license.is_none() && inner.validation_warning.is_none()
            };
            if unlicensed {
                if let Ok(mut last) = self.sync.disk_stamp.lock() {
                    *last = Some(empty.clone());
                }
                return false;
            }
        }
        let changed = {
            // Lock order: memory then disk, as everywhere else.
            let mut inner = self.state.write().await;
            // The disk lock (`_store`) is held to the end of this block.
            let (_store, contents) = match self.locked_store().await {
                Ok(locked) => locked,
                // FIFO, oversized, non-regular: metadata-only identity.
                Err(StoreReadError::Rejected(reason)) => {
                    self.remember_rejected(stamp, None, reason);
                    return self.install_unreadable(&mut inner);
                }
                Err(error) => {
                    // Busy or transient: keep the licence in effect, retry
                    // on the next call.
                    debug!(%error, "license file not reconciled this time; keeping the license in effect");
                    return false;
                }
            };
            let same = match inner.license.as_ref() {
                Some(license) => match contents.same_license(license) {
                    Ok(same) => same,
                    Err(_) => {
                        self.remember_rejected(stamp, contents.license_digest(), "malformed");
                        return self.install_unreadable(&mut inner);
                    }
                },
                None => false,
            };
            if same {
                false
            } else {
                let validated = validate_store_contents(&self.registry, &self.gate, &contents);
                if matches!(validated, Err("unreadable")) {
                    // Only a malformed document gets here (I/O failures
                    // returned above): refused until the file changes.
                    self.remember_rejected(stamp, contents.license_digest(), "malformed");
                    return self.install_unreadable(&mut inner);
                }
                let unchanged = match &validated {
                    Ok(license) => inner.validation_warning.is_none()
                        && serde_json::to_value(license).ok() == serde_json::to_value(&inner.license).ok(),
                    Err(reason) => inner.license.is_none() && inner.validation_warning == Some(*reason),
                };
                if unchanged {
                    false
                } else {
                    self.install_reconciled(&mut inner, validated);
                    true
                }
            }
        };
        // Record what was seen BEFORE the comparison: a write racing with it
        // changes the stamp again and is reconciled on the next call.
        if let (Some(stamp), Ok(mut last)) = (stamp, self.sync.disk_stamp.lock()) {
            *last = Some(stamp);
        }
        if let Ok(mut rejected) = self.sync.rejected.lock() {
            *rejected = None;
        }
        if changed {
            let snapshot = self.snapshot().await;
            info!(tier = %snapshot.tier, installed = snapshot.installed,
                "license file changed on disk; applied without restart");
            if snapshot.subscription_id.is_some() {
                // The reconcile dropped the cached distributor notice / brand
                // of the previous file; fetch the current ones now.
                crate::distributor_notice::runtime::spawn_fetch_now(self.clone());
                crate::distributor_brand::runtime::spawn_fetch_now(self.clone());
            }
        }
        changed
    }

    /// RM-175: refresh the licence from the control plane now (dashboard
    /// `license.sync`, change probe). Wraps the scheduled phone-home
    /// (`do_phone_home_once`) without changing how its answer is verified.
    ///
    /// One sync at a time, shared with the scheduled phone-home: a caller
    /// arriving while one is in flight waits (at most [`FLIGHT_WAIT`]) and
    /// takes that sync's outcome. Vendor calls of either kind are at least
    /// [`SYNC_MIN_SPACING`] apart; inside that window the answer is `Busy`
    /// and no request is sent. A licence the CLI wrote is reconciled first,
    /// so the request carries the identity actually installed. An absent or
    /// unusable file first drops the runtime to the bootstrap-equivalent state.
    pub async fn sync_now(&self) -> SyncOutcome {
        self.sync_gated(move || async move {
            let result = self.do_phone_home_once().await;
            if let Err(error) = &result {
                warn!(%error, "license sync failed; the installed license is unchanged");
            }
            result
        }).await
    }

    async fn sync_gated<F, Fut>(&self, call: F) -> SyncOutcome
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<PhoneHomeOk, PhoneHomeError>>,
    {
        self.sync_gated_as(SyncCaller::OnDemand, call).await
    }

    /// THE refresh primitive for both caller kinds: acquire the flight
    /// (bounded wait), take a result that finished while waiting, check and
    /// record the last-start time, run, record the result.
    async fn sync_gated_as<F, Fut>(&self, caller: SyncCaller, call: F) -> SyncOutcome
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<PhoneHomeOk, PhoneHomeError>>,
    {
        use std::sync::atomic::Ordering;
        let seen = self.sync.generation.load(Ordering::Acquire);
        let mut flight = match self.sync.flight.try_lock() {
            Ok(guard) => guard,
            Err(_) => match tokio::time::timeout(FLIGHT_WAIT, self.sync.flight.lock()).await {
                Ok(guard) => guard,
                Err(_) => {
                    // The holder is stuck past every transport / lock bound
                    // it should have hit. Skip; scheduled callers retry in
                    // about an hour.
                    warn!(caller = ?caller, "license sync skipped: another sync is still in flight");
                    return SyncOutcome::Busy { retry_after_secs: SYNC_MIN_SPACING.as_secs() };
                }
            },
        };
        if self.sync.generation.load(Ordering::Acquire) != seen {
            // A sync (of either kind) finished while this caller waited. A
            // scheduled caller takes only a verified answer; otherwise the
            // spacing check below answers `Busy` (retry in about an hour).
            if let Some(outcome) = self.sync.last_outcome() {
                if caller == SyncCaller::OnDemand || outcome.is_verified_answer() {
                    return outcome;
                }
            }
        }
        self.reconcile_with_disk(true).await;
        if self.notice_identity().await.is_none() {
            let warning = self.state.read().await.validation_warning;
            if let Some(reason) = warning {
                return self.sync.finish(SyncOutcome::UnreadableLicenseFile { reason });
            }
            return self.sync.finish(SyncOutcome::NoLicense);
        }
        let now = tokio::time::Instant::now();
        if let Some(last) = *flight {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < SYNC_MIN_SPACING {
                if caller == SyncCaller::Scheduled {
                    // A verified answer from the sync that just ran is this
                    // cycle's: the loop re-computes its next sleep from the
                    // stored `last_phone_home`, so nothing due is skipped.
                    // An offline / failed one is not adopted: no second
                    // vendor call inside the window, and the `Busy` below
                    // makes the loop retry in about an hour.
                    if let Some(outcome) = self.sync.last_outcome().filter(SyncOutcome::is_verified_answer) {
                        debug!(outcome = outcome.as_str(), "scheduled phone-home: a sync just ran; using its result");
                        return outcome;
                    }
                }
                let wait = SYNC_MIN_SPACING - elapsed;
                let retry_after_secs = wait.as_millis().div_ceil(1000).max(1) as u64;
                return SyncOutcome::Busy { retry_after_secs };
            }
        }
        *flight = Some(now);
        let mut outcome = sync_outcome(call().await);
        // The file may also have become unusable during refresh_snapshot(),
        // after the initial reconciliation. Report that published state too.
        if outcome == SyncOutcome::NoLicense {
            if let Some(reason) = self.state.read().await.validation_warning {
                outcome = SyncOutcome::UnreadableLicenseFile { reason };
            }
        }
        self.sync.finish(outcome)
    }

    /// Lock `license.json.lock` (bounded) and read the store, off the async
    /// workers.
    async fn locked_store(&self) -> Result<(storage::LockedLicenseStore, storage::StoreContents), StoreReadError> {
        lock_and_read(&self.store_ops, self.home_dir.join(storage::DEFAULT_LICENSE_FILENAME)).await
    }

    /// Remember a refused store so the watcher does not read it again until
    /// it changes or [`REJECTED_RETRY`] passes; logged once per distinct file
    /// state (stamp plus, for a content refusal, the digest of the bytes
    /// read). The caller also publishes the unlicensed, unreadable state.
    fn remember_rejected(&self, stamp: Option<DiskStamp>, digest: Option<[u8; 32]>, reason: &'static str) {
        let Some(stamp) = stamp else {
            debug!(reason, "license file refused; running unlicensed");
            return;
        };
        let Ok(mut rejected) = self.sync.rejected.lock() else { return };
        // Same bytes refused again (only re-stamped, or retried after the
        // period): no second warning.
        let known = rejected.as_ref().is_some_and(|known|
            known.digest == digest && (digest.is_some() || known.stamp == stamp));
        if !known {
            warn!(reason, "license file on disk refused (not re-read until it changes); running unlicensed");
        }
        // A re-read of the same refused state restarts the retry period.
        *rejected = Some(RejectedStore { stamp, digest, at: tokio::time::Instant::now() });
    }

    fn verify_status_response(&self, request: &RefreshSnapshot, nonce: &str,
        body: &serde_json::Value, status: &str) -> Result<(), PhoneHomeError> {
        duduclaw_license::refresh::validate_status_response(&self.registry, body, status,
            nonce, &request.subscription_id, &request.machine_fingerprint, &request.proposed_fingerprint)
            .map_err(|e| PhoneHomeError::Rejected(e.to_string()))
    }

    /// Returns whether the refreshed licence grants something different from
    /// the one it replaced ([`entitlement_changed`]).
    async fn apply_refresh(&self, request: &RefreshSnapshot, license: License,
        matched: &duduclaw_license::FingerprintMatch) -> Result<bool, PhoneHomeError> {
        let mut inner = self.state.write().await;
        if !request.matches(&inner) { return Err(PhoneHomeError::StaleResponse); }
        let (store, contents) = self.locked_store().await.map_err(PhoneHomeError::from_store)?;
        if contents.snapshot() != request.disk {
            return Err(PhoneHomeError::StaleResponse);
        }
        let to_save = license.clone();
        let _store = run_store_op(&self.store_ops, move || {
            store.save(&to_save).map_err(|e| StoreReadError::Io(e.to_string()))?;
            Ok(store)
        }).await.map_err(PhoneHomeError::from_store)?;
        let changed = inner.license.as_ref().is_none_or(|old| entitlement_changed(old, &license));
        inner.license = Some(license);
        inner.validation_warning = None;
        inner.revision += 1;
        if matched.legacy { warn_legacy_binding_once(); }
        set_effective_fingerprint(&matched.fingerprint);
        Ok(changed)
    }

    async fn apply_revocation_response(&self, request: &RefreshSnapshot, nonce: &str,
        body: &serde_json::Value) -> Result<(), PhoneHomeError> {
        self.verify_status_response(request, nonce, body, "revoked")?;
        let reason = body["reason"].as_str().ok_or_else(|| PhoneHomeError::Parse("missing reason".into()))?;
        self.apply_revocation(request, reason).await
    }

    async fn apply_revocation(&self, request: &RefreshSnapshot, reason: &str) -> Result<(), PhoneHomeError> {
        let mut inner = self.state.write().await;
        if !request.matches(&inner) { return Err(PhoneHomeError::StaleResponse); }
        let (store, contents) = self.locked_store().await.map_err(PhoneHomeError::from_store)?;
        if contents.snapshot() != request.disk {
            return Err(PhoneHomeError::StaleResponse);
        }
        // A persisted marker fences restart even if removing the artifact fails.
        let reason = reason.to_string();
        let revoked = run_store_op(&self.store_ops, move || {
            let persisted = store.revoke(&reason).map_err(|e| e.to_string());
            Ok((store, persisted))
        }).await;
        let (_store, persisted) = match revoked {
            Ok((store, persisted)) => (Some(store), persisted),
            // Abandoned past the deadline (the write may still complete), or
            // no store permit free (the lock guard is dropped unwritten):
            // memory is revoked anyway, fail closed, and the error reported.
            Err(error) => (None, Err(error.to_string())),
        };
        inner.license = None;
        inner.revision += 1;
        inner.validation_warning = Some("revoked");
        clear_effective_fingerprint();
        crate::distributor_notice::runtime::reset(&self.home_dir);
        crate::distributor_brand::runtime::reset(&self.home_dir);
        persisted.map_err(|e| PhoneHomeError::Save(e.to_string()))?;
        info!("downgraded to OpenSource mode");
        Ok(())
    }

}

/// File (under `home_dir`) marking that the installed license was revoked.
/// Honored at bootstrap so a revocation survives restarts even if the license
/// file itself could not be deleted. Cleared on a successful re-subscription.
const REVOKED_FILENAME: &str = storage::REVOKED_FILENAME;

#[derive(Debug, thiserror::Error)]
enum PhoneHomeError {
    #[error("license changed while refresh was in flight; stale response discarded")]
    StaleResponse,
    #[error("no license installed")]
    NoLicense,
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("save error: {0}")]
    Save(String),
    /// C4.3: the refresh response did not echo our request nonce — treated as a
    /// replay and the license is NOT installed.
    #[error("refresh response nonce missing or mismatched — rejecting replay")]
    NonceMismatch,
    /// C4.4: a (possibly validly-signed) refreshed license failed acceptance —
    /// untrusted signature, expired, wrong machine, or grace exceeded.
    #[error("refreshed license rejected: {0}")]
    Rejected(String),
    /// `license.json.lock` was held by another process past the deadline;
    /// nothing was read, written or changed in memory.
    #[error("license file is busy (locked by another process); nothing changed")]
    Busy,
}

impl PhoneHomeError {
    fn from_store(error: StoreReadError) -> Self {
        match error {
            StoreReadError::Busy => Self::Busy,
            other => Self::Save(other.to_string()),
        }
    }
}

/// What a completed (verified) phone-home did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhoneHomeOk {
    /// A verified licence was installed; `changed` = it grants something
    /// different from the previous one.
    Refreshed { changed: bool },
    /// A verified revocation was applied.
    Revoked,
    /// A verified "rebind pending" answer; nothing installed.
    Pending,
}

/// Map a phone-home result onto the outcome a sync caller sees. Pure.
fn sync_outcome(result: Result<PhoneHomeOk, PhoneHomeError>) -> SyncOutcome {
    match result {
        Ok(PhoneHomeOk::Refreshed { changed: true }) => SyncOutcome::Updated,
        Ok(PhoneHomeOk::Refreshed { changed: false }) => SyncOutcome::Unchanged,
        Ok(PhoneHomeOk::Revoked) => SyncOutcome::Revoked,
        Ok(PhoneHomeOk::Pending) => SyncOutcome::PendingRebind,
        Err(PhoneHomeError::NoLicense) => SyncOutcome::NoLicense,
        Err(PhoneHomeError::Http(_)) => SyncOutcome::Offline { reason: "network" },
        Err(PhoneHomeError::Parse(_)) | Err(PhoneHomeError::NonceMismatch) => {
            SyncOutcome::Offline { reason: "invalid_response" }
        }
        Err(PhoneHomeError::Rejected(_)) => SyncOutcome::Offline { reason: "rejected" },
        Err(PhoneHomeError::Save(_)) => SyncOutcome::Failed { reason: "storage" },
        Err(PhoneHomeError::StaleResponse) => SyncOutcome::Failed { reason: "changed_during_sync" },
        Err(PhoneHomeError::Busy) => SyncOutcome::Failed { reason: "license_file_busy" },
    }
}

/// Whether `new` grants something different from `old`. Renewal bookkeeping
/// (issue time, phone-home stamp, signature) is ignored; every other field —
/// tier, expiry, seat quota, binding, flags, metadata — counts. A comparison
/// that cannot be made reports a change.
fn entitlement_changed(old: &License, new: &License) -> bool {
    let mut normalized = old.clone();
    normalized.issued_at = new.issued_at;
    normalized.last_phone_home = new.last_phone_home;
    normalized.signature = new.signature.clone();
    match (serde_json::to_value(&normalized), serde_json::to_value(new)) {
        (Ok(old), Ok(new)) => old != new,
        _ => true,
    }
}

/// C4.3: pure nonce-echo check. The response is accepted only when it carries a
/// top-level `"nonce"` string equal to the one we sent. Missing field, wrong
/// type, or a different value all fail. This detects verbatim replay only;
/// response-signature v2 is needed to authenticate the echo.
fn response_nonce_ok(sent: &str, body: &serde_json::Value) -> bool {
    body.get("nonce").and_then(|v| v.as_str()) == Some(sent)
}

/// Captured under the state lock before network I/O. Revision also fences an
/// ABA change where a new installation restores the same subscription/binding.
struct RefreshSnapshot {
    subscription_id: String,
    machine_fingerprint: String,
    proposed_fingerprint: String,
    revision: u64,
    disk: storage::LicenseSnapshot,
    is_self_host: bool,
    /// Local installation public key sent with the request, if available.
    installation_pubkey: Option<String>,
}

impl RefreshSnapshot {
    fn matches(&self, inner: &LicenseRuntimeInner) -> bool {
        inner.revision == self.revision && inner.license.as_ref().is_some_and(|license|
            license.subscription_id == self.subscription_id &&
            license.machine_fingerprint == self.machine_fingerprint)
    }
}

/// An accepted refresh: the binding match plus the MFA relay key reason
/// (`None` when the signed installation key equals the requesting key).
struct RefreshAccepted {
    matched: duduclaw_license::FingerprintMatch,
    relay_key_reason: Option<&'static str>,
}

/// Validate the signed artifact, request identity and deployment before CAS.
/// A license whose `installation_pubkey` is absent or differs from the local
/// key is still accepted (tier and binding are unaffected); only the relay
/// reason is set, and the relay stays unavailable.
#[allow(clippy::too_many_arguments)]
fn accept_refreshed_license(
    registry: &PublicKeyRegistry,
    new_license: &License,
    candidates: &[FingerprintCandidate],
    phone_home_days: i64,
    grace_days: i64,
    request: &RefreshSnapshot,
    nonce: &str,
    body: &serde_json::Value,
) -> Result<RefreshAccepted, PhoneHomeError> {
    duduclaw_license::refresh::validate_response(registry, new_license, body, nonce,
        &request.subscription_id, &request.machine_fingerprint, &request.proposed_fingerprint)
        .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
    new_license.validate_tier_deployment_binding(request.is_self_host)
        .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
    let matched = new_license.validate_any(candidates, phone_home_days, grace_days)
        .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
    Ok(RefreshAccepted { matched,
        relay_key_reason: refreshed_relay_key_reason(new_license, request.installation_pubkey.as_deref()) })
}

// ── CRL loop ──────────────────────────────────────────────────

async fn crl_loop(runtime: LicenseRuntime) {
    loop {
        // Re-emit a proactive expiry warning on each daily cycle so a
        // long-running gateway surfaces the 30/7-day pre-expiry window even if
        // it was started while the license was still comfortably in date.
        runtime.log_expiry_status().await;
        if let Err(e) = runtime.do_crl_fetch_once().await {
            debug!(error = %e, "CRL fetch failed; will retry next cycle");
        }
        tokio::time::sleep(CRL_POLL_INTERVAL).await;
    }
}

impl LicenseRuntime {
    /// Best-effort, read-only proactive expiry warning for the current license.
    /// Called once per CRL cycle (daily).
    async fn log_expiry_status(&self) {
        let inner = self.state.read().await;
        if let Some(lic) = inner.license.as_ref() {
            log_expiry_warning(lic.tier, lic.days_until_expiry());
        }
    }

    async fn do_crl_fetch_once(&self) -> Result<(), PhoneHomeError> {
        let request = match self.refresh_snapshot().await {
            Ok(request) => request,
            Err(PhoneHomeError::NoLicense) => return Ok(()),
            Err(error) => return Err(error),
        };
        let subscription_id = &request.subscription_id;

        let endpoint = format!("{}/v1/license/crl", self.control_url.trim_end_matches('/'));
        let response = crate::license_transport::request(reqwest::Method::GET, &endpoint, PHONE_HOME_TIMEOUT)
            .await.map_err(PhoneHomeError::Http)?
            .send()
            .await
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?;
        // Bounded before the signature is checked.
        let crl: SignedCrl = crate::license_transport::read_bounded_json(
            response, crate::license_transport::MAX_CRL_RESPONSE_BYTES,
        ).await.map_err(PhoneHomeError::Parse)?;

        if let Err(e) = crl.verify(&self.registry) {
            return Err(PhoneHomeError::Parse(format!("CRL signature invalid: {e}")));
        }

        // HS13/D5: a valid signature is not enough — a signed-but-old CRL can be
        // replayed forever to mask a later revocation. Enforce two freshness
        // checks before trusting the document:
        //
        //   1. `is_stale()` — reject CRLs older than their own declared TTL.
        //   2. Monotonicity floor — reject any CRL whose `generated_at` is not
        //      strictly newer than the last CRL we accepted (persisted to disk),
        //      so a replayed pre-revocation snapshot is rejected even within TTL.
        if crl.is_stale() {
            return Err(PhoneHomeError::Parse(format!(
                "CRL is stale (generated_at {} older than ttl {}s) — rejecting replay",
                crl.generated_at, crl.ttl_seconds
            )));
        }

        {
            let inner = self.state.read().await;
            if let Err(reason) = check_crl_monotonic(crl.generated_at, inner.last_seen_crl_at) {
                return Err(PhoneHomeError::Parse(reason));
            }
        }

        // Accepted: advance the last-seen floor (in-memory + on-disk) before
        // acting on the revocation list. We only move the floor forward.
        {
            let mut inner = self.state.write().await;
            let advance = inner
                .last_seen_crl_at
                .map_or(true, |prev| crl.generated_at > prev);
            if advance {
                inner.last_seen_crl_at = Some(crl.generated_at);
                if let Err(e) = write_last_seen_crl(&self.home_dir, crl.generated_at) {
                    warn!(error = %e, "could not persist last-seen CRL timestamp");
                }
            }
        }

        if crl.is_revoked(&subscription_id) {
            warn!(subscription_id = %subscription_id, "CRL lists our subscription as revoked — downgrading to OpenSource");
            self.apply_revocation(&request, "crl_revoked").await?;
        } else {
            debug!(
                revoked_count = crl.revoked.len(),
                "CRL fetched; subscription not revoked"
            );
        }
        Ok(())
    }
}

/// HS13/D5: pure monotonicity check. A CRL is rejected when its `generated_at`
/// is older than the last-seen accepted CRL (a replay). Equal-or-newer is
/// accepted (equal allows a benign re-fetch of the same document).
fn check_crl_monotonic(
    crl_generated_at: chrono::DateTime<Utc>,
    last_seen: Option<chrono::DateTime<Utc>>,
) -> Result<(), String> {
    match last_seen {
        Some(prev) if crl_generated_at < prev => Err(format!(
            "CRL generated_at {crl_generated_at} older than last-seen {prev} — rejecting replay"
        )),
        _ => Ok(()),
    }
}

/// Read the persisted last-seen CRL `generated_at` timestamp, if present and
/// parseable. Any error (missing file, malformed contents) yields `None` so a
/// fresh install simply starts with no floor.
fn read_last_seen_crl(home_dir: &Path) -> Option<chrono::DateTime<Utc>> {
    let path = home_dir.join(LAST_CRL_FILENAME);
    let raw = std::fs::read_to_string(path).ok()?;
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Persist the last-seen CRL `generated_at` timestamp (RFC3339) under
/// `home_dir`. Best-effort; callers log but do not propagate failures.
fn write_last_seen_crl(home_dir: &Path, ts: chrono::DateTime<Utc>) -> std::io::Result<()> {
    duduclaw_core::config::create_home_dir(home_dir)?;
    let path = home_dir.join(LAST_CRL_FILENAME);
    std::fs::write(path, ts.to_rfc3339())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn seat_note_keeps_cap_url_separated() {
        for tier in [None, Some("team")] {
            for separator in [" ", "\n"] {
                let message = with_agent_seat_note(&agent_cap_text(5, tier), separator);
                assert!(message.contains(&format!("https://futurecorp.tw{separator}")));
                assert!(message.ends_with(&format!("{separator}{AGENT_SEAT_NOTE}")));
                assert!(!message.contains(&format!("https://futurecorp.tw{AGENT_SEAT_NOTE}")));
                assert_eq!(message.contains('\n'), separator == "\n");
            }
        }
    }

    use chrono::Duration as ChronoDuration;
    use duduclaw_license::License;

    static FINGERPRINT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) const TEST_HARDWARE_FINGERPRINT: &str = "synthetic-hardware-for-license-runtime-tests";

    tokio::task_local! {
        pub(super) static VALIDATION_CANDIDATES: &'static [FingerprintCandidate];
    }

    // Scope fixtures to explicitly selected tests. Never upgrade the real MAC
    // candidates, mutate process-wide caches, or pass the override to child tasks.
    pub(crate) async fn with_hardware_fixture<T>(future: impl std::future::Future<Output = T>) -> T {
        static CANDIDATES: std::sync::OnceLock<Vec<FingerprintCandidate>> = std::sync::OnceLock::new();
        let candidates = CANDIDATES.get_or_init(|| vec![FingerprintCandidate::for_test(TEST_HARDWARE_FINGERPRINT,
            duduclaw_license::FingerprintStrength::Hardware)]);
        VALIDATION_CANDIDATES.scope(candidates.as_slice(), future).await
    }

    #[tokio::test]
    async fn hardware_fixture_is_scoped_and_does_not_override_child_tasks() {
        let observed = cached_candidates().to_vec();
        let child_expected = observed.clone();
        with_hardware_fixture(async {
            assert_eq!(cached_candidates()[0].fingerprint(), TEST_HARDWARE_FINGERPRINT);
            assert_eq!(cached_candidates()[0].strength(), duduclaw_license::FingerprintStrength::Hardware);
            let child = tokio::spawn(async move {
                assert_eq!(cached_candidates(), child_expected.as_slice());
            });
            child.await.unwrap();
        }).await;
        assert_eq!(cached_candidates(), observed.as_slice());
    }

    fn fake_license(tier: LicenseTier, fingerprint: &str) -> License {
        License::new(
            "sub_test",
            "cus_test",
            tier,
            fingerprint,
            ChronoDuration::days(30),
            "v1",
        )
    }

    #[test]
    fn parse_license_key_rejects_junk_and_paths() {
        assert!(parse_license_key("").is_err());
        assert!(parse_license_key("   ").is_err());
        assert!(parse_license_key("not-base64-not-json!!!").is_err());
        // A filesystem path must NOT be accepted from the dashboard form.
        assert!(parse_license_key("/etc/passwd").is_err());
        // Valid base64 of non-license JSON still fails at deserialization.
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"{\"foo\":1}");
        assert!(parse_license_key(&b64).is_err());
    }

    #[test]
    fn parse_license_key_accepts_json_and_base64() {
        let lic = fake_license(LicenseTier::Studio, "fp_test");
        let json = serde_json::to_string(&lic).unwrap();
        let parsed = parse_license_key(&json).unwrap();
        assert_eq!(parsed.subscription_id, "sub_test");

        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        // Whitespace/newlines inside a pasted base64 blob are tolerated.
        let wrapped = format!("{}\n{}", &b64[..10], &b64[10..]);
        let parsed = parse_license_key(&wrapped).unwrap();
        assert_eq!(parsed.customer_id, "cus_test");
    }

    #[tokio::test]
    async fn install_and_reload_fails_closed_without_registry_keys() {
        let tmp = std::env::temp_dir().join(format!("dudu-lic-rt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let rt = LicenseRuntime::bootstrap(tmp.clone(), PublicKeyRegistry::new()).await;
        let lic = fake_license(LicenseTier::Studio, "fp_test");
        let err = rt.install_and_reload(lic).await.unwrap_err();
        assert!(err.contains("公鑰"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn opensource_when_no_keys_available() {
        // No embedded keys → license can't be trusted → opensource
        let registry = PublicKeyRegistry::new();
        let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();

        let lic = fake_license(LicenseTier::Studio, "fp_test");
        // Pretend we managed to load this (skip the actual file lookup)
        let result = load_and_validate_with(&registry, &gate, lic).await;
        assert!(result.is_none());
    }

    /// Helper for tests — variant of load_and_validate that takes the
    /// loaded license directly instead of reading from disk.
    async fn load_and_validate_with(
        registry: &PublicKeyRegistry,
        gate: &FeatureGate,
        license: License,
    ) -> Option<License> {
        if registry.is_empty() {
            return None;
        }
        if registry.verify(&license).is_err() {
            return None;
        }
        let ph = gate.phone_home_interval_days(license.tier);
        let gp = gate.grace_period_days(license.tier);
        match license.validate_any(cached_candidates(), ph, gp) {
            Ok(_) => Some(license),
            Err(_) => None,
        }
    }

    #[tokio::test]
    async fn mfa_bootstrap_uses_explicit_home_and_verifies_signed_license() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (key, public) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", public);
            let tier = if is_self_host_deployment() { LicenseTier::Oem } else { LicenseTier::Business };
            let mut license = fake_license(tier, TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &key);
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            assert_eq!(runtime.snapshot().await.tier, tier);
            assert!(!runtime.boot_edition(None, None).await.is_personal());
            assert!(!home.path().join("users.db").exists());

            std::fs::write(home.path().join(REVOKED_FILENAME), "revoked").unwrap();
            let revoked = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            assert_eq!(revoked.snapshot().await.tier, LicenseTier::OpenSource);
            std::fs::remove_file(home.path().join(REVOKED_FILENAME)).unwrap();
            license.signature[0] ^= 1;
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let invalid = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            assert_eq!(invalid.snapshot().await.tier, LicenseTier::OpenSource);
        }).await;
    }

    #[tokio::test]
    async fn mfa_boot_edition_uses_live_license_before_identity_creation() {
        use duduclaw_core::EditionProfile;
        for (tier, personal) in [
            (LicenseTier::Business, false), (LicenseTier::Oem, false),
            (LicenseTier::Partner, false), (LicenseTier::SelfHostPro, false),
            (LicenseTier::PersonalProSelfHost, true), (LicenseTier::OpenSource, true),
        ] {
            let home = tempfile::tempdir().unwrap();
            // Model an already-validated runtime, as the other snapshot tests do.
            let runtime = LicenseRuntime {
                state: Arc::new(RwLock::new(LicenseRuntimeInner {
                revision: 0,
                    license: Some(fake_license(tier, "fp_mfa")), validation_warning: None, last_seen_crl_at: None,
                })),
                gate: Arc::new(FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap()),
                registry: Arc::new(PublicKeyRegistry::new()),
                home_dir: home.path().into(), control_url: "https://control.invalid".into(),
                sync: Arc::default(), store_ops: STORE_OPS.clone(),
            };
            let edition = runtime.boot_edition(None, None).await;
            assert_eq!(edition.is_personal(), personal, "{tier:?}");
            assert!(!home.path().join("users.db").exists());
            crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
                home.path(), "127.0.0.1:18789".parse().unwrap(), edition.is_personal(), None,
            ).unwrap();
            assert_eq!(crate::mfa::boot_policy(home.path(), edition.is_personal()).unwrap(), !personal);
            // Once enabled, even an OpenSource/Personal runtime must keep enforcing.
            drop(duduclaw_auth::UserDb::new_with_mfa_default(&home.path().join("users.db"), true).unwrap());
            std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
            crate::mfa::reload_policy(home.path());
            assert!(crate::mfa::boot_policy(home.path(), edition.is_personal()).unwrap());
            assert!(crate::mfa::enforced(home.path(), edition.is_personal()).unwrap());
            assert!(runtime.boot_edition(None, Some(EditionProfile::Personal)).await.is_personal());
            assert_eq!(runtime.boot_edition(Some("enterprise"), Some(EditionProfile::Personal)).await.is_personal(),
                personal);
        }
    }

    #[tokio::test]
    async fn snapshot_when_opensource_reports_correctly() {
        let snap = LicenseSnapshot::from_state(None, LicenseTier::OpenSource);
        assert_eq!(snap.tier, LicenseTier::OpenSource);
        assert_eq!(snap.mode, "opensource");
        assert!(!snap.installed);
        assert!(snap.customer_id.is_none());
        assert!(snap.subscription_id.is_none());
    }

    #[tokio::test]
    async fn snapshot_with_license_reports_commercial_fields() {
        let lic = fake_license(LicenseTier::SelfHostPro, "fp_snap");
        let snap = LicenseSnapshot::from_state(Some(&lic), LicenseTier::SelfHostPro);
        assert_eq!(snap.tier, LicenseTier::SelfHostPro);
        assert_eq!(snap.mode, "commercial");
        assert!(snap.installed);
        assert_eq!(snap.customer_id.as_deref(), Some("cus_test"));
        assert!(snap.days_until_expiry.unwrap() >= 29);
    }

    #[tokio::test]
    async fn brand_reads_recheck_expiry_and_offline_grace_without_a_refresh() {
        use futures_util::FutureExt;
        use crate::distributor_brand::{Brand, ORIGINAL_PRODUCT_NAME, effective_for};

        let home = tempfile::tempdir().unwrap();
        let runtime = LicenseRuntime {
            state: Arc::new(RwLock::new(LicenseRuntimeInner {
                revision: 0,
                license: Some(fake_license(LicenseTier::Oem, "fp_brand")),
                validation_warning: None,
                last_seen_crl_at: None,
            })),
            gate: Arc::new(FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap()),
            registry: Arc::new(PublicKeyRegistry::new()),
            home_dir: home.path().into(),
            control_url: "https://control.invalid".into(),
            sync: Arc::default(),
            store_ops: STORE_OPS.clone(),
        };
        let accepted = crate::distributor_brand::runtime::Accepted {
            subscription_id: "sub_test".into(),
            distributor_id: "dist_test".into(),
            version: 1,
            brand: Brand {
                product_name: "Distributor".into(),
                logo_light: "data:image/png;base64,iVBORw0KGgo=".into(),
                logo_dark: None,
            },
        };
        for tier in [LicenseTier::Hobby, LicenseTier::Solo, LicenseTier::Studio,
            LicenseTier::Business, LicenseTier::Partner, LicenseTier::PersonalProSelfHost,
            LicenseTier::SelfHostPro, LicenseTier::Oem] {
            {
                let mut inner = runtime.state.write().await;
                inner.license = Some(fake_license(tier, "fp_brand"));
            }
            // Both brand poll entry points use notice_identity, independently
            // of the customer's tier issued through the distributor console.
            assert_eq!(runtime.notice_identity().await,
                Some(("sub_test".into(), "fp_brand".into())), "{tier:?}");
            let grace = runtime.gate.grace_period_days(tier);
            assert!(grace > 0);

            // Keep the same installed license and accepted brand. Only advance
            // stored times to model a long-running gateway with failed polls.
            for (expires_in_days, offline_days, expected) in [
                (30, 0, "Distributor"),
                (30, grace - 1, "Distributor"),
                (-1, 0, ORIGINAL_PRODUCT_NAME),
                (-1, grace + 1, ORIGINAL_PRODUCT_NAME),
                (30, grace + 1, ORIGINAL_PRODUCT_NAME),
                (30, 0, "Distributor"),
            ] {
                {
                    let mut inner = runtime.state.write().await;
                    let license = inner.license.as_mut().unwrap();
                    license.expires_at = Utc::now() + ChronoDuration::days(expires_in_days);
                    license.last_phone_home = Utc::now() - ChronoDuration::days(offline_days);
                }
                // Synchronous brand renderers use this same nonblocking read.
                let snapshot = runtime.snapshot().now_or_never().unwrap();
                assert_eq!(runtime.current_tier().await, snapshot.tier);
                let expected_warning = if expires_in_days < 0 { Some("expired") }
                    else if offline_days > grace { Some("grace_period_expired") }
                    else { None };
                assert_eq!(snapshot.validation_warning, expected_warning);
                assert_eq!(snapshot.mode, if expected_warning.is_some() { "opensource" } else { "commercial" });
                let name = effective_for(snapshot.tier, snapshot.subscription_id.as_deref(), &accepted)
                    .map(|brand| brand.product_name)
                    .unwrap_or_else(|| ORIGINAL_PRODUCT_NAME.into());
                assert_eq!(name, expected, "{tier:?}: expiry={expires_in_days}, offline={offline_days}");
            }
        }
        runtime.state.write().await.license = None;
        assert_eq!(runtime.notice_identity().await, None);
        let snapshot = runtime.snapshot().await;
        assert_eq!(effective_for(snapshot.tier, snapshot.subscription_id.as_deref(), &accepted), None);
    }

    /// Helper that exercises the env-var parser without touching the
    /// global environment — pulls a closure of (key, value) pairs.
    fn build_registry_from_pairs(pairs: &[(&str, &str)]) -> PublicKeyRegistry {
        let mut registry = PublicKeyRegistry::new();
        for (k, v) in pairs {
            let Some(suffix) = k.strip_prefix(PUBKEY_ENV_PREFIX) else {
                continue;
            };
            if suffix.is_empty() {
                continue;
            }
            let key_id = suffix.to_ascii_lowercase();
            let bytes = match hex::decode(v.trim()) {
                Ok(b) if b.len() == 32 => b,
                _ => continue,
            };
            registry = registry.with_key(&key_id, bytes);
        }
        registry
    }

    #[test]
    fn registry_parses_two_keys() {
        let r = build_registry_from_pairs(&[
            ("DUDUCLAW_LICENSE_PUBKEY_V1", "00".repeat(32).as_str()),
            ("DUDUCLAW_LICENSE_PUBKEY_V2", "ff".repeat(32).as_str()),
            ("UNRELATED_VAR", "foo"),
        ]);
        assert!(r.get("v1").is_some());
        assert!(r.get("v2").is_some());
        assert!(r.get("v3").is_none());
    }

    #[test]
    fn registry_skips_invalid_keys() {
        let r = build_registry_from_pairs(&[
            ("DUDUCLAW_LICENSE_PUBKEY_BAD_HEX", "not-hex"),
            ("DUDUCLAW_LICENSE_PUBKEY_WRONG_LEN", "deadbeef"),
            ("DUDUCLAW_LICENSE_PUBKEY_", "00".repeat(32).as_str()),
            ("DUDUCLAW_LICENSE_PUBKEY_OK", "11".repeat(32).as_str()),
        ]);
        assert!(r.get("bad_hex").is_none());
        assert!(r.get("wrong_len").is_none());
        assert!(r.get("").is_none());
        assert!(r.get("ok").is_some());
    }

    // ── HS13 / D5: CRL freshness + monotonicity ────────────────────────────────

    #[test]
    fn crl_monotonic_rejects_older_than_last_seen() {
        let last_seen = Utc::now();
        let older = last_seen - ChronoDuration::seconds(10);
        // An older CRL (a replay of a pre-revocation snapshot) must be rejected.
        assert!(check_crl_monotonic(older, Some(last_seen)).is_err());
    }

    #[test]
    fn crl_monotonic_accepts_newer_or_equal() {
        let last_seen = Utc::now();
        let newer = last_seen + ChronoDuration::seconds(10);
        assert!(check_crl_monotonic(newer, Some(last_seen)).is_ok());
        // Equal generated_at is a benign re-fetch of the same document.
        assert!(check_crl_monotonic(last_seen, Some(last_seen)).is_ok());
    }

    #[test]
    fn crl_monotonic_accepts_first_ever() {
        // No floor yet → first CRL is always accepted.
        assert!(check_crl_monotonic(Utc::now(), None).is_ok());
    }

    // ── C4.3: phone-home nonce anti-replay ─────────────────────────────────

    #[test]
    fn response_nonce_matches() {
        let body = json!({ "status": "active", "nonce": "abc-123" });
        assert!(response_nonce_ok("abc-123", &body));
    }

    #[test]
    fn response_nonce_mismatch_rejected() {
        let body = json!({ "status": "active", "nonce": "different" });
        assert!(!response_nonce_ok("abc-123", &body));
    }

    #[test]
    fn response_nonce_missing_rejected() {
        // No nonce at all (e.g. a replayed pre-nonce response) must fail closed.
        let body = json!({ "status": "active" });
        assert!(!response_nonce_ok("abc-123", &body));
        // Wrong type also fails.
        let body_num = json!({ "status": "active", "nonce": 42 });
        assert!(!response_nonce_ok("abc-123", &body_num));
    }

    // ── C4.4: refreshed-license acceptance (signature + validate) ───────────

    /// Generate a fresh Ed25519 issuer keypair via `ring` (already a gateway
    /// dep). Returns (signing key pair, 32-byte public key bytes). `ring`'s
    /// Ed25519 is standards-compliant, so signatures verify under the license
    /// crate's `ed25519-dalek` verifier.
    fn gen_issuer_keypair() -> (ring::signature::Ed25519KeyPair, Vec<u8>) {
        use ring::signature::KeyPair as _;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("generate pkcs8");
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8");
        let pubkey = kp.public_key().as_ref().to_vec();
        (kp, pubkey)
    }

    /// Sign `license`'s canonical payload with `kp`, mutating its `signature`
    /// field in place — mirrors what the closed-source signer does on the wire.
    fn sign_license(license: &mut License, kp: &ring::signature::Ed25519KeyPair) {
        let payload = license.canonical_payload().expect("canonical payload");
        let sig = kp.sign(&payload);
        license.signature = sig.as_ref().to_vec();
    }

    /// Build a freshly-issued, currently-valid license bound to `fp`.
    fn valid_license_for(fp: &str) -> License {
        // 30-day expiry, last_phone_home = now → well within any grace window.
        fake_license(LicenseTier::SelfHostPro, fp)
    }

    #[test]
    fn mfa_relay_tiers_require_paid_and_non_nfr() {
        for tier in [LicenseTier::OpenSource, LicenseTier::Hobby, LicenseTier::Solo,
            LicenseTier::Studio, LicenseTier::Business, LicenseTier::Partner,
            LicenseTier::PersonalProSelfHost, LicenseTier::SelfHostPro, LicenseTier::Oem] {
            assert_eq!(mfa_relay_tier_eligibility(tier, false).is_ok(), tier.is_paid());
            assert_eq!(mfa_relay_tier_eligibility(tier, true), Err("relay_nfr"));
        }
        assert_eq!(mfa_relay_tier_eligibility(LicenseTier::Partner, false), Err("relay_nfr"));
    }

    #[tokio::test]
    async fn mfa_relay_license_revalidates_disk_and_preserves_signed_artifact() {
        with_hardware_fixture(async {
            let home = tempfile::tempdir().unwrap();
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();
            let path = home.path().join("license.json");
            assert!(mfa_relay_license_with(home.path(), &registry, &gate).is_err());
            assert!(!path.with_file_name("license.json.lock").exists());
            let (key, _) = crate::installation_identity::load_or_create(home.path()).unwrap();
            let bound = Some(key.public_key_hex());
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            license.control_url = Some("https://attacker.example.com".into());
            license.installation_pubkey = bound.clone();
            sign_license(&mut license, &kp);
            storage::save_to(&license, &path).unwrap();
            let mut original = serde_json::to_value(&license).unwrap();
            original["future_metadata"] = json!({"preserved":true});
            std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
            let relay = mfa_relay_license_with(home.path(), &registry, &gate).unwrap();
            assert_eq!(relay.artifact, original);
            assert_eq!(relay.fingerprint, TEST_HARDWARE_FINGERPRINT);
            assert_eq!(relay.subscription_id, license.subscription_id);
            assert_eq!(relay.key.public_key_hex(), key.public_key_hex());
            let mut oem = fake_license(LicenseTier::Oem, "");
            oem.installation_pubkey = bound.clone();
            sign_license(&mut oem, &kp);
            storage::save_to(&oem, &path).unwrap();
            assert_eq!(mfa_relay_license_with(home.path(), &registry, &gate).err().unwrap(), "relay_unbound");
            oem.machine_fingerprint = TEST_HARDWARE_FINGERPRINT.into();
            sign_license(&mut oem, &kp);
            storage::save_to(&oem, &path).unwrap();
            assert!(mfa_relay_license_with(home.path(), &registry, &gate).is_ok());
            let mut partner = fake_license(LicenseTier::Partner, TEST_HARDWARE_FINGERPRINT);
            assert!(!partner.nfr);
            partner.installation_pubkey = bound.clone();
            sign_license(&mut partner, &kp);
            storage::save_to(&partner, &path).unwrap();
            assert_eq!(mfa_relay_license_with(home.path(), &registry, &gate).err().unwrap(), "relay_nfr");
            let mut nfr = license.clone();
            nfr.nfr = true;
            sign_license(&mut nfr, &kp);
            storage::save_to(&nfr, &path).unwrap();
            assert_eq!(mfa_relay_license_with(home.path(), &registry, &gate).err().unwrap(), "relay_nfr");
            nfr.nfr = false;
            storage::save_to(&nfr, &path).unwrap();
            assert_eq!(mfa_relay_license_with(home.path(), &registry, &gate).err().unwrap(), "no_smtp");
            for mut denied in [
                fake_license(LicenseTier::OpenSource, TEST_HARDWARE_FINGERPRINT),
                fake_license(LicenseTier::Hobby, TEST_HARDWARE_FINGERPRINT),
                valid_license_for("another-machine"),
                { let mut l=license.clone(); l.expires_at=Utc::now()-ChronoDuration::days(1); l },
            ] {
                sign_license(&mut denied, &kp);
                storage::save_to(&denied, &path).unwrap();
                assert!(mfa_relay_license_with(home.path(), &registry, &gate).is_err());
            }
            let mut tampered = license.clone();
            tampered.subscription_id = "tampered".into();
            storage::save_to(&tampered, &path).unwrap();
            assert!(mfa_relay_license_with(home.path(), &registry, &gate).is_err());
            storage::save_to(&license, &path).unwrap();
            std::fs::write(home.path().join(REVOKED_FILENAME), "revoked").unwrap();
            assert!(mfa_relay_license_with(home.path(), &registry, &gate).is_err());
        }).await;
    }

    #[tokio::test]
    async fn relay_requires_bound_matching_installation_key_without_changing_tier() {
        with_hardware_fixture(async {
            let home = tempfile::tempdir().unwrap();
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();
            let path = home.path().join("license.json");
            let save = |pubkey: Option<String>| {
                let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
                license.installation_pubkey = pubkey;
                sign_license(&mut license, &kp);
                storage::save_to(&license, &path).unwrap();
            };
            let tier = || read_validated_license(home.path(), &registry, &gate).unwrap().unwrap().tier;
            let reason = || mfa_relay_license_with(home.path(), &registry, &gate).err();

            // Unbound license: relay off, paid tier intact, no key generated.
            save(None);
            assert_eq!(reason(), Some("relay_key_unbound"));
            assert_eq!(tier(), LicenseTier::SelfHostPro);
            assert!(!home.path().join("state/installation.key").exists());

            // Bound, but the local key is missing (e.g. restored to a new machine).
            save(Some("ab".repeat(32)));
            assert_eq!(reason(), Some("relay_key_mismatch"));
            assert_eq!(tier(), LicenseTier::SelfHostPro);
            assert!(!home.path().join("state/installation.key").exists(), "availability never generates keys");

            // A local key that differs from the signed one.
            let (key, created) = crate::installation_identity::load_or_create(home.path()).unwrap();
            assert!(created);
            assert_eq!(reason(), Some("relay_key_mismatch"));
            assert_eq!(tier(), LicenseTier::SelfHostPro);

            // Bound to this installation.
            save(Some(key.public_key_hex()));
            assert_eq!(reason(), None);

            // The relay cannot reach the network without an eligible RelayLicense.
            save(None);
            let calls = std::sync::Arc::new(std::sync::Mutex::new(0usize));
            let seen = calls.clone();
            let result = crate::mfa_email::relay_send_for_test(home.path(), "x@y.test", "123456", 10_000,
                |home| mfa_relay_license_with(home, &registry, &gate), move |_call| {
                    *seen.lock().unwrap() += 1;
                    std::future::ready(Err("unexpected relay request"))
                }).await;
            assert!(result.is_err());
            assert_eq!(*calls.lock().unwrap(), 0, "no challenge or enroll call when unbound");
            assert!(!home.path().join("state/mfa-relay-state.json").exists(), "enrollment budget untouched");
            save(Some("cd".repeat(32)));
            let seen = calls.clone();
            assert!(crate::mfa_email::relay_send_for_test(home.path(), "x@y.test", "123456", 10_000,
                |home| mfa_relay_license_with(home, &registry, &gate), move |_call| {
                    *seen.lock().unwrap() += 1;
                    std::future::ready(Err("unexpected relay request"))
                }).await.is_err());
            assert_eq!(*calls.lock().unwrap(), 0, "no challenge or enroll call on key mismatch");
        }).await;
    }

    #[test]
    fn validation_warning_is_only_exposed_to_admin() {
        let mut snapshot = LicenseSnapshot::from_state(None, LicenseTier::OpenSource);
        snapshot.installed = true;
        snapshot.validation_warning = Some("machine_mismatch");
        assert_eq!(snapshot.clone().for_admin(true).validation_warning, Some("machine_mismatch"));
        let visible = serde_json::to_value(snapshot.for_admin(false)).unwrap();
        assert!(visible["validation_warning"].is_null());
        assert!(visible.get("machine_fingerprint").is_none());
    }

    #[tokio::test]
    async fn invalid_installed_license_retains_warning_and_recovers_after_activation() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let missing = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            assert!(!missing.snapshot().await.installed);
            assert_eq!(missing.snapshot().await.validation_warning, None);
            let mut license = valid_license_for("another-machine");
            sign_license(&mut license, &kp);
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let mut runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            runtime.control_url = "http://127.0.0.1:9".into();
            let snap = runtime.snapshot().await;
            assert_eq!(snap.tier, LicenseTier::OpenSource);
            assert_eq!(snap.mode, "opensource");
            assert!(snap.installed);
            let reason = if duduclaw_license::fingerprint_hardware_unavailable() {
                "hardware_identity_unavailable"
            } else { "machine_mismatch" };
            assert_eq!(snap.validation_warning, Some(reason));
            assert!(snap.customer_id.is_none());
            assert!(snap.subscription_id.is_none());
            let json = serde_json::to_string(&snap).unwrap();
            assert!(!json.contains("another-machine"));
            license.machine_fingerprint = TEST_HARDWARE_FINGERPRINT.to_string();
            sign_license(&mut license, &kp);
            let restored = runtime.install_and_reload(license).await.unwrap();
            assert_eq!(restored.tier, LicenseTier::SelfHostPro);
            assert!(restored.installed);
            assert_eq!(restored.validation_warning, None);
        }).await;
    }

    #[tokio::test]
    async fn malformed_or_bad_signature_license_warns_without_exposing_content() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
        let home = tempfile::tempdir().unwrap();
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        std::fs::write(home.path().join("license.json"), "private-invalid-content").unwrap();
        let runtime = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
        assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));
        let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
        sign_license(&mut license, &kp);
        license.signature[0] ^= 1;
        storage::save_to(&license, &home.path().join("license.json")).unwrap();
        let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
        assert_eq!(runtime.snapshot().await.validation_warning, Some("invalid_signature"));
    }

    fn test_refresh_snapshot(license: &License) -> RefreshSnapshot {
        RefreshSnapshot { subscription_id: license.subscription_id.clone(),
            machine_fingerprint: license.machine_fingerprint.clone(),
            proposed_fingerprint: license.machine_fingerprint.clone(), revision: 0,
            disk: storage::LockedLicenseStore::acquire(&tempfile::tempdir().unwrap().path().join("license.json"))
                .unwrap().snapshot().unwrap(), is_self_host: true, installation_pubkey: None }
    }

    fn signed_response(kp: &ring::signature::Ed25519KeyPair, license: Option<&License>,
        request: &RefreshSnapshot, status: &str, nonce: &str) -> serde_json::Value {
        use base64::Engine as _;
        let digest = license.map(duduclaw_license::refresh::license_digest).transpose().unwrap().unwrap_or_default();
        let reason = if status == "revoked" { "cancelled" } else { "" };
        let effective = "2020-01-01T00:00:00Z";
        let payload = duduclaw_license::refresh::response_payload(status, reason, effective, nonce,
            &request.subscription_id, &request.machine_fingerprint, &request.proposed_fingerprint, &digest).unwrap();
        json!({"status":status, "reason":reason, "effective_from":effective,
            "nonce":nonce, "subscription_id":request.subscription_id,
            "original_fingerprint":request.machine_fingerprint,
            "proposed_fingerprint":request.proposed_fingerprint, "license_digest":digest,
            "public_key_id":"v1", "response_signature_version":2,
            "response_signature":base64::engine::general_purpose::STANDARD.encode(kp.sign(&payload).as_ref())})
    }

    fn accept_test_response(kp: &ring::signature::Ed25519KeyPair, registry: &PublicKeyRegistry, license: &License,
        candidates: &[FingerprintCandidate], ph: i64, grace: i64) -> Result<duduclaw_license::FingerprintMatch, PhoneHomeError> {
        let request = test_refresh_snapshot(license);
        let body = signed_response(kp, Some(license), &request, "active", "nonce");
        accept_refreshed_license(registry, license, candidates, ph, grace, &request, "nonce", &body)
            .map(|accepted| accepted.matched)
    }

    #[test]
    fn accept_refreshed_license_keeps_unbound_or_mismatched_installation_key() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let candidates = [FingerprintCandidate::for_test(TEST_HARDWARE_FINGERPRINT,
            duduclaw_license::FingerprintStrength::Hardware)];
        let local = "ab".repeat(32);
        for (bound, sent, expected) in [
            (None, Some(local.clone()), Some("relay_key_unbound")),
            (Some("cd".repeat(32)), Some(local.clone()), Some("relay_key_mismatch")),
            (Some(local.clone()), None, Some("relay_key_mismatch")),
            (Some(local.clone()), Some(local.clone()), None),
        ] {
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            license.installation_pubkey = bound;
            sign_license(&mut license, &kp);
            let mut request = test_refresh_snapshot(&license);
            request.installation_pubkey = sent;
            let body = signed_response(&kp, Some(&license), &request, "active", "n");
            let accepted = accept_refreshed_license(&registry, &license, &candidates, 7, 14, &request, "n", &body)
                .expect("the paid license is accepted regardless of the relay key");
            assert_eq!(accepted.relay_key_reason, expected);
            assert_eq!(accepted.matched.fingerprint, TEST_HARDWARE_FINGERPRINT);
        }
    }

    #[test]
    fn accept_refreshed_license_binds_request_and_deployment() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let mut license = valid_license_for("old");
        sign_license(&mut license, &kp);
        let mut request = test_refresh_snapshot(&license);
        request.proposed_fingerprint = "new".into();
        let candidates = [
            FingerprintCandidate::for_test("old", duduclaw_license::FingerprintStrength::Hardware),
            FingerprintCandidate::for_test("new", duduclaw_license::FingerprintStrength::Hardware),
            FingerprintCandidate::for_test("unrequested", duduclaw_license::FingerprintStrength::Hardware),
        ];
        let accept = |license: &License, request: &RefreshSnapshot| {
            let body = signed_response(&kp, Some(license), request, "active", "n");
            accept_refreshed_license(&registry, license, &candidates, 7, 14, request, "n", &body)
        };
        assert!(accept(&license, &request).is_ok());
        license.machine_fingerprint = "new".into();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_ok());
        license.subscription_id = "different-subscription".into();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_err());
        license.subscription_id = request.subscription_id.clone();
        license.machine_fingerprint = "unrequested".into();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_err());
        // A legitimate unbound OEM remains supported, but cannot replace a
        // bound license or enter a cloud deployment through refresh.
        license.tier = LicenseTier::Oem;
        license.machine_fingerprint.clear();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_err());
        request.machine_fingerprint.clear();
        request.proposed_fingerprint.clear();
        assert!(accept(&license, &request).is_ok());
        request.is_self_host = false;
        assert!(accept(&license, &request).is_err());
    }

    #[test]
    fn refresh_snapshot_checks_subscription_binding_and_revision_independently() {
        let license = valid_license_for("fp");
        let request = test_refresh_snapshot(&license);
        let mut inner = LicenseRuntimeInner { revision: 0, license: Some(license.clone()),
            validation_warning: None, last_seen_crl_at: None };
        assert!(request.matches(&inner));
        inner.license.as_mut().unwrap().subscription_id = "other".into();
        assert!(!request.matches(&inner));
        inner.license = Some(license.clone());
        inner.license.as_mut().unwrap().machine_fingerprint = "other".into();
        assert!(!request.matches(&inner));
        inner.license = Some(license);
        inner.revision = 1;
        assert!(!request.matches(&inner));
        inner.license = None;
        assert!(!request.matches(&inner));
    }

    #[tokio::test]
    async fn stale_refresh_cannot_overwrite_reinstalled_license_or_disk() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let mut runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            runtime.control_url = "http://127.0.0.1:9".into();
            let request = runtime.refresh_snapshot().await.unwrap();
            // Identical subscription/binding still changes the installation. Both
            // revision and full disk snapshot fence a delayed quota rollback.
            let mut replacement = license.clone();
            replacement.max_agents = Some(42);
            sign_license(&mut replacement, &kp);
            runtime.install_and_reload(replacement).await.unwrap();
            let before = std::fs::read(home.path().join("license.json")).unwrap();
            let matched = license.validate_any(cached_candidates(), 7, 30).unwrap();
            assert!(matches!(runtime.apply_refresh(&request, license, &matched).await,
                Err(PhoneHomeError::StaleResponse)));
            assert_eq!(std::fs::read(home.path().join("license.json")).unwrap(), before);
            let inner = runtime.state.read().await;
            assert_eq!(inner.revision, 1);
            assert_eq!(inner.license.as_ref().unwrap().max_agents, Some(42));
        }).await;
    }

    #[tokio::test]
    async fn external_install_fences_gateway_refresh_and_revocation() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &path).unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            let request = runtime.refresh_snapshot().await.unwrap();
            let mut replacement = license.clone();
            replacement.max_agents = Some(42);
            sign_license(&mut replacement, &kp);
            // This writer never touches the gateway's in-memory revision.
            storage::save_to(&replacement, &path).unwrap();
            let before = std::fs::read(&path).unwrap();
            let matched = license.validate_any(cached_candidates(), 7, 30).unwrap();
            assert!(matches!(runtime.apply_refresh(&request, license, &matched).await,
                Err(PhoneHomeError::StaleResponse)));
            let revoked = signed_response(&kp, None, &request, "revoked", "n");
            assert!(matches!(runtime.apply_revocation_response(&request, "n", &revoked).await,
                Err(PhoneHomeError::StaleResponse)));
            // The independently authenticated CRL path uses the same disk CAS.
            assert!(matches!(runtime.apply_revocation(&request, "crl_revoked").await,
                Err(PhoneHomeError::StaleResponse)));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!home.path().join(REVOKED_FILENAME).exists());
            assert_eq!(runtime.state.read().await.revision, 0);
            assert!(matches!(runtime.refresh_snapshot().await, Err(PhoneHomeError::StaleResponse)));
            assert_eq!(runtime.snapshot().await.max_agents, Some(42));
            assert!(runtime.refresh_snapshot().await.is_ok());
        }).await;
    }

    #[tokio::test]
    async fn external_revocation_or_deactivation_reconciles_gateway_memory() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            for revoke in [true, false] {
                let home = tempfile::tempdir().unwrap();
                let path = home.path().join("license.json");
                let (kp, pubkey) = gen_issuer_keypair();
                let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
                let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
                sign_license(&mut license, &kp);
                storage::save_to(&license, &path).unwrap();
                let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
                let store = storage::LockedLicenseStore::acquire(&path).unwrap();
                if revoke { store.revoke("cancelled").unwrap(); } else { store.delete().unwrap(); }
                drop(store);
                // CRL capture reconciles disk first, even though there is now no
                // subscription to fetch. It must not retain the old paid state.
                runtime.do_crl_fetch_once().await.unwrap();
                assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
                assert!(runtime.state.read().await.license.is_none());
                assert_eq!(runtime.snapshot().await.validation_warning, if revoke { Some("revoked") } else { None });
            }
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bootstrap_waits_past_running_lock_deadline_without_blocking_async() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (previous, license, _) = licensed_runtime(home.path()).await;
            let registry = (*previous.registry).clone();
            let held = storage::LockedLicenseStore::acquire(&home.path().join("license.json")).unwrap();
            let release = async move {
                tokio::time::sleep(LICENSE_LOCK_DEADLINE + StdDuration::from_millis(250)).await;
                drop(held);
            };
            let started = std::time::Instant::now();
            let (runtime, ()) = tokio::join!(LicenseRuntime::bootstrap(home.path().into(), registry), release);
            assert!(started.elapsed() > LICENSE_LOCK_DEADLINE);
            assert!(started.elapsed() < BOOT_LICENSE_LOCK_DEADLINE);
            assert_eq!(runtime.snapshot().await.tier, license.tier);
        }).await;
    }

    #[test]
    fn bootstrap_read_waits_for_atomic_revocation() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.blocking_lock();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("license.json");
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
        sign_license(&mut license, &kp);
        storage::save_to(&license, &path).unwrap();
        let store = storage::LockedLicenseStore::acquire(&path).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let home_path = home.path().to_path_buf();
        let reader = std::thread::spawn(move || {
            let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();
            started_tx.send(()).unwrap();
            done_tx.send(read_validated_license(&home_path, &registry, &gate)).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(StdDuration::from_millis(50)).is_err());
        store.revoke("cancelled").unwrap();
        drop(store);
        assert!(matches!(done_rx.recv_timeout(StdDuration::from_secs(5)).unwrap(), Err("revoked")));
        reader.join().unwrap();
    }

    #[tokio::test]
    async fn unsigned_or_tampered_revocation_cannot_change_disk_or_memory() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &path).unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            let request = runtime.refresh_snapshot().await.unwrap();
            let signed = signed_response(&kp, None, &request, "revoked", "n");
            let before = std::fs::read(&path).unwrap();
            let mut forged = vec![json!({"status":"revoked"})];
            for field in ["reason", "effective_from", "nonce", "response_signature_version"] {
                let mut changed = signed.clone();
                changed.as_object_mut().unwrap().remove(field);
                forged.push(changed);
            }
            let mut changed = signed.clone();
            changed["reason"] = json!("different reason");
            forged.push(changed);
            for body in forged {
                assert!(runtime.apply_revocation_response(&request, "n", &body).await.is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert!(!home.path().join(REVOKED_FILENAME).exists());
                assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);
            }
            runtime.apply_revocation_response(&request, "n", &signed).await.unwrap();
            assert!(!path.exists());
            assert!(home.path().join(REVOKED_FILENAME).exists());
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            let restarted = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            assert_eq!(restarted.snapshot().await.validation_warning, Some("revoked"));
        }).await;
    }

    #[tokio::test]
    async fn weak_installed_license_is_opensource_with_admin_only_rebind_warning() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
        let home = tempfile::tempdir().unwrap();
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let weak = cached_candidates().iter().find(|c|
            c.strength() == duduclaw_license::FingerprintStrength::Weak).unwrap();
        let mut license = valid_license_for(weak.fingerprint());
        sign_license(&mut license, &kp);
        storage::save_to(&license, &home.path().join("license.json")).unwrap();
        let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.tier, LicenseTier::OpenSource);
        assert_eq!(snapshot.mode, "opensource");
        assert!(snapshot.installed);
        assert_eq!(snapshot.validation_warning, Some("weak_binding"));
        assert_eq!(snapshot.for_admin(false).validation_warning, None);
        assert!(runtime.install_and_reload(license).await.is_err());
    }

    #[test]
    fn accept_refreshed_license_accepts_valid() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let fp = TEST_HARDWARE_FINGERPRINT.to_string();

        let mut lic = valid_license_for(&fp);
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(
            res.is_ok(),
            "valid signed+current license must be accepted: {res:?}"
        );
        assert!(!res.unwrap().legacy, "strong match must not be flagged legacy");
    }

    #[test]
    fn accept_refreshed_license_accepts_a_legacy_candidate() {
        // A legacy real-hardware fingerprint remains accepted and flagged.
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let legacy_fp = "1743ebb5466bcad6c206e7e42371f45d".to_string();

        let mut lic = valid_license_for(&legacy_fp);
        sign_license(&mut lic, &kp);

        // Literal candidates, not this machine's real ones: a sandboxed test
        // runner can mask MACs, which would make strong == legacy and silently
        // stop exercising the fallback.
        let candidates = vec![FingerprintCandidate::for_test("strong-fp-of-this-machine", duduclaw_license::FingerprintStrength::Hardware),
            FingerprintCandidate::for_test(legacy_fp.clone(), duduclaw_license::FingerprintStrength::Hardware)];
        let matched = accept_test_response(&kp, &registry, &lic, &candidates, 7, 14)
            .expect("legacy-bound license must still be accepted");
        assert!(matched.legacy, "a compatibility match must be flagged legacy");
        assert_eq!(matched.fingerprint, legacy_fp);
    }

    #[test]
    fn effective_fingerprint_defaults_to_strong_and_records_legacy_matches() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.blocking_lock();
        // The effective fingerprint is what phone-home requests send.
        // It must fall back to the strong value whenever nothing matched, and
        // carry the legacy value while a legacy-bound license is active.
        clear_effective_fingerprint();
        assert_eq!(cached_fingerprint(), strong_fingerprint());

        set_effective_fingerprint("1743ebb5466bcad6c206e7e42371f45d");
        assert_eq!(cached_fingerprint(), "1743ebb5466bcad6c206e7e42371f45d");

        clear_effective_fingerprint();
        assert_eq!(cached_fingerprint(), strong_fingerprint());
    }

    #[test]
    fn candidate_list_leads_with_the_strong_fingerprint() {
        let candidates = cached_candidates();
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].fingerprint(), strong_fingerprint());
    }

    #[test]
    fn accept_refreshed_license_rejects_expired() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let fp = TEST_HARDWARE_FINGERPRINT.to_string();

        // Issued + expired in the past — validly signed but past expiry.
        let mut lic = License::new(
            "sub_exp",
            "cus_exp",
            LicenseTier::SelfHostPro,
            &fp,
            ChronoDuration::days(-1),
            "v1",
        );
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(res.is_err(), "expired license must be rejected");
    }

    #[test]
    fn accept_refreshed_license_rejects_wrong_fingerprint() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let current_fp = TEST_HARDWARE_FINGERPRINT.to_string();

        // Validly signed but bound to a *different* machine.
        let mut lic = valid_license_for("some-other-machine-fingerprint");
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&current_fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(
            res.is_err(),
            "license bound to another machine must be rejected"
        );
    }

    #[test]
    fn accept_refreshed_license_rejects_bad_signature() {
        // Sign with one key but trust a *different* key → signature untrusted.
        let (kp, _pubkey) = gen_issuer_keypair();
        let (_other_kp, other_pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", other_pubkey);
        let fp = TEST_HARDWARE_FINGERPRINT.to_string();

        let mut lic = valid_license_for(&fp);
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(
            res.is_err(),
            "license signed by an untrusted key must be rejected"
        );
    }

    #[test]
    fn deployment_binding_rejects_cloud_tier_on_self_host() {
        // A cloud-only tier must be refused on a self-host deployment …
        let cloud = fake_license(LicenseTier::Studio, "fp");
        assert!(cloud.validate_tier_deployment_binding(true).is_err());
        // … and accepted in Cloud.
        assert!(cloud.validate_tier_deployment_binding(false).is_ok());
    }

    #[test]
    fn deployment_binding_rejects_self_host_tier_in_cloud() {
        // Self-host-only tiers (incl. the new Partner NFR) must be refused in
        // Cloud and accepted on self-host.
        for tier in [
            LicenseTier::Partner,
            LicenseTier::PersonalProSelfHost,
            LicenseTier::SelfHostPro,
        ] {
            let lic = fake_license(tier, "fp");
            assert!(
                lic.validate_tier_deployment_binding(false).is_err(),
                "{tier} should be rejected in Cloud"
            );
            assert!(
                lic.validate_tier_deployment_binding(true).is_ok(),
                "{tier} should be accepted on self-host"
            );
        }
    }

    #[test]
    fn cap_exceeded_semantics() {
        // 0 = unlimited → never capped.
        assert!(!cap_exceeded(0, 0));
        assert!(!cap_exceeded(0, 1_000));
        // Hobby: max 1 → first is allowed, second is capped.
        assert!(!cap_exceeded(1, 0));
        assert!(cap_exceeded(1, 1));
        // Studio: max 3.
        assert!(!cap_exceeded(3, 2));
        assert!(cap_exceeded(3, 3));
        assert!(cap_exceeded(3, 4));
    }

    #[test]
    fn agent_cap_enforced_p_license_override() {
        // Cloud always enforces, override or not.
        assert!(agent_cap_enforced(false, false));
        assert!(agent_cap_enforced(false, true));
        // Self-host is normally exempt (Apache 2.0)...
        assert!(!agent_cap_enforced(true, false));
        // ...but a signed per-license max_agents override DOES enforce on
        // self-host (§7(b)) — this is the whole point of P-License.
        assert!(agent_cap_enforced(true, true));
    }

    #[test]
    fn os_native_quota_personal_is_one_enterprise_unlimited() {
        use duduclaw_core::EditionProfile;
        // Personal edition: exactly one OS-native seat (quota lock, not a
        // capability lock).
        assert_eq!(os_native_agent_quota(EditionProfile::Personal), Some(1));
        // Enterprise edition: uncapped — the license schema has no os-native
        // field, so there is nothing to gate on.
        assert_eq!(os_native_agent_quota(EditionProfile::Enterprise), None);
    }

    #[test]
    fn effective_max_agents_via_snapshot_field() {
        // A LicenseSnapshot carries the per-license override so the enforcement
        // path can detect it (is_some) even on a self-host tier.
        let mut lic = fake_license(LicenseTier::SelfHostPro, "fp");
        let snap_none = LicenseSnapshot::from_state(Some(&lic), LicenseTier::SelfHostPro);
        assert!(snap_none.max_agents.is_none());
        lic.max_agents = Some(2);
        let snap_some = LicenseSnapshot::from_state(Some(&lic), LicenseTier::SelfHostPro);
        assert_eq!(snap_some.max_agents, Some(2));
    }

    // ── Baked production issuer key (single-binary commercial upgrade) ──────

    #[test]
    fn baked_prod_pubkey_is_empty_or_32_byte_hex() {
        // The baked constant is either empty (v2 issuance pending) or a valid
        // 32-byte Ed25519 public key. A malformed non-empty value would silently
        // disable commercial licensing on stock binaries, so guard against it.
        // This test survives the pending→baked transition unchanged.
        if PROD_ISSUER_PUBKEY_HEX.is_empty() {
            return; // v2 pending — env-only, fail-safe.
        }
        let bytes = hex::decode(PROD_ISSUER_PUBKEY_HEX).expect("baked pubkey must be valid hex");
        assert_eq!(bytes.len(), 32, "Ed25519 public key must be 32 bytes");
    }

    #[test]
    fn production_registry_matches_baked_state() {
        // With no env override for the active key id, the registry reflects the
        // baked state: empty constant ⇒ no key (v1 retired, v2 pending); a baked
        // key ⇒ present. Guard: skip if the host exports an override for this id.
        let env_var = format!(
            "DUDUCLAW_LICENSE_PUBKEY_{}",
            PROD_ISSUER_KEY_ID.to_ascii_uppercase()
        );
        if std::env::var(&env_var).is_ok() {
            return;
        }
        let reg = production_registry();
        if PROD_ISSUER_PUBKEY_HEX.is_empty() {
            assert!(
                reg.get(PROD_ISSUER_KEY_ID).is_none(),
                "no key must be baked while v2 issuance is pending"
            );
        } else {
            assert_eq!(reg.get(PROD_ISSUER_KEY_ID).map(|k| k.len()), Some(32));
        }
    }

    #[test]
    fn expiry_urgency_thresholds() {
        use ExpiryUrgency::*;
        assert_eq!(classify_expiry_urgency(-1), Expired);
        assert_eq!(classify_expiry_urgency(0), Critical); // expires today, still valid
        assert_eq!(classify_expiry_urgency(7), Critical);
        assert_eq!(classify_expiry_urgency(8), Warning);
        assert_eq!(classify_expiry_urgency(30), Warning);
        assert_eq!(classify_expiry_urgency(31), Ok);
        assert_eq!(classify_expiry_urgency(365), Ok);
    }

    // ── RM-175: sync_now / reload_from_disk_if_changed ─────────────────────

    #[test]
    fn sync_outcome_maps_every_phone_home_result() {
        use SyncOutcome::*;
        assert_eq!(sync_outcome(Ok(PhoneHomeOk::Refreshed { changed: true })), Updated);
        assert_eq!(sync_outcome(Ok(PhoneHomeOk::Refreshed { changed: false })), Unchanged);
        assert_eq!(sync_outcome(Ok(PhoneHomeOk::Revoked)), Revoked);
        assert_eq!(sync_outcome(Ok(PhoneHomeOk::Pending)), PendingRebind);
        assert_eq!(sync_outcome(Err(PhoneHomeError::NoLicense)), NoLicense);
        assert_eq!(sync_outcome(Err(PhoneHomeError::Http("x".into()))), Offline { reason: "network" });
        assert_eq!(sync_outcome(Err(PhoneHomeError::Parse("x".into()))), Offline { reason: "invalid_response" });
        assert_eq!(sync_outcome(Err(PhoneHomeError::NonceMismatch)), Offline { reason: "invalid_response" });
        assert_eq!(sync_outcome(Err(PhoneHomeError::Rejected("x".into()))), Offline { reason: "rejected" });
        assert_eq!(sync_outcome(Err(PhoneHomeError::Save("x".into()))), Failed { reason: "storage" });
        assert_eq!(sync_outcome(Err(PhoneHomeError::StaleResponse)), Failed { reason: "changed_during_sync" });
        assert_eq!(sync_outcome(Err(PhoneHomeError::Busy)), Failed { reason: "license_file_busy" });
        // The wire form carries fixed codes only — never the underlying error text.
        let offline = sync_outcome(Err(PhoneHomeError::Http("https://secret.example/fp_123".into()))).to_json();
        assert_eq!(offline["outcome"], "offline");
        assert_eq!(offline["reason"], "network");
        assert!(!offline.to_string().contains("fp_123"));
        assert_eq!(Busy { retry_after_secs: 12 }.to_json()["retry_after_secs"], 12);
        assert!(Unchanged.to_json().get("reason").is_none());
    }

    #[test]
    fn entitlement_change_ignores_renewal_bookkeeping_only() {
        let (kp, _) = gen_issuer_keypair();
        let mut old = valid_license_for("fp");
        sign_license(&mut old, &kp);
        let mut renewed = old.clone();
        renewed.last_phone_home = old.last_phone_home + ChronoDuration::hours(1);
        renewed.issued_at = old.issued_at + ChronoDuration::hours(1);
        sign_license(&mut renewed, &kp);
        assert!(!entitlement_changed(&old, &renewed));
        let mut more_seats = renewed.clone();
        more_seats.max_agents = Some(9);
        assert!(entitlement_changed(&old, &more_seats));
        let mut later = renewed.clone();
        later.expires_at = old.expires_at + ChronoDuration::days(30);
        assert!(entitlement_changed(&old, &later));
        let mut other_tier = renewed;
        other_tier.tier = LicenseTier::Oem;
        assert!(entitlement_changed(&old, &other_tier));
    }

    /// A bootstrapped runtime with a signed, valid license on disk and in memory.
    async fn licensed_runtime(home: &Path) -> (LicenseRuntime, License, ring::signature::Ed25519KeyPair) {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
        sign_license(&mut license, &kp);
        storage::save_to(&license, &home.join("license.json")).unwrap();
        let mut runtime = LicenseRuntime::bootstrap(home.to_path_buf(), registry).await;
        // Never reach a real control plane from tests (transport refuses http).
        runtime.control_url = "http://127.0.0.1:9".into();
        assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);
        (runtime, license, kp)
    }

    #[tokio::test(start_paused = true)]
    async fn sync_now_is_single_flight_and_spaced_thirty_seconds() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (runtime, _, _) = licensed_runtime(home.path()).await;
            let calls_counter = AtomicUsize::new(0);
            let entered_flag = AtomicBool::new(false);
            let (calls, entered) = (&calls_counter, &entered_flag);
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

            // Two concurrent callers: one vendor call, both see its result.
            let first = runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.store(true, Ordering::SeqCst);
                let _ = release_rx.await;
                Ok(PhoneHomeOk::Refreshed { changed: true })
            });
            let second = async {
                while !entered.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
                runtime.sync_gated(move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(PhoneHomeOk::Refreshed { changed: false })
                }).await
            };
            let release = async {
                while !entered.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
                for _ in 0..4 { tokio::task::yield_now().await; }
                let _ = release_tx.send(());
            };
            let (a, b, ()) = tokio::join!(first, second, release);
            assert_eq!(a, SyncOutcome::Updated);
            assert_eq!(b, SyncOutcome::Updated, "the waiting caller takes the in-flight result");
            assert_eq!(calls.load(Ordering::SeqCst), 1);

            // Within 30 s: Busy, no vendor call.
            tokio::time::advance(StdDuration::from_secs(10)).await;
            let busy = runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(PhoneHomeOk::Refreshed { changed: false })
            }).await;
            assert_eq!(busy, SyncOutcome::Busy { retry_after_secs: 20 });
            assert_eq!(calls.load(Ordering::SeqCst), 1);

            // After the spacing: a new call; transport errors map to Offline.
            tokio::time::advance(StdDuration::from_secs(21)).await;
            let offline = runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(PhoneHomeError::Http("connection refused".into()))
            }).await;
            assert_eq!(offline, SyncOutcome::Offline { reason: "network" });
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro, "failure leaves the license");

            // A waiter behind a holder that records no outcome (the test
            // holds the flight itself) runs its own sync once the spacing
            // allows it.
            tokio::time::advance(SYNC_MIN_SPACING).await;
            let held = runtime.sync.flight.lock().await;
            let waiter = runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(PhoneHomeOk::Pending)
            });
            let unlock = async {
                for _ in 0..4 { tokio::task::yield_now().await; }
                drop(held);
            };
            let (pending, ()) = tokio::join!(waiter, unlock);
            assert_eq!(pending, SyncOutcome::PendingRebind);
            assert_eq!(calls.load(Ordering::SeqCst), 3);
        }).await;
    }

    #[tokio::test]
    async fn sync_now_without_a_license_makes_no_call() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let mut runtime = LicenseRuntime::bootstrap(home.path().into(), PublicKeyRegistry::new()).await;
            runtime.control_url = "http://127.0.0.1:9".into();
            let called_flag = std::sync::atomic::AtomicBool::new(false);
            let called = &called_flag;
            for _ in 0..2 {
                let outcome = runtime.sync_gated(move || async move {
                    called.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(PhoneHomeOk::Refreshed { changed: true })
                }).await;
                // Not Busy on the second call either: no vendor call was made.
                assert_eq!(outcome, SyncOutcome::NoLicense);
            }
            assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(runtime.sync_now().await, SyncOutcome::NoLicense);
        }).await;
    }

    // These tests own their runtime; the process-global RPC runtime is not
    // isolated per handler on this branch. RPCs serialize snapshot() after sync.
    #[tokio::test]
    async fn sync_now_new_invalid_file_updates_outcome_and_status_snapshot() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().to_path_buf(), PublicKeyRegistry::new()).await;
            assert!(!runtime.snapshot().await.installed);
            std::fs::write(home.path().join("license.json"), "{invalid json").unwrap();

            let outcome = runtime.sync_now().await;
            assert_eq!(outcome, SyncOutcome::UnreadableLicenseFile { reason: "unreadable" });
            let snapshot = runtime.snapshot().await;
            assert!(snapshot.installed);
            assert_eq!(snapshot.validation_warning, Some("unreadable"));
            assert_eq!(snapshot.tier, LicenseTier::OpenSource);
            let response = json!({ "sync": outcome.to_json(), "status": snapshot });
            assert_eq!(response["sync"]["outcome"], "unreadable_license_file");
            let status = serde_json::to_value(runtime.snapshot().await).unwrap();
            assert_eq!(response["status"], status);
            assert_eq!(status["installed"], true);
            assert_eq!(status["validation_warning"], "unreadable");
        }).await;
    }

    #[tokio::test]
    async fn sync_now_removed_invalid_file_clears_warning_and_status_snapshot() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            std::fs::write(&path, "{invalid json").unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().to_path_buf(), PublicKeyRegistry::new()).await;
            assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));
            // Populate the rejected memo as well as the startup warning.
            assert_eq!(runtime.sync_now().await, SyncOutcome::UnreadableLicenseFile { reason: "unreadable" });
            std::fs::remove_file(path).unwrap();

            let outcome = runtime.sync_now().await;
            assert_eq!(outcome, SyncOutcome::NoLicense);
            let snapshot = runtime.snapshot().await;
            assert!(!snapshot.installed);
            assert_eq!(snapshot.validation_warning, None);
            let response = json!({ "sync": outcome.to_json(), "status": snapshot });
            assert_eq!(response["sync"]["outcome"], "no_license");
            let status = serde_json::to_value(runtime.snapshot().await).unwrap();
            assert_eq!(response["status"], status);
            assert_eq!(status["installed"], false);
            assert!(status["validation_warning"].is_null());
        }).await;
    }

    #[tokio::test]
    async fn sync_reports_refusal_discovered_during_refresh_snapshot() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (runtime, _, _) = licensed_runtime(home.path()).await;
            let outcome = runtime.sync_gated(|| async {
                // Change after sync's first reconciliation, before the request
                // snapshot is captured. No transport is invoked by this test.
                std::fs::write(home.path().join("license.json"), "{invalid json").unwrap();
                runtime.refresh_snapshot().await?;
                panic!("an unusable licence must not reach transport");
            }).await;
            assert_eq!(outcome, SyncOutcome::UnreadableLicenseFile { reason: "unreadable" });
            let snapshot = runtime.snapshot().await;
            assert!(snapshot.installed);
            assert_eq!(snapshot.validation_warning, Some("unreadable"));
            assert_eq!(snapshot.tier, LicenseTier::OpenSource);
            assert_eq!(snapshot.max_agents, None);
        }).await;
    }

    #[tokio::test]
    async fn watcher_clears_startup_unreadable_warning_after_removal() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            std::fs::write(&path, "{invalid json").unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().to_path_buf(), PublicKeyRegistry::new()).await;
            assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));
            std::fs::remove_file(path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert!(!runtime.snapshot().await.installed);
            assert_eq!(runtime.snapshot().await.validation_warning, None);
            assert!(!runtime.reload_from_disk_if_changed().await);
        }).await;
    }

    #[tokio::test]
    async fn watcher_applies_appearance_refusal_recovery_and_removal_without_sleeping() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let runtime = LicenseRuntime::bootstrap(home.path().to_path_buf(), registry).await;
            assert!(!runtime.reload_from_disk_if_changed().await);

            std::fs::write(&path, "{invalid json").unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert!(runtime.snapshot().await.installed);
            assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));
            assert!(!runtime.reload_from_disk_if_changed().await);
            // Return to the original absent stamp must not be skipped.
            std::fs::remove_file(&path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert!(!runtime.snapshot().await.installed);
            assert_eq!(runtime.snapshot().await.validation_warning, None);

            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);
            std::fs::write(&path, "{broken again").unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            assert_eq!(runtime.snapshot().await.max_agents, None);
            assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));
            // Recovery uses full signature verification, never just parsing.
            let mut forged = license.clone();
            forged.max_agents = Some(99);
            storage::save_to(&forged, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.validation_warning, Some("invalid_signature"));
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            storage::save_to(&license, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);
            assert_eq!(runtime.snapshot().await.validation_warning, None);
            std::fs::remove_file(&path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert!(!runtime.snapshot().await.installed);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            assert_eq!(runtime.snapshot().await.validation_warning, None);
        }).await;
    }

    #[tokio::test]
    async fn sync_now_first_applies_a_license_the_cli_wrote() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            let mut cli = license.clone();
            cli.max_agents = Some(4);
            sign_license(&mut cli, &kp);
            storage::save_to(&cli, &home.path().join("license.json")).unwrap();
            let outcome = runtime.sync_gated(|| async { Ok(PhoneHomeOk::Refreshed { changed: false }) }).await;
            assert_eq!(outcome, SyncOutcome::Unchanged);
            assert_eq!(runtime.snapshot().await.max_agents, Some(4));
        }).await;
    }

    #[tokio::test]
    async fn reload_from_disk_picks_up_a_rewritten_license_and_ignores_an_identical_one() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;

            // Nothing changed / identical bytes rewritten: no reload.
            assert!(!runtime.reload_from_disk_if_changed().await);
            storage::save_to(&license, &path).unwrap();
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.state.read().await.revision, 0);

            // `duduclaw license refresh` wrote a license with more seats.
            let mut more = license.clone();
            more.max_agents = Some(7);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(7));
            assert_eq!(runtime.state.read().await.revision, 1);
            assert!(!runtime.reload_from_disk_if_changed().await, "unchanged stamp is not re-read");

            // Same validation as bootstrap: a tampered seat count is refused.
            let mut forged = more.clone();
            forged.max_agents = Some(99);
            storage::save_to(&forged, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            let snapshot = runtime.snapshot().await;
            assert_eq!(snapshot.tier, LicenseTier::OpenSource);
            assert_eq!(snapshot.validation_warning, Some("invalid_signature"));
            assert_eq!(snapshot.max_agents, None);

            storage::save_to(&more, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);

            // Malformed content drops all entitlements, just as on restart.
            std::fs::write(&path, b"{ half written").unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, None);
            assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));

            // Deactivation from the CLI applies too.
            std::fs::remove_file(&path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            let snapshot = runtime.snapshot().await;
            assert!(!snapshot.installed);
            assert_eq!(snapshot.tier, LicenseTier::OpenSource);
            assert!(!runtime.reload_from_disk_if_changed().await);
        }).await;
    }

    /// F3: one primitive for both caller kinds — back to back inside 30 s
    /// there is exactly one vendor call, in either order.
    #[tokio::test(start_paused = true)]
    async fn scheduled_and_on_demand_back_to_back_make_one_vendor_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (runtime, _, _) = licensed_runtime(home.path()).await;
            let calls_counter = AtomicUsize::new(0);
            let calls = &calls_counter;
            let vendor = |changed: bool| move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, PhoneHomeError>(PhoneHomeOk::Refreshed { changed })
            };

            // On demand, then scheduled 5 s later: the scheduled run takes
            // the just-finished refresh as its own.
            assert_eq!(runtime.sync_gated(vendor(false)).await, SyncOutcome::Unchanged);
            tokio::time::advance(StdDuration::from_secs(5)).await;
            assert_eq!(runtime.scheduled_phone_home(vendor(true)).await, SyncOutcome::Unchanged);
            assert_eq!(calls.load(Ordering::SeqCst), 1);

            // Scheduled (after the spacing), then on demand 5 s later: Busy.
            tokio::time::advance(SYNC_MIN_SPACING).await;
            assert_eq!(runtime.scheduled_phone_home(vendor(true)).await, SyncOutcome::Updated);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            tokio::time::advance(StdDuration::from_secs(5)).await;
            assert_eq!(runtime.sync_gated(vendor(false)).await, SyncOutcome::Busy { retry_after_secs: 25 });
            assert_eq!(calls.load(Ordering::SeqCst), 2);

            // A scheduled run waiting behind an in-flight on-demand sync
            // takes its result instead of calling the vendor again.
            tokio::time::advance(SYNC_MIN_SPACING).await;
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            let entered_flag = std::sync::atomic::AtomicBool::new(false);
            let entered = &entered_flag;
            let on_demand = runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.store(true, Ordering::SeqCst);
                let _ = release_rx.await;
                Ok(PhoneHomeOk::Pending)
            });
            let scheduled = async {
                while !entered.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
                runtime.scheduled_phone_home(vendor(true)).await
            };
            let release = async {
                while !entered.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
                for _ in 0..4 { tokio::task::yield_now().await; }
                let _ = release_tx.send(());
            };
            let (a, b, ()) = tokio::join!(on_demand, scheduled, release);
            assert_eq!(a, SyncOutcome::PendingRebind);
            assert_eq!(b, SyncOutcome::PendingRebind);
            assert_eq!(calls.load(Ordering::SeqCst), 3);
        }).await;
    }

    /// F2(d): a flight held past every bound makes a scheduled caller skip
    /// (no vendor call, licence untouched) instead of waiting forever.
    #[tokio::test(start_paused = true)]
    async fn scheduled_loop_skips_when_the_flight_is_held_too_long() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (runtime, _, _) = licensed_runtime(home.path()).await;
            let calls_counter = AtomicUsize::new(0);
            let calls = &calls_counter;
            let held = runtime.sync.flight.lock().await;
            let started = tokio::time::Instant::now();
            let outcome = runtime.scheduled_phone_home(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(PhoneHomeOk::Refreshed { changed: true })
            }).await;
            assert!(matches!(outcome, SyncOutcome::Busy { .. }), "{outcome:?}");
            assert!(started.elapsed() >= FLIGHT_WAIT && started.elapsed() < FLIGHT_WAIT + StdDuration::from_secs(1));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            // On-demand callers are bounded the same way.
            assert!(matches!(runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(PhoneHomeOk::Refreshed { changed: true })
            }).await, SyncOutcome::Busy { .. }));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            drop(held);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);
            // The bound covers what a holder may legitimately take.
            assert!(FLIGHT_WAIT >= PHONE_HOME_TIMEOUT * 2 + STORE_OP_DEADLINE * 2);
            assert!(STORE_OP_DEADLINE > LICENSE_LOCK_DEADLINE);
        }).await;
    }

    /// F2(b): a local process holding `license.json.lock` yields "busy"
    /// within the deadline; the in-memory licence is not touched.
    #[tokio::test]
    async fn held_license_lock_yields_busy_and_keeps_the_license() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            let mut more = license.clone();
            more.max_agents = Some(9);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &path).unwrap();
            let held = storage::LockedLicenseStore::acquire(&path).unwrap();
            let bound = STORE_OP_DEADLINE + StdDuration::from_secs(2);

            let started = std::time::Instant::now();
            let changed = tokio::time::timeout(bound, runtime.reload_from_disk_if_changed()).await
                .expect("the watcher returns within the lock deadline");
            assert!(!changed);
            assert!(started.elapsed() >= LICENSE_LOCK_DEADLINE, "{:?}", started.elapsed());

            let started = std::time::Instant::now();
            let snapshot = tokio::time::timeout(bound, runtime.refresh_snapshot()).await
                .expect("the phone-home snapshot returns within the lock deadline");
            assert!(matches!(snapshot, Err(PhoneHomeError::Busy)));
            assert!(started.elapsed() < bound);
            assert_eq!(sync_outcome(Err(PhoneHomeError::Busy)), SyncOutcome::Failed { reason: "license_file_busy" });

            let inner = runtime.state.read().await;
            assert_eq!(inner.revision, 0, "memory untouched");
            assert_eq!(inner.license.as_ref().unwrap().max_agents, license.max_agents);
            assert!(inner.validation_warning.is_none());
            drop(inner);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);

            // Not remembered as rejected: applied as soon as the lock is free.
            drop(held);
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(9));
        }).await;
    }

    /// F2(a)/(c): a FIFO at `license.json` returns promptly and is not
    /// re-read until it changes; the runtime drops to OpenSource.
    #[cfg(unix)]
    #[tokio::test]
    async fn fifo_at_the_license_path_returns_promptly_and_drops_entitlements() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            std::fs::remove_file(&path).unwrap();
            nix::unistd::mkfifo(path.as_path(), nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
            for attempt in 0..3 {
                let changed = tokio::time::timeout(StdDuration::from_secs(3), runtime.reload_from_disk_if_changed())
                    .await.expect("a FIFO licence path must not block the watcher");
                assert_eq!(changed, attempt == 0);
            }
            // Forced (sync) path too, without blocking or calling the vendor.
            let outcome = tokio::time::timeout(StdDuration::from_secs(3),
                runtime.sync_gated(|| async { Ok(PhoneHomeOk::Refreshed { changed: false }) })).await
                .expect("sync returns promptly");
            assert_eq!(outcome, SyncOutcome::UnreadableLicenseFile { reason: "unreadable" });
            assert!(rejected_stamp(&runtime).is_some());
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            assert_eq!(runtime.state.read().await.revision, 1);

            // Replaced by a real licence: the new stamp is read and applied.
            std::fs::remove_file(&path).unwrap();
            let mut more = license.clone();
            more.max_agents = Some(5);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(5));
            assert!(rejected_stamp(&runtime).is_none());
        }).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_license_is_applied_and_target_replacement_is_noticed() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let elsewhere = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            // Operator-managed links are followed on reload.
            let mut more = license.clone();
            more.max_agents = Some(50);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &elsewhere.path().join("license.json")).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(elsewhere.path().join("license.json"), &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(50));
            assert!(!runtime.reload_from_disk_if_changed().await);
            let first_stamp = DiskStamp::read(home.path()).unwrap();
            more.max_agents = Some(51);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &elsewhere.path().join("license.json")).unwrap();
            assert_ne!(DiskStamp::read(home.path()).unwrap(), first_stamp);
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(51));
            assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
            assert!(rejected_stamp(&runtime).is_none());
        }).await;
    }

    /// F2(c): an oversized file is refused, remembered by stamp and not read
    /// again until it changes.
    #[tokio::test]
    async fn oversized_license_is_refused_and_not_reread_until_changed() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            std::fs::write(&path, vec![b' '; storage::MAX_LICENSE_FILE_BYTES as usize + 1]).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            let remembered = rejected_stamp(&runtime);
            assert!(remembered.is_some());
            // Same stamp: skipped before any lock or read — even with the
            // lock held by someone else, the call returns at once.
            let held = storage::LockedLicenseStore::acquire(&path).unwrap();
            let started = std::time::Instant::now();
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert!(started.elapsed() < LICENSE_LOCK_DEADLINE, "re-read a refused file: {:?}", started.elapsed());
            drop(held);
            assert_eq!(rejected_stamp(&runtime), remembered);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            assert_eq!(runtime.state.read().await.revision, 1);

            // Changed: read again and applied.
            let mut more = license.clone();
            more.max_agents = Some(3);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &path).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(3));
        }).await;
    }

    /// F2(c): an oversized file is refused, remembered by stamp and not read
    /// again until it changes.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_oversized_license_is_not_reread_until_target_changes() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            let target = home.path().join("managed-license.json");
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            std::fs::write(&target, vec![b' '; storage::MAX_LICENSE_FILE_BYTES as usize + 1]).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            let remembered = rejected_stamp(&runtime);
            assert!(remembered.is_some());
            // Same stamp: skipped before any lock or read — even with the
            // lock held by someone else, the call returns at once.
            let held = storage::LockedLicenseStore::acquire(&path).unwrap();
            let started = std::time::Instant::now();
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert!(started.elapsed() < LICENSE_LOCK_DEADLINE, "re-read a refused file: {:?}", started.elapsed());
            drop(held);
            assert_eq!(rejected_stamp(&runtime), remembered);
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            assert_eq!(runtime.state.read().await.revision, 1);

            // Changed: read again and applied.
            let mut more = license.clone();
            more.max_agents = Some(3);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &target).unwrap();
            assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(3));
        }).await;
    }

    fn rejected_stamp(runtime: &LicenseRuntime) -> Option<DiskStamp> {
        runtime.sync.rejected.lock().unwrap().as_ref().map(|rejected| rejected.stamp.clone())
    }

    /// Wait (real time, bounded) until every permit of `ops` is free again.
    async fn wait_for_free_permits(ops: &StoreOps) {
        let until = std::time::Instant::now() + StdDuration::from_secs(5);
        while ops.permits.available_permits() < STORE_OP_PERMITS {
            assert!(std::time::Instant::now() < until, "store permits not released");
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    }

    /// Take every permit of `ops` with operations that only end when the
    /// returned senders are dropped or sent to (a hung mount).
    async fn hold_every_store_permit(ops: &StoreOps) -> Vec<std::sync::mpsc::Sender<()>> {
        let mut releases = Vec::new();
        for _ in 0..STORE_OP_PERMITS {
            let (release, wait) = std::sync::mpsc::channel::<()>();
            let held = run_store_op_within(ops, StdDuration::from_millis(1), move || {
                let _ = wait.recv();
                Ok(())
            }).await;
            assert!(matches!(held, Err(StoreReadError::Busy) | Ok(())));
            releases.push(release);
        }
        releases
    }

    /// M2: with both store permits held by operations that never finish,
    /// the watcher, `sync_now` and the CRL path answer Busy at once, submit
    /// no blocking work and leave the licence in memory untouched.
    #[tokio::test]
    async fn exhausted_store_permits_answer_busy_at_once_and_submit_nothing() {
        use std::sync::atomic::Ordering;
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (mut runtime, license, kp) = licensed_runtime(home.path()).await;
            let ops = Arc::new(StoreOps::new());
            runtime.store_ops = ops.clone();
            let releases = hold_every_store_permit(&ops).await;
            assert_eq!(ops.permits.available_permits(), 0);
            let submitted = ops.spawned.load(Ordering::SeqCst);
            assert_eq!(submitted, STORE_OP_PERMITS);

            // Written by the CLI meanwhile: not read while the store is busy.
            let mut more = license.clone();
            more.max_agents = Some(9);
            sign_license(&mut more, &kp);
            storage::save_to(&more, &path).unwrap();

            let bound = StdDuration::from_secs(2);
            for _ in 0..3 {
                let changed = tokio::time::timeout(bound, runtime.reload_from_disk_if_changed()).await
                    .expect("the watcher answers at once");
                assert!(!changed);
            }
            let outcome = tokio::time::timeout(bound, runtime.sync_now()).await.expect("sync answers at once");
            assert_eq!(outcome, SyncOutcome::Failed { reason: "license_file_busy" });
            let crl = tokio::time::timeout(bound, runtime.do_crl_fetch_once()).await.expect("CRL answers at once");
            assert!(matches!(crl, Err(PhoneHomeError::Busy)), "{crl:?}");
            assert_eq!(ops.spawned.load(Ordering::SeqCst), submitted, "no further blocking work submitted");
            assert_eq!(ops.permits.available_permits(), 0);
            {
                let inner = runtime.state.read().await;
                assert_eq!(inner.revision, 0, "memory untouched");
                assert_eq!(inner.license.as_ref().unwrap().max_agents, license.max_agents);
                assert!(inner.validation_warning.is_none());
            }

            // The permits come back only when the operations really end;
            // the next watcher cycle then applies the licence.
            for release in releases {
                release.send(()).unwrap();
            }
            wait_for_free_permits(&ops).await;
            assert!(runtime.reload_from_disk_if_changed().await);
            assert_eq!(runtime.snapshot().await.max_agents, Some(9));
        }).await;
    }

    /// M2: boot does not wait for a read that never completes; it starts as
    /// for an unreadable licence within the overall deadline.
    #[tokio::test]
    async fn boot_load_returns_within_its_deadline_when_the_read_never_completes() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
        let registry = PublicKeyRegistry::new();
        let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();
        let ops = StoreOps::new();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let deadline = StdDuration::from_millis(200);
        let started = std::time::Instant::now();
        let loaded = tokio::time::timeout(StdDuration::from_secs(5),
            load_and_validate_from(&registry, &gate, &ops, deadline, move || {
                let _ = wait.recv();
                Ok(None)
            })).await.expect("boot must not wait for a read that never completes");
        assert!(started.elapsed() >= deadline, "{:?}", started.elapsed());
        assert!(loaded.0.is_none());
        assert_eq!(loaded.1, Some("unreadable"));
        // The abandoned read keeps its permit until it really ends.
        assert_eq!(ops.permits.available_permits(), STORE_OP_PERMITS - 1);
        release.send(()).unwrap();
        wait_for_free_permits(&ops).await;
        // The production bound covers the boot lock wait with a margin.
        assert!(BOOT_LOAD_DEADLINE >= BOOT_LICENSE_LOCK_DEADLINE + StdDuration::from_secs(10));
    }

    /// L1: a malformed file rewritten in place with a valid licence of the
    /// same length, the modification time restored, is read again.
    #[cfg(unix)]
    #[tokio::test]
    async fn in_place_rewrite_restoring_length_and_mtime_is_reread() {
        use std::io::Write as _;
        use std::os::unix::fs::MetadataExt as _;
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (runtime, license, kp) = licensed_runtime(home.path()).await;
            let mut more = license.clone();
            more.max_agents = Some(6);
            sign_license(&mut more, &kp);
            let valid = serde_json::to_vec_pretty(&more).unwrap();
            let mut malformed = valid.clone();
            // Unterminated JSON of exactly the same length.
            assert_eq!(malformed.pop(), Some(b'}'));
            malformed.push(b' ');

            std::fs::write(&path, &malformed).unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            let refused = runtime.sync.rejected.lock().unwrap().clone().expect("refused");
            let malformed_digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&malformed).into();
            assert_eq!(refused.digest, Some(malformed_digest));
            let before = std::fs::metadata(&path).unwrap();

            // Past the coarse kernel clock tick, so the status-change time moves.
            std::thread::sleep(StdDuration::from_millis(50));
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all(&valid).unwrap();
            file.set_modified(before.modified().unwrap()).unwrap();
            drop(file);
            let after = std::fs::metadata(&path).unwrap();
            assert_eq!((after.dev(), after.ino(), after.len()), (before.dev(), before.ino(), before.len()));
            assert_eq!(after.modified().unwrap(), before.modified().unwrap());

            assert!(runtime.reload_from_disk_if_changed().await, "the rewritten licence must be read again");
            assert_eq!(runtime.snapshot().await.max_agents, Some(6));
            assert!(runtime.sync.rejected.lock().unwrap().is_none());
        }).await;
    }

    /// L1: refused content with an unchanged stamp is not read again until
    /// the retry period passes; a forced (admin) sync always reads it once.
    #[tokio::test(start_paused = true)]
    async fn refused_content_is_retried_after_the_period_and_by_a_forced_sync() {
        use std::sync::atomic::Ordering;
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (mut runtime, _, _) = licensed_runtime(home.path()).await;
            let ops = Arc::new(StoreOps::new());
            runtime.store_ops = ops.clone();
            let submitted = || ops.spawned.load(Ordering::SeqCst);

            std::fs::write(&path, b"{ not a licence").unwrap();
            assert!(runtime.reload_from_disk_if_changed().await);
            let refused = runtime.sync.rejected.lock().unwrap().clone().expect("refused");
            assert!(refused.digest.is_some());

            // Identical bytes, unchanged stamp: only the stamp is read.
            let before = submitted();
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert_eq!(submitted(), before + 1, "a refused file was read again");

            // After the retry period: read once more (stamp + locked read),
            // still refused, and skipped again afterwards.
            tokio::time::advance(REJECTED_RETRY).await;
            let before = submitted();
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert_eq!(submitted(), before + 2, "an expired refusal must be read again");
            let again = runtime.sync.rejected.lock().unwrap().clone().expect("still refused");
            assert_eq!(again.digest, refused.digest);
            assert!(again.at > refused.at);
            let before = submitted();
            assert!(!runtime.reload_from_disk_if_changed().await);
            assert_eq!(submitted(), before + 1);

            // A forced sync ignores the refusal and reads once.
            let before = submitted();
            let outcome = runtime.sync_gated(|| async { Ok(PhoneHomeOk::Refreshed { changed: false }) }).await;
            assert_eq!(outcome, SyncOutcome::UnreadableLicenseFile { reason: "unreadable" });
            assert_eq!(submitted(), before + 2, "a forced sync must read the store");
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            assert_eq!(runtime.state.read().await.revision, 1);
        }).await;
    }

    /// L2: an offline on-demand sync just before a due scheduled run is not
    /// adopted: one vendor call in the window, next attempt in about an hour.
    /// An adopted verified result keeps the normal cadence.
    #[tokio::test(start_paused = true)]
    async fn scheduled_cycle_after_an_offline_sync_retries_in_an_hour_not_a_day() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (runtime, _, _) = licensed_runtime(home.path()).await;
            let calls_counter = AtomicUsize::new(0);
            let calls = &calls_counter;
            let refreshed = move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, PhoneHomeError>(PhoneHomeOk::Refreshed { changed: false })
            };

            let offline = runtime.sync_gated(move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(PhoneHomeError::Http("connection refused".into()))
            }).await;
            assert_eq!(offline, SyncOutcome::Offline { reason: "network" });
            tokio::time::advance(StdDuration::from_secs(5)).await;
            let next = runtime.scheduled_cycle(refreshed).await;
            assert_eq!(calls.load(Ordering::SeqCst), 1, "no second vendor call inside the window");
            let jitter = crate::license_rev::PROBE_JITTER;
            assert!(next >= SCHEDULED_RETRY - jitter && next <= SCHEDULED_RETRY + jitter, "{next:?}");

            // A verified result just before is adopted: normal cadence.
            tokio::time::advance(SYNC_MIN_SPACING).await;
            assert_eq!(runtime.sync_gated(refreshed).await, SyncOutcome::Unchanged);
            tokio::time::advance(StdDuration::from_secs(5)).await;
            let next = runtime.scheduled_cycle(refreshed).await;
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(next, runtime.cadence_sleep().await);
            assert!(next >= StdDuration::from_secs(24 * 60 * 60), "{next:?}");
        }).await;
    }

    /// `BOOT_EDITION` is written in exactly one place — gateway bootstrap
    /// (`bootstrap_with_edition`) — and only once per process.
    #[test]
    fn boot_edition_has_a_single_writer_and_is_set_once() {
        use duduclaw_core::EditionProfile;
        // Accessor semantics, on a private cell of the same type.
        let cell = std::sync::OnceLock::new();
        assert!(record_boot_edition(&cell, EditionProfile::Personal));
        assert!(!record_boot_edition(&cell, EditionProfile::Enterprise), "a second write is ignored");
        assert_eq!(cell.get().copied(), Some(EditionProfile::Personal));
        assert_eq!(boot_edition_profile(), BOOT_EDITION.get().copied());

        // Source level: no other writer anywhere in the gateway or CLI.
        let name = ["BOOT_", "EDITION"].concat();
        let own = include_str!("license_runtime.rs").replace("\r\n", "\n");
        let production = own.split("#[cfg(test)]\npub(crate) mod tests").next().unwrap();
        assert_eq!(production.matches(&format!("record_boot_edition(&{name}")).count(), 1);
        assert_eq!(production.matches(&format!("{name}.set(")).count(), 0, "writes go through record_boot_edition");
        assert_eq!(production.matches(&format!("{name}.get_or_init")).count(), 0);
        let writer = production.find(&format!("record_boot_edition(&{name}")).unwrap();
        let bootstrap = production.find("pub async fn bootstrap_with_edition(").unwrap();
        let next_fn = production[bootstrap + 1..].find("\n    async fn ").map(|i| i + bootstrap + 1).unwrap();
        assert!(writer > bootstrap && writer < next_fn, "the only writer is inside bootstrap_with_edition");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for root in [manifest.join("src"), manifest.join("../duduclaw-cli/src")] {
            let mut stack = vec![root];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("license_runtime.rs") {
                        let text = std::fs::read_to_string(&path).unwrap();
                        assert!(!text.contains(&name), "{} touches the boot edition directly", path.display());
                    }
                }
            }
        }
    }

    #[test]
    fn last_seen_crl_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_last_seen_crl(dir.path()).is_none(), "no file → None");

        let ts = Utc::now();
        write_last_seen_crl(dir.path(), ts).unwrap();
        let read = read_last_seen_crl(dir.path()).expect("persisted timestamp");
        // RFC3339 round-trip preserves to (at least) second precision.
        assert_eq!(read.timestamp(), ts.timestamp());
    }
}
