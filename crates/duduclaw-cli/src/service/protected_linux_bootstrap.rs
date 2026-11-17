//! v1.83 isolation bootstrap: fresh-host install orchestration (plan §4.3, R3).
//!
//! Entered only as `duduclaw service bootstrap-continue --protected-linux`,
//! executed by `duduclaw-bootstrap` from a sealed memfd holding the staged,
//! verified gateway (R1). Steps S0–S9 are a resumable state machine recorded
//! in the root-owned journal `/var/lib/duduclaw-protected/isolation-bootstrap.json`
//! (root 0600, strict, `deny_unknown_fields`). Each step is first recorded as
//! entered, then performed, read back and recorded as completed; every write is
//! temp file → fsync → renameat2 → directory fsync through the verified dirfd of
//! the protected root (`files::Directory::atomic_json`). A crash at any point
//! resumes with the same command; every step is idempotent and reads back what
//! an earlier attempt left.
//!
//! The journal is bound to one generation and one host (SHA-256 of
//! `/etc/machine-id`); another generation's or host's journal is refused.
//! Its cursor is only a HINT (review F1): every invocation re-runs S0 and the
//! complete, side-effect-free S1 preflight (review F6), then re-establishes
//! every step the journal claims complete against the real host (`Steps::holds`:
//! published files with their digest/owner/mode, accounts, unit files and
//! their enabled/active state, policy files, the launcher receipt, trust state
//! bound to this generation) and re-runs from the first step whose invariants
//! do not hold. S9 independently requires the readiness check in the same run.
//! Every object a step creates is first recorded as an intent and then with
//! its identity in the creation ledger (review F4, `protected_linux_ledger.rs`);
//! rollback removes nothing else.
//!
//! LOCK ORDER (consistent with R1's `bootstrap_verify` LOCK ORDER):
//!   0. `<protected>/isolation-bootstrap.lock` — this run (and rollback) only;
//!      taken first, while holding nothing, LOCK_NB, for the whole run;
//!   1. `<protected>/bootstrap/update.lock` — R1's stage lock;
//!   2. `<protected>/version-lock` — the CLI's `version_lock()`;
//!   3. `/var/lib/duduclaw-updated/update.lock` — updater state.
//! Every step except S4(a) runs under lock 2 (`with_version_lock`), so a
//! concurrent `service install|upgrade|uninstall --protected-linux` (which take
//! lock 2 and refuse while this journal is uncommitted) never interleaves with
//! a step. S4(a) calls `trust_init::publish`, which itself takes 1 → 2 → 3, and
//! therefore runs while this process holds only lock 0: holding lock 2 there
//! would make publish wait for itself (flock is per open file description) and
//! be refused `Busy` after 60 s. Lock 3 is taken only beneath lock 2
//! (`parse_runtime_tree`). No path takes an earlier lock while holding a later one.
//!
//! Nothing here runs a shell: every tool goes through `command()` (fixed
//! argv, cleared environment, bounded output and time); subprocess output is
//! parsed, never printed. Privileged paths are opened without following links
//! (`files::Directory`, `O_NOFOLLOW` dirfds, `trusted`/`parents` checks).
use super::*;
use std::{ffi::{CStr, CString}, io::{Read, Seek, SeekFrom}, net::{IpAddr, SocketAddr}, os::fd::FromRawFd,
    time::{Duration, Instant}};
use duduclaw_isolation::{egress::{DnsRoute, ProfileId}, ids::Sha256Digest,
    update::{InstallTarget, ReleaseArtifact, ReleaseManifest}};
use duduclaw_updated::linux::{bootstrap_verify::{self, StageReceipt}, files::Directory};
use serde::{Deserialize, Serialize};
use super::ledger::{self, Kind, LiveRecorder};

// ── Fixed locations ───────────────────────────────────────────────────────

const PROTECTED_ROOT: &str = bootstrap_verify::PROTECTED_ROOT;
const JOURNAL_FILE: &str = bootstrap_verify::BOOTSTRAP_JOURNAL;
const RUN_LOCK_FILE: &str = "isolation-bootstrap.lock";
const STAGE_DIRECTORY: &str = bootstrap_verify::STAGE_DIRECTORY;
const MAX_JOURNAL_BYTES: usize = 64 * 1024;
/// The gateway account's home: root:root 0711, holding only the private
/// data directory `.duduclaw` (duduclaw 0700, `layout::migrated_home_is_private`).
pub(super) const GATEWAY_HOME: &str = "/var/lib/duduclaw";
const GATEWAY_DATA: &CStr = c".duduclaw";
const LICENSE_NAME: &CStr = c"license.json";
const LICENSE_TEMPORARY: &CStr = c".license.json.bootstrap";
/// Root 0700 download directory of the bundle (design §8 step 2); the TEAM
/// license is read from here when the gateway has none yet (Q9).
const INBOX: &str = bootstrap_verify::MIGRATE_INBOX;
const INBOX_LICENSE: &str = "license.json";
/// The nightly update requester (Q11: enabled after install). Same files the
/// standalone `scripts/linux-protected/auto-upgrade/install.sh` installs; the
/// helper only runs `/usr/libexec/duduclaw-updated-launch --check-latest`.
pub(super) const NIGHTLY_HELPER: &str = "/usr/local/sbin/duduclaw-auto-upgrade";
const NIGHTLY_HELPER_TEXT: &str = include_str!("../../../../scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade");
pub(super) const NIGHTLY_UNITS: [(&str, &str); 2] = [
    ("duduclaw-auto-upgrade.service", include_str!("../../../../scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade.service")),
    ("duduclaw-auto-upgrade.timer", include_str!("../../../../scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade.timer")),
];
pub(super) const NIGHTLY_TIMER: &str = "duduclaw-auto-upgrade.timer";
/// Root-owned spawnd anchors the installer must precreate (spawnd README
/// "installed publication consumers"): 0700 directories and the root-only
/// host publication with no entitlements yet.
const SPAWND_ROOT: &str = "/var/lib/duduclaw-spawnd";
const SPAWND_ANCHORS: [&str; 3] = ["etc-state", "projections", "snapshots"];
const SPAWND_PUBLICATION: &str = "publication.json";
/// The signed isolation policy artifact (`duduclaw-spawnd` installed.rs).
const POLICY_REGISTRY_ID: &str = "isolation-policy-v1";
/// Egress profile values the signed policy does not carry (see `egress_document`).
pub(super) const EGRESS_PORTS: [u16; 1] = [443];
pub(super) const EGRESS_MAX_RUNS: u16 = 32;
pub(super) const EGRESS_RUN_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub(super) const EGRESS_RUN_TOTAL_CONNECTIONS: u32 = 4096;
pub(super) const MAX_HOST_PUBLIC_ADDRESSES: usize = 64;
/// Isolation services in start order (plan S6); the broker socket only on the broker route.
const SERVICE_ORDER: [&str; 7] = ["duduclaw-egress.socket", "duduclaw-dns-broker.socket", "duduclaw-parse.socket",
    "duduclaw-spawnd.socket", "duduclaw-spawnd.service", "duduclaw-egress.service", "duduclaw-updated.service"];
/// The units the isolated gateway unit binds to
/// (`duduclaw_core::root_key::recovery::ISOLATED_GATEWAY_UNIT_DEPENDENCIES`)
/// that this tree's layout installs: enabled after [`SERVICE_ORDER`] at S6,
/// required active on resume, stopped and disabled by rollback. Empty in this
/// tree: `duduclaw-isolation-ready.service` and its unit file come with the R6
/// branch; merging adds the name here and the file to `layout::UNITS` (a unit
/// test requires every entry to be a layout unit). Never invent a unit here.
pub(super) const GATEWAY_READY_UNITS: &[&str] = &[];
/// spawnd's runtime directory (systemd creates it at S6; a ledgered tree).
pub(super) const SPAWND_RUNTIME: &str = "/run/duduclaw-spawnd";
/// The trees R1's `trust_init::publish` creates at S4(a).
const TRUST_TREES: [&str; 3] = ["/opt/duduclaw-isolated", "/etc/duduclaw-updated", "/var/lib/duduclaw-updated"];
/// The trees of `layout::DIRECTORIES` created at S4(c).
const LAYOUT_TREES: [&str; 3] = ["/var/lib/duduclaw-spawnd", "/var/log/duduclaw-spawnd", "/var/lib/duduclaw-migrate"];
/// spawnd's root installation receipt (`duduclaw-spawnd` installed.rs, `RECEIPTS`/`installation.json`).
const INSTALLATION_RECEIPT: &str = "/run/duduclaw-isolation-evidence/installation.json";
const MACHINE_ID: &str = "/etc/machine-id";
const READY_WAIT: Duration = Duration::from_secs(30);

// ── Administrator text (zh-TW; no internal state, never subprocess output) ──

pub(super) const MIGRATE_FROM_REFUSED: &str = "從 v1.83.0 起，受保護安裝不再支援 --migrate-from：舊資料必須先經過官方遷移流程審查才能帶入。全新主機請使用 duduclaw-bootstrap install；已有單一執行檔受保護安裝的主機請使用 duduclaw-bootstrap upgrade。未變更任何東西。";
pub(super) const FRESH_INSTALL_REFUSED: &str = "從 v1.83.0 起，全新主機的受保護安裝只能由已獨立驗證的 duduclaw-bootstrap install 進行。未變更任何東西。";
const IN_PROGRESS: &str = "這台主機有尚未完成的隔離安裝。請以 duduclaw-bootstrap install 續接，或以 duduclaw-bootstrap rollback 回退；完成或回退前不能執行其他受保護安裝、升級或移除。";
const JOURNAL_DAMAGED: &str = "隔離安裝日誌無法驗證（可能已損毀或被修改），已停止。請勿刪除日誌；請保留現況並聯絡原廠協助人工處理。";
const RUN_BUSY: &str = "另一個隔離安裝或回退正在進行，請等它結束後再執行。";
const ALREADY_COMMITTED: &str = "這台主機的隔離安裝已經完成。請以 sudo duduclaw service readiness --protected-linux 檢查狀態。";
const OTHER_GENERATION: &str = "這台主機上進行中的隔離安裝屬於另一個安裝包或模式；請先執行 duduclaw-bootstrap rollback 回退，再安裝新的安裝包。";
const UPGRADE_NOT_HERE: &str = "單一執行檔主機的升級（duduclaw-bootstrap upgrade）尚未在此版本提供；未變更任何東西。";
const NOT_STAGED_GATEWAY: &str = "此命令只能由 duduclaw-bootstrap 以已驗證的 Gateway 執行；請改用 duduclaw-bootstrap install。";
const STAGE_UNVERIFIED: &str = "暫存的安裝包無法重新驗證，已停止。請重新下載安裝包並以 duduclaw-bootstrap install 執行。";
const OPTIONS_CHANGED: &str = "續接時的 DNS 或公開位址設定與第一次執行不同。請以第一次的設定續接；如需變更，請先執行 duduclaw-bootstrap rollback。";
const HARDEN_FAILED: &str = "無法以受限的 root 權限執行安裝。";
const LICENSE_MISSING: &str = "員工隔離只支援團隊版：找不到團隊版授權。請把團隊版授權檔 license.json 放進 /root/duduclaw-migrate/（root 擁有、權限 0600 或 0644）後重新執行。";
const LICENSE_NOT_TEAM: &str = "員工隔離只支援團隊版：提供的授權不是本機有效的團隊版授權。";
const POLICY_REFUSED: &str = "安裝包的簽章政策沒有可用的對外連線設定，或與已驗證的政策不符。";
const SYSTEMCTL_FAILED: &str = "systemd 指令失敗或回應無法判讀。";
const OTHER_HOST: &str = "隔離安裝日誌屬於另一台主機（或這台主機的識別碼已改變），已停止。請勿刪除日誌；請保留現況並聯絡原廠協助人工處理。";
const HOST_UNKNOWN: &str = "無法讀取這台主機的識別碼 /etc/machine-id（必須是 root 擁有的一般檔案）。";
const HOST_DNS_CHANGED: &str = "主機的 DNS 狀態（systemd-resolved 是否可用）與第一次執行時不同。請恢復原狀後續接，或先執行 duduclaw-bootstrap rollback。";
const PUBLISHED_MISMATCH: &str = "已發布的內容與這次安裝不符。";
const LICENSE_MODE: &str = "團隊版授權檔 license.json 的權限必須是 0600 或 0644。";
const IDENTITY_ALIAS: &str = "服務帳號或群組的識別碼與其他帳號或群組重複、帶有額外成員，或帳號資料無法完整列出；其他帳號會因此取得相同的檔案權限，已停止。請先移除重複的帳號或群組。";
const STORAGE_THIN: &str = "/var/lib 位於精簡配置（thin provisioning）、快照或快取的區塊裝置上；員工儲存空間需要預先配置的磁碟。";
const STORAGE_UNDETERMINED: &str = "無法確定 /var/lib 的完整底層區塊裝置鏈（或含有不支援的裝置類型），無法證明它不是精簡配置的儲存。";
const RECEIPT_PRODUCER_MISSING: &str = "隔離執行服務需要 root 量測收據 /run/duduclaw-isolation-evidence/installation.json（員工帳號 SSH 實測、trampoline 自我測試、DNS 實測），但這個版本沒有產生它的正式程式，設計也尚未決定由誰產生。安裝停在 S6，尚未啟動任何隔離服務；可以執行 duduclaw-bootstrap rollback 回退，或聯絡原廠。";

// ── Journal ───────────────────────────────────────────────────────────────

/// Plan §4.3 step names, in order. `Rollback` marks a rollback in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Step {
    Verified, HostPreflight, StopGateway, ProcessesClear, PublishPlatform, DnsPolicy, StartServices,
    Readiness, StartGateway, Commit, Rollback,
}

impl Step {
    const ORDER: [Step; 10] = [Step::Verified, Step::HostPreflight, Step::StopGateway, Step::ProcessesClear,
        Step::PublishPlatform, Step::DnsPolicy, Step::StartServices, Step::Readiness, Step::StartGateway, Step::Commit];
    /// The step after `self` in [`Step::ORDER`]; none after `Commit` or `Rollback`.
    pub(super) fn next(self) -> Option<Step> {
        let position = Self::ORDER.iter().position(|step| *step == self)?;
        Self::ORDER.get(position + 1).copied()
    }
    pub(super) fn number(self) -> u8 {
        Self::ORDER.iter().position(|step| *step == self).and_then(|index| u8::try_from(index).ok()).unwrap_or(u8::MAX)
    }
    fn label(self) -> &'static str {
        match self {
            Step::Verified => "驗證暫存的安裝包", Step::HostPreflight => "主機預檢", Step::StopGateway => "確認 Gateway 未執行",
            Step::ProcessesClear => "確認服務帳號沒有程序", Step::PublishPlatform => "發布隔離平台",
            Step::DnsPolicy => "DNS 與對外連線設定", Step::StartServices => "啟動隔離服務",
            Step::Readiness => "綜合就緒檢查", Step::StartGateway => "啟用 Gateway", Step::Commit => "提交",
            Step::Rollback => "回退",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase { Entered, Completed }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// `Upgrade` journals are written by R5; this package only reads and refuses them.
#[allow(dead_code)]
pub(super) enum JournalMode { Install, Upgrade }

/// The host before the bootstrap (plan §4.3). A fresh install has no prior
/// single-binary service; the upgrade fields are filled only by R5.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
// The upgrade fields are read by R5's rollback; a fresh install compares the whole record.
#[allow(dead_code)]
pub(super) struct Prior {
    pub(super) single_binary: bool,
    pub(super) floor: Option<String>,
    pub(super) gateway_active: bool,
    pub(super) gateway_enabled: bool,
    pub(super) previous_binary_sha256: Option<String>,
}

impl Prior {
    pub(super) fn fresh() -> Self {
        Self { single_binary: false, floor: None, gateway_active: false, gateway_enabled: false, previous_binary_sha256: None }
    }
}

/// The DNS decision of S1, fixed for the whole run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dns {
    pub(super) route: DnsRoute,
    pub(super) upstream: Option<SocketAddr>,
    /// `--host-public-address` values (cloud 1:1 NAT), written to egress.json at S5.
    pub(super) host_public_addresses: Vec<IpAddr>,
}

/// `isolation-bootstrap.json`. R1's verifier reads `schema_version`, `mode`,
/// `generation` and `committed` from the same document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub(super) schema_version: u16,
    pub(super) mode: JournalMode,
    /// The staged generation, 64 lowercase hex digits.
    pub(super) generation: String,
    /// [`host_digest`] of the host the journal was created on (review F1).
    pub(super) host: String,
    pub(super) step: Step,
    pub(super) phase: Phase,
    pub(super) started_at_unix: u64,
    pub(super) prior: Prior,
    pub(super) dns: Dns,
    pub(super) committed: bool,
}

/// Exactly 64 lowercase hex digits (R1 `parse_digest`).
pub(super) fn parse_generation(text: &str) -> Option<Sha256Digest> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
    let bytes: [u8; 32] = hex::decode(text).ok()?.try_into().ok()?;
    Some(Sha256Digest(bytes))
}

impl Journal {
    pub(super) fn new(generation: &str, host: &str, dns: Dns, now: u64) -> Self {
        Self { schema_version: 1, mode: JournalMode::Install, generation: generation.to_owned(), host: host.to_owned(),
            step: Step::HostPreflight, phase: Phase::Completed, started_at_unix: now, prior: Prior::fresh(), dns,
            committed: false }
    }

    /// Strict parse plus every cross-field rule. Anything else is damage.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_JOURNAL_BYTES { return Err(refused(JOURNAL_DAMAGED)); }
        let journal: Self = serde_json::from_slice(bytes).map_err(|_| refused(JOURNAL_DAMAGED))?;
        journal.validate()?;
        Ok(journal)
    }

    pub(super) fn validate(&self) -> Result<()> {
        let damaged = || refused(JOURNAL_DAMAGED);
        if self.schema_version != 1 || parse_generation(&self.generation).is_none() || parse_generation(&self.host).is_none()
            || self.started_at_unix == 0 {
            return Err(damaged());
        }
        // A journal exists only from S1 completed on (it is created then).
        if self.step < Step::HostPreflight || (self.step == Step::HostPreflight && self.phase != Phase::Completed) {
            return Err(damaged());
        }
        let commit_done = self.step == Step::Commit && self.phase == Phase::Completed;
        if self.committed != commit_done || (self.step == Step::Rollback && self.phase != Phase::Entered) {
            return Err(damaged());
        }
        let dns = &self.dns;
        let upstream_ok = match dns.route {
            DnsRoute::ResolvedVarlink => dns.upstream.is_none(),
            DnsRoute::DedicatedDnsBroker => dns.upstream.is_some(),
        };
        if !upstream_ok || dns.host_public_addresses.len() > MAX_HOST_PUBLIC_ADDRESSES
            || dns.host_public_addresses.iter().enumerate()
                .any(|(index, address)| dns.host_public_addresses.iter().take(index).any(|other| other == address)) {
            return Err(damaged());
        }
        if let Some(digest) = &self.prior.previous_binary_sha256 {
            if parse_generation(digest).is_none() { return Err(damaged()); }
        }
        if self.mode == JournalMode::Install && self.prior != Prior::fresh() { return Err(damaged()); }
        Ok(())
    }

    pub(super) fn at(&self, step: Step, phase: Phase) -> Self {
        let mut next = self.clone();
        next.step = step;
        next.phase = phase;
        next.committed = step == Step::Commit && phase == Phase::Completed;
        next
    }

    /// The only transitions a run may write: enter the next step, complete the
    /// entered step, or start a rollback of an uncommitted run. Every other
    /// field is immutable for the life of the journal.
    pub(super) fn check_transition(&self, next: &Journal) -> Result<()> {
        next.validate()?;
        let mut same = next.clone();
        same.step = self.step;
        same.phase = self.phase;
        same.committed = self.committed;
        if same != *self { return Err(refused(JOURNAL_DAMAGED)); }
        let allowed = match (self.phase, next.phase) {
            _ if next.step == Step::Rollback => !self.committed && next.phase == Phase::Entered,
            _ if self.step == Step::Rollback => false,
            (Phase::Entered, Phase::Completed) => next.step == self.step,
            (Phase::Completed, Phase::Entered) => self.step.next() == Some(next.step),
            _ => false,
        };
        if !allowed { return Err(refused(JOURNAL_DAMAGED)); }
        Ok(())
    }
}

/// Plan §4.3 rollback rule (pure): an uncommitted journal of this package's
/// mode, bound to the generation the caller names (if any). Q8: an upgrade's
/// window ends at `KeysRotated`; that boundary belongs to R5, so upgrade
/// journals are refused here.
pub(super) fn rollback_permitted(journal: &Journal, generation: Option<&str>) -> Result<()> {
    if journal.committed {
        return Err(refused("隔離安裝已提交，不能回退。提交後只能以 sudo duduclaw service uninstall --protected-linux 移除服務（會保留信任與世代）。"));
    }
    if generation.is_some_and(|generation| generation != journal.generation) { return Err(refused(OTHER_GENERATION)); }
    if journal.mode != JournalMode::Install {
        return Err(refused("升級模式的回退由遷移流程負責，尚未在此版本提供；未變更任何東西。"));
    }
    Ok(())
}

/// Review F1 (pure): the host binding — SHA-256 over a fixed domain and the
/// machine id (32 lowercase hex digits, one optional trailing newline).
pub(super) fn host_digest_of(machine_id: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let id = machine_id.strip_suffix('\n').unwrap_or(machine_id);
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-isolation-bootstrap-host-v1\0");
    hash.update(id.as_bytes());
    Some(hex::encode(hash.finalize()))
}

/// This host's binding, from the root-owned `/etc/machine-id` (never followed as a link).
pub(super) fn host_digest() -> Result<String> {
    let text = read_root_text(MACHINE_ID, 64).map_err(|_| refused(HOST_UNKNOWN))?.ok_or_else(|| refused(HOST_UNKNOWN))?;
    host_digest_of(&text).ok_or_else(|| refused(HOST_UNKNOWN))
}

/// Review F1 (pure): a journal continues only on its own host, for its own
/// generation and mode.
pub(super) fn bound_to(journal: &Journal, generation: &str, host: &str) -> Result<()> {
    if journal.host != host { return Err(refused(OTHER_HOST)); }
    if journal.mode != JournalMode::Install || journal.generation != generation { return Err(refused(OTHER_GENERATION)); }
    Ok(())
}

/// Rollback acts only on the host the journal (and its ledger) belong to.
pub(super) fn require_this_host(journal: &Journal) -> Result<()> {
    if journal.host != host_digest()? { return Err(refused(OTHER_HOST)); }
    Ok(())
}

/// The protected root (`/var/lib/duduclaw-protected`, root 0700) and its journal.
pub(super) struct Store { dir: Directory }

fn update_missing(error: &duduclaw_updated::Error) -> bool {
    matches!(error, duduclaw_updated::Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}

impl Store {
    /// `Ok(None)` when the protected root does not exist (no bootstrap ever ran).
    pub(super) fn open_existing() -> Result<Option<Self>> {
        let dir = match Directory::open(Path::new(PROTECTED_ROOT)) {
            Ok(dir) => dir,
            Err(error) if update_missing(&error) => return Ok(None),
            Err(_) => return Err(refused(JOURNAL_DAMAGED)),
        };
        let metadata = fs::symlink_metadata(PROTECTED_ROOT)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.gid() != 0 || metadata.mode() & 0o7777 != 0o700 {
            return Err(refused(JOURNAL_DAMAGED));
        }
        Ok(Some(Self { dir }))
    }

    pub(super) fn open() -> Result<Self> {
        Self::open_existing()?.ok_or_else(|| refused(NOT_STAGED_GATEWAY))
    }

    /// The verified protected-root dirfd (the creation ledger lives beside the journal).
    pub(super) fn directory(&self) -> &Directory { &self.dir }

    /// Lock 0 of the LOCK ORDER; never waits.
    pub(super) fn run_lock(&self) -> Result<fs::File> {
        let file = match self.dir.create(RUN_LOCK_FILE) {
            Ok(file) => file,
            Err(duduclaw_updated::Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists =>
                self.dir.read(RUN_LOCK_FILE).map_err(|_| refused(RUN_BUSY))?,
            Err(_) => return Err(refused(RUN_BUSY)),
        };
        if file.metadata()?.mode() & 0o7777 != 0o600 { return Err(refused(RUN_BUSY)); }
        // SAFETY: scalar flock on a live descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 { return Err(refused(RUN_BUSY)); }
        Ok(file)
    }

    /// NotFound = `None`; anything else that is not a strict journal refuses.
    pub(super) fn read(&self) -> Result<Option<Journal>> {
        let file = match self.dir.read(JOURNAL_FILE) {
            Ok(file) => file,
            Err(error) if update_missing(&error) => return Ok(None),
            Err(_) => return Err(refused(JOURNAL_DAMAGED)),
        };
        let metadata = file.metadata()?;
        if metadata.mode() & 0o7777 != 0o600 || metadata.len() > MAX_JOURNAL_BYTES as u64 {
            return Err(refused(JOURNAL_DAMAGED));
        }
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        Journal::parse(&bytes).map(Some)
    }

    /// Durable, checked write: the on-disk journal must still be `previous`,
    /// the transition must be allowed, and the result is read back.
    pub(super) fn write(&self, previous: Option<&Journal>, next: &Journal) -> Result<()> {
        match previous {
            Some(previous) => previous.check_transition(next)?,
            None => {
                next.validate()?;
                if next.step != Step::HostPreflight || next.phase != Phase::Completed { return Err(refused(JOURNAL_DAMAGED)); }
            }
        }
        if self.read()?.as_ref() != previous { return Err(refused(JOURNAL_DAMAGED)); }
        self.dir.atomic_json(JOURNAL_FILE, next).map_err(|_| refused("無法寫入隔離安裝日誌。"))?;
        if self.read()?.as_ref() != Some(next) { return Err(refused(JOURNAL_DAMAGED)); }
        Ok(())
    }

    /// The last action of a completed rollback.
    pub(super) fn remove(&self) -> Result<()> {
        match self.dir.remove(JOURNAL_FILE) {
            Ok(()) => Ok(()),
            Err(error) if update_missing(&error) => Ok(()),
            Err(_) => Err(refused("無法移除隔離安裝日誌。")),
        }
    }
}

/// Called under the version lock by `service install|upgrade|uninstall
/// --protected-linux`: an uncommitted bootstrap owns the host.
pub(super) fn refuse_if_in_progress() -> Result<()> {
    let Some(store) = Store::open_existing()? else { return Ok(()) };
    match store.read()? {
        Some(journal) if !journal.committed => Err(refused(IN_PROGRESS)),
        _ => Ok(()),
    }
}

pub(super) fn print_fresh_install_instructions() {
    println!("從 v1.83.0 起，全新主機的受保護安裝（含員工隔離）步驟如下：");
    println!("1. 以 root 建立下載目錄：sudo install -d -m 0700 -o root -g root /root/duduclaw-migrate");
    println!("2. 從官網下載 duduclaw-bootstrap、duduclaw-isolated-linux-<架構>-<版本>.tar 與它的 .minisig、.authorization.json、.authorization.json.signatures.json，存成檔案放進該目錄；團隊版授權檔 license.json 也放進同一個目錄。");
    println!("3. 以 sha256sum -c 比對官網與授權 console 公布的 SHA-256，兩處必須一致。");
    println!("4. 執行：sudo /root/duduclaw-migrate/duduclaw-bootstrap install [--dns-upstream <IP:53>] [--host-public-address <IP>] [--enable-now]");
}

// ── Continue options ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode { Install, Upgrade, Rollback }

/// The typed values of `service bootstrap-continue`.
pub(crate) struct ContinueOptions {
    mode: Mode,
    generation: Sha256Digest,
    generation_hex: String,
    dns_upstream: Option<SocketAddr>,
    host_public_addresses: Vec<IpAddr>,
    enable_now: bool,
}

impl ContinueOptions {
    pub(crate) fn from_args(args: super::super::BootstrapContinueArgs) -> Result<Self> {
        let mode = match args.mode.as_str() {
            "install" => Mode::Install, "upgrade" => Mode::Upgrade, "rollback" => Mode::Rollback,
            _ => return Err(refused(NOT_STAGED_GATEWAY)),
        };
        let generation = parse_generation(&args.generation).ok_or_else(|| refused(NOT_STAGED_GATEWAY))?;
        let hosts = &args.host_public_address;
        if hosts.len() > MAX_HOST_PUBLIC_ADDRESSES
            || hosts.iter().enumerate().any(|(index, address)| hosts.iter().take(index).any(|other| other == address)) {
            return Err(refused("--host-public-address 的數量超過上限或有重複。"));
        }
        if mode != Mode::Install && (args.dns_upstream.is_some() || !hosts.is_empty() || args.enable_now) {
            return Err(refused(NOT_STAGED_GATEWAY));
        }
        Ok(Self { mode, generation, generation_hex: args.generation, dns_upstream: args.dns_upstream,
            host_public_addresses: args.host_public_address, enable_now: args.enable_now })
    }
}

/// Review F6: S1 runs on every invocation, fresh or resumed. A resumed run
/// re-checks the host with the journal's DNS choices (omitted options reuse
/// them, given ones must equal them) and its decision must still be the
/// recorded one. Returns the journal and whether it is new (to be written).
pub(super) fn begin(journal: Option<Journal>, options: &ContinueOptions, host: &str, now: u64,
    preflight: impl FnOnce(Option<SocketAddr>, &[IpAddr]) -> Result<Dns>) -> Result<(Journal, bool)> {
    match journal {
        Some(journal) => {
            reconcile(&journal.dns, options.dns_upstream, &options.host_public_addresses)?;
            let dns = preflight(journal.dns.upstream, &journal.dns.host_public_addresses)?;
            if dns != journal.dns { return Err(refused(HOST_DNS_CHANGED)); }
            Ok((journal, false))
        }
        None => {
            let dns = preflight(options.dns_upstream, &options.host_public_addresses)?;
            Ok((Journal::new(&options.generation_hex, host, dns, now), true))
        }
    }
}

/// A resumed run must not silently change the S1 decisions: omitted values
/// reuse the journal's, given values must equal them.
pub(super) fn reconcile(journal: &Dns, upstream: Option<SocketAddr>, hosts: &[IpAddr]) -> Result<()> {
    if upstream.is_some_and(|upstream| journal.upstream != Some(upstream))
        || (!hosts.is_empty() && hosts != journal.host_public_addresses.as_slice()) {
        return Err(refused(OPTIONS_CHANGED));
    }
    Ok(())
}

/// Plan S5 route rule (pure): systemd-resolved's Varlink socket ⇒ resolved,
/// and then no upstream is accepted (it could only widen what the host
/// resolves through); otherwise exactly one broker upstream is required and
/// it must pass the broker's own validator. The upstream goes only to the
/// broker's root config, never into egress.json, so it cannot open a private
/// address to employees.
pub(super) fn dns_route(resolved: bool, upstream: Option<SocketAddr>,
    validate: impl FnOnce(SocketAddr) -> bool) -> std::result::Result<(DnsRoute, Option<SocketAddr>), &'static str> {
    match (resolved, upstream) {
        (true, None) => Ok((DnsRoute::ResolvedVarlink, None)),
        (true, Some(_)) => Err("這台主機使用 systemd-resolved，不需要也不接受 --dns-upstream。"),
        (false, None) => Err("這台主機沒有可用的 systemd-resolved；請以 --dns-upstream <IP:53> 指定專用 DNS 轉送服務的上游 DNS。"),
        (false, Some(upstream)) if validate(upstream) => Ok((DnsRoute::DedicatedDnsBroker, Some(upstream))),
        (false, Some(_)) => Err("--dns-upstream 必須是單一 IP 位址加上連接埠 53。"),
    }
}

// ── Entry ─────────────────────────────────────────────────────────────────

struct Context {
    generation: Sha256Digest,
    generation_hex: String,
    stage: StageReceipt,
    enable_now: bool,
    /// Review F4: this transaction's creation ledger.
    recorder: LiveRecorder,
}

#[cfg(test)]
thread_local! { static STOP_AFTER: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) }; }

/// Test-only crash injection right after a step's completion was recorded.
fn checkpoint(step: Step) -> Result<()> {
    #[cfg(test)]
    if STOP_AFTER.with(|value| value.get() == Some(step)) {
        STOP_AFTER.with(|value| value.set(None));
        return Err(refused("test interruption"));
    }
    let _ = step;
    Ok(())
}

fn now() -> Result<u64> { duduclaw_updated::linux::now().map_err(|_| refused("系統時間無效。")) }

pub(super) fn continue_run(options: ContinueOptions) -> Result<()> {
    continue_with(options, true)
}

/// `verify_self`: the running program must be the staged generation's gateway
/// (production always; only root tests that drive the state machine from the
/// test binary pass false).
fn continue_with(options: ContinueOptions, verify_self: bool) -> Result<()> {
    duduclaw_updated::linux::harden().map_err(|_| refused(HARDEN_FAILED))?;
    duduclaw_updated::linux::require_root().map_err(|_| refused(HARDEN_FAILED))?;
    let store = Store::open()?;
    let _run = store.run_lock()?;
    let journal = store.read()?;
    match options.mode {
        Mode::Rollback => return super::rollback::run(&store, journal, Some(options.generation_hex.as_str())),
        Mode::Upgrade => return Err(refused(UPGRADE_NOT_HERE)),
        Mode::Install => (),
    }
    if let Some(journal) = &journal {
        if journal.step == Step::Rollback {
            println!("上一次的回退尚未完成，現在繼續完成回退；完成後請重新執行 duduclaw-bootstrap install。");
            return super::rollback::run(&store, Some(journal.clone()), Some(options.generation_hex.as_str()));
        }
        if journal.committed { return Err(refused(ALREADY_COMMITTED)); }
    }
    // S0 Verified: every run re-verifies the stage (R1) and that this program is its gateway.
    let stage = verified_stage(&options, verify_self)?;
    // Review F1: the journal belongs to this generation and this host.
    let host = host_digest()?;
    if let Some(journal) = &journal { bound_to(journal, &options.generation_hex, &host)?; }
    // A ledger exists only beside a journal (created after it, removed before it).
    let ours = ledger::read(&store)?;
    if journal.is_none() && ours.is_some() { return Err(refused(ledger::LEDGER_DAMAGED)); }
    // S1 HostPreflight on every invocation (review F6): no side effect; what
    // this transaction already created (per its ledger) is not "residue".
    let (mut journal, fresh) = begin(journal, &options, &host, now()?,
        |upstream, hosts| preflight(upstream, hosts, &stage, ours.as_ref()))?;
    if fresh { store.write(None, &journal)?; }
    println!("[完成] S1 {}", Step::HostPreflight.label());
    let recorder = LiveRecorder::open_live(&options.generation_hex)?;
    let mut context = Context { generation: options.generation, generation_hex: options.generation_hex.clone(),
        stage, enable_now: options.enable_now, recorder };
    let dns = journal.dns.clone();
    let result = {
        let mut live = Live { context: &mut context, dns: &dns };
        drive(&mut journal, &mut live, &mut |previous: &Journal, next: &Journal| store.write(Some(previous), next), Step::Commit)
    };
    if let Err(error) = result {
        let step = journal.step;
        eprintln!("安裝停在 S{} {}。修正原因後以同一個 duduclaw-bootstrap install 命令續接；或執行 duduclaw-bootstrap rollback 回到安裝前的狀態。",
            step.number(), step.label());
        return Err(error);
    }
    // The stage and the creation ledger are not facts; after the commit they are no longer needed.
    let stage_removed = super::rollback::remove_stage(&context.generation_hex).is_ok();
    let ledger_removed = ledger::remove(&store).is_ok();
    if !stage_removed || !ledger_removed {
        eprintln!("警告：無法清除暫存的安裝包或安裝建立紀錄；安裝已完成，不影響運作。");
    }
    println!("員工隔離的受保護安裝已完成。");
    if !context.enable_now {
        println!("Gateway 已啟用但尚未啟動。啟動：sudo systemctl start {UNIT}，之後請執行 sudo duduclaw service readiness --protected-linux。");
    }
    Ok(())
}

/// Plan S0: R1 re-verifies the staged generation (root pin, signatures, every
/// artifact's mode, size and hash, exact file set); the receipt must be an
/// install of exactly this generation, and the running program must be its
/// gateway artifact (compared through `/proc/self/exe`, the sealed memfd).
fn verified_stage(options: &ContinueOptions, verify_self: bool) -> Result<StageReceipt> {
    let stage = bootstrap_verify::load_stage(options.generation).map_err(|_| refused(STAGE_UNVERIFIED))?;
    if stage.manifest_digest != options.generation || stage.mode != bootstrap_verify::BootstrapMode::Install {
        return Err(refused(STAGE_UNVERIFIED));
    }
    let gateway = single_artifact(&stage.artifacts, &InstallTarget::Gateway).ok_or_else(|| refused(STAGE_UNVERIFIED))?;
    if verify_self && !self_is(gateway)? { return Err(refused(NOT_STAGED_GATEWAY)); }
    Ok(stage)
}

fn single_artifact<'a>(artifacts: &'a [ReleaseArtifact], target: &InstallTarget) -> Option<&'a ReleaseArtifact> {
    let mut matching = artifacts.iter().filter(|artifact| artifact.install_target == *target);
    match (matching.next(), matching.next()) { (Some(artifact), None) => Some(artifact), _ => None }
}

/// Bounded SHA-256 of the running executable against the artifact.
fn self_is(artifact: &ReleaseArtifact) -> Result<bool> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open("/proc/self/exe")?;
    if file.metadata()?.len() != artifact.size_bytes { return Ok(false); }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        total = total.saturating_add(count as u64);
        if total > artifact.size_bytes { return Ok(false); }
        hash.update(buffer.get(..count).ok_or_else(|| refused(NOT_STAGED_GATEWAY))?);
    }
    Ok(total == artifact.size_bytes && hash.finalize().as_slice() == artifact.sha256.0)
}

fn with_version_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = version_lock()?;
    f()
}

/// Review F1 seam: what a step established, checked against the real host,
/// and doing the step.
pub(super) trait Steps {
    /// The step's invariants hold on the host now (never the journal's word).
    fn holds(&mut self, step: Step) -> Result<bool>;
    fn perform(&mut self, step: Step) -> Result<()>;
}

/// The last step after S1 the journal claims complete, if any.
fn claimed_complete(journal: &Journal) -> Option<Step> {
    match journal.phase {
        Phase::Completed => Some(journal.step),
        Phase::Entered => Step::ORDER.iter().position(|step| *step == journal.step)
            .and_then(|index| index.checked_sub(1)).and_then(|index| Step::ORDER.get(index).copied()),
    }
}

/// Review F1: the journal cursor is only a hint. Every step it claims
/// complete after S1 (S0 and S1 already re-ran) is re-established against the
/// real host, in order. From the first step whose invariants do not hold, that
/// step and every later claimed step are re-run (their own checks were made
/// against a state that no longer holds). S2/S3 are pure checks, so re-running
/// them refuses rather than changes anything; S4–S8 are idempotent.
pub(super) fn reestablish(steps: &mut dyn Steps, journal: &Journal) -> Result<()> {
    let Some(last) = claimed_complete(journal) else { return Ok(()) };
    let mut rerun = false;
    for step in Step::ORDER.iter().copied().filter(|step| *step >= Step::StopGateway && *step <= last && *step != Step::Commit) {
        if !rerun && steps.holds(step)? { continue; }
        if !rerun {
            println!("[重新建立] 日誌記錄 S{} {} 已完成，但主機現況不符；從這一步重新執行。", step.number(), step.label());
        }
        rerun = true;
        steps.perform(step)?;
        println!("[完成] S{} {}", step.number(), step.label());
    }
    Ok(())
}

/// Run every step after the journal's position up to and including
/// `through` (plan §5: the seam R5 extends to drive S2..=S6 for an upgrade
/// journal; the upgrade-specific S2 is not implemented in this package).
/// `write` makes one checked journal transition durable.
fn run_steps(journal: &mut Journal, steps: &mut dyn Steps, write: &mut dyn FnMut(&Journal, &Journal) -> Result<()>,
    through: Step) -> Result<()> {
    loop {
        let step = match journal.phase {
            Phase::Entered => journal.step,
            Phase::Completed => match journal.step.next() {
                Some(next) if next <= through => {
                    let entered = journal.at(next, Phase::Entered);
                    write(&*journal, &entered)?;
                    *journal = entered;
                    next
                }
                _ => return Ok(()),
            },
        };
        steps.perform(step)?;
        let completed = journal.at(step, Phase::Completed);
        write(&*journal, &completed)?;
        *journal = completed;
        println!("[完成] S{} {}", step.number(), step.label());
        checkpoint(step)?;
    }
}

/// Re-establish what the journal claims, then continue from its position.
pub(super) fn drive(journal: &mut Journal, steps: &mut dyn Steps, write: &mut dyn FnMut(&Journal, &Journal) -> Result<()>,
    through: Step) -> Result<()> {
    reestablish(steps, journal)?;
    run_steps(journal, steps, write, through)
}

struct Live<'a> { context: &'a mut Context, dns: &'a Dns }

impl Steps for Live<'_> {
    fn holds(&mut self, step: Step) -> Result<bool> { Ok(invariants(step, &*self.context, self.dns).is_ok()) }
    fn perform(&mut self, step: Step) -> Result<()> { perform(step, &mut *self.context, self.dns) }
}

fn perform(step: Step, context: &mut Context, dns: &Dns) -> Result<()> {
    match step {
        Step::Verified | Step::HostPreflight | Step::Rollback => Err(refused(JOURNAL_DAMAGED)),
        Step::StopGateway => with_version_lock(gateway_not_running),
        Step::ProcessesClear => with_version_lock(super::removal::processes_clear),
        Step::PublishPlatform => publish_platform(context),
        Step::DnsPolicy => with_version_lock(|| dns_policy(context, dns)),
        Step::StartServices => with_version_lock(|| start_services(context, dns)),
        Step::Readiness => with_version_lock(|| {
            // spawnd republishes its short-lived evidence; let one cycle pass.
            std::thread::sleep(Duration::from_secs(2));
            readiness::check_platform(&mut readiness::InstalledComposite)
        }),
        Step::StartGateway => with_version_lock(|| start_gateway(context)),
        Step::Commit => with_version_lock(|| commit(context)),
    }
}

/// Review F1: the read-only invariants of each step on the real host.
fn invariants(step: Step, context: &Context, dns: &Dns) -> Result<()> {
    match step {
        // A gateway may run only as the one this transaction published at S8.
        Step::StopGateway => with_version_lock(|| if gateway_not_running().is_ok() { Ok(()) } else { gateway_published(context) }),
        Step::ProcessesClear => with_version_lock(|| {
            if gateway_active() && gateway_published(context).is_ok() { Ok(()) } else { super::removal::processes_clear() }
        }),
        Step::PublishPlatform => with_version_lock(|| platform_published(context)),
        Step::DnsPolicy => with_version_lock(|| dns_readback(context, dns)),
        Step::StartServices => with_version_lock(|| services_running(context, dns)),
        Step::Readiness => with_version_lock(|| readiness::check_platform(&mut readiness::InstalledComposite)),
        Step::StartGateway => with_version_lock(|| gateway_published(context)),
        Step::Verified | Step::HostPreflight | Step::Commit | Step::Rollback => Err(refused(JOURNAL_DAMAGED)),
    }
}

// ── S1 HostPreflight ──────────────────────────────────────────────────────

fn exists(path: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// At most `limit` bytes of a root-owned regular file, opened without
/// following links after its ancestors were checked. NotFound = `None`.
fn read_root_file(path: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let path = Path::new(path);
    parents(path)?;
    let file = match fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.len() > limit {
        return Err(refused(format!("系統檔案不符合要求：{}", path.display())));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit { return Err(refused(format!("系統檔案過大：{}", path.display()))); }
    Ok(Some(bytes))
}

fn read_root_text(path: &str, limit: u64) -> Result<Option<String>> {
    read_root_file(path, limit)?.map(|bytes| String::from_utf8(bytes)
        .map_err(|_| refused(format!("系統檔案不是 UTF-8 文字：{path}")))).transpose()
}

/// "systemd 249 (249.11-0ubuntu3)" → 249.
pub(super) fn systemd_major(version_output: &str) -> Option<u32> {
    let line = version_output.lines().next()?;
    let mut words = line.split_whitespace();
    if words.next()? != "systemd" { return None; }
    words.next()?.parse().ok()
}

/// "5.15.0-1120-gcp" → (5, 15).
pub(super) fn kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.trim().split(['.', '-']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// `ID`/`VERSION_ID` of os-release: Ubuntu 22.04/24.04 or Debian 12 (plan §7).
pub(super) fn supported_distribution(os_release: &str) -> bool {
    let value = |key: &str| os_release.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(|value| value.trim().trim_matches('"'));
    matches!((value("ID"), value("VERSION_ID")), (Some("ubuntu"), Some("22.04" | "24.04")) | (Some("debian"), Some("12")))
}

/// Any `name:start:count` range of /etc/subuid or /etc/subgid that meets the
/// employee range 70000–99999. Malformed lines also count (fail closed).
pub(super) fn subordinate_overlap(text: &str) -> bool {
    let (low, high) = (u64::from(duduclaw_isolation::ids::EMPLOYEE_UID_MIN), u64::from(duduclaw_isolation::ids::EMPLOYEE_UID_MAX));
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).any(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let [_, start, count] = fields.as_slice() else { return true };
        let (Ok(start), Ok(count)) = (start.parse::<u64>(), count.parse::<u64>()) else { return true };
        count > 0 && start <= high && start.saturating_add(count - 1) >= low
    })
}

/// `UID_MIN`..`UID_MAX` (or `GID_*`) of login.defs, defaults 1000..60000,
/// must stay below the employee range.
pub(super) fn login_range_clear(defs: &str, prefix: &str) -> bool {
    let value = |key: String, default: u64| -> Option<u64> {
        let mut found = None;
        for line in defs.lines() {
            let fields: Vec<&str> = line.split('#').next().unwrap_or("").split_whitespace().collect();
            if let [name, value] = fields.as_slice() {
                if *name == key { found = Some(value.parse::<u64>().ok()?); }
            }
        }
        Some(found.unwrap_or(default))
    };
    match (value(format!("{prefix}_MIN"), 1000), value(format!("{prefix}_MAX"), 60000)) {
        (Some(min), Some(max)) => min <= max && max < u64::from(duduclaw_isolation::ids::EMPLOYEE_UID_MIN),
        _ => false,
    }
}

/// `getent passwd|group` enumeration: any id in the employee range.
pub(super) fn enumeration_overlap(text: &str) -> bool {
    let range = duduclaw_isolation::ids::EMPLOYEE_UID_MIN..=duduclaw_isolation::ids::EMPLOYEE_UID_MAX;
    text.lines().any(|line| line.split(':').nth(2).is_some_and(|id| id.parse::<u32>().is_ok_and(|id| range.contains(&id))))
}

/// The passwd/group lines of nsswitch.conf name an external account source.
pub(super) fn external_nss(nsswitch: &str) -> bool {
    nsswitch.lines().map(|line| line.split('#').next().unwrap_or("")).any(|line| {
        let mut words = line.split_whitespace();
        matches!(words.next(), Some("passwd:" | "group:" | "shadow:"))
            && words.any(|source| matches!(source, "ldap" | "sss" | "winbind" | "nis" | "nisplus" | "hesiod"))
    })
}

/// sshd's main configuration includes the drop-in directory spawnd writes.
pub(super) fn sshd_includes_dropins(config: &str) -> bool {
    config.lines().map(str::trim).any(|line| {
        let mut words = line.split_whitespace();
        words.next().is_some_and(|keyword| keyword.eq_ignore_ascii_case("include"))
            && words.any(|path| matches!(path, "/etc/ssh/sshd_config.d/*.conf" | "sshd_config.d/*.conf"))
    })
}

/// cron.allow / at.allow never admit a DuDuClaw account (their deny lists
/// would otherwise be ignored for it).
pub(super) fn allow_list_clear(text: &str) -> bool {
    !text.lines().map(str::trim).any(|line| line.starts_with("duduclaw") || line == "ALL")
}

#[allow(clippy::unnecessary_cast)]
fn statfs_type(path: &CStr) -> Option<i64> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: NUL-terminated path; the buffer is written on success.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 { return None; }
    // SAFETY: initialized by the successful statfs.
    let stat = unsafe { stat.assume_init() };
    Some(stat.f_type as i64)
}

#[allow(clippy::unnecessary_cast)]
fn free_bytes(path: &CStr) -> Option<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: NUL-terminated path; the buffer is written on success.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 { return None; }
    // SAFETY: initialized by the successful statvfs.
    let stat = unsafe { stat.assume_init() };
    (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64)
}

fn tool_present(name: &str) -> bool {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"].iter().any(|dir| Path::new(dir).join(name).is_file())
}

/// Paths a v1.80–v1.82 single-binary protected install (or an earlier
/// attempt of it) leaves. Any of them makes this a host for R5's upgrade.
fn single_binary_residue() -> [&'static str; 8] {
    [duduclaw_core::root_key::KEY_PATH, VERSION_FLOOR, VERSION_PENDING, BINARY, LAUNCHER, UNIT_PATH,
        RECOVERY_BOUND, fresh::FRESH_PENDING]
}

/// Review F8: a license file is read only when its mode is exactly 0600 or 0644.
pub(super) fn license_mode_ok(mode: u32) -> bool { matches!(mode & 0o7777, 0o600 | 0o644) }

/// The license a TEAM install will use: the gateway's own when present,
/// otherwise the root-owned inbox copy. `Ok(None)` when neither exists.
fn license_bytes() -> Result<Option<Vec<u8>>> {
    let own = format!("{GATEWAY_HOME}/.duduclaw/license.json");
    if exists(GATEWAY_HOME)? && exists(&own)? {
        // The gateway's private directory: read only through checked dirfds.
        let (data, _) = open_gateway_data(None)?;
        return read_data_file(&data, LICENSE_NAME).map(Some);
    }
    let inbox = fs::symlink_metadata(INBOX);
    match inbox {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) => if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o700 {
            return Err(refused("下載目錄 /root/duduclaw-migrate 必須是 root 擁有、權限 0700 的目錄。"));
        },
    }
    let directory = Directory::open(Path::new(INBOX)).map_err(|_| refused(LICENSE_MISSING))?;
    // `Directory::read`: no-follow, root:root, regular, one link, not group/other writable.
    let file = match directory.read(INBOX_LICENSE) {
        Ok(file) => file,
        Err(error) if update_missing(&error) => return Ok(None),
        Err(_) => return Err(refused(LICENSE_MISSING)),
    };
    let metadata = file.metadata()?;
    if !license_mode_ok(metadata.mode()) { return Err(refused(LICENSE_MODE)); }
    if metadata.len() > readiness::MAX_LICENSE_BYTES { return Err(refused(LICENSE_NOT_TEAM)); }
    let mut bytes = Vec::new();
    file.take(readiness::MAX_LICENSE_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// The gateway home may pre-exist only as the isolated layout this installer
/// creates (left by a rolled-back attempt): root 0711 holding only a private
/// `.duduclaw` with nothing but an optional `license.json`.
fn gateway_home_clear() -> Result<bool> {
    if !exists(GATEWAY_HOME)? { return Ok(true); }
    parents(Path::new(GATEWAY_HOME))?;
    let metadata = fs::symlink_metadata(GATEWAY_HOME)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.gid() != 0 || metadata.mode() & 0o7777 != 0o711 {
        return Ok(false);
    }
    let entries: Vec<_> = fs::read_dir(GATEWAY_HOME)?.take(3).collect::<std::result::Result<_, _>>()?;
    match entries.as_slice() {
        [] => Ok(true),
        [entry] if entry.file_name() == ".duduclaw" => {
            let Some(gateway) = passwd_entry("duduclaw")? else { return Ok(false) };
            let (data, _) = open_gateway_data(Some((gateway.uid, gateway.gid)))?;
            let names = directory_names(&data, 4)?;
            Ok(names.iter().all(|name| name.as_c_str() == LICENSE_NAME))
        }
        _ => Ok(false),
    }
}

/// Plan S1. Every check runs; all failures are listed, then the run refuses.
/// Nothing is changed. Returns the DNS decision recorded in the journal.
/// `ours` (a resumed run, review F6): what this transaction's creation ledger
/// records as created by it is not residue of another install; everything
/// else is checked exactly as on a fresh run.
fn preflight(upstream: Option<SocketAddr>, hosts: &[IpAddr], stage: &StageReceipt, ours: Option<&ledger::Ledger>) -> Result<Dns> {
    let mut failures: Vec<String> = Vec::new();
    let mut fail = |message: &str| failures.push(message.to_owned());
    let foreign = |path: &str| exists(path).unwrap_or(true) && !ours.is_some_and(|recorded| recorded.covers(path));

    // Q9: TEAM edition first.
    match license_bytes() {
        Ok(Some(bytes)) => if !readiness::license_bytes_admit_isolation(&bytes).unwrap_or(false) { fail(LICENSE_NOT_TEAM) },
        Ok(None) => fail(LICENSE_MISSING),
        Err(_) => fail(LICENSE_MISSING),
    }
    // Isolation facts (R1 checked them before staging; checked again here).
    if duduclaw_core::isolation_install::FACTS.iter().any(|fact| foreign(*fact)) {
        fail("這台主機已有員工隔離的安裝紀錄；請先執行 duduclaw-bootstrap rollback 或聯絡原廠。");
    }
    // A single-binary protected host is R5's upgrade path, never a fresh install.
    if single_binary_residue().iter().any(|path| foreign(*path)) {
        fail("這台主機已有單一執行檔的受保護安裝（或其遺留檔案）；請改用升級流程 duduclaw-bootstrap upgrade，不要刪除這些檔案。");
    }
    if [duduclaw_core::isolation_install::DURABLE_IMPORT_MANIFEST, duduclaw_egress::daemon::CONFIG_PATH,
        duduclaw_egress::daemon::BROKER_CONFIG_PATH, NIGHTLY_HELPER].iter().any(|path| foreign(*path)) {
        fail("這台主機有先前安裝留下的設定檔（/etc/duduclaw 或自動更新程式）；請先確認並移除後再安裝。");
    }
    // After S8 the gateway writes its own data; its snapshot is in the ledger.
    let snapshot = ours.is_some_and(|recorded| recorded.entry(ledger::GATEWAY_DATA_PATH).is_some());
    if !snapshot && !gateway_home_clear().unwrap_or(false) {
        fail("/var/lib/duduclaw 已有資料；全新安裝不能覆蓋既有資料。若這是舊版的受保護安裝，請改用升級流程。");
    }
    // Platform.
    // /etc/os-release is normally a symlink to /usr/lib/os-release; links are
    // never followed, so the regular file is read first.
    let os_release = read_root_text("/usr/lib/os-release", 64 * 1024).ok().flatten()
        .or_else(|| read_root_text("/etc/os-release", 64 * 1024).ok().flatten());
    if !os_release.is_some_and(|text| supported_distribution(&text)) {
        fail("此發行版不在支援清單（Ubuntu 22.04、Ubuntu 24.04、Debian 12）。");
    }
    let systemd = command("systemctl", &["--version"]).ok()
        .and_then(|output| String::from_utf8(output.stdout).ok()).and_then(|text| systemd_major(&text));
    if !systemd.is_some_and(|major| major >= 249) { fail("需要 systemd 249 以上。"); }
    let kernel = read_root_text("/proc/sys/kernel/osrelease", 256).ok().flatten().and_then(|text| kernel_version(&text));
    if !kernel.is_some_and(|version| version >= (5, 15)) { fail("需要 Linux 核心 5.15 以上。"); }
    if statfs_type(c"/sys/fs/cgroup") != Some(0x6367_7270) { fail("需要 cgroup v2（unified）。"); }
    if let Err(message) = storage_clear() { fail(message); }
    for tool in ["systemctl", "useradd", "getent", "id", "loginctl", "python3", "setpriv"] {
        if !tool_present(tool) { fail("缺少必要的系統工具（systemctl、useradd、getent、id、loginctl、python3、setpriv）。"); break; }
    }
    // Identities (design §5.1).
    match read_root_text("/etc/login.defs", 1024 * 1024) {
        Ok(Some(defs)) => {
            if layout::system_id_limit(&defs, "SYS_UID_MAX").is_err() || layout::system_id_limit(&defs, "SYS_GID_MAX").is_err() {
                fail("/etc/login.defs 必須明確設定 SYS_UID_MAX 與 SYS_GID_MAX。");
            }
            if !login_range_clear(&defs, "UID") || !login_range_clear(&defs, "GID") {
                fail("一般帳號的 uid／gid 範圍與員工專用範圍 70000–99999 重疊。");
            }
        }
        _ => fail("無法讀取 /etc/login.defs。"),
    }
    for path in ["/etc/subuid", "/etc/subgid"] {
        match read_root_text(path, 1024 * 1024) {
            Ok(None) => (),
            Ok(Some(text)) if !subordinate_overlap(&text) => (),
            _ => fail("/etc/subuid 或 /etc/subgid 的範圍與員工專用範圍 70000–99999 重疊，或無法讀取。"),
        }
    }
    for database in ["passwd", "group"] {
        let overlap = command("getent", &[database]).ok().filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok()).map(|text| enumeration_overlap(&text));
        if overlap != Some(false) { fail("已有帳號或群組使用員工專用範圍 70000–99999，或無法列出帳號。"); }
    }
    if let Err(message) = nss_clear() { fail(message); }
    if let Err(message) = sshd_clear() { fail(message); }
    for path in ["/etc/cron.allow", "/etc/at.allow"] {
        match read_root_text(path, 1024 * 1024) {
            Ok(None) => (),
            Ok(Some(text)) if allow_list_clear(&text) => (),
            _ => fail("/etc/cron.allow 或 /etc/at.allow 允許了 DuDuClaw 帳號（或無法讀取）；這會讓員工帳號的封鎖失效。"),
        }
    }
    // Space: the generation, the fixed-path copies and the stage are all on disk.
    let total = stage.artifacts.iter().try_fold(0u64, |sum, artifact| sum.checked_add(artifact.size_bytes)).unwrap_or(u64::MAX);
    for path in [c"/opt", c"/usr", c"/var/lib"] {
        if !free_bytes(path).is_some_and(|free| free >= total.saturating_mul(2).saturating_add(1024 * 1024 * 1024)) {
            fail("可用磁碟空間不足（/opt、/usr、/var/lib 各需要至少安裝包大小的兩倍再加 1 GiB）。");
            break;
        }
    }
    // DNS (plan S5).
    let decision = dns_route(resolved_available(), upstream,
        |upstream| duduclaw_egress::daemon::BrokerConfig { upstream }.validate().is_ok());
    let dns = match decision {
        Ok((route, upstream)) => Some(Dns { route, upstream, host_public_addresses: hosts.to_vec() }),
        Err(message) => { fail(message); None }
    };
    match (failures.is_empty(), dns) {
        (true, Some(dns)) => Ok(dns),
        _ => {
            println!("[未通過] S1 {}，未變更任何東西：", Step::HostPreflight.label());
            for failure in &failures { println!("  - {failure}"); }
            Err(refused(format!("主機預檢有 {} 項未通過。", failures.len())))
        }
    }
}

// ── Host conditions re-verified before first use (review F6, F7) ──────────

fn resolved_available() -> bool {
    fs::symlink_metadata(duduclaw_egress::resolver::RESOLVED_SOCKET)
        .is_ok_and(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()))
        && command("systemctl", &["is-active", "--quiet", "systemd-resolved.service"])
            .is_ok_and(|output| output.status.success())
}

fn nss_clear() -> std::result::Result<(), &'static str> {
    match read_root_text("/etc/nsswitch.conf", 256 * 1024) {
        Ok(Some(text)) if external_nss(&text) =>
            Err("偵測到外部帳號服務（LDAP／SSSD 等）。本版安裝程式不支援這類主機：外部帳號服務必須先永久保留員工 uid 範圍 70000–99999，並由原廠協助確認。"),
        Ok(_) => Ok(()),
        Err(_) => Err("無法讀取 /etc/nsswitch.conf。"),
    }
}

fn sshd_clear() -> std::result::Result<(), &'static str> {
    match read_root_text("/etc/ssh/sshd_config", 1024 * 1024) {
        Ok(Some(text)) if !sshd_includes_dropins(&text) =>
            Err("sshd 主設定必須包含 Include /etc/ssh/sshd_config.d/*.conf，員工帳號的 SSH 封鎖才會生效。"),
        Ok(_) => Ok(()),
        Err(_) => Err("無法讀取 /etc/ssh/sshd_config。"),
    }
}

/// The DNS decision of S1 still holds (before S5 writes it, before S6 uses it).
fn dns_route_unchanged(dns: &Dns) -> Result<()> {
    let decision = dns_route(resolved_available(), dns.upstream,
        |upstream| duduclaw_egress::daemon::BrokerConfig { upstream }.validate().is_ok());
    if decision != Ok((dns.route, dns.upstream)) { return Err(refused(HOST_DNS_CHANGED)); }
    Ok(())
}

/// Review F6: the volatile conditions the isolation services depend on,
/// right before they first start (S6).
fn services_preconditions(dns: &Dns) -> Result<()> {
    dns_route_unchanged(dns)?;
    storage_clear().map_err(refused)?;
    nss_clear().map_err(refused)?;
    sshd_clear().map_err(refused)
}

/// `/var/lib` is ext4 or XFS (statfs magic) on a fully determined block
/// device chain with no thin, snapshot or cache layer (review F7, design §4.3).
fn storage_clear() -> std::result::Result<(), &'static str> {
    if !matches!(statfs_type(c"/var/lib"), Some(0xEF53 | 0x5846_5342)) {
        return Err("員工儲存空間所在的 /var/lib 必須是 ext4 或 XFS。");
    }
    let device = device_of(c"/var/lib").ok_or(STORAGE_UNDETERMINED)?;
    storage_chain(&mut LiveBlockDevices, device)
}

/// `major:minor` of the filesystem holding `path`.
fn device_of(path: &CStr) -> Option<(u32, u32)> {
    // SAFETY: an all-zero `statx` (plain integers) is a valid value.
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    duduclaw_core::linux_syscalls::statx(libc::AT_FDCWD, path, 0, libc::STATX_BASIC_STATS, &mut stat).ok()?;
    Some((stat.stx_dev_major, stat.stx_dev_minor))
}

const MAX_CHAIN_DEVICES: usize = 64;
const MAX_CHAIN_DEPTH: usize = 16;
/// LVM sub-volume suffixes of thin pools, snapshots, caches and VDO pools (`dm/uuid`).
const NON_PREALLOCATED_UUID_SUFFIXES: [&str; 11] = ["-tpool", "-tdata", "-tmeta", "-pool", "-cow", "-real",
    "-cdata", "-cmeta", "-corig", "-vpool", "-vdata"];

/// The sysfs view of the block layer (test seam).
pub(super) trait BlockDevices {
    /// The canonical sysfs directory of `/sys/dev/block/<major>:<minor>`.
    fn node(&mut self, device: (u32, u32)) -> Option<String>;
    /// A small sysfs attribute file.
    fn read(&mut self, path: &str) -> Option<String>;
    fn is_dir(&mut self, path: &str) -> bool;
    fn exists(&mut self, path: &str) -> bool;
    /// Entry names of a directory (`None`: unreadable or too many).
    fn list(&mut self, path: &str) -> Option<Vec<String>>;
    /// `dmsetup table` of a device-mapper device.
    fn dm_table(&mut self, device: (u32, u32)) -> Option<String>;
}

fn parse_device(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.trim().split_once(':')?;
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    if !digits(major) || !digits(minor) { return None; }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// sd*, vd*, xvd* and nvme<c>n<n> whole disks (SCSI/SATA, virtio, Xen, NVMe).
fn physical_disk(name: &str) -> bool {
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = name.strip_prefix("nvme") {
        return rest.split_once('n').is_some_and(|(controller, namespace)| digits(controller) && digits(namespace));
    }
    ["xvd", "sd", "vd"].iter().any(|prefix| name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase())))
}

/// `dmsetup table` targets (design §4.3): only linear, crypt, striped and raid.
/// `Err(true)`: a thin, snapshot, cache or similar target; `Err(false)`: unknown or unreadable.
pub(super) fn dm_targets_clear(table: &str) -> std::result::Result<(), bool> {
    if table.len() > 64 * 1024 { return Err(false); }
    let mut any = false;
    for line in table.lines().filter(|line| !line.trim().is_empty()) {
        match line.split_whitespace().nth(2) {
            Some("linear" | "crypt" | "striped" | "raid") => any = true,
            Some("thin" | "thin-pool" | "snapshot" | "snapshot-origin" | "snapshot-merge" | "cache" | "writecache"
                | "era" | "vdo" | "dm-clone") => return Err(true),
            _ => return Err(false),
        }
    }
    if any { Ok(()) } else { Err(false) }
}

/// Review F7 (pure over the seam): walk the backing chain of `root` through
/// sysfs (`/sys/dev/block/<maj:min>`, `slaves/`, a partition's parent disk,
/// `dm/uuid` and the device-mapper table), bounded in depth and size. Any
/// thin/snapshot/cache layer refuses; so does any layer that cannot be
/// determined or is not a partition, md, device-mapper or physical disk.
pub(super) fn storage_chain(devices: &mut dyn BlockDevices, root: (u32, u32)) -> std::result::Result<(), &'static str> {
    let undetermined = STORAGE_UNDETERMINED;
    let mut pending = vec![(root, 0usize)];
    let mut seen: Vec<(u32, u32)> = Vec::new();
    while let Some((device, depth)) = pending.pop() {
        if depth > MAX_CHAIN_DEPTH { return Err(undetermined); }
        if seen.contains(&device) { continue; }
        if seen.len() >= MAX_CHAIN_DEVICES { return Err(undetermined); }
        seen.push(device);
        let node = devices.node(device).ok_or(undetermined)?;
        if !node.starts_with("/sys/devices/") || devices.read(&format!("{node}/dev")).and_then(|text| parse_device(&text)) != Some(device) {
            return Err(undetermined);
        }
        let slaves = format!("{node}/slaves");
        let mut below = Vec::new();
        if devices.is_dir(&slaves) {
            for name in devices.list(&slaves).ok_or(undetermined)? {
                below.push(devices.read(&format!("{slaves}/{name}/dev")).and_then(|text| parse_device(&text)).ok_or(undetermined)?);
            }
        }
        if devices.is_dir(&format!("{node}/dm")) {
            let uuid = devices.read(&format!("{node}/dm/uuid")).ok_or(undetermined)?;
            if NON_PREALLOCATED_UUID_SUFFIXES.iter().any(|suffix| uuid.trim().ends_with(suffix)) { return Err(STORAGE_THIN); }
            let table = devices.dm_table(device).ok_or(undetermined)?;
            dm_targets_clear(&table).map_err(|thin| if thin { STORAGE_THIN } else { undetermined })?;
            if below.is_empty() { return Err(undetermined); }
        } else if devices.exists(&format!("{node}/partition")) {
            let (parent, _) = node.rsplit_once('/').ok_or(undetermined)?;
            if !below.is_empty() { return Err(undetermined); }
            below.push(devices.read(&format!("{parent}/dev")).and_then(|text| parse_device(&text)).ok_or(undetermined)?);
        } else if devices.is_dir(&format!("{node}/md")) {
            if below.is_empty() { return Err(undetermined); }
        } else {
            let name = node.rsplit('/').next().unwrap_or("");
            if !below.is_empty() || !physical_disk(name) || !devices.exists(&format!("{node}/device")) { return Err(undetermined); }
        }
        pending.extend(below.into_iter().map(|next| (next, depth + 1)));
    }
    Ok(())
}

struct LiveBlockDevices;

impl BlockDevices for LiveBlockDevices {
    fn node(&mut self, (major, minor): (u32, u32)) -> Option<String> {
        // Only the kernel's sysfs answers (SYSFS_MAGIC).
        if statfs_type(c"/sys") != Some(0x6265_6572) { return None; }
        fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok()?.to_str().map(str::to_owned)
    }
    fn read(&mut self, path: &str) -> Option<String> {
        let mut text = String::new();
        fs::File::open(path).ok()?.take(4097).read_to_string(&mut text).ok()?;
        (text.len() <= 4096).then_some(text)
    }
    fn is_dir(&mut self, path: &str) -> bool { fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) }
    fn exists(&mut self, path: &str) -> bool { fs::symlink_metadata(path).is_ok() }
    fn list(&mut self, path: &str) -> Option<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path).ok()? {
            if names.len() >= MAX_CHAIN_DEVICES { return None; }
            names.push(entry.ok()?.file_name().into_string().ok()?);
        }
        Some(names)
    }
    fn dm_table(&mut self, (major, minor): (u32, u32)) -> Option<String> {
        let (major, minor) = (major.to_string(), minor.to_string());
        let output = command("dmsetup", &["table", "--major", &major, "--minor", &minor]).ok()?;
        if !output.status.success() { return None; }
        String::from_utf8(output.stdout).ok()
    }
}

// ── Service identities (review F2) ────────────────────────────────────────

const MAX_DATABASE_ROWS: usize = 100_000;
const GATEWAY_ONLY: &[&str] = &["duduclaw"];

/// The service accounts and their homes (the gateway and `layout::ACCOUNTS`).
fn service_accounts() -> Vec<(&'static str, &'static str)> {
    std::iter::once(("duduclaw", GATEWAY_HOME)).chain(layout::ACCOUNTS.iter().map(|name| (*name, "/nonexistent"))).collect()
}

fn service_names() -> Vec<&'static str> { service_accounts().into_iter().map(|(name, _)| name).collect() }

struct UserRow<'a> { name: &'a str, uid: u32, gid: u32, home: &'a str, shell: &'a str }
struct GroupRow<'a> { name: &'a str, gid: u32, members: &'a str }

fn numeric_id(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) { return None; }
    text.parse().ok()
}

/// Every line split into exactly `fields` fields; anything else is unparseable.
fn database_rows(text: &str, fields: usize) -> Option<Vec<Vec<&str>>> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let row: Vec<&str> = line.split(':').collect();
        if row.len() != fields || rows.len() >= MAX_DATABASE_ROWS { return None; }
        rows.push(row);
    }
    Some(rows)
}

/// Review F2 (pure): over the complete passwd and group databases as the
/// system resolves them, every present service account has exactly one name
/// for its uid, the expected home and a no-login shell; its gid belongs to
/// exactly one group, named like the account, with no members, and to no other
/// account's primary group; the account is no other group's member. An absent
/// account must not have a same-named group waiting for it; `required` names
/// must be present.
pub(super) fn identities_unaliased(passwd: &str, group: &str, required: &[&str]) -> std::result::Result<(), &'static str> {
    let users: Vec<UserRow> = database_rows(passwd, 7).ok_or(IDENTITY_ALIAS)?.into_iter()
        .map(|row| Some(UserRow { name: *row.first()?, uid: numeric_id(row.get(2)?)?, gid: numeric_id(row.get(3)?)?,
            home: *row.get(5)?, shell: *row.get(6)? }))
        .collect::<Option<_>>().ok_or(IDENTITY_ALIAS)?;
    let groups: Vec<GroupRow> = database_rows(group, 4).ok_or(IDENTITY_ALIAS)?.into_iter()
        .map(|row| Some(GroupRow { name: *row.first()?, gid: numeric_id(row.get(2)?)?, members: *row.get(3)? }))
        .collect::<Option<_>>().ok_or(IDENTITY_ALIAS)?;
    for (name, home) in service_accounts() {
        let named: Vec<&UserRow> = users.iter().filter(|user| user.name == name).collect();
        let user = match named.as_slice() {
            [] if required.contains(&name) => return Err(IDENTITY_ALIAS),
            [] => {
                if groups.iter().any(|group| group.name == name) { return Err(IDENTITY_ALIAS); }
                continue;
            }
            [user] => *user,
            _ => return Err(IDENTITY_ALIAS),
        };
        let own: Vec<&GroupRow> = groups.iter().filter(|group| group.gid == user.gid).collect();
        let own_ok = matches!(own.as_slice(), [group] if group.name == name && group.members.is_empty());
        if !own_ok || user.home != home || !matches!(user.shell, "/usr/sbin/nologin" | "/sbin/nologin")
            || users.iter().filter(|other| other.uid == user.uid).count() != 1
            || users.iter().any(|other| other.gid == user.gid && other.name != name)
            || groups.iter().filter(|group| group.name == name).count() != 1
            || groups.iter().any(|group| group.members.split(',').any(|member| member == name)) {
            return Err(IDENTITY_ALIAS);
        }
    }
    Ok(())
}

/// One review F2 check point, over `getent passwd` and `getent group` in full.
fn verify_identities(required: &[&str]) -> Result<()> {
    let database = |name: &str| -> Result<String> {
        let output = command("getent", &[name])?;
        if !output.status.success() { return Err(refused(IDENTITY_ALIAS)); }
        String::from_utf8(output.stdout).map_err(|_| refused(IDENTITY_ALIAS))
    };
    identities_unaliased(&database("passwd")?, &database("group")?, required).map_err(refused)
}

// ── S2 / S3 ───────────────────────────────────────────────────────────────

/// Plan S2 (fresh host): no gateway service is running. Unknown systemctl
/// answers are failures (same rule as `install`).
fn gateway_not_running() -> Result<()> {
    let output = command("systemctl", &["is-active", "--quiet", UNIT])?;
    match output.status.code() {
        Some(3 | 4) => Ok(()),
        Some(0) => Err(refused("偵測到執行中的 duduclaw-gateway.service；全新安裝不能在既有 Gateway 運作時進行。")),
        _ => Err(refused(SYSTEMCTL_FAILED)),
    }
}

/// The gateway service is active now (any other answer: not active).
fn gateway_active() -> bool {
    command("systemctl", &["is-active", "--quiet", UNIT]).is_ok_and(|output| output.status.code() == Some(0))
}

// ── S4 PublishPlatform ────────────────────────────────────────────────────

/// A directory this step needs: created with `mode` when absent; an existing
/// one (not this transaction's unless ledgered) is only checked, never chowned
/// or chmodded.
fn ensure_directory(path: &str, mode: u32) -> Result<()> {
    if exists(path)? {
        parents(Path::new(path))?;
        return trusted(Path::new(path), true);
    }
    mkdir(path, mode)
}

fn unit_file(name: &str) -> String { format!("/etc/systemd/system/{name}") }

fn publish_platform(context: &mut Context) -> Result<()> {
    // (a) Trust root, state, slot A, generation and `current` (R1). Called
    // WITHOUT the version lock: publish takes stage lock → version-lock →
    // update.lock itself (see LOCK ORDER above). Re-entrant. The trees it
    // creates are ledgered first (review F4).
    let stage = &context.stage;
    let digest = context.recorder.record(&TRUST_TREES.map(|tree| (tree, Kind::Tree)), || {
        duduclaw_updated::linux::trust_init::publish(stage)
            .map_err(|error| refused(format!("無法發布更新信任根與世代（{:?}）。", error.code())))
    })?;
    if digest != context.generation { return Err(refused(STAGE_UNVERIFIED)); }
    let manifest = readiness::authenticated_manifest(digest)?;
    let version = duduclaw_updated::version_text(manifest.version);
    if manifest.artifacts != context.stage.artifacts || version != duduclaw_gateway::updater::current_version() {
        return Err(refused(STAGE_UNVERIFIED));
    }
    let recorder = &mut context.recorder;
    with_version_lock(|| {
        // The protected-mode marker only narrows; rollback keeps it (not ledgered).
        write_mode_marker()?;
        // The floor intent before any program is published (same rule as install).
        recorder.record(&[(VERSION_PENDING, Kind::File)], || prepare_version(&version))?;
        // (b) Fixed-path programs, the frozen launcher and its receipt.
        publish_programs(&manifest, digest, recorder)?;
        // (c) Accounts, the gateway home and license, directories, unit files.
        let defs = String::from_utf8(read_root_file("/etc/login.defs", 1024 * 1024)?
            .ok_or_else(|| refused("無法讀取 /etc/login.defs。"))?).map_err(|_| refused("無法讀取 /etc/login.defs。"))?;
        layout::system_id_limit(&defs, "SYS_UID_MAX").map_err(refused)?;
        layout::system_id_limit(&defs, "SYS_GID_MAX").map_err(refused)?;
        // Review F2, check point 1: before any account is created.
        verify_identities(&[])?;
        let gateway = gateway_account(&defs)?;
        // Check point 2: after creating it (check point 3 is inside the two
        // functions below, right before their chown).
        verify_identities(GATEWAY_ONLY)?;
        gateway_home(gateway)?;
        install_license(gateway)?;
        if !layout::published_components(Path::new("/"), 0, 0)? { return Err(refused(STAGE_UNVERIFIED)); }
        let units: Vec<String> = layout::UNITS.iter().map(|(name, _)| unit_file(name)).collect();
        let mut items: Vec<(&str, Kind)> = LAYOUT_TREES.iter().map(|tree| (*tree, Kind::Tree)).collect();
        items.extend(units.iter().map(|unit| (unit.as_str(), Kind::File)));
        // Check point 1 for the helper accounts created below.
        verify_identities(GATEWAY_ONLY)?;
        recorder.record(&items, || {
            parse_runtime_tree(true)?;
            install_isolation_support_for(gateway, &defs)
        })?;
        // Check point 2 for the helper accounts (check point 3: S6, before they run).
        verify_identities(&service_names())?;
        spawnd_anchors(digest)?;
        // (d) The nightly update requester: files only; enabled at S6 (Q11).
        nightly_files(recorder)
    })
}

/// Same no-follow, owner, mode, link and hash rules as R4 item #3.
fn installed_matches(path: &str, artifact: &ReleaseArtifact, mode: u32) -> Result<bool> {
    parents(Path::new(path))?;
    let file = match fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o7777 != mode { return Ok(false); }
    Ok(duduclaw_updated::linux::files::verify_fd(file, artifact.size_bytes, artifact.sha256).is_ok())
}

/// Copy one verified artifact of the published generation to `path`
/// (temp + fsync + rename + directory fsync, root 0755), then read it back.
/// Programs come only from the generation published from the stage.
fn publish_program(generation: &Directory, artifact: &ReleaseArtifact, path: &str) -> Result<()> {
    if installed_matches(path, artifact, 0o755)? { return Ok(()); }
    let name = duduclaw_updated::publication::artifact_name(&artifact.install_target).map_err(|_| refused(STAGE_UNVERIFIED))?;
    let source = generation.read(&name).map_err(|_| refused(STAGE_UNVERIFIED))?;
    let mut source = duduclaw_updated::linux::files::verify_fd(source, artifact.size_bytes, artifact.sha256)
        .map_err(|_| refused(STAGE_UNVERIFIED))?;
    source.seek(SeekFrom::Start(0))?;
    atomic_install(path, 0o755, |target| {
        duduclaw_updated::linux::files::copy_artifact(&mut source, target, artifact).map_err(|_| refused(STAGE_UNVERIFIED))
    })?;
    if !installed_matches(path, artifact, 0o755)? {
        return Err(refused(format!("發布後讀回不符：{path}")));
    }
    Ok(())
}

/// Every fixed-path program (`layout::BINARIES` and the gateway).
fn program_paths() -> Vec<&'static str> { layout::BINARIES.iter().copied().chain(std::iter::once(BINARY)).collect() }

/// The signed artifact a fixed-path program is a copy of.
fn program_artifact<'a>(manifest: &'a ReleaseManifest, path: &str) -> Result<&'a ReleaseArtifact> {
    let target = if path == readiness::UPDATER_LAUNCHER { InstallTarget::UpdateSlot }
        else { readiness::component_target(path).ok_or_else(|| refused(STAGE_UNVERIFIED))? };
    let artifact = single_artifact(&manifest.artifacts, &target).ok_or_else(|| refused(STAGE_UNVERIFIED))?;
    // Every fixed-path program is root 0755 (layout); the generation's mode
    // must agree except for the slot artifact, whose launcher copy has its own receipt.
    if target != InstallTarget::UpdateSlot && artifact.mode != 0o755 { return Err(refused(STAGE_UNVERIFIED)); }
    Ok(artifact)
}

/// The frozen launcher's receipt of this generation (the time is not compared).
fn launcher_record(manifest: &ReleaseManifest, digest: Sha256Digest, now: u64) -> Result<readiness::LauncherReceiptRecord> {
    let slot = single_artifact(&manifest.artifacts, &InstallTarget::UpdateSlot).ok_or_else(|| refused(STAGE_UNVERIFIED))?;
    Ok(readiness::LauncherReceiptRecord { schema_version: 1, generation: digest, sha256: slot.sha256,
        size_bytes: slot.size_bytes, mode: 0o755, installed_at_unix: now })
}

fn publish_programs(manifest: &ReleaseManifest, digest: Sha256Digest, recorder: &mut LiveRecorder) -> Result<()> {
    use duduclaw_updated::linux::{CONFIG_ROOT, INSTALL_ROOT};
    let generation = Directory::open(Path::new(INSTALL_ROOT)).and_then(|install| install.child("generations"))
        .and_then(|generations| generations.child(&hex::encode(digest.0))).map_err(|_| refused(STAGE_UNVERIFIED))?;
    // Same parent set the single-binary install creates, plus the isolation
    // program directories (`layout::DIRECTORIES` re-applies their modes later).
    // Pre-existing directories are used as they are and never ledgered.
    let directories = [("/opt/duduclaw", Kind::Directory), ("/opt/duduclaw/bin", Kind::Directory),
        ("/usr/local/libexec", Kind::Directory), ("/usr/local/libexec/duduclaw", Kind::Directory),
        ("/usr/lib/duduclaw", Kind::Tree), ("/usr/libexec/duduclaw", Kind::Tree)];
    recorder.record(&directories, || {
        for (directory, _) in directories { ensure_directory(directory, 0o755)?; }
        // Inside the ledgered /usr/lib/duduclaw tree.
        ensure_directory("/usr/lib/duduclaw/current", 0o755)
    })?;
    let programs = program_paths();
    let items: Vec<(&str, Kind)> = programs.iter().map(|path| (*path, Kind::File)).collect();
    recorder.record(&items, || {
        for path in &programs { publish_program(&generation, program_artifact(manifest, path)?, path)?; }
        Ok(())
    })?;
    // The frozen launcher's receipt, for R4 item #3 (never overwritten; inside
    // the ledgered /etc/duduclaw-updated tree).
    let record = launcher_record(manifest, digest, now()?)?;
    match readiness::read_launcher_receipt()? {
        Some(existing) if existing == record.receipt() => (),
        Some(_) => return Err(refused("既有的更新服務啟動程式收據與這次安裝不符，已停止。")),
        None => Directory::open(Path::new(CONFIG_ROOT))
            .and_then(|config| config.atomic_json(readiness::LAUNCHER_RECEIPT_FILE, &record))
            .map_err(|_| refused("無法寫入更新服務啟動程式收據。"))?,
    }
    if readiness::read_launcher_receipt()? != Some(record.receipt()) {
        return Err(refused("更新服務啟動程式收據讀回不符。"));
    }
    // The root launcher of the gateway unit (single-binary layout path).
    recorder.record(&[(LAUNCHER, Kind::File)], || update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT).map(|_| ()))
}

pub(super) struct PasswdEntry { pub(super) uid: u32, pub(super) gid: u32, home: String, shell: String }

/// `getent passwd <name>`: `Ok(None)` only for "not found" (exit 2).
pub(super) fn passwd_entry(name: &str) -> Result<Option<PasswdEntry>> {
    let output = command("getent", &["passwd", name])?;
    match output.status.code() {
        Some(2) => return Ok(None),
        Some(0) => (),
        _ => return Err(refused("無法查詢服務帳號。")),
    }
    let text = String::from_utf8(output.stdout).map_err(|_| refused("無法查詢服務帳號。"))?;
    let fields: Vec<&str> = text.trim_end_matches('\n').split(':').collect();
    let [entry, _, uid, gid, _, home, shell] = fields.as_slice() else { return Err(refused("服務帳號資料格式不符。")) };
    if *entry != name { return Err(refused("服務帳號資料格式不符。")); }
    let uid = uid.parse::<u32>().map_err(|_| refused("服務帳號資料格式不符。"))?;
    let gid = gid.parse::<u32>().map_err(|_| refused("服務帳號資料格式不符。"))?;
    Ok(Some(PasswdEntry { uid, gid, home: (*home).to_owned(), shell: (*shell).to_owned() }))
}

/// The gateway account of the isolated layout: a static system account with
/// home `/var/lib/duduclaw` (created by `gateway_home`, not by useradd), no
/// login shell, a locked password, no supplementary group and no linger.
fn gateway_account(defs: &str) -> Result<(u32, u32)> {
    if passwd_entry(duduclaw_isolation::ids::GATEWAY_USER)?.is_none() {
        let uid_max = format!("SYS_UID_MAX={}", layout::system_id_limit(defs, "SYS_UID_MAX").map_err(refused)?.saturating_sub(1));
        let gid_max = format!("SYS_GID_MAX={}", layout::system_id_limit(defs, "SYS_GID_MAX").map_err(refused)?.saturating_sub(1));
        let created = command("useradd", &["--system", "--user-group", "--no-create-home", "--no-log-init",
            "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0", "--key", &uid_max, "--key", &gid_max,
            "--home-dir", GATEWAY_HOME, "--shell", "/usr/sbin/nologin", "--password", "!*", "duduclaw"])?;
        if !created.status.success() { return Err(refused("無法建立 duduclaw 服務帳號。")); }
    }
    let entry = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    if entry.home != GATEWAY_HOME || !matches!(entry.shell.as_str(), "/usr/sbin/nologin" | "/sbin/nologin") {
        return Err(refused("duduclaw 帳號的家目錄或登入設定不符。"));
    }
    layout::validate_system_ids(entry.uid, entry.gid, defs).map_err(refused)?;
    let names = command("id", &["-Gn", "duduclaw"])?;
    let ids = command("id", &["-G", "duduclaw"])?;
    if !names.status.success() || !ids.status.success() { return Err(refused("無法查詢服務帳號群組。")); }
    let names = String::from_utf8(names.stdout).map_err(|_| refused("無法查詢服務帳號群組。"))?;
    let ids = String::from_utf8(ids.stdout).map_err(|_| refused("無法查詢服務帳號群組。"))?;
    validate_account(entry.uid, entry.gid, &entry.shell, &names)?;
    if ids.split_whitespace().collect::<Vec<_>>() != vec![entry.gid.to_string().as_str()] {
        return Err(refused("duduclaw 帳號帶有額外群組。"));
    }
    let shadow = command("getent", &["shadow", "duduclaw"])?;
    if !shadow.status.success() { return Err(refused("無法確認服務帳號已鎖定。")); }
    let shadow = String::from_utf8(shadow.stdout).map_err(|_| refused("無法確認服務帳號已鎖定。"))?;
    let locked = shadow.trim_end_matches('\n').split(':').nth(1).is_some_and(|field| field.starts_with('!') || field.starts_with('*'));
    if !locked { return Err(refused("duduclaw 帳號的密碼未鎖定。")); }
    let linger = command("loginctl", &["disable-linger", "duduclaw"])?;
    if !linger.status.success() || exists("/var/lib/systemd/linger/duduclaw")? {
        return Err(refused("duduclaw 帳號仍可啟動背景登入工作階段。"));
    }
    Ok((entry.uid, entry.gid))
}

/// The private data directory, opened through a no-follow dirfd of the
/// root-owned home. `owner`: the expected uid/gid (checked when given).
pub(super) fn open_gateway_data(owner: Option<(u32, u32)>) -> Result<(fs::File, fs::File)> {
    let home = Path::new(GATEWAY_HOME);
    parents(home)?;
    trusted(home, true)?;
    let parent = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(home)?;
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), GATEWAY_DATA.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    let data = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = data.metadata()?;
    if !metadata.is_dir() || metadata.mode() & 0o7777 != 0o700 || metadata.dev() != parent.metadata()?.dev()
        || owner.is_some_and(|(uid, gid)| metadata.uid() != uid || metadata.gid() != gid) {
        return Err(refused("Gateway 資料目錄的擁有者或權限不符。"));
    }
    Ok((data, parent))
}

/// Names in a directory (bounded), without `.`/`..`.
pub(super) fn directory_names(directory: &fs::File, limit: usize) -> Result<Vec<CString>> {
    // SAFETY: duplicating a live descriptor; the copy is owned by the DIR stream below.
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: `duplicate` is a fresh directory descriptor; fdopendir takes ownership.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: fdopendir failed, so the descriptor is still ours.
        unsafe { libc::close(duplicate); }
        return Err(std::io::Error::last_os_error().into());
    }
    struct Stream(*mut libc::DIR);
    // SAFETY: the stream is open until this guard drops.
    impl Drop for Stream { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }
    let stream = Stream(stream);
    // SAFETY: rewind a stream opened from a duplicated (shared-offset) descriptor.
    unsafe { libc::rewinddir(stream.0); }
    let mut names = Vec::new();
    loop {
        // SAFETY: errno is thread-local; reset to tell end-of-stream from an error.
        unsafe { *libc::__errno_location() = 0; }
        // SAFETY: the stream is live.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) { return Err(error.into()); }
            break;
        }
        // SAFETY: readdir returned a valid entry whose name is NUL-terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name == c"." || name == c".." { continue; }
        if names.len() >= limit { return Err(refused("目錄項目超過上限。")); }
        names.push(name.to_owned());
    }
    Ok(names)
}

/// A root-owned or gateway-owned (the data directory's owner) regular license
/// file in `directory`, mode 0600 or 0644 (review F8), read once without following links.
fn read_data_file(directory: &fs::File, name: &CStr) -> Result<Vec<u8>> {
    let owner = directory.metadata()?.uid();
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW
        | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > readiness::MAX_LICENSE_BYTES
        || (metadata.uid() != 0 && metadata.uid() != owner) {
        return Err(refused(LICENSE_NOT_TEAM));
    }
    if !license_mode_ok(metadata.mode()) { return Err(refused(LICENSE_MODE)); }
    let mut bytes = Vec::new();
    file.take(readiness::MAX_LICENSE_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `/var/lib/duduclaw` root:root 0711 and `.duduclaw` duduclaw:duduclaw 0700
/// (`layout::migrated_home_is_private`), created through no-follow dirfds.
fn gateway_home((uid, gid): (u32, u32)) -> Result<()> {
    mkdir(GATEWAY_HOME, 0o711)?;
    let parent = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(GATEWAY_HOME)?;
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let created = unsafe { libc::mkdirat(parent.as_raw_fd(), GATEWAY_DATA.as_ptr(), 0o700) } == 0;
    if !created && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(std::io::Error::last_os_error().into());
    }
    if created {
        // SAFETY: live dirfd and a NUL-terminated single-component name.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), GATEWAY_DATA.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
        // SAFETY: a fresh descriptor owned here.
        let data = unsafe { fs::File::from_raw_fd(fd) };
        // Review F2, check point 3: immediately before the first chown to the account.
        verify_identities(GATEWAY_ONLY)?;
        // SAFETY: scalar arguments on a live descriptor.
        if unsafe { libc::fchown(data.as_raw_fd(), uid, gid) } != 0 || unsafe { libc::fchmod(data.as_raw_fd(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        data.sync_all()?;
        parent.sync_all()?;
    }
    open_gateway_data(Some((uid, gid)))?;
    if !layout::migrated_home_is_private(Path::new(GATEWAY_HOME), 0, 0, uid, gid)? {
        return Err(refused("Gateway 資料目錄的擁有者或權限不符。"));
    }
    Ok(())
}

/// Q9: the gateway needs its TEAM license where it reads it. Copy the inbox
/// license into the private data directory (owned by the account, 0600) when
/// the gateway has none; never replace an existing one.
fn install_license((uid, gid): (u32, u32)) -> Result<()> {
    let (data, _) = open_gateway_data(Some((uid, gid)))?;
    if !directory_names(&data, 4096)?.iter().all(|name| name.as_c_str() != LICENSE_NAME) {
        let bytes = read_data_file(&data, LICENSE_NAME)?;
        if !readiness::license_bytes_admit_isolation(&bytes)? { return Err(refused(LICENSE_NOT_TEAM)); }
        return Ok(());
    }
    let bytes = license_bytes()?.ok_or_else(|| refused(LICENSE_MISSING))?;
    if !readiness::license_bytes_admit_isolation(&bytes)? { return Err(refused(LICENSE_NOT_TEAM)); }
    // A temporary left by an interrupted attempt is untrusted: unlink by name.
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    if unsafe { libc::unlinkat(data.as_raw_fd(), LICENSE_TEMPORARY.as_ptr(), 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(data.as_raw_fd(), LICENSE_TEMPORARY.as_ptr(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    file.write_all(&bytes)?;
    // Review F2, check point 3: immediately before the chown to the account.
    verify_identities(GATEWAY_ONLY)?;
    // SAFETY: scalar arguments on a live descriptor.
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 || unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    drop(file);
    // SAFETY: live dirfds and NUL-terminated single-component names; never replaces.
    if unsafe { libc::syscall(libc::SYS_renameat2, data.as_raw_fd(), LICENSE_TEMPORARY.as_ptr(), data.as_raw_fd(),
        LICENSE_NAME.as_ptr(), libc::RENAME_NOREPLACE) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    data.sync_all()?;
    if read_data_file(&data, LICENSE_NAME)? != bytes { return Err(refused("授權檔寫入後讀回不符。")); }
    Ok(())
}

/// spawnd's root anchors and its root-only host publication (no entitlement
/// yet: employees are confirmed later through spawnd's own RPC).
fn spawnd_anchors(digest: Sha256Digest) -> Result<()> {
    let root = Directory::open(Path::new(SPAWND_ROOT)).map_err(|_| refused("隔離執行服務的狀態目錄不符。"))?;
    for name in SPAWND_ANCHORS {
        match root.child(name) {
            Ok(_) => (),
            Err(error) if update_missing(&error) => { root.create_child(name).map_err(|_| refused("無法建立隔離執行服務的目錄。"))?; }
            Err(_) => return Err(refused("隔離執行服務的目錄不符。")),
        }
        let metadata = fs::symlink_metadata(Path::new(SPAWND_ROOT).join(name))?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o700 {
            return Err(refused("隔離執行服務的目錄權限不符。"));
        }
    }
    let expected = serde_json::json!({ "version": 1, "generation": digest, "entitlements": [] });
    match root.bytes(SPAWND_PUBLICATION, 1024 * 1024) {
        Ok(bytes) => {
            let existing: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| refused("隔離執行服務的發布紀錄不符。"))?;
            if existing.get("version") != expected.get("version") || existing.get("generation") != expected.get("generation") {
                return Err(refused("隔離執行服務的發布紀錄屬於其他世代。"));
            }
        }
        Err(error) if update_missing(&error) =>
            root.atomic_json(SPAWND_PUBLICATION, &expected).map_err(|_| refused("無法寫入隔離執行服務的發布紀錄。"))?,
        Err(_) => return Err(refused("隔離執行服務的發布紀錄不符。")),
    }
    Ok(())
}

fn nightly_files(recorder: &mut LiveRecorder) -> Result<()> {
    recorder.record(&[("/usr/local/sbin", Kind::Directory)], || ensure_directory("/usr/local/sbin", 0o755))?;
    let units: Vec<String> = NIGHTLY_UNITS.iter().map(|(name, _)| unit_file(name)).collect();
    let mut items = vec![(NIGHTLY_HELPER, Kind::File)];
    items.extend(units.iter().map(|unit| (unit.as_str(), Kind::File)));
    recorder.record(&items, || {
        update_text_if_different(NIGHTLY_HELPER, 0o755, NIGHTLY_HELPER_TEXT)?;
        for (name, text) in NIGHTLY_UNITS { update_text_if_different(&unit_file(name), 0o644, text)?; }
        Ok(())
    })
}

// ── S5 DnsPolicy ──────────────────────────────────────────────────────────

/// `/etc/duduclaw/egress.json` for the profile ids the signed policy names.
/// The signed policy carries only the profile ids, so the per-profile values
/// are fixed here and only narrow: CONNECT to port 443 only, the egress
/// service's default limits, the DNS route of S1, bounded run budgets.
pub(super) fn egress_document(policy: &[ProfileId], route: DnsRoute, hosts: &[IpAddr]) -> Result<serde_json::Value> {
    if policy.is_empty() || policy.len() > 128
        || policy.iter().enumerate().any(|(index, id)| policy.iter().take(index).any(|other| other == id)) {
        return Err(refused(POLICY_REFUSED));
    }
    if hosts.len() > MAX_HOST_PUBLIC_ADDRESSES { return Err(refused(POLICY_REFUSED)); }
    let limits = serde_json::to_value(duduclaw_egress::budget::default_limits()).map_err(|_| refused(POLICY_REFUSED))?;
    let route = serde_json::to_value(route).map_err(|_| refused(POLICY_REFUSED))?;
    let profiles: Vec<serde_json::Value> = policy.iter().map(|id| serde_json::json!({
        "id": id, "allowed_connect_ports": EGRESS_PORTS, "dns_route": route.clone(), "limits": limits.clone(),
    })).collect();
    Ok(serde_json::json!({
        "profiles": profiles,
        "run_caps": { "total_bytes": EGRESS_RUN_TOTAL_BYTES, "total_connections": EGRESS_RUN_TOTAL_CONNECTIONS },
        "max_runs": EGRESS_MAX_RUNS,
        "host_public_addresses": hosts,
    }))
}

/// The written egress config (as the egress service parses it) must name
/// exactly the signed policy's profiles, in order, with the fixed values.
pub(super) fn check_egress(config: &duduclaw_egress::daemon::Config, policy: &[ProfileId], route: DnsRoute,
    hosts: &[IpAddr]) -> Result<()> {
    config.validate().map_err(|_| refused(POLICY_REFUSED))?;
    let profiles = config.profiles.as_slice();
    let limits = duduclaw_egress::budget::default_limits();
    let ports: Vec<u16> = EGRESS_PORTS.to_vec();
    if profiles.len() != policy.len() || config.host_public_addresses != hosts || config.max_runs != EGRESS_MAX_RUNS
        || config.run_caps.total_bytes != EGRESS_RUN_TOTAL_BYTES
        || config.run_caps.total_connections != EGRESS_RUN_TOTAL_CONNECTIONS
        || profiles.iter().zip(policy).any(|(profile, id)| profile.id != *id || profile.dns_route != route
            || profile.limits != limits
            || profile.allowed_connect_ports.as_slice().iter().map(|port| port.get()).collect::<Vec<_>>() != ports) {
        return Err(refused(POLICY_REFUSED));
    }
    Ok(())
}

/// The signed policy's egress profile ids, through spawnd's own strict reader.
fn signed_policy_profiles(context: &Context) -> Result<Vec<ProfileId>> {
    use duduclaw_updated::linux::INSTALL_ROOT;
    let manifest = readiness::authenticated_manifest(context.generation)?;
    let target = InstallTarget::RuntimeEntry { registry_id: POLICY_REGISTRY_ID.into() };
    let artifact = single_artifact(&manifest.artifacts, &target).ok_or_else(|| refused(POLICY_REFUSED))?;
    if artifact.mode != 0o444 || artifact.size_bytes == 0 || artifact.size_bytes > 1024 * 1024 { return Err(refused(POLICY_REFUSED)); }
    let name = duduclaw_updated::publication::artifact_name(&target).map_err(|_| refused(POLICY_REFUSED))?;
    let generation = Directory::open(Path::new(INSTALL_ROOT)).and_then(|install| install.child("generations"))
        .and_then(|generations| generations.child(&context.generation_hex)).map_err(|_| refused(POLICY_REFUSED))?;
    let bytes = generation.bytes(&name, 1024 * 1024).map_err(|_| refused(POLICY_REFUSED))?;
    if bytes.len() as u64 != artifact.size_bytes || duduclaw_updated::publication::manifest_digest(&bytes) != artifact.sha256 {
        return Err(refused(POLICY_REFUSED));
    }
    duduclaw_spawnd::installed::signed_policy_egress_profiles(&bytes).map_err(|_| refused(POLICY_REFUSED))
}

/// Write `bytes` to a root config (0644) unless it already holds exactly them.
fn write_root_config(path: &str, bytes: &[u8]) -> Result<()> {
    if read_root_file(path, 256 * 1024)?.as_deref() == Some(bytes) { return Ok(()); }
    atomic_install(path, 0o644, |file| { file.write_all(bytes)?; Ok(()) })
}

fn dns_policy(context: &mut Context, dns: &Dns) -> Result<()> {
    use duduclaw_egress::daemon::{BROKER_CONFIG_PATH, CONFIG_PATH};
    // Review F6: the S1 DNS decision still holds right before it is written.
    dns_route_unchanged(dns)?;
    let policy = signed_policy_profiles(context)?;
    let document = egress_document(&policy, dns.route, &dns.host_public_addresses)?;
    let bytes = serde_json::to_vec(&document).map_err(|_| refused(POLICY_REFUSED))?;
    if let (DnsRoute::DedicatedDnsBroker, Some(upstream)) = (dns.route, dns.upstream) {
        let broker = serde_json::to_vec(&serde_json::json!({ "upstream": upstream })).map_err(|_| refused(POLICY_REFUSED))?;
        context.recorder.record(&[(BROKER_CONFIG_PATH, Kind::File)], || write_root_config(BROKER_CONFIG_PATH, &broker))?;
    }
    context.recorder.record(&[(CONFIG_PATH, Kind::File)], || write_root_config(CONFIG_PATH, &bytes))?;
    dns_readback(context, dns)
}

/// S5's invariants: both configs as the services parse them, equal to the
/// signed policy and the S1 route (also the resume check, review F1).
fn dns_readback(context: &Context, dns: &Dns) -> Result<()> {
    use duduclaw_egress::daemon::{read_root_config, BrokerConfig, Config, BROKER_CONFIG_PATH, CONFIG_PATH};
    let policy = signed_policy_profiles(context)?;
    match (dns.route, dns.upstream) {
        (DnsRoute::DedicatedDnsBroker, Some(upstream)) => {
            let written: BrokerConfig = read_root_config(BROKER_CONFIG_PATH).map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
            written.validate().map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
            if written.upstream != upstream { return Err(refused("DNS 轉送服務設定讀回不符。")); }
        }
        (DnsRoute::ResolvedVarlink, None) => if exists(BROKER_CONFIG_PATH)? {
            return Err(refused("使用 systemd-resolved 的主機不應有 DNS 轉送服務設定；請檢查 /etc/duduclaw/dns-broker.json。"));
        },
        _ => return Err(refused(JOURNAL_DAMAGED)),
    }
    let written: Config = read_root_config(CONFIG_PATH).map_err(|_| refused("對外連線設定讀回失敗。"))?;
    check_egress(&written, &policy, dns.route, &dns.host_public_addresses)
}

// ── S6 StartServices ──────────────────────────────────────────────────────

fn wait_for(deadline: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    loop {
        if ready() { return true; }
        if Instant::now() >= end { return false; }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn enable_now(unit: &str) -> Result<()> {
    let output = command("systemctl", &["enable", "--now", unit])?;
    if !output.status.success() { return Err(refused(format!("無法啟動 {unit}。"))); }
    if !wait_for(READY_WAIT, || readiness::unit_loaded_active(unit).is_ok()) {
        return Err(refused(format!("{unit} 未在時限內進入執行狀態，或含有未驗證的覆寫設定。")));
    }
    Ok(())
}

/// Review F5: spawnd admits work only with a fresh root installation receipt
/// at [`INSTALLATION_RECEIPT`] (`duduclaw-spawnd` installed.rs). The design
/// and plan define no production producer of it (spawnd README "I1 round 3":
/// a root-harness receipt this tree does not implement), so a host without
/// one stops here, before any isolation service starts, with a fixed message
/// instead of a 30-second evidence timeout. The consumer is unchanged; a
/// present receipt is still validated only by spawnd.
fn require_installation_receipt() -> Result<()> {
    match fs::symlink_metadata(INSTALLATION_RECEIPT) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(refused(RECEIPT_PRODUCER_MISSING)),
        Err(error) => Err(error.into()),
    }
}

/// The units S6 starts, in order (the broker socket only on the broker route).
fn started_units(dns: &Dns) -> Vec<&'static str> {
    SERVICE_ORDER.iter().copied()
        .filter(|unit| *unit != "duduclaw-dns-broker.socket" || dns.route == DnsRoute::DedicatedDnsBroker)
        .chain(GATEWAY_READY_UNITS.iter().copied()).collect()
}

/// Enabled (or static, for units without `[Install]`).
fn unit_enabled(unit: &str) -> Result<()> {
    let output = command("systemctl", &["is-enabled", unit])?;
    let state = String::from_utf8(output.stdout).map_err(|_| refused(SYSTEMCTL_FAILED))?;
    if !output.status.success() || !matches!(state.trim(), "enabled" | "static") {
        return Err(refused(format!("{unit} 尚未啟用。")));
    }
    Ok(())
}

fn spawnd_evidence_valid(gid: u32) -> bool {
    duduclaw_spawnd::host::EvidenceReport::read(gid).is_ok_and(|report| report.check().is_ok())
}

fn updater_reports(generation: Sha256Digest) -> bool {
    let nonce = duduclaw_isolation::ids::RequestId(*uuid::Uuid::new_v4().as_bytes());
    duduclaw_updated::client::probe(nonce).is_ok_and(|evidence| evidence.generation == generation)
}

fn start_services(context: &mut Context, dns: &Dns) -> Result<()> {
    require_installation_receipt()?;
    // Review F6: volatile host conditions, right before the services first use them.
    services_preconditions(dns)?;
    // Review F2, check point 3 for the helper accounts: before they first run.
    verify_identities(&service_names())?;
    let output = command("systemctl", &["daemon-reload"])?;
    if !output.status.success() { return Err(refused(SYSTEMCTL_FAILED)); }
    let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    // systemd creates spawnd's runtime directory: ledgered before it can exist.
    context.recorder.intend(&[(SPAWND_RUNTIME, Kind::Tree)])?;
    for unit in started_units(dns) {
        enable_now(unit)?;
        if unit == "duduclaw-spawnd.service" && !wait_for(READY_WAIT, || spawnd_evidence_valid(gateway.gid)) {
            return Err(refused("隔離執行服務未在 30 秒內發布有效的主機證據。"));
        }
        if unit == "duduclaw-updated.service" && !wait_for(READY_WAIT, || updater_reports(context.generation)) {
            return Err(refused("更新服務未在 30 秒內回報與安裝相同的世代。"));
        }
    }
    context.recorder.confirm(&[SPAWND_RUNTIME])?;
    // Q11: the nightly update requester is enabled after install.
    enable_now(NIGHTLY_TIMER)
}

// ── Resume invariants (review F1) ─────────────────────────────────────────

/// A root-owned file holding exactly `expected` with `mode`.
fn text_installed(path: &str, mode: u32, expected: &str) -> Result<()> {
    if read_root_file(path, 256 * 1024)?.as_deref() != Some(expected.as_bytes())
        || fs::symlink_metadata(path)?.mode() & 0o7777 != mode {
        return Err(refused(PUBLISHED_MISMATCH));
    }
    Ok(())
}

/// S4's invariants: trust state and `current` bound to THIS generation, every
/// program equal to its signed artifact (digest, owner, mode), the launcher
/// receipt, launcher, version intent, accounts, gateway home and license,
/// directories, unit files, the parse runtime tree, spawnd's publication, the
/// nightly files, and no unconfirmed ledger intent left behind.
fn platform_published(context: &Context) -> Result<()> {
    let mismatch = || refused(PUBLISHED_MISMATCH);
    let current = duduclaw_updated::linux::activation::read_current().map_err(|_| mismatch())?;
    let state: duduclaw_updated::state::State = Directory::open(Path::new(duduclaw_updated::linux::STATE_ROOT))
        .and_then(|root| root.json("state.json")).map_err(|_| mismatch())?;
    state.validate().map_err(|_| mismatch())?;
    if current != context.generation || state.effective_digest() != context.generation || state.pending.is_some() {
        return Err(mismatch());
    }
    let manifest = readiness::authenticated_manifest(context.generation)?;
    let version = duduclaw_updated::version_text(manifest.version);
    if manifest.artifacts != context.stage.artifacts || version != duduclaw_gateway::updater::current_version() {
        return Err(mismatch());
    }
    for path in program_paths() {
        if !installed_matches(path, program_artifact(&manifest, path)?, 0o755)? { return Err(mismatch()); }
    }
    if readiness::read_launcher_receipt()? != Some(launcher_record(&manifest, context.generation, 0)?.receipt()) {
        return Err(mismatch());
    }
    text_installed(LAUNCHER, 0o755, LAUNCHER_TEXT)?;
    if !exists(duduclaw_core::protected_mode::MODE_MARKER)? { return Err(mismatch()); }
    let version = Some(version.to_owned());
    if read_version_file(VERSION_PENDING)? != version && read_version_file(VERSION_FLOOR)? != version { return Err(mismatch()); }
    verify_identities(&service_names())?;
    let gateway = passwd_entry("duduclaw")?.ok_or_else(mismatch)?;
    let (data, _) = open_gateway_data(Some((gateway.uid, gateway.gid)))?;
    if !layout::migrated_home_is_private(Path::new(GATEWAY_HOME), 0, 0, gateway.uid, gateway.gid)?
        || !readiness::license_bytes_admit_isolation(&read_data_file(&data, LICENSE_NAME)?)? {
        return Err(mismatch());
    }
    for (path, mode) in layout::DIRECTORIES {
        parents(Path::new(path))?;
        trusted(Path::new(path), true)?;
        if fs::symlink_metadata(path)?.mode() & 0o7777 != *mode { return Err(mismatch()); }
    }
    for (name, text) in layout::UNITS { text_installed(&unit_file(name), 0o644, text)?; }
    parse_runtime_tree(false)?;
    let spawnd = Directory::open(Path::new(SPAWND_ROOT)).map_err(|_| mismatch())?;
    for name in SPAWND_ANCHORS { spawnd.child(name).map_err(|_| mismatch())?; }
    let publication: serde_json::Value = serde_json::from_slice(&spawnd.bytes(SPAWND_PUBLICATION, 1024 * 1024)
        .map_err(|_| mismatch())?).map_err(|_| mismatch())?;
    if publication.get("generation") != Some(&serde_json::json!(context.generation)) { return Err(mismatch()); }
    text_installed(NIGHTLY_HELPER, 0o755, NIGHTLY_HELPER_TEXT)?;
    for (name, text) in NIGHTLY_UNITS { text_installed(&unit_file(name), 0o644, text)?; }
    // A creation whose identity was never recorded (interrupted) is re-run so it is.
    for entry in &context.recorder.ledger().entries {
        if entry.state == ledger::State::Intent && ledger::present(&entry.path)? && entry.path != duduclaw_core::root_key::KEY_PATH {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// S6's invariants: every started unit loaded from its fragment, active and
/// enabled; the helper identities unaliased; spawnd's evidence valid; the
/// update service reports THIS generation.
fn services_running(context: &Context, dns: &Dns) -> Result<()> {
    for unit in started_units(dns).into_iter().chain(std::iter::once(NIGHTLY_TIMER)) {
        readiness::unit_loaded_active(unit)?;
        unit_enabled(unit)?;
    }
    verify_identities(&service_names())?;
    let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    if !spawnd_evidence_valid(gateway.gid) || !updater_reports(context.generation) {
        return Err(refused(PUBLISHED_MISMATCH));
    }
    Ok(())
}

/// S8's invariants: the gateway unit, its recovery bound and launcher exactly
/// as published, the unit enabled, and active when `--enable-now` was asked.
fn gateway_published(context: &Context) -> Result<()> {
    let text = duduclaw_core::root_key::recovery::isolated_unit_text();
    text_installed(UNIT_PATH, 0o644, &text)?;
    text_installed(RECOVERY_BOUND, 0o644, &text)?;
    text_installed(LAUNCHER, 0o755, LAUNCHER_TEXT)?;
    unit_enabled(UNIT)?;
    if context.enable_now { readiness::unit_loaded_active(UNIT)?; }
    Ok(())
}

// ── S8 StartGateway / S9 Commit ───────────────────────────────────────────

fn start_gateway(context: &mut Context) -> Result<()> {
    let text = duduclaw_core::root_key::recovery::isolated_unit_text();
    let recorder = &mut context.recorder;
    recorder.intend(&[(UNIT_PATH, Kind::File), (RECOVERY_BOUND, Kind::File), (LAUNCHER, Kind::File)])?;
    invalidate_recovery_bound()?;
    update_text_if_different(UNIT_PATH, 0o644, &text)?;
    update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT)?;
    reload_and_certify_recovery_text(&text)?;
    recorder.confirm(&[UNIT_PATH, RECOVERY_BOUND, LAUNCHER])?;
    // Review F4: before the gateway can first start, the names its data
    // directory holds (the license) are recorded; rollback keeps exactly those.
    let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    let (data, _) = open_gateway_data(Some((gateway.uid, gateway.gid)))?;
    let keep = directory_names(&data, 64)?.into_iter()
        .map(|name| name.into_string().map_err(|_| refused("Gateway 資料目錄含有無法辨識的名稱。")))
        .collect::<Result<Vec<_>>>()?;
    recorder.contents(ledger::GATEWAY_DATA_PATH, keep)?;
    // The gateway's launcher creates the root key at its first start.
    recorder.intend(&[(duduclaw_core::root_key::KEY_PATH, Kind::File)])?;
    let output = command("systemctl", &["enable", UNIT])?;
    if !output.status.success() { return Err(refused(format!("無法啟用 {UNIT}。"))); }
    if context.enable_now {
        let output = command("systemctl", &["start", UNIT])?;
        if !output.status.success() { return Err(refused(format!("無法啟動 {UNIT}。"))); }
        let started = wait_for(Duration::from_secs(60), || readiness::unit_loaded_active(UNIT).is_ok());
        context.recorder.confirm(&[duduclaw_core::root_key::KEY_PATH])?;
        if !started { return Err(refused("Gateway 未在時限內進入執行狀態。")); }
        // The install reports success only when the full check passes.
        readiness::check(&mut readiness::InstalledComposite)?;
    }
    Ok(())
}

/// Plan S9. Review F1: never on the journal's word — S8's invariants, the
/// service identities and the readiness check must pass in this same run
/// (the full check when the gateway runs; otherwise the platform check, the
/// full check minus the gateway being active), then the floor is recorded.
fn commit(context: &mut Context) -> Result<()> {
    gateway_published(context)?;
    verify_identities(&service_names())?;
    if context.enable_now || gateway_active() {
        readiness::check(&mut readiness::InstalledComposite)?;
    } else {
        readiness::check_platform(&mut readiness::InstalledComposite)?;
    }
    let manifest = readiness::authenticated_manifest(context.generation)?;
    let version = duduclaw_updated::version_text(manifest.version);
    context.recorder.record(&[(VERSION_FLOOR, Kind::File)], || record_floor(&version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Sha256Digest { Sha256Digest([byte; 32]) }
    fn broker_dns() -> Dns {
        Dns { route: DnsRoute::DedicatedDnsBroker, upstream: Some("192.0.2.53:53".parse().unwrap()),
            host_public_addresses: vec!["203.0.113.7".parse().unwrap()] }
    }
    fn host() -> String { "ef".repeat(32) }
    fn journal() -> Journal { Journal::new(&"ab".repeat(32), &host(), broker_dns(), 100) }

    #[test]
    fn journal_rejects_unknown_fields_and_step_regression() {
        let start = journal();
        let bytes = serde_json::to_vec(&start).unwrap();
        assert_eq!(Journal::parse(&bytes).unwrap(), start);
        // R1's minimal reader sees the four fields it needs.
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["mode"], "install");
        assert_eq!(value["generation"], "ab".repeat(32));
        assert_eq!(value["host"], host());
        assert_eq!(value["committed"], false);
        assert_eq!(value["dns"]["route"], "dedicated_dns_broker");
        // Unknown or damaged fields refuse.
        let mutate = |change: fn(&mut serde_json::Value)| {
            let mut value = value.clone();
            change(&mut value);
            serde_json::to_vec(&value).unwrap()
        };
        for bad in [mutate(|v| v["extra"] = true.into()), mutate(|v| v["prior"]["extra"] = 1.into()),
            mutate(|v| v["dns"]["extra"] = 1.into()), mutate(|v| v["schema_version"] = 2.into()),
            mutate(|v| v["generation"] = "AB".repeat(32).into()), mutate(|v| v["generation"] = "ab".into()),
            mutate(|v| v["host"] = "ef".into()), mutate(|v| { v.as_object_mut().unwrap().remove("host"); }),
            mutate(|v| v["step"] = "unknown".into()), mutate(|v| v["committed"] = true.into()),
            mutate(|v| v["dns"]["upstream"] = serde_json::Value::Null),
            mutate(|v| v["prior"]["single_binary"] = true.into()), mutate(|v| v["step"] = "verified".into()),
            mutate(|v| v["phase"] = "entered".into()), mutate(|v| { v.as_object_mut().unwrap().remove("dns"); }),
            b"[]".to_vec(), Vec::new()] {
            assert!(Journal::parse(&bad).is_err(), "{}", String::from_utf8_lossy(&bad));
        }
        assert!(Journal::parse(&vec![b' '; MAX_JOURNAL_BYTES + 1]).is_err());
        // Forward one phase at a time; never back, never skip, never edit.
        let entered = start.at(Step::StopGateway, Phase::Entered);
        start.check_transition(&entered).unwrap();
        let completed = entered.at(Step::StopGateway, Phase::Completed);
        entered.check_transition(&completed).unwrap();
        assert!(completed.check_transition(&start).is_err(), "regression");
        assert!(completed.check_transition(&entered).is_err(), "regression");
        assert!(start.check_transition(&start.at(Step::ProcessesClear, Phase::Entered)).is_err(), "skip");
        assert!(entered.check_transition(&entered.at(Step::ProcessesClear, Phase::Completed)).is_err(), "skip");
        let mut edited = completed.at(Step::ProcessesClear, Phase::Entered);
        edited.generation = "cd".repeat(32);
        assert!(completed.check_transition(&edited).is_err(), "generation is immutable");
        let mut edited = completed.at(Step::ProcessesClear, Phase::Entered);
        edited.dns.host_public_addresses.clear();
        assert!(completed.check_transition(&edited).is_err(), "dns is immutable");
        let mut edited = completed.at(Step::ProcessesClear, Phase::Entered);
        edited.host = "cd".repeat(32);
        assert!(completed.check_transition(&edited).is_err(), "host is immutable");
        // Rollback can start from any uncommitted position, and nothing follows it.
        let rolling = completed.at(Step::Rollback, Phase::Entered);
        completed.check_transition(&rolling).unwrap();
        assert!(rolling.check_transition(&rolling.at(Step::ProcessesClear, Phase::Entered)).is_err());
        // Commit completes with committed:true and is final.
        let mut position = start.clone();
        while let Some(next) = position.step.next() {
            let entered = position.at(next, Phase::Entered);
            position.check_transition(&entered).unwrap();
            let done = entered.at(next, Phase::Completed);
            entered.check_transition(&done).unwrap();
            position = done;
        }
        assert!(position.committed && position.step == Step::Commit);
        assert!(position.check_transition(&position.at(Step::Rollback, Phase::Entered)).is_err());
        assert_eq!(Journal::parse(&serde_json::to_vec(&position).unwrap()).unwrap(), position);
    }

    #[test]
    fn rollback_refused_after_commit() {
        let open = journal().at(Step::StartServices, Phase::Entered);
        rollback_permitted(&open, None).unwrap();
        rollback_permitted(&open, Some(&"ab".repeat(32))).unwrap();
        assert!(rollback_permitted(&open, Some(&"cd".repeat(32))).is_err());
        let committed = journal().at(Step::Commit, Phase::Completed);
        assert!(committed.committed);
        assert!(rollback_permitted(&committed, None).is_err());
        assert!(rollback_permitted(&committed, Some(&"ab".repeat(32))).is_err());
        let mut upgrade = open.clone();
        upgrade.mode = JournalMode::Upgrade;
        assert!(rollback_permitted(&upgrade, None).is_err(), "R5 owns the upgrade rollback window (Q8)");
    }

    #[test]
    fn dns_route_selection_requires_upstream_without_resolved() {
        let upstream: SocketAddr = "192.0.2.53:53".parse().unwrap();
        let broker = |upstream| duduclaw_egress::daemon::BrokerConfig { upstream }.validate().is_ok();
        assert_eq!(dns_route(true, None, broker), Ok((DnsRoute::ResolvedVarlink, None)));
        assert!(dns_route(true, Some(upstream), broker).is_err(), "no upstream on resolved hosts");
        assert!(dns_route(false, None, broker).is_err(), "broker hosts need an upstream");
        assert_eq!(dns_route(false, Some(upstream), broker), Ok((DnsRoute::DedicatedDnsBroker, Some(upstream))));
        // A private upstream is the broker's only, never an egress exception.
        let private: SocketAddr = "10.0.0.2:53".parse().unwrap();
        assert_eq!(dns_route(false, Some(private), broker), Ok((DnsRoute::DedicatedDnsBroker, Some(private))));
        for bad in ["192.0.2.53:5353", "0.0.0.0:53", "224.0.0.1:53", "255.255.255.255:53"] {
            assert!(dns_route(false, Some(bad.parse().unwrap()), broker).is_err(), "{bad}");
        }
        // A resumed run must use the decisions of its first run.
        let recorded = broker_dns();
        reconcile(&recorded, None, &[]).unwrap();
        reconcile(&recorded, Some(upstream), &recorded.host_public_addresses).unwrap();
        assert!(reconcile(&recorded, Some(private), &[]).is_err());
        assert!(reconcile(&recorded, None, &["198.51.100.1".parse().unwrap()]).is_err());
    }

    #[test]
    fn egress_profiles_must_match_signed_policy() {
        use duduclaw_egress::daemon::Config;
        let policy = [ProfileId(digest(1)), ProfileId(digest(2))];
        let hosts: Vec<IpAddr> = vec!["203.0.113.7".parse().unwrap()];
        let document = egress_document(&policy, DnsRoute::ResolvedVarlink, &hosts).unwrap();
        // The egress service's own parser and validator accept it.
        let config: Config = serde_json::from_slice(&serde_json::to_vec(&document).unwrap()).unwrap();
        check_egress(&config, &policy, DnsRoute::ResolvedVarlink, &hosts).unwrap();
        // Any difference from the signed policy refuses.
        assert!(check_egress(&config, &policy[..1], DnsRoute::ResolvedVarlink, &hosts).is_err());
        assert!(check_egress(&config, &[policy[1], policy[0]], DnsRoute::ResolvedVarlink, &hosts).is_err());
        assert!(check_egress(&config, &[policy[0], ProfileId(digest(3))], DnsRoute::ResolvedVarlink, &hosts).is_err());
        assert!(check_egress(&config, &policy, DnsRoute::DedicatedDnsBroker, &hosts).is_err());
        assert!(check_egress(&config, &policy, DnsRoute::ResolvedVarlink, &[]).is_err());
        let mut widened = document.clone();
        widened["profiles"][0]["allowed_connect_ports"] = serde_json::json!([443, 80]);
        let widened: Config = serde_json::from_slice(&serde_json::to_vec(&widened).unwrap()).unwrap();
        assert!(check_egress(&widened, &policy, DnsRoute::ResolvedVarlink, &hosts).is_err());
        // No egress profile, duplicates or too many addresses: nothing is written.
        assert!(egress_document(&[], DnsRoute::ResolvedVarlink, &[]).is_err());
        assert!(egress_document(&[policy[0], policy[0]], DnsRoute::ResolvedVarlink, &[]).is_err());
        let many: Vec<IpAddr> = (0..=MAX_HOST_PUBLIC_ADDRESSES).map(|index| IpAddr::from([198, 51, 100, index as u8])).collect();
        assert!(egress_document(&policy, DnsRoute::ResolvedVarlink, &many).is_err());
    }

    #[test]
    fn preflight_parsers_fail_closed() {
        assert_eq!(systemd_major("systemd 249 (249.11-0ubuntu3.12)\n+PAM +AUDIT"), Some(249));
        assert_eq!(systemd_major("systemd 255 (255.4-1ubuntu8)"), Some(255));
        assert_eq!(systemd_major("not systemd 300"), None);
        assert_eq!(kernel_version("5.15.0-1120-gcp\n"), Some((5, 15)));
        assert_eq!(kernel_version("6.1.0-54-cloud-amd64"), Some((6, 1)));
        assert_eq!(kernel_version("garbage"), None);
        assert!(supported_distribution("ID=ubuntu\nVERSION_ID=\"22.04\"\n"));
        assert!(supported_distribution("ID=debian\nVERSION_ID=\"12\"\n"));
        assert!(!supported_distribution("ID=ubuntu\nVERSION_ID=\"20.04\"\n"));
        assert!(!supported_distribution("ID=fedora\nVERSION_ID=40\n"));
        assert!(!subordinate_overlap("ubuntu:100000:65536\n"));
        assert!(subordinate_overlap("svc:70000:1\n"));
        assert!(subordinate_overlap("svc:60000:20000\n"));
        assert!(subordinate_overlap("broken line\n"));
        assert!(login_range_clear("UID_MIN 1000\nUID_MAX 60000\n", "UID"));
        assert!(login_range_clear("", "UID"));
        assert!(!login_range_clear("UID_MAX 80000\n", "UID"));
        assert!(!login_range_clear("UID_MAX many\n", "UID"));
        assert!(enumeration_overlap("emp:x:70001:70001::/:/bin/false\n"));
        assert!(!enumeration_overlap("root:x:0:0:root:/root:/bin/bash\n"));
        assert!(external_nss("passwd: files sss\ngroup: files\n"));
        assert!(external_nss("group:  files ldap # remote\n"));
        assert!(!external_nss("passwd: files systemd\n# passwd: ldap\nhosts: files dns ldap\n"));
        assert!(sshd_includes_dropins("Include /etc/ssh/sshd_config.d/*.conf\nPort 22\n"));
        assert!(!sshd_includes_dropins("#Include /etc/ssh/sshd_config.d/*.conf\n"));
        assert!(allow_list_clear("alice\nbob\n"));
        assert!(!allow_list_clear("duduclaw-emp-70001\n"));
        assert!(!allow_list_clear("ALL\n"));
        assert_eq!(parse_generation(&"0f".repeat(32)), Some(Sha256Digest([0x0f; 32])));
        for bad in ["0F".repeat(32), "0f".repeat(31), format!("{}g", "0".repeat(63)), String::new()] {
            assert_eq!(parse_generation(&bad), None, "{bad}");
        }
        assert_eq!(Step::Verified.number(), 0);
        assert_eq!(Step::Commit.number(), 9);
        assert_eq!(Step::Commit.next(), None);
        assert_eq!(Step::Rollback.next(), None);
    }

    /// A host whose step invariants are scripted (review F1).
    #[derive(Default)]
    struct ScriptedHost { broken: Vec<Step>, refuse: Vec<Step>, performed: Vec<Step> }

    impl Steps for ScriptedHost {
        fn holds(&mut self, step: Step) -> Result<bool> { Ok(!self.broken.contains(&step)) }
        fn perform(&mut self, step: Step) -> Result<()> {
            if self.refuse.contains(&step) { return Err(refused("refused")); }
            self.broken.retain(|broken| *broken != step);
            self.performed.push(step);
            Ok(())
        }
    }

    fn drive_scripted(journal: &mut Journal, host: &mut ScriptedHost) -> (Result<()>, Vec<(Step, Phase)>) {
        let mut writes = Vec::new();
        let result = drive(journal, host, &mut |previous: &Journal, next: &Journal| {
            previous.check_transition(next)?;
            writes.push((next.step, next.phase));
            Ok(())
        }, Step::Commit);
        (result, writes)
    }

    #[test]
    fn forged_journal_completion_is_reestablished_never_trusted() {
        // A schema-valid journal claiming S8 complete on a host where S4's invariants do not hold.
        let forged = journal().at(Step::StartGateway, Phase::Completed);
        forged.validate().unwrap();
        let mut host = ScriptedHost { broken: vec![Step::PublishPlatform], ..Default::default() };
        let mut position = forged.clone();
        let (result, writes) = drive_scripted(&mut position, &mut host);
        result.unwrap();
        // S4 and every later claimed step re-ran before the commit was even entered.
        assert_eq!(host.performed, vec![Step::PublishPlatform, Step::DnsPolicy, Step::StartServices, Step::Readiness,
            Step::StartGateway, Step::Commit]);
        assert_eq!(writes, vec![(Step::Commit, Phase::Entered), (Step::Commit, Phase::Completed)]);
        assert!(position.committed);
        // Where a re-run refuses, nothing after it runs, nothing is written and nothing is committed.
        let mut host = ScriptedHost { broken: vec![Step::PublishPlatform], refuse: vec![Step::StartServices], ..Default::default() };
        let mut position = forged.clone();
        let (result, writes) = drive_scripted(&mut position, &mut host);
        assert!(result.is_err());
        assert_eq!(host.performed, vec![Step::PublishPlatform, Step::DnsPolicy]);
        assert!(writes.is_empty() && position == forged && !position.committed);
        // A gateway runs that this transaction did not publish: S2 is a pure check and refuses.
        let mut host = ScriptedHost { broken: vec![Step::StopGateway], refuse: vec![Step::StopGateway], ..Default::default() };
        let mut position = forged.clone();
        let (result, writes) = drive_scripted(&mut position, &mut host);
        assert!(result.is_err() && host.performed.is_empty() && writes.is_empty() && !position.committed);
        // An entered step: the steps before it are re-established, the entered one runs anyway.
        let mut host = ScriptedHost { broken: vec![Step::DnsPolicy], ..Default::default() };
        let mut position = journal().at(Step::Readiness, Phase::Entered);
        drive_scripted(&mut position, &mut host).0.unwrap();
        assert_eq!(host.performed, vec![Step::DnsPolicy, Step::StartServices, Step::Readiness, Step::StartGateway, Step::Commit]);
        // Everything holds: nothing is re-run; the commit (which itself requires readiness) runs.
        let mut host = ScriptedHost::default();
        let mut position = forged;
        drive_scripted(&mut position, &mut host).0.unwrap();
        assert_eq!(host.performed, vec![Step::Commit]);
    }

    #[test]
    fn journal_for_another_generation_or_host_is_refused() {
        let recorded = journal();
        bound_to(&recorded, &"ab".repeat(32), &host()).unwrap();
        let other_host = bound_to(&recorded, &"ab".repeat(32), &"01".repeat(32)).unwrap_err().to_string();
        assert!(other_host.contains("另一台主機"), "{other_host}");
        let other_generation = bound_to(&recorded, &"cd".repeat(32), &host()).unwrap_err().to_string();
        assert!(other_generation.contains("rollback"), "{other_generation}");
        let mut upgrade = recorded.clone();
        upgrade.mode = JournalMode::Upgrade;
        assert!(bound_to(&upgrade, &"ab".repeat(32), &host()).is_err());
        // The machine-id binding: strict format, fixed domain, distinct hosts differ.
        let id = "0123456789abcdef0123456789abcdef";
        let digest = host_digest_of(&format!("{id}\n")).unwrap();
        assert_eq!(host_digest_of(id), Some(digest.clone()));
        assert!(parse_generation(&digest).is_some());
        assert_ne!(host_digest_of("fedcba9876543210fedcba9876543210"), Some(digest));
        for bad in ["", "0123456789ABCDEF0123456789abcdef", "0123456789abcdef", "0123456789abcdef0123456789abcdef\n\n",
            "uninitialized\n"] {
            assert_eq!(host_digest_of(bad), None, "{bad:?}");
        }
    }

    fn unit_options(upstream: Option<SocketAddr>) -> ContinueOptions {
        ContinueOptions::from_args(super::super::super::BootstrapContinueArgs { mode: "install".into(),
            generation: "ab".repeat(32), dns_upstream: upstream, host_public_address: Vec::new(), enable_now: false }).unwrap()
    }

    #[test]
    fn resume_reruns_host_preflight_every_time() {
        let recorded = journal();
        let options = unit_options(None);
        // A resumed run still runs S1, with the journal's DNS choices, and stops when it fails.
        let mut asked = None;
        let result = begin(Some(recorded.clone()), &options, &host(), 5, |upstream, hosts| {
            asked = Some((upstream, hosts.to_vec()));
            Err(refused("S1 failed"))
        });
        assert!(result.is_err());
        assert_eq!(asked, Some((recorded.dns.upstream, recorded.dns.host_public_addresses.clone())));
        // Passing S1 with the recorded decision resumes the same journal (not a new one).
        let (resumed, fresh) = begin(Some(recorded.clone()), &options, &host(), 5, |_, _| Ok(broker_dns())).unwrap();
        assert!(!fresh && resumed == recorded);
        // A host whose DNS decision changed since the first run refuses.
        let resolved = Dns { route: DnsRoute::ResolvedVarlink, upstream: None,
            host_public_addresses: broker_dns().host_public_addresses };
        assert!(begin(Some(recorded.clone()), &options, &host(), 5, |_, _| Ok(resolved)).is_err());
        // Given options must equal the recorded ones.
        let changed = unit_options(Some("198.51.100.53:53".parse().unwrap()));
        assert!(begin(Some(recorded), &changed, &host(), 5, |_, _| Ok(broker_dns())).is_err());
        // A fresh run records S1's decision and this host.
        let (created, fresh) = begin(None, &options, &host(), 5, |_, _| Ok(broker_dns())).unwrap();
        assert!(fresh && created.host == host() && created.step == Step::HostPreflight && created.phase == Phase::Completed);
        created.validate().unwrap();
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
duduclaw:x:990:990::/var/lib/duduclaw:/usr/sbin/nologin\n\
duduclaw-runsup:x:989:989::/nonexistent:/usr/sbin/nologin\n\
duduclaw-egress:x:988:988::/nonexistent:/usr/sbin/nologin\n\
duduclaw-dl:x:987:987::/nonexistent:/usr/sbin/nologin\n\
duduclaw-dns:x:986:986::/nonexistent:/usr/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nduduclaw:x:990:\nduduclaw-runsup:x:989:\nduduclaw-egress:x:988:\n\
duduclaw-dl:x:987:\nduduclaw-dns:x:986:\nsudo:x:27:alice\n";

    #[test]
    fn service_identity_aliases_refused_at_every_check_point() {
        let all = service_names();
        // The three check points: before creating accounts, after creating them,
        // immediately before the first chown or service start that uses them.
        let points: [&[&str]; 3] = [&[], GATEWAY_ONLY, &all];
        for required in points { identities_unaliased(PASSWD, GROUP, required).unwrap(); }
        let cases: Vec<(String, String)> = vec![
            // Another account sharing the gateway's uid.
            (format!("{PASSWD}backup2:x:990:34::/var/backups:/usr/sbin/nologin\n"), GROUP.into()),
            // Another group sharing the gateway's gid.
            (PASSWD.into(), format!("{GROUP}staff2:x:990:\n")),
            // Another account whose primary group is the gateway's.
            (format!("{PASSWD}alice:x:1000:990::/home/alice:/bin/bash\n"), GROUP.into()),
            // Members in the gateway's group; a service account in another group.
            (PASSWD.into(), GROUP.replace("duduclaw:x:990:\n", "duduclaw:x:990:alice\n")),
            (PASSWD.into(), GROUP.replace("sudo:x:27:alice", "sudo:x:27:alice,duduclaw-dns")),
            // A helper account's uid alias.
            (format!("{PASSWD}shadow2:x:987:42::/nonexistent:/usr/sbin/nologin\n"), GROUP.into()),
            // Duplicate name, wrong home, malformed or non-numeric ids.
            (format!("{PASSWD}duduclaw:x:991:991::/var/lib/duduclaw:/usr/sbin/nologin\n"), GROUP.into()),
            (PASSWD.replace("990::/var/lib/duduclaw", "990::/home/duduclaw"), GROUP.into()),
            (PASSWD.replace("duduclaw:x:990:990", "duduclaw:x:+990:990"), GROUP.into()),
            (format!("{PASSWD}garbage\n"), GROUP.into()),
            (PASSWD.into(), format!("{GROUP}broken:x\n")),
        ];
        for (passwd, group) in &cases {
            for required in points {
                assert!(identities_unaliased(passwd, group, required).is_err(), "{required:?}\n{passwd}\n{group}");
            }
        }
        // Before the accounts exist: nothing required, but no group may wait for one.
        let (passwd, group) = ("root:x:0:0:root:/root:/bin/bash\n", "root:x:0:\n");
        identities_unaliased(passwd, group, &[]).unwrap();
        assert!(identities_unaliased(passwd, group, GATEWAY_ONLY).is_err(), "created account missing");
        assert!(identities_unaliased(passwd, "root:x:0:\nduduclaw:x:990:\n", &[]).is_err());
    }

    #[derive(Default)]
    struct FakeSysfs { nodes: Vec<((u32, u32), String)>, files: Vec<(String, String)>, dirs: Vec<String>,
        tables: Vec<((u32, u32), String)> }

    impl FakeSysfs {
        fn file(&mut self, path: &str, text: &str) { self.files.push((path.into(), text.into())); }
        fn dir(&mut self, path: &str) { self.dirs.push(path.into()); }
        fn device(&mut self, (major, minor): (u32, u32), node: &str) {
            self.nodes.push(((major, minor), node.into()));
            self.file(&format!("{node}/dev"), &format!("{major}:{minor}\n"));
        }
    }

    impl BlockDevices for FakeSysfs {
        fn node(&mut self, device: (u32, u32)) -> Option<String> {
            self.nodes.iter().find(|(id, _)| *id == device).map(|(_, node)| node.clone())
        }
        fn read(&mut self, path: &str) -> Option<String> {
            self.files.iter().find(|(file, _)| file == path).map(|(_, text)| text.clone())
        }
        fn is_dir(&mut self, path: &str) -> bool { self.dirs.iter().any(|dir| dir == path) }
        fn exists(&mut self, path: &str) -> bool { self.is_dir(path) || self.files.iter().any(|(file, _)| file == path) }
        fn list(&mut self, path: &str) -> Option<Vec<String>> {
            let prefix = format!("{path}/");
            Some(self.dirs.iter().filter_map(|dir| dir.strip_prefix(prefix.as_str()))
                .filter(|rest| !rest.contains('/')).map(str::to_owned).collect())
        }
        fn dm_table(&mut self, device: (u32, u32)) -> Option<String> {
            self.tables.iter().find(|(id, _)| *id == device).map(|(_, table)| table.clone())
        }
    }

    const DISK: &str = "/sys/devices/pci0000:00/0000:00:04.0/virtio1/block/vda";

    fn disk_with_partition() -> FakeSysfs {
        let mut sysfs = FakeSysfs::default();
        sysfs.device((252, 0), DISK);
        sysfs.file(&format!("{DISK}/device"), "");
        sysfs.device((252, 1), &format!("{DISK}/vda1"));
        sysfs.file(&format!("{DISK}/vda1/partition"), "1\n");
        sysfs
    }

    /// Device-mapper node `dm-<minor>` over one slave, with `uuid` and `table`.
    fn dm(sysfs: &mut FakeSysfs, minor: u32, uuid: &str, table: &str, slave: (&str, (u32, u32))) {
        let (name, (slave_major, slave_minor)) = slave;
        let node = format!("/sys/devices/virtual/block/dm-{minor}");
        sysfs.device((253, minor), &node);
        sysfs.dir(&format!("{node}/dm"));
        sysfs.file(&format!("{node}/dm/uuid"), &format!("{uuid}\n"));
        sysfs.dir(&format!("{node}/slaves"));
        sysfs.dir(&format!("{node}/slaves/{name}"));
        sysfs.file(&format!("{node}/slaves/{name}/dev"), &format!("{slave_major}:{slave_minor}\n"));
        sysfs.tables.push(((253, minor), table.into()));
    }

    #[test]
    fn storage_chain_refuses_thin_and_undetermined() {
        // A partition of a virtio disk; a linear LV over it; crypt over that.
        let mut sysfs = disk_with_partition();
        storage_chain(&mut sysfs, (252, 1)).unwrap();
        dm(&mut sysfs, 0, "LVM-aaaa", "0 2048 linear 252:1 2048\n", ("vda1", (252, 1)));
        storage_chain(&mut sysfs, (253, 0)).unwrap();
        dm(&mut sysfs, 5, "CRYPT-LUKS2-bbbb-root", "0 2048 crypt aes-xts-plain64 :64:logon:x 0 253:0 32768\n", ("dm-0", (253, 0)));
        storage_chain(&mut sysfs, (253, 5)).unwrap();
        // LVM thin (ext4/XFS pass the statfs check there): the thin LV and its pool are refused.
        let mut thin = disk_with_partition();
        dm(&mut thin, 2, "LVM-pool-tpool", "0 4096 thin-pool 253:3 253:4 128 0\n", ("vda1", (252, 1)));
        dm(&mut thin, 1, "LVM-thinlv", "0 2048 thin 253:2 1\n", ("dm-2", (253, 2)));
        assert_eq!(storage_chain(&mut thin, (253, 1)), Err(STORAGE_THIN));
        assert_eq!(storage_chain(&mut thin, (253, 2)), Err(STORAGE_THIN));
        // Crypt over a thin LV: refused at the lower layer.
        dm(&mut thin, 6, "CRYPT-LUKS2-cccc-x", "0 2048 crypt aes 0 253:1 0\n", ("dm-1", (253, 1)));
        assert_eq!(storage_chain(&mut thin, (253, 6)), Err(STORAGE_THIN));
        // A thin-pool sub-volume name is refused whatever its table says.
        let mut named = disk_with_partition();
        dm(&mut named, 3, "LVM-pool-tpool", "0 2048 linear 252:1 0\n", ("vda1", (252, 1)));
        assert_eq!(storage_chain(&mut named, (253, 3)), Err(STORAGE_THIN));
        for target in ["snapshot", "cache", "era", "writecache"] {
            let mut other = disk_with_partition();
            dm(&mut other, 4, "LVM-x", &format!("0 2048 {target} 252:1 0\n"), ("vda1", (252, 1)));
            assert_eq!(storage_chain(&mut other, (253, 4)), Err(STORAGE_THIN), "{target}");
        }
        // Undetermined: no sysfs node, an unknown or unreadable dm table, a dev
        // mismatch, an unsupported device type, a disk without its device link.
        let mut sysfs = disk_with_partition();
        assert_eq!(storage_chain(&mut sysfs, (8, 0)), Err(STORAGE_UNDETERMINED));
        dm(&mut sysfs, 7, "LVM-y", "0 2048 mystery 252:1 0\n", ("vda1", (252, 1)));
        assert_eq!(storage_chain(&mut sysfs, (253, 7)), Err(STORAGE_UNDETERMINED));
        sysfs.tables.retain(|(id, _)| *id != (253, 7));
        assert_eq!(storage_chain(&mut sysfs, (253, 7)), Err(STORAGE_UNDETERMINED));
        let mut wrong = disk_with_partition();
        let partition_dev = format!("{DISK}/vda1/dev");
        wrong.files.retain(|(path, _)| *path != partition_dev);
        wrong.file(&partition_dev, "252:9\n");
        assert_eq!(storage_chain(&mut wrong, (252, 1)), Err(STORAGE_UNDETERMINED));
        let mut loopback = FakeSysfs::default();
        loopback.device((7, 0), "/sys/devices/virtual/block/loop0");
        assert_eq!(storage_chain(&mut loopback, (7, 0)), Err(STORAGE_UNDETERMINED));
        let mut bare = FakeSysfs::default();
        bare.device((252, 0), DISK);
        assert_eq!(storage_chain(&mut bare, (252, 0)), Err(STORAGE_UNDETERMINED));
        assert_eq!(dm_targets_clear(""), Err(false));
        assert_eq!(dm_targets_clear("0 10 linear 8:1 0\n10 10 striped 2 128 8:2 0 8:3 0\n"), Ok(()));
    }

    #[test]
    fn license_mode_must_be_0600_or_0644() {
        use std::os::unix::fs::PermissionsExt;
        assert!(license_mode_ok(0o100_600) && license_mode_ok(0o100_644) && license_mode_ok(0o600));
        for mode in [0o640, 0o604, 0o660, 0o666, 0o400, 0o4600, 0o700, 0o755] {
            assert!(!license_mode_ok(mode), "{mode:o}");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("license.json");
        fs::write(&path, b"{}").unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        for (mode, readable) in [(0o600, true), (0o644, true), (0o640, false), (0o666, false), (0o400, false)] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(read_data_file(&dir, LICENSE_NAME).is_ok(), readable, "{mode:o}");
        }
    }

    // ── Root tests (disposable VM; plan §6) ───────────────────────────────
    //
    // Prerequisites: DUDUCLAW_ISO_DISPOSABLE=1; a test-key bundle staged by
    // `duduclaw-bootstrap install` (whose verified gateway then refused or was
    // interrupted before S2 is fine) and its generation in
    // ISO_BOOTSTRAP_GENERATION; a TEAM test license in /root/duduclaw-migrate;
    // a supported distribution with systemd-resolved (or ISO_DNS_UPSTREAM).
    // The test binary is not the staged gateway, so `verify_self` is off.
    // Tests that pass S6 also need a root harness renewing
    // /run/duduclaw-isolation-evidence/installation.json (review F5: no
    // production producer exists; without it S6 refuses with a fixed message).

    fn disposable_generation() -> String {
        assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
            "set DUDUCLAW_ISO_DISPOSABLE=1 only on a disposable VM");
        std::env::var("ISO_BOOTSTRAP_GENERATION").expect("set ISO_BOOTSTRAP_GENERATION to the staged generation")
    }

    fn options(mode: &str, generation: &str) -> ContinueOptions {
        ContinueOptions::from_args(super::super::super::BootstrapContinueArgs { mode: mode.into(),
            generation: generation.into(), dns_upstream: std::env::var("ISO_DNS_UPSTREAM").ok().map(|value| value.parse().unwrap()),
            host_public_address: Vec::new(), enable_now: mode == "install" }).unwrap()
    }

    fn rollback_options(generation: &str) -> ContinueOptions {
        ContinueOptions::from_args(super::super::super::BootstrapContinueArgs { mode: "rollback".into(),
            generation: generation.into(), dns_upstream: None, host_public_address: Vec::new(), enable_now: false }).unwrap()
    }

    fn journal_now() -> Option<Journal> { Store::open().unwrap().read().unwrap() }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_fresh_end_to_end() {
        let generation = disposable_generation();
        continue_with(options("install", &generation), false).unwrap();
        let journal = journal_now().unwrap();
        assert!(journal.committed && journal.step == Step::Commit);
        assert_eq!(journal.host, host_digest().unwrap());
        // The creation ledger ends with the commit.
        assert!(ledger::read(&Store::open().unwrap()).unwrap().is_none());
        assert!(duduclaw_core::isolation_install::installed_under(Path::new("/")));
        readiness::check(&mut readiness::InstalledComposite).unwrap();
        assert!(readiness::read_launcher_receipt().unwrap().is_some());
        assert_eq!(fs::read_to_string(UNIT_PATH).unwrap(), duduclaw_core::root_key::recovery::isolated_unit_text());
        readiness::unit_loaded_active(NIGHTLY_TIMER).unwrap();
        // A committed install refuses a second run and any rollback.
        assert!(continue_with(options("install", &generation), false).is_err());
        assert!(continue_with(rollback_options(&generation), false).is_err());
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_interrupt_each_step_and_resume() {
        let generation = disposable_generation();
        for step in [Step::StopGateway, Step::ProcessesClear, Step::PublishPlatform, Step::DnsPolicy,
            Step::StartServices, Step::Readiness, Step::StartGateway] {
            STOP_AFTER.with(|value| value.set(Some(step)));
            assert!(continue_with(options("install", &generation), false).is_err(), "interrupted after {step:?}");
            STOP_AFTER.with(|value| value.set(None));
            let journal = journal_now().unwrap();
            assert_eq!((journal.step, journal.phase, journal.committed), (step, Phase::Completed, false));
        }
        continue_with(options("install", &generation), false).unwrap();
        assert!(journal_now().unwrap().committed);
        readiness::check(&mut readiness::InstalledComposite).unwrap();
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_rollback_before_first_fact_is_clean() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::ProcessesClear)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        assert!(!duduclaw_core::isolation_install::installed_under(Path::new("/")));
        continue_with(rollback_options(&generation), false).unwrap();
        assert!(journal_now().is_none());
        assert!(!duduclaw_core::isolation_install::installed_under(Path::new("/")));
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_rollback_after_s6_removes_every_fact() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::StartServices)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        assert!(duduclaw_core::isolation_install::installed_under(Path::new("/")));
        continue_with(rollback_options(&generation), false).unwrap();
        assert!(journal_now().is_none());
        for fact in duduclaw_core::isolation_install::FACTS {
            assert!(!Path::new(fact).exists() && fs::symlink_metadata(fact).is_err(), "{fact}");
        }
        for path in [UNIT_PATH, RECOVERY_BOUND, BINARY, LAUNCHER, VERSION_PENDING, NIGHTLY_HELPER,
            duduclaw_egress::daemon::CONFIG_PATH, duduclaw_egress::daemon::BROKER_CONFIG_PATH] {
            assert!(fs::symlink_metadata(path).is_err(), "{path}");
        }
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM"]
    fn iso_root_install_refuses_migrate_from() {
        assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
        let marker_before = fs::symlink_metadata(duduclaw_core::protected_mode::MODE_MARKER).is_ok();
        let error = install(InstallOptions { enable_now: false, i_verified_this_binary: true,
            migrate_from: Some(std::path::PathBuf::from("/root/.duduclaw")), force_migrate: false,
            allow_missing_runtime: true }).unwrap_err().to_string();
        assert!(error.contains("--migrate-from"), "{error}");
        // Refused before any side effect.
        assert_eq!(fs::symlink_metadata(duduclaw_core::protected_mode::MODE_MARKER).is_ok(), marker_before);
        assert!(fs::symlink_metadata(VERSION_PENDING).is_err());
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_refuses_different_generation_resume() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::StopGateway)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        let before = journal_now().unwrap();
        let other = if generation.starts_with('0') { "1".repeat(64) } else { "0".repeat(64) };
        let error = continue_with(options("install", &other), false).unwrap_err().to_string();
        assert!(error.contains("rollback"), "{error}");
        assert!(continue_with(rollback_options(&other), false).is_err());
        assert_eq!(journal_now().unwrap(), before, "a refused resume changes nothing");
        continue_with(rollback_options(&generation), false).unwrap();
    }

    /// Writes a schema-valid journal directly (bypassing the transition check), as an attacker or a stale copy would.
    fn write_raw(journal: &Journal) {
        Store::open().unwrap().directory().atomic_json(JOURNAL_FILE, journal).unwrap();
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_forged_journal_is_reestablished_not_committed() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::ProcessesClear)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        // "S8 completed" on a host where nothing after S3 exists.
        write_raw(&journal_now().unwrap().at(Step::StartGateway, Phase::Completed));
        let result = continue_with(options("install", &generation), false);
        let after = journal_now().unwrap();
        match result {
            // Re-established from S4 on and committed only after readiness passed in this run.
            Ok(()) => {
                assert!(after.committed);
                readiness::check(&mut readiness::InstalledComposite).unwrap();
            }
            // A re-run step refused (e.g. S6 without an installation receipt): never committed.
            Err(_) => assert!(!after.committed),
        }
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_refuses_journal_from_another_host() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::StopGateway)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        let original = journal_now().unwrap();
        let mut foreign = original.clone();
        foreign.host = "00".repeat(32);
        write_raw(&foreign);
        let error = continue_with(options("install", &generation), false).unwrap_err().to_string();
        assert!(error.contains("另一台主機"), "{error}");
        assert!(continue_with(rollback_options(&generation), false).is_err(), "rollback is bound to the host too");
        assert_eq!(journal_now().unwrap(), foreign, "a refused run changes nothing");
        write_raw(&original);
        continue_with(rollback_options(&generation), false).unwrap();
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_rollback_keeps_replaced_object_and_ends_incomplete() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::PublishPlatform)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        // An administrator replaces a ledgered file after the interruption.
        let replacement = format!("{NIGHTLY_HELPER}.admin");
        fs::write(&replacement, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::rename(&replacement, NIGHTLY_HELPER).unwrap();
        assert!(continue_with(rollback_options(&generation), false).is_err(), "incomplete, resumable");
        assert_eq!(fs::read(NIGHTLY_HELPER).unwrap(), b"#!/bin/sh\nexit 0\n", "kept");
        assert_eq!(journal_now().unwrap().step, Step::Rollback);
        // Once the administrator resolved it, the same rollback finishes.
        fs::remove_file(NIGHTLY_HELPER).unwrap();
        continue_with(rollback_options(&generation), false).unwrap();
        assert!(journal_now().is_none());
        assert!(!duduclaw_core::isolation_install::installed_under(Path::new("/")));
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with a staged test-key bundle (ISO_BOOTSTRAP_GENERATION)"]
    fn iso_root_install_rollback_refuses_bind_mount_inside_tree() {
        let generation = disposable_generation();
        STOP_AFTER.with(|value| value.set(Some(Step::PublishPlatform)));
        assert!(continue_with(options("install", &generation), false).is_err());
        STOP_AFTER.with(|value| value.set(None));
        // A same-filesystem bind mount (same st_dev, another mount id) inside a ledgered tree.
        let (target, point) = ("/var/lib/duduclaw-iso-bind-target", "/var/lib/duduclaw-spawnd/bind");
        fs::create_dir(target).unwrap();
        fs::write(format!("{target}/keep"), b"not the installer's").unwrap();
        fs::create_dir(point).unwrap();
        assert!(command("mount", &["--bind", target, point]).unwrap().status.success());
        let refused = continue_with(rollback_options(&generation), false);
        assert!(command("umount", &[point]).unwrap().status.success());
        assert!(refused.is_err());
        assert!(Path::new(&format!("{target}/keep")).is_file(), "the bind target's contents are untouched");
        assert_eq!(journal_now().unwrap().step, Step::Rollback);
        continue_with(rollback_options(&generation), false).unwrap();
        fs::remove_dir_all(target).unwrap();
    }
}
