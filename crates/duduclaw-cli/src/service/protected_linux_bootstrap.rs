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
//! resumes with the same command at the first step that is not completed; every
//! step is idempotent and reads back what an earlier attempt left.
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
    pub(super) fn new(generation: &str, dns: Dns, now: u64) -> Self {
        Self { schema_version: 1, mode: JournalMode::Install, generation: generation.to_owned(),
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
        if self.schema_version != 1 || parse_generation(&self.generation).is_none() || self.started_at_unix == 0 {
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
        if journal.mode != JournalMode::Install || journal.generation != options.generation_hex {
            return Err(refused(OTHER_GENERATION));
        }
    }
    // S0 Verified: every run re-verifies the stage (R1) and that this program is its gateway.
    let stage = verified_stage(&options, verify_self)?;
    let mut journal = match journal {
        Some(journal) => {
            reconcile(&journal.dns, options.dns_upstream, &options.host_public_addresses)?;
            journal
        }
        None => {
            // S1 HostPreflight: no side effect; the journal records its decisions.
            let dns = preflight(&options, &stage)?;
            let journal = Journal::new(&options.generation_hex, dns, now()?);
            store.write(None, &journal)?;
            println!("[完成] S1 {}", Step::HostPreflight.label());
            journal
        }
    };
    let mut context = Context { generation: options.generation, generation_hex: options.generation_hex.clone(),
        stage, enable_now: options.enable_now };
    if let Err(error) = run_steps(&store, &mut journal, &mut context, Step::Commit) {
        let step = journal.step;
        eprintln!("安裝停在 S{} {}。修正原因後以同一個 duduclaw-bootstrap install 命令續接；或執行 duduclaw-bootstrap rollback 回到安裝前的狀態。",
            step.number(), step.label());
        return Err(error);
    }
    // The stage is not a fact; after the commit it is no longer needed.
    if super::rollback::remove_stage(&context.generation_hex).is_err() {
        eprintln!("警告：無法清除暫存的安裝包；安裝已完成，不影響運作。");
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

/// Run every step after the journal's position up to and including
/// `through` (plan §5: the seam R5 extends to drive S2..=S6 for an upgrade
/// journal; the upgrade-specific S2 is not implemented in this package).
fn run_steps(store: &Store, journal: &mut Journal, context: &mut Context, through: Step) -> Result<()> {
    loop {
        let step = match journal.phase {
            Phase::Entered => journal.step,
            Phase::Completed => match journal.step.next() {
                Some(next) if next <= through => {
                    let entered = journal.at(next, Phase::Entered);
                    store.write(Some(&*journal), &entered)?;
                    *journal = entered;
                    next
                }
                _ => return Ok(()),
            },
        };
        perform(step, context, &journal.dns)?;
        let completed = journal.at(step, Phase::Completed);
        store.write(Some(&*journal), &completed)?;
        *journal = completed;
        println!("[完成] S{} {}", step.number(), step.label());
        checkpoint(step)?;
    }
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
    let file = match directory.read(INBOX_LICENSE) {
        Ok(file) => file,
        Err(error) if update_missing(&error) => return Ok(None),
        Err(_) => return Err(refused(LICENSE_MISSING)),
    };
    if file.metadata()?.len() > readiness::MAX_LICENSE_BYTES { return Err(refused(LICENSE_NOT_TEAM)); }
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
fn preflight(options: &ContinueOptions, stage: &StageReceipt) -> Result<Dns> {
    let mut failures: Vec<String> = Vec::new();
    let mut fail = |message: &str| failures.push(message.to_owned());

    // Q9: TEAM edition first.
    match license_bytes() {
        Ok(Some(bytes)) => if !readiness::license_bytes_admit_isolation(&bytes).unwrap_or(false) { fail(LICENSE_NOT_TEAM) },
        Ok(None) => fail(LICENSE_MISSING),
        Err(_) => fail(LICENSE_MISSING),
    }
    // Isolation facts (R1 checked them before staging; checked again here).
    if duduclaw_core::isolation_install::installed_under(Path::new("/")) {
        fail("這台主機已有員工隔離的安裝紀錄；請先執行 duduclaw-bootstrap rollback 或聯絡原廠。");
    }
    // A single-binary protected host is R5's upgrade path, never a fresh install.
    let mut residue = false;
    for path in single_binary_residue() { residue |= exists(path).unwrap_or(true); }
    if residue {
        fail("這台主機已有單一執行檔的受保護安裝（或其遺留檔案）；請改用升級流程 duduclaw-bootstrap upgrade，不要刪除這些檔案。");
    }
    for path in [duduclaw_core::isolation_install::DURABLE_IMPORT_MANIFEST, duduclaw_egress::daemon::CONFIG_PATH,
        duduclaw_egress::daemon::BROKER_CONFIG_PATH, NIGHTLY_HELPER] {
        if exists(path).unwrap_or(true) { fail("這台主機有先前安裝留下的設定檔（/etc/duduclaw 或自動更新程式）；請先確認並移除後再安裝。"); break; }
    }
    if !gateway_home_clear().unwrap_or(false) {
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
    if !matches!(statfs_type(c"/var/lib"), Some(0xEF53 | 0x5846_5342)) {
        fail("員工儲存空間所在的 /var/lib 必須是 ext4 或 XFS。");
    }
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
    match read_root_text("/etc/nsswitch.conf", 256 * 1024) {
        Ok(Some(text)) if external_nss(&text) =>
            fail("偵測到外部帳號服務（LDAP／SSSD 等）。本版安裝程式不支援這類主機：外部帳號服務必須先永久保留員工 uid 範圍 70000–99999，並由原廠協助確認。"),
        Ok(_) => (),
        Err(_) => fail("無法讀取 /etc/nsswitch.conf。"),
    }
    match read_root_text("/etc/ssh/sshd_config", 1024 * 1024) {
        Ok(Some(text)) if !sshd_includes_dropins(&text) =>
            fail("sshd 主設定必須包含 Include /etc/ssh/sshd_config.d/*.conf，員工帳號的 SSH 封鎖才會生效。"),
        Ok(_) => (),
        Err(_) => fail("無法讀取 /etc/ssh/sshd_config。"),
    }
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
    let resolved = fs::symlink_metadata(duduclaw_egress::resolver::RESOLVED_SOCKET)
        .is_ok_and(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()))
        && command("systemctl", &["is-active", "--quiet", "systemd-resolved.service"])
            .is_ok_and(|output| output.status.success());
    let decision = dns_route(resolved, options.dns_upstream,
        |upstream| duduclaw_egress::daemon::BrokerConfig { upstream }.validate().is_ok());
    let dns = match decision {
        Ok((route, upstream)) => Some(Dns { route, upstream, host_public_addresses: options.host_public_addresses.clone() }),
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

// ── S4 PublishPlatform ────────────────────────────────────────────────────

fn publish_platform(context: &mut Context) -> Result<()> {
    // (a) Trust root, state, slot A, generation and `current` (R1). Called
    // WITHOUT the version lock: publish takes stage lock → version-lock →
    // update.lock itself (see LOCK ORDER above). Re-entrant.
    let digest = duduclaw_updated::linux::trust_init::publish(&context.stage)
        .map_err(|error| refused(format!("無法發布更新信任根與世代（{:?}）。", error.code())))?;
    if digest != context.generation { return Err(refused(STAGE_UNVERIFIED)); }
    let manifest = readiness::authenticated_manifest(digest)?;
    let version = duduclaw_updated::version_text(manifest.version);
    if manifest.artifacts != context.stage.artifacts || version != duduclaw_gateway::updater::current_version() {
        return Err(refused(STAGE_UNVERIFIED));
    }
    with_version_lock(|| {
        write_mode_marker()?;
        // The floor intent before any program is published (same rule as install).
        prepare_version(&version)?;
        // (b) Fixed-path programs, the frozen launcher and its receipt.
        publish_programs(&manifest, digest)?;
        // (c) Accounts, the gateway home and license, directories, unit files.
        let defs = String::from_utf8(read_root_file("/etc/login.defs", 1024 * 1024)?
            .ok_or_else(|| refused("無法讀取 /etc/login.defs。"))?).map_err(|_| refused("無法讀取 /etc/login.defs。"))?;
        layout::system_id_limit(&defs, "SYS_UID_MAX").map_err(refused)?;
        layout::system_id_limit(&defs, "SYS_GID_MAX").map_err(refused)?;
        let gateway = gateway_account(&defs)?;
        gateway_home(gateway)?;
        install_license(gateway)?;
        if !layout::published_components(Path::new("/"), 0, 0)? { return Err(refused(STAGE_UNVERIFIED)); }
        parse_runtime_tree(true)?;
        install_isolation_support_for(gateway, &defs)?;
        spawnd_anchors(digest)?;
        // (d) The nightly update requester: files only; enabled at S6 (Q11).
        nightly_files()
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

fn publish_programs(manifest: &ReleaseManifest, digest: Sha256Digest) -> Result<()> {
    use duduclaw_updated::linux::{CONFIG_ROOT, INSTALL_ROOT};
    let generation = Directory::open(Path::new(INSTALL_ROOT)).and_then(|install| install.child("generations"))
        .and_then(|generations| generations.child(&hex::encode(digest.0))).map_err(|_| refused(STAGE_UNVERIFIED))?;
    // Same parent set the single-binary install creates, plus the isolation
    // program directories (`layout::DIRECTORIES` re-applies their modes later).
    for directory in ["/opt/duduclaw", "/opt/duduclaw/bin", "/usr/local/libexec", "/usr/local/libexec/duduclaw",
        "/usr/lib/duduclaw", "/usr/lib/duduclaw/current", "/usr/libexec/duduclaw"] {
        mkdir(directory, 0o755)?;
    }
    for path in layout::BINARIES.iter().copied().chain(std::iter::once(BINARY)) {
        let target = if path == readiness::UPDATER_LAUNCHER { InstallTarget::UpdateSlot }
            else { readiness::component_target(path).ok_or_else(|| refused(STAGE_UNVERIFIED))? };
        let artifact = single_artifact(&manifest.artifacts, &target).ok_or_else(|| refused(STAGE_UNVERIFIED))?;
        // Every fixed-path program is root 0755 (layout); the generation's mode
        // must agree except for the slot artifact, whose launcher copy has its own receipt.
        if target != InstallTarget::UpdateSlot && artifact.mode != 0o755 { return Err(refused(STAGE_UNVERIFIED)); }
        publish_program(&generation, artifact, path)?;
    }
    // The frozen launcher's receipt, for R4 item #3 (never overwritten).
    let slot = single_artifact(&manifest.artifacts, &InstallTarget::UpdateSlot).ok_or_else(|| refused(STAGE_UNVERIFIED))?;
    let record = readiness::LauncherReceiptRecord { schema_version: 1, generation: digest, sha256: slot.sha256,
        size_bytes: slot.size_bytes, mode: 0o755, installed_at_unix: now()? };
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
    update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT)?;
    Ok(())
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

/// A root-owned or gateway-owned regular file in `directory`, read once without following links.
fn read_data_file(directory: &fs::File, name: &CStr) -> Result<Vec<u8>> {
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW
        | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
    // SAFETY: a fresh descriptor owned here.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > readiness::MAX_LICENSE_BYTES {
        return Err(refused(LICENSE_NOT_TEAM));
    }
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

fn nightly_files() -> Result<()> {
    match fs::symlink_metadata("/usr/local/sbin") {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => mkdir("/usr/local/sbin", 0o755)?,
        Err(error) => return Err(error.into()),
        Ok(_) => { parents(Path::new("/usr/local/sbin"))?; trusted(Path::new("/usr/local/sbin"), true)?; }
    }
    update_text_if_different(NIGHTLY_HELPER, 0o755, NIGHTLY_HELPER_TEXT)?;
    for (name, text) in NIGHTLY_UNITS {
        update_text_if_different(&format!("/etc/systemd/system/{name}"), 0o644, text)?;
    }
    Ok(())
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

fn dns_policy(context: &Context, dns: &Dns) -> Result<()> {
    use duduclaw_egress::daemon::{read_root_config, BrokerConfig, Config, BROKER_CONFIG_PATH, CONFIG_PATH};
    let policy = signed_policy_profiles(context)?;
    let document = egress_document(&policy, dns.route, &dns.host_public_addresses)?;
    let bytes = serde_json::to_vec(&document).map_err(|_| refused(POLICY_REFUSED))?;
    match (dns.route, dns.upstream) {
        (DnsRoute::DedicatedDnsBroker, Some(upstream)) => {
            let broker = serde_json::to_vec(&serde_json::json!({ "upstream": upstream })).map_err(|_| refused(POLICY_REFUSED))?;
            write_root_config(BROKER_CONFIG_PATH, &broker)?;
            let written: BrokerConfig = read_root_config(BROKER_CONFIG_PATH).map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
            written.validate().map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
            if written.upstream != upstream { return Err(refused("DNS 轉送服務設定讀回不符。")); }
        }
        (DnsRoute::ResolvedVarlink, None) => if exists(BROKER_CONFIG_PATH)? {
            return Err(refused("使用 systemd-resolved 的主機不應有 DNS 轉送服務設定；請檢查 /etc/duduclaw/dns-broker.json。"));
        },
        _ => return Err(refused(JOURNAL_DAMAGED)),
    }
    write_root_config(CONFIG_PATH, &bytes)?;
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

fn start_services(context: &Context, dns: &Dns) -> Result<()> {
    let output = command("systemctl", &["daemon-reload"])?;
    if !output.status.success() { return Err(refused(SYSTEMCTL_FAILED)); }
    let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    for unit in SERVICE_ORDER {
        if unit == "duduclaw-dns-broker.socket" && dns.route != DnsRoute::DedicatedDnsBroker { continue; }
        enable_now(unit)?;
        if unit == "duduclaw-spawnd.service" && !wait_for(READY_WAIT, || {
            duduclaw_spawnd::host::EvidenceReport::read(gateway.gid).is_ok_and(|report| report.check().is_ok())
        }) {
            return Err(refused("隔離執行服務未在 30 秒內發布有效的主機證據。"));
        }
        if unit == "duduclaw-updated.service" && !wait_for(READY_WAIT, || {
            let nonce = duduclaw_isolation::ids::RequestId(*uuid::Uuid::new_v4().as_bytes());
            duduclaw_updated::client::probe(nonce).is_ok_and(|evidence| evidence.generation == context.generation)
        }) {
            return Err(refused("更新服務未在 30 秒內回報與安裝相同的世代。"));
        }
    }
    // Q11: the nightly update requester is enabled after install.
    enable_now(NIGHTLY_TIMER)
}

// ── S8 StartGateway / S9 Commit ───────────────────────────────────────────

fn start_gateway(context: &Context) -> Result<()> {
    let text = duduclaw_core::root_key::recovery::isolated_unit_text();
    invalidate_recovery_bound()?;
    update_text_if_different(UNIT_PATH, 0o644, &text)?;
    update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT)?;
    reload_and_certify_recovery_text(&text)?;
    let output = command("systemctl", &["enable", UNIT])?;
    if !output.status.success() { return Err(refused(format!("無法啟用 {UNIT}。"))); }
    if context.enable_now {
        let output = command("systemctl", &["start", UNIT])?;
        if !output.status.success() { return Err(refused(format!("無法啟動 {UNIT}。"))); }
        if !wait_for(Duration::from_secs(60), || readiness::unit_loaded_active(UNIT).is_ok()) {
            return Err(refused("Gateway 未在時限內進入執行狀態。"));
        }
        // The install reports success only when the full check passes.
        readiness::check(&mut readiness::InstalledComposite)?;
    }
    Ok(())
}

fn commit(context: &Context) -> Result<()> {
    let manifest = readiness::authenticated_manifest(context.generation)?;
    record_floor(&duduclaw_updated::version_text(manifest.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Sha256Digest { Sha256Digest([byte; 32]) }
    fn broker_dns() -> Dns {
        Dns { route: DnsRoute::DedicatedDnsBroker, upstream: Some("192.0.2.53:53".parse().unwrap()),
            host_public_addresses: vec!["203.0.113.7".parse().unwrap()] }
    }
    fn journal() -> Journal { Journal::new(&"ab".repeat(32), broker_dns(), 100) }

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

    // ── Root tests (disposable VM; plan §6) ───────────────────────────────
    //
    // Prerequisites: DUDUCLAW_ISO_DISPOSABLE=1; a test-key bundle staged by
    // `duduclaw-bootstrap install` (whose verified gateway then refused or was
    // interrupted before S2 is fine) and its generation in
    // ISO_BOOTSTRAP_GENERATION; a TEAM test license in /root/duduclaw-migrate;
    // a supported distribution with systemd-resolved (or ISO_DNS_UPSTREAM).
    // The test binary is not the staged gateway, so `verify_self` is off.

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
}
