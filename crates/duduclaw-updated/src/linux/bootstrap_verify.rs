//! R1 bootstrap verifier (plan §4.1). Verifies the migration bundle in the fixed
//! root-only inbox WITHOUT publishing any isolation fact, stages the verified
//! generation in the root-only bootstrap area (not a fact), then execs the staged
//! gateway from a sealed memfd. The CLI accepts no path: every location below is
//! a constant. Every failure is final: no retry, no downgrade, no path fallback.
use std::{ffi::CString, fs::File, io::{Read, Seek, SeekFrom, Write}, mem::MaybeUninit,
    os::{fd::{AsRawFd, FromRawFd, RawFd}, unix::fs::{FileExt, PermissionsExt}}, path::{Path, PathBuf},
    time::{Duration, Instant}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use crate::{Error, Result, bundle::{self, BundleEntries, Limits}, metadata::MAX_METADATA_BYTES,
    publication::manifest_digest, state::State, trust_store::TrustLedger};
use super::files::{self, Directory, copy_artifact, verify_fd};

/// Customer download directory (design §8 step 2), root 0700.
pub const MIGRATE_INBOX: &str = "/root/duduclaw-migrate";
/// Root 0700 single-binary protected state directory; NOT an isolation fact.
pub const PROTECTED_ROOT: &str = "/var/lib/duduclaw-protected";
/// `<PROTECTED_ROOT>/bootstrap/<manifest-hex>/` holds one verified generation.
pub const STAGE_DIRECTORY: &str = "bootstrap";
/// R3's re-entrant journal (plan §4.3); read here only for the resume check.
pub const BOOTSTRAP_JOURNAL: &str = "isolation-bootstrap.json";
pub const STAGE_RECEIPT: &str = "verified.json";
/// Root-owned version floor of the single-binary protected install (plan §3.1,
/// design §10.0 item 2; written by duduclaw-cli `protected_linux_install::record_floor`).
pub const VERSION_FLOOR: &str = "version-floor";
/// Version reserved by an interrupted single-binary upgrade (same writer and rules).
pub const VERSION_PENDING: &str = "version-pending";
/// `<PROTECTED_ROOT>/bootstrap-high-water.json` (root 0600, NOT an isolation fact):
/// the newest bundle this host's verifier accepted, persisted before anything runs.
pub const BOOTSTRAP_HIGH_WATER: &str = "bootstrap-high-water.json";
const MAX_HIGH_WATER_BYTES: u64 = 4096;
/// `<PROTECTED_ROOT>/version-lock`: duduclaw-cli `protected_linux_install::version_lock`
/// holds it (exclusive flock) while it writes version-floor / version-pending.
pub const VERSION_LOCK: &str = "version-lock";
/// `files::Directory::lock`'s file, in the state root and in the bootstrap stage directory.
const UPDATE_LOCK: &str = "update.lock";
/// Bounded wait for all acceptance locks together (see [`AcceptanceLocks`]).
const LOCK_WAIT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(50);
/// Updater state and trust ledger below `linux::STATE_ROOT` (plan §3.1).
const UPDATER_STATE: &str = "state.json";
const UPDATER_LEDGER: &str = "trust-ledger.json";
const STAGE_ARTIFACTS: &str = "artifacts";
const MAX_SIDE_FILE_BYTES: u64 = 64 * 1024;
const MAX_RECEIPT_BYTES: usize = 1024 * 1024;
const MAX_JOURNAL_BYTES: usize = 64 * 1024;
/// Same bound as duduclaw-cli `read_version_file`.
const MAX_VERSION_FILE_BYTES: u64 = 128;
/// Linux 6.3+ `MFD_EXEC`; older kernels reject the unknown flag with EINVAL.
const MFD_EXEC: libc::c_uint = 0x0010;
const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;

/// Which root key set the compiled pin belongs to (plan §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinKind { Production, Test }
impl PinKind {
    pub fn label(self) -> &'static str { match self { Self::Production => "PRODUCTION", Self::Test => "TEST" } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapMode { Install, Upgrade, Rollback }
impl BootstrapMode {
    pub fn as_str(self) -> &'static str {
        match self { Self::Install => "install", Self::Upgrade => "upgrade", Self::Rollback => "rollback" }
    }
}

/// `verified.json` (plan §4.1), root 0600. `mode` and `verified_at_unix` record the
/// FIRST verification of this generation; a later run re-verifies everything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageReceipt {
    pub schema_version: u16,
    pub mode: BootstrapMode,
    pub manifest_digest: Sha256Digest,
    pub root_sha256: Sha256Digest,
    pub bundle_sha256: Sha256Digest,
    pub bundle_size: u64,
    pub authorization_sha256: Sha256Digest,
    pub verifier_pin_kind: PinKind,
    pub verified_at_unix: u64,
    /// Exactly the signed manifest's artifacts: id, sha256, size_bytes, mode, install_target.
    pub artifacts: Vec<ReleaseArtifact>,
}

/// `bootstrap-high-water.json`: written atomically (temp + fsync + rename +
/// directory fsync) after a bundle is accepted and before any of its bytes run.
/// It only moves forward ([`raised_high_water`]); a failed or aborted run never lowers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HighWater {
    pub schema_version: u16,
    pub version: ReleaseVersion,
    pub serial: u64,
    /// Generation (signed manifest) digest.
    pub digest: Sha256Digest,
    /// Digest of the root-authorised bundle (`BundleAuthorization::bundle_sha256`).
    pub bundle_sha256: Sha256Digest,
}

/// The eleven steps of `verify_and_stage` + exec (plan §4.1), for refusal reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    Harden = 1, Preconditions = 2, OpenInbox = 3, Seal = 4, LegacySignature = 5, Root = 6,
    Authorization = 7, Manifest = 8, Stage = 9, Announce = 10, Exec = 11,
}
impl Step {
    pub fn number(self) -> u8 { self as u8 }
    /// Fixed zh-TW guidance; never a path, file content or OS error text.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Harden => "需要以 root 執行，而且這個驗證程式必須是內建信任根指紋的發行建置。",
            Self::Preconditions => "這台主機已有隔離安裝的紀錄，且不屬於這個安裝包或這個模式時，請先執行 duduclaw-bootstrap rollback；rollback 只能用於日誌中尚未提交、且與這個安裝包相同的世代。",
            Self::OpenInbox => "遷移暫存目錄或其中的檔案不符合要求：必須由 root 擁有、目錄權限 0700、檔案不可被他人寫入、不可為連結，且檔名須與這個驗證程式的版本一致。",
            Self::Seal => "無法把安裝包複製進密封的記憶體檔案。",
            Self::LegacySignature => "安裝包的發行金鑰（minisign）簽章驗證未通過。",
            Self::Root => "信任根與內建指紋不符，或信任根的門檻簽章未通過。",
            Self::Authorization => "遷移安裝包授權（信任根門檻簽章）未通過、已過期，或與安裝包、版本、架構不符。",
            Self::Manifest => "發行清單簽章未通過、其版本或元件與安裝包授權不符，或其版本／序號不新於這台主機已記錄的版本下限或已接受的發行（不能用來降版；同版本只接受先前已記錄的同一個安裝包）；升級模式另需這台主機的版本下限紀錄。",
            Self::Stage => "暫存區寫入或重新驗證失敗；既有的暫存內容與這次的驗證結果不同時不會覆寫。",
            Self::Announce => "無法輸出驗證結果。",
            Self::Exec => "無法執行已驗證的 gateway。",
        }
    }
}

#[derive(Debug)]
pub struct Refusal { pub step: Step, pub error: Error }

#[cfg(duduclaw_test_root)]
const BUILD_PIN_KIND: PinKind = PinKind::Test;
#[cfg(not(duduclaw_test_root))]
const BUILD_PIN_KIND: PinKind = PinKind::Production;

/// The ONLY source of the bootstrap root pin: a compile-time constant of this
/// independently hash-checked binary, never a flag, file or environment at runtime.
/// The kind is fixed by `cfg(duduclaw_test_root)`, so one build cannot report both.
pub fn pinned_root() -> Result<(Sha256Digest, PinKind)> {
    Ok((parse_pin(option_env!("DUDUCLAW_BOOTSTRAP_ROOT_SHA256"))?, BUILD_PIN_KIND))
}
fn parse_pin(raw: Option<&str>) -> Result<Sha256Digest> {
    let text = raw.ok_or(Error::Maintenance)?;
    parse_digest(text).map_err(|_| Error::Maintenance)
}
/// Exactly 64 lowercase hex digits (sha256sum output).
fn parse_digest(text: &str) -> Result<Sha256Digest> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(Error::Invalid);
    }
    let bytes: [u8; 32] = hex::decode(text).map_err(|_| Error::Invalid)?.try_into().map_err(|_| Error::Invalid)?;
    Ok(Sha256Digest(bytes))
}

/// Legacy release minisign key. A production build uses the release key only; a
/// test-key build uses a compiled test key and refuses the production key, so
/// neither build accepts the other's bundles (the root pins differ as well).
fn legacy_release_key() -> Result<&'static str> {
    #[cfg(not(duduclaw_test_root))]
    { Ok(LEGACY_RELEASE_MINISIGN_KEY) }
    #[cfg(duduclaw_test_root)]
    {
        let key = option_env!("DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY").ok_or(Error::Maintenance)?;
        if key.trim() == LEGACY_RELEASE_MINISIGN_KEY { return Err(Error::Maintenance); }
        Ok(key)
    }
}

fn arch_text(arch: Architecture) -> &'static str {
    match arch { Architecture::X86_64 => "x86_64", Architecture::Aarch64 => "aarch64" }
}

/// Every value a verification decision depends on. Production values come only
/// from compiled constants ([`Context::production`]); tests inject a test key set.
pub(crate) struct Context {
    pub pin: Sha256Digest,
    pub pin_kind: PinKind,
    pub release_key: String,
    pub arch: Architecture,
    /// The verifier's own release; the bundle file name and authorization must match it.
    pub version: ReleaseVersion,
    pub now: u64,
}
impl Context {
    pub(crate) fn production() -> Result<Self> {
        let (pin, pin_kind) = pinned_root()?;
        Ok(Self { pin, pin_kind, release_key: legacy_release_key()?.to_owned(),
            arch: super::bootstrap::host_architecture()?,
            version: crate::parse_version(env!("CARGO_PKG_VERSION"))?, now: super::now()? })
    }
    /// Fixed bundle file name (plan §3.2) for this verifier's version and the host arch.
    pub(crate) fn tar_name(&self) -> String {
        format!("duduclaw-isolated-linux-{}-{}.tar", arch_text(self.arch), crate::version_text(self.version))
    }
    /// The comment the release pipeline signs (`minisign -t "duduclaw $TAG $f"`).
    pub(crate) fn trusted_comment(&self) -> String {
        format!("duduclaw v{} {}", crate::version_text(self.version), self.tar_name())
    }
}

pub(crate) struct Paths {
    /// Inbox relative to "/", resolved with openat2 from a "/" dirfd.
    pub inbox: String,
    pub protected: PathBuf,
    /// Root under which `ISOLATION_FACTS` are probed ("/" in production).
    pub facts_root: PathBuf,
    /// Updater state directory holding state.json and trust-ledger.json (`linux::STATE_ROOT`).
    pub state_root: PathBuf,
}
impl Paths {
    fn production() -> Self {
        Self { inbox: MIGRATE_INBOX.trim_start_matches('/').to_owned(), protected: PathBuf::from(PROTECTED_ROOT),
            facts_root: PathBuf::from("/"), state_root: PathBuf::from(super::STATE_ROOT) }
    }
}

pub(crate) struct Config {
    pub context: Context,
    pub paths: Paths,
    /// Test hook run right after the bundle is sealed (TOCTOU tests).
    #[cfg(test)]
    pub after_seal: Option<fn(&Config)>,
}

/// The three small inbox files, read once through their verified fds.
#[derive(Clone)]
pub(crate) struct SideFiles {
    pub minisig: String,
    pub authorization: Vec<u8>,
    pub authorization_signatures: Vec<u8>,
}

pub(crate) struct Verified {
    pub bundle_sha256: Sha256Digest,
    pub bundle_size: u64,
    pub authorization_sha256: Sha256Digest,
    pub entries: BundleEntries,
    pub root_bytes: Vec<u8>,
    pub root_signature_bytes: Vec<u8>,
    pub manifest_bytes: Vec<u8>,
    pub manifest_signature_bytes: Vec<u8>,
    pub manifest: ReleaseManifest,
}

pub(crate) struct Staged {
    pub receipt: StageReceipt,
    pub version: ReleaseVersion,
    pub gateway: ReleaseArtifact,
}

/// Files of a re-verified staging directory.
pub(crate) struct StagedFiles {
    pub artifacts: Directory,
    pub root_bytes: Vec<u8>,
    pub root_signatures: Vec<String>,
    pub manifest_bytes: Vec<u8>,
    pub manifest_signature: String,
    pub manifest: ReleaseManifest,
}

fn missing(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}
fn cstring(value: &str) -> Result<CString> { CString::new(value).map_err(|_| Error::Invalid) }
fn signature_list(bytes: &[u8]) -> Result<Vec<String>> { serde_json::from_slice(bytes).map_err(|_| Error::Signature) }

// ---------------------------------------------------------------- public entry points

/// Steps 1–9: verify everything and write the staging area; returns its receipt.
pub fn verify_and_stage(mode: BootstrapMode) -> Result<StageReceipt> {
    let config = harden_and_configure()?;
    let mut step = Step::Harden;
    Ok(verify_and_stage_with(&config, mode, &mut step)?.receipt)
}

/// Steps 1–11. Returns only on refusal; success replaces this process with the
/// staged gateway (`service bootstrap-continue`, implemented by R3).
pub fn run(mode: BootstrapMode) -> std::result::Result<std::convert::Infallible, Refusal> {
    let config = harden_and_configure().map_err(|error| Refusal { step: Step::Harden, error })?;
    let mut step = Step::Harden;
    let staged = verify_and_stage_with(&config, mode, &mut step).map_err(move |error| Refusal { step, error })?;
    announce(&config.context, &staged).map_err(|error| Refusal { step: Step::Announce, error })?;
    let error = match exec_gateway(&config.paths, &staged, mode) { Ok(never) => match never {}, Err(error) => error };
    Err(Refusal { step: Step::Exec, error })
}

/// R3 S0: re-verify a staged generation (root pin, signatures, every artifact hash
/// and mode, exact file set) and return its receipt.
pub fn load_stage(generation: Sha256Digest) -> Result<StageReceipt> {
    let context = Context::production()?;
    Ok(open_stage_at(Path::new(PROTECTED_ROOT), generation, &context)?.0)
}

// ---------------------------------------------------------------- state machine

/// Step 1: dumpable 0, NNP, umask 077; real AND effective uid 0; compiled pin present.
fn harden_and_configure() -> Result<Config> {
    super::harden()?;
    super::require_root()?;
    Ok(Config { context: Context::production()?, paths: Paths::production(),
        #[cfg(test)]
        after_seal: None })
}

pub(crate) fn verify_and_stage_with(config: &Config, mode: BootstrapMode, step: &mut Step) -> Result<Staged> {
    *step = Step::Preconditions;
    let resume = preconditions(&config.paths, mode)?;
    *step = Step::OpenInbox;
    let Inbox { tar, tar_size, side } = open_inbox(&config.paths, &config.context)?;
    *step = Step::Seal;
    let image = SealedImage::copy_from(&tar, tar_size)?;
    drop(tar); // From here on only the sealed memfd is used, never the path.
    #[cfg(test)]
    let _hooked = config.after_seal.map(|hook| hook(config));
    let verified = verify_bundle(image.bytes(), &side, &config.context, step)?;
    // Deferred half of step 2: the generation is known only after step 8.
    let generation = manifest_digest(&verified.manifest_bytes);
    if resume.is_some_and(|expected| expected != generation) {
        *step = Step::Preconditions;
        return Err(Error::Maintenance);
    }
    // Anti-rollback (part of accepting the manifest), per mode: see check_history.
    // Refused here before anything is created; `stage` re-checks under the acceptance
    // locks ([`AcceptanceLocks`]) and durably records the high-water mark before
    // staging (and so before step 11).
    *step = Step::Manifest;
    check_history(&read_history(&config.paths)?, mode, resume, &verified.manifest,
        &high_water_of(&verified, generation), config.context.now)?;
    *step = Step::Stage;
    let receipt = stage(&config.paths, &config.context, mode, resume, image.bytes(), &verified)?;
    let gateway = verified.manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::Gateway)
        .cloned().ok_or(Error::Signature)?;
    Ok(Staged { receipt, version: verified.manifest.version, gateway })
}

// Step 2 ------------------------------------------------------------------------

/// Only NotFound proves absence (same rule as duduclaw-core `isolation_install`).
fn present(path: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}
pub(crate) fn facts_present(root: &Path) -> Vec<&'static str> {
    ISOLATION_FACTS.iter().copied().filter(|fact| present(&root.join(fact.trim_start_matches('/')))).collect()
}

pub(crate) struct Journal { pub mode: BootstrapMode, pub generation: Sha256Digest, pub committed: bool }

/// Minimal strict read of R3's journal: the four fields this decision needs, each
/// with an exact type. R3 owns the full `deny_unknown_fields` schema. Malformed = refuse.
fn read_journal(protected: &Path) -> Result<Option<Journal>> {
    let dir = match Directory::open(protected) {
        Ok(dir) => dir,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if mode_of(&dir)? != 0o700 { return Err(Error::Maintenance); }
    let bytes = match dir.bytes(BOOTSTRAP_JOURNAL, MAX_JOURNAL_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Error::Maintenance)?;
    let object = value.as_object().ok_or(Error::Maintenance)?;
    if object.get("schema_version").and_then(serde_json::Value::as_u64) != Some(1) { return Err(Error::Maintenance); }
    let mode = match object.get("mode").and_then(serde_json::Value::as_str) {
        Some("install") => BootstrapMode::Install,
        Some("upgrade") => BootstrapMode::Upgrade,
        _ => return Err(Error::Maintenance),
    };
    let generation = parse_digest(object.get("generation").and_then(serde_json::Value::as_str).ok_or(Error::Maintenance)?)
        .map_err(|_| Error::Maintenance)?;
    let committed = object.get("committed").and_then(serde_json::Value::as_bool).ok_or(Error::Maintenance)?;
    Ok(Some(Journal { mode, generation, committed }))
}

/// No fact may exist unless R3's journal records an uncommitted run (of the same
/// mode, except for rollback); its generation must equal the one verified later.
fn preconditions(paths: &Paths, mode: BootstrapMode) -> Result<Option<Sha256Digest>> {
    let facts = !facts_present(&paths.facts_root).is_empty();
    precondition_rule(read_journal(&paths.protected)?.as_ref(), facts, mode)
}

/// Pure step-2 rule. Rollback is authority only to undo the in-progress run: it
/// needs an uncommitted journal and is bound to that journal's exact generation
/// (checked after step 8), and it never stages anything new (see `stage`). No
/// emergency authorization exists for the bootstrap path (plan §4.1/§4.3), so an
/// ordinary migration authorization can never move a host to another generation.
pub(crate) fn precondition_rule(journal: Option<&Journal>, facts: bool, mode: BootstrapMode) -> Result<Option<Sha256Digest>> {
    match journal {
        Some(journal) if journal.committed => Err(Error::Maintenance),
        Some(journal) if mode != BootstrapMode::Rollback && journal.mode != mode => Err(Error::Maintenance),
        Some(journal) => Ok(Some(journal.generation)),
        None if mode == BootstrapMode::Rollback => Err(Error::Maintenance),
        None if facts => Err(Error::Maintenance),
        None => Ok(None),
    }
}

// Anti-rollback (deferred part of step 8) ----------------------------------------

/// Root-owned history that bounds which release may be staged. `None` = absent.
pub(crate) struct History {
    pub floor: Option<ReleaseVersion>,
    pub pending: Option<ReleaseVersion>,
    pub high_water: Option<HighWater>,
    pub state: Option<State>,
    pub ledger: Option<TrustLedger>,
}

fn optional<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The floor file format written by duduclaw-cli `record_floor`/`prepare_version`:
/// one stable `X.Y.Z`, no prefix, no whitespace or newline. Anything else refuses.
pub(crate) fn parse_version_file(bytes: &[u8]) -> Result<ReleaseVersion> {
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit() || *b == b'.') { return Err(Error::Maintenance); }
    crate::parse_version(std::str::from_utf8(bytes).map_err(|_| Error::Maintenance)?).map_err(|_| Error::Maintenance)
}

/// Same rules as duduclaw-cli `protected_linux_install::read_version_file` (regular
/// file, no symlink, root:root, exactly 0600, one link, at most 128 bytes), read
/// through the verified directory fd instead of a pathname.
fn read_version_file(dir: &Directory, name: &str) -> Result<Option<ReleaseVersion>> {
    let Some(bytes) = read_private_file(dir, name, MAX_VERSION_FILE_BYTES)? else { return Ok(None) };
    parse_version_file(&bytes).map(Some)
}
/// Regular root-owned file, exactly 0600, one link, at most `limit` bytes, read
/// through the verified directory fd. NotFound = `None`; anything else refuses.
fn read_private_file(dir: &Directory, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let Some(file) = optional(dir.read(name))? else { return Ok(None) };
    let metadata = file.metadata()?;
    if metadata.permissions().mode() & 0o7777 != 0o600 || metadata.len() > limit {
        return Err(Error::Maintenance);
    }
    let mut bytes = Vec::new();
    Read::take(&file, limit + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| Error::Invalid)? != metadata.len() { return Err(Error::Maintenance); }
    Ok(Some(bytes))
}
/// Strict: size-bounded, `deny_unknown_fields`, schema 1. Corrupt = refuse, never absent.
pub(crate) fn parse_high_water(bytes: &[u8]) -> Result<HighWater> {
    let record: HighWater = serde_json::from_slice(bytes).map_err(|_| Error::Maintenance)?;
    if record.schema_version != 1 { return Err(Error::Maintenance); }
    Ok(record)
}
fn read_high_water(dir: &Directory) -> Result<Option<HighWater>> {
    read_private_file(dir, BOOTSTRAP_HIGH_WATER, MAX_HIGH_WATER_BYTES)?.map(|bytes| parse_high_water(&bytes)).transpose()
}

/// Read the version floor/pending files and the bootstrap high-water record
/// (protected root, 0700) and the updater's state.json/trust-ledger.json (state
/// root, 0700) through the existing dirfd readers (`files::Directory`, the same
/// `json` reader the launcher and `trust_admin::ledger` use). Only NotFound means
/// absent; unreadable or corrupt content refuses.
pub(crate) fn read_history(paths: &Paths) -> Result<History> {
    let (floor, pending, high_water) = match optional(Directory::open(&paths.protected))? {
        None => (None, None, None),
        Some(dir) => {
            if mode_of(&dir)? != 0o700 { return Err(Error::Maintenance); }
            (read_version_file(&dir, VERSION_FLOOR)?, read_version_file(&dir, VERSION_PENDING)?, read_high_water(&dir)?)
        }
    };
    let (state, ledger) = match optional(Directory::open(&paths.state_root))? {
        None => (None, None),
        Some(dir) => {
            if mode_of(&dir)? != 0o700 { return Err(Error::Maintenance); }
            (optional(dir.json::<State>(UPDATER_STATE))?, optional(dir.json::<TrustLedger>(UPDATER_LEDGER))?)
        }
    };
    Ok(History { floor, pending, high_water, state, ledger })
}

/// The high-water record this bundle would set.
pub(crate) fn high_water_of(verified: &Verified, generation: Sha256Digest) -> HighWater {
    HighWater { schema_version: 1, version: verified.manifest.version, serial: verified.manifest.serial,
        digest: generation, bundle_sha256: verified.bundle_sha256 }
}

/// Pure anti-rollback rule. `candidate` is this bundle's record ([`high_water_of`]);
/// `resume` is the uncommitted R3 journal's generation, already proven equal to
/// `candidate.digest` (step 2, deferred half).
/// - Every recorded version (version-floor, version-pending, the high-water record,
///   the updater state's committed floor): a lower version refuses, and an EQUAL
///   version is accepted only as the exact bundle already recorded for it — the
///   identical high-water record, the generation the updater state commits, or the
///   journal's generation (resume of the same bundle).
/// - High-water record: any other bundle needs a strictly higher version AND serial.
/// - Updater state: `State::check_manifest` (version above the committed floor,
///   serial above the committed one, never below the highest accepted serial)
///   unless it already commits this exact generation; a trust ledger must validate
///   and cannot exist without its state (`trust_admin::ledger`).
/// - Mode `Upgrade` migrates a single-binary protected host, identified by its
///   version floor (design §10.0 item 2): a missing floor refuses. On that host the
///   updater state is absent by definition (§10.0 item 3), so its absence is the
///   first-migration case; acceptance there rests on the root-threshold
///   BundleAuthorization of exactly this bundle digest (step 7) and is recorded as
///   the high-water mark before anything runs. Unreadable or corrupt history never
///   counts as absent (`read_history`).
/// - Mode `Install` (fresh host) has no history to compare on first contact; the
///   same authorization binding and high-water record apply.
/// - Mode `Rollback` is bound to the journal's generation and records nothing.
pub(crate) fn check_history(history: &History, mode: BootstrapMode, resume: Option<Sha256Digest>,
    manifest: &ReleaseManifest, candidate: &HighWater, now: u64) -> Result<()> {
    let digest = candidate.digest;
    if candidate.schema_version != 1 || candidate.version != manifest.version || candidate.serial != manifest.serial {
        return Err(Error::Invalid);
    }
    let state_commits = history.state.as_ref().is_some_and(|state| state.committed_digest == digest
        && state.serial == manifest.serial && state.floor == manifest.version && state.product == manifest.product
        && state.arch == manifest.arch);
    let recorded = history.high_water.as_ref() == Some(candidate) || resume == Some(digest) || state_commits;
    if let Some(high) = &history.high_water {
        if high.schema_version != 1 { return Err(Error::Maintenance); }
        if raised_high_water(Some(high), candidate).is_err() { return Err(Error::Rollback); }
    }
    for bound in [history.floor, history.pending].into_iter().flatten() {
        if manifest.version < bound || (manifest.version == bound && !recorded) { return Err(Error::Rollback); }
    }
    if let Some(ledger) = &history.ledger {
        ledger.current(now)?;
        if history.state.is_none() { return Err(Error::Maintenance); }
    }
    if let Some(state) = &history.state {
        state.validate()?;
        if !state_commits { state.check_manifest(manifest, digest, now)?; }
    }
    if mode == BootstrapMode::Upgrade && history.floor.is_none() { return Err(Error::Maintenance); }
    Ok(())
}

/// The high-water record only moves forward: the identical record needs no write
/// (`None`); any other must be strictly newer in version AND serial, else refused.
pub(crate) fn raised_high_water(recorded: Option<&HighWater>, candidate: &HighWater) -> Result<Option<HighWater>> {
    match recorded {
        Some(old) if old == candidate => Ok(None),
        Some(old) if candidate.version <= old.version || candidate.serial <= old.serial => Err(Error::Rollback),
        _ => Ok(Some(*candidate)),
    }
}

/// Locks held from the final anti-rollback re-read until this bundle is staged
/// (install/upgrade) or the staged generation is re-verified (rollback).
///
/// LOCK ORDER — the only order used in this crate; acquired 1 → 3, released 3 → 1
/// (fields drop in declaration order, so `state` goes first and `stage` last):
///   1. `<protected>/bootstrap/update.lock` — the stage lock; taken only by this
///      verifier (acceptance and rollback) and by the creator of the state root.
///   2. `<protected>/version-lock` ([`VERSION_LOCK`]) — duduclaw-cli
///      `protected_linux_install::version_lock` holds it (blocking LOCK_EX) for the whole
///      of install / uninstall / upgrade, which write version-floor and version-pending
///      (`prepare_version`, `record_floor`, also from protected_linux_upgrade.rs).
///   3. `<state_root>/update.lock`, when the state root exists — `files::Directory::lock`
///      (LOCK_EX|LOCK_NB) held by every updater-state writer (service, launcher,
///      trust_init, trust_admin, emergency, bootstrap) and by the CLI's
///      `parse_runtime_tree`.
/// Creating the state root: when it does not exist there is no lock 3 to hold, so its
/// creator must be serialised with acceptance by locks that exist beforehand. The one
/// creator in this crate, `trust_init::publish`, goes through
/// [`create_state_root_locked`]: lock 1, then lock 2 (bounded, like acceptance), then
/// lock 3 if the root already exists, then its acceptance re-check
/// ([`reverify_for_publish`]: stage, history, exact high-water record), and only if
/// that passes create the root (0700) and take its lock 3 (LOCK_NB); all held until it
/// returns. A refused re-check therefore creates no state root.
/// The CLI's install creates it (`protected_linux_layout::DIRECTORIES`) while holding
/// lock 2, which acceptance holds too. As a second line of defence acceptance still
/// refuses (`Busy`) when a root that was absent when its locks were taken exists after
/// the history re-read or after the decision ([`read_unless_state_root_appeared`]).
/// Deadlock check: lock 1 is only ever taken first, while holding nothing, with a
/// bounded wait (acceptance, rollback, the state-root creator). No writer takes an
/// earlier lock while holding a later one: the nested writers take 2 then 3 (CLI
/// install → parse_runtime_tree) or 1, 2, 3 (acceptance, rollback, the creator). Every
/// lock-3 holder except acceptance and rollback fails instead of waiting (LOCK_NB), and
/// those two wait for it only within their deadline. So no cycle can form, and every
/// wait in this crate ends (`Busy`). flock is per open file description: a caller that
/// already holds lock 1 or 2 through another description (e.g. the CLI's version_lock)
/// and then enters acceptance or the creator waits for itself and is refused (`Busy`)
/// after [`LOCK_WAIT`]; it never proceeds unlocked.
/// All three are taken with exclusive flock on the writers' own lock files, opened
/// through the verified dirfds (openat2, O_NOFOLLOW, no symlinks), within one bounded
/// deadline ([`LOCK_WAIT`]): a lock that cannot be had in time refuses (`Busy`), never
/// proceeds unlocked.
struct AcceptanceLocks { state: Option<File>, _version: File, _stage: File }
impl AcceptanceLocks {
    fn acquire(paths: &Paths, protected: &Directory, stages: &Directory, wait: Duration) -> Result<Self> {
        let deadline = Instant::now() + wait;
        let (stage, version) = outer_locks(protected, stages, deadline)?;
        let state = match optional(Directory::open(&paths.state_root))? {
            None => None,
            Some(dir) => {
                if mode_of(&dir)? != 0o700 { return Err(Error::Maintenance); }
                let lock = open_lock(&dir, UPDATE_LOCK, &[UPDATER_STATE, UPDATER_LEDGER])?;
                flock_until(&lock, deadline)?;
                Some(lock)
            }
        };
        Ok(Self { state, _version: version, _stage: stage })
    }
}

/// Locks 1 → 2 of LOCK ORDER ([`AcceptanceLocks`]), each within `deadline`.
fn outer_locks(protected: &Directory, stages: &Directory, deadline: Instant) -> Result<(File, File)> {
    let stage = open_lock(stages, UPDATE_LOCK, &[])?;
    flock_until(&stage, deadline)?;
    let version = open_lock(protected, VERSION_LOCK, &[VERSION_FLOOR, VERSION_PENDING])?;
    flock_until(&version, deadline)?;
    Ok((stage, version))
}

/// What the creator of the state root holds (LOCK ORDER, [`AcceptanceLocks`]): the
/// state root itself and locks 3, 2, 1, released in that order (declaration order).
pub(crate) struct StateRootLocks { pub state: Directory, _state_lock: File, _version: File, _stage: File }

/// Create (or open) the state root only under the bootstrap acceptance locks, and only
/// after `accept` passed under them: lock 1 (`<protected>/bootstrap/update.lock`),
/// lock 2 (`<protected>/version-lock`), lock 3 (`files::Directory::lock`) when the
/// root already exists (so `accept` reads its files serialised with their writers),
/// then `accept`, then — if the root was absent — the root (root 0700, any other mode
/// refused) and its lock 3. When `accept` refuses, nothing but missing lock files is
/// created (and no state root). `protected` must already hold the verified stage
/// directory. flock is per open file description: a caller that already holds lock 1
/// or 2 (e.g. the CLI installer inside its `version_lock`) waits for itself and is
/// refused (`Busy`) after [`LOCK_WAIT`] — it must not call this (nor `trust_init::publish`).
pub(crate) fn create_state_root_locked<T>(protected: &Path, state_root: &Path, accept: impl FnOnce() -> Result<T>)
    -> Result<(StateRootLocks, T)> {
    create_state_root_locked_until(protected, state_root, Instant::now() + LOCK_WAIT, accept)
}

fn create_state_root_locked_until<T>(protected: &Path, state_root: &Path, deadline: Instant,
    accept: impl FnOnce() -> Result<T>) -> Result<(StateRootLocks, T)> {
    let protected = Directory::open(protected)?;
    if mode_of(&protected)? != 0o700 { return Err(Error::Maintenance); }
    let stages = protected.child(STAGE_DIRECTORY)?;
    if mode_of(&stages)? != 0o700 { return Err(Error::Maintenance); }
    let (parent, name) = open_parent(state_root)?;
    accept_then_create(
        || {
            let (stage, version) = outer_locks(&protected, &stages, deadline)?;
            let existing = match optional(parent.child(&name))? {
                None => None,
                Some(dir) => {
                    if mode_of(&dir)? != 0o700 { return Err(Error::Maintenance); }
                    let lock = dir.lock()?;
                    Some((dir, lock))
                }
            };
            Ok((stage, version, existing))
        },
        |_| accept(),
        |(stage, version, existing): (File, File, Option<(Directory, File)>)| {
            let (state, state_lock) = match existing {
                Some(pair) => pair,
                None => {
                    // Absent when the locks were taken and nobody else may create it under
                    // locks 1+2: a root that appeared anyway was not serialised with us.
                    if optional(parent.child(&name))?.is_some() { return Err(Error::Busy); }
                    let state = ensure_dir(&parent, &name, 0o700)?;
                    let lock = state.lock()?;
                    (state, lock)
                }
            };
            Ok(StateRootLocks { state, _state_lock: state_lock, _version: version, _stage: stage })
        })
}

/// Ordering of [`create_state_root_locked`]: take the locks, run the re-check under
/// them, and only when it passes create anything (`create` consumes the guard).
/// Generic only so the ordering is unit-testable.
pub(crate) fn accept_then_create<G, T, S>(lock: impl FnOnce() -> Result<G>, accept: impl FnOnce(&G) -> Result<T>,
    create: impl FnOnce(G) -> Result<S>) -> Result<(S, T)> {
    let guard = lock()?;
    let value = accept(&guard)?;
    Ok((create(guard)?, value))
}

/// The pure publish rule (`trust_init::publish`, R1-1): the staged generation may be
/// published only while it is still the newest accepted bundle — the high-water record
/// must be EXACTLY this stage's (version, serial, generation digest, bundle digest) —
/// and the full anti-rollback rule (`check_history`) still accepts it in the mode it
/// was staged in. A newer acceptance after the receipt (higher mark), a missing or
/// different record, or a rollback receipt refuses.
pub(crate) fn check_publish(history: &History, receipt: &StageReceipt, manifest: &ReleaseManifest, now: u64)
    -> Result<()> {
    if receipt.schema_version != 1 || receipt.mode == BootstrapMode::Rollback { return Err(Error::Maintenance); }
    if receipt.artifacts != manifest.artifacts { return Err(Error::Signature); }
    let candidate = HighWater { schema_version: 1, version: manifest.version, serial: manifest.serial,
        digest: receipt.manifest_digest, bundle_sha256: receipt.bundle_sha256 };
    if history.high_water.as_ref() != Some(&candidate) { return Err(Error::Rollback); }
    check_history(history, receipt.mode, None, manifest, &candidate, now)
}

/// `trust_init::publish`'s re-check, run by [`create_state_root_locked`] under locks
/// 1–3 before anything is created: re-open and fully re-verify the stage from disk
/// (`check_stage`), require it to equal the caller's receipt, re-read the history and
/// apply [`check_publish`]. Returns the re-verified staged files.
pub(crate) fn reverify_for_publish(protected: &Path, state_root: &Path, stage: &StageReceipt, context: &Context)
    -> Result<StagedFiles> {
    let (receipt, staged) = open_stage_at(protected, stage.manifest_digest, context)?;
    if receipt != *stage { return Err(Error::Signature); }
    let paths = Paths { inbox: String::new(), protected: protected.to_path_buf(), facts_root: PathBuf::from("/"),
        state_root: state_root.to_path_buf() };
    check_publish(&read_history(&paths)?, &receipt, &staged.manifest, context.now)?;
    Ok(staged)
}

/// Open the lock file `name` in `dir` the way its writers do. Writers create a lock
/// before the first file it guards and never remove it, so a missing lock beside any
/// `guarded` file refuses; otherwise a missing lock is created (O_EXCL, 0600; an
/// existing one is opened instead). Symlinks and other non-regular files refuse
/// (openat2 O_NOFOLLOW + `files` owner checks), as does anything but root:root, one
/// link and exactly 0600 (the CLI's `version_lock` rule).
fn open_lock(dir: &Directory, name: &str, guarded: &[&str]) -> Result<File> {
    let file = match dir.read(name) {
        Ok(file) => file,
        Err(error) if missing(&error) => {
            for guarded in guarded {
                if optional(dir.read(guarded))?.is_some() { return Err(Error::Maintenance); }
            }
            match dir.create(name) {
                Ok(file) => file,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => dir.read(name)?,
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    if file.metadata()?.permissions().mode() & 0o7777 != 0o600 { return Err(Error::Maintenance); }
    Ok(file)
}

/// Exclusive flock, polled until `deadline`; `Busy` when it is still held then.
pub(crate) fn flock_until(file: &File, deadline: Instant) -> Result<()> {
    loop {
        // SAFETY: scalar flock on a live descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 { return Ok(()); }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK | libc::EINTR)) { return Err(error.into()); }
        let now = Instant::now();
        if now >= deadline { return Err(Error::Busy); }
        std::thread::sleep(LOCK_POLL.min(deadline - now));
    }
}

/// Serialised acceptance: take every lock first (`lock`, in LOCK ORDER), then re-read
/// the history and decide/record (`accept`) — closing the window between the unlocked
/// check and the high-water write. The guard is returned so the caller holds the
/// locks until staging is complete. Generic only so the ordering is unit-testable.
pub(crate) fn accept_locked<G>(lock: impl FnOnce() -> Result<G>, read: impl FnOnce(&G) -> Result<History>,
    accept: impl FnOnce(&History) -> Result<()>) -> Result<G> {
    let guard = lock()?;
    let history = read(&guard)?;
    accept(&history)?;
    Ok(guard)
}

/// Rollback under the same acceptance locks (LOCK ORDER): take them, re-read the
/// history and re-run the anti-rollback check (`check`; rollback records nothing),
/// then re-verify the already staged generation (`reverify`) while they are still
/// held; they are released only after it returns. Generic only so the ordering is
/// unit-testable.
pub(crate) fn rollback_locked<G, T>(lock: impl FnOnce() -> Result<G>, read: impl FnOnce(&G) -> Result<History>,
    check: impl FnOnce(&History) -> Result<()>, reverify: impl FnOnce(&G) -> Result<T>) -> Result<T> {
    let guard = accept_locked(lock, read, check)?;
    let result = reverify(&guard);
    drop(guard);
    result
}

/// `read_history` under [`AcceptanceLocks`] ([`read_unless_state_root_appeared`]).
fn read_history_locked(paths: &Paths, locks: &AcceptanceLocks) -> Result<History> {
    read_unless_state_root_appeared(locks.state.is_some(), &paths.state_root, || read_history(paths))
}

/// Second line of defence (LOCK ORDER): `state_locked` says whether the state root
/// existed, and so had its update.lock taken, when the acceptance locks were taken.
/// A root that was absent then but exists after `read` has a writer this run did not
/// serialise with: refuse (`Busy`). Checked after the read, so a root created at any
/// point up to then is caught; `stage` repeats it after the decision.
pub(crate) fn read_unless_state_root_appeared<T>(state_locked: bool, state_root: &Path,
    read: impl FnOnce() -> Result<T>) -> Result<T> {
    let value = read()?;
    if !state_locked && present(state_root) { return Err(Error::Busy); }
    Ok(value)
}

/// Durably raise the high-water record with `atomic_json` (temp file, fsync,
/// renameat2, directory fsync) and read it back. Called only from `accept_locked`,
/// before the stage is written or reused, so before step 11.
fn record_high_water(protected: &Directory, history: &History, candidate: &HighWater) -> Result<()> {
    if let Some(raised) = raised_high_water(history.high_water.as_ref(), candidate)? {
        protected.atomic_json(BOOTSTRAP_HIGH_WATER, &raised)?;
    }
    if read_high_water(protected)?.as_ref() != Some(candidate) { return Err(Error::Maintenance); }
    Ok(())
}

// Step 3 ------------------------------------------------------------------------

struct Stat { mode: u32, nlink: u32, uid: u32, size: u64 }
fn statx(file: &File) -> Result<Stat> {
    let mask = libc::STATX_TYPE | libc::STATX_MODE | libc::STATX_NLINK | libc::STATX_UID | libc::STATX_GID | libc::STATX_SIZE;
    let mut buffer = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: empty path with AT_EMPTY_PATH describes the live fd; buffer is writable.
    let rc = unsafe { libc::syscall(libc::SYS_statx, file.as_raw_fd(), c"".as_ptr(),
        libc::AT_EMPTY_PATH | libc::AT_STATX_SYNC_AS_STAT, mask, buffer.as_mut_ptr()) };
    if rc != 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: the successful syscall initialized the structure (zeroed beforehand).
    let stat = unsafe { buffer.assume_init() };
    if stat.stx_mask & mask != mask { return Err(Error::Maintenance); }
    Ok(Stat { mode: u32::from(stat.stx_mode), nlink: stat.stx_nlink, uid: stat.stx_uid, size: stat.stx_size })
}

struct Inbox { tar: File, tar_size: u64, side: SideFiles }

/// The inbox directory is opened once (openat2: no symlinks, no magic links, no
/// mount crossing) and must be root 0700; each member is opened once beneath it.
fn open_inbox(paths: &Paths, context: &Context) -> Result<Inbox> {
    let slash = File::open("/")?;
    let inbox = files::open_at(&slash, &cstring(&paths.inbox)?, libc::O_RDONLY | libc::O_DIRECTORY, 0, true)?;
    let stat = statx(&inbox)?;
    if stat.mode & libc::S_IFMT != libc::S_IFDIR || stat.uid != 0 || stat.mode & 0o7777 != 0o700 {
        return Err(Error::Maintenance);
    }
    let tar_name = context.tar_name();
    let (tar, tar_size) = open_member(&inbox, &tar_name, MAX_MIGRATION_BUNDLE_BYTES)?;
    let minisig = read_member(&inbox, &format!("{tar_name}.minisig"))?;
    let authorization = read_member(&inbox, &format!("{tar_name}.authorization.json"))?;
    let authorization_signatures = read_member(&inbox, &format!("{tar_name}.authorization.json.signatures.json"))?;
    Ok(Inbox { tar, tar_size, side: SideFiles { minisig: String::from_utf8(minisig).map_err(|_| Error::Signature)?,
        authorization, authorization_signatures } })
}
/// Regular file, one link, uid 0, not group/other writable, 0 < size <= limit.
fn open_member(dir: &File, name: &str, limit: u64) -> Result<(File, u64)> {
    // O_NONBLOCK: a FIFO in place of a member must not block before it is refused.
    let file = files::open_at(dir, &cstring(name)?, libc::O_RDONLY | libc::O_NONBLOCK, 0, true)?;
    let stat = statx(&file)?;
    if stat.mode & libc::S_IFMT != libc::S_IFREG || stat.nlink != 1 || stat.uid != 0 || stat.mode & 0o022 != 0
        || stat.size == 0 || stat.size > limit { return Err(Error::Maintenance); }
    Ok((file, stat.size))
}
fn read_member(dir: &File, name: &str) -> Result<Vec<u8>> {
    let (file, size) = open_member(dir, name, MAX_SIDE_FILE_BYTES)?;
    let mut bytes = Vec::new();
    Read::take(&file, MAX_SIDE_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| Error::Invalid)? != size { return Err(Error::Maintenance); }
    Ok(bytes)
}

// Step 4 ------------------------------------------------------------------------

fn memfd(name: &std::ffi::CStr, executable: bool) -> Result<File> {
    let base = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    // SAFETY: name is a valid C string; flags are scalar.
    let mut fd = if executable { unsafe { libc::memfd_create(name.as_ptr(), base | MFD_EXEC) } } else { -1 };
    if fd < 0 && (!executable || std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL)) {
        // Before Linux 6.3 MFD_EXEC is unknown and every memfd is executable.
        // SAFETY: as above.
        fd = unsafe { libc::memfd_create(name.as_ptr(), base) };
    }
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh, uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
/// Add SEAL|SHRINK|GROW|WRITE and read them back.
fn seal(file: &File) -> Result<()> {
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: scalar fcntl operations on a live fd.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: as above.
    let actual = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if actual < 0 || actual & seals != seals { return Err(Error::Maintenance); }
    Ok(())
}

/// Read-only mapping of a sealed memfd holding the bundle. Seals forbid writes and
/// size changes, so the mapped bytes cannot change and access cannot fault.
pub(crate) struct SealedImage { _memfd: File, map: *mut libc::c_void, len: usize }
impl SealedImage {
    fn copy_from(source: &File, size: u64) -> Result<Self> {
        let len = usize::try_from(size).map_err(|_| Error::Invalid)?;
        if len == 0 { return Err(Error::Invalid); }
        let mut memfd = memfd(c"duduclaw-bundle", false)?;
        let mut reader = source;
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut remaining = size;
        while remaining > 0 {
            let cap = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| Error::Invalid)?;
            let count = reader.read(buffer.get_mut(..cap).ok_or(Error::Invalid)?)?;
            if count == 0 { return Err(Error::Signature); }
            memfd.write_all(buffer.get(..count).ok_or(Error::Invalid)?)?;
            remaining -= count as u64;
        }
        // The file grew after statx: refuse instead of choosing a length.
        if reader.read(&mut [0u8; 1])? != 0 { return Err(Error::Signature); }
        seal(&memfd)?;
        if memfd.metadata()?.len() != size { return Err(Error::Signature); }
        // SAFETY: maps `len` readable bytes of a live, sealed memfd of exactly that size.
        let map = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, memfd.as_raw_fd(), 0) };
        if map == libc::MAP_FAILED { return Err(std::io::Error::last_os_error().into()); }
        Ok(Self { _memfd: memfd, map, len })
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is live for &self, `len` bytes long and never written.
        unsafe { std::slice::from_raw_parts(self.map as *const u8, self.len) }
    }
}
impl Drop for SealedImage {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the region mapped in copy_from.
        unsafe { libc::munmap(self.map, self.len); }
    }
}

// Steps 5–8 ---------------------------------------------------------------------

/// Pure verification over the sealed bytes. BOTH halves of the transition dual
/// signature are required: legacy minisign (5) and root threshold authorization (7).
pub(crate) fn verify_bundle(image: &[u8], side: &SideFiles, context: &Context, step: &mut Step) -> Result<Verified> {
    *step = Step::LegacySignature;
    let bundle_size = u64::try_from(image.len()).map_err(|_| Error::Invalid)?;
    if bundle_size == 0 || bundle_size > MAX_MIGRATION_BUNDLE_BYTES { return Err(Error::Invalid); }
    let bundle_sha256 = Sha256Digest(Sha256::digest(image).into());
    bundle::verify_legacy_signature(image, &side.minisig, &context.release_key, &context.trusted_comment())?;

    *step = Step::Root;
    let entries = bundle::read_ustar(image, &Limits::BOOTSTRAP)?;
    let root_bytes = BundleEntries::data(image, entries.root)?.to_vec();
    let root_signature_bytes = BundleEntries::data(image, entries.root_signatures)?.to_vec();
    let root_signatures = signature_list(&root_signature_bytes)?;
    // SHA-256 == compiled pin, 2-of-3 distinct root keys, expiry, revocation.
    let root = crate::trust::verify_root(&root_bytes, &root_signatures, context.pin, context.now)?;

    *step = Step::Authorization;
    let authorization = bundle::verify_authorization(&root, &side.authorization,
        &signature_list(&side.authorization_signatures)?, bundle_sha256, bundle_size, context.arch,
        context.version, context.now)?;

    *step = Step::Manifest;
    let manifest_bytes = BundleEntries::data(image, entries.manifest)?.to_vec();
    let manifest_signature_bytes = BundleEntries::data(image, entries.manifest_signatures)?.to_vec();
    let manifest = crate::publication::authenticate(&root_bytes, &root_signatures, context.pin, &manifest_bytes,
        &signature_list(&manifest_signature_bytes)?, context.arch, context.now)?;
    if manifest.version != authorization.version { return Err(Error::Incompatible); }
    entries.check_manifest(&manifest)?;
    Ok(Verified { bundle_sha256, bundle_size, authorization_sha256: Sha256Digest(Sha256::digest(&side.authorization).into()),
        entries, root_bytes, root_signature_bytes, manifest_bytes, manifest_signature_bytes, manifest })
}

// Step 9 ------------------------------------------------------------------------

pub(crate) fn mode_of(dir: &Directory) -> Result<u32> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live directory fd; the buffer is written on success.
    if unsafe { libc::fstat(dir.raw_fd(), stat.as_mut_ptr()) } != 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: initialized by the successful fstat.
    Ok(unsafe { stat.assume_init() }.st_mode & 0o7777)
}
/// Open a root-owned child directory with exactly `mode`, creating it if absent.
/// An existing directory with another mode is refused, never repaired.
pub(crate) fn ensure_dir(parent: &Directory, name: &str, mode: u32) -> Result<Directory> {
    let dir = match parent.child(name) {
        Ok(dir) => dir,
        Err(error) if missing(&error) => {
            let dir = parent.create_child(name)?;
            // SAFETY: live directory fd; fixed mode.
            if unsafe { libc::fchmod(dir.raw_fd(), mode as libc::mode_t) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            dir.sync()?;
            parent.sync()?;
            dir
        }
        Err(error) => return Err(error),
    };
    if mode_of(&dir)? != mode { return Err(Error::Maintenance); }
    Ok(dir)
}
fn open_parent(path: &Path) -> Result<(Directory, String)> {
    let parent = path.parent().ok_or(Error::Invalid)?;
    let name = path.file_name().and_then(|name| name.to_str()).ok_or(Error::Invalid)?;
    Ok((Directory::open(parent)?, name.to_owned()))
}

fn same_verification(on_disk: &StageReceipt, fresh: &StageReceipt) -> bool {
    let mut normalized = fresh.clone();
    normalized.verified_at_unix = on_disk.verified_at_unix;
    normalized.mode = on_disk.mode;
    normalized == *on_disk
}

/// Remove an interrupted `<hex>.next` (never a published name) with the bounded,
/// link-refusing cleanup of `files::Directory`.
fn remove_partial(stages: &Directory, next: &str) -> Result<()> {
    let dir = match stages.child(next) {
        Ok(dir) => dir,
        Err(error) if missing(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    match dir.child(STAGE_ARTIFACTS) {
        Ok(_) => dir.remove_artifact_directory(STAGE_ARTIFACTS)?,
        Err(error) if missing(&error) => (),
        Err(error) => return Err(error),
    }
    drop(dir);
    stages.remove_artifact_directory(next)
}

fn stage(paths: &Paths, context: &Context, mode: BootstrapMode, resume: Option<Sha256Digest>, image: &[u8],
    verified: &Verified) -> Result<StageReceipt> {
    let generation = manifest_digest(&verified.manifest_bytes);
    let expected = StageReceipt { schema_version: 1, mode, manifest_digest: generation, root_sha256: context.pin,
        bundle_sha256: verified.bundle_sha256, bundle_size: verified.bundle_size,
        authorization_sha256: verified.authorization_sha256, verifier_pin_kind: context.pin_kind,
        verified_at_unix: context.now, artifacts: verified.manifest.artifacts.clone() };
    let candidate = high_water_of(verified, generation);
    let final_name = hex::encode(generation.0);
    if mode == BootstrapMode::Rollback {
        // Rollback never creates or writes a stage, a directory or a record: it only
        // re-verifies the generation the in-progress run already staged (same
        // receipt), or refuses. It takes the same three acceptance locks (LOCK ORDER),
        // re-reads the history and re-runs check_history under them, so an updater
        // that committed a higher floor or state after the unlocked check makes it
        // refuse; the locks are held until the re-verification is complete.
        let protected = Directory::open(&paths.protected)?;
        if mode_of(&protected)? != 0o700 { return Err(Error::Maintenance); }
        let stages = protected.child(STAGE_DIRECTORY)?;
        if mode_of(&stages)? != 0o700 { return Err(Error::Maintenance); }
        return rollback_locked(|| AcceptanceLocks::acquire(paths, &protected, &stages, LOCK_WAIT),
            |locks| read_history_locked(paths, locks),
            |history| check_history(history, mode, resume, &verified.manifest, &candidate, context.now),
            |locks: &AcceptanceLocks| {
                read_unless_state_root_appeared(locks.state.is_some(), &paths.state_root, || Ok(()))?;
                let (receipt, _) = check_stage(&stages.child(&final_name)?, generation, context)?;
                if !same_verification(&receipt, &expected) { return Err(Error::Maintenance); }
                Ok(receipt)
            });
    }
    let (parent, name) = open_parent(&paths.protected)?;
    let protected = ensure_dir(&parent, &name, 0o700)?;
    let stages = ensure_dir(&protected, STAGE_DIRECTORY, 0o700)?;
    // Accepted under all three acceptance locks (LOCK ORDER, `AcceptanceLocks`), held
    // until this function returns, i.e. until the stage is published or reused. The
    // high-water record is durable before the stage is written or reused, so before
    // any bundle byte can be executed (step 11).
    let locks = accept_locked(|| AcceptanceLocks::acquire(paths, &protected, &stages, LOCK_WAIT),
        |locks| read_history_locked(paths, locks),
        |history| {
            check_history(history, mode, resume, &verified.manifest, &candidate, context.now)?;
            record_high_water(&protected, history, &candidate)
        })?;
    // Second line of defence, after the decision: no unserialised state root appeared.
    read_unless_state_root_appeared(locks.state.is_some(), &paths.state_root, || Ok(()))?;
    match stages.child(&final_name) {
        Ok(existing) => {
            // Never overwritten: the whole stage is re-verified and must agree.
            let (receipt, _) = check_stage(&existing, generation, context)?;
            if !same_verification(&receipt, &expected) { return Err(Error::Maintenance); }
            return Ok(receipt);
        }
        Err(error) if missing(&error) => (),
        Err(error) => return Err(error),
    }
    let total = verified.manifest.artifacts.iter().try_fold(0u64, |sum, artifact| sum.checked_add(artifact.size_bytes))
        .ok_or(Error::Invalid)?;
    stages.reserve_space(total)?;
    let next = format!("{final_name}.next");
    remove_partial(&stages, &next)?;
    let dir = stages.create_child(&next)?;
    let artifacts = dir.create_child(STAGE_ARTIFACTS)?;
    for artifact in &verified.manifest.artifacts {
        let entry = *verified.entries.artifacts.get(&artifact.id).ok_or(Error::Signature)?;
        let mut source = BundleEntries::data(image, entry)?;
        let mut output = artifacts.create(&artifact.id)?;
        copy_artifact(&mut source, &mut output, artifact)?;
        output.set_permissions(std::fs::Permissions::from_mode(u32::from(artifact.mode)))?;
        output.sync_all()?;
        drop(output);
        verify_fd(artifacts.read(&artifact.id)?, artifact.size_bytes, artifact.sha256)?;
    }
    artifacts.sync()?;
    let receipt_bytes = serde_json::to_vec(&expected)?;
    for (file, content) in [(bundle::ROOT_ENTRY, verified.root_bytes.as_slice()),
        (bundle::ROOT_SIGNATURES_ENTRY, verified.root_signature_bytes.as_slice()),
        (bundle::MANIFEST_ENTRY, verified.manifest_bytes.as_slice()),
        (bundle::MANIFEST_SIGNATURES_ENTRY, verified.manifest_signature_bytes.as_slice()),
        (STAGE_RECEIPT, receipt_bytes.as_slice())] {
        let mut output = dir.create(file)?;
        output.write_all(content)?;
        output.sync_all()?;
    }
    dir.sync()?;
    let (written, _) = check_stage(&dir, generation, context)?;
    if written != expected { return Err(Error::Maintenance); }
    drop(artifacts);
    drop(dir);
    // renameat2(NOREPLACE) fsyncs the bootstrap directory; then fsync its parent.
    stages.rename(&next, &stages, &final_name, false)?;
    protected.sync()?;
    let (published, _) = check_stage(&stages.child(&final_name)?, generation, context)?;
    if published != expected { return Err(Error::Maintenance); }
    Ok(published)
}

/// Full re-verification of one staging directory: exact file set, root pin and
/// threshold, target signature, receipt binding, every artifact's mode, size, hash.
fn check_stage(dir: &Directory, generation: Sha256Digest, context: &Context) -> Result<(StageReceipt, StagedFiles)> {
    if mode_of(dir)? != 0o700 { return Err(Error::Maintenance); }
    let mut names = dir.entries(8)?;
    names.sort();
    let mut expected = vec![STAGE_ARTIFACTS, bundle::ROOT_ENTRY, bundle::ROOT_SIGNATURES_ENTRY, bundle::MANIFEST_ENTRY,
        bundle::MANIFEST_SIGNATURES_ENTRY, STAGE_RECEIPT];
    expected.sort_unstable();
    if names != expected { return Err(Error::Signature); }
    let root_bytes = dir.bytes(bundle::ROOT_ENTRY, MAX_METADATA_BYTES)?;
    let root_signatures = signature_list(&dir.bytes(bundle::ROOT_SIGNATURES_ENTRY, MAX_SIDE_FILE_BYTES as usize)?)?;
    let manifest_bytes = dir.bytes(bundle::MANIFEST_ENTRY, MAX_METADATA_BYTES)?;
    let manifest_signatures = signature_list(&dir.bytes(bundle::MANIFEST_SIGNATURES_ENTRY, MAX_SIDE_FILE_BYTES as usize)?)?;
    let manifest = crate::publication::authenticate(&root_bytes, &root_signatures, context.pin, &manifest_bytes,
        &manifest_signatures, context.arch, context.now)?;
    if manifest_digest(&manifest_bytes) != generation { return Err(Error::Signature); }
    let receipt: StageReceipt = serde_json::from_slice(&dir.bytes(STAGE_RECEIPT, MAX_RECEIPT_BYTES)?)?;
    if receipt.schema_version != 1 || receipt.manifest_digest != generation || receipt.root_sha256 != context.pin
        || receipt.verifier_pin_kind != context.pin_kind || receipt.artifacts != manifest.artifacts
        || receipt.verified_at_unix > context.now || receipt.bundle_size == 0
        || receipt.bundle_size > MAX_MIGRATION_BUNDLE_BYTES { return Err(Error::Signature); }
    let artifacts = dir.child(STAGE_ARTIFACTS)?;
    if mode_of(&artifacts)? != 0o700 { return Err(Error::Maintenance); }
    let mut ids = artifacts.entries(MAX_ARTIFACTS + 1)?;
    ids.sort();
    let mut expected_ids: Vec<&str> = manifest.artifacts.iter().map(|artifact| artifact.id.as_str()).collect();
    expected_ids.sort_unstable();
    if ids != expected_ids { return Err(Error::Signature); }
    for artifact in &manifest.artifacts {
        let file = artifacts.read(&artifact.id)?;
        if file.metadata()?.permissions().mode() & 0o7777 != u32::from(artifact.mode) { return Err(Error::Signature); }
        verify_fd(file, artifact.size_bytes, artifact.sha256)?;
    }
    // publication::authenticate accepted exactly one target signature.
    let manifest_signature = manifest_signatures.into_iter().next().ok_or(Error::Signature)?;
    Ok((receipt, StagedFiles { artifacts, root_bytes, root_signatures, manifest_bytes, manifest_signature, manifest }))
}

/// Open and fully re-verify `<protected>/bootstrap/<generation>`.
pub(crate) fn open_stage_at(protected: &Path, generation: Sha256Digest, context: &Context) -> Result<(StageReceipt, StagedFiles)> {
    let protected = Directory::open(protected)?;
    if mode_of(&protected)? != 0o700 { return Err(Error::Maintenance); }
    let stages = protected.child(STAGE_DIRECTORY)?;
    if mode_of(&stages)? != 0o700 { return Err(Error::Maintenance); }
    check_stage(&stages.child(&hex::encode(generation.0))?, generation, context)
}

// Step 10 -----------------------------------------------------------------------

/// Pin, bundle digest and version for the administrator's second comparison with
/// the published values (design §8 step 3). No paths are printed.
fn announce(context: &Context, staged: &Staged) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{} {}", context.pin_kind.label(), hex::encode(context.pin.0))?;
    writeln!(out, "bundle sha256 {}", hex::encode(staged.receipt.bundle_sha256.0))?;
    writeln!(out, "version {}", crate::version_text(staged.version))?;
    writeln!(out, "請把以上三行與官網及授權 console 公布的值再比對一次；任何一項不一致，請立即停止並聯絡原廠。")?;
    out.flush()?;
    Ok(())
}

// Step 11 -----------------------------------------------------------------------

/// Copy a verified executable into a fresh sealed memfd and re-hash the sealed
/// bytes, so what execveat maps is exactly what was verified.
pub(crate) fn sealed_executable(mut source: File, artifact: &ReleaseArtifact) -> Result<File> {
    source.seek(SeekFrom::Start(0))?;
    let mut memfd = memfd(c"duduclaw-gateway", true)?;
    copy_artifact(&mut source, &mut memfd, artifact)?;
    seal(&memfd)?;
    if memfd.metadata()?.len() != artifact.size_bytes { return Err(Error::Signature); }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut offset = 0u64;
    while offset < artifact.size_bytes {
        let count = memfd.read_at(&mut buffer, offset)?;
        if count == 0 { return Err(Error::Signature); }
        hash.update(buffer.get(..count).ok_or(Error::Invalid)?);
        offset += count as u64;
    }
    if offset != artifact.size_bytes || hash.finalize().as_slice() != artifact.sha256.0 { return Err(Error::Signature); }
    Ok(memfd)
}

/// Mark every descriptor above stderr close-on-exec, then execveat the sealed fd
/// with AT_EMPTY_PATH. fd 0/1/2 are kept (R5's first-administrator link needs the
/// terminal). Returns only the failure; there is no path-based fallback.
/// `argv` and `envp` are NULL-terminated pointer arrays prepared by the caller,
/// so this is allocation-free and usable after fork.
pub(crate) fn exec_sealed(fd: RawFd, argv: &[*const libc::c_char], envp: &[*const libc::c_char]) -> std::io::Error {
    if argv.last().is_none_or(|last| !last.is_null()) || envp.last().is_none_or(|last| !last.is_null()) {
        return std::io::Error::from(std::io::ErrorKind::InvalidInput);
    }
    // SAFETY: scalar arguments only.
    if unsafe { libc::syscall(libc::SYS_close_range, 3 as libc::c_uint, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC) } != 0 {
        return std::io::Error::last_os_error();
    }
    // SAFETY: NULL-terminated arrays of live C strings; the fd is live.
    unsafe { libc::syscall(libc::SYS_execveat, fd, c"".as_ptr(), argv.as_ptr(), envp.as_ptr(), libc::AT_EMPTY_PATH); }
    std::io::Error::last_os_error()
}

fn exec_gateway(paths: &Paths, staged: &Staged, mode: BootstrapMode) -> Result<std::convert::Infallible> {
    let gateway = &staged.gateway;
    let generation = hex::encode(staged.receipt.manifest_digest.0);
    let stage = Directory::open(&paths.protected)?.child(STAGE_DIRECTORY)?.child(&generation)?.child(STAGE_ARTIFACTS)?;
    let source = verify_fd(stage.read(&gateway.id)?, gateway.size_bytes, gateway.sha256)?;
    let memfd = sealed_executable(source, gateway)?;
    let args = ["duduclaw", "service", "bootstrap-continue", "--protected-linux", "--mode", mode.as_str(),
        "--generation", generation.as_str()].into_iter().map(cstring).collect::<Result<Vec<_>>>()?;
    let env = ["PATH=/usr/sbin:/usr/bin:/sbin:/bin", "LANG=C"].into_iter().map(cstring).collect::<Result<Vec<_>>>()?;
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|value| value.as_ptr()).collect();
    argv.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> = env.iter().map(|value| value.as_ptr()).collect();
    envp.push(std::ptr::null());
    Err(exec_sealed(memfd.as_raw_fd(), &argv, &envp).into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{bundle::tests::{append, finish}, metadata::canonical_bytes, publication::artifact_name,
        sshsig::test_support::{key, sign}};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::{Signer, SigningKey as PrivateKey};

    use crate::bundle::tests::OFFICIAL_SIGNATURE;

    /// The verifier's own release: the workspace version, never a hardcoded literal
    /// (the workspace is bumped only at release time).
    pub(crate) fn version() -> ReleaseVersion {
        crate::parse_version(env!("CARGO_PKG_VERSION")).expect("workspace version is a stable X.Y.Z")
    }
    fn next_patch() -> ReleaseVersion { ReleaseVersion { patch: version().patch + 1, ..version() } }
    fn version_string() -> String { crate::version_text(version()) }
    // Fixed times keep fixtures (and so the generation digest) deterministic.
    pub(crate) const CREATED: u64 = 1_700_000_000;
    pub(crate) const EXPIRES: u64 = 4_000_000_000;
    const NOW: u64 = 1_800_000_000;

    /// Software minisign signer for the test key set (prehashed = BLAKE2b-512).
    pub(crate) struct Minisign { secret: PrivateKey, key_id: [u8; 8] }
    impl Minisign {
        pub(crate) fn new(seed: u8) -> Self { Self { secret: PrivateKey::from_bytes(&[seed; 32]), key_id: [seed; 8] } }
        pub(crate) fn public(&self) -> String {
            let mut blob = b"Ed".to_vec();
            blob.extend_from_slice(&self.key_id);
            blob.extend_from_slice(&self.secret.verifying_key().to_bytes());
            STANDARD.encode(blob)
        }
        pub(crate) fn sign(&self, data: &[u8], trusted_comment: &str, prehashed: bool) -> String {
            let (algorithm, signature) = if prehashed {
                (b"ED", self.secret.sign(<blake2::Blake2b512 as blake2::Digest>::digest(data).as_slice()))
            } else {
                (b"Ed", self.secret.sign(data))
            };
            let mut blob = algorithm.to_vec();
            blob.extend_from_slice(&self.key_id);
            blob.extend_from_slice(&signature.to_bytes());
            let mut global = signature.to_bytes().to_vec();
            global.extend_from_slice(trusted_comment.as_bytes());
            let global = self.secret.sign(&global);
            format!("untrusted comment: test signature\n{}\ntrusted comment: {trusted_comment}\n{}\n",
                STANDARD.encode(blob), STANDARD.encode(global.to_bytes()))
        }
    }

    pub(crate) struct Fixture {
        pub roots: Vec<PrivateKey>,
        pub root: RootMetadata,
        pub target: PrivateKey,
        pub minisign: Minisign,
        pub root_bytes: Vec<u8>,
        pub root_signatures: Vec<String>,
        pub manifest: ReleaseManifest,
        pub manifest_bytes: Vec<u8>,
        pub manifest_signature: String,
        pub image: Vec<u8>,
        pub side: SideFiles,
    }
    pub(crate) fn root_sign(root: &RootMetadata, keys: &[PrivateKey], bytes: &[u8], namespace: &str, signers: &[usize]) -> Vec<String> {
        signers.iter().map(|index| sign(&keys[*index], &root.root_keys[*index], namespace, bytes, 5)).collect()
    }
    /// Test root set of plan §3.3: R1 simulated `sk-ssh-ed25519` (ssh:duduclaw-root),
    /// R2/R3 `ssh-ed25519`, simulated `sk-ssh-ed25519` target (ssh:duduclaw-target).
    /// The returned fixture is fully signed (legacy minisign + root authorization).
    pub(crate) fn build(arch: Architecture, updater: Option<Vec<u8>>, manifest_version: ReleaseVersion) -> Fixture {
        let pairs = [key(11, Some(ROOT_SK_APPLICATION)), key(12, None), key(13, None)];
        let (target, target_key) = key(14, Some(TARGET_SK_APPLICATION));
        let root = RootMetadata { schema_version: 1, version: 1, expires_at_unix: EXPIRES, root_threshold: 2,
            root_keys: pairs.iter().map(|(_, key)| key.clone()).collect(), target_key, revoked_key_ids: vec![], frozen: false };
        let roots: Vec<PrivateKey> = pairs.into_iter().map(|(secret, _)| secret).collect();
        let root_bytes = canonical_bytes(&root).unwrap();
        let root_signatures = root_sign(&root, &roots, &root_bytes, ROOT_SSHSIG_NAMESPACE, &[0, 1]);
        let targets = [InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline, InstallTarget::Egress,
            InstallTarget::EgressForwarder, InstallTarget::McpBridge, InstallTarget::ParseWorker, InstallTarget::Importer,
            InstallTarget::Migration, InstallTarget::UpdateSlot];
        let artifacts: Vec<(ReleaseArtifact, Vec<u8>)> = targets.into_iter().map(|target| {
            let id = artifact_name(&target).unwrap();
            let bytes = match (&target, &updater) {
                (InstallTarget::UpdateSlot, Some(binary)) => binary.clone(),
                _ => format!("fixture-{id}").into_bytes(),
            };
            (ReleaseArtifact { id, sha256: manifest_digest(&bytes), size_bytes: bytes.len() as u64, mode: 0o755,
                install_target: target }, bytes)
        }).collect();
        let manifest = ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: manifest_version,
            channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch,
            minimum_version: ReleaseVersion { major: 1, minor: 80, patch: 0 }, serial: 1,
            created_at_unix: CREATED, expires_at_unix: EXPIRES,
            artifacts: artifacts.iter().map(|(artifact, _)| artifact.clone()).collect(),
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1,
                mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] };
        let manifest_bytes = canonical_bytes(&manifest).unwrap();
        let manifest_signature = sign(&target, &root.target_key, RELEASE_SSHSIG_NAMESPACE, &manifest_bytes, 5);
        let mut tar = Vec::new();
        append(&mut tar, bundle::ROOT_ENTRY, &root_bytes);
        append(&mut tar, bundle::ROOT_SIGNATURES_ENTRY, &serde_json::to_vec(&root_signatures).unwrap());
        append(&mut tar, bundle::MANIFEST_ENTRY, &manifest_bytes);
        append(&mut tar, bundle::MANIFEST_SIGNATURES_ENTRY, &serde_json::to_vec(&[&manifest_signature]).unwrap());
        for (artifact, bytes) in &artifacts { append(&mut tar, &format!("artifacts/{}", artifact.id), bytes); }
        let mut fixture = Fixture { roots, root, target, minisign: Minisign::new(21), root_bytes, root_signatures,
            manifest, manifest_bytes, manifest_signature, image: finish(tar),
            side: SideFiles { minisig: String::new(), authorization: vec![], authorization_signatures: vec![] } };
        let authorization = fixture.authorization(version(), EXPIRES);
        fixture.authorize(&authorization, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE);
        fixture.side.minisig = fixture.minisign.sign(&fixture.image, &fixture.trusted_comment(), true);
        fixture
    }
    impl Fixture {
        pub(crate) fn authorization(&self, version: ReleaseVersion, expires: u64) -> BundleAuthorization {
            BundleAuthorization { schema_version: 1, product: Product::Duduclaw, version, arch: self.manifest.arch,
                bundle_sha256: manifest_digest(&self.image), bundle_size: self.image.len() as u64, expires_at_unix: expires }
        }
        pub(crate) fn authorize(&mut self, authorization: &BundleAuthorization, signers: &[usize], namespace: &str) {
            let bytes = canonical_bytes(authorization).unwrap();
            self.side.authorization_signatures =
                serde_json::to_vec(&root_sign(&self.root, &self.roots, &bytes, namespace, signers)).unwrap();
            self.side.authorization = bytes;
        }
        pub(crate) fn context(&self, now: u64) -> Context {
            Context { pin: manifest_digest(&self.root_bytes), pin_kind: PinKind::Test, release_key: self.minisign.public(),
                arch: self.manifest.arch, version: version(), now }
        }
        pub(crate) fn trusted_comment(&self) -> String { self.context(NOW).trusted_comment() }
    }
    fn verify(fixture: &Fixture) -> std::result::Result<Verified, (Step, Error)> {
        let mut step = Step::Harden;
        verify_bundle(&fixture.image, &fixture.side, &fixture.context(NOW), &mut step).map_err(|error| (step, error))
    }
    fn refused_at(result: std::result::Result<Verified, (Step, Error)>) -> Step {
        match result { Ok(_) => panic!("verification unexpectedly succeeded"), Err((step, _)) => step }
    }

    #[test]
    fn authorization_threshold_namespace_expiry_size() {
        let fixture = build(Architecture::X86_64, None, version());
        let verified = verify(&fixture).unwrap();
        assert_eq!(verified.bundle_sha256, manifest_digest(&fixture.image));
        let good = fixture.authorization(version(), NOW + 10);
        let check = |authorization: &BundleAuthorization, signers: &[usize], namespace: &str, now: u64| {
            let bytes = canonical_bytes(authorization).unwrap();
            let signatures = root_sign(&fixture.root, &fixture.roots, &bytes, namespace, signers);
            bundle::verify_authorization(&fixture.root, &bytes, &signatures, manifest_digest(&fixture.image),
                fixture.image.len() as u64, Architecture::X86_64, version(), now)
        };
        let threshold: [&[usize]; 4] = [&[0, 1], &[0, 2], &[1, 2], &[0, 1, 2]];
        for signers in threshold { assert!(check(&good, signers, MIGRATION_BUNDLE_SSHSIG_NAMESPACE, NOW).is_ok()); }
        let below: [&[usize]; 5] = [&[], &[0], &[1], &[2], &[1, 1]];
        for signers in below { assert!(check(&good, signers, MIGRATION_BUNDLE_SSHSIG_NAMESPACE, NOW).is_err(), "{signers:?}"); }
        for namespace in [ROOT_SSHSIG_NAMESPACE, RELEASE_SSHSIG_NAMESPACE, EMERGENCY_SSHSIG_NAMESPACE, ""] {
            assert!(check(&good, &[0, 1], namespace, NOW).is_err(), "{namespace}");
        }
        // The target key is not a root key: an extra target signature refuses the set.
        let bytes = canonical_bytes(&good).unwrap();
        let mut signatures = root_sign(&fixture.root, &fixture.roots, &bytes, MIGRATION_BUNDLE_SSHSIG_NAMESPACE, &[0, 1]);
        signatures.push(sign(&fixture.target, &fixture.root.target_key, MIGRATION_BUNDLE_SSHSIG_NAMESPACE, &bytes, 5));
        assert!(bundle::verify_authorization(&fixture.root, &bytes, &signatures, manifest_digest(&fixture.image),
            fixture.image.len() as u64, Architecture::X86_64, version(), NOW).is_err());
        assert!(check(&good, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE, NOW + 9).is_ok());
        assert!(matches!(check(&good, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE, NOW + 10), Err(Error::Expired)));
        for bad in [BundleAuthorization { bundle_size: good.bundle_size + 1, ..good.clone() },
            BundleAuthorization { bundle_size: good.bundle_size - 1, ..good.clone() },
            BundleAuthorization { bundle_sha256: Sha256Digest([0; 32]), ..good.clone() },
            BundleAuthorization { version: next_patch(), ..good.clone() },
            BundleAuthorization { arch: Architecture::Aarch64, ..good.clone() },
            BundleAuthorization { product: Product::DuduclawPro, ..good.clone() },
            BundleAuthorization { schema_version: 2, ..good.clone() }] {
            assert!(check(&bad, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE, NOW).is_err(), "{bad:?}");
        }
        // Signed but non-canonical bytes are refused.
        let pretty = serde_json::to_vec_pretty(&good).unwrap();
        let signatures = root_sign(&fixture.root, &fixture.roots, &pretty, MIGRATION_BUNDLE_SSHSIG_NAMESPACE, &[0, 1]);
        assert!(bundle::verify_authorization(&fixture.root, &pretty, &signatures, manifest_digest(&fixture.image),
            fixture.image.len() as u64, Architecture::X86_64, version(), NOW).is_err());
        // The pipeline stops at step 7 for an authorization of another size.
        let mut wrong = build(Architecture::X86_64, None, version());
        let authorization = BundleAuthorization { bundle_size: wrong.image.len() as u64 - 1, ..wrong.authorization(version(), EXPIRES) };
        wrong.authorize(&authorization, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE);
        assert_eq!(refused_at(verify(&wrong)), Step::Authorization);
        let mut single = build(Architecture::X86_64, None, version());
        let authorization = single.authorization(version(), EXPIRES);
        single.authorize(&authorization, &[0], MIGRATION_BUNDLE_SSHSIG_NAMESPACE);
        assert_eq!(refused_at(verify(&single)), Step::Authorization);
    }

    #[test]
    fn minisign_required_and_prehashed_only() {
        // The compiled constant is the real release key: an official signature verifies.
        bundle::verify_legacy_signature(b"FAKE_BINARY", OFFICIAL_SIGNATURE, LEGACY_RELEASE_MINISIGN_KEY, "duduclaw test").unwrap();
        assert!(bundle::verify_legacy_signature(b"tampered", OFFICIAL_SIGNATURE, LEGACY_RELEASE_MINISIGN_KEY, "duduclaw test").is_err());
        let v = version_string();
        let x86_comment = format!("duduclaw v{v} duduclaw-isolated-linux-x86_64-{v}.tar");
        assert!(bundle::verify_legacy_signature(b"FAKE_BINARY", OFFICIAL_SIGNATURE, LEGACY_RELEASE_MINISIGN_KEY,
            &x86_comment).is_err());
        #[cfg(not(duduclaw_test_root))]
        assert_eq!(legacy_release_key().unwrap(), LEGACY_RELEASE_MINISIGN_KEY);
        let fixture = build(Architecture::X86_64, None, version());
        verify(&fixture).unwrap();
        let comment = fixture.trusted_comment();
        assert_eq!(comment, x86_comment);
        // Missing, legacy (non-prehashed), other key, other comment, other bytes,
        // and a valid signature followed by trailing lines (strict text, bundle.rs).
        let trailing = format!("{}untrusted extra line\n", fixture.side.minisig);
        for minisig in [String::new(), fixture.minisign.sign(&fixture.image, &comment, false),
            Minisign::new(22).sign(&fixture.image, &comment, true),
            fixture.minisign.sign(&fixture.image, &format!("duduclaw v{v} duduclaw-isolated-linux-aarch64-{v}.tar"), true),
            fixture.minisign.sign(b"other bytes", &comment, true), OFFICIAL_SIGNATURE.to_owned(), trailing] {
            let mut side = fixture.side.clone();
            side.minisig = minisig;
            let mut step = Step::Harden;
            assert!(verify_bundle(&fixture.image, &side, &fixture.context(NOW), &mut step).is_err());
            assert_eq!(step, Step::LegacySignature);
        }
        // A valid legacy signature never substitutes for the root authorization.
        for (authorization, signatures) in [(vec![], fixture.side.authorization_signatures.clone()),
            (fixture.side.authorization.clone(), b"[]".to_vec()),
            (fixture.side.authorization.clone(), vec![])] {
            let mut side = fixture.side.clone();
            side.authorization = authorization;
            side.authorization_signatures = signatures;
            let mut step = Step::Harden;
            assert!(verify_bundle(&fixture.image, &side, &fixture.context(NOW), &mut step).is_err());
            assert_eq!(step, Step::Authorization);
        }
    }

    #[test]
    fn root_pin_mismatch_refused() {
        assert!(parse_pin(None).is_err());
        assert_eq!(parse_pin(Some(&"ab".repeat(32))).unwrap(), Sha256Digest([0xab; 32]));
        for bad in [String::new(), "AB".repeat(32), "ab".repeat(31), "ab".repeat(33), format!(" {}", "ab".repeat(32)),
            format!("{}\n", "ab".repeat(32)), "zz".repeat(32), "ａｂ".repeat(16)] {
            assert!(parse_pin(Some(&bad)).is_err(), "{bad:?}");
        }
        assert_eq!(PinKind::Production.label(), "PRODUCTION");
        assert_eq!(PinKind::Test.label(), "TEST");
        #[cfg(not(duduclaw_test_root))]
        assert_eq!(BUILD_PIN_KIND, PinKind::Production);
        let fixture = build(Architecture::X86_64, None, version());
        let mut context = fixture.context(NOW);
        context.pin = Sha256Digest([0; 32]);
        let mut step = Step::Harden;
        assert!(verify_bundle(&fixture.image, &fixture.side, &context, &mut step).is_err());
        assert_eq!(step, Step::Root);
        // Another key set (e.g. production vs test) has another pin.
        let mut other = fixture.context(NOW);
        other.pin = manifest_digest(b"{\"other\":\"root\"}");
        assert!(verify_bundle(&fixture.image, &fixture.side, &other, &mut step).is_err());
        assert_eq!(step, Step::Root);
        // The correct pin with fewer than two root signatures is still refused.
        let pin = manifest_digest(&fixture.root_bytes);
        assert!(crate::trust::verify_root(&fixture.root_bytes, &fixture.root_signatures[..1], pin, NOW).is_err());
        assert!(crate::trust::verify_root(&fixture.root_bytes, &fixture.root_signatures, pin, EXPIRES).is_err());
    }

    #[test]
    fn manifest_version_must_equal_authorization() {
        let patch = next_patch();
        // Authorization names the verifier's version; the signed manifest says another.
        let mismatched = build(Architecture::X86_64, None, patch);
        let error = match verify(&mismatched) { Err((Step::Manifest, error)) => error, _ => panic!("expected step 8") };
        assert!(matches!(error, Error::Incompatible));
        // Authorization of a version other than this verifier's stops at step 7.
        let mut other = build(Architecture::X86_64, None, patch);
        let authorization = other.authorization(patch, EXPIRES);
        other.authorize(&authorization, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE);
        assert_eq!(refused_at(verify(&other)), Step::Authorization);
        let good = build(Architecture::X86_64, None, version());
        let verified = verify(&good).unwrap();
        assert_eq!(verified.manifest.version, version());
        assert_eq!(verified.manifest_bytes, good.manifest_bytes);
        assert_eq!(verified.entries.artifacts.len(), good.manifest.artifacts.len());
        // The tar must carry exactly the manifest's artifacts (no extra member).
        let mut extra = good.image[..good.image.len() - 10 * 512].to_vec();
        append(&mut extra, "artifacts/unsigned", b"unsigned");
        let mut extended = build(Architecture::X86_64, None, version());
        extended.image = finish(extra);
        let authorization = extended.authorization(version(), EXPIRES);
        extended.authorize(&authorization, &[0, 1], MIGRATION_BUNDLE_SSHSIG_NAMESPACE);
        extended.side.minisig = extended.minisign.sign(&extended.image, &extended.trusted_comment(), true);
        assert_eq!(refused_at(verify(&extended)), Step::Manifest);
    }

    fn no_history() -> History { History { floor: None, pending: None, high_water: None, state: None, ledger: None } }
    /// The record the fixture bundle sets (`high_water_of` without a sealed image).
    fn candidate(fixture: &Fixture) -> HighWater {
        HighWater { schema_version: 1, version: fixture.manifest.version, serial: fixture.manifest.serial,
            digest: manifest_digest(&fixture.manifest_bytes), bundle_sha256: manifest_digest(&fixture.image) }
    }
    /// A stable version the fixture's `minimum_version` (1.80.0) allows and below `version()`.
    fn lower() -> ReleaseVersion {
        let lower = ReleaseVersion { major: 1, minor: 80, patch: 0 };
        assert!(lower < version(), "fixtures assume the workspace version is above 1.80.0");
        lower
    }

    #[test]
    fn version_file_format_is_strict() {
        assert_eq!(parse_version_file(b"1.82.0").unwrap(), ReleaseVersion { major: 1, minor: 82, patch: 0 });
        assert_eq!(parse_version_file(version_string().as_bytes()).unwrap(), version());
        for bad in [&b""[..], b"v1.82.0", b"1.82.0\n", b" 1.82.0", b"1.82.0 ", b"1.82", b"1.82.0-rc.1", b"1.82.0+b",
            b"01.82.0", b"1.82.0.0", b"1..0", "１.82.0".as_bytes(), b"1.82.0\0"] {
            assert!(parse_version_file(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn version_floor_and_updater_serials_refuse_rollback() {
        let fixture = build(Architecture::X86_64, None, version());
        let manifest = &fixture.manifest;
        let digest = manifest_digest(&fixture.manifest_bytes);
        let mine = candidate(&fixture);
        let check = |history: History| check_history(&history, BootstrapMode::Install, None, manifest, &mine, NOW);
        // A fresh host with no floor and no updater state: first contact (bound to step 7).
        check(no_history()).unwrap();
        // Floor (and the pending reservation, same rule): lower passes, higher refuses ...
        check(History { floor: Some(lower()), ..no_history() }).unwrap();
        check(History { pending: Some(lower()), ..no_history() }).unwrap();
        assert!(matches!(check(History { floor: Some(next_patch()), ..no_history() }), Err(Error::Rollback)));
        assert!(matches!(check(History { pending: Some(next_patch()), ..no_history() }), Err(Error::Rollback)));
        assert!(matches!(check(History { floor: Some(lower()), pending: Some(next_patch()), ..no_history() }),
            Err(Error::Rollback)));
        // ... and EQUAL refuses unless this exact bundle is already recorded for that version.
        assert!(matches!(check(History { floor: Some(version()), ..no_history() }), Err(Error::Rollback)));
        assert!(matches!(check(History { pending: Some(version()), ..no_history() }), Err(Error::Rollback)));
        check(History { floor: Some(version()), high_water: Some(mine), ..no_history() }).unwrap();
        check(History { pending: Some(version()), high_water: Some(mine), ..no_history() }).unwrap();
        check(History { floor: Some(version()), state: Some(crate::linux::trust_init::initial_state(manifest, digest)),
            ..no_history() }).unwrap();

        // Updater state: resuming the generation it already commits is not a rollback.
        let same = crate::linux::trust_init::initial_state(manifest, digest);
        check(History { state: Some(same.clone()), ..no_history() }).unwrap();
        // Another generation at the same serial and version (a replayed older pair) refuses.
        let other = Sha256Digest([7; 32]);
        let replay = crate::linux::trust_init::initial_state(manifest, other);
        assert!(matches!(check(History { state: Some(replay), ..no_history() }), Err(Error::Rollback)));
        // Strictly newer than every accepted serial and above the floor: State::check_manifest accepts.
        let mut older = crate::linux::trust_init::initial_state(manifest, other);
        older.floor = lower();
        older.serial = 0;
        older.highest_serial = 0;
        check(History { state: Some(older.clone()), ..no_history() }).unwrap();
        // ... but not below the highest accepted serial, nor equal to it without the exact pending retry.
        let mut highest = older.clone();
        highest.highest_serial = manifest.serial + 1;
        highest.highest_digest = Sha256Digest([8; 32]);
        assert!(matches!(check(History { state: Some(highest), ..no_history() }), Err(Error::Rollback)));
        let mut equal = older.clone();
        equal.highest_serial = manifest.serial;
        equal.highest_digest = Sha256Digest([9; 32]);
        assert!(matches!(check(History { state: Some(equal), ..no_history() }), Err(Error::Rollback)));
        // A committed floor above the target refuses; maintenance and invalid states refuse.
        let mut newer = older.clone();
        newer.floor = next_patch();
        assert!(matches!(check(History { state: Some(newer), ..no_history() }), Err(Error::Rollback)));
        let mut maintenance = older.clone();
        maintenance.maintenance = true;
        assert!(check(History { state: Some(maintenance), ..no_history() }).is_err());
        let mut invalid = same.clone();
        invalid.schema_version = 2;
        assert!(check(History { state: Some(invalid), ..no_history() }).is_err());
        // The floor applies even when the state would accept.
        assert!(matches!(check(History { floor: Some(next_patch()), state: Some(older.clone()), ..no_history() }),
            Err(Error::Rollback)));

        // Trust ledger: must validate and cannot exist without its state.
        let pin = manifest_digest(&fixture.root_bytes);
        let ledger = TrustLedger::bootstrap(&fixture.root_bytes, &fixture.root_signatures, pin, NOW).unwrap();
        assert!(check(History { ledger: Some(ledger.clone()), ..no_history() }).is_err());
        check(History { ledger: Some(ledger.clone()), state: Some(same.clone()), ..no_history() }).unwrap();
        let mut broken = ledger;
        broken.pin = Sha256Digest([0; 32]);
        assert!(check(History { ledger: Some(broken), state: Some(same), ..no_history() }).is_err());
    }

    #[test]
    fn upgrade_without_state_is_only_the_first_migration_of_a_floored_host() {
        use BootstrapMode::{Install, Rollback, Upgrade};
        let fixture = build(Architecture::X86_64, None, version());
        let manifest = &fixture.manifest;
        let mine = candidate(&fixture);
        let check = |history: History, mode, resume| check_history(&history, mode, resume, manifest, &mine, NOW);
        let same = || crate::linux::trust_init::initial_state(manifest, mine.digest);
        // No version floor = not an identified single-binary host: refused, whatever else exists.
        assert!(matches!(check(no_history(), Upgrade, None), Err(Error::Maintenance)));
        assert!(matches!(check(History { high_water: Some(mine), ..no_history() }, Upgrade, Some(mine.digest)),
            Err(Error::Maintenance)));
        assert!(matches!(check(History { state: Some(same()), ..no_history() }, Upgrade, Some(mine.digest)),
            Err(Error::Maintenance)));
        // Exactly the plan's first migration: floor present, no updater state, no journal, no record.
        check(History { floor: Some(lower()), ..no_history() }, Upgrade, None).unwrap();
        // ... but not when a different bundle of this version was already accepted on this host.
        let other = HighWater { digest: Sha256Digest([7; 32]), bundle_sha256: Sha256Digest([8; 32]), ..mine };
        assert!(matches!(check(History { floor: Some(lower()), high_water: Some(other), ..no_history() }, Upgrade, None),
            Err(Error::Rollback)));
        // Install (fresh host) and rollback (bound to the journal) need no floor.
        check(no_history(), Install, None).unwrap();
        check(no_history(), Rollback, Some(mine.digest)).unwrap();
    }

    #[test]
    fn same_version_needs_the_recorded_digest_and_serial_never_goes_down() {
        use BootstrapMode::Upgrade;
        let fixture = build(Architecture::X86_64, None, version());
        let manifest = &fixture.manifest;
        let mine = candidate(&fixture);
        let check = |high_water: HighWater, floor: ReleaseVersion, resume: Option<Sha256Digest>|
            check_history(&History { floor: Some(floor), high_water: Some(high_water), ..no_history() }, Upgrade,
                resume, manifest, &mine, NOW);
        // Same version, different digest (an older root-authorised bundle of this version): refused,
        // even when its recorded serial is lower, and even as a "resume".
        for other in [HighWater { digest: Sha256Digest([7; 32]), bundle_sha256: Sha256Digest([8; 32]), ..mine },
            HighWater { digest: Sha256Digest([7; 32]), serial: mine.serial - 1, ..mine },
            HighWater { bundle_sha256: Sha256Digest([8; 32]), ..mine }] {
            assert!(matches!(check(other, lower(), None), Err(Error::Rollback)), "{other:?}");
            assert!(matches!(check(other, lower(), Some(mine.digest)), Err(Error::Rollback)), "{other:?}");
        }
        // A serial below (or equal to) the recorded high-water mark: refused even for a newer version.
        let earlier = HighWater { schema_version: 1, version: lower(), serial: mine.serial + 1,
            digest: Sha256Digest([9; 32]), bundle_sha256: Sha256Digest([9; 32]) };
        assert!(matches!(check(earlier, lower(), None), Err(Error::Rollback)));
        assert!(matches!(check(HighWater { serial: mine.serial, ..earlier }, lower(), None), Err(Error::Rollback)));
        // A recorded newer version refuses.
        assert!(matches!(check(HighWater { version: next_patch(), serial: mine.serial + 1, ..earlier }, lower(), None),
            Err(Error::Rollback)));
        // Strictly newer in version AND serial than the record: accepted.
        check(HighWater { serial: mine.serial - 1, ..earlier }, lower(), None).unwrap();
        // Same version + same digest: the resume of the same bundle — through the high-water record
        // (also with an equal floor) or the uncommitted journal entry.
        check(mine, lower(), None).unwrap();
        check(mine, version(), None).unwrap();
        check(mine, version(), Some(mine.digest)).unwrap();
        let journal = |floor| check_history(&History { floor: Some(floor), ..no_history() }, Upgrade,
            Some(mine.digest), manifest, &mine, NOW);
        journal(version()).unwrap();
        // Without any record of this digest the equal floor refuses.
        assert!(matches!(check_history(&History { floor: Some(version()), ..no_history() }, Upgrade, None, manifest,
            &mine, NOW), Err(Error::Rollback)));
        // Strict parsing of the record: exact fields, schema 1, no unknown or missing field.
        let bytes = serde_json::to_vec(&mine).unwrap();
        assert_eq!(parse_high_water(&bytes).unwrap(), mine);
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["extra"] = serde_json::json!(1);
        assert!(parse_high_water(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["schema_version"] = serde_json::json!(2);
        assert!(parse_high_water(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value.as_object_mut().unwrap().remove("bundle_sha256");
        assert!(parse_high_water(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(parse_high_water(bytes.get(..bytes.len() - 1).unwrap()).is_err());
        assert!(parse_high_water(b"").is_err());
    }

    #[test]
    fn high_water_only_moves_forward_and_a_failed_install_never_lowers_it() {
        let record = HighWater { schema_version: 1, version: version(), serial: 5, digest: Sha256Digest([1; 32]),
            bundle_sha256: Sha256Digest([2; 32]) };
        assert_eq!(raised_high_water(None, &record).unwrap(), Some(record));
        assert_eq!(raised_high_water(Some(&record), &record).unwrap(), None);
        let newer = HighWater { version: next_patch(), serial: 6, digest: Sha256Digest([3; 32]), ..record };
        assert_eq!(raised_high_water(Some(&record), &newer).unwrap(), Some(newer));
        let refused = [HighWater { serial: 4, ..record }, HighWater { version: lower(), serial: 6, ..record },
            HighWater { digest: Sha256Digest([9; 32]), ..record }, HighWater { serial: 6, digest: Sha256Digest([9; 32]), ..record },
            HighWater { version: next_patch(), ..record }, HighWater { version: lower(), serial: 1, ..record }];
        for attempt in refused {
            assert!(matches!(raised_high_water(Some(&record), &attempt), Err(Error::Rollback)), "{attempt:?}");
        }
        // The locked writer only ever stores what raised_high_water returns: a run that is
        // refused (or fails/aborts later) leaves the stored mark exactly as it was.
        let mut stored = Some(record);
        for attempt in refused {
            if let Ok(Some(next)) = raised_high_water(stored.as_ref(), &attempt) { stored = Some(next); }
        }
        assert_eq!(stored, Some(record));
        // And the version floor bounds are unaffected by any high-water record: a lower
        // version than the floor refuses even when the record would allow it.
        let fixture = build(Architecture::X86_64, None, version());
        let mine = candidate(&fixture);
        assert!(matches!(check_history(&History { floor: Some(next_patch()), high_water: Some(mine), ..no_history() },
            BootstrapMode::Upgrade, Some(mine.digest), &fixture.manifest, &mine, NOW), Err(Error::Rollback)));
    }

    #[test]
    fn rollback_only_undoes_the_journaled_uncommitted_generation() {
        use BootstrapMode::{Install, Rollback, Upgrade};
        let generation = Sha256Digest([3; 32]);
        let journal = |mode, committed| Journal { mode, generation, committed };
        // Without an in-progress journal, rollback has no authority at all.
        for facts in [false, true] { assert!(precondition_rule(None, facts, Rollback).is_err()); }
        // It resumes exactly the journal's generation (bound after step 8), never a committed run.
        for mode in [Install, Upgrade] {
            for facts in [false, true] {
                assert_eq!(precondition_rule(Some(&journal(mode, false)), facts, Rollback).unwrap(), Some(generation));
                assert!(precondition_rule(Some(&journal(mode, true)), facts, Rollback).is_err());
            }
        }
        // Install/upgrade keep their rules.
        assert_eq!(precondition_rule(None, false, Install).unwrap(), None);
        assert_eq!(precondition_rule(None, false, Upgrade).unwrap(), None);
        assert!(precondition_rule(None, true, Install).is_err());
        assert!(precondition_rule(None, true, Upgrade).is_err());
        assert!(precondition_rule(Some(&journal(Install, false)), true, Upgrade).is_err());
        assert!(precondition_rule(Some(&journal(Upgrade, false)), true, Install).is_err());
        assert_eq!(precondition_rule(Some(&journal(Upgrade, false)), true, Upgrade).unwrap(), Some(generation));
    }

    #[test]
    fn acceptance_waits_for_the_version_lock_and_rechecks_a_floor_raised_meanwhile() {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let lock_path = temp.path().join(VERSION_LOCK);
        let floor_path = temp.path().join(VERSION_FLOOR);
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&lock_path).unwrap();
        std::fs::write(&floor_path, crate::version_text(lower())).unwrap();
        let fixture = build(Architecture::X86_64, None, version());
        let mine = candidate(&fixture);
        // The production flock (`flock_until`), each time on a fresh open file description.
        let lock = |wait: Duration| -> Result<File> {
            let file = File::open(&lock_path)?;
            flock_until(&file, Instant::now() + wait)?;
            Ok(file)
        };
        let read = |_: &File| -> Result<History> {
            Ok(History { floor: Some(parse_version_file(&std::fs::read(&floor_path)?)?), ..no_history() })
        };
        let accept = |history: &History| check_history(history, BootstrapMode::Upgrade, None, &fixture.manifest, &mine, NOW);
        // Nobody holds the lock: a floor below the bundle is accepted at once.
        drop(accept_locked(|| lock(Duration::from_secs(5)), read, accept).unwrap());
        // A legacy writer holds the version lock (the CLI's `version_lock`: LOCK_EX) ...
        let holder = File::open(&lock_path).unwrap();
        // SAFETY: scalar flock on a live descriptor.
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);
        // ... so a bounded acquisition refuses (Busy) instead of proceeding unlocked ...
        assert!(matches!(accept_locked(|| lock(Duration::from_millis(200)), read, accept), Err(Error::Busy)));
        // ... and acceptance blocks until the writer, which raises the floor above this
        // bundle while it holds the lock, releases it; the re-read then refuses.
        let hold = Duration::from_millis(400);
        let raised = floor_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(hold);
            std::fs::write(&raised, crate::version_text(next_patch())).unwrap();
            drop(holder);
        });
        let started = Instant::now();
        let result = accept_locked(|| lock(Duration::from_secs(30)), read, accept);
        assert!(started.elapsed() >= hold / 2, "acceptance did not wait for the version lock");
        assert!(matches!(result, Err(Error::Rollback)));
        writer.join().unwrap();
    }

    #[test]
    fn state_root_appearing_after_the_locks_were_taken_refuses_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("updated-state");
        let fixture = build(Architecture::X86_64, None, version());
        let mine = candidate(&fixture);
        let accepted = std::cell::Cell::new(false);
        // The guard records what `AcceptanceLocks::acquire` decides: whether the state root
        // existed (and so had its update.lock taken) when the locks were taken.
        let lock = || -> Result<bool> { Ok(present(&state_root)) };
        let accept = |history: &History| -> Result<()> {
            check_history(history, BootstrapMode::Install, None, &fixture.manifest, &mine, NOW)?;
            accepted.set(true);
            Ok(())
        };
        // Absent when locked and after the read: a fresh host, accepted.
        let unchanged = |locked: &bool| read_unless_state_root_appeared(*locked, &state_root, || Ok(no_history()));
        assert!(!accept_locked(lock, unchanged, accept).unwrap());
        assert!(accepted.replace(false));
        // Another process creates the state root after acceptance's lock-time check (here
        // while the history is re-read) without serialising with it: refused, nothing is
        // decided or recorded.
        let appearing = |locked: &bool| read_unless_state_root_appeared(*locked, &state_root, || {
            std::fs::create_dir(&state_root)?;
            Ok(no_history())
        });
        assert!(matches!(accept_locked(lock, appearing, accept), Err(Error::Busy)));
        assert!(!accepted.get());
        // Present when the locks were taken (its update.lock is held then): accepted.
        assert!(accept_locked(lock, unchanged, accept).unwrap());
        assert!(accepted.replace(false));
        // The re-check after the decision (`stage`, before anything is staged) catches a
        // root that appears after the read.
        std::fs::remove_dir(&state_root).unwrap();
        let locked = accept_locked(lock, unchanged, accept).unwrap();
        std::fs::create_dir(&state_root).unwrap();
        assert!(matches!(read_unless_state_root_appeared(locked, &state_root, || Ok(())), Err(Error::Busy)));
    }

    fn publish_receipt(fixture: &Fixture, mode: BootstrapMode) -> StageReceipt {
        let mine = candidate(fixture);
        StageReceipt { schema_version: 1, mode, manifest_digest: mine.digest, root_sha256: manifest_digest(&fixture.root_bytes),
            bundle_sha256: mine.bundle_sha256, bundle_size: fixture.image.len() as u64,
            authorization_sha256: manifest_digest(&fixture.side.authorization), verifier_pin_kind: PinKind::Test,
            verified_at_unix: NOW, artifacts: fixture.manifest.artifacts.clone() }
    }

    #[test]
    fn newer_bundle_accepted_between_receipt_and_publish_refuses_publish_and_creates_no_state() {
        let fixture = build(Architecture::X86_64, None, version());
        let mine = candidate(&fixture);
        let receipt = publish_receipt(&fixture, BootstrapMode::Install);
        let with = |high_water: Option<HighWater>| History { high_water, ..no_history() };
        // Still the newest accepted bundle: publishable.
        check_publish(&with(Some(mine)), &receipt, &fixture.manifest, NOW).unwrap();
        // Receipt A; another acceptance then staged a newer bundle and raised the mark.
        let newer = HighWater { schema_version: 1, version: next_patch(), serial: mine.serial + 1,
            digest: Sha256Digest([7; 32]), bundle_sha256: Sha256Digest([8; 32]) };
        assert!(matches!(check_publish(&with(Some(newer)), &receipt, &fixture.manifest, NOW), Err(Error::Rollback)));
        // The record must be exactly this candidate: missing, or any field different, refuses.
        for other in [None, Some(HighWater { bundle_sha256: Sha256Digest([8; 32]), ..mine }),
            Some(HighWater { digest: Sha256Digest([7; 32]), ..mine }), Some(HighWater { serial: mine.serial + 1, ..mine }),
            Some(HighWater { version: next_patch(), ..mine })] {
            assert!(check_publish(&with(other), &receipt, &fixture.manifest, NOW).is_err(), "{other:?}");
        }
        // check_history still applies (an upgrade receipt needs its version floor; a
        // floor raised above the bundle refuses), and a rollback receipt never publishes.
        let upgrade = publish_receipt(&fixture, BootstrapMode::Upgrade);
        assert!(check_publish(&with(Some(mine)), &upgrade, &fixture.manifest, NOW).is_err());
        check_publish(&History { floor: Some(lower()), ..with(Some(mine)) }, &upgrade, &fixture.manifest, NOW).unwrap();
        assert!(matches!(check_publish(&History { floor: Some(next_patch()), ..with(Some(mine)) }, &upgrade,
            &fixture.manifest, NOW), Err(Error::Rollback)));
        let rollback = publish_receipt(&fixture, BootstrapMode::Rollback);
        assert!(check_publish(&with(Some(mine)), &rollback, &fixture.manifest, NOW).is_err());

        // Ordering of create_state_root_locked: the re-check runs under the locks and the
        // state root is created only after it passed.
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("updated-state");
        let locked = std::cell::Cell::new(false);
        let lock = || -> Result<()> { locked.set(true); Ok(()) };
        let create = |_: ()| -> Result<()> { std::fs::create_dir(&state_root)?; Ok(()) };
        let refused = accept_then_create(lock, |_: &()| {
            assert!(locked.get(), "re-check ran before the locks were taken");
            check_publish(&with(Some(newer)), &receipt, &fixture.manifest, NOW)
        }, create);
        assert!(matches!(refused, Err(Error::Rollback)));
        assert!(!state_root.exists(), "a refused publish created the state root");
        accept_then_create(lock, |_: &()| check_publish(&with(Some(mine)), &receipt, &fixture.manifest, NOW), create)
            .unwrap();
        assert!(state_root.is_dir());
    }

    #[test]
    fn rollback_refuses_a_floor_raised_during_rollback_and_reverifies_under_the_locks() {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let lock_path = temp.path().join(VERSION_LOCK);
        let floor_path = temp.path().join(VERSION_FLOOR);
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&lock_path).unwrap();
        std::fs::write(&floor_path, crate::version_text(lower())).unwrap();
        let fixture = build(Architecture::X86_64, None, version());
        let mine = candidate(&fixture);
        // The production flock (`flock_until`), each time on a fresh open file description.
        let lock = |wait: Duration| -> Result<File> {
            let file = File::open(&lock_path)?;
            flock_until(&file, Instant::now() + wait)?;
            Ok(file)
        };
        // The in-progress run recorded this generation; the floor is re-read under the lock.
        let read = |_: &File| -> Result<History> {
            Ok(History { floor: Some(parse_version_file(&std::fs::read(&floor_path)?)?), high_water: Some(mine),
                ..no_history() })
        };
        let check = |history: &History| check_history(history, BootstrapMode::Rollback, Some(mine.digest),
            &fixture.manifest, &mine, NOW);
        // Uncontended: the stage is re-verified while the lock is still held (another open
        // file description cannot take it then).
        let held_during_reverify = |_: &File| -> Result<bool> {
            let probe = File::open(&lock_path)?;
            // SAFETY: scalar flock on a live descriptor.
            Ok(unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0)
        };
        assert!(rollback_locked(|| lock(Duration::from_secs(5)), read, check, held_during_reverify).unwrap());
        // A concurrent updater holds the version lock and raises the floor above this
        // generation: rollback waits, re-reads under the lock and refuses; the old
        // generation is never re-verified, so never executed.
        let holder = File::open(&lock_path).unwrap();
        // SAFETY: scalar flock on a live descriptor.
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);
        let hold = Duration::from_millis(400);
        let raised = floor_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(hold);
            std::fs::write(&raised, crate::version_text(next_patch())).unwrap();
            drop(holder);
        });
        let reverified = std::cell::Cell::new(false);
        let started = Instant::now();
        let result = rollback_locked(|| lock(Duration::from_secs(30)), read, check,
            |_: &File| -> Result<()> { reverified.set(true); Ok(()) });
        assert!(started.elapsed() >= hold / 2, "rollback did not wait for the version lock");
        assert!(matches!(result, Err(Error::Rollback)));
        assert!(!reverified.get());
        writer.join().unwrap();
    }

    // ------------------------------------------------------------ root (VM) tests

    pub(crate) fn host_fixture(updater: Option<Vec<u8>>) -> Fixture {
        build(super::super::bootstrap::host_architecture().unwrap(), updater, version())
    }
    /// A root 0700 inbox below a /root temporary directory (same mount as "/").
    pub(crate) fn root_config(temp: &Path, fixture: &Fixture, facts_root: PathBuf) -> Config {
        let inbox = temp.join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o700)).unwrap();
        let context = fixture.context(super::super::now().unwrap());
        let name = context.tar_name();
        std::fs::write(inbox.join(&name), &fixture.image).unwrap();
        std::fs::write(inbox.join(format!("{name}.minisig")), &fixture.side.minisig).unwrap();
        std::fs::write(inbox.join(format!("{name}.authorization.json")), &fixture.side.authorization).unwrap();
        std::fs::write(inbox.join(format!("{name}.authorization.json.signatures.json")),
            &fixture.side.authorization_signatures).unwrap();
        Config { context, paths: Paths { inbox: inbox.strip_prefix("/").unwrap().to_str().unwrap().to_owned(),
            protected: temp.join("protected"), facts_root, state_root: temp.join("updated-state") }, after_seal: None }
    }
    fn root_temp() -> tempfile::TempDir {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0, "requires root");
        tempfile::Builder::new().prefix("duduclaw-bootstrap-").tempdir_in("/root").unwrap()
    }
    fn write_journal(config: &Config, mode: &str, generation: &str, committed: bool) {
        let path = config.paths.protected.join(BOOTSTRAP_JOURNAL);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"schema_version": 1, "mode": mode,
            "generation": generation, "step": "verified", "committed": committed})).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    #[ignore = "requires root native Linux openat2/statx/memfd; writes only its own /root fixture tree"]
    fn iso_root_bootstrap_verify_stages_without_any_fact() {
        let before = facts_present(Path::new("/"));
        let temp = root_temp();
        let empty = temp.path().join("facts-root");
        std::fs::create_dir(&empty).unwrap();
        let fixture = host_fixture(None);
        let config = root_config(temp.path(), &fixture, empty.clone());
        let mut step = Step::Harden;
        let staged = verify_and_stage_with(&config, BootstrapMode::Install, &mut step).unwrap();
        let receipt = &staged.receipt;
        assert_eq!(receipt.bundle_sha256, manifest_digest(&fixture.image));
        assert_eq!(receipt.bundle_size, fixture.image.len() as u64);
        assert_eq!(receipt.manifest_digest, manifest_digest(&fixture.manifest_bytes));
        assert_eq!(receipt.root_sha256, manifest_digest(&fixture.root_bytes));
        assert_eq!(receipt.authorization_sha256, manifest_digest(&fixture.side.authorization));
        assert_eq!((receipt.mode, receipt.verifier_pin_kind), (BootstrapMode::Install, PinKind::Test));
        assert_eq!(receipt.artifacts, fixture.manifest.artifacts);
        // Verification and staging published no isolation fact (installed() stays as it was).
        assert_eq!(facts_present(Path::new("/")), before);
        let (reopened, files) = open_stage_at(&config.paths.protected, receipt.manifest_digest, &config.context).unwrap();
        assert_eq!(&reopened, receipt);
        assert_eq!(files.manifest, fixture.manifest);
        let stage_dir = config.paths.protected.join(STAGE_DIRECTORY).join(hex::encode(receipt.manifest_digest.0));
        use std::os::unix::fs::MetadataExt;
        for path in [config.paths.protected.clone(), config.paths.protected.join(STAGE_DIRECTORY), stage_dir.clone()] {
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!((metadata.uid(), metadata.mode() & 0o7777), (0, 0o700));
        }
        // Re-entrant: a later run re-verifies the whole stage and keeps the first receipt.
        let later = Config { context: fixture.context(config.context.now + 5),
            paths: Paths { inbox: config.paths.inbox.clone(), protected: config.paths.protected.clone(),
                facts_root: empty.clone(), state_root: config.paths.state_root.clone() }, after_seal: None };
        assert_eq!(&verify_and_stage_with(&later, BootstrapMode::Install, &mut step).unwrap().receipt, receipt);
        // Rollback without an in-progress journal has no authority, even for a staged generation.
        assert!(verify_and_stage_with(&later, BootstrapMode::Rollback, &mut step).is_err());
        assert_eq!(step, Step::Preconditions);
        // An R3 journal of the same generation resumes (rollback re-verifies the same
        // stage and keeps its receipt); another mode, another generation or a
        // committed run is refused.
        let generation = hex::encode(receipt.manifest_digest.0);
        write_journal(&later, "install", &generation, false);
        assert!(verify_and_stage_with(&later, BootstrapMode::Install, &mut step).is_ok());
        assert_eq!(&verify_and_stage_with(&later, BootstrapMode::Rollback, &mut step).unwrap().receipt, receipt);
        assert!(verify_and_stage_with(&later, BootstrapMode::Upgrade, &mut step).is_err());
        assert_eq!(step, Step::Preconditions);
        write_journal(&later, "install", &"00".repeat(32), false);
        assert!(verify_and_stage_with(&later, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::Preconditions);
        write_journal(&later, "install", &generation, true);
        assert!(verify_and_stage_with(&later, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::Preconditions);
        std::fs::write(later.paths.protected.join(BOOTSTRAP_JOURNAL), b"{not json").unwrap();
        assert!(verify_and_stage_with(&later, BootstrapMode::Rollback, &mut step).is_err());
        assert_eq!(step, Step::Preconditions);
        std::fs::remove_file(later.paths.protected.join(BOOTSTRAP_JOURNAL)).unwrap();
        // Any fact without a journal is refused before the inbox is even opened.
        std::fs::create_dir_all(empty.join("var/lib/duduclaw-updated")).unwrap();
        assert!(verify_and_stage_with(&later, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::Preconditions);
        std::fs::remove_dir(empty.join("var/lib/duduclaw-updated")).unwrap();
        // A changed staged artifact is never accepted and never overwritten.
        let gateway = stage_dir.join(STAGE_ARTIFACTS).join(&staged.gateway.id);
        let mut tampered = std::fs::read(&gateway).unwrap();
        tampered[0] ^= 1;
        std::fs::write(&gateway, &tampered).unwrap();
        assert!(verify_and_stage_with(&later, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::Stage);
        assert_eq!(std::fs::read(&gateway).unwrap(), tampered);
        assert_eq!(facts_present(Path::new("/")), before);
    }

    fn replace_bundle_after_seal(config: &Config) {
        let path = Path::new("/").join(&config.paths.inbox).join(config.context.tar_name());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replaced after the bundle was sealed").unwrap();
    }

    #[test]
    #[ignore = "requires root native Linux openat2/statx/memfd seals; writes only its own /root fixture tree"]
    fn iso_root_bootstrap_verify_rejects_swapped_file_after_hash() {
        let temp = root_temp();
        let empty = temp.path().join("facts-root");
        std::fs::create_dir(&empty).unwrap();
        let fixture = host_fixture(None);
        let mut config = root_config(temp.path(), &fixture, empty);
        config.after_seal = Some(replace_bundle_after_seal);
        let mut step = Step::Harden;
        // The pathname changes after sealing; the verified and staged bytes are the memfd's.
        let staged = verify_and_stage_with(&config, BootstrapMode::Install, &mut step).unwrap();
        assert_eq!(staged.receipt.bundle_sha256, manifest_digest(&fixture.image));
        // A new run reads the replaced pathname and refuses it.
        config.after_seal = None;
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::LegacySignature);
        // Step 3: owner/mode/link checks on the inbox and its members.
        let inbox = Path::new("/").join(&config.paths.inbox);
        let tar = inbox.join(config.context.tar_name());
        std::fs::remove_file(&tar).unwrap();
        std::fs::write(&tar, &fixture.image).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_ok());
        let alias = inbox.join("alias");
        std::fs::hard_link(&tar, &alias).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::OpenInbox);
        std::fs::remove_file(&alias).unwrap();
        let real = inbox.join("real.tar");
        std::fs::rename(&tar, &real).unwrap();
        std::os::unix::fs::symlink(&real, &tar).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::OpenInbox);
        std::fs::remove_file(&tar).unwrap();
        std::fs::rename(&real, &tar).unwrap();
        let minisig = inbox.join(format!("{}.minisig", config.context.tar_name()));
        std::fs::set_permissions(&minisig, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::OpenInbox);
        std::fs::set_permissions(&minisig, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::OpenInbox);
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_ok());
    }

    #[test]
    #[ignore = "requires root native Linux openat2/statx/memfd; writes only its own /root fixture tree"]
    fn iso_root_bootstrap_anti_rollback_floor_state_and_rollback_scope() {
        use std::os::unix::fs::symlink;
        let temp = root_temp();
        let empty = temp.path().join("facts-root");
        std::fs::create_dir(&empty).unwrap();
        let fixture = host_fixture(None);
        let config = root_config(temp.path(), &fixture, empty);
        let protected = config.paths.protected.clone();
        let stages = protected.join(STAGE_DIRECTORY);
        let mut step = Step::Harden;
        let private_file = |path: &Path, bytes: &[u8], mode: u32| {
            let _ = std::fs::remove_file(path);
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        std::fs::create_dir(&protected).unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o700)).unwrap();
        // The floor writer (CLI `version_lock`) creates its lock before any floor file.
        private_file(&protected.join(VERSION_LOCK), b"", 0o600);

        // Rollback with an uncommitted journal of this very generation but nothing
        // staged: refused at step 9 and nothing is created.
        let generation = manifest_digest(&fixture.manifest_bytes);
        write_journal(&config, "upgrade", &hex::encode(generation.0), false);
        assert!(verify_and_stage_with(&config, BootstrapMode::Rollback, &mut step).is_err());
        assert_eq!(step, Step::Stage);
        assert!(!stages.exists());
        std::fs::remove_file(protected.join(BOOTSTRAP_JOURNAL)).unwrap();

        // version-floor above the bundle: refused as a rollback before anything is staged.
        let floor = protected.join(VERSION_FLOOR);
        let pending = protected.join(VERSION_PENDING);
        private_file(&floor, crate::version_text(next_patch()).as_bytes(), 0o600);
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step), Err(Error::Rollback)));
        assert_eq!(step, Step::Manifest);
        assert!(!stages.exists());
        // Unsafe or malformed floor files refuse; they are never treated as absent.
        for (text, mode) in [(version_string(), 0o644), (version_string(), 0o400), (format!("{}\n", version_string()), 0o600),
            (format!("v{}", version_string()), 0o600), ("1".repeat(129), 0o600)] {
            private_file(&floor, text.as_bytes(), mode);
            assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_err(), "{text:?} {mode:o}");
            assert_eq!(step, Step::Manifest);
        }
        std::fs::remove_file(&floor).unwrap();
        private_file(&protected.join("floor-target"), version_string().as_bytes(), 0o600);
        symlink(protected.join("floor-target"), &floor).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_err());
        assert_eq!(step, Step::Manifest);
        std::fs::remove_file(&floor).unwrap();
        std::fs::remove_file(protected.join("floor-target")).unwrap();
        // version-pending reserves a version exactly like the floor.
        private_file(&pending, crate::version_text(next_patch()).as_bytes(), 0o600);
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Rollback)));
        std::fs::remove_file(&pending).unwrap();
        // Upgrade without the single-binary version floor is refused (no updater state either).
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Maintenance)));
        assert_eq!(step, Step::Manifest);
        // An equal floor with no record of this bundle for that version: a possible replay, refused.
        private_file(&floor, version_string().as_bytes(), 0o600);
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Rollback)));
        assert_eq!(step, Step::Manifest);
        assert!(!stages.exists() && !protected.join(BOOTSTRAP_HIGH_WATER).exists());
        // The first migration of a single-binary host (lower floor, no updater state) is accepted
        // and recorded; afterwards the equal floor resumes exactly this bundle.
        private_file(&floor, crate::version_text(lower()).as_bytes(), 0o600);
        let staged = verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).unwrap();
        assert_eq!(staged.receipt.manifest_digest, generation);
        assert!(protected.join(BOOTSTRAP_HIGH_WATER).exists());
        private_file(&floor, version_string().as_bytes(), 0o600);
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_ok());

        // Updater state committing another generation at the same serial: a replay, refused.
        let state_root = config.paths.state_root.clone();
        std::fs::create_dir(&state_root).unwrap();
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        // State writers (`files::Directory::lock`) create update.lock before state.json.
        private_file(&state_root.join(UPDATE_LOCK), b"", 0o600);
        let state_file = state_root.join(UPDATER_STATE);
        let replay = crate::linux::trust_init::initial_state(&fixture.manifest, Sha256Digest([7; 32]));
        private_file(&state_file, &serde_json::to_vec(&replay).unwrap(), 0o600);
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Rollback)));
        assert_eq!(step, Step::Manifest);
        // The generation the state already commits (an interrupted run of this bundle) resumes.
        let same = crate::linux::trust_init::initial_state(&fixture.manifest, generation);
        private_file(&state_file, &serde_json::to_vec(&same).unwrap(), 0o600);
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_ok());
        // A trust ledger must validate; a corrupt one refuses.
        let ledger_file = state_root.join(UPDATER_LEDGER);
        let ledger = TrustLedger::bootstrap(&fixture.root_bytes, &fixture.root_signatures,
            manifest_digest(&fixture.root_bytes), config.context.now).unwrap();
        private_file(&ledger_file, &serde_json::to_vec(&ledger).unwrap(), 0o600);
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_ok());
        private_file(&ledger_file, b"{\"schema_version\":1}", 0o600);
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_err());
        assert_eq!(step, Step::Manifest);
        // A ledger without the state it is bound to is incomplete.
        private_file(&ledger_file, &serde_json::to_vec(&ledger).unwrap(), 0o600);
        std::fs::remove_file(&state_file).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_err());
        assert_eq!(step, Step::Manifest);
    }

    #[test]
    #[ignore = "requires root native Linux openat2/statx/memfd; writes only its own /root fixture tree"]
    fn iso_root_bootstrap_high_water_recorded_before_exec_and_never_lowered() {
        use std::os::unix::fs::MetadataExt;
        let temp = root_temp();
        let empty = temp.path().join("facts-root");
        std::fs::create_dir(&empty).unwrap();
        let fixture = host_fixture(None);
        let config = root_config(temp.path(), &fixture, empty);
        let protected = config.paths.protected.clone();
        let record_path = protected.join(BOOTSTRAP_HIGH_WATER);
        let generation = manifest_digest(&fixture.manifest_bytes);
        let expected = HighWater { schema_version: 1, version: fixture.manifest.version, serial: fixture.manifest.serial,
            digest: generation, bundle_sha256: manifest_digest(&fixture.image) };
        let private_dir = |path: &Path| {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        let private_file = |path: &Path, bytes: &[u8]| {
            let _ = std::fs::remove_file(path);
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        let mut step = Step::Harden;
        // Abort right after acceptance: a foreign directory under this generation's stage
        // name makes step 9 refuse. Nothing is staged or executed, yet the acceptance is durable.
        private_dir(&protected);
        private_dir(&protected.join(STAGE_DIRECTORY));
        let foreign = protected.join(STAGE_DIRECTORY).join(hex::encode(generation.0));
        private_dir(&foreign);
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::Stage);
        assert_eq!(parse_high_water(&std::fs::read(&record_path).unwrap()).unwrap(), expected);
        let metadata = std::fs::symlink_metadata(&record_path).unwrap();
        assert_eq!((metadata.uid(), metadata.mode() & 0o7777, metadata.nlink()), (0, 0o600, 1));
        assert!(!protected.join(format!("{BOOTSTRAP_HIGH_WATER}.next")).exists());
        std::fs::remove_dir(&foreign).unwrap();
        // verify_and_stage_with is everything that precedes announce and execveat (`run`):
        // when it returns the record is already on disk, unchanged by the resume.
        verify_and_stage_with(&config, BootstrapMode::Install, &mut step).unwrap();
        assert_eq!(parse_high_water(&std::fs::read(&record_path).unwrap()).unwrap(), expected);
        // A higher recorded mark, another digest of this version, a corrupt or unknown-field
        // record: refused at step 8 and the stored record is left byte-for-byte untouched.
        let newer = HighWater { version: next_patch(), serial: expected.serial + 1, ..expected };
        let other = HighWater { digest: Sha256Digest([7; 32]), ..expected };
        let mut unknown: serde_json::Value = serde_json::to_value(expected).unwrap();
        unknown["extra"] = serde_json::json!(true);
        for bytes in [serde_json::to_vec(&newer).unwrap(), serde_json::to_vec(&other).unwrap(),
            b"{\"schema_version\":1}".to_vec(), serde_json::to_vec(&unknown).unwrap()] {
            private_file(&record_path, &bytes);
            assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
            assert_eq!(step, Step::Manifest);
            assert_eq!(std::fs::read(&record_path).unwrap(), bytes);
        }
        // A record that is not exactly root 0600 refuses as well.
        private_file(&record_path, &serde_json::to_vec(&expected).unwrap());
        std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_err());
        assert_eq!(step, Step::Manifest);
        std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Install, &mut step).is_ok());
    }

    #[test]
    #[ignore = "requires root native Linux openat2/statx/memfd and flock; writes only its own /root fixture tree"]
    fn iso_root_bootstrap_acceptance_serialises_with_floor_and_state_writers() {
        use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt};
        let temp = root_temp();
        let empty = temp.path().join("facts-root");
        std::fs::create_dir(&empty).unwrap();
        let fixture = host_fixture(None);
        let config = root_config(temp.path(), &fixture, empty);
        let protected = config.paths.protected.clone();
        let state_root = config.paths.state_root.clone();
        let generation = manifest_digest(&fixture.manifest_bytes);
        let staged = protected.join(STAGE_DIRECTORY).join(hex::encode(generation.0));
        let record = protected.join(BOOTSTRAP_HIGH_WATER);
        let floor = protected.join(VERSION_FLOOR);
        let version_lock = protected.join(VERSION_LOCK);
        let private_file = |path: &Path, bytes: &[u8], mode: u32| {
            let _ = std::fs::remove_file(path);
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let private_dir = |path: &Path| {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        let nothing_accepted = || !record.exists() && !staged.exists();
        private_dir(&protected);
        private_file(&version_lock, b"", 0o600);
        private_file(&floor, crate::version_text(lower()).as_bytes(), 0o600);
        // Long enough that the unlocked step-8 check has surely run before the writer acts.
        let hold = Duration::from_millis(1500);
        let mut step = Step::Harden;

        // 1. The single-binary updater (CLI `version_lock`: O_NOFOLLOW open, blocking
        //    LOCK_EX) holds the version lock and raises the floor above this bundle.
        let (locked, wait_locked) = std::sync::mpsc::channel();
        let writer = {
            let (lock_path, floor) = (version_lock.clone(), floor.clone());
            std::thread::spawn(move || {
                let lock = std::fs::OpenOptions::new().read(true).write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path).unwrap();
                // SAFETY: scalar flock on a live descriptor.
                assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
                locked.send(()).unwrap();
                std::thread::sleep(hold);
                let _ = std::fs::remove_file(&floor);
                std::fs::write(&floor, crate::version_text(next_patch())).unwrap();
                std::fs::set_permissions(&floor, std::fs::Permissions::from_mode(0o600)).unwrap();
                drop(lock);
            })
        };
        wait_locked.recv().unwrap();
        let started = Instant::now();
        // The unlocked check saw the lower floor; the re-read under the locks sees the raised one.
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Rollback)));
        assert_eq!(step, Step::Stage);
        assert!(started.elapsed() >= hold / 2, "acceptance did not wait for the version lock");
        writer.join().unwrap();
        assert!(nothing_accepted());

        // 2. An updater-state writer (`files::Directory::lock` on the state root) commits
        //    another generation at this serial while it holds update.lock.
        private_file(&floor, crate::version_text(lower()).as_bytes(), 0o600);
        private_dir(&state_root);
        let (locked, wait_locked) = std::sync::mpsc::channel();
        let writer = {
            let state_root = state_root.clone();
            let replay = crate::linux::trust_init::initial_state(&fixture.manifest, Sha256Digest([7; 32]));
            std::thread::spawn(move || {
                let dir = Directory::open(&state_root).unwrap();
                let lock = dir.lock().unwrap();
                locked.send(()).unwrap();
                std::thread::sleep(hold);
                dir.atomic_json(UPDATER_STATE, &replay).unwrap();
                drop(lock);
            })
        };
        wait_locked.recv().unwrap();
        let started = Instant::now();
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Rollback)));
        assert_eq!(step, Step::Stage);
        assert!(started.elapsed() >= hold / 2, "acceptance did not wait for the state lock");
        writer.join().unwrap();
        assert!(nothing_accepted());
        std::fs::remove_file(state_root.join(UPDATER_STATE)).unwrap();

        // 3. Lock files unlike the ones their writers create refuse; nothing is accepted.
        private_file(&protected.join("lock-target"), b"", 0o600);
        std::fs::remove_file(&version_lock).unwrap();
        symlink(protected.join("lock-target"), &version_lock).unwrap();
        assert!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).is_err());
        assert_eq!(step, Step::Stage);
        std::fs::remove_file(&version_lock).unwrap();
        std::fs::remove_file(protected.join("lock-target")).unwrap();
        private_file(&version_lock, b"", 0o644);
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Maintenance)));
        assert_eq!(step, Step::Stage);
        // Missing beside the floor it guards: the floor writer always creates it first.
        std::fs::remove_file(&version_lock).unwrap();
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Maintenance)));
        assert_eq!(step, Step::Stage);
        assert!(!version_lock.exists());
        private_file(&version_lock, b"", 0o600);
        // The state root's update.lock missing beside the state.json it guards.
        let same = crate::linux::trust_init::initial_state(&fixture.manifest, generation);
        private_file(&state_root.join(UPDATER_STATE), &serde_json::to_vec(&same).unwrap(), 0o600);
        std::fs::remove_file(state_root.join(UPDATE_LOCK)).unwrap();
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step), Err(Error::Maintenance)));
        assert_eq!(step, Step::Stage);
        assert!(nothing_accepted());
        std::fs::remove_file(state_root.join(UPDATER_STATE)).unwrap();

        // 4. Uncontended: accepted and staged; the lock files are the writers' own (root 0600).
        let receipt = verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).unwrap().receipt;
        assert_eq!(receipt.manifest_digest, generation);
        assert!(record.exists() && staged.exists());
        for lock in [version_lock.clone(), state_root.join(UPDATE_LOCK), protected.join(STAGE_DIRECTORY).join(UPDATE_LOCK)] {
            let metadata = std::fs::symlink_metadata(&lock).unwrap();
            assert!(metadata.file_type().is_file());
            assert_eq!((metadata.uid(), metadata.mode() & 0o7777, metadata.nlink()), (0, 0o600, 1));
        }

        // 5. Rollback takes the same locks: uncontended it re-verifies the staged
        //    generation; when an updater raises the floor while rollback waits for the
        //    version lock, the re-read under the locks refuses and nothing is executed.
        write_journal(&config, "upgrade", &hex::encode(generation.0), false);
        assert_eq!(verify_and_stage_with(&config, BootstrapMode::Rollback, &mut step).unwrap().receipt, receipt);
        let (locked, wait_locked) = std::sync::mpsc::channel();
        let writer = {
            let (lock_path, floor) = (version_lock.clone(), floor.clone());
            std::thread::spawn(move || {
                let lock = std::fs::OpenOptions::new().read(true).write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path).unwrap();
                // SAFETY: scalar flock on a live descriptor.
                assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
                locked.send(()).unwrap();
                std::thread::sleep(hold);
                let _ = std::fs::remove_file(&floor);
                std::fs::write(&floor, crate::version_text(next_patch())).unwrap();
                std::fs::set_permissions(&floor, std::fs::Permissions::from_mode(0o600)).unwrap();
                drop(lock);
            })
        };
        wait_locked.recv().unwrap();
        let started = Instant::now();
        assert!(matches!(verify_and_stage_with(&config, BootstrapMode::Rollback, &mut step), Err(Error::Rollback)));
        assert_eq!(step, Step::Stage);
        assert!(started.elapsed() >= hold / 2, "rollback did not wait for the version lock");
        writer.join().unwrap();
        std::fs::remove_file(protected.join(BOOTSTRAP_JOURNAL)).unwrap();
        private_file(&floor, crate::version_text(lower()).as_bytes(), 0o600);

        // 6. The state-root creator (trust_init) is serialised with acceptance by locks that
        //    exist before the root does: while the CLI holds the version lock it creates
        //    nothing; free, it creates the root with its update.lock, and an acceptance
        //    started while it holds its locks waits for it, then takes the new root's lock.
        std::fs::remove_file(state_root.join(UPDATE_LOCK)).unwrap();
        std::fs::remove_dir(&state_root).unwrap();
        let holder = std::fs::OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&version_lock).unwrap();
        // SAFETY: scalar flock on a live descriptor.
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert!(matches!(create_state_root_locked_until(&protected, &state_root,
            Instant::now() + Duration::from_millis(300), || Ok(())), Err(Error::Busy)));
        assert!(!state_root.exists());
        drop(holder);
        let creator = create_state_root_locked_until(&protected, &state_root,
            Instant::now() + Duration::from_secs(5), || Ok(())).unwrap();
        let metadata = std::fs::symlink_metadata(&state_root).unwrap();
        assert_eq!((metadata.uid(), metadata.mode() & 0o7777), (0, 0o700));
        assert!(state_root.join(UPDATE_LOCK).exists());
        let release = std::thread::spawn(move || {
            std::thread::sleep(hold);
            drop(creator);
        });
        let started = Instant::now();
        assert_eq!(verify_and_stage_with(&config, BootstrapMode::Upgrade, &mut step).unwrap().receipt, receipt);
        assert!(started.elapsed() >= hold / 2, "acceptance did not wait for the state-root creator");
        release.join().unwrap();

        // 7. Publish (trust_init::publish = create_state_root_locked + reverify_for_publish):
        //    receipt A is still the newest acceptance, so the gate passes. Then another
        //    acceptance raises the high-water mark (a newer bundle staged) while publish
        //    waits for the version lock: the re-check under the locks refuses publishing
        //    A, and no state root is created; with an existing root nothing is written.
        std::fs::remove_file(state_root.join(UPDATE_LOCK)).unwrap();
        std::fs::remove_dir(&state_root).unwrap();
        let publish_gate = |wait: Duration| create_state_root_locked_until(&protected, &state_root,
            Instant::now() + wait, || reverify_for_publish(&protected, &state_root, &receipt, &config.context));
        drop(publish_gate(Duration::from_secs(5)).unwrap());
        std::fs::remove_file(state_root.join(UPDATE_LOCK)).unwrap();
        std::fs::remove_dir(&state_root).unwrap();
        let newer = HighWater { schema_version: 1, version: next_patch(), serial: fixture.manifest.serial + 1,
            digest: Sha256Digest([7; 32]), bundle_sha256: Sha256Digest([8; 32]) };
        let (locked, wait_locked) = std::sync::mpsc::channel();
        let writer = {
            let (lock_path, record) = (version_lock.clone(), record.clone());
            std::thread::spawn(move || {
                let lock = std::fs::OpenOptions::new().read(true).write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path).unwrap();
                // SAFETY: scalar flock on a live descriptor.
                assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
                locked.send(()).unwrap();
                std::thread::sleep(hold);
                let _ = std::fs::remove_file(&record);
                std::fs::write(&record, serde_json::to_vec(&newer).unwrap()).unwrap();
                std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
                drop(lock);
            })
        };
        wait_locked.recv().unwrap();
        let started = Instant::now();
        assert!(matches!(publish_gate(Duration::from_secs(30)), Err(Error::Rollback)));
        assert!(started.elapsed() >= hold / 2, "publish did not wait for the version lock");
        writer.join().unwrap();
        assert!(!state_root.exists(), "a refused publish created the state root");
        private_dir(&state_root);
        assert!(matches!(publish_gate(Duration::from_secs(5)), Err(Error::Rollback)));
        let left: Vec<_> = std::fs::read_dir(&state_root).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from(UPDATE_LOCK)]);
    }

    /// Fork, put the pty on fd 0/1/2, then exec the sealed memfd through the
    /// production `exec_sealed`. `extra_fd` first duplicates stdin to fd 9
    /// without close-on-exec.
    fn exec_probe(memfd: RawFd, args: Vec<CString>, tty: &File, extra_fd: bool) -> bool {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let mut command = Command::new("/usr/bin/false");
        command.env_clear().stdin(Stdio::from(tty.try_clone().unwrap()))
            .stdout(Stdio::from(tty.try_clone().unwrap())).stderr(Stdio::from(tty.try_clone().unwrap()));
        let env = CString::new("LANG=C").unwrap();
        // SAFETY: the closure only calls dup2 and the allocation-free exec_sealed.
        unsafe {
            command.pre_exec(move || {
                if extra_fd && libc::dup2(0, 9) != 9 { return Err(std::io::Error::last_os_error()); }
                let mut argv: [*const libc::c_char; 16] = [std::ptr::null(); 16];
                for (slot, value) in argv.iter_mut().zip(args.iter().take(15)) { *slot = value.as_ptr(); }
                let envp = [env.as_ptr(), std::ptr::null()];
                Err(exec_sealed(memfd, &argv, &envp))
            });
        }
        command.status().unwrap().success()
    }

    #[test]
    #[ignore = "requires root native Linux memfd seals, execveat and close_range (kernel >= 5.11) and /usr/bin/test"]
    fn iso_root_bootstrap_execveat_memfd_keeps_tty() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0, "requires root");
        let bytes = std::fs::read("/usr/bin/test").unwrap();
        let artifact = ReleaseArtifact { id: "probe".into(), sha256: manifest_digest(&bytes),
            size_bytes: bytes.len() as u64, mode: 0o755, install_target: InstallTarget::Gateway };
        let memfd = sealed_executable(File::open("/usr/bin/test").unwrap(), &artifact).unwrap();
        // Sealed after verification: no write, growth or truncation.
        assert!((&memfd).write_all(b"x").is_err());
        assert!(memfd.set_len(1).is_err());
        let mut wrong = artifact.clone();
        wrong.sha256 = Sha256Digest([0; 32]);
        assert!(sealed_executable(File::open("/usr/bin/test").unwrap(), &wrong).is_err());
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: out-pointers to live integers; no name/termios/winsize requested.
        assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        // SAFETY: openpty returned two fresh descriptors owned here.
        let (_master, tty) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        let args = |list: &[&str]| list.iter().map(|value| CString::new(*value).unwrap()).collect::<Vec<_>>();
        // fd 0/1/2 still refer to the terminal in the exec'd image.
        assert!(exec_probe(memfd.as_raw_fd(), args(&["test", "-t", "0", "-a", "-t", "1", "-a", "-t", "2"]), &tty, false));
        // Every other inherited descriptor was closed by exec.
        assert!(!exec_probe(memfd.as_raw_fd(), args(&["test", "-t", "9"]), &tty, true));
    }
}
