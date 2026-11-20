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
//! The journal is bound to one generation, one host (SHA-256 of
//! `/etc/machine-id` and the DMI product UUID or its absence, review M4) and
//! one transaction nonce shared with the creation ledger (review H3); another
//! generation's, host's or transaction's journal or ledger is refused.
//! Its cursor is only a HINT (review F1): every invocation re-runs S0 and the
//! complete, side-effect-free S1 preflight (review F6), then re-establishes
//! every step the journal claims complete against the real host (`Steps::holds`:
//! published files with their digest/owner/mode, accounts, unit files and
//! their enabled/active state, policy files, the launcher receipt, trust state
//! bound to this generation) and re-runs from the first step whose invariants
//! do not hold. S9 independently requires the readiness check in the same run.
//! Every object a step creates is first recorded as an intent and then with
//! the identity its creation proves in the creation ledger (review F4, R3
//! item 4, `protected_linux_ledger.rs`); rollback removes nothing else.
//! Before any S4–S7 step re-runs on resume, every unit this installation owns
//! is stopped and the full S3 process check passes (review R3 item 1); since
//! review round 4 item 1 that is everything PID 1 has loaded of it (template
//! instances, transient runs and employee slices, queued jobs, product
//! cgroups), through the one routine rollback uses too (`quiesce_owned`).
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
const SPAWND_ANCHORS: [&str; 5] = ["etc-state", "projections", "snapshots", "images", "storage"];
const SPAWND_PUBLICATION: &str = "publication.json";
/// The signed isolation policy artifact (`duduclaw-spawnd` installed.rs).
const POLICY_REGISTRY_ID: &str = "isolation-policy-v1";
/// Egress profile values the signed policy does not carry (see `egress_document`).
pub(super) const EGRESS_PORTS: [u16; 1] = [443];
pub(super) const EGRESS_MAX_RUNS: u16 = 32;
pub(super) const EGRESS_RUN_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub(super) const EGRESS_RUN_TOTAL_CONNECTIONS: u32 = 4096;
pub(super) const MAX_HOST_PUBLIC_ADDRESSES: usize = 64;
/// The units S6 starts, in start order, which is the boot order (plan S6,
/// [`service_starts`]): the sockets and spawnd, egress, the gateway (started,
/// enabled only at S8) and the updater, whose recovery reads the live gateway,
/// egress and spawnd units back before it answers. The broker socket only on
/// the broker route; the consistency gate right after spawnd.
const SERVICE_ORDER: [&str; 8] = ["duduclaw-egress.socket", "duduclaw-dns-broker.socket", "duduclaw-parse.socket",
    "duduclaw-spawnd.socket", SPAWND_SERVICE, "duduclaw-egress.service", UNIT, UPDATER_UNIT];
const SPAWND_SERVICE: &str = "duduclaw-spawnd.service";
const UPDATER_UNIT: &str = "duduclaw-updated.service";
/// The units the isolated gateway unit binds to
/// (`duduclaw_isolation::ids::ISOLATED_GATEWAY_UNIT_DEPENDENCIES`): R6's
/// consistency gate, a `layout::UNITS` oneshot (`RemainAfterExit=yes`, so
/// "active (exited)" is its healthy state) bound to spawnd. It has no
/// `[Install]` and is NEVER enabled: spawnd pulls it in (`Wants=`). It means
/// "spawnd is open", not "every readiness check passed". At S6 it is
/// started right after spawnd and required active before egress (which binds
/// to it) starts; on resume it must be active (and `static`); the quiesce and
/// rollback stop it right after the gateway, before spawnd. Never invent a unit here.
pub(super) const GATEWAY_READY_UNITS: &[&str] = &[duduclaw_isolation::ids::ISOLATION_READY_UNIT];
/// spawnd's runtime directory (systemd creates it at S6; a ledgered tree).
pub(super) const SPAWND_RUNTIME: &str = "/run/duduclaw-spawnd";
/// The trees R1's `trust_init::publish` creates at S4(a).
const TRUST_TREES: [&str; 3] = ["/opt/duduclaw-isolated", "/etc/duduclaw-updated", "/var/lib/duduclaw-updated"];
/// The trees of `layout::DIRECTORIES` created at S4(c).
const LAYOUT_TREES: [&str; 2] = ["/var/lib/duduclaw-spawnd", "/var/lib/duduclaw-migrate"];
const MACHINE_ID: &str = "/etc/machine-id";
/// `/sys/class/dmi/id/product_uuid` (`/sys/class/dmi/id` is a link to this
/// canonical sysfs directory; links are never followed). Absent without DMI.
const PRODUCT_UUID: &str = "/sys/devices/virtual/dmi/id/product_uuid";
const SYSFS_MAGIC: i64 = 0x6265_6572;
/// Journal schema (review M4: 2 binds the product UUID and the transaction nonce).
pub(super) const JOURNAL_SCHEMA: u16 = 2;
/// Review H1: bound of spawnd's root host publication as this installer reads it.
const MAX_PUBLICATION_BYTES: u64 = 64 * 1024;
const READY_WAIT: Duration = Duration::from_secs(30);
/// S6: the bound of the gateway's first start (S8's bound before C1).
const GATEWAY_START_WAIT: Duration = Duration::from_secs(60);
/// S6 (C1): the bound of the updater's first answer after its start. Before
/// it answers, its recovery drains spawnd (up to 30 s) and reads the live
/// gateway, egress and spawnd units back.
const UPDATER_ANSWER_WAIT: Duration = Duration::from_secs(120);
/// S6 (C2/C4): the bound of spawnd's full host evidence once the updater
/// answers. Budget: start-up reconciliation retries every 2 s until the
/// updater answers; the R7a self-test (it needs the updater's generation)
/// takes up to 40 s; the DNS probe retries every 5 s and egress's lookup
/// window is 10 s; the rest is margin for the per-account SSH probes.
const SPAWND_EVIDENCE_WAIT: Duration = Duration::from_secs(150);
/// Review R3 item 6 / round 4 item 4: ONE absolute deadline for the whole
/// quiesce (enumerate, stop, wait, process scan, re-check); every command and
/// every lookup of the process scan gets `min(QUIESCE_COMMAND_CAP, what is
/// left)` and nothing is spawned or read once it has passed.
const QUIESCE_WAIT: Duration = Duration::from_secs(30);
const QUIESCE_COMMAND_CAP: Duration = Duration::from_secs(30);
const QUIESCE_POLL: Duration = Duration::from_millis(250);
/// Review round 4 item 1: the names PID 1 is asked to enumerate (fixed argv,
/// no shell; systemctl matches the patterns itself).
const UNIT_PATTERNS: [&str; 2] = ["duduclaw-*", "duduclaw.slice"];
/// Bound of one enumeration (units or jobs); more lines refuse.
const MAX_LISTED: usize = 4096;
/// Review round 5 item 3: stops of one unit within one quiesce; a unit busy again after that many refuses.
const MAX_STOPS: u32 = 3;
/// Review round 4 item 1: the template units whose instances may be loaded —
/// (name prefix, suffix, template unit file).
/// R7c: spawnd's one-shot SSH denial probe instances
/// (`duduclaw-isolation-probe@<account>.service`, in `duduclaw-tcb.slice`).
const TEMPLATES: [(&str, &str, &str); 5] = [
    ("duduclaw-parse@", ".service", "duduclaw-parse@.service"),
    ("duduclaw-import@", ".service", "duduclaw-import@.service"),
    ("duduclaw-migrate-expire@", ".service", "duduclaw-migrate-expire@.service"),
    ("duduclaw-migrate-expire@", ".timer", "duduclaw-migrate-expire@.timer"),
    (duduclaw_isolation::ids::ISOLATION_PROBE_UNIT_PREFIX, ".service", duduclaw_isolation::ids::ISOLATION_PROBE_UNIT_TEMPLATE),
];
/// Review round 4 item 1: the product's cgroups below `/sys/fs/cgroup` that
/// must read `populated 0` (or be absent): the aggregate slice (the TCB, the
/// employees' slice, every per-employee slice and run below it) and the
/// implicit slices systemd creates for the template instances.
pub(super) const PRODUCT_CGROUPS: [&str; 4] = ["duduclaw.slice", "system.slice/system-duduclaw\\x2dparse.slice",
    "system.slice/system-duduclaw\\x2dimport.slice", "system.slice/system-duduclaw\\x2dmigrate\\x2dexpire.slice"];
const CGROUP2_MAGIC: i64 = 0x6367_7270;
/// Review R3 item 4: the unit whose socket systemd creates [`SPAWND_RUNTIME`] for
/// (`ListenSequentialPacket=/run/duduclaw-spawnd/control.sock`).
const SPAWND_RUNTIME_OWNER: &str = "duduclaw-spawnd.socket";

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
const HOST_UNKNOWN: &str = "無法讀取這台主機的識別碼：/etc/machine-id 必須是 root 擁有的一般檔案，內容為 32 位小寫十六進位且不能全為 0 或同一個字元；/sys/class/dmi/id/product_uuid 存在時必須是標準 UUID 格式。";
/// The prefix of every quiesce refusal: the same routine serves the install
/// resume (before S4–S7 re-run) and the rollback (review round 4, item 1).
const QUIESCE_CONTEXT: &str = "停止這次安裝的所有服務時（續接重新執行平台或政策步驟前，或回退前）";
const GATEWAY_PROCESSES: &str = "停止這次安裝的所有服務時（續接重新執行平台或政策步驟前，或回退前），duduclaw-gateway.service 停止後，duduclaw（Gateway）帳號仍有程序；已停止，未重新執行任何步驟，也未移除任何東西。請先結束這些程序後再執行同一個命令。";
const ISOLATION_PROCESSES: &str = "停止這次安裝的所有服務時（續接重新執行平台或政策步驟前，或回退前），這次安裝的服務都已停止，但在時限內仍有服務帳號（duduclaw、duduclaw-*）或員工帳號（uid 70000–99999）的程序，或有從隔離安裝路徑（/opt/duduclaw-isolated、/opt/duduclaw、/usr/lib/duduclaw、/usr/libexec/duduclaw 等）執行的程序；已停止，未重新執行任何步驟，也未移除任何東西。請先結束這些程序後再執行同一個命令。";
const UNITS_UNREADABLE: &str = "systemd 的單元或工作清單無法判讀（輸出超過上限、格式不明，或含有不屬於查詢範圍的名稱）；已停止，未停止任何不明單元。";
const CGROUP_UNREADABLE: &str = "無法讀取隔離服務的 cgroup 狀態（/sys/fs/cgroup 必須是 cgroup v2，cgroup.events 必須可判讀）；已停止，未重新執行任何步驟，也未移除任何東西。";

/// Review H2 / R3 item 1 / round 4 item 1: a unit up (or with a job) that is not provably this install's.
fn unit_not_ours(unit: &str) -> DuDuClawError {
    refused(format!("{QUIESCE_CONTEXT}，{unit} 正在執行（或有待處理的工作），但無法確認它屬於這次安裝：一般單元必須從記錄的原單元檔載入、沒有覆寫設定、不需要 daemon-reload、不是暫時單元；範本的執行個體必須從記錄的原範本檔以同樣條件載入；員工執行單元與員工 slice 必須是暫時單元，且位於 duduclaw-employees.slice 之下。已停止，沒有停止它。請先手動停止它（sudo systemctl stop {unit}）並確認來源後再執行同一個命令。"))
}

/// Review R3 items 1/6: a unit not confirmed down with no pending job within the deadline.
fn unit_not_quiet(unit: &str) -> DuDuClawError {
    refused(format!("{QUIESCE_CONTEXT}，無法在時限內確認 {unit} 已停止且沒有待處理的工作；已停止，未重新執行任何步驟，也未移除任何東西。請先手動停止它（sudo systemctl stop {unit}）並確認後再執行同一個命令。"))
}

/// Review round 4 item 1 / round 5 item 1: a `duduclaw-` unit of a shape this
/// installation never creates that is up or has a queued job (never stopped here).
fn unit_unknown(unit: &str) -> DuDuClawError {
    refused(format!("{QUIESCE_CONTEXT}，{unit} 正在執行（或有待處理的工作）。它的名稱以 duduclaw- 開頭，但不是這次安裝的單元，安裝程式不會停止它；已停止，沒有停止它或任何其他不屬於這次安裝的單元，未重新執行任何步驟，也未移除任何東西。請先停止它（sudo systemctl stop {unit}）後再執行同一個命令。"))
}

/// Review round 5 item 1: `duduclaw-` units of another shape that are down with no job (ignored, never stopped).
fn units_ignored(units: &[String]) -> String {
    format!("[資訊] 以下單元的名稱以 duduclaw- 開頭，但不是這次安裝的單元；它們沒有在執行、也沒有待處理的工作，不影響這次停止，也不會被停止：{}", units.join("、"))
}

/// Review round 5 item 3: a unit of this installation that keeps coming back after [`MAX_STOPS`] stops.
fn unit_restarting(unit: &str) -> DuDuClawError {
    refused(format!("{QUIESCE_CONTEXT}，{unit} 已停止 {MAX_STOPS} 次，但每次都又回到執行狀態或又有待處理的工作（可能有其他單元、socket 啟用或管理者一直重新啟動它）；已停止，不再停止它，未重新執行任何步驟，也未移除任何東西。請找出並停止重新啟動它的來源（sudo systemctl status {unit}、sudo systemctl list-dependencies --reverse {unit}）後再執行同一個命令。"))
}

/// Review round 4 item 1: a product cgroup still holding processes at the deadline.
fn cgroup_not_quiet(cgroup: &str) -> DuDuClawError {
    refused(format!("{QUIESCE_CONTEXT}，這次安裝的服務都已停止，但 cgroup /sys/fs/cgroup/{cgroup} 在時限內仍有程序；已停止，未重新執行任何步驟，也未移除任何東西。請先結束這些程序後再執行同一個命令。"))
}

/// Review round 4 item 3: the definition PID 1 loads for a stopped unit changed across its stop.
fn unit_redefined(unit: &str) -> DuDuClawError {
    refused(format!("{QUIESCE_CONTEXT}，{unit} 的單元檔或 systemd 載入的定義在停止它的期間被更換或重新載入；被停止的可能不是這次安裝的單元。已停止，未重新執行任何步驟，也未移除任何東西。請確認 {unit} 的來源（sudo systemctl status {unit}）後再執行同一個命令。"))
}
const PUBLICATION_MISMATCH: &str = "隔離執行服務的發布紀錄 /var/lib/duduclaw-spawnd/publication.json 不是這次安裝的內容（必須是 root 擁有、權限 0600 的一般檔案，只含這個世代，且沒有授權或只有通過簽署政策檢查的授權）；已停止，未覆蓋它。";
const HOST_DNS_CHANGED: &str = "主機的 DNS 狀態（systemd-resolved 是否可用）與第一次執行時不同。請恢復原狀後續接，或先執行 duduclaw-bootstrap rollback。";
const PUBLISHED_MISMATCH: &str = "已發布的內容與這次安裝不符。";
const LICENSE_MODE: &str = "團隊版授權檔 license.json 的權限必須是 0600 或 0644。";
const IDENTITY_ALIAS: &str = "服務帳號或群組的識別碼與其他帳號或群組重複、帶有額外成員，或帳號資料無法完整列出；其他帳號會因此取得相同的檔案權限，已停止。請先移除重複的帳號或群組。";
const STORAGE_THIN: &str = "/var/lib 位於精簡配置（thin provisioning）、快照或快取的區塊裝置上；員工儲存空間需要預先配置的磁碟。";
const STORAGE_UNDETERMINED: &str = "無法確定 /var/lib 的完整底層區塊裝置鏈（或含有不支援的裝置類型），無法證明它不是精簡配置的儲存。";
/// `--enable-now` is accepted for compatibility only.
const ENABLE_NOW_INERT: &str = "[資訊] 已不需要 --enable-now：安裝程式現在一定會啟動 Gateway。";

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
            Step::DnsPolicy => "DNS 與對外連線設定", Step::StartServices => "啟動隔離服務與 Gateway",
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
    /// [`host_digest`] of the host the journal was created on (review F1, M4).
    pub(super) host: String,
    /// Review H3: this transaction's random 256-bit nonce (64 lowercase hex
    /// digits), generated at S0 of the run that created the journal; the
    /// creation ledger carries the same value and must match on every load.
    pub(super) nonce: String,
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
    pub(super) fn new(generation: &str, host: &str, nonce: &str, dns: Dns, now: u64) -> Self {
        Self { schema_version: JOURNAL_SCHEMA, mode: JournalMode::Install, generation: generation.to_owned(),
            host: host.to_owned(), nonce: nonce.to_owned(), step: Step::HostPreflight, phase: Phase::Completed, started_at_unix: now, prior: Prior::fresh(), dns,
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
        // Review M4: a journal of an older schema (host bound to the machine id alone) is refused.
        if self.schema_version != JOURNAL_SCHEMA || parse_generation(&self.generation).is_none()
            || parse_generation(&self.host).is_none() || parse_generation(&self.nonce).is_none() || self.started_at_unix == 0 {
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

/// A value made of one repeated character (all-zero included) identifies nothing.
fn degenerate(text: &str) -> bool {
    let mut digits = text.bytes().filter(|b| *b != b'-');
    digits.next().is_none_or(|first| digits.all(|b| b == first))
}

/// Review M4 (pure): DMI `product_uuid` in strict 8-4-4-4-12 hex form (one
/// optional trailing newline), lowercased. `Ok(None)`: a degenerate UUID
/// (one repeated digit, e.g. all zero) adds nothing and is recorded as absent.
pub(super) fn parse_product_uuid(text: &str) -> std::result::Result<Option<String>, ()> {
    let uuid = text.strip_suffix('\n').unwrap_or(text);
    let shape = uuid.len() == 36 && uuid.bytes().enumerate().all(|(index, b)| match index {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    });
    if !shape { return Err(()); }
    if degenerate(uuid) { return Ok(None); }
    Ok(Some(uuid.to_ascii_lowercase()))
}

/// Review F1/M4 (pure): the host binding — SHA-256 over a fixed domain, the
/// machine id (32 lowercase hex digits, one optional trailing newline, never
/// all-zero or one repeated digit) and the DMI product UUID or the explicit
/// fact that the platform has none, so a resume where it appears or vanishes
/// is another host.
///
/// Limit: a clone that preserves BOTH identifiers (a copied disk image with
/// `/etc/machine-id` kept, on a hypervisor that copies or does not provide the
/// DMI UUID) is indistinguishable from the original; that is outside what the
/// installer can detect.
pub(super) fn host_digest_of(machine_id: &str, product_uuid: Option<&str>) -> Option<String> {
    use sha2::{Digest, Sha256};
    let id = machine_id.strip_suffix('\n').unwrap_or(machine_id);
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) || degenerate(id) {
        return None;
    }
    let uuid = match product_uuid {
        Some(text) => parse_product_uuid(text).ok()?,
        None => None,
    };
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-isolation-bootstrap-host-v2\0machine-id\0");
    hash.update(id.as_bytes());
    match uuid {
        Some(uuid) => { hash.update(b"\0product-uuid\0"); hash.update(uuid.as_bytes()); }
        None => hash.update(b"\0product-uuid-absent\0"),
    }
    Some(hex::encode(hash.finalize()))
}

/// The DMI product UUID when the platform has one (read from the kernel's
/// sysfs only, root-owned, bounded, never through a link). Absent = `None`.
fn product_uuid() -> Result<Option<String>> {
    if statfs_type(c"/sys") != Some(SYSFS_MAGIC) { return Err(refused(HOST_UNKNOWN)); }
    if !exists(PRODUCT_UUID).map_err(|_| refused(HOST_UNKNOWN))? { return Ok(None); }
    read_root_text(PRODUCT_UUID, 4096).map_err(|_| refused(HOST_UNKNOWN))
}

/// This host's binding, from the root-owned `/etc/machine-id` and the DMI
/// product UUID (neither followed as a link).
pub(super) fn host_digest() -> Result<String> {
    let text = read_root_text(MACHINE_ID, 64).map_err(|_| refused(HOST_UNKNOWN))?.ok_or_else(|| refused(HOST_UNKNOWN))?;
    let uuid = product_uuid()?;
    host_digest_of(&text, uuid.as_deref()).ok_or_else(|| refused(HOST_UNKNOWN))
}

/// `bytes` bytes from the kernel's CSPRNG (`getrandom(2)`), lowercase hex.
pub(super) fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    let mut filled = 0;
    while filled < bytes {
        let rest = buffer.get_mut(filled..).ok_or_else(|| refused("無法取得隨機數。"))?;
        // SAFETY: SYS_getrandom writes at most `rest.len()` bytes into the live buffer; flags are zero.
        let count = unsafe { libc::syscall(libc::SYS_getrandom, rest.as_mut_ptr(), rest.len(), 0 as libc::c_uint) };
        if count < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) { continue; }
            return Err(refused("無法取得隨機數。"));
        }
        if count == 0 { return Err(refused("無法取得隨機數。")); }
        filled += count as usize;
    }
    Ok(hex::encode(buffer))
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
    println!("4. 執行：sudo /root/duduclaw-migrate/duduclaw-bootstrap install [--dns-upstream <IP:53>] [--host-public-address <IP>]");
    println!("   安裝程式會啟動 Gateway 並設為開機自動啟動，不需要再加 --enable-now。");
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
    /// `--enable-now`: accepted for compatibility, no effect (the install
    /// always leaves the gateway running); only [`ENABLE_NOW_INERT`] is printed.
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
/// `nonce` (review H3, generated at S0) is used only for a new journal.
pub(super) fn begin(journal: Option<Journal>, options: &ContinueOptions, host: &str, nonce: &str, now: u64,
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
            Ok((Journal::new(&options.generation_hex, host, nonce, dns, now), true))
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
        // Refused whatever `validate` says, with what to do instead.
        (false, Some(upstream)) if layout::ipv4_link_local(upstream) => Err(layout::LINK_LOCAL_UPSTREAM_REFUSED),
        (false, Some(upstream)) if validate(upstream) => Ok((DnsRoute::DedicatedDnsBroker, Some(upstream))),
        (false, Some(_)) => Err("--dns-upstream 必須是單一 IP 位址加上連接埠 53，且不能是本機迴路、未指定、群播、廣播、IPv4 或 IPv6 鏈路本地、或 IPv4 對應的 IPv6 位址。"),
    }
}

/// The upstream rule S1 applies (and re-applies before S5 and S6): the
/// broker's own validator, and an upstream the broker unit's generated
/// `IPAddressAllow=` line can carry (`layout::dns_broker_allow_line`).
pub(super) fn upstream_valid(upstream: SocketAddr) -> bool {
    duduclaw_egress::daemon::BrokerConfig { upstream }.validate().is_ok() && layout::dns_broker_allow_line(upstream).is_ok()
}

/// This install's broker upstream for the broker unit's text: `Some` on the
/// broker route, `None` on the resolved route; any other pairing is damage.
fn broker_upstream(dns: &Dns) -> Result<Option<SocketAddr>> {
    match (dns.route, dns.upstream) {
        (DnsRoute::DedicatedDnsBroker, Some(upstream)) => Ok(Some(upstream)),
        (DnsRoute::ResolvedVarlink, None) => Ok(None),
        _ => Err(refused(JOURNAL_DAMAGED)),
    }
}

// ── Entry ─────────────────────────────────────────────────────────────────

struct Context {
    generation: Sha256Digest,
    generation_hex: String,
    stage: StageReceipt,
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
    if options.enable_now { println!("{ENABLE_NOW_INERT}"); }
    if let Some(journal) = &journal {
        if journal.step == Step::Rollback {
            println!("上一次的回退尚未完成，現在繼續完成回退；完成後請重新執行 duduclaw-bootstrap install。");
            return super::rollback::run(&store, Some(journal.clone()), Some(options.generation_hex.as_str()));
        }
        if journal.committed { return Err(refused(ALREADY_COMMITTED)); }
    }
    // S0 Verified: every run re-verifies the stage (R1) and that this program is its gateway.
    let stage = verified_stage(&options, verify_self)?;
    // Review H3: a new transaction's nonce, generated at S0 (used only when no journal exists).
    let nonce = random_hex(32)?;
    // Review F1: the journal belongs to this generation and this host.
    let host = host_digest()?;
    if let Some(journal) = &journal { bound_to(journal, &options.generation_hex, &host)?; }
    // A ledger exists only beside a journal (created after it, removed before
    // it) and only of the same transaction (generation and nonce, review H3).
    let ours = ledger::read(&store)?;
    match (&journal, &ours) {
        (None, Some(_)) => return Err(refused(ledger::LEDGER_DAMAGED)),
        (Some(journal), Some(ours)) => ledger::belongs_to(ours, &journal.generation, &journal.nonce)?,
        _ => (),
    }
    // Review R3 item 5: an interrupted rollback quarantine is finished before anything else.
    let ours = match ours {
        Some(recorded) if super::rollback::reconcile_on_resume(&store, &recorded)? => ledger::read(&store)?,
        ours => ours,
    };
    // S1 HostPreflight on every invocation (review F6): no side effect; what
    // this transaction already created (per its ledger) is not "residue".
    let (mut journal, fresh) = begin(journal, &options, &host, &nonce, now()?,
        |upstream, hosts| preflight(upstream, hosts, &stage, ours.as_ref()))?;
    if fresh { store.write(None, &journal)?; }
    println!("[完成] S1 {}", Step::HostPreflight.label());
    let recorder = LiveRecorder::open_live(&options.generation_hex, &journal.nonce)?;
    let mut context = Context { generation: options.generation, generation_hex: options.generation_hex.clone(),
        stage, recorder };
    let dns = journal.dns.clone();
    // S3's check after this transaction started its isolation services (S6
    // entered) admits their own processes (`processes_quiet`).
    let services_started = journal.step >= Step::StartServices;
    let result = {
        let mut live = Live { context: &mut context, dns: &dns, services_started };
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
    /// Review H2 / R3 item 1: before any S4–S7 step re-runs, bring down the
    /// gateway and EVERY isolation unit this installation owns, prove them
    /// down, then pass the full S3 process check ([`quiesce_isolation`]);
    /// refuses when that cannot be proven.
    fn quiesce(&mut self) -> Result<()>;
}

/// Review H2 / R3 items 1, 2, 6 / round 4 items 1, 3, 4: what quiescing
/// needs from the host (test seam). Every call that spawns a command or reads
/// host state is given the one absolute quiesce `deadline`; it gets at most
/// what is left of it and does nothing once it has passed.
pub(super) trait UnitControl {
    /// Monotonic time (a fake clock in tests).
    fn now(&mut self) -> Instant;
    /// stdout of `systemctl list-units --all --no-legend --plain --no-pager
    /// duduclaw-* duduclaw.slice` (parsed by [`parse_unit_list`]).
    fn list_units(&mut self, deadline: Instant) -> Result<Vec<u8>>;
    /// stdout of `systemctl list-jobs --no-legend --plain --no-pager
    /// duduclaw-* duduclaw.slice` (parsed by [`parse_job_list`]).
    fn list_jobs(&mut self, deadline: Instant) -> Result<Vec<u8>>;
    /// What PID 1 loads for `unit` of `shape` when it is provably this
    /// installation's ([`live_definition`]): comparable text (review round 4
    /// item 3). `None`: not provably ours (any failure included).
    fn definition(&mut self, unit: &str, shape: UnitShape, deadline: Instant) -> Option<String>;
    /// `systemctl stop <unit>`.
    fn stop(&mut self, unit: &str, deadline: Instant) -> Result<()>;
    /// `systemctl kill --signal=SIGKILL <unit>` (transient units only).
    fn kill(&mut self, unit: &str, deadline: Instant) -> Result<()>;
    /// The first of [`PRODUCT_CGROUPS`] whose `cgroup.events` reports
    /// `populated 1`; `None` when every one reads `populated 0` or is absent.
    fn populated_cgroup(&mut self, deadline: Instant) -> Result<Option<&'static str>>;
    /// Any process of the gateway account remains.
    fn gateway_account_busy(&mut self, deadline: Instant) -> Result<bool>;
    /// The full S3 check of a fresh install (`removal::processes_clear_until`):
    /// no service-account or employee process, nothing executing from the
    /// isolation install paths; every lookup it makes is bounded by `deadline`.
    fn processes_clear(&mut self, deadline: Instant) -> Result<()>;
    /// Sleep for `pause` (never past the deadline).
    fn pause(&mut self, pause: Duration);
    /// One informational line for the operator (review round 5 item 1).
    fn inform(&mut self, line: &str);
}

/// Review round 4 item 1: the shape of a `duduclaw-` unit this installation
/// may own. Any other loaded name with the prefix is never stopped: refused
/// by name when it is up or has a job, otherwise ignored and reported (round 5 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnitShape {
    /// The gateway, a unit it binds to, the nightly requester, a layout unit
    /// or an aggregate slice ([`super::rollback::stop_order`]).
    Fixed,
    /// An instance of the named template unit file ([`TEMPLATES`]).
    Instance(&'static str),
    /// spawnd's transient employee run: `duduclaw-run-` + 32 lowercase hex + `.service`.
    Run,
    /// spawnd's transient per-employee slice: `duduclaw-employees-` + 32 lowercase hex + `.slice`.
    EmployeeSlice,
}

impl UnitShape {
    /// Created by spawnd through PID 1 (no unit file).
    fn transient(self) -> bool { matches!(self, UnitShape::Run | UnitShape::EmployeeSlice) }
}

/// spawnd's run and employee ids (`duduclaw_spawnd::systemd::run_unit`, `employee_slice`).
fn lower_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The instance part the product generates: systemd's `Accept=yes` AF_UNIX
/// instances of the parse socket (`<n>-<pid>-<uid>`, `<n>-unknown`) and the
/// migration's lowercase ids — 1 to 64 of `[a-z0-9-]`, not starting with `-`.
fn instance_valid(instance: &str) -> bool {
    !instance.is_empty() && instance.len() <= 64 && !instance.starts_with('-')
        && instance.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-')
}

/// Review round 4 item 1 (pure): the exact shape of `name`, or `None`.
pub(super) fn unit_shape(name: &str) -> Option<UnitShape> {
    if super::rollback::stop_order().contains(&name) { return Some(UnitShape::Fixed); }
    for (prefix, suffix, template) in TEMPLATES {
        if let Some(instance) = name.strip_prefix(prefix).and_then(|rest| rest.strip_suffix(suffix)) {
            return instance_valid(instance).then_some(UnitShape::Instance(template));
        }
    }
    if name.strip_prefix("duduclaw-run-").and_then(|rest| rest.strip_suffix(".service")).is_some_and(lower_hex_id) {
        return Some(UnitShape::Run);
    }
    if name.strip_prefix("duduclaw-employees-").and_then(|rest| rest.strip_suffix(".slice")).is_some_and(lower_hex_id) {
        return Some(UnitShape::EmployeeSlice);
    }
    None
}

/// A name the enumeration may return at all (what [`UNIT_PATTERNS`] match).
fn listed_name(name: &str) -> bool { name.starts_with("duduclaw-") || name == "duduclaw.slice" }

/// A systemd state word: lowercase letters and `-`.
fn state_word(text: &str) -> bool { !text.is_empty() && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') }

/// Review round 4 item 1 (pure, strict): `systemctl list-units --all
/// --no-legend --plain` output → each unit and whether it is up (anything but
/// `inactive`/`failed`: active, activating, deactivating, reloading, …).
/// A line that is not `<name> <load> <active> <sub> [description]` with known
/// load/active states and a name of the queried patterns, a duplicate, more
/// than [`MAX_LISTED`] lines or non-UTF-8 output: refused.
pub(super) fn parse_unit_list(output: &[u8]) -> Result<Vec<(String, bool)>> {
    const LOAD: [&str; 7] = ["loaded", "not-found", "bad-setting", "error", "masked", "merged", "stub"];
    const ACTIVE: [&str; 8] = ["active", "reloading", "inactive", "failed", "activating", "deactivating", "maintenance", "refreshing"];
    let text = std::str::from_utf8(output).map_err(|_| refused(UNITS_UNREADABLE))?;
    let mut units: Vec<(String, bool)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() { continue; }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [name, load, active, sub, ..] = fields.as_slice() else { return Err(refused(UNITS_UNREADABLE)) };
        if !listed_name(name) || !LOAD.contains(load) || !ACTIVE.contains(active) || !state_word(sub)
            || units.len() >= MAX_LISTED || units.iter().any(|(listed, _)| listed == name) {
            return Err(refused(UNITS_UNREADABLE));
        }
        units.push(((*name).to_owned(), !matches!(*active, "inactive" | "failed")));
    }
    Ok(units)
}

/// Review round 4 item 1 (pure, strict): `systemctl list-jobs --no-legend
/// --plain` output → the unit of every queued job. Each line must be exactly
/// `<id> <unit> <type> <waiting|running>`; anything else refuses.
pub(super) fn parse_job_list(output: &[u8]) -> Result<Vec<String>> {
    let text = std::str::from_utf8(output).map_err(|_| refused(UNITS_UNREADABLE))?;
    let mut jobs: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() { continue; }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [id, name, kind, state] = fields.as_slice() else { return Err(refused(UNITS_UNREADABLE)) };
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || !listed_name(name) || !state_word(kind)
            || !matches!(*state, "waiting" | "running") || jobs.len() >= MAX_LISTED {
            return Err(refused(UNITS_UNREADABLE));
        }
        if !jobs.iter().any(|job| job == name) { jobs.push((*name).to_owned()); }
    }
    Ok(jobs)
}

/// Review round 4 item 1 (pure): `cgroup.events` → `populated`. Every line
/// must be `<key> <digits>`, `populated` exactly once with 0 or 1; otherwise `None`.
pub(super) fn cgroup_populated(text: &str) -> Option<bool> {
    let mut populated = None;
    for line in text.lines() {
        let (key, value) = line.split_once(' ')?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if key == "populated" {
            if populated.is_some() { return None; }
            populated = Some(match value { "0" => false, "1" => true, _ => return None });
        }
    }
    populated
}

/// Review round 4 item 1 (pure): the stop order — the gateway (and the units
/// it binds to), every activation source (timers, then sockets), the
/// services (fixed ones in [`super::rollback::stop_order`], then instances
/// and runs), the per-employee slices, then the aggregate slices.
fn stop_rank(name: &str, shape: UnitShape) -> (u8, usize) {
    let position = super::rollback::stop_order().iter().position(|unit| *unit == name).unwrap_or(usize::MAX);
    let class = if name == UNIT || GATEWAY_READY_UNITS.contains(&name) { 0 }
        else if name.ends_with(".timer") { 1 }
        else if name.ends_with(".socket") { 2 }
        else if name.ends_with(".service") { 3 }
        else if shape == UnitShape::EmployeeSlice { 4 }
        else { 5 };
    (class, position)
}

/// A unit up or with a queued job in one observation round.
#[derive(Clone, Debug)]
struct Busy { name: String, shape: UnitShape }

/// What the last observation round was still waiting for (named at the deadline).
#[derive(Clone, Debug)]
enum Waiting { Unit(String), Cgroup(&'static str), GatewayAccount, Processes }

fn waiting_refusal(waiting: &Waiting) -> DuDuClawError {
    match waiting {
        Waiting::Unit(unit) => unit_not_quiet(unit),
        Waiting::Cgroup(cgroup) => cgroup_not_quiet(cgroup),
        Waiting::GatewayAccount => refused(GATEWAY_PROCESSES),
        Waiting::Processes => refused(ISOLATION_PROCESSES),
    }
}

/// Review round 5 item 4 (pure): the `InvocationID=` of a transient unit's
/// definition text ([`transient_show_is_ours`] admits only 32 lowercase hex or
/// empty); `None` when empty, missing or of any other form.
fn invocation_of(definition: &str) -> Option<&str> {
    definition.lines().find_map(|line| line.strip_prefix("InvocationID=")).filter(|id| lower_hex_id(id))
}

/// One run of [`quiesce_isolation`].
struct Quiesce<'a> {
    control: &'a mut dyn UnitControl,
    deadline: Instant,
    last: Option<Waiting>,
    /// Every unit stopped, with the definition read immediately before its stop (review round 4 item 3).
    stopped: Vec<(Busy, String)>,
    /// Review round 5 item 3: stop attempts per unit.
    attempts: Vec<(String, u32)>,
    /// Review round 5 item 4: a transient unit's InvocationID read (with its
    /// ownership) immediately before its last stop; `None`: it had none.
    invocations: Vec<(String, Option<String>)>,
    /// Transient units killed, with the InvocationID checked immediately before the kill.
    killed: Vec<(String, String)>,
    /// Review round 5 item 1: unknown-shape units already reported as ignored.
    reported: Vec<String>,
}

impl Quiesce<'_> {
    /// Refuses once the deadline has passed, naming what was last still up.
    fn budget(&mut self) -> Result<()> {
        let now = self.control.now();
        if command_budget(now, self.deadline).is_some() { return Ok(()); }
        Err(waiting_refusal(&self.last.clone().unwrap_or_else(|| Waiting::Unit(UNIT_PATTERNS[0].to_owned()))))
    }

    /// One enumeration from PID 1 (units, then jobs): every unit that is up
    /// or has a queued job. Review round 5 item 1: a `duduclaw-` name of any
    /// other shape that is up or has a job refuses by name before anything
    /// else happens in this round; one that is inactive or failed with no job
    /// is ignored (never stopped) and reported once.
    fn observe(&mut self) -> Result<Vec<Busy>> {
        self.budget()?;
        let units = self.control.list_units(self.deadline).map_err(|_| refused(SYSTEMCTL_FAILED))?;
        let units = parse_unit_list(&units)?;
        self.budget()?;
        let jobs = self.control.list_jobs(self.deadline).map_err(|_| refused(SYSTEMCTL_FAILED))?;
        let jobs = parse_job_list(&jobs)?;
        let mut idle_unknown: Vec<String> = Vec::new();
        for (name, up) in &units {
            if unit_shape(name).is_some() { continue; }
            if *up || jobs.contains(name) { return Err(unit_unknown(name)); }
            if !self.reported.contains(name) { idle_unknown.push(name.clone()); }
        }
        for name in &jobs {
            if unit_shape(name).is_none() { return Err(unit_unknown(name)); }
        }
        if !idle_unknown.is_empty() {
            self.control.inform(&units_ignored(&idle_unknown));
            self.reported.extend(idle_unknown);
        }
        let mut busy: Vec<Busy> = Vec::new();
        for name in units.iter().filter(|(_, up)| *up).map(|(name, _)| name).chain(jobs.iter()) {
            if busy.iter().any(|unit| unit.name == *name) { continue; }
            let shape = unit_shape(name).ok_or_else(|| unit_unknown(name))?;
            busy.push(Busy { name: name.clone(), shape });
        }
        busy.sort_by(|a, b| (stop_rank(&a.name, a.shape), &a.name).cmp(&(stop_rank(&b.name, b.shape), &b.name)));
        Ok(busy)
    }

    /// The definition of `unit` when provably ours; refuses by name otherwise.
    fn owned(&mut self, unit: &Busy) -> Result<String> {
        self.budget()?;
        self.control.definition(&unit.name, unit.shape, self.deadline).ok_or_else(|| unit_not_ours(&unit.name))
    }

    /// Review round 4 items 1, 3 / round 5 items 3, 4: every unit busy in
    /// this round is (re-)checked for ownership before any of them is acted
    /// on. A file-backed unit — new, or stopped before and busy again — is
    /// stopped (again). A transient unit is killed only while it is the very
    /// invocation stopped earlier (same InvocationID); with any other
    /// InvocationID (or none) it is a newly appeared unit and is stopped.
    fn act(&mut self, busy: Vec<Busy>) -> Result<()> {
        let mut stop: Vec<Busy> = Vec::new();
        let mut lingering: Vec<(Busy, String)> = Vec::new();
        for unit in busy {
            let definition = self.owned(&unit)?;
            let current = invocation_of(&definition).map(str::to_owned);
            let stopped = self.invocations.iter().find(|(name, _)| *name == unit.name).map(|(_, id)| id.clone());
            match (unit.shape.transient(), current, stopped) {
                (true, Some(current), Some(Some(stopped))) if current == stopped => lingering.push((unit, current)),
                _ => stop.push(unit),
            }
        }
        self.stop_all(&stop)?;
        self.kill_lingering(&lingering)
    }

    /// Review round 4 items 1, 3: stop each of `units` (in stop order),
    /// re-reading what PID 1 loads and the unit file identity IMMEDIATELY
    /// before the stop. A failed stop of a transient unit is judged by the
    /// next observation (it may already be gone); of any other unit it refuses.
    /// Review round 5 item 3: at most [`MAX_STOPS`] stops of one unit.
    fn stop_all(&mut self, units: &[Busy]) -> Result<()> {
        for unit in units {
            let attempts = self.attempts.iter().find(|(name, _)| *name == unit.name).map_or(0, |(_, count)| *count);
            if attempts >= MAX_STOPS { return Err(unit_restarting(&unit.name)); }
            let before = self.owned(unit)?;
            self.budget()?;
            self.attempts.retain(|(name, _)| *name != unit.name);
            self.attempts.push((unit.name.clone(), attempts + 1));
            if unit.shape.transient() {
                let invocation = invocation_of(&before).map(str::to_owned);
                self.invocations.retain(|(name, _)| *name != unit.name);
                self.invocations.push((unit.name.clone(), invocation));
            }
            match self.control.stop(&unit.name, self.deadline) {
                Ok(()) => (),
                Err(_) if unit.shape.transient() => (),
                Err(_) => return Err(unit_not_quiet(&unit.name)),
            }
            self.stopped.push((unit.clone(), before));
        }
        Ok(())
    }

    /// A transient unit already stopped that is still up as the same
    /// invocation: killed once per invocation (SIGKILL, fixed argv). Review
    /// round 5 item 4: its InvocationID is read again (with the ownership
    /// properties) IMMEDIATELY before the kill; unless it is still the one
    /// that passed this round's check, nothing is sent — the next observation
    /// treats it as a newly appeared unit. (`systemctl kill` names the unit,
    /// not the invocation: a concurrent privileged actor replacing it between
    /// that read and the kill is otherwise out of scope.)
    fn kill_lingering(&mut self, units: &[(Busy, String)]) -> Result<()> {
        for (unit, checked) in units {
            if self.killed.iter().any(|(name, id)| *name == unit.name && id == checked) { continue; }
            self.budget()?;
            let now = self.control.definition(&unit.name, unit.shape, self.deadline);
            if now.as_deref().and_then(invocation_of) != Some(checked.as_str()) { continue; }
            self.budget()?;
            // Judged by the next observation (it may have exited meanwhile).
            let _ = self.control.kill(&unit.name, self.deadline);
            self.killed.push((unit.name.clone(), checked.clone()));
        }
        Ok(())
    }

    /// Units and jobs (acting on every busy unit), then the product cgroups.
    /// `None`: no unit up, no job queued and every cgroup `populated 0`.
    fn settle(&mut self) -> Result<Option<Waiting>> {
        let busy = self.observe()?;
        if let Some(first) = busy.first().map(|unit| unit.name.clone()) {
            self.act(busy)?;
            return Ok(Some(Waiting::Unit(first)));
        }
        self.budget()?;
        Ok(self.control.populated_cgroup(self.deadline)?.map(Waiting::Cgroup))
    }

    /// Review round 5 item 2: one round — observe (units, jobs, cgroups), the
    /// gateway account, the full process scan, then the SAME observation
    /// again. `None` only when both observations and the scan were quiet.
    fn round(&mut self) -> Result<Option<Waiting>> {
        if let Some(waiting) = self.settle()? { return Ok(Some(waiting)); }
        self.budget()?;
        if self.control.gateway_account_busy(self.deadline).map_err(|_| refused(GATEWAY_PROCESSES))? {
            return Ok(Some(Waiting::GatewayAccount));
        }
        self.budget()?;
        if self.control.processes_clear(self.deadline).is_err() { return Ok(Some(Waiting::Processes)); }
        self.settle()
    }

    /// Review round 4 item 3: after everything converged, what PID 1 loads
    /// for every stopped unit backed by a unit file must be what it loaded
    /// immediately before the stop; otherwise the unit stopped may not have
    /// been ours. (Transient units are gone once stopped; their Transient=yes
    /// and slice placement were checked before the stop.)
    fn unchanged_across_stop(&mut self) -> Result<()> {
        let stopped = std::mem::take(&mut self.stopped);
        for (unit, before) in stopped.iter().filter(|(unit, _)| !unit.shape.transient()) {
            self.budget()?;
            if self.control.definition(&unit.name, unit.shape, self.deadline).as_ref() != Some(before) {
                return Err(unit_redefined(&unit.name));
            }
        }
        Ok(())
    }
}

/// Review R3 item 6: what is left of `deadline` for one command (at most its
/// own cap); `None` once it has passed — nothing may be spawned then.
pub(super) fn command_budget(now: Instant, deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(now);
    (!remaining.is_zero()).then(|| remaining.min(QUIESCE_COMMAND_CAP))
}

/// Review H2 / R3 items 1, 6 / round 4 items 1, 3, 4 (pure over the seam):
/// bring down everything of this installation that PID 1 has loaded and
/// prove it down. The SAME routine serves the install resume (before any
/// S4–S7 step re-runs) and the rollback ([`quiesce_owned`]). Within ONE
/// absolute deadline of `wait`:
///   1. enumerate from PID 1 every loaded `duduclaw-*`/`duduclaw.slice` unit
///      and every queued job; any such name not of an allowed [`UnitShape`]
///      that is up or has a job refuses by name (one that is inactive or
///      failed with no job is ignored, reported once, never stopped), and
///      BEFORE ANY EFFECT every unit that is up or has a job must be provably
///      ours (else a refusal naming it);
///   2. stop them in [`stop_rank`] order — the gateway and every activation
///      source (timers, sockets) first — re-reading the definition
///      immediately before each stop;
///   3. loop, re-enumerating every round, until ONE round observes no unit
///      up, no job queued, every product cgroup `populated 0` (or absent), no
///      gateway-account process, a passing full S3 process scan, and then
///      again no unit up, no job queued and every cgroup `populated 0`. A unit
///      busy in a later round is checked the same way before it is stopped
///      (again; at most [`MAX_STOPS`] times, then refused by name); a stopped
///      transient unit still up as the same invocation is killed once;
///   4. every stopped file-backed unit must still load the definition (and
///      unit file identity) it had immediately before its stop.
/// At the deadline it refuses naming what was last still up. Concurrent root
/// mutation of the unit files (a package transaction replacing a unit and
/// reloading PID 1 between the last read and the stop) is out of scope beyond
/// the narrowing of step 2 and the check of step 4.
pub(super) fn quiesce_isolation(control: &mut dyn UnitControl, wait: Duration) -> Result<()> {
    let deadline = control.now() + wait;
    let mut run = Quiesce { control, deadline, last: None, stopped: Vec::new(), attempts: Vec::new(),
        invocations: Vec::new(), killed: Vec::new(), reported: Vec::new() };
    loop {
        let Some(waiting) = run.round()? else { break };
        run.last = Some(waiting.clone());
        let now = run.control.now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() { return Err(waiting_refusal(&waiting)); }
        run.control.pause(remaining.min(QUIESCE_POLL));
    }
    run.unchanged_across_stop()
}

/// Review round 4 item 1: the one quiesce of this installation, used by the
/// install resume and by the rollback alike (so the two cannot drift).
pub(super) fn quiesce_owned(control: &mut dyn UnitControl) -> Result<()> {
    quiesce_isolation(control, QUIESCE_WAIT)
}

/// Review R3 item 2 (pure): PID 1's answer to `systemctl show <unit>
/// --property=LoadState,FragmentPath,DropInPaths,NeedDaemonReload,Transient`
/// (`None`: the read failed) names exactly the ledgered unit file, loaded,
/// with no drop-in, no pending daemon-reload and not transient. Any other
/// value, a duplicate or missing line or unparseable output: not ours.
/// (Never made to pass by a `daemon-reload`.)
pub(super) fn unit_show_is_ours(output: Option<&[u8]>, unit: &str) -> bool {
    let Some(text) = output.and_then(|bytes| std::str::from_utf8(bytes).ok()) else { return false };
    let mut fields: Vec<&str> = text.lines().collect();
    fields.sort_unstable();
    let fragment = format!("FragmentPath={}", unit_file(unit));
    fields == ["DropInPaths=", fragment.as_str(), "LoadState=loaded", "NeedDaemonReload=no", "Transient=no"]
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
///
/// Review H2 / R3 item 1: S2/S3 hold while the gateway (unit files published
/// at S4) and the isolation services this transaction started at S6 run. When
/// any step of S4–S7 is about to re-run, the gateway and every isolation unit
/// this installation owns are quiesced FIRST (each stopped only when its unit
/// is provably ours) and the full S3 process check must pass; only then do
/// the steps re-run, S6 starting the services again and S8 among them when
/// claimed (its completion is no longer trusted), or next in order. A pure
/// S8 (S9) resume — nothing in S4–S7 re-runs — leaves the isolation services
/// running (S3 then holds with the narrowed check, `processes_quiet`).
pub(super) fn reestablish(steps: &mut dyn Steps, journal: &Journal) -> Result<()> {
    let Some(last) = claimed_complete(journal) else { return Ok(()) };
    let claimed: Vec<Step> = Step::ORDER.iter().copied()
        .filter(|step| *step >= Step::StopGateway && *step <= last && *step != Step::Commit).collect();
    let mut first = None;
    for step in claimed.iter().copied() {
        if !steps.holds(step)? { first = Some(step); break; }
    }
    let Some(first) = first else { return Ok(()) };
    println!("[重新建立] 日誌記錄 S{} {} 已完成，但主機現況不符；從這一步重新執行。", first.number(), first.label());
    let rerun: Vec<Step> = claimed.into_iter().filter(|step| *step >= first).collect();
    if rerun.iter().any(|step| (Step::PublishPlatform..=Step::Readiness).contains(step)) {
        println!("[重新建立] 重新執行平台步驟前，先停止 Gateway 與這次安裝的所有隔離服務，並確認沒有殘留程序。");
        steps.quiesce()?;
    }
    for step in rerun {
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

struct Live<'a> { context: &'a mut Context, dns: &'a Dns, services_started: bool }

impl Steps for Live<'_> {
    fn holds(&mut self, step: Step) -> Result<bool> {
        Ok(invariants(step, &*self.context, self.dns, self.services_started).is_ok())
    }
    fn perform(&mut self, step: Step) -> Result<()> {
        perform(step, &mut *self.context, self.dns, self.services_started)?;
        // S6 (re-)started this transaction's isolation services.
        if step == Step::StartServices { self.services_started = true; }
        Ok(())
    }
    fn quiesce(&mut self) -> Result<()> {
        let ledger = self.context.recorder.ledger();
        with_version_lock(|| quiesce_owned(&mut LiveUnits { ledger }))?;
        // Review R3 item 1: none of this transaction's services runs now; until
        // S6 re-runs, every S3 check on this path is the full one.
        self.services_started = false;
        Ok(())
    }
}

/// A system command bounded by the absolute `deadline` (review R3 item 6,
/// round 4 item 4: `command_within` checks it on entry, before and after the
/// spawn, and gives the child `min(30 s, what is left)`).
fn command_until(name: &str, args: &[&str], deadline: Instant) -> Result<Output> {
    command_within(name, args, deadline)
}

/// The ledgered unit file of `unit`, exactly as recorded, as comparable text
/// (device, inode, mode, digest). `None`: not ledgered, absent, replaced or unreadable.
fn ledgered_unit_file(ledger: &ledger::Ledger, unit: &str) -> Option<String> {
    let path = unit_file(unit);
    let entry = ledger.entry(&path)?;
    let identity = (|| -> Result<Option<ledger::Identity>> {
        if !ledger::present(&path)? { return Ok(None); }
        let (directory, name) = ledger::open_parent(&path)?;
        if ledger::classify_at(&directory, &name, entry, &ledger::boot_id()?)? != ledger::Now::Recorded { return Ok(None); }
        ledger::identity_at(&directory, &name, Kind::File)
    })().ok().flatten()?;
    Some(format!("{path} {}:{} {:o} {:?}", identity.device, identity.inode, identity.mode, identity.sha256))
}

/// Review round 4 item 1 (pure): PID 1's answer to `systemctl show <unit>
/// --property=LoadState,Transient,Slice,ControlGroup,InvocationID` for a
/// transient unit of spawnd: loaded, `Transient=yes`, and placed under the
/// product's employees slice — a run in a per-employee slice, a per-employee
/// slice directly in `duduclaw-employees.slice` — with its control group
/// (when it has one) exactly there. Review round 5 item 4: `InvocationID`
/// is 32 lowercase hex, or empty (not started yet). Any other value, a
/// duplicate, missing or extra line: not ours.
pub(super) fn transient_show_is_ours(output: &[u8], unit: &str, shape: UnitShape) -> bool {
    let Ok(text) = std::str::from_utf8(output) else { return false };
    let mut fields: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { return false };
        if fields.iter().any(|(seen, _)| *seen == key) { return false; }
        fields.push((key, value));
    }
    let value = |key: &str| fields.iter().find(|(seen, _)| *seen == key).map(|(_, value)| *value);
    let (Some("loaded"), Some("yes"), Some(slice), Some(cgroup), Some(invocation)) =
        (value("LoadState"), value("Transient"), value("Slice"), value("ControlGroup"), value("InvocationID")) else { return false };
    if fields.len() != 5 || !(invocation.is_empty() || lower_hex_id(invocation)) { return false; }
    let expected = match shape {
        UnitShape::Run if unit_shape(slice) == Some(UnitShape::EmployeeSlice) =>
            format!("/duduclaw.slice/duduclaw-employees.slice/{slice}/{unit}"),
        UnitShape::EmployeeSlice if slice == "duduclaw-employees.slice" =>
            format!("/duduclaw.slice/duduclaw-employees.slice/{unit}"),
        _ => return false,
    };
    unit_shape(unit) == Some(shape) && (cgroup.is_empty() || cgroup == expected)
}

/// Review R3 item 2 / round 4 items 1, 3: what PID 1 loads for `unit` of
/// `shape` when it is provably this installation's, as comparable text.
/// A fixed unit: its ledgered unit file exactly as recorded AND PID 1 loading
/// exactly that file ([`unit_show_is_ours`]). A template instance: the same,
/// with the TEMPLATE's ledgered file as the fragment. A transient run or
/// employee slice: [`transient_show_is_ours`] (the text carries its
/// InvocationID, [`invocation_of`]). Any failure: `None`.
fn live_definition(ledger: &ledger::Ledger, unit: &str, shape: UnitShape, deadline: Instant) -> Option<String> {
    let show = |properties: &str| command_until("systemctl", &["show", unit, properties], deadline).ok()
        .filter(|output| output.status.success()).map(|output| output.stdout);
    match shape {
        UnitShape::Fixed | UnitShape::Instance(_) => {
            let source = match shape { UnitShape::Instance(template) => template, _ => unit };
            let file = ledgered_unit_file(ledger, source)?;
            let stdout = show("--property=LoadState,FragmentPath,DropInPaths,NeedDaemonReload,Transient")?;
            unit_show_is_ours(Some(stdout.as_slice()), source).then(|| format!("{file}\n{}", String::from_utf8_lossy(&stdout)))
        }
        UnitShape::Run | UnitShape::EmployeeSlice => {
            let stdout = show("--property=LoadState,Transient,Slice,ControlGroup,InvocationID")?;
            transient_show_is_ours(&stdout, unit, shape).then(|| String::from_utf8_lossy(&stdout).into_owned())
        }
    }
}

/// Review R3 item 2: the unit file of `unit` is the object this transaction
/// ledgered (identity and content as recorded) AND PID 1 loads exactly it
/// ([`unit_show_is_ours`]). Any failure: false.
fn live_unit_is_ours(ledger: &ledger::Ledger, unit: &str, deadline: Instant) -> bool {
    live_definition(ledger, unit, UnitShape::Fixed, deadline).is_some()
}

/// Review H2 / R3 item 1 / round 4 item 1: the live units, cgroups, the
/// gateway account and the full process scan (install resume and rollback).
pub(super) struct LiveUnits<'a> { pub(super) ledger: &'a ledger::Ledger }

impl UnitControl for LiveUnits<'_> {
    fn now(&mut self) -> Instant { Instant::now() }
    fn list_units(&mut self, deadline: Instant) -> Result<Vec<u8>> {
        let output = command_until("systemctl", &["list-units", "--all", "--no-legend", "--plain", "--no-pager",
            UNIT_PATTERNS[0], UNIT_PATTERNS[1]], deadline)?;
        if !output.status.success() { return Err(refused(SYSTEMCTL_FAILED)); }
        Ok(output.stdout)
    }
    fn list_jobs(&mut self, deadline: Instant) -> Result<Vec<u8>> {
        // `--no-legend` prints nothing at all when no job matches.
        let output = command_until("systemctl", &["list-jobs", "--no-legend", "--plain", "--no-pager",
            UNIT_PATTERNS[0], UNIT_PATTERNS[1]], deadline)?;
        if !output.status.success() { return Err(refused(SYSTEMCTL_FAILED)); }
        Ok(output.stdout)
    }
    fn definition(&mut self, unit: &str, shape: UnitShape, deadline: Instant) -> Option<String> {
        live_definition(self.ledger, unit, shape, deadline)
    }
    fn stop(&mut self, unit: &str, deadline: Instant) -> Result<()> {
        let output = command_until("systemctl", &["stop", unit], deadline)?;
        if !output.status.success() { return Err(refused(format!("無法停止 {unit}。"))); }
        Ok(())
    }
    fn kill(&mut self, unit: &str, deadline: Instant) -> Result<()> {
        let output = command_until("systemctl", &["kill", "--signal=SIGKILL", unit], deadline)?;
        if !output.status.success() { return Err(refused(format!("無法結束 {unit}。"))); }
        Ok(())
    }
    fn populated_cgroup(&mut self, deadline: Instant) -> Result<Option<&'static str>> {
        if statfs_type(c"/sys/fs/cgroup") != Some(CGROUP2_MAGIC) { return Err(refused(CGROUP_UNREADABLE)); }
        for cgroup in PRODUCT_CGROUPS {
            // Nothing is read once the deadline has passed.
            if Instant::now() >= deadline { return Err(cgroup_not_quiet(cgroup)); }
            let path = format!("/sys/fs/cgroup/{cgroup}/cgroup.events");
            let file = match fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(refused(CGROUP_UNREADABLE)),
            };
            let mut text = String::new();
            file.take(4096).read_to_string(&mut text).map_err(|_| refused(CGROUP_UNREADABLE))?;
            match cgroup_populated(&text) {
                Some(false) => (),
                Some(true) => return Ok(Some(cgroup)),
                None => return Err(refused(CGROUP_UNREADABLE)),
            }
        }
        Ok(None)
    }
    fn gateway_account_busy(&mut self, deadline: Instant) -> Result<bool> {
        let output = command_until("getent", &["passwd", "duduclaw"], deadline)?;
        let Some(gateway) = passwd_from("duduclaw", output)? else { return Ok(false) };
        uid_processes(|uid| uid == gateway.uid, Some(deadline))
    }
    fn processes_clear(&mut self, deadline: Instant) -> Result<()> {
        super::removal::processes_clear_until(Some(deadline))
    }
    fn pause(&mut self, pause: Duration) { std::thread::sleep(pause); }
    fn inform(&mut self, line: &str) { println!("{line}"); }
}

/// Any live process with a real, effective, saved or filesystem uid `matches`
/// accepts. With a `deadline` (review round 4 item 4) no `/proc` entry is read
/// once it has passed (refused).
fn uid_processes(matches: impl Fn(u32) -> bool, deadline: Option<Instant>) -> Result<bool> {
    let mut count = 0usize;
    for entry in fs::read_dir("/proc")? {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) { return Err(refused("程序檢查超過時限。")); }
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        if pid == std::process::id() { continue; }
        count += 1;
        if count > 100_000 { return Err(refused("程序檢查超過上限。")); }
        let mut status = String::new();
        let read = fs::File::open(entry.path().join("status"))
            .and_then(|file| file.take(64 * 1024).read_to_string(&mut status));
        match read {
            Ok(_) => (),
            // The process exited meanwhile.
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => continue,
            Err(error) => return Err(error.into()),
        }
        let Some(line) = status.lines().find_map(|line| line.strip_prefix("Uid:")) else {
            // A process that exited while being read has an empty status.
            if status.is_empty() { continue; }
            return Err(refused("無法確認程序身分。"));
        };
        let ids = line.split_whitespace().map(str::parse::<u32>).collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| refused("無效的程序身分。"))?;
        if ids.len() != 4 { return Err(refused("無效的程序身分。")); }
        if ids.into_iter().any(&matches) { return Ok(true); }
    }
    Ok(false)
}

/// Plan S3. Before this transaction started its isolation services, nothing
/// of DuDuClaw may run (`processes_clear`). From S6 on, those services are
/// this transaction's own (S6's invariants check them); the gateway account
/// and the employee range must still have no process.
///
/// Review R3 item 1: the narrowed (post-S6) variant is used ONLY while nothing
/// in S4–S7 re-runs — the S3 invariant of a pure S8/S9 resume. Before any
/// S4–S7 re-run the services are stopped and the full `processes_clear` runs
/// (`quiesce_isolation`), and `Live` then passes `services_started: false`
/// until S6 has started them again.
fn processes_quiet(services_started: bool) -> Result<()> {
    if !services_started { return super::removal::processes_clear(); }
    let gateway = passwd_entry("duduclaw")?.map(|entry| entry.uid);
    let employees = duduclaw_isolation::ids::EMPLOYEE_UID_MIN..=duduclaw_isolation::ids::EMPLOYEE_UID_MAX;
    if uid_processes(|uid| Some(uid) == gateway || employees.contains(&uid), None)? {
        return Err(refused("仍有 Gateway 或員工程序在執行。"));
    }
    Ok(())
}

fn perform(step: Step, context: &mut Context, dns: &Dns, services_started: bool) -> Result<()> {
    match step {
        Step::Verified | Step::HostPreflight | Step::Rollback => Err(refused(JOURNAL_DAMAGED)),
        Step::StopGateway => with_version_lock(gateway_not_running),
        Step::ProcessesClear => with_version_lock(|| processes_quiet(services_started)),
        Step::PublishPlatform => publish_platform(context, dns),
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
fn invariants(step: Step, context: &Context, dns: &Dns, services_started: bool) -> Result<()> {
    match step {
        // A gateway may run only as the one whose unit files this transaction
        // published at S4 (S6 starts it, S8 enables it; it is quiesced before
        // any S4–S7 step re-runs, review H2).
        Step::StopGateway => with_version_lock(|| if gateway_not_running().is_ok() { Ok(()) } else { gateway_ours(context) }),
        Step::ProcessesClear => with_version_lock(|| {
            if gateway_active() && gateway_ours(context).is_ok() { Ok(()) } else { processes_quiet(services_started) }
        }),
        Step::PublishPlatform => with_version_lock(|| platform_published(context, dns)),
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

/// Review M2: why nsswitch.conf is not acceptable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NssProblem {
    /// An external account source the code names (LDAP, SSSD, NIS, …).
    External,
    /// A source the code does not classify, or `compat` whose `*_compat` line reads more than files.
    Unknown(String),
    /// An unterminated `[ … ]` action item.
    Malformed,
    /// Review R3 item 3: an account database line (or the `*_compat` line
    /// `compat` reads) given more than once.
    Duplicate(String),
    /// Review R3 item 3: `compat` without its `*_compat` line (glibc's default
    /// there is NIS) or with one that lists nothing; names that line.
    CompatUnbounded(String),
}

/// Account sources the installer classifies as local.
const NSS_LOCAL: [&str; 2] = ["files", "systemd"];
const NSS_EXTERNAL: [&str; 6] = ["ldap", "sss", "winbind", "nis", "nisplus", "hesiod"];

/// One nsswitch.conf line (comment removed) as `(database, sources)`; the
/// `[STATUS=action …]` items are parsed and dropped, never taken as sources.
fn nss_line(line: &str) -> std::result::Result<Option<(&str, Vec<&str>)>, NssProblem> {
    let Some((database, rest)) = line.split_once(':') else { return Ok(None) };
    let mut sources = Vec::new();
    let mut rest = rest.trim_start();
    while !rest.is_empty() {
        if let Some(action) = rest.strip_prefix('[') {
            let (_, after) = action.split_once(']').ok_or(NssProblem::Malformed)?;
            rest = after.trim_start();
            continue;
        }
        let end = rest.find(|c: char| c.is_whitespace() || c == '[').unwrap_or(rest.len());
        let (source, after) = rest.split_at(end);
        sources.push(source);
        rest = after.trim_start();
    }
    Ok(Some((database.trim(), sources)))
}

/// Review M2 (pure): allowlist semantics for the `passwd`, `group`, `shadow`
/// and `initgroups` databases. `files` and `systemd` are local; the external
/// sources the code names refuse as before; `compat` is local only when the
/// corresponding `passwd_compat` / `group_compat` / `shadow_compat` line is
/// PRESENT exactly once and lists only `files` (review R3 item 3: an absent
/// one means NIS to glibc); every other source refuses by name. A database
/// line of `passwd`, `group`, `shadow` or `initgroups` given twice refuses.
pub(super) fn nss_verdict(nsswitch: &str) -> std::result::Result<(), NssProblem> {
    let mut lines = Vec::new();
    for line in nsswitch.lines() {
        if let Some(parsed) = nss_line(line.split('#').next().unwrap_or(""))? { lines.push(parsed); }
    }
    for database in ["passwd", "group", "shadow", "initgroups"] {
        if lines.iter().filter(|(line, _)| *line == database).count() > 1 {
            return Err(NssProblem::Duplicate(database.to_owned()));
        }
    }
    // What `compat` reads besides the local file: the `<database>_compat` line.
    let compat_reads = |database: &str| -> std::result::Result<(), NssProblem> {
        let name = format!("{database}_compat");
        let matching: Vec<&Vec<&str>> = lines.iter().filter(|(line, _)| *line == name).map(|(_, sources)| sources).collect();
        let sources = match matching.as_slice() {
            [sources] if !sources.is_empty() => *sources,
            [] | [_] => return Err(NssProblem::CompatUnbounded(name)),
            _ => return Err(NssProblem::Duplicate(name)),
        };
        for source in sources {
            if *source == "files" { continue; }
            if NSS_EXTERNAL.contains(source) { return Err(NssProblem::External); }
            return Err(NssProblem::Unknown((*source).to_owned()));
        }
        Ok(())
    };
    for (database, sources) in &lines {
        let compat = match *database {
            "passwd" => "passwd", "shadow" => "shadow", "group" | "initgroups" => "group",
            _ => continue,
        };
        for source in sources {
            if NSS_LOCAL.contains(source) { continue; }
            if NSS_EXTERNAL.contains(source) { return Err(NssProblem::External); }
            if *source == "compat" { compat_reads(compat)?; continue; }
            return Err(NssProblem::Unknown((*source).to_owned()));
        }
    }
    Ok(())
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
fn filesystem_bytes(path: &CStr) -> Option<(u64, u64)> {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: NUL-terminated path; the buffer is written on success.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 { return None; }
    // SAFETY: initialized by the successful statvfs.
    let stat = unsafe { stat.assume_init() };
    Some(((stat.f_blocks as u64).checked_mul(stat.f_frsize as u64)?,
        (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64)?))
}

fn free_bytes(path: &CStr) -> Option<u64> { filesystem_bytes(path).map(|(_, free)| free) }

const MKFS_REQUIRED: &str = "缺少可信任的 /usr/sbin/mkfs.ext4；請安裝 e2fsprogs，並確認檔案及上層目錄由 root 管理。";
const LOOP_REQUIRED: &str = "核心 loop 模組必須可用：請執行 modprobe loop，確認 /dev/loop-control 是字元裝置 10:237。";
const RESERVE_REQUIRED: &str = "/var/lib 所在檔案系統的可用空間必須至少保留 20 GiB 或總容量的 10%（取較大值）。";
const DMSETUP_REQUIRED: &str = "無法讀取 device-mapper 裝置表；請確認已安裝 dmsetup／lvm2 套件，且 dmsetup 可由 root 執行。";

// Pure decisions also cover absent/unreadable host evidence (fail closed).
fn storage_tools_clear(mkfs_trusted: bool, loop_device: Option<(bool, u64, u64)>) -> std::result::Result<(), &'static str> {
    if !mkfs_trusted { return Err(MKFS_REQUIRED); }
    if loop_device != Some((true, 10, 237)) { return Err(LOOP_REQUIRED); }
    Ok(())
}
fn storage_reserve_clear(capacity: Option<(u64, u64)>) -> std::result::Result<(), &'static str> {
    match capacity {
        Some((total, free)) if total != 0 && free <= total && free >= duduclaw_isolation::storage_reserve_bytes(total) => Ok(()),
        _ => Err(RESERVE_REQUIRED),
    }
}
fn storage_tools() -> std::result::Result<(), &'static str> {
    use std::os::unix::fs::FileTypeExt;
    let mkfs = Path::new("/usr/sbin/mkfs.ext4");
    let trusted = fs::canonicalize(mkfs).ok().is_some_and(|path|
        parents(&path).and_then(|_| trusted(&path, false)).is_ok());
    let device = fs::symlink_metadata("/dev/loop-control").ok().map(|metadata|
        (metadata.file_type().is_char_device(), libc::major(metadata.rdev()) as u64, libc::minor(metadata.rdev()) as u64));
    storage_tools_clear(trusted, device)
}

const REQUIRED_TOOLS: [&str; 9] = ["systemctl", "systemd-tmpfiles", "useradd", "usermod", "getent", "id", "loginctl", "python3", "setpriv"];

fn required_tools_present(mut present: impl FnMut(&str) -> bool) -> bool {
    REQUIRED_TOOLS.into_iter().all(|tool| present(tool))
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
    // Review R3 item 5: a rollback quarantine directory no ledger of this transaction explains.
    match super::rollback::unexplained_quarantines(ours) {
        Ok(found) if found.is_empty() => (),
        Ok(found) => fail(&format!("這台主機有先前回退留下、沒有安裝建立紀錄可說明的隔離目錄（{}）；裡面可能仍有完整的檔案或目錄樹。請先確認內容並處理後再安裝。", found.join("、"))),
        Err(_) => fail("無法檢查先前回退留下的隔離目錄（.duduclaw-rollback-*）。"),
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
    if let Err(message) = storage_tools() { fail(message); }
    if let Err(message) = storage_reserve_clear(filesystem_bytes(c"/var/lib")) { fail(message); }
    if !required_tools_present(tool_present) {
        fail("缺少必要的系統工具（systemctl、systemd-tmpfiles、useradd、usermod、getent、id、loginctl、python3、setpriv）。");
    }
    // Identities (design §5.1).
    match read_root_text("/etc/login.defs", 1024 * 1024) {
        Ok(Some(defs)) => {
            if layout::system_id_limit(&defs, "SYS_UID_MAX").is_err() || layout::system_id_limit(&defs, "SYS_GID_MAX").is_err() {
                fail("/etc/login.defs 的 SYS_UID_MAX 或 SYS_GID_MAX 設定或推算結果無效：請檢查是否有格式錯誤、重複設定，或數值超出允許範圍。");
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
    if let Err(message) = nss_clear() { fail(&message); }
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
    let decision = dns_route(resolved_available(), upstream, upstream_valid);
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

fn nss_clear() -> std::result::Result<(), String> {
    let text = match read_root_text("/etc/nsswitch.conf", 256 * 1024) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(()),
        Err(_) => return Err("無法讀取 /etc/nsswitch.conf。".into()),
    };
    match nss_verdict(&text) {
        Ok(()) => Ok(()),
        Err(NssProblem::External) =>
            Err("偵測到外部帳號服務（LDAP／SSSD 等）。本版安裝程式不支援這類主機：外部帳號服務必須先永久保留員工 uid 範圍 70000–99999，並由原廠協助確認。".into()),
        Err(NssProblem::Unknown(source)) => Err(format!(
            "/etc/nsswitch.conf 的帳號資料庫（passwd、group、shadow、initgroups）使用了安裝程式無法判定的來源「{source}」；只接受 files、systemd，以及對應 *_compat 設定剛好一行且只列出 files 的 compat。請先移除該來源，或聯絡原廠確認。")),
        Err(NssProblem::Malformed) => Err("/etc/nsswitch.conf 的動作設定（[ … ]）格式不完整，無法判定帳號資料來源。".into()),
        Err(NssProblem::Duplicate(line)) => Err(format!(
            "/etc/nsswitch.conf 的「{line}:」設定出現不只一次，無法判定實際使用的帳號資料來源；請只保留一行。")),
        Err(NssProblem::CompatUnbounded(line)) => Err(format!(
            "/etc/nsswitch.conf 的帳號資料庫使用 compat，但沒有剛好一行只列出 files 的「{line}:」設定（未設定時 glibc 會改向 NIS 查詢）。請加上一行「{line}: files」，或改用 files。")),
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
    let decision = dns_route(resolved_available(), dns.upstream, upstream_valid);
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
            let table = devices.dm_table(device).ok_or(DMSETUP_REQUIRED)?;
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

/// Review L1: `install_isolation_support_for` re-applies `layout::DIRECTORIES`
/// modes, some to directories this transaction created and ledgered (the
/// trust trees of R1's publish among them). Exactly that transition — the
/// same object with the layout's permission bits — is re-recorded durably;
/// any other change stays a different identity. Also run first on a re-run,
/// for a run interrupted between the chmod and its re-record.
fn rerecord_layout_modes(recorder: &mut LiveRecorder) -> Result<()> {
    for (path, mode) in layout::DIRECTORIES { recorder.rerecord_mode(path, *mode)?; }
    Ok(())
}

/// The unit files are written here with their final bytes: the broker unit
/// carries the S1 upstream's allow line (`layout::installed_unit_text`; the
/// journal's DNS decision, fixed for the run), so no later step rewrites a
/// ledgered file. S6 reloads systemd before it starts the broker socket.
fn publish_platform(context: &mut Context, dns: &Dns) -> Result<()> {
    let upstream = broker_upstream(dns)?;
    rerecord_layout_modes(&mut context.recorder)?;
    // (a) Trust root, state, slot A, generation and `current` (R1). Called
    // WITHOUT the version lock: publish takes stage lock → version-lock →
    // update.lock itself (see LOCK ORDER above). Re-entrant. The trees it
    // creates are ledgered first (review F4).
    let stage = &context.stage;
    let state_root = duduclaw_updated::linux::STATE_ROOT;
    // Review round 6 item 1: a state root R1 created in an interrupted run
    // before it was confirmed is confirmed now (or the step refuses).
    resume_component(&mut context.recorder, state_root, context.generation)?;
    // Review round 4 item 2: the window of R1's publish opens here.
    let window = ledger::window_start();
    let digest = context.recorder.record(&TRUST_TREES.map(|tree| (tree, Kind::Tree)), || {
        // Review R3 item 4: the trees publish uses as it finds them (same
        // owner and mode) are created here first, so this installer's own
        // descriptors prove their creation. The state root is created only by
        // publish, under its locks (R1): recorded below by component proof.
        for (tree, mode) in duduclaw_updated::linux::trust_init::UPDATE_DIRECTORIES {
            if TRUST_TREES.contains(tree) && *tree != state_root { ensure_directory(tree, *mode)?; }
        }
        duduclaw_updated::linux::trust_init::publish(stage)
            .map_err(|error| refused(format!("無法發布更新信任根與世代（{:?}）。", error.code())))
    })?;
    if digest != context.generation { return Err(refused(STAGE_UNVERIFIED)); }
    // Recorded only after publish succeeded for this generation, as the
    // root-owned private directory on its parent's mount publish leaves,
    // absent at the durable intent `record` made right before publish (review
    // round 5 item 5), and holding R1's state file for this generation with
    // nothing pending. Not proven: the step fails (review round 4 item 2).
    let names_generation = state_names_generation(context.generation);
    let absent = context.recorder.absent_at_intent(state_root);
    context.recorder.confirm_component(state_root,
        |identity| Ok(state_root_proven(identity, &component_facts(state_root, absent, window), names_generation)))?;
    let manifest = readiness::authenticated_manifest(digest)?;
    let version = duduclaw_updated::version_text(manifest.version);
    if manifest.artifacts != context.stage.artifacts || version != duduclaw_gateway::updater::current_version() {
        return Err(refused(STAGE_UNVERIFIED));
    }
    let generation_hex = context.generation_hex.clone();
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
        items.extend([(parse_lease::CONFIG, Kind::File), (parse_lease::DIRECTORY, Kind::Tree)]);
        items.extend(units.iter().map(|unit| (unit.as_str(), Kind::File)));
        // Check point 1 for the helper accounts created below.
        verify_identities(GATEWAY_ONLY)?;
        recorder.record(&items, || {
            parse_runtime_tree(true)?;
            install_isolation_support_for(gateway, &defs, upstream)
        })?;
        rerecord_layout_modes(recorder)?;
        // Check point 2 for the helper accounts (check point 3: S6, before they run).
        verify_identities(&service_names())?;
        spawnd_anchors(digest, &|entitlements| entitlements_admitted(digest, &generation_hex, entitlements))?;
        // (d) The nightly update requester: files only; enabled at S6 (Q11).
        nightly_files(recorder)?;
        // (e) C3: the gateway unit files; neither enabled nor started here.
        gateway_unit_files(recorder)
    })
}

/// C3: spawnd's resource check loads `duduclaw-gateway.service`, so its unit
/// file, the recovery bound and the launcher are published at S4, written
/// exactly as S8 wrote them before (review R3 item 4: recorded only as the
/// files these writes prove they created; rollback removes them). Neither
/// enabled nor started: S6 starts the gateway, S8 enables it.
fn gateway_unit_files(recorder: &mut LiveRecorder) -> Result<()> {
    let text = duduclaw_core::root_key::recovery::isolated_unit_text();
    recorder.record(&[(UNIT_PATH, Kind::File), (RECOVERY_BOUND, Kind::File), (LAUNCHER, Kind::File)], || {
        invalidate_recovery_bound()?;
        update_text_if_different(UNIT_PATH, 0o644, &text)?;
        update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT)?;
        reload_and_certify_recovery_text(&text)
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
    passwd_from(name, command("getent", &["passwd", name])?)
}

/// The answer of `getent passwd <name>` (`output`), parsed as [`passwd_entry`] does.
fn passwd_from(name: &str, output: Output) -> Result<Option<PasswdEntry>> {
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

/// Review H1: spawnd's root host publication `publication.json`. Its consumer
/// type (`duduclaw-spawnd` installed.rs `HostPublication`) is mirrored here
/// with the same three fields and `deny_unknown_fields` (pinned by
/// `spawnd_publication_field_set_is_pinned`). This install writes the empty
/// list; after it, spawnd writes the entitlements (R8d), so the records stay
/// untyped JSON here and are judged by spawnd's own check
/// (`signed_policy_admits_entitlements`).
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostPublication {
    pub(super) version: u16,
    pub(super) generation: Sha256Digest,
    pub(super) entitlements: Vec<serde_json::Value>,
}

impl HostPublication {
    /// Exactly what this install publishes: version 1, this generation, no entitlement.
    pub(super) fn expected(generation: Sha256Digest) -> Self {
        Self { version: 1, generation, entitlements: Vec::new() }
    }
}

/// Review H1: the publication below `directory`, read strictly: opened
/// without following a link, a regular file with one link owned by
/// `owner` (root:root in production), mode exactly 0600, bounded, parsed with
/// `deny_unknown_fields`. `Ok(None)`: absent; anything else unreadable refuses.
pub(super) fn read_publication_at(directory: &fs::File, (uid, gid): (u32, u32)) -> Result<Option<HostPublication>> {
    let mismatch = || refused(PUBLICATION_MISMATCH);
    // SAFETY: live dirfd and a NUL-terminated single-component name.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), c"publication.json".as_ptr(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) { return Ok(None); }
        return Err(mismatch());
    }
    // SAFETY: a fresh descriptor owned here.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != uid || metadata.gid() != gid
        || metadata.mode() & 0o7777 != 0o600 || metadata.len() > MAX_PUBLICATION_BYTES {
        return Err(mismatch());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PUBLICATION_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PUBLICATION_BYTES { return Err(mismatch()); }
    serde_json::from_slice(&bytes).map(Some).map_err(|_| mismatch())
}

/// Review H1 / R8d C: S4 holds for the publication when it is version 1 of
/// THIS generation and its entitlements are either the empty list this
/// install publishes or, once spawnd has entitled employees, records that
/// `admitted` (spawnd's production check against this generation's signed
/// policy) accepts. Another generation or invalid content never holds.
pub(super) fn publication_holds(directory: &fs::File, owner: (u32, u32), generation: Sha256Digest,
    admitted: &dyn Fn(&[serde_json::Value]) -> bool) -> bool {
    matches!(read_publication_at(directory, owner), Ok(Some(publication)) if publication_admitted(&publication, generation, admitted))
}

fn publication_admitted(publication: &HostPublication, generation: Sha256Digest,
    admitted: &dyn Fn(&[serde_json::Value]) -> bool) -> bool {
    let expected = HostPublication::expected(generation);
    publication.version == expected.version && publication.generation == expected.generation
        && (publication.entitlements.is_empty() || admitted(&publication.entitlements))
}

/// `/var/lib/duduclaw-spawnd` as a no-follow dirfd after its ancestors and itself were checked.
fn spawnd_root_fd() -> Result<fs::File> {
    let root = Path::new(SPAWND_ROOT);
    parents(root)?;
    trusted(root, true)?;
    Ok(fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root)?)
}

/// spawnd's root anchors and its root-only host publication (no entitlement
/// on a fresh install: spawnd writes them later through its own RPC, R8d; a
/// re-run after that accepts them when `admitted`, never rewriting them).
fn spawnd_anchors(digest: Sha256Digest, admitted: &dyn Fn(&[serde_json::Value]) -> bool) -> Result<()> {
    let root = Directory::open(Path::new(SPAWND_ROOT)).map_err(|_| refused("隔離執行服務的狀態目錄不符。"))?;
    for name in SPAWND_ANCHORS {
        match root.child(name) {
            Ok(_) => (),
            Err(error) if update_missing(&error) => { root.create_child(name).map_err(|_| refused("無法建立隔離執行服務的目錄。"))?; }
            Err(_) => return Err(refused("隔離執行服務的目錄不符。")),
        }
        let metadata = fs::symlink_metadata(Path::new(SPAWND_ROOT).join(name))?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.gid() != 0 || metadata.mode() & 0o7777 != 0o700 {
            return Err(refused("隔離執行服務的目錄權限不符。"));
        }
    }
    // Review H1: an existing publication must be this install's generation
    // with admitted entitlements (never overwritten); an absent one is
    // written (root 0600, no entitlement) and read back.
    let expected = HostPublication::expected(digest);
    let directory = spawnd_root_fd()?;
    match read_publication_at(&directory, (0, 0))? {
        Some(existing) if publication_admitted(&existing, digest, admitted) => (),
        Some(_) => return Err(refused(PUBLICATION_MISMATCH)),
        None => root.atomic_json(SPAWND_PUBLICATION, &expected).map_err(|_| refused("無法寫入隔離執行服務的發布紀錄。"))?,
    }
    if !publication_holds(&directory, (0, 0), digest, admitted) { return Err(refused(PUBLICATION_MISMATCH)); }
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
    let bytes = signed_policy_bytes(context.generation, &context.generation_hex)?;
    duduclaw_spawnd::installed::signed_policy_egress_profiles(&bytes).map_err(|_| refused(POLICY_REFUSED))
}

/// The bytes of `generation`'s signed policy artifact, exactly as its
/// authenticated manifest binds them (mode, size, sha256).
fn signed_policy_bytes(generation: Sha256Digest, generation_hex: &str) -> Result<Vec<u8>> {
    use duduclaw_updated::linux::INSTALL_ROOT;
    let manifest = readiness::authenticated_manifest(generation)?;
    let target = InstallTarget::RuntimeEntry { registry_id: POLICY_REGISTRY_ID.into() };
    let artifact = single_artifact(&manifest.artifacts, &target).ok_or_else(|| refused(POLICY_REFUSED))?;
    if artifact.mode != 0o444 || artifact.size_bytes == 0 || artifact.size_bytes > 1024 * 1024 { return Err(refused(POLICY_REFUSED)); }
    let name = duduclaw_updated::publication::artifact_name(&target).map_err(|_| refused(POLICY_REFUSED))?;
    let directory = Directory::open(Path::new(INSTALL_ROOT)).and_then(|install| install.child("generations"))
        .and_then(|generations| generations.child(generation_hex)).map_err(|_| refused(POLICY_REFUSED))?;
    let bytes = directory.bytes(&name, 1024 * 1024).map_err(|_| refused(POLICY_REFUSED))?;
    if bytes.len() as u64 != artifact.size_bytes || duduclaw_updated::publication::manifest_digest(&bytes) != artifact.sha256 {
        return Err(refused(POLICY_REFUSED));
    }
    Ok(bytes)
}

/// R8d C: whether the entitlements of an existing host publication pass
/// spawnd's production check against `generation`'s signed policy. The empty
/// list (a fresh install) needs no policy read; anything unreadable admits
/// nothing.
fn entitlements_admitted(generation: Sha256Digest, generation_hex: &str, entitlements: &[serde_json::Value]) -> bool {
    entitlements.is_empty() || signed_policy_bytes(generation, generation_hex).is_ok_and(|bytes|
        duduclaw_spawnd::installed::signed_policy_admits_entitlements(&bytes, entitlements).is_ok())
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
    let recorded = match (dns.route, dns.upstream) {
        (DnsRoute::DedicatedDnsBroker, Some(upstream)) => {
            let written: BrokerConfig = read_root_config(BROKER_CONFIG_PATH).map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
            written.validate().map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
            if written.upstream != upstream { return Err(refused("DNS 轉送服務設定讀回不符。")); }
            Some(written.upstream)
        }
        (DnsRoute::ResolvedVarlink, None) => if exists(BROKER_CONFIG_PATH)? {
            return Err(refused("使用 systemd-resolved 的主機不應有 DNS 轉送服務設定；請檢查 /etc/duduclaw/dns-broker.json。"));
        } else { None },
        _ => return Err(refused(JOURNAL_DAMAGED)),
    };
    // The broker unit S4 wrote allows exactly the upstream the broker config records.
    broker_unit_installed(recorded)?;
    let written: Config = read_root_config(CONFIG_PATH).map_err(|_| refused("對外連線設定讀回失敗。"))?;
    check_egress(&written, &policy, dns.route, &dns.host_public_addresses)
}

// ── S6 StartServices ──────────────────────────────────────────────────────

/// How often S6 asks again while it waits for a unit or an object.
const START_POLL: Duration = Duration::from_millis(250);

fn wait_for(deadline: Duration, ready: impl FnMut() -> bool) -> bool {
    wait_until(Instant::now() + deadline, START_POLL, ready)
}

/// `ready` asked at once and then every `poll` until it holds (`true`) or
/// `end` has passed (`false`; asked at least once even when `end` already passed).
pub(super) fn wait_until(end: Instant, poll: Duration, mut ready: impl FnMut() -> bool) -> bool {
    loop {
        if ready() { return true; }
        if Instant::now() >= end { return false; }
        std::thread::sleep(poll);
    }
}

fn enable_now(unit: &str, bound: Duration) -> Result<()> {
    enable_now_with(unit, unit_failed_result,
        |unit| Ok(command("systemctl", &["enable", "--now", unit])?.status.success()),
        || wait_for(bound, || readiness::unit_loaded_active(unit).is_ok()))
}

/// S6 `EnableNow` with its effects injected: `failed` is
/// [`unit_failed_result`] (`None` for every unit but the updater), `start`
/// runs `systemctl enable --now` (`Ok(false)`: it exited non-zero), `active`
/// waits for the unit. A unit already `failed` (a resumed install after the
/// updater failed) is refused BEFORE any start attempt, as its message says
/// (the installer never restarts it); `failed` is asked again after a failed
/// start or wait.
pub(super) fn enable_now_with(unit: &str, mut failed: impl FnMut(&str) -> Option<String>,
    start: impl FnOnce(&str) -> Result<bool>, active: impl FnOnce() -> bool) -> Result<()> {
    if let Some(result) = failed(unit) { return Err(updater_failed(unit, &result)); }
    if !start(unit)? {
        if let Some(result) = failed(unit) { return Err(updater_failed(unit, &result)); }
        return Err(refused(format!("無法啟動 {unit}。")));
    }
    if !active() {
        if let Some(result) = failed(unit) { return Err(updater_failed(unit, &result)); }
        return Err(refused(format!("{unit} 未在時限內進入執行狀態，或含有未驗證的覆寫設定。")));
    }
    Ok(())
}

/// The update service's `Result=` when PID 1 reports it `failed` (start-limit
/// hits included); `None` for any other unit, state or unreadable answer.
fn unit_failed_result(unit: &str) -> Option<String> {
    if unit != UPDATER_UNIT { return None; }
    let output = command("systemctl", &["show", unit, "--property=ActiveState,Result"]).ok()?;
    if !output.status.success() { return None; }
    failed_result(std::str::from_utf8(&output.stdout).ok()?).map(str::to_owned)
}

/// Pure: `Result=` of a `systemctl show --property=ActiveState,Result` answer
/// whose `ActiveState` is `failed`.
pub(super) fn failed_result(show: &str) -> Option<&str> {
    let value = |key: &str| show.lines().find_map(|line| line.strip_prefix(key));
    if value("ActiveState=")? != "failed" { return None; }
    let result = value("Result=")?;
    if !state_word(result) { return None; }
    Some(result)
}

/// The update service is in `failed` state: never restarted in a loop here.
pub(super) fn updater_failed(unit: &str, result: &str) -> DuDuClawError {
    let cause = if result == "start-limit-hit" { "：短時間內重新啟動太多次，systemd 已停止再啟動它（start-limit-hit）" } else { "" };
    refused(format!("{unit} 已停在失敗狀態（{result}）{cause}。安裝程式不會一直重新啟動它。請先執行 sudo journalctl -b -u {unit} 查看原因，修正後執行 sudo systemctl reset-failed {unit}，再以同一個 duduclaw-bootstrap install 命令續接。"))
}

/// Review R3 item 4 (pure): `systemctl show <socket> --property=Listen` names a
/// socket path directly inside `directory`.
pub(super) fn listens_in(output: &[u8], directory: &str) -> bool {
    let Ok(text) = std::str::from_utf8(output) else { return false };
    text.lines().filter_map(|line| line.strip_prefix("Listen=")).any(|value| value.split_once(' ')
        .is_some_and(|(path, _)| Path::new(path).is_absolute() && Path::new(path).parent() == Some(Path::new(directory))))
}

/// Review R3 item 4 / round 4 item 2 / round 5 item 5: what the installer
/// observes of an object another component created, read AFTER its identity
/// (the statx facts are bound to the same inode by [`ledger::created_in_window`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct ComponentFacts {
    /// Mount id of the object's parent directory (`None`: unreadable).
    pub(super) parent_mount: Option<u64>,
    /// `None`: the statx facts could not be read.
    pub(super) creation: Option<ledger::Creation>,
    /// Review round 5 item 5: the durable intent made immediately before this
    /// install started the producer found nothing at the path
    /// ([`ledger::Recorder::absent_at_intent`]) — the proof of absence.
    pub(super) absent_at_intent: bool,
    /// Both clocks when this install started the producer (`None`: unreadable).
    pub(super) window: Option<ledger::Clocks>,
    /// Both clocks at this check (`None`: unreadable).
    pub(super) now: Option<ledger::Clocks>,
}

fn component_facts(path: &str, absent_at_intent: bool, window: Option<ledger::Clocks>) -> ComponentFacts {
    let parent_mount = ledger::open_parent(path).ok()
        .and_then(|(directory, _)| ledger::stat_at(directory.as_raw_fd(), c"").ok()).and_then(|stat| stat.mount_id);
    ComponentFacts { parent_mount, creation: ledger::creation(path).ok(), absent_at_intent, window, now: ledger::clocks() }
}

/// Review R3 item 4 (pure): root:root, of `file_type`, without group/other
/// bits when `private`, and on its parent directory's mount (not a mount
/// point of its own).
fn component_shape(identity: &ledger::Identity, facts: &ComponentFacts, file_type: u32, private: bool) -> bool {
    identity.uid == 0 && identity.gid == 0 && identity.mode & libc::S_IFMT == file_type
        && (!private || identity.mode & 0o077 == 0) && facts.parent_mount == Some(identity.mount_id)
}

/// Review round 5 item 5 (pure): absence-then-identity. The path was absent
/// when its intent was made durable immediately before this install started
/// the producer, and the statx facts read now are of the object `identity`
/// names; where the filesystem reports a birth time and the wall clock did
/// not step, it does not predate the window ([`ledger::created_in_window`]).
/// The callers add the shape, owner, mode and producer checks. Residual: a
/// concurrent root actor creating the object at the path between the intent
/// and the producer is outside what the installer can distinguish.
fn created_by_this_install(identity: &ledger::Identity, facts: &ComponentFacts) -> bool {
    facts.absent_at_intent && facts.creation.as_ref()
        .is_some_and(|creation| ledger::created_in_window(identity, creation, facts.window, facts.now))
}

/// Review round 4 item 2 (pure): R1's updater state root — a root-only
/// directory created no earlier than this install's call to `publish`, whose
/// own state file names this generation (`names_generation`).
pub(super) fn state_root_proven(identity: &ledger::Identity, facts: &ComponentFacts, names_generation: bool) -> bool {
    names_generation && component_shape(identity, facts, libc::S_IFDIR, true) && created_by_this_install(identity, facts)
}

/// Review round 4 item 2 (pure): spawnd's runtime directory — root-owned,
/// created no earlier than this install started the socket, and PID 1 reports
/// the ledgered, unmodified socket unit listening inside it (`socket_listens`).
pub(super) fn runtime_directory_proven(identity: &ledger::Identity, facts: &ComponentFacts, socket_listens: bool) -> bool {
    socket_listens && component_shape(identity, facts, libc::S_IFDIR, false) && created_by_this_install(identity, facts)
}

/// Review round 4 item 2 (pure): the root key — a root-owned 0400 regular
/// file with exactly 32 bytes, one link and a recorded digest (so rollback's
/// identity match is exact), created no earlier than the gateway start this
/// install performed, while PID 1 loads this install's gateway unit (`gateway_ours`).
pub(super) fn root_key_proven(identity: &ledger::Identity, facts: &ComponentFacts, gateway_ours: bool) -> bool {
    gateway_ours && component_shape(identity, facts, libc::S_IFREG, true) && identity.mode & 0o7777 == duduclaw_core::root_key::KEY_MODE
        && identity.size == Some(duduclaw_core::root_key::KEY_BYTES) && identity.sha256.is_some()
        && facts.creation.is_some_and(|creation| creation.nlink == 1) && created_by_this_install(identity, facts)
}

/// Review round 6 item 1 (pure): a component object (`path`, one of
/// [`ledger::RESUMABLE_COMPONENTS`]) its producer created in an interrupted
/// run, checked on resume or at rollback by exactly the producer-specific
/// proof of the original confirm — shape, owner, mode, mount, the statx facts
/// of the same inode, the absence the durable intent proves
/// (`facts.absent_at_intent`) and the producer binding `producer` (R1's state
/// file names this generation / PID 1 reports this install's socket unit
/// listening inside it / PID 1 loads this install's gateway unit) — except
/// the time comparison: the crashed run's window is gone, so only that is
/// skipped (`window: None`). Any other path: false.
pub(super) fn resumed_component_proven(path: &str, identity: &ledger::Identity, facts: &ComponentFacts, producer: bool) -> bool {
    let facts = ComponentFacts { window: None, ..*facts };
    if path == duduclaw_updated::linux::STATE_ROOT {
        state_root_proven(identity, &facts, producer)
    } else if path == SPAWND_RUNTIME {
        runtime_directory_proven(identity, &facts, producer)
    } else if path == duduclaw_core::root_key::KEY_PATH {
        root_key_proven(identity, &facts, producer)
    } else {
        false
    }
}

/// R1's state file in the updater state root is valid, names `generation`
/// and has nothing pending.
fn state_names_generation(generation: Sha256Digest) -> bool {
    let saved: std::result::Result<duduclaw_updated::state::State, _> =
        Directory::open(Path::new(duduclaw_updated::linux::STATE_ROOT)).and_then(|root| root.json("state.json"));
    saved.is_ok_and(|state| state.validate().is_ok() && state.pending.is_none() && state.effective_digest() == generation)
}

/// PID 1 loads this install's ledgered, unmodified spawnd socket unit and
/// reports it listening inside spawnd's runtime directory.
fn runtime_owner_listens(ledger: &ledger::Ledger) -> bool {
    live_unit_is_ours(ledger, SPAWND_RUNTIME_OWNER, Instant::now() + QUIESCE_COMMAND_CAP)
        && command("systemctl", &["show", SPAWND_RUNTIME_OWNER, "--property=Listen"]).ok()
            .filter(|output| output.status.success())
            .is_some_and(|output| listens_in(&output.stdout, SPAWND_RUNTIME))
}

/// Review round 6 item 1: the live resume of component `path`
/// ([`ledger::Recorder::resume_component`], [`resumed_component_proven`]).
/// Nothing to resume: `Ok`. Present but not proven: `component_unconfirmed`.
fn resume_component(recorder: &mut LiveRecorder, path: &str, generation: Sha256Digest) -> Result<()> {
    recorder.resume_component(path, |ledger, identity, absent| {
        let producer = if path == duduclaw_updated::linux::STATE_ROOT {
            state_names_generation(generation)
        } else if path == SPAWND_RUNTIME {
            runtime_owner_listens(ledger)
        } else if path == duduclaw_core::root_key::KEY_PATH {
            live_unit_is_ours(ledger, UNIT, Instant::now() + QUIESCE_COMMAND_CAP)
        } else {
            false
        };
        Ok(resumed_component_proven(path, identity, &component_facts(path, absent, None), producer))
    })
}

/// Review round 6 item 1: before a rollback stops or removes anything, each
/// component entry still `intent` without an identity whose object is present
/// gets the same confirmation attempt as an install resume. Confirmed: it has
/// an identity and the removal below removes it by the ordinary rule. Not
/// confirmed (any failure of the attempt included): left `intent`, so the
/// removal keeps and lists it as before.
pub(super) fn confirm_components_for_rollback(ledger: ledger::Ledger) -> Result<ledger::Ledger> {
    let pending = ledger::RESUMABLE_COMPONENTS.iter().any(|(path, _)| ledger.entry(path)
        .is_some_and(|entry| entry.state == ledger::State::Intent && entry.identity.is_none()));
    if !pending { return Ok(ledger); }
    let generation = parse_generation(&ledger.generation).ok_or_else(|| refused(ledger::LEDGER_DAMAGED))?;
    let mut recorder = LiveRecorder::over_live(ledger)?;
    for (path, _) in ledger::RESUMABLE_COMPONENTS {
        let _ = resume_component(&mut recorder, path, generation);
    }
    Ok(recorder.ledger().clone())
}

/// What S6 does to one unit ([`service_starts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StartAction {
    /// `systemctl enable --now`.
    EnableNow,
    /// The consistency gate: `systemctl start --no-block`, never enabled ([`start_gate`]).
    Gate,
    /// `systemctl start` only (the gateway: S8 enables it).
    Start,
    /// Nothing is started: only the wait (the unit was started earlier in the list).
    Await,
}

/// What S6 waits for after a [`StartAction`], with its bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StartWait {
    /// Loaded from its fragment and active ([`readiness::unit_loaded_active`]).
    Active(Duration),
    /// The update service answers with this install's generation ([`updater_reports`]).
    UpdaterGeneration(Duration),
    /// spawnd publishes valid, complete host evidence ([`spawnd_evidence_valid`]).
    SpawndEvidence(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ServiceStart { pub(super) unit: &'static str, pub(super) action: StartAction, pub(super) wait: StartWait }

/// S6 as data (C1–C4): the start order equals the boot order, and nothing
/// waits on a unit that starts later. The sockets and spawnd; the
/// consistency gate ("spawnd is open"); egress; the gateway (started only;
/// S8 enables it); the updater, then its first answer (its recovery reads the
/// live gateway, egress and spawnd units back); then spawnd's full evidence
/// (its start-up reconciliation needs the updater); last the nightly timer.
/// The broker socket only on the broker route.
pub(super) fn service_starts(dns: &Dns) -> Vec<ServiceStart> {
    let mut plan = Vec::new();
    for unit in SERVICE_ORDER {
        if unit == "duduclaw-dns-broker.socket" && dns.route != DnsRoute::DedicatedDnsBroker { continue; }
        plan.push(if unit == UNIT {
            ServiceStart { unit, action: StartAction::Start, wait: StartWait::Active(GATEWAY_START_WAIT) }
        } else {
            ServiceStart { unit, action: StartAction::EnableNow, wait: StartWait::Active(READY_WAIT) }
        });
        if unit == SPAWND_SERVICE {
            plan.extend(GATEWAY_READY_UNITS.iter().copied().map(|gate|
                ServiceStart { unit: gate, action: StartAction::Gate, wait: StartWait::Active(READY_WAIT) }));
        }
        if unit == UPDATER_UNIT {
            plan.push(ServiceStart { unit, action: StartAction::Await, wait: StartWait::UpdaterGeneration(UPDATER_ANSWER_WAIT) });
        }
    }
    plan.push(ServiceStart { unit: SPAWND_SERVICE, action: StartAction::Await, wait: StartWait::SpawndEvidence(SPAWND_EVIDENCE_WAIT) });
    plan.push(ServiceStart { unit: NIGHTLY_TIMER, action: StartAction::EnableNow, wait: StartWait::Active(READY_WAIT) });
    plan
}

/// The units S6 starts, in order (every [`service_starts`] entry that starts
/// a unit, except the nightly timer); the gateway among them.
fn started_units(dns: &Dns) -> Vec<&'static str> {
    service_starts(dns).into_iter()
        .filter(|start| start.action != StartAction::Await && start.unit != NIGHTLY_TIMER)
        .map(|start| start.unit).collect()
}

/// Enabled (or static, for units without `[Install]`); the consistency gate
/// must be exactly `static` (never enabled by anyone).
fn unit_enabled(unit: &str) -> Result<()> {
    let output = command("systemctl", &["is-enabled", unit])?;
    let state = String::from_utf8(output.stdout).map_err(|_| refused(SYSTEMCTL_FAILED))?;
    let accepted = if GATEWAY_READY_UNITS.contains(&unit) { state.trim() == "static" }
        else { matches!(state.trim(), "enabled" | "static") };
    if !output.status.success() || !accepted {
        return Err(refused(format!("{unit} 尚未啟用。")));
    }
    Ok(())
}

/// The consistency gate: started without enabling it (spawnd's `Wants=` has
/// normally queued it already) and without waiting on the job (its start has
/// no timeout), then required "active (exited)" from its fragment within the
/// same bound as every other unit.
fn start_gate(unit: &str, bound: Duration) -> Result<()> {
    let output = command("systemctl", &["start", "--no-block", unit])?;
    if !output.status.success() { return Err(refused(format!("無法啟動 {unit}。"))); }
    if !wait_for(bound, || readiness::unit_loaded_active(unit).is_ok()) {
        return Err(refused(format!("{unit} 未在時限內確認隔離元件一致，或含有未驗證的覆寫設定。")));
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

/// C2/C4: the S6 refusal when spawnd's evidence did not become valid within
/// `bound`: the readiness fields still false in the published report
/// (`checks`; `None`: unreadable), in the readiness check's wording.
pub(super) fn evidence_timeout(bound: Duration, checks: Option<&duduclaw_spawnd::readiness::Readiness>) -> String {
    let seconds = bound.as_secs();
    let Some(checks) = checks else {
        return format!("隔離執行服務未在 {seconds} 秒內發布有效的主機證據，目前的主機證據也無法讀取。");
    };
    let unmet = readiness::unmet_host_checks(checks);
    if unmet.is_empty() {
        format!("隔離執行服務未在 {seconds} 秒內發布有效的主機證據：各項檢查都已通過，但證據已過期、屬於其他開機或資源數量不符。")
    } else {
        format!("隔離執行服務未在 {seconds} 秒內發布有效的主機證據，仍未通過的項目：{}", unmet.join(""))
    }
}

/// S6 step "spawnd's full evidence" ([`StartWait::SpawndEvidence`]).
fn await_spawnd_evidence(gid: u32, bound: Duration) -> Result<()> {
    if wait_for(bound, || spawnd_evidence_valid(gid)) { return Ok(()); }
    let checks = duduclaw_spawnd::host::EvidenceReport::read(gid).ok().map(|report| report.checks);
    Err(refused(evidence_timeout(bound, checks.as_ref())))
}

/// S6 step "the updater's first answer" ([`StartWait::UpdaterGeneration`]).
/// A unit PID 1 reports `failed` (start-limit hits included) refuses at once.
fn await_updater(unit: &str, generation: Sha256Digest, bound: Duration) -> Result<()> {
    let end = Instant::now() + bound;
    loop {
        if updater_reports(generation) { return Ok(()); }
        if let Some(result) = unit_failed_result(unit) { return Err(updater_failed(unit, &result)); }
        if Instant::now() >= end {
            return Err(refused(format!("更新服務未在 {} 秒內回報與安裝相同的世代。", bound.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// S6 step "the gateway" (C1): its first start. Review F4: before it can
/// first start, the names its data directory holds (the license) are
/// recorded; rollback keeps exactly those. The gateway's launcher creates the
/// root key at its first start, so the key's durable intent and window come
/// right before this start and its confirmation right after it, exactly as
/// S8 did before. Never enabled here (S8 enables it).
///
/// The keep list is captured exactly once per install, from the directory
/// as it is before this install's gateway ever started: a resumed step (the
/// gateway may have run and written its own data meanwhile) finds it in the
/// durable ledger and never lists the directory again (`Recorder::contents`).
fn first_gateway_start(context: &mut Context, bound: Duration) -> Result<()> {
    context.recorder.contents(ledger::GATEWAY_DATA_PATH, || {
        let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
        let (data, _) = open_gateway_data(Some((gateway.uid, gateway.gid)))?;
        directory_names(&data, 64)?.into_iter()
            .map(|name| name.into_string().map_err(|_| refused("Gateway 資料目錄含有無法辨識的名稱。")))
            .collect::<Result<Vec<_>>>()
    })?;
    // Review round 6 item 1: a root key the launcher created in an interrupted
    // run before it was confirmed is confirmed now (or the step refuses).
    resume_component(&mut context.recorder, duduclaw_core::root_key::KEY_PATH, context.generation)?;
    context.recorder.intend(&[(duduclaw_core::root_key::KEY_PATH, Kind::File)])?;
    // Review round 4 item 2: the window of the root key opens before the launcher starts.
    let window = ledger::window_start();
    let output = command("systemctl", &["start", UNIT])?;
    if !output.status.success() { return Err(refused(format!("無法啟動 {UNIT}。"))); }
    // Defect 21: one budget for "unit active" and then "key published"
    // (`confirm_root_key_after_start`); the key is confirmed only after both.
    let end = Instant::now() + bound;
    // Review R3 item 4: the gateway's launcher created it, not this
    // installer: recorded only as a root-owned 0400 single-link regular
    // file on the protected root's mount, absent at the durable intent
    // made before this start (review round 5 item 5), while PID 1 loads
    // this install's ledgered, unmodified gateway
    // unit. Review round 4 item 2: otherwise the step fails.
    let key = duduclaw_core::root_key::KEY_PATH;
    confirm_root_key_after_start(&mut context.recorder, bound, end, START_POLL,
        || readiness::unit_loaded_active(UNIT).is_ok(),
        || ledger::present(key).unwrap_or(false),
        |recorded, absent, identity| {
            let owner = live_unit_is_ours(recorded, UNIT, Instant::now() + QUIESCE_COMMAND_CAP);
            root_key_proven(identity, &component_facts(key, absent, window), owner)
        })
}

/// Defect 21: the gateway unit is `Type=simple`, so PID 1 reports it active as
/// soon as the root launcher runs, a moment BEFORE the launcher publishes the
/// root key (atomic rename onto [`duduclaw_core::root_key::KEY_PATH`]: present
/// at that name means complete; its temporary name never counts).
pub(super) fn root_key_not_created(bound: Duration) -> DuDuClawError {
    refused(format!("Gateway 已進入執行狀態，但它的啟動器未在 {} 秒內建立 root 金鑰 {}；這一步未完成，金鑰的建立紀錄維持未確認，未記錄任何物件。請先執行 sudo journalctl -b -u {UNIT} 查看啟動器的紀錄，修正後以同一個 duduclaw-bootstrap install 命令續接，或執行 duduclaw-bootstrap rollback。",
        bound.as_secs(), duduclaw_core::root_key::KEY_PATH))
}

/// S6, after `systemctl start` of the gateway (effects injected): within the
/// one budget `end` (`bound` from the start), wait for the unit to be active
/// (`active`), then for the launcher's key at its final path (`key_present`),
/// then ONE [`ledger::Recorder::confirm_component`] of the key whose proof is
/// `accept(ledger, absent_at_intent, identity)`. The key absent when the
/// budget is spent: [`root_key_not_created`], nothing confirmed or saved (the
/// entry stays `intent`, so a re-run resumes). The unit never active: the key
/// is confirmed as before (a key the launcher made is still recorded for
/// rollback), then the "not active in time" refusal.
pub(super) fn confirm_root_key_after_start<H: ledger::LedgerHost>(recorder: &mut ledger::Recorder<H>,
    bound: Duration, end: Instant, poll: Duration, active: impl FnMut() -> bool, key_present: impl FnMut() -> bool,
    accept: impl FnOnce(&ledger::Ledger, bool, &ledger::Identity) -> bool) -> Result<()> {
    let key = duduclaw_core::root_key::KEY_PATH;
    let started = wait_until(end, poll, active);
    if started && !wait_until(end, poll, key_present) { return Err(root_key_not_created(bound)); }
    let (ledger, absent) = (recorder.ledger().clone(), recorder.absent_at_intent(key));
    let confirmed = recorder.confirm_component(key, |identity| Ok(accept(&ledger, absent, identity)));
    if !started { return Err(refused("Gateway 未在時限內進入執行狀態。")); }
    confirmed
}

/// One [`service_starts`] entry.
fn run_start(context: &mut Context, start: ServiceStart, gid: u32) -> Result<()> {
    match (start.action, start.wait) {
        (StartAction::EnableNow, StartWait::Active(bound)) => enable_now(start.unit, bound),
        (StartAction::Gate, StartWait::Active(bound)) => start_gate(start.unit, bound),
        (StartAction::Start, StartWait::Active(bound)) if start.unit == UNIT => first_gateway_start(context, bound),
        (StartAction::Await, StartWait::UpdaterGeneration(bound)) => await_updater(start.unit, context.generation, bound),
        (StartAction::Await, StartWait::SpawndEvidence(bound)) => await_spawnd_evidence(gid, bound),
        _ => Err(refused(SYSTEMCTL_FAILED)),
    }
}

fn start_services(context: &mut Context, dns: &Dns) -> Result<()> {
    // Review F6: volatile host conditions, right before the services first use them.
    services_preconditions(dns)?;
    // Review F2, check point 3 for the helper accounts: before they first run.
    verify_identities(&service_names())?;
    let output = command("systemctl", &["daemon-reload"])?;
    if !output.status.success() { return Err(refused(SYSTEMCTL_FAILED)); }
    let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    parse_lease::verify(gateway.gid)?;
    // Review round 6 item 1: a runtime directory systemd created in an
    // interrupted run before it was confirmed is confirmed now (or the step refuses).
    resume_component(&mut context.recorder, SPAWND_RUNTIME, context.generation)?;
    // systemd creates spawnd's runtime directory: ledgered before it can exist.
    context.recorder.intend(&[(SPAWND_RUNTIME, Kind::Tree)])?;
    // Review round 4 item 2: the window of the runtime directory opens before the socket starts.
    let window = ledger::window_start();
    // A resumed S6 repeats every entry: a unit already started stays as it
    // is and only its wait is repeated.
    let (plan, timer): (Vec<ServiceStart>, Vec<ServiceStart>) =
        service_starts(dns).into_iter().partition(|start| start.unit != NIGHTLY_TIMER);
    let started = (|| -> Result<()> {
        for start in plan { run_start(context, start, gateway.gid)?; }
        Ok(())
    })();
    // Review R3 item 4: systemd created it, not this installer. Recorded (also
    // when a later unit failed) only when it is root-owned, on its parent's
    // mount, absent at the durable intent made right before the socket
    // started (review round 5 item 5), and PID
    // 1 reports this install's ledgered, unmodified socket unit as listening
    // in it. Review round 4 item 2: otherwise the step fails (never adopted).
    let owner = runtime_owner_listens(context.recorder.ledger());
    let absent = context.recorder.absent_at_intent(SPAWND_RUNTIME);
    let confirmed = context.recorder.confirm_component(SPAWND_RUNTIME,
        |identity| Ok(runtime_directory_proven(identity, &component_facts(SPAWND_RUNTIME, absent, window), owner)));
    started?;
    confirmed?;
    // Q11: the nightly update requester is enabled after install.
    for start in timer { run_start(context, start, gateway.gid)?; }
    Ok(())
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

/// The broker unit file holding exactly the text generated for `upstream`
/// (`None`: the resolved route, the shipped text), root 0644. A byte
/// mismatch names the expected and the found allow rule, never the file.
fn broker_unit_installed(upstream: Option<SocketAddr>) -> Result<()> {
    let path = unit_file(layout::DNS_BROKER_UNIT);
    if let Some(installed) = read_root_file(&path, 256 * 1024)? {
        layout::dns_broker_unit_check(&installed, upstream).map_err(refused)?;
    }
    let expected = layout::dns_broker_unit_text(layout::DNS_BROKER_UNIT_TEXT, upstream).map_err(refused)?;
    text_installed(&path, 0o644, &expected)
}

/// S4's invariants: trust state and `current` bound to THIS generation, every
/// program equal to its signed artifact (digest, owner, mode), the launcher
/// receipt, launcher, version intent, accounts, gateway home and license,
/// directories, unit files, the parse runtime tree, spawnd's publication, the
/// nightly files, and no unconfirmed ledger intent left behind.
fn platform_published(context: &Context, dns: &Dns) -> Result<()> {
    let mismatch = || refused(PUBLISHED_MISMATCH);
    let upstream = broker_upstream(dns)?;
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
    for (name, text) in layout::UNITS {
        // The broker unit: the text S4 generated for the journal's upstream.
        if *name == layout::DNS_BROKER_UNIT { broker_unit_installed(upstream)?; }
        else { text_installed(&unit_file(name), 0o644, text)?; }
    }
    text_installed(parse_lease::CONFIG, 0o644, parse_lease::TEXT)?;
    parse_lease::verify(gateway.gid)?;
    parse_runtime_tree(false)?;
    let spawnd = Directory::open(Path::new(SPAWND_ROOT)).map_err(|_| mismatch())?;
    for name in SPAWND_ANCHORS {
        spawnd.child(name).map_err(|_| mismatch())?;
        let metadata = fs::symlink_metadata(Path::new(SPAWND_ROOT).join(name))?;
        if metadata.mode() & 0o7777 != 0o700 { return Err(mismatch()); }
    }
    // Review H1 / R8d C: this generation, and no entitlement or only ones
    // spawnd's production check admits (a resumed install after employees exist).
    let admitted = |entitlements: &[serde_json::Value]| entitlements_admitted(context.generation, &context.generation_hex, entitlements);
    if !publication_holds(&spawnd_root_fd()?, (0, 0), context.generation, &admitted) { return Err(mismatch()); }
    text_installed(NIGHTLY_HELPER, 0o755, NIGHTLY_HELPER_TEXT)?;
    for (name, text) in NIGHTLY_UNITS { text_installed(&unit_file(name), 0o644, text)?; }
    // C3: the gateway unit files are S4's now (unexpected bytes do not hold).
    gateway_files()?;
    // A creation whose identity was never recorded (interrupted) does not
    // hold; the re-run's intent then refuses it by path (review H3: never adopted).
    // The objects S6's producers create (spawnd's runtime directory, the
    // gateway's root key) are confirmed by S6 itself (`resume_component`).
    for entry in &context.recorder.ledger().entries {
        if entry.state == ledger::State::Intent && ledger::present(&entry.path)?
            && entry.path != duduclaw_core::root_key::KEY_PATH && entry.path != SPAWND_RUNTIME {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// S6's invariants: every started unit loaded from its fragment and active;
/// enabled (the consistency gate: "active (exited)" and `static`) except the
/// gateway, which S8 enables; the helper identities unaliased; spawnd's
/// evidence valid; the update service reports THIS generation.
fn services_running(context: &Context, dns: &Dns) -> Result<()> {
    for unit in started_units(dns).into_iter().chain(std::iter::once(NIGHTLY_TIMER)) {
        readiness::unit_loaded_active(unit)?;
        // S6 only starts the gateway; S8 enables it.
        if unit != UNIT { unit_enabled(unit)?; }
    }
    verify_identities(&service_names())?;
    let gateway = passwd_entry("duduclaw")?.ok_or_else(|| refused("無法查詢服務帳號。"))?;
    if !spawnd_evidence_valid(gateway.gid) || !updater_reports(context.generation) {
        return Err(refused(PUBLISHED_MISMATCH));
    }
    Ok(())
}

/// The gateway unit, its recovery bound and launcher exactly as S4 publishes them.
fn gateway_files() -> Result<()> {
    let text = duduclaw_core::root_key::recovery::isolated_unit_text();
    text_installed(UNIT_PATH, 0o644, &text)?;
    text_installed(RECOVERY_BOUND, 0o644, &text)?;
    text_installed(LAUNCHER, 0o755, LAUNCHER_TEXT)
}

/// S2/S3 (review H2): a running gateway is this transaction's — its unit
/// files exactly as S4 published them, and PID 1 loads that ledgered,
/// unmodified unit.
fn gateway_ours(context: &Context) -> Result<()> {
    gateway_files()?;
    if !live_unit_is_ours(context.recorder.ledger(), UNIT, Instant::now() + QUIESCE_COMMAND_CAP) {
        return Err(refused(PUBLISHED_MISMATCH));
    }
    Ok(())
}

/// S8's invariants: the gateway unit files exactly as published, the unit
/// enabled and active, and its root key confirmed (not left an intent).
fn gateway_published(context: &Context) -> Result<()> {
    gateway_files()?;
    unit_enabled(UNIT)?;
    readiness::unit_loaded_active(UNIT)?;
    let key = context.recorder.ledger().entry(duduclaw_core::root_key::KEY_PATH);
    if !key.is_some_and(|entry| entry.state == ledger::State::Created) { return Err(refused(PUBLISHED_MISMATCH)); }
    Ok(())
}

// ── S8 StartGateway / S9 Commit ───────────────────────────────────────────

/// Plan S8. The unit files are S4's and the first start is S6's (C1); what
/// stays here: the root key's ledger entry confirmed (a key S6's interrupted
/// run left unconfirmed is confirmed now, or the step refuses), the unit
/// enabled, the gateway confirmed active, and the full readiness check.
fn start_gateway(context: &mut Context) -> Result<()> {
    resume_component(&mut context.recorder, duduclaw_core::root_key::KEY_PATH, context.generation)?;
    let key = context.recorder.ledger().entry(duduclaw_core::root_key::KEY_PATH);
    if !key.is_some_and(|entry| entry.state == ledger::State::Created) {
        return Err(refused(format!("Gateway 的 root 金鑰 {} 尚未確認是這次安裝建立的。", duduclaw_core::root_key::KEY_PATH)));
    }
    let output = command("systemctl", &["enable", UNIT])?;
    if !output.status.success() { return Err(refused(format!("無法啟用 {UNIT}。"))); }
    readiness::unit_loaded_active(UNIT).map_err(|_| refused("Gateway 沒有在執行。"))?;
    // The install reports success only when the full platform check passes.
    // R8m 30b: the platform result only — the gateway's isolation tool
    // service may still be assembling in the background.
    readiness::check_platform_all(&mut readiness::InstalledComposite)
}

/// Plan S9. Review F1: never on the journal's word — S8's invariants, the
/// service identities and the full readiness check must pass in this same
/// run (the gateway runs: S6 started it), then the floor is recorded.
fn commit(context: &mut Context) -> Result<()> {
    gateway_published(context)?;
    verify_identities(&service_names())?;
    readiness::check_platform_all(&mut readiness::InstalledComposite)?;
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
    fn nonce() -> String { "5a".repeat(32) }
    fn journal() -> Journal { Journal::new(&"ab".repeat(32), &host(), &nonce(), broker_dns(), 100) }

    #[test]
    fn journal_rejects_unknown_fields_and_step_regression() {
        let start = journal();
        let bytes = serde_json::to_vec(&start).unwrap();
        assert_eq!(Journal::parse(&bytes).unwrap(), start);
        // R1's minimal reader sees the four fields it needs (schema 2, review M4).
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["nonce"], nonce());
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
            mutate(|v| v["dns"]["extra"] = 1.into()), mutate(|v| v["schema_version"] = 3.into()),
            // An older journal (host bound to the machine id alone, no nonce) is refused.
            mutate(|v| v["schema_version"] = 1.into()),
            mutate(|v| { v["schema_version"] = 1.into(); v.as_object_mut().unwrap().remove("nonce"); }),
            mutate(|v| v["nonce"] = "5A".repeat(32).into()), mutate(|v| v["nonce"] = "5a".into()),
            mutate(|v| { v.as_object_mut().unwrap().remove("nonce"); }),
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
        let mut edited = completed.at(Step::ProcessesClear, Phase::Entered);
        edited.nonce = "6b".repeat(32);
        assert!(completed.check_transition(&edited).is_err(), "nonce is immutable");
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
        let broker = upstream_valid;
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
        // The broker's validator accepts these; the unit's allow line cannot carry them.
        for bad in ["127.0.0.1:53", "[::1]:53", "[::ffff:192.0.2.53]:53", "[fe80::1]:53"] {
            let address: SocketAddr = bad.parse().unwrap();
            assert!(duduclaw_egress::daemon::BrokerConfig { upstream: address }.validate().is_ok(), "{bad}");
            assert!(dns_route(false, Some(address), broker).is_err(), "{bad}");
        }
        // IPv4 link-local (cloud metadata/resolver hosts): refused with the
        // systemd-resolved advice, even by a validator that would accept it.
        for bad in ["169.254.169.254:53", "169.254.169.253:53"] {
            let address: SocketAddr = bad.parse().unwrap();
            assert!(!upstream_valid(address), "{bad}");
            assert_eq!(dns_route(false, Some(address), broker), Err(layout::LINK_LOCAL_UPSTREAM_REFUSED), "{bad}");
            assert_eq!(dns_route(false, Some(address), |_| true), Err(layout::LINK_LOCAL_UPSTREAM_REFUSED), "{bad}");
            assert!(layout::dns_broker_unit_text(layout::DNS_BROKER_UNIT_TEXT, Some(address)).is_err(), "{bad}");
        }
        // Whatever S1 accepts, the broker accepts and the broker unit can be generated for.
        for good in ["192.0.2.53:53", "10.0.0.2:53", "[2001:4860:4860::8888]:53"] {
            let address: SocketAddr = good.parse().unwrap();
            assert!(upstream_valid(address), "{good}");
            assert!(duduclaw_egress::daemon::BrokerConfig { upstream: address }.validate().is_ok(), "{good}");
            assert!(layout::dns_broker_unit_text(layout::DNS_BROKER_UNIT_TEXT, Some(address)).is_ok(), "{good}");
        }
        // The broker unit's upstream follows the route; any other pairing is damage.
        assert_eq!(broker_upstream(&broker_dns()).unwrap(), Some(upstream));
        let resolved = Dns { route: DnsRoute::ResolvedVarlink, upstream: None, host_public_addresses: Vec::new() };
        assert_eq!(broker_upstream(&resolved).unwrap(), None);
        assert!(broker_upstream(&Dns { upstream: Some(upstream), ..resolved.clone() }).is_err());
        assert!(broker_upstream(&Dns { upstream: None, ..broker_dns() }).is_err());
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
    fn preflight_storage_tools_refuse_missing_mkfs_and_invalid_loop_control() {
        assert_eq!(storage_tools_clear(false, Some((true, 10, 237))), Err(MKFS_REQUIRED));
        assert!(MKFS_REQUIRED.contains("mkfs.ext4") && MKFS_REQUIRED.contains("e2fsprogs"));
        for device in [None, Some((false, 10, 237)), Some((true, 7, 237)), Some((true, 10, 236))] {
            assert_eq!(storage_tools_clear(true, device), Err(LOOP_REQUIRED));
        }
        assert!(LOOP_REQUIRED.contains("modprobe loop"));
        assert_eq!(storage_tools_clear(true, Some((true, 10, 237))), Ok(()));
    }

    #[test]
    fn preflight_storage_reserve_accepts_only_at_or_above_shared_boundary() {
        const G: u64 = 1024 * 1024 * 1024;
        for (total, reserve) in [(100 * G, 20 * G), (300 * G + 1, 30 * G + 1)] {
            assert_eq!(storage_reserve_clear(Some((total, reserve - 1))), Err(RESERVE_REQUIRED));
            assert_eq!(storage_reserve_clear(Some((total, reserve))), Ok(()));
            assert_eq!(storage_reserve_clear(Some((total, reserve + 1))), Ok(()));
        }
        for capacity in [None, Some((0, 0)), Some((10 * G, 10 * G)), Some((100 * G, 101 * G))] {
            assert_eq!(storage_reserve_clear(capacity), Err(RESERVE_REQUIRED));
        }
    }

    #[test]
    fn spawnd_anchor_list_includes_storage_in_install_order() {
        assert_eq!(SPAWND_ANCHORS, ["etc-state", "projections", "snapshots", "images", "storage"]);
        // Both are below the already-ledgered tree; no new host installation fact.
        assert!(duduclaw_core::isolation_install::FACTS.contains(&SPAWND_ROOT));
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
        assert_eq!(nss_verdict("passwd: files sss\ngroup: files\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("group:  files ldap # remote\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("passwd: files systemd\n# passwd: ldap\nhosts: files dns ldap\n"), Ok(()));
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

    #[test]
    fn nsswitch_sources_are_an_allowlist() {
        // Ubuntu / Debian defaults.
        assert_eq!(nss_verdict("passwd:         files systemd\ngroup:          files systemd\nshadow:         files\n\
gshadow:        files\nhosts:          files mdns4_minimal [NOTFOUND=return] dns\nnetgroup:       nis\n"), Ok(()));
        // compat reads passwd_compat / group_compat / shadow_compat (e.g. NIS).
        // Review R3 item 3: absent means NIS to glibc — refused; exactly one `files` line is required.
        assert_eq!(nss_verdict("passwd: compat\n"), Err(NssProblem::CompatUnbounded("passwd_compat".into())));
        assert_eq!(nss_verdict("group: files\ninitgroups: compat\n"), Err(NssProblem::CompatUnbounded("group_compat".into())));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat: files\npasswd_compat: files\n"),
            Err(NssProblem::Duplicate("passwd_compat".into())));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat: files\n# passwd_compat: nis\n"), Ok(()));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat: files\n"), Ok(()));
        assert_eq!(nss_verdict("passwd: compat\ngroup: compat\nshadow: compat\npasswd_compat: files\ngroup_compat: files\nshadow_compat: files\n"), Ok(()));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat: nis\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat: files nis\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("group: files\ninitgroups: compat\ngroup_compat: nis\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("shadow: compat\nshadow_compat: sss\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat: extrausers\n"), Err(NssProblem::Unknown("extrausers".into())));
        assert_eq!(nss_verdict("passwd: compat\npasswd_compat:\n"), Err(NssProblem::CompatUnbounded("passwd_compat".into())));
        // Review R3 item 3: an account database given twice refuses (which line wins is not ours to guess).
        assert_eq!(nss_verdict("passwd: files\npasswd: files ldap\n"), Err(NssProblem::Duplicate("passwd".into())));
        assert_eq!(nss_verdict("passwd: files\npasswd:   files\n"), Err(NssProblem::Duplicate("passwd".into())));
        for database in ["group", "shadow", "initgroups"] {
            assert_eq!(nss_verdict(&format!("{database}: files\n{database}: files\n")), Err(NssProblem::Duplicate(database.into())));
        }
        assert_eq!(nss_verdict("hosts: files\nhosts: dns\n"), Ok(()), "not an account database");
        // A *_compat line of a database not using compat changes nothing.
        assert_eq!(nss_verdict("passwd: files\npasswd_compat: nis\n"), Ok(()));
        // Any source the installer does not classify refuses by name (never treated as local).
        assert_eq!(nss_verdict("passwd: files extrausers\n"), Err(NssProblem::Unknown("extrausers".into())));
        assert_eq!(nss_verdict("group: files mymachines\n"), Err(NssProblem::Unknown("mymachines".into())));
        assert_eq!(nss_verdict("initgroups: files oslogin\n"), Err(NssProblem::Unknown("oslogin".into())));
        assert_eq!(nss_verdict("shadow: files cache\n"), Err(NssProblem::Unknown("cache".into())));
        // Bracketed action items are parsed, not sources — with or without inner spaces or separators.
        assert_eq!(nss_verdict("passwd: files [NOTFOUND=return] systemd\n"), Ok(()));
        assert_eq!(nss_verdict("passwd: files[!UNAVAIL=return]systemd\n"), Ok(()));
        assert_eq!(nss_verdict("passwd: files [ NOTFOUND=return SUCCESS=return ] systemd\n"), Ok(()));
        assert_eq!(nss_verdict("passwd: files [NOTFOUND=return] ldap\n"), Err(NssProblem::External));
        assert_eq!(nss_verdict("passwd: files [NOTFOUND=return\n"), Err(NssProblem::Malformed));
        // Other databases are not account sources here.
        assert_eq!(nss_verdict("hosts: files resolve [!UNAVAIL=return] dns myhostname\n"), Ok(()));
    }

    /// Review H2 / R3 items 1, 2, 6 / round 4 items 1, 3, 4: PID 1 scripted
    /// on a fake clock — loaded units, template instances, transient runs and
    /// employee slices, queued jobs, cgroup population. Every observation and
    /// effect is appended to the shared log in order.
    struct FakeUnits {
        /// Units that are up (listed `loaded active running`).
        up: Vec<String>,
        /// Units loaded but down (listed `loaded inactive dead`).
        loaded: Vec<String>,
        /// Raw extra `list-units` lines (an unknown `duduclaw-` unit …).
        extra: Vec<String>,
        /// Queued jobs: the unit and for how many more observations it stays queued (`u32::MAX`: never clears).
        jobs: Vec<(String, u32)>,
        /// Units that come up at the n-th `list-units` observation (1-based).
        appear: Vec<(u32, String)>,
        /// Units that are not provably this install's (`definition` is `None`).
        foreign: Vec<String>,
        /// Units a stop (or kill) does not bring down.
        stuck: Vec<String>,
        /// Units whose loaded definition differs once stopped (replaced and reloaded meanwhile).
        redefined: Vec<String>,
        /// For how many more reads a product cgroup stays populated (`u32::MAX`: always).
        populated: u32,
        /// A process of the gateway account remains (gone when the gateway stops).
        gateway_processes: bool,
        /// The full S3 check finds an isolation-account or root-from-install-path process.
        leftover: bool,
        /// Review round 5 item 3: units a stop brings down that PID 1 starts
        /// again before the next listing, and how many more times (`u32::MAX`: always).
        restarts: Vec<(String, u32)>,
        /// Review round 5 item 2: units given a start job (seen by one
        /// `list-jobs`) by the first full process scan.
        scan_queues: Vec<String>,
        scans: u32,
        /// Review round 5 item 4: the current invocation of every transient
        /// unit (its InvocationID is this number in 32 lowercase hex) …
        invocation: u32,
        /// … and the `definition` calls (1-based, counted over all units) before
        /// whose answer a transient unit is re-invoked (a new InvocationID).
        reinvoke_at: Vec<u32>,
        shows: u32,
        stopped: Vec<String>,
        listings: u32,
        start: Instant,
        clock: Instant,
        /// What every command or read costs on the fake clock (bounded by the deadline, as `command_within` is).
        cost: Duration,
        /// Commands or reads started at or after their deadline (must stay 0).
        late: u32,
        commands: u32,
        log: Vec<String>,
    }

    impl Default for FakeUnits {
        fn default() -> Self {
            let now = Instant::now();
            Self { up: Vec::new(), loaded: Vec::new(), extra: Vec::new(), jobs: Vec::new(), appear: Vec::new(),
                foreign: Vec::new(), stuck: Vec::new(), redefined: Vec::new(), populated: 0, gateway_processes: false,
                leftover: false, restarts: Vec::new(), scan_queues: Vec::new(), scans: 0, invocation: 1,
                reinvoke_at: Vec::new(), shows: 0, stopped: Vec::new(), listings: 0, start: now, clock: now,
                cost: Duration::from_millis(5), late: 0, commands: 0, log: Vec::new() }
        }
    }

    fn names(units: &[&str]) -> Vec<String> { units.iter().map(|unit| (*unit).to_owned()).collect() }

    const RUN: &str = "duduclaw-run-0123456789abcdef0123456789abcdef.service";
    const EMPLOYEE_SLICE: &str = "duduclaw-employees-fedcba9876543210fedcba9876543210.slice";
    const PARSE_INSTANCE: &str = "duduclaw-parse@3-1234-0.service";

    impl FakeUnits {
        fn running() -> Self {
            Self { up: names(&[UNIT, NIGHTLY_TIMER, "duduclaw-egress.socket", "duduclaw-spawnd.socket", "duduclaw-spawnd.service",
                duduclaw_isolation::ids::ISOLATION_READY_UNIT, "duduclaw-egress.service", "duduclaw-updated.service"]),
                gateway_processes: true, ..Self::default() }
        }
        /// Everything of the installation up: fixed units, an instance, a run and its employee slice, the slices.
        fn everything() -> Self {
            let mut units = Self::running();
            units.up.extend(names(&[PARSE_INSTANCE, RUN, EMPLOYEE_SLICE, "duduclaw-employees.slice", "duduclaw-tcb.slice",
                "duduclaw.slice", "duduclaw-migrate-expire@0a1b.timer"]));
            units
        }
        fn command(&mut self, deadline: Instant) {
            self.commands += 1;
            match command_budget(self.clock, deadline) {
                Some(limit) => self.clock += self.cost.min(limit),
                None => self.late += 1,
            }
        }
    }

    impl UnitControl for FakeUnits {
        fn now(&mut self) -> Instant { self.clock }
        fn list_units(&mut self, deadline: Instant) -> Result<Vec<u8>> {
            self.command(deadline);
            self.listings += 1;
            let listings = self.listings;
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.appear).into_iter().partition(|(at, _)| *at <= listings);
            self.appear = later;
            self.up.extend(due.into_iter().map(|(_, unit)| unit));
            // Started again by PID 1 since its last stop.
            for (unit, left) in self.restarts.iter_mut() {
                if *left == 0 || self.up.contains(unit) || !self.stopped.contains(unit) { continue; }
                if *left != u32::MAX { *left -= 1; }
                self.up.push(unit.clone());
            }
            self.log.push("list-units".into());
            let mut text = String::new();
            for unit in self.up.clone() {
                self.log.push(format!("up {unit}"));
                text.push_str(&format!("{unit} loaded active running Fake unit\n"));
            }
            for unit in self.loaded.iter().filter(|unit| !self.up.contains(*unit)) {
                text.push_str(&format!("{unit} loaded inactive dead Fake unit\n"));
            }
            for line in &self.extra { text.push_str(line); text.push('\n'); }
            Ok(text.into_bytes())
        }
        fn list_jobs(&mut self, deadline: Instant) -> Result<Vec<u8>> {
            self.command(deadline);
            self.log.push("list-jobs".into());
            let mut text = String::new();
            for (index, (unit, left)) in self.jobs.iter_mut().enumerate() {
                if *left == 0 { continue; }
                if *left != u32::MAX { *left -= 1; }
                self.log.push(format!("job {unit}"));
                text.push_str(&format!("{} {unit} start waiting\n", 100 + index));
            }
            Ok(text.into_bytes())
        }
        fn definition(&mut self, unit: &str, shape: UnitShape, deadline: Instant) -> Option<String> {
            self.command(deadline);
            self.shows += 1;
            if self.reinvoke_at.contains(&self.shows) { self.invocation += 1; }
            self.log.push(format!("show {unit}"));
            if self.foreign.iter().any(|foreign| foreign == unit) { return None; }
            let changed = self.redefined.iter().any(|name| name == unit) && self.stopped.iter().any(|name| name == unit);
            let definition = format!("{unit} v{}", if changed { 2 } else { 1 });
            if !shape.transient() { return Some(definition); }
            self.log.push(format!("invocation {unit} {}", self.invocation));
            Some(format!("{definition}\nInvocationID={:032x}", self.invocation))
        }
        fn stop(&mut self, unit: &str, deadline: Instant) -> Result<()> {
            self.command(deadline);
            self.log.push(format!("stop {unit}"));
            self.stopped.push(unit.to_owned());
            if !self.stuck.iter().any(|stuck| stuck == unit) {
                self.up.retain(|up| up != unit);
                // Transient units are gone once stopped; others stay loaded, down.
                if !unit_shape(unit).is_some_and(UnitShape::transient) && !self.loaded.iter().any(|name| name == unit) {
                    self.loaded.push(unit.to_owned());
                }
                if unit == UNIT { self.gateway_processes = false; }
            }
            Ok(())
        }
        fn kill(&mut self, unit: &str, deadline: Instant) -> Result<()> {
            self.command(deadline);
            self.log.push(format!("kill {unit}"));
            if !self.stuck.iter().any(|stuck| stuck == unit) { self.up.retain(|up| up != unit); }
            Ok(())
        }
        fn populated_cgroup(&mut self, deadline: Instant) -> Result<Option<&'static str>> {
            self.command(deadline);
            self.log.push("cgroup".into());
            if self.populated == 0 { return Ok(None); }
            if self.populated != u32::MAX { self.populated -= 1; }
            Ok(Some(PRODUCT_CGROUPS[0]))
        }
        fn gateway_account_busy(&mut self, deadline: Instant) -> Result<bool> {
            self.command(deadline);
            self.log.push("gateway".into());
            Ok(self.gateway_processes)
        }
        fn processes_clear(&mut self, deadline: Instant) -> Result<()> {
            // The scan's account lookups are commands bounded by the same deadline.
            self.command(deadline);
            self.log.push("processes_clear".into());
            self.scans += 1;
            if self.scans == 1 {
                for unit in std::mem::take(&mut self.scan_queues) { self.jobs.push((unit, 1)); }
            }
            if self.leftover { Err(refused("仍有隔離元件正在執行")) } else { Ok(()) }
        }
        fn pause(&mut self, pause: Duration) { self.clock += pause; }
        fn inform(&mut self, line: &str) { self.log.push(format!("note {line}")); }
    }

    /// Stops, kills, the process scan and performed steps, in order (observations dropped).
    fn effects(log: &[String]) -> Vec<String> {
        log.iter().filter(|line| line.starts_with("stop ") || line.starts_with("kill ") || line.starts_with("perform ")
            || *line == "processes_clear").cloned().collect()
    }

    /// A host whose step invariants are scripted (review F1).
    #[derive(Default)]
    struct ScriptedHost { broken: Vec<Step>, refuse: Vec<Step>, performed: Vec<Step>, units: FakeUnits }

    impl Steps for ScriptedHost {
        fn holds(&mut self, step: Step) -> Result<bool> { Ok(!self.broken.contains(&step)) }
        fn perform(&mut self, step: Step) -> Result<()> {
            if self.refuse.contains(&step) { return Err(refused("refused")); }
            self.broken.retain(|broken| *broken != step);
            self.performed.push(step);
            self.units.log.push(format!("perform {step:?}"));
            Ok(())
        }
        fn quiesce(&mut self) -> Result<()> { quiesce_owned(&mut self.units) }
    }

    /// The stop effects of `units` (those up), in the installation's stop order.
    fn stops(up: &[String]) -> Vec<String> {
        super::super::rollback::stop_order().into_iter().filter(|unit| up.iter().any(|up| up == unit))
            .map(|unit| format!("stop {unit}")).collect()
    }

    #[test]
    fn resume_stops_every_owned_unit_before_rerunning_platform_steps() {
        let forged = journal().at(Step::StartGateway, Phase::Completed);
        // S8's gateway and S6's services run as published (S2/S3 hold through them); S5 no longer holds.
        let mut host = ScriptedHost { broken: vec![Step::DnsPolicy], units: FakeUnits::running(), ..Default::default() };
        let up = host.units.up.clone();
        let mut position = forged.clone();
        let (result, writes) = drive_scripted(&mut position, &mut host);
        result.unwrap();
        // Every owned unit is stopped FIRST (gateway first, sockets before
        // services), then the FULL process check, then S5..S8 re-run, then the commit.
        let mut expected = stops(&up);
        assert_eq!(expected.first().map(String::as_str), Some("stop duduclaw-gateway.service"));
        assert_eq!(expected.len(), up.len());
        expected.extend(["processes_clear", "perform DnsPolicy", "perform StartServices", "perform Readiness",
            "perform StartGateway", "perform Commit"].map(String::from));
        assert_eq!(effects(&host.units.log), expected);
        assert_eq!(writes, vec![(Step::Commit, Phase::Entered), (Step::Commit, Phase::Completed)]);
        assert_eq!(host.units.late, 0);
        // A re-run that starts at S2 or S3 but reaches S4–S7 quiesces too.
        let mut host = ScriptedHost { broken: vec![Step::ProcessesClear], units: FakeUnits::running(), ..Default::default() };
        let mut position = forged.clone();
        drive_scripted(&mut position, &mut host).0.unwrap();
        let done = effects(&host.units.log);
        assert_eq!(done.iter().position(|line| line == "processes_clear"), Some(up.len()));
        assert_eq!(done.get(up.len() + 1).map(String::as_str), Some("perform ProcessesClear"));
        // Units already down are not stopped (nothing to prove about their files).
        let mut host = ScriptedHost { broken: vec![Step::StartServices],
            units: FakeUnits { foreign: names(&stop_order_all()), loaded: names(&stop_order_all()), ..Default::default() },
            ..Default::default() };
        let mut position = forged;
        drive_scripted(&mut position, &mut host).0.unwrap();
        assert_eq!(effects(&host.units.log), ["processes_clear", "perform StartServices", "perform Readiness",
            "perform StartGateway", "perform Commit"]);
        assert!(!host.units.log.iter().any(|line| line.starts_with("show ")), "nothing down is checked or stopped");
    }

    fn stop_order_all() -> Vec<&'static str> { super::super::rollback::stop_order() }

    #[test]
    fn resume_refuses_unless_every_owned_unit_is_down_and_no_process_remains() {
        let forged = journal().at(Step::StartGateway, Phase::Completed);
        let refused_with = |units: FakeUnits, broken: Step| {
            let mut host = ScriptedHost { broken: vec![broken], units, ..Default::default() };
            let mut position = forged.clone();
            let (result, writes) = drive_scripted(&mut position, &mut host);
            assert!(host.performed.is_empty() && writes.is_empty() && position == forged && !position.committed);
            assert_eq!(host.units.late, 0, "nothing spawned after the deadline");
            (result.unwrap_err().to_string(), host.units)
        };
        // A unit that is up but not provably ours (the gateway or an isolation
        // service): refused by name before any effect.
        for unit in [UNIT, "duduclaw-egress.service"] {
            let (error, units) = refused_with(FakeUnits { foreign: names(&[unit]), ..FakeUnits::running() }, Step::PublishPlatform);
            assert!(error.contains(unit) && error.contains("手動停止"), "{error}");
            assert!(effects(&units.log).is_empty(), "{:?}", units.log);
        }
        // An owned unit that stays active: refused by name after the one deadline; no process check, nothing re-run.
        let (error, units) = refused_with(FakeUnits { stuck: names(&["duduclaw-spawnd.service"]), ..FakeUnits::running() },
            Step::Readiness);
        assert!(error.contains("duduclaw-spawnd.service"), "{error}");
        assert!(!units.log.iter().any(|line| line == "processes_clear"));
        assert!(units.clock.duration_since(units.start) <= QUIESCE_WAIT);
        // The gateway account keeps a process: refused naming the account.
        let (error, _) = refused_with(FakeUnits { gateway_processes: true, ..Default::default() }, Step::DnsPolicy);
        assert!(error.contains("duduclaw（Gateway）帳號"), "{error}");
        // An isolation-account or root-from-install-path process remains until the deadline: the full check refuses.
        let (error, units) = refused_with(FakeUnits { leftover: true, ..FakeUnits::running() }, Step::StartServices);
        assert!(error.contains("服務帳號") && error.contains("隔離安裝路徑"), "{error}");
        assert_eq!(effects(&units.log).last().map(String::as_str), Some("processes_clear"));
        // A product cgroup that stays populated: refused naming it, never reaching the process scan.
        let (error, units) = refused_with(FakeUnits { populated: u32::MAX, ..FakeUnits::running() }, Step::DnsPolicy);
        assert!(error.contains("/sys/fs/cgroup/duduclaw.slice"), "{error}");
        assert!(!units.log.iter().any(|line| line == "processes_clear"));
    }

    #[test]
    fn pure_s8_resume_leaves_isolation_services_running() {
        let forged = journal().at(Step::StartGateway, Phase::Completed);
        let mut host = ScriptedHost { broken: vec![Step::StartGateway], units: FakeUnits::running(), ..Default::default() };
        let mut position = forged;
        drive_scripted(&mut position, &mut host).0.unwrap();
        assert_eq!(host.units.log, ["perform StartGateway", "perform Commit"]);
        assert_eq!(host.units.commands, 0);
    }

    #[test]
    fn quiesce_deadline_is_one_strict_instant() {
        // Each command takes long on the fake clock: the whole quiesce still
        // ends by the one deadline and nothing is spawned after it.
        let mut slow = FakeUnits { cost: Duration::from_secs(7), ..FakeUnits::running() };
        assert!(quiesce_isolation(&mut slow, QUIESCE_WAIT).is_err());
        assert_eq!(slow.late, 0);
        assert!(slow.clock.duration_since(slow.start) <= QUIESCE_WAIT);
        assert!(slow.commands <= 5, "{}", slow.commands);
        // A transient run that never goes down (stopped, then killed once):
        // polled until exactly the deadline, then refused by name.
        let mut stuck = FakeUnits { up: names(&[RUN]), stuck: names(&[RUN]), cost: Duration::ZERO, ..Default::default() };
        let error = quiesce_isolation(&mut stuck, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains(RUN), "{error}");
        assert_eq!(stuck.late, 0);
        assert_eq!(stuck.clock.duration_since(stuck.start), QUIESCE_WAIT);
        // Every command is given at most what is left (never the full cap near the end).
        let start = Instant::now();
        assert_eq!(command_budget(start, start + Duration::from_secs(90)), Some(QUIESCE_COMMAND_CAP));
        assert_eq!(command_budget(start, start + Duration::from_millis(40)), Some(Duration::from_millis(40)));
        assert_eq!(command_budget(start + Duration::from_secs(1), start), None);
        assert_eq!(command_budget(start, start), None);
    }

    #[test]
    fn quiesce_spawns_and_reads_nothing_after_the_deadline_process_scan_included() {
        // Enumeration, cgroup read and the gateway lookup use up the deadline:
        // the process scan (and its account lookups) is never started after it.
        let mut late = FakeUnits { cost: Duration::from_secs(10), ..Default::default() };
        let error = quiesce_isolation(&mut late, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains("duduclaw-*"), "{error}");
        assert_eq!(late.late, 0, "nothing spawned or read after the deadline");
        assert!(!late.log.iter().any(|line| line == "processes_clear"), "{:?}", late.log);
        assert!(late.clock.duration_since(late.start) <= QUIESCE_WAIT);
        // Every cost on the fake clock: never a command after the deadline, whatever the state.
        for cost in [1, 3, 7, 11, 29, 31].map(Duration::from_secs) {
            for mut units in [FakeUnits { cost, ..FakeUnits::everything() }, FakeUnits { cost, leftover: true, ..FakeUnits::running() },
                FakeUnits { cost, jobs: vec![(PARSE_INSTANCE.into(), u32::MAX)], ..Default::default() }] {
                let _ = quiesce_isolation(&mut units, QUIESCE_WAIT);
                assert_eq!(units.late, 0, "{cost:?}: {:?}", units.log);
                assert!(units.clock.duration_since(units.start) <= QUIESCE_WAIT);
            }
        }
        // The live helpers themselves: an expired deadline spawns and reads nothing.
        let past = Instant::now();
        assert!(command_within("true", &[], past).is_err());
        assert!(super::super::removal::processes_clear_until(Some(past)).is_err());
        assert!(uid_processes(|_| false, Some(past)).is_err());
    }

    /// Review round 5 item 6: a child as the bounded reaper sees it.
    struct FakeChild {
        /// `try_wait` reports it reaped once killed (else never).
        dies_on_kill: bool,
        /// Review round 6 item 2: `try_wait` fails (`Err`) while it is not reaped.
        try_wait_fails: bool,
        /// Security review: `try_wait` fails with `ECHILD` (no child left) —
        /// from the first check, or once killed.
        echild_first: bool,
        echild_after_kill: bool,
        killed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// How many times `kill` was called.
        kills: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        reaped: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// `wait()` blocks until this is signalled (or its sender dropped).
        release: std::sync::mpsc::Receiver<()>,
    }

    impl super::super::Reapable for FakeChild {
        fn try_reap(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            use std::os::unix::process::ExitStatusExt;
            let killed = self.killed.load(std::sync::atomic::Ordering::SeqCst);
            if self.echild_first || (self.echild_after_kill && killed) {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            let dead = self.dies_on_kill && killed;
            if !dead && self.try_wait_fails { return Err(std::io::Error::other("waitpid failed")); }
            Ok(dead.then(|| std::process::ExitStatus::from_raw(9)))
        }
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
            self.kills.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            use std::os::unix::process::ExitStatusExt;
            let _ = self.release.recv();
            self.reaped.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(std::process::ExitStatus::from_raw(9))
        }
    }

    #[test]
    fn failing_try_wait_is_not_reaped_and_still_gets_sigkill() {
        use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};
        // Review round 6 item 2: `try_wait` errs for a child still running.
        let fake = |dies_on_kill: bool| {
            let (killed, reaped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let (release, receiver) = mpsc::channel();
            (FakeChild { dies_on_kill, try_wait_fails: true, echild_first: false, echild_after_kill: false, killed: killed.clone(),
                kills: Arc::new(std::sync::atomic::AtomicUsize::new(0)), reaped: reaped.clone(), release: receiver },
                killed, reaped, release)
        };
        // It dies on SIGKILL: the kill was sent and the bounded reap saw it.
        let (child, killed, reaped, _release) = fake(true);
        assert!(super::super::kill_and_reap(child, Duration::from_millis(200), Duration::from_millis(20)));
        assert!(killed.load(Ordering::SeqCst), "an Err is not 'already reaped': SIGKILL was sent");
        assert!(!reaped.load(Ordering::SeqCst));
        // `try_wait` keeps failing: SIGKILL, the whole bounded reap, then the detached reaper.
        let (child, killed, reaped, release) = fake(false);
        let started = Instant::now();
        assert!(!super::super::kill_and_reap(child, Duration::from_millis(200), Duration::from_millis(20)));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(200) && took < Duration::from_secs(2), "{took:?}");
        assert!(killed.load(Ordering::SeqCst));
        assert!(!reaped.load(Ordering::SeqCst), "the caller did not wait for it");
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while !reaped.load(Ordering::SeqCst) && Instant::now() < until { std::thread::sleep(Duration::from_millis(10)); }
        assert!(reaped.load(Ordering::SeqCst), "handed to the detached reaper");
    }

    #[test]
    fn echild_from_try_wait_is_reaped_and_never_signalled() {
        use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc, Arc};
        // Security review: `ECHILD` means no child is left (its pid may be reused).
        let fake = |echild_first: bool, echild_after_kill: bool| {
            let (killed, reaped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let kills = Arc::new(AtomicUsize::new(0));
            let (release, receiver) = mpsc::channel::<()>();
            // Dropped: a reaper thread, if one were spawned, would return at once and record it.
            drop(release);
            (FakeChild { dies_on_kill: false, try_wait_fails: true, echild_first, echild_after_kill, killed: killed.clone(),
                kills: kills.clone(), reaped: reaped.clone(), release: receiver }, kills, reaped)
        };
        // `ECHILD` on the first check: no kill, no hand-off, returned at once.
        let (child, kills, reaped) = fake(true, false);
        let started = Instant::now();
        assert!(super::super::kill_and_reap(child, Duration::from_millis(200), Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(kills.load(Ordering::SeqCst), 0, "no signal to a pid that may be reused");
        // `ECHILD` in the polling loop after the kill: reaped, no second kill.
        let (child, kills2, reaped2) = fake(false, true);
        let started = Instant::now();
        assert!(super::super::kill_and_reap(child, Duration::from_millis(200), Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(200), "the loop ended as reaped");
        assert_eq!(kills2.load(Ordering::SeqCst), 1, "SIGKILL exactly once");
        // Neither was handed to the detached reaper.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!reaped.load(Ordering::SeqCst) && !reaped2.load(Ordering::SeqCst), "no reaper hand-off");
    }

    #[test]
    fn command_reap_and_output_read_never_block_past_their_bounds() {
        use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};
        let fake = |dies_on_kill: bool| {
            let (killed, reaped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let (release, receiver) = mpsc::channel();
            (FakeChild { dies_on_kill, try_wait_fails: false, echild_first: false, echild_after_kill: false, killed: killed.clone(),
                kills: Arc::new(std::sync::atomic::AtomicUsize::new(0)), reaped: reaped.clone(), release: receiver },
                killed, reaped, release)
        };
        // Dies on SIGKILL: reaped within the bound by the caller itself.
        let (child, killed, reaped, _release) = fake(true);
        assert!(super::super::kill_and_reap(child, Duration::from_millis(200), Duration::from_millis(20)));
        assert!(killed.load(Ordering::SeqCst) && !reaped.load(Ordering::SeqCst));
        // Cannot be reaped within the bound: the caller returns after about the
        // bound (never waits for it); a detached thread reaps it once it exits.
        let (child, killed, reaped, release) = fake(false);
        let started = Instant::now();
        assert!(!super::super::kill_and_reap(child, Duration::from_millis(200), Duration::from_millis(20)));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(200) && took < Duration::from_secs(2), "{took:?}");
        assert!(killed.load(Ordering::SeqCst));
        assert!(!reaped.load(Ordering::SeqCst), "the caller did not wait for it");
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while !reaped.load(Ordering::SeqCst) && Instant::now() < until { std::thread::sleep(Duration::from_millis(10)); }
        assert!(reaped.load(Ordering::SeqCst), "reaped by the detached thread once it exited");
        // The live bound itself: SIGKILL then at most 2 s of 20 ms polls.
        assert_eq!((super::super::REAP_BOUND, super::super::REAP_STEP), (Duration::from_secs(2), Duration::from_millis(20)));
        // Output: an endless stream read on a fake clock stops at the deadline
        // (refused), whatever is still to come.
        let start = Instant::now();
        let mut ticks = 0u32;
        let clock = || { ticks += 1; start + Duration::from_secs(u64::from(ticks)) };
        let endless = std::io::repeat(b'x');
        assert!(super::super::read_until(endless, u64::MAX - 1, start + Duration::from_secs(3), clock).is_err());
        assert_eq!(ticks, 3, "three clock reads: two chunks, then the deadline");
        // Nothing is read at or after the deadline; within it, at most limit + 1 bytes.
        assert!(super::super::read_until(&b"abc"[..], 10, start, || start).is_err());
        assert_eq!(super::super::read_until(&b"abc"[..], 10, start + Duration::from_secs(1), || start).unwrap(), b"abc");
        assert_eq!(super::super::read_until(std::io::repeat(b'y'), 10, start + Duration::from_secs(1), || start).unwrap().len(), 11);
    }

    #[test]
    fn queued_instance_job_keeps_the_quiesce_waiting_until_it_clears() {
        // A start job for a parse instance queued before its socket stopped,
        // and it never clears: waited for (not ignored), the instance stopped
        // again each round (review round 5 item 3) and refused naming it once
        // it has been stopped MAX_STOPS times.
        let mut never = FakeUnits { loaded: names(&[PARSE_INSTANCE]), jobs: vec![(PARSE_INSTANCE.into(), u32::MAX)],
            cost: Duration::ZERO, ..FakeUnits::running() };
        let error = quiesce_isolation(&mut never, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains(PARSE_INSTANCE) && error.contains("待處理的工作"), "{error}");
        assert!(never.clock.duration_since(never.start) <= QUIESCE_WAIT);
        assert_eq!(never.late, 0);
        assert!(!never.log.iter().any(|line| line == "processes_clear"), "never scanned while a job is queued");
        // Checked (its TEMPLATE's ownership) immediately before every stop.
        let stops: Vec<usize> = never.log.iter().enumerate()
            .filter(|(_, line)| **line == format!("stop {PARSE_INSTANCE}")).map(|(index, _)| index).collect();
        assert_eq!(stops.len(), MAX_STOPS as usize);
        for stop in stops { assert_eq!(never.log[stop - 1], format!("show {PARSE_INSTANCE}")); }
        // The job clears after a few rounds: the quiesce completes, and every
        // condition held in ONE round — units, jobs, cgroups, the gateway
        // account, the full process scan, then units, jobs and cgroups again.
        let mut clears = FakeUnits { loaded: names(&[PARSE_INSTANCE]), jobs: vec![(PARSE_INSTANCE.into(), 2)],
            populated: 2, ..FakeUnits::running() };
        quiesce_isolation(&mut clears, QUIESCE_WAIT).unwrap();
        let log = &clears.log;
        let scan = log.iter().rposition(|line| line == "processes_clear").unwrap();
        let observed = log.iter().rposition(|line| line.starts_with("job ") || line.starts_with("up ")).unwrap();
        assert!(observed < scan, "{log:?}");
        assert_eq!(log[scan - 4..scan], ["list-units", "list-jobs", "cgroup", "gateway"].map(String::from), "{log:?}");
        assert_eq!(log[scan + 1..scan + 4], ["list-units", "list-jobs", "cgroup"].map(String::from), "{log:?}");
        assert!(log[scan + 4..].iter().all(|line| line.starts_with("show ")), "only the post-stop re-reads follow: {log:?}");
        assert_eq!(log.iter().filter(|line| *line == "processes_clear").count(), 1);
    }

    #[test]
    fn success_requires_two_quiet_observations_around_the_process_scan() {
        // Review round 5 item 2: the first scan passes, but a start job for the
        // egress service is queued meanwhile; the observation after the scan
        // sees it, so the round does not succeed: the service is checked and
        // stopped again, and success needs another quiet observation, scan
        // and quiet observation.
        let egress = "duduclaw-egress.service";
        let mut units = FakeUnits { scan_queues: names(&[egress]), ..FakeUnits::running() };
        quiesce_isolation(&mut units, QUIESCE_WAIT).unwrap();
        let log = &units.log;
        let scans: Vec<usize> = log.iter().enumerate().filter(|(_, line)| *line == "processes_clear").map(|(index, _)| index).collect();
        assert_eq!(scans.len(), 2, "{log:?}");
        let job = log.iter().position(|line| *line == format!("job {egress}")).unwrap();
        let restop = log.iter().rposition(|line| *line == format!("stop {egress}")).unwrap();
        assert!(scans[0] < job && job < restop && restop < scans[1], "{log:?}");
        assert_eq!(log[scans[0] + 1..scans[0] + 3], ["list-units", "list-jobs"].map(String::from), "{log:?}");
        assert_eq!(log[restop - 1], format!("show {egress}"));
        assert_eq!(log.iter().filter(|line| **line == format!("stop {egress}")).count(), 2);
        assert_eq!(log[scans[1] + 1..scans[1] + 4], ["list-units", "list-jobs", "cgroup"].map(String::from), "{log:?}");
        assert_eq!(units.late, 0);
    }

    #[test]
    fn file_backed_unit_that_restarts_is_stopped_again_then_refused_after_the_cap() {
        let egress = "duduclaw-egress.service";
        // Started again once (a dependency, socket activation, an administrator):
        // re-checked and stopped again, then the quiesce completes.
        let mut once = FakeUnits { restarts: vec![(egress.into(), 1)], ..FakeUnits::running() };
        quiesce_isolation(&mut once, QUIESCE_WAIT).unwrap();
        let stops: Vec<usize> = once.log.iter().enumerate()
            .filter(|(_, line)| **line == format!("stop {egress}")).map(|(index, _)| index).collect();
        assert_eq!(stops.len(), 2, "{:?}", once.log);
        for stop in stops { assert_eq!(once.log[stop - 1], format!("show {egress}"), "ownership re-read right before"); }
        // Started again every time: refused naming it after MAX_STOPS stops, well before the deadline.
        let mut always = FakeUnits { restarts: vec![(egress.into(), u32::MAX)], ..FakeUnits::running() };
        let error = quiesce_isolation(&mut always, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains(egress) && error.contains(&format!("已停止 {MAX_STOPS} 次")), "{error}");
        assert_eq!(always.log.iter().filter(|line| **line == format!("stop {egress}")).count(), MAX_STOPS as usize);
        assert!(!always.log.iter().any(|line| line == "processes_clear"));
        assert!(always.clock.duration_since(always.start) < QUIESCE_WAIT);
    }

    #[test]
    fn transient_unit_reinvoked_between_check_and_kill_is_not_killed_and_is_rechecked() {
        // Review round 5 item 4: a stopped run still up is about to be killed,
        // but the InvocationID read immediately before the kill is a new one:
        // nothing is sent; the next round treats it as a newly appeared unit
        // (ownership checked, stopped), and only that invocation is killed later.
        let mut units = FakeUnits { up: names(&[RUN]), stuck: names(&[RUN]), reinvoke_at: vec![4], cost: Duration::ZERO,
            ..Default::default() };
        let error = quiesce_isolation(&mut units, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains(RUN), "{error}");
        let log = &units.log;
        assert_eq!(effects(log), [format!("stop {RUN}"), format!("stop {RUN}"), format!("kill {RUN}")]);
        let new = log.iter().position(|line| *line == format!("invocation {RUN} 2")).unwrap();
        let second_stop = log.iter().rposition(|line| *line == format!("stop {RUN}")).unwrap();
        let kill = log.iter().position(|line| *line == format!("kill {RUN}")).unwrap();
        assert!(new < second_stop && second_stop < kill, "{log:?}");
        // The kill follows the re-read of the same (new) invocation, and the
        // stop of the new invocation follows its own ownership read.
        assert_eq!(log[kill - 2..kill], [format!("show {RUN}"), format!("invocation {RUN} 2")], "{log:?}");
        assert_eq!(log[second_stop - 2..second_stop], [format!("show {RUN}"), format!("invocation {RUN} 2")], "{log:?}");
        assert!(!log[..second_stop].iter().any(|line| line.starts_with("kill ")), "{log:?}");
        assert_eq!(units.late, 0);
    }

    #[test]
    fn inactive_unknown_unit_is_ignored_and_reported_once_by_resume_and_rollback() {
        // Review round 5 item 1: an administrator's own unit, merely loaded.
        let backup = "duduclaw-backup.service";
        for state in ["inactive dead", "failed failed"] {
            let line = format!("{backup} loaded {state} Backup job");
            let mut resume = FakeUnits { extra: vec![line.clone()], ..FakeUnits::running() };
            quiesce_isolation(&mut resume, QUIESCE_WAIT).unwrap();
            let mut rollback = FakeUnits { extra: vec![line], ..FakeUnits::everything() };
            super::super::rollback::quiesce(&mut rollback).unwrap();
            for units in [&resume, &rollback] {
                let notes: Vec<&String> = units.log.iter().filter(|line| line.starts_with("note ")).collect();
                assert_eq!(notes.len(), 1, "reported once: {:?}", units.log);
                assert!(notes[0].contains(backup) && notes[0].contains("不會被停止"), "{}", notes[0]);
                assert!(!units.log.iter().any(|line| line.contains(backup) && !line.starts_with("note ")), "never probed or stopped");
                assert_eq!(units.late, 0);
            }
        }
    }

    #[test]
    fn busy_unknown_unit_refuses_with_an_actionable_message_and_nothing_stopped() {
        let backup = "duduclaw-backup.service";
        let check = |units: &mut FakeUnits, rollback: bool| {
            let result = if rollback { super::super::rollback::quiesce(units) } else { quiesce_isolation(units, QUIESCE_WAIT) };
            let error = result.unwrap_err().to_string();
            assert!(error.contains(&format!("sudo systemctl stop {backup}")) && error.contains("不是這次安裝的單元")
                && error.contains("不會停止它"), "{error}");
            assert!(!units.log.iter().any(|line| line.contains(backup) && !line.starts_with("up ") && !line.starts_with("job ")),
                "never probed, stopped or killed: {:?}", units.log);
        };
        for rollback in [false, true] {
            for state in ["active running", "activating start", "deactivating stop-sigterm", "reloading reload"] {
                let mut units = FakeUnits { extra: vec![format!("{backup} loaded {state} Backup")], ..FakeUnits::running() };
                check(&mut units, rollback);
                assert!(effects(&units.log).is_empty(), "refused before any effect: {:?}", units.log);
            }
            // Inactive, but with a queued job: busy.
            let mut units = FakeUnits { extra: vec![format!("{backup} loaded inactive dead Backup")],
                jobs: vec![(backup.into(), u32::MAX)], ..FakeUnits::running() };
            check(&mut units, rollback);
            assert!(effects(&units.log).is_empty());
            // Up only in a later round: refused then; it is never stopped.
            let mut units = FakeUnits { appear: vec![(2, backup.into())], ..FakeUnits::running() };
            check(&mut units, rollback);
        }
    }

    #[test]
    fn unit_appearing_in_a_later_round_is_checked_then_stopped() {
        let mut units = FakeUnits { appear: vec![(2, "duduclaw-parse@4-99-0.service".into()), (3, RUN.into())],
            ..FakeUnits::running() };
        quiesce_isolation(&mut units, QUIESCE_WAIT).unwrap();
        let log = &units.log;
        let position = |line: String| log.iter().position(|seen| *seen == line).unwrap();
        // The last stop of the first round (services stop in the fixed order; spawnd's last).
        let last_first_round = position("stop duduclaw-spawnd.service".to_owned());
        for unit in ["duduclaw-parse@4-99-0.service", RUN] {
            let (show, stop) = (position(format!("show {unit}")), position(format!("stop {unit}")));
            assert!(last_first_round < show && show < stop, "{log:?}");
        }
        assert_eq!(units.late, 0);
        // The same later unit, not provably ours: refused by name, never stopped.
        let mut foreign = FakeUnits { appear: vec![(2, "duduclaw-parse@4-99-0.service".into())],
            foreign: names(&["duduclaw-parse@4-99-0.service"]), ..FakeUnits::running() };
        let error = quiesce_isolation(&mut foreign, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains("duduclaw-parse@4-99-0.service") && error.contains("手動停止"), "{error}");
        assert!(!foreign.log.iter().any(|line| line == "stop duduclaw-parse@4-99-0.service"));
        assert!(!foreign.log.iter().any(|line| line == "processes_clear"));
    }

    #[test]
    fn unknown_duduclaw_unit_shape_is_refused_by_name_and_nothing_stopped() {
        // Up (an inactive one is ignored: `inactive_unknown_unit_is_ignored_and_reported_once_by_resume_and_rollback`).
        for (line, name) in [("duduclaw-helper.service loaded activating start Helper", "duduclaw-helper.service"),
            ("duduclaw-run-XYZ.service loaded active running Run", "duduclaw-run-XYZ.service"),
            ("duduclaw-parse@A_B.service loaded active running Parse", "duduclaw-parse@A_B.service"),
            ("duduclaw-migrate-import.socket loaded active listening Import", "duduclaw-migrate-import.socket")] {
            let mut units = FakeUnits { extra: vec![line.into()], ..FakeUnits::running() };
            let error = quiesce_isolation(&mut units, QUIESCE_WAIT).unwrap_err().to_string();
            assert!(error.contains(name) && error.contains("duduclaw-"), "{error}");
            assert!(effects(&units.log).is_empty(), "{:?}", units.log);
            assert!(!units.log.iter().any(|line| line.starts_with("show ")), "not even probed");
        }
        // A queued job for an unknown name refuses the same way.
        let mut units = FakeUnits { jobs: vec![("duduclaw-other.service".into(), u32::MAX)], ..FakeUnits::running() };
        let error = quiesce_isolation(&mut units, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains("duduclaw-other.service"), "{error}");
        assert!(effects(&units.log).is_empty());
        // Unparseable enumeration output refuses before any effect.
        for line in ["● duduclaw-egress.service loaded active running Egress", "duduclaw-egress.service loaded",
            "duduclaw-egress.service loaded exploded running x", "sshd.service loaded active running OpenSSH"] {
            let mut units = FakeUnits { extra: vec![line.into()], ..FakeUnits::running() };
            assert!(quiesce_isolation(&mut units, QUIESCE_WAIT).is_err(), "{line}");
            assert!(effects(&units.log).is_empty(), "{line}");
        }
    }

    #[test]
    fn unit_name_shapes_are_exact() {
        for fixed in stop_order_all() { assert_eq!(unit_shape(fixed), Some(UnitShape::Fixed), "{fixed}"); }
        for aggregate in ["duduclaw.slice", "duduclaw-tcb.slice", "duduclaw-employees.slice"] {
            assert_eq!(unit_shape(aggregate), Some(UnitShape::Fixed));
        }
        // Every fixed unit is a layout, nightly or gateway unit (never invented here).
        for fixed in stop_order_all() {
            assert!(fixed == UNIT || layout::UNITS.iter().any(|(name, _)| *name == fixed)
                || NIGHTLY_UNITS.iter().any(|(name, _)| *name == fixed), "{fixed}");
        }
        assert_eq!(unit_shape(PARSE_INSTANCE), Some(UnitShape::Instance("duduclaw-parse@.service")));
        assert_eq!(unit_shape("duduclaw-parse@3-unknown.service"), Some(UnitShape::Instance("duduclaw-parse@.service")));
        assert_eq!(unit_shape("duduclaw-import@0a1b2c.service"), Some(UnitShape::Instance("duduclaw-import@.service")));
        assert_eq!(unit_shape("duduclaw-migrate-expire@0a1b.service"), Some(UnitShape::Instance("duduclaw-migrate-expire@.service")));
        assert_eq!(unit_shape("duduclaw-migrate-expire@0a1b.timer"), Some(UnitShape::Instance("duduclaw-migrate-expire@.timer")));
        // R7c: a probe instance names an employee account.
        assert_eq!(unit_shape("duduclaw-isolation-probe@duduclaw-emp-70000.service"),
            Some(UnitShape::Instance("duduclaw-isolation-probe@.service")));
        for unknown in ["duduclaw-isolation-probe@.service", "duduclaw-isolation-probe@duduclaw-emp-70000.timer",
            "duduclaw-isolation-probe@Duduclaw.service", "duduclaw-isolation-probe@-x.service"] {
            assert_eq!(unit_shape(unknown), None, "{unknown}");
        }
        assert_eq!(unit_shape(RUN), Some(UnitShape::Run));
        assert_eq!(unit_shape(EMPLOYEE_SLICE), Some(UnitShape::EmployeeSlice));
        for template in TEMPLATES.map(|(_, _, template)| template) {
            assert!(layout::UNITS.iter().any(|(name, _)| *name == template), "{template}");
        }
        let too_long = format!("duduclaw-parse@{}.service", "1".repeat(65));
        for unknown in ["duduclaw-parse@.service", "duduclaw-parse@-1.service", "duduclaw-parse@A.service",
            "duduclaw-parse@a_b.service", "duduclaw-parse@a.b.service", "duduclaw-parse@../x.service",
            too_long.as_str(), "duduclaw-parse@1.timer", "duduclaw-import@1.timer",
            "duduclaw-run-0123456789abcdef0123456789abcde.service", "duduclaw-run-0123456789ABCDEF0123456789abcdef.service",
            "duduclaw-run-0123456789abcdef0123456789abcdef.scope", "duduclaw-employees-0123.slice",
            "duduclaw-employees-0123456789abcdef0123456789abcdef.service", "duduclaw-helper.service", "duduclaw.service",
            "duduclaw-gateway.socket", "duduclaw-migrate-import.service", "duduclaw-egress.service.d", ""] {
            assert_eq!(unit_shape(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn layout_and_gated_units_are_in_the_quiesce_stop_order() {
        use duduclaw_isolation::ids::{ISOLATED_GATEWAY_UNIT, ISOLATED_GATEWAY_UNIT_DEPENDENCIES, ISOLATION_GATED_UNITS,
            ISOLATION_READY_UNIT, ISOLATION_SELFTEST_UNIT};
        let order = stop_order_all();
        let templates = TEMPLATES.map(|(_, _, template)| template);
        // (i) Every layout unit is accepted by the fixed-name shape and stopped
        // in the fixed order; a template unit file (never loaded itself) has
        // its instances accepted instead.
        for (name, _) in layout::UNITS {
            if templates.contains(name) { continue; }
            assert_eq!(unit_shape(name), Some(UnitShape::Fixed), "{name}");
            assert!(order.contains(name), "{name}");
        }
        for (prefix, suffix, template) in TEMPLATES {
            assert_eq!(format!("{prefix}{suffix}"), template);
            assert_eq!(unit_shape(&format!("{prefix}1{suffix}")), Some(UnitShape::Instance(template)), "{template}");
        }
        // (ii) Every unit R6 gates on the consistency gate is stopped by the quiesce.
        assert_eq!(UNIT, ISOLATED_GATEWAY_UNIT);
        for unit in ISOLATION_GATED_UNITS {
            assert!(order.contains(&unit), "{unit}");
            assert_eq!(unit_shape(unit), Some(UnitShape::Fixed), "{unit}");
        }
        // (iii) The gate stops right after the gateway, no later than spawnd
        // (in the fixed order and in the quiesce's rank).
        assert_eq!(GATEWAY_READY_UNITS, [ISOLATION_READY_UNIT]);
        let position = |unit: &str| order.iter().position(|item| *item == unit).unwrap();
        assert_eq!(position(UNIT), 0);
        assert_eq!(position(ISOLATION_READY_UNIT), 1);
        assert!(position(ISOLATION_READY_UNIT) < position("duduclaw-spawnd.service"));
        assert!(stop_rank(ISOLATION_READY_UNIT, UnitShape::Fixed) < stop_rank("duduclaw-spawnd.service", UnitShape::Fixed));
        assert!(stop_rank(UNIT, UnitShape::Fixed) < stop_rank(ISOLATION_READY_UNIT, UnitShape::Fixed));
        // S6 starts the gate right after spawnd, before egress (bound to it), on both DNS routes.
        for dns in [broker_dns(), Dns { route: DnsRoute::ResolvedVarlink, upstream: None, host_public_addresses: Vec::new() }] {
            let started = started_units(&dns);
            let at = |unit: &str| started.iter().position(|item| *item == unit).unwrap();
            assert_eq!(started.iter().filter(|unit| **unit == ISOLATION_READY_UNIT).count(), 1);
            assert_eq!(at(ISOLATION_READY_UNIT), at("duduclaw-spawnd.service") + 1);
            assert!(at(ISOLATION_READY_UNIT) < at("duduclaw-egress.service"));
            // (iv) R7a: the installer never enables or starts the self-test unit (spawnd does, through PID 1).
            assert!(!started.contains(&ISOLATION_SELFTEST_UNIT));
        }
        // (iv) It is a fixed, installed layout unit, stopped right after spawnd (its
        // only starter), never a gate the gateway binds to and never of the run shape.
        assert_eq!(unit_shape(ISOLATION_SELFTEST_UNIT), Some(UnitShape::Fixed));
        assert_eq!(position(ISOLATION_SELFTEST_UNIT), position("duduclaw-spawnd.service") + 1);
        assert!(stop_rank("duduclaw-spawnd.service", UnitShape::Fixed) < stop_rank(ISOLATION_SELFTEST_UNIT, UnitShape::Fixed));
        assert!(stop_rank(ISOLATION_SELFTEST_UNIT, UnitShape::Fixed) < stop_rank(RUN, UnitShape::Run));
        assert!(!GATEWAY_READY_UNITS.contains(&ISOLATION_SELFTEST_UNIT));
        assert!(!SERVICE_ORDER.contains(&ISOLATION_SELFTEST_UNIT));
        assert!(!ISOLATION_GATED_UNITS.contains(&ISOLATION_SELFTEST_UNIT));
        assert!(!ISOLATED_GATEWAY_UNIT_DEPENDENCIES.contains(ISOLATION_SELFTEST_UNIT));
        // (v) R7c: the probe template is a layout unit file whose instances are
        // owned (stopped with the other services); never enabled or started here.
        use duduclaw_isolation::ids::ISOLATION_PROBE_UNIT_TEMPLATE;
        assert!(templates.contains(&ISOLATION_PROBE_UNIT_TEMPLATE));
        assert!(layout::UNITS.iter().any(|(name, _)| *name == ISOLATION_PROBE_UNIT_TEMPLATE));
        let instance = "duduclaw-isolation-probe@duduclaw-emp-70000.service";
        assert!(stop_rank("duduclaw-spawnd.service", UnitShape::Fixed)
            < stop_rank(instance, UnitShape::Instance(ISOLATION_PROBE_UNIT_TEMPLATE)));
        for dns in [broker_dns(), Dns { route: DnsRoute::ResolvedVarlink, upstream: None, host_public_addresses: Vec::new() }] {
            assert!(!started_units(&dns).iter().any(|unit| unit.starts_with("duduclaw-isolation-probe@")));
        }
        assert!(!SERVICE_ORDER.iter().any(|unit| unit.starts_with("duduclaw-isolation-probe@")));
        assert!(!order.iter().any(|unit| unit.starts_with("duduclaw-isolation-probe@")));
    }

    /// C1–C4: S6 as data. The start order is the boot order and nothing
    /// waits on a unit that starts later.
    #[test]
    fn s6_starts_in_boot_order_and_never_waits_on_a_later_unit() {
        use duduclaw_isolation::ids::ISOLATION_READY_UNIT;
        let entry = |unit: &'static str, action, wait| ServiceStart { unit, action, wait };
        let active = StartWait::Active(READY_WAIT);
        let broker = vec![
            entry("duduclaw-egress.socket", StartAction::EnableNow, active),
            entry("duduclaw-dns-broker.socket", StartAction::EnableNow, active),
            entry("duduclaw-parse.socket", StartAction::EnableNow, active),
            entry("duduclaw-spawnd.socket", StartAction::EnableNow, active),
            entry(SPAWND_SERVICE, StartAction::EnableNow, active),
            entry(ISOLATION_READY_UNIT, StartAction::Gate, active),
            entry("duduclaw-egress.service", StartAction::EnableNow, active),
            entry(UNIT, StartAction::Start, StartWait::Active(GATEWAY_START_WAIT)),
            entry(UPDATER_UNIT, StartAction::EnableNow, active),
            entry(UPDATER_UNIT, StartAction::Await, StartWait::UpdaterGeneration(UPDATER_ANSWER_WAIT)),
            entry(SPAWND_SERVICE, StartAction::Await, StartWait::SpawndEvidence(SPAWND_EVIDENCE_WAIT)),
            entry(NIGHTLY_TIMER, StartAction::EnableNow, active),
        ];
        assert_eq!(service_starts(&broker_dns()), broker);
        let resolved = Dns { route: DnsRoute::ResolvedVarlink, upstream: None, host_public_addresses: Vec::new() };
        let without_broker: Vec<_> = broker.iter().copied().filter(|start| start.unit != "duduclaw-dns-broker.socket").collect();
        assert_eq!(service_starts(&resolved), without_broker);
        for dns in [broker_dns(), resolved] {
            let plan = service_starts(&dns);
            let started_at = |unit: &str| plan.iter().position(|start| start.unit == unit && start.action != StartAction::Await).unwrap();
            let waits_at = |wait: StartWait| plan.iter().position(|start| start.wait == wait).unwrap();
            // Every wait-only entry waits on a unit started before it.
            for (index, start) in plan.iter().enumerate() {
                if start.action == StartAction::Await { assert!(started_at(start.unit) < index, "{start:?}"); }
            }
            // C1: the updater's recovery reads the live gateway, egress and
            // spawnd back, so all of them are started before its answer is awaited.
            let updater = waits_at(StartWait::UpdaterGeneration(UPDATER_ANSWER_WAIT));
            for unit in [SPAWND_SERVICE, "duduclaw-egress.service", UNIT, UPDATER_UNIT] { assert!(started_at(unit) < updater, "{unit}"); }
            // C2/C4: spawnd's full evidence only after the updater answered (and egress runs).
            assert!(updater < waits_at(StartWait::SpawndEvidence(SPAWND_EVIDENCE_WAIT)));
            // The gate ("spawnd is open") before egress, egress before the gateway.
            assert_eq!(started_at(ISOLATION_READY_UNIT), started_at(SPAWND_SERVICE) + 1);
            assert!(started_at("duduclaw-egress.service") < started_at(UNIT));
            assert_eq!(plan.last().map(|start| start.unit), Some(NIGHTLY_TIMER));
            // `started_units` (S6's resume check, the quiesce's coverage) is the started part; the gateway is in it.
            let started: Vec<&str> = plan.iter().filter(|start| start.action != StartAction::Await && start.unit != NIGHTLY_TIMER)
                .map(|start| start.unit).collect();
            assert_eq!(started_units(&dns), started);
            assert!(started.contains(&UNIT));
            // Every started unit is stopped by the rollback/quiesce, the gateway first.
            let order = super::super::rollback::stop_order();
            assert_eq!(order[0], UNIT);
            for unit in &started { assert!(order.contains(unit), "{unit}"); }
        }
        assert_eq!((GATEWAY_START_WAIT, UPDATER_ANSWER_WAIT, SPAWND_EVIDENCE_WAIT),
            (Duration::from_secs(60), Duration::from_secs(120), Duration::from_secs(150)));
    }

    /// C3: the gateway unit files are written at S4 (ledgered, removed by
    /// rollback), the gateway is started at S6 and enabled only at S8.
    #[test]
    fn gateway_unit_is_written_at_s4_started_at_s6_and_enabled_only_at_s8() {
        let source = include_str!("protected_linux_bootstrap.rs");
        let body = |signature: &str| -> &str {
            let start = source.find(signature).unwrap_or_else(|| panic!("{signature}"));
            let rest = &source[start..];
            &rest[..rest.find("\n}\n").unwrap()]
        };
        assert!(body("fn publish_platform(").contains("gateway_unit_files(recorder)"));
        let files = body("fn gateway_unit_files(");
        for item in ["(UNIT_PATH, Kind::File)", "(RECOVERY_BOUND, Kind::File)", "(LAUNCHER, Kind::File)",
            "invalidate_recovery_bound()", "update_text_if_different(UNIT_PATH, 0o644, &text)",
            "update_text_if_different(LAUNCHER, 0o755, LAUNCHER_TEXT)", "reload_and_certify_recovery_text(&text)"] {
            assert!(files.contains(item), "{item}");
        }
        assert!(!files.contains("\"enable\"") && !files.contains("\"start\""));
        let first = body("fn first_gateway_start(");
        assert!(first.contains("&[\"start\", UNIT]") && !first.contains("\"enable\""));
        // R7d: the gateway data is listed only inside the once-per-install snapshot
        // (`Recorder::contents` never calls it again once recorded).
        let snapshot = first.find("context.recorder.contents(ledger::GATEWAY_DATA_PATH, || {").unwrap();
        assert!(first.find("directory_names(").is_some_and(|listing| listing > snapshot));
        assert!(first.find("open_gateway_data(").is_some_and(|open| open > snapshot));
        assert_eq!(first.matches("directory_names(").count(), 1);
        let s8 = body("fn start_gateway(");
        assert!(s8.contains("&[\"enable\", UNIT]"));
        assert!(!s8.contains("\"start\"") && !s8.contains("update_text_if_different") && !s8.contains("intend("));
        // Resume checks: S4 covers the gateway unit files, S8 the enabled unit.
        assert!(body("fn platform_published(").contains("gateway_files()?"));
        assert!(body("fn gateway_published(").contains("unit_enabled(UNIT)?"));
        // S6 never enables it (data).
        for dns in [broker_dns(), Dns { route: DnsRoute::ResolvedVarlink, upstream: None, host_public_addresses: Vec::new() }] {
            let gateway: Vec<_> = service_starts(&dns).into_iter().filter(|start| start.unit == UNIT).collect();
            assert_eq!(gateway, [ServiceStart { unit: UNIT, action: StartAction::Start, wait: StartWait::Active(GATEWAY_START_WAIT) }]);
        }
        // Rollback removes what S4 ledgered.
        let ledgered = super::super::rollback::ledger_files();
        for path in [UNIT_PATH, RECOVERY_BOUND, LAUNCHER] { assert!(ledgered.iter().any(|file| file == path), "{path}"); }
    }

    /// The broker unit's allow line: written once at S4 with its final bytes
    /// for the journal's upstream (no later step rewrites the ledgered file),
    /// expected by S4's and S5's resume checks (S5: the upstream the broker
    /// config records), and loaded by S6's daemon-reload before any start.
    #[test]
    fn broker_unit_is_written_at_s4_for_the_recorded_upstream_and_reloaded_before_start() {
        let source = include_str!("protected_linux_bootstrap.rs");
        let body = |signature: &str| -> &str {
            let start = source.find(signature).unwrap_or_else(|| panic!("{signature}"));
            let rest = &source[start..];
            &rest[..rest.find("\n}\n").unwrap()]
        };
        let s4 = body("fn publish_platform(");
        assert!(s4.contains("let upstream = broker_upstream(dns)?;"));
        assert!(s4.contains("install_isolation_support_for(gateway, &defs, upstream)"));
        assert!(body("fn platform_published(").contains("broker_unit_installed(upstream)?"));
        let s5 = body("fn dns_readback(");
        assert!(s5.contains("Some(written.upstream)") && s5.contains("broker_unit_installed(recorded)?"));
        assert!(!body("fn dns_policy(").contains("unit_file("), "S5 never rewrites a unit file");
        assert!(body("fn broker_unit_installed(").contains("layout::dns_broker_unit_check(&installed, upstream)"));
        let staging = include_str!("protected_linux_install.rs").split("fn install_isolation_support_for(").nth(1).unwrap()
            .split("\nfn isolation_account(").next().unwrap();
        assert!(staging.contains("layout::installed_unit_text(name, shipped, broker_upstream)"));
        // S6 reloads before its first start; the broker socket is one of them on the broker route.
        let s6 = body("fn start_services(");
        assert!(s6.find("&[\"daemon-reload\"]").unwrap() < s6.find("run_start(").unwrap());
        assert!(service_starts(&broker_dns()).iter().any(|start| start.unit == "duduclaw-dns-broker.socket"));
    }

    /// C4: the S6 timeout names the readiness fields still false.
    #[test]
    fn s6_evidence_timeout_names_the_false_fields() {
        let mut checks = duduclaw_spawnd::readiness::Readiness { protocol: duduclaw_isolation::ipc::PROTOCOL_VERSION,
            registry_verified: true, accounts_locked: true, ssh_live_negative_checks: true, cron_at_acl_and_jobs_verified: true,
            external_iam_reservation_verified: true, trampoline_verified: true, landlock_self_test: true, namespace_self_test: true,
            egress_verified: true, dns_route_verified: true, storage_verified: true, resources_verified: true,
            startup_reconciled: true, process_scan_verified: true,
            egress_second_layer: duduclaw_spawnd::readiness::EgressSecondLayer::Unavailable };
        assert!(readiness::unmet_host_checks(&checks).is_empty());
        let passed = evidence_timeout(SPAWND_EVIDENCE_WAIT, Some(&checks));
        assert!(passed.contains("150 秒") && passed.contains("各項檢查都已通過"), "{passed}");
        checks.startup_reconciled = false;
        checks.dns_route_verified = false;
        let named = evidence_timeout(SPAWND_EVIDENCE_WAIT, Some(&checks));
        assert!(named.contains("啟動時的執行紀錄對帳尚未完成。") && named.contains("DNS 解析路徑未通過驗證。"), "{named}");
        assert!(!named.contains("員工身分登錄未通過驗證。") && !named.contains("Landlock"), "{named}");
        checks.landlock_self_test = false;
        assert!(evidence_timeout(SPAWND_EVIDENCE_WAIT, Some(&checks)).contains("Landlock"));
        // Nothing true: every field (13 primary checks and Landlock) is named.
        let none = duduclaw_spawnd::readiness::Readiness::default();
        let unmet = readiness::unmet_host_checks(&none);
        assert_eq!(unmet.len(), 14);
        let text = evidence_timeout(SPAWND_EVIDENCE_WAIT, Some(&none));
        for message in unmet { assert!(text.contains(message), "{message}"); }
        assert!(evidence_timeout(SPAWND_EVIDENCE_WAIT, None).contains("無法讀取"));
    }

    /// S6: an update service PID 1 reports `failed` is named, with the journal command; never restarted in a loop.
    #[test]
    fn failed_updater_is_reported_with_its_journal_command() {
        assert_eq!(failed_result("ActiveState=failed\nResult=start-limit-hit\n"), Some("start-limit-hit"));
        assert_eq!(failed_result("Result=exit-code\nActiveState=failed\n"), Some("exit-code"));
        assert_eq!(failed_result("ActiveState=active\nResult=success\n"), None);
        assert_eq!(failed_result("ActiveState=activating\nResult=success\n"), None);
        assert_eq!(failed_result("ActiveState=failed\nResult=Bad value\n"), None);
        assert_eq!(failed_result("ActiveState=failed\n"), None);
        let message = updater_failed(UPDATER_UNIT, "start-limit-hit").to_string();
        for part in ["start-limit-hit", "短時間內重新啟動太多次", "sudo journalctl -b -u duduclaw-updated.service",
            "sudo systemctl reset-failed duduclaw-updated.service", "不會一直重新啟動"] {
            assert!(message.contains(part), "{part}: {message}");
        }
        assert!(!updater_failed(UPDATER_UNIT, "exit-code").to_string().contains("短時間內"));
        // The wait loop refuses on `failed` instead of starting the unit again.
        let source = include_str!("protected_linux_bootstrap.rs");
        let start = source.find("fn await_updater(").unwrap();
        let rest = &source[start..];
        let body = &rest[..rest.find("\n}\n").unwrap()];
        assert!(body.contains("unit_failed_result(unit)") && !body.contains("systemctl"));
        // `EnableNow` asks the failed state BEFORE `enable --now`: a resumed
        // install after the updater failed is refused and nothing is started.
        let run = |before: Option<&str>, after: Option<&str>, started: bool, active: bool| {
            let calls = std::cell::RefCell::new(Vec::new());
            let mut asked = 0;
            let result = enable_now_with(UPDATER_UNIT,
                |unit| { assert_eq!(unit, UPDATER_UNIT); calls.borrow_mut().push("failed?"); asked += 1;
                    (if asked == 1 { before } else { after }).map(str::to_owned) },
                |unit| { assert_eq!(unit, UPDATER_UNIT); calls.borrow_mut().push("enable --now"); Ok(started) },
                || { calls.borrow_mut().push("wait"); active });
            (result.map_err(|error| error.to_string()), calls.into_inner())
        };
        let (result, calls) = run(Some("exit-code"), None, true, true);
        let message = result.unwrap_err();
        assert!(message.contains("sudo systemctl reset-failed duduclaw-updated.service")
            && message.contains("sudo journalctl -b -u duduclaw-updated.service"), "{message}");
        assert_eq!(calls, ["failed?"], "failed before start: no start attempted");
        let (result, calls) = run(Some("start-limit-hit"), None, true, true);
        assert!(result.unwrap_err().contains("短時間內重新啟動太多次"));
        assert_eq!(calls, ["failed?"]);
        // Not failed: started, then waited for.
        let (result, calls) = run(None, None, true, true);
        assert!(result.is_ok());
        assert_eq!(calls, ["failed?", "enable --now", "wait"]);
        // The post-start failed check stays: after a non-zero start and after a timed-out wait.
        let (result, calls) = run(None, Some("exit-code"), false, true);
        assert!(result.unwrap_err().contains("reset-failed"));
        assert_eq!(calls, ["failed?", "enable --now", "failed?"]);
        let (result, calls) = run(None, Some("exit-code"), true, false);
        assert!(result.unwrap_err().contains("reset-failed"));
        assert_eq!(calls, ["failed?", "enable --now", "wait", "failed?"]);
        let (result, _) = run(None, None, true, false);
        assert!(result.unwrap_err().contains("未在時限內進入執行狀態"));
        // Production wiring: `enable_now` uses this order with the real probe.
        let start = source.find("fn enable_now(").unwrap();
        let rest = &source[start..];
        let body = &rest[..rest.find("\n}\n").unwrap()];
        assert!(body.contains("enable_now_with(unit, unit_failed_result,"), "{body}");
    }

    /// `--enable-now` is accepted (compat) and has no effect beyond one notice.
    #[test]
    fn enable_now_is_accepted_and_inert() {
        let args = |mode: &str, enable_now| super::super::super::BootstrapContinueArgs { mode: mode.into(),
            generation: "ab".repeat(32), dns_upstream: None, host_public_address: Vec::new(), enable_now };
        assert!(ContinueOptions::from_args(args("install", true)).unwrap().enable_now);
        assert!(!ContinueOptions::from_args(args("install", false)).unwrap().enable_now);
        // Outside an install it stays refused, as before.
        assert!(ContinueOptions::from_args(args("rollback", true)).is_err());
        assert!(ENABLE_NOW_INERT.contains("--enable-now"));
        let source = include_str!("protected_linux_bootstrap.rs");
        let production = &source[..source.find("#[cfg(test)]\nmod tests").unwrap()];
        assert!(production.contains("if options.enable_now { println!(\"{ENABLE_NOW_INERT}\"); }"));
        assert!(!production.contains("context.enable_now"));
        assert_eq!(production.matches("options.enable_now").count(), 1, "read only for the notice");
    }

    #[test]
    fn transient_runs_and_slices_must_be_transient_under_the_employees_slice() {
        let invocation = "00112233445566778899aabbccddeeff";
        let run = format!("LoadState=loaded\nTransient=yes\nSlice={EMPLOYEE_SLICE}\n\
            ControlGroup=/duduclaw.slice/duduclaw-employees.slice/{EMPLOYEE_SLICE}/{RUN}\nInvocationID={invocation}\n");
        assert!(transient_show_is_ours(run.as_bytes(), RUN, UnitShape::Run));
        assert_eq!(invocation_of(&run), Some(invocation));
        // Not running (no control group yet / any more): placement by Slice alone.
        let idle = run.replace(&format!("/duduclaw.slice/duduclaw-employees.slice/{EMPLOYEE_SLICE}/{RUN}"), "");
        assert!(transient_show_is_ours(idle.as_bytes(), RUN, UnitShape::Run));
        // Review round 5 item 4: not started yet (a queued job only): no invocation.
        let unstarted = run.replace(&format!("InvocationID={invocation}"), "InvocationID=");
        assert!(transient_show_is_ours(unstarted.as_bytes(), RUN, UnitShape::Run));
        assert_eq!(invocation_of(&unstarted), None);
        // The InvocationID strict: 32 lowercase hex, present exactly once.
        for bad in [run.replace(invocation, &invocation.to_uppercase()), run.replace(invocation, &invocation[1..]),
            run.replace(invocation, &format!("{invocation}0")), run.replace(invocation, "0123456789abcdef-0123456789abcde"),
            run.replace(&format!("InvocationID={invocation}\n"), ""), format!("{run}InvocationID={invocation}\n")] {
            assert!(!transient_show_is_ours(bad.as_bytes(), RUN, UnitShape::Run), "{bad}");
        }
        for bad in [run.replace("Transient=yes", "Transient=no"), run.replace("LoadState=loaded", "LoadState=not-found"),
            run.replace(&format!("Slice={EMPLOYEE_SLICE}"), "Slice=system.slice"),
            run.replace(&format!("Slice={EMPLOYEE_SLICE}"), "Slice=duduclaw-employees.slice"),
            run.replace("ControlGroup=/duduclaw.slice/duduclaw-employees.slice/", "ControlGroup=/system.slice/"),
            run.replace(&format!("/{RUN}\n"), "/other.service\n"), format!("{run}Transient=yes\n"), format!("{run}Extra=1\n"),
            run.replace("Transient=yes\n", ""), "garbage".into(), String::new()] {
            assert!(!transient_show_is_ours(bad.as_bytes(), RUN, UnitShape::Run), "{bad}");
        }
        assert!(!transient_show_is_ours(&[0xff], RUN, UnitShape::Run));
        // Placed right, but the shape asked for is another one.
        assert!(!transient_show_is_ours(run.as_bytes(), RUN, UnitShape::EmployeeSlice));
        assert!(!transient_show_is_ours(run.as_bytes(), "duduclaw-run-x.service", UnitShape::Run));
        let slice = format!("LoadState=loaded\nTransient=yes\nSlice=duduclaw-employees.slice\n\
            ControlGroup=/duduclaw.slice/duduclaw-employees.slice/{EMPLOYEE_SLICE}\nInvocationID={invocation}\n");
        assert!(transient_show_is_ours(slice.as_bytes(), EMPLOYEE_SLICE, UnitShape::EmployeeSlice));
        for bad in [slice.replace("Transient=yes", "Transient=no"), slice.replace("Slice=duduclaw-employees.slice", "Slice=duduclaw.slice"),
            slice.replace("ControlGroup=/duduclaw.slice/duduclaw-employees.slice/", "ControlGroup=/duduclaw.slice/")] {
            assert!(!transient_show_is_ours(bad.as_bytes(), EMPLOYEE_SLICE, UnitShape::EmployeeSlice), "{bad}");
        }
        // Through the quiesce: a run (or employee slice) up that is not provably
        // ours (Transient=no, not under the employees slice) refuses by name, nothing stopped.
        for unit in [RUN, EMPLOYEE_SLICE] {
            let mut units = FakeUnits { up: names(&[unit]), foreign: names(&[unit]), ..Default::default() };
            let error = quiesce_isolation(&mut units, QUIESCE_WAIT).unwrap_err().to_string();
            assert!(error.contains(unit) && error.contains("duduclaw-employees.slice"), "{error}");
            assert!(effects(&units.log).is_empty(), "{:?}", units.log);
        }
        // A stopped transient run still up is killed once (fixed argv), then gone.
        let mut lingering = FakeUnits { up: names(&[RUN]), ..Default::default() };
        lingering.stuck = names(&[RUN]);
        let error = quiesce_isolation(&mut lingering, QUIESCE_WAIT).unwrap_err().to_string();
        assert!(error.contains(RUN), "{error}");
        assert_eq!(effects(&lingering.log), [format!("stop {RUN}"), format!("kill {RUN}")]);
    }

    #[test]
    fn rollback_and_resume_run_the_same_quiesce() {
        // One fake PID 1 state, driven once by the install resume and once by the rollback.
        let mut host = ScriptedHost { broken: vec![Step::DnsPolicy], units: FakeUnits::everything(), ..Default::default() };
        let mut position = journal().at(Step::StartGateway, Phase::Completed);
        drive_scripted(&mut position, &mut host).0.unwrap();
        let mut rollback = FakeUnits::everything();
        super::super::rollback::quiesce(&mut rollback).unwrap();
        let resume: Vec<String> = effects(&host.units.log).into_iter().filter(|line| !line.starts_with("perform ")).collect();
        assert_eq!(resume, effects(&rollback.log));
        assert_eq!(host.units.log.iter().filter(|line| !line.starts_with("perform ")).cloned().collect::<Vec<_>>(), rollback.log);
        // The order: the gateway, the consistency gate it binds to, every activation
        // source (timers, sockets), the services (fixed, then instances and runs),
        // the employee slice, the aggregate slices.
        let order: Vec<&str> = resume.iter().filter_map(|line| line.strip_prefix("stop ")).collect();
        assert_eq!(order, [UNIT, duduclaw_isolation::ids::ISOLATION_READY_UNIT, NIGHTLY_TIMER, "duduclaw-migrate-expire@0a1b.timer", "duduclaw-egress.socket",
            "duduclaw-spawnd.socket", "duduclaw-updated.service", "duduclaw-egress.service", "duduclaw-spawnd.service",
            PARSE_INSTANCE, RUN, EMPLOYEE_SLICE, "duduclaw-employees.slice", "duduclaw-tcb.slice", "duduclaw.slice"]);
        assert_eq!(resume.last().map(String::as_str), Some("processes_clear"));
    }

    #[test]
    fn unit_redefined_across_its_stop_is_refused() {
        // A package replaced the egress unit and reloaded PID 1 while it was being stopped.
        let forged = journal().at(Step::StartGateway, Phase::Completed);
        let mut host = ScriptedHost { broken: vec![Step::PublishPlatform],
            units: FakeUnits { redefined: names(&["duduclaw-egress.service"]), ..FakeUnits::running() }, ..Default::default() };
        let mut position = forged.clone();
        let (result, writes) = drive_scripted(&mut position, &mut host);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("duduclaw-egress.service") && error.contains("更換"), "{error}");
        assert!(host.performed.is_empty() && writes.is_empty() && position == forged);
        // The definition was read immediately before the stop and once more after convergence.
        let log = &host.units.log;
        let stop = log.iter().position(|line| line == "stop duduclaw-egress.service").unwrap();
        assert_eq!(log.get(stop - 1).map(String::as_str), Some("show duduclaw-egress.service"));
        let after = log.iter().rposition(|line| line == "show duduclaw-egress.service").unwrap();
        assert!(after > log.iter().rposition(|line| line == "processes_clear").unwrap());
    }

    #[test]
    fn enumeration_output_is_parsed_strictly() {
        let units = parse_unit_list(b"duduclaw-egress.service loaded active running DuDuClaw egress\n\
            duduclaw.slice loaded inactive dead Slice\nduduclaw-parse@3-1-0.service loaded activating start Parse\n").unwrap();
        assert_eq!(units, vec![("duduclaw-egress.service".to_owned(), true), ("duduclaw.slice".to_owned(), false),
            ("duduclaw-parse@3-1-0.service".to_owned(), true)]);
        assert_eq!(parse_unit_list(b"").unwrap(), Vec::new());
        for bad in [&b"duduclaw-egress.service loaded\n"[..], b"duduclaw-egress.service loaded weird running x\n",
            b"duduclaw-egress.service someload active running x\n", b"other.service loaded active running x\n",
            b"duduclaw-a.service loaded active running x\nduduclaw-a.service loaded active running x\n", &[0xff, b'\n']] {
            assert!(parse_unit_list(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let flood: String = (0..=MAX_LISTED).map(|index| format!("duduclaw-u{index}.service loaded inactive dead x\n")).collect();
        assert!(parse_unit_list(flood.as_bytes()).is_err());
        assert_eq!(parse_job_list(b"123 duduclaw-parse@3-1-0.service start waiting\n").unwrap(), ["duduclaw-parse@3-1-0.service"]);
        assert_eq!(parse_job_list(b"").unwrap(), Vec::<String>::new());
        for bad in [&b"x duduclaw-a.service start waiting\n"[..], b"1 duduclaw-a.service start\n",
            b"1 duduclaw-a.service start waiting extra\n", b"1 duduclaw-a.service start paused\n", b"1 sshd.service start waiting\n",
            b"No jobs running.\n"] {
            assert!(parse_job_list(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert_eq!(cgroup_populated("populated 0\nfrozen 0\n"), Some(false));
        assert_eq!(cgroup_populated("populated 1\nfrozen 0\n"), Some(true));
        for bad in ["", "frozen 0\n", "populated 2\n", "populated 0\npopulated 0\n", "populated\n", "Populated 0\n", "populated x\n"] {
            assert_eq!(cgroup_populated(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn component_proofs_require_this_installs_window() {
        let window = Some(ledger::Clocks { realtime: (2_000, 0), boottime: (100, 0) });
        // Checked 10 s later on both clocks: no wall-clock step.
        let now = Some(ledger::Clocks { realtime: (2_010, 0), boottime: (110, 0) });
        let facts = |identity: &ledger::Identity, btime: Option<ledger::Timestamp>, nlink: u64| ComponentFacts {
            parent_mount: Some(identity.mount_id), absent_at_intent: true, window, now,
            creation: Some(ledger::Creation { device: identity.device, inode: identity.inode, nlink, btime }) };
        let boot = "0123abcd-0000-4000-8000-00000000beef".to_owned();
        let directory = |mode: u32| ledger::Identity { device: 1, inode: 2, mount_id: 3, mode: libc::S_IFDIR | mode, uid: 0, gid: 0,
            size: None, sha256: None, boot_id: boot.clone() };
        let key = ledger::Identity { device: 1, inode: 4, mount_id: 3, mode: libc::S_IFREG | duduclaw_core::root_key::KEY_MODE, uid: 0, gid: 0,
            size: Some(duduclaw_core::root_key::KEY_BYTES), sha256: Some("ab".repeat(32)), boot_id: boot.clone() };
        let (state, runtime) = (directory(0o700), directory(0o755));
        // Absent at intent, born inside the window: proven.
        assert!(state_root_proven(&state, &facts(&state, Some((2_000, 1)), 2), true));
        assert!(runtime_directory_proven(&runtime, &facts(&runtime, Some((2_005, 0)), 2), true));
        assert!(root_key_proven(&key, &facts(&key, Some((2_000, 0)), 1), true));
        // Review round 5 item 5: no birth time reported (never the change time instead): proven.
        assert!(state_root_proven(&state, &facts(&state, None, 2), true));
        assert!(runtime_directory_proven(&runtime, &facts(&runtime, None, 2), true));
        assert!(root_key_proven(&key, &facts(&key, None, 1), true));
        // A birth time predating the window with no step: each of the three refused.
        let early = Some((1_999, 999_999_999));
        assert!(!state_root_proven(&state, &facts(&state, early, 2), true));
        assert!(!runtime_directory_proven(&runtime, &facts(&runtime, early, 2), true));
        assert!(!root_key_proven(&key, &facts(&key, early, 1), true));
        // The wall clock stepped back an hour after the window opened: the
        // object is "born" before it, yet proven (absence-then-identity).
        let back = Some(ledger::Clocks { realtime: (2_000 - 3_600 + 10, 0), boottime: (110, 0) });
        let stepped = |identity: &ledger::Identity, nlink: u64|
            ComponentFacts { now: back, ..facts(identity, Some((2_000 - 3_600 + 5, 0)), nlink) };
        assert!(state_root_proven(&state, &stepped(&state, 2), true));
        assert!(runtime_directory_proven(&runtime, &stepped(&runtime, 2), true));
        assert!(root_key_proven(&key, &stepped(&key, 1), true));
        // The path was not absent at the intent (present as a recorded object,
        // or never intended in this run): refused whatever the time facts say.
        for btime in [Some((2_001, 0)), None] {
            let present = |identity: &ledger::Identity, nlink: u64|
                ComponentFacts { absent_at_intent: false, ..facts(identity, btime, nlink) };
            assert!(!state_root_proven(&state, &present(&state, 2), true), "{btime:?}");
            assert!(!runtime_directory_proven(&runtime, &present(&runtime, 2), true), "{btime:?}");
            assert!(!root_key_proven(&key, &present(&key, 1), true), "{btime:?}");
        }
        // A clock unreadable: no time comparison (absence-then-identity still required).
        let unreadable = ComponentFacts { window: None, ..facts(&key, early, 1) };
        assert!(root_key_proven(&key, &unreadable, true));
        assert!(!root_key_proven(&key, &ComponentFacts { absent_at_intent: false, ..unreadable }, true));
        // The statx facts of another inode: refused.
        let other = ComponentFacts { creation: Some(ledger::Creation { device: 1, inode: 99, nlink: 1, btime: Some((2_001, 0)) }),
            ..facts(&key, None, 1) };
        assert!(!root_key_proven(&key, &other, true));
        assert!(!state_root_proven(&state, &ComponentFacts { creation: None, ..facts(&state, Some((2_001, 0)), 2) }, true));
        // The producer facts: R1's state file not naming this generation, the
        // socket not listening in it, the gateway unit not this install's.
        assert!(!state_root_proven(&state, &facts(&state, Some((2_001, 0)), 2), false));
        assert!(!runtime_directory_proven(&runtime, &facts(&runtime, Some((2_001, 0)), 2), false));
        assert!(!root_key_proven(&key, &facts(&key, Some((2_001, 0)), 1), false));
        // The root key: exactly 0400 and 32 bytes, one link, root-owned, with a digest.
        for bad in [ledger::Identity { mode: libc::S_IFREG | 0o640, ..key.clone() },
            ledger::Identity { mode: libc::S_IFREG | 0o600, ..key.clone() },
            ledger::Identity { size: Some(31), ..key.clone() }, ledger::Identity { size: Some(33), ..key.clone() },
            ledger::Identity { size: None, ..key.clone() }, ledger::Identity { uid: 1, ..key.clone() },
            ledger::Identity { sha256: None, ..key.clone() }, ledger::Identity { mode: libc::S_IFDIR | duduclaw_core::root_key::KEY_MODE, ..key.clone() }] {
            assert!(!root_key_proven(&bad, &facts(&bad, Some((2_001, 0)), 1), true), "{bad:?}");
        }
        assert!(!root_key_proven(&key, &facts(&key, Some((2_001, 0)), 2), true), "a second link");
        // The state root must be private; neither directory may be a mount point of its own.
        assert!(!state_root_proven(&directory(0o755), &facts(&directory(0o755), Some((2_001, 0)), 2), true));
        let mounted = ComponentFacts { parent_mount: Some(9), ..facts(&runtime, Some((2_001, 0)), 2) };
        assert!(!runtime_directory_proven(&runtime, &mounted, true));
    }

    #[test]
    fn unit_is_ours_only_for_the_ledgered_loaded_unmodified_unit() {
        let unit = "duduclaw-egress.service";
        let ours = "LoadState=loaded\nFragmentPath=/etc/systemd/system/duduclaw-egress.service\nDropInPaths=\nNeedDaemonReload=no\nTransient=no\n";
        assert!(unit_show_is_ours(Some(ours.as_bytes()), unit));
        for bad in [ours.replace("NeedDaemonReload=no", "NeedDaemonReload=yes"), ours.replace("Transient=no", "Transient=yes"),
            ours.replace("LoadState=loaded", "LoadState=not-found"), ours.replace("LoadState=loaded", "LoadState=masked"),
            ours.replace("DropInPaths=", "DropInPaths=/run/systemd/system/duduclaw-egress.service.d/x.conf"),
            ours.replace("/etc/systemd/system/", "/usr/lib/systemd/system/"), ours.replace("Transient=no\n", ""),
            format!("{ours}Transient=no\n"), String::new(), "garbage".into()] {
            assert!(!unit_show_is_ours(Some(bad.as_bytes()), unit), "{bad}");
        }
        // Unreadable (a failed `systemctl show`) or not UTF-8: not ours.
        assert!(!unit_show_is_ours(None, unit));
        assert!(!unit_show_is_ours(Some(&[0xff_u8, 0xfe][..]), unit));
        // Another unit's fragment is not this unit's.
        assert!(!unit_show_is_ours(Some(ours.as_bytes()), "duduclaw-spawnd.service"));
    }

    #[test]
    fn runtime_directory_owner_is_the_socket_listening_in_it() {
        let listen = b"Listen=/run/duduclaw-spawnd/control.sock (SequentialPacket)\n";
        assert!(listens_in(listen, SPAWND_RUNTIME));
        assert!(!listens_in(b"Listen=/run/other/control.sock (SequentialPacket)\n", SPAWND_RUNTIME));
        assert!(!listens_in(b"Listen=/run/duduclaw-spawnd/sub/control.sock (SequentialPacket)\n", SPAWND_RUNTIME));
        assert!(!listens_in(b"Listen=\n", SPAWND_RUNTIME));
        assert!(!listens_in(b"", SPAWND_RUNTIME));
        assert!(!listens_in(&[0xff], SPAWND_RUNTIME));
    }

    #[test]
    fn spawnd_publication_field_set_is_pinned() {
        // The producer's serialisation (what S4 writes) has exactly the fields
        // of spawnd's consumer `HostPublication` (installed.rs), in its shape.
        let generation = digest(1);
        let written = serde_json::to_value(HostPublication::expected(generation)).unwrap();
        let mut fields: Vec<&str> = written.as_object().unwrap().keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(fields, ["entitlements", "generation", "version"]);
        assert_eq!(written, serde_json::json!({ "version": 1, "generation": generation, "entitlements": [] }));
        let parsed: HostPublication = serde_json::from_value(written).unwrap();
        assert_eq!(parsed, HostPublication::expected(generation));
    }

    #[test]
    fn publication_must_equal_this_install_in_every_field() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let me = unsafe { (libc::geteuid(), libc::getegid()) };
        let generation = digest(1);
        // spawnd's check refusing every entitlement (the pre-R8d rule).
        let refuse = |_: &[serde_json::Value]| false;
        let path = directory.path().join("publication.json");
        let write = |value: &serde_json::Value| {
            let _ = fs::remove_file(&path);
            fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        let good = serde_json::json!({ "version": 1, "generation": generation, "entitlements": [] });
        write(&good);
        assert!(publication_holds(&dir, me, generation, &refuse));
        // Absent: nothing holds (and the reader says absent, not damaged).
        fs::remove_file(&path).unwrap();
        assert!(read_publication_at(&dir, me).unwrap().is_none());
        assert!(!publication_holds(&dir, me, generation, &refuse));
        // Same generation, but forged or left-over content: S4 does not hold.
        let entitlement = serde_json::json!([{ "employee": "00".repeat(16), "profiles": [digest(2)] }]);
        for bad in [
            serde_json::json!({ "version": 1, "generation": generation, "entitlements": [], "extra": true }),
            serde_json::json!({ "version": 2, "generation": generation, "entitlements": [] }),
            serde_json::json!({ "version": 1, "generation": generation, "entitlements": entitlement }),
            serde_json::json!({ "version": 1, "generation": digest(3), "entitlements": [] }),
            serde_json::json!({ "version": 1, "generation": generation }),
            serde_json::json!([]),
        ] {
            write(&bad);
            assert!(!publication_holds(&dir, me, generation, &refuse), "{bad}");
        }
        // Wrong mode, another owner, a hard link, oversize: refused before parsing.
        write(&good);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!publication_holds(&dir, me, generation, &refuse));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!publication_holds(&dir, (me.0.wrapping_add(1), me.1), generation, &refuse));
        fs::hard_link(&path, directory.path().join("alias")).unwrap();
        assert!(!publication_holds(&dir, me, generation, &refuse));
        fs::remove_file(directory.path().join("alias")).unwrap();
        assert!(publication_holds(&dir, me, generation, &refuse));
        let mut padded = serde_json::to_vec(&good).unwrap();
        padded.resize(MAX_PUBLICATION_BYTES as usize + 1, b' ');
        fs::write(&path, &padded).unwrap();
        assert!(!publication_holds(&dir, me, generation, &refuse));
        // A symlink to a correct publication is never followed.
        fs::remove_file(&path).unwrap();
        let target = directory.path().join("real.json");
        fs::write(&target, serde_json::to_vec(&good).unwrap()).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_publication_at(&dir, me).is_err());
        assert!(!publication_holds(&dir, me, generation, &refuse));
    }

    /// R8d C: a resumed / re-run install after spawnd entitled employees
    /// holds when the entitlements pass spawnd's check for THIS generation;
    /// another generation or invalid content never holds.
    #[test]
    fn publication_with_admitted_entitlements_of_this_generation_holds() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let dir = fs::File::open(directory.path()).unwrap();
        let me = unsafe { (libc::geteuid(), libc::getegid()) };
        let generation = digest(1);
        let path = directory.path().join("publication.json");
        let write = |value: &serde_json::Value| {
            let _ = fs::remove_file(&path);
            fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        let record = serde_json::json!({ "employee": vec![5u8; 16], "profiles": [digest(2)] });
        let entitled = serde_json::json!({ "version": 1, "generation": generation, "entitlements": [record.clone()] });
        // What the check is asked about is exactly the stored records.
        let seen = std::cell::RefCell::new(Vec::new());
        let admit = |entitlements: &[serde_json::Value]| { seen.borrow_mut().push(entitlements.to_vec()); true };
        let refuse = |_: &[serde_json::Value]| false;
        write(&entitled);
        assert!(publication_holds(&dir, me, generation, &admit));
        assert_eq!(*seen.borrow(), vec![vec![record.clone()]]);
        assert!(!publication_holds(&dir, me, generation, &refuse), "invalid entitlements");
        // Another generation never holds, whatever the check says.
        write(&serde_json::json!({ "version": 1, "generation": digest(3), "entitlements": [record.clone()] }));
        assert!(!publication_holds(&dir, me, generation, &admit));
        write(&serde_json::json!({ "version": 2, "generation": generation, "entitlements": [record] }));
        assert!(!publication_holds(&dir, me, generation, &admit));
        // A fresh install's empty list holds without asking the check.
        seen.borrow_mut().clear();
        write(&serde_json::json!({ "version": 1, "generation": generation, "entitlements": [] }));
        assert!(publication_holds(&dir, me, generation, &refuse));
        assert!(seen.borrow().is_empty());
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
        let digest = host_digest_of(&format!("{id}\n"), None).unwrap();
        assert_eq!(host_digest_of(id, None), Some(digest.clone()));
        assert!(parse_generation(&digest).is_some());
        assert_ne!(host_digest_of("fedcba9876543210fedcba9876543210", None), Some(digest));
        for bad in ["", "0123456789ABCDEF0123456789abcdef", "0123456789abcdef", "0123456789abcdef0123456789abcdef\n\n",
            "uninitialized\n"] {
            assert_eq!(host_digest_of(bad, None), None, "{bad:?}");
        }
    }

    #[test]
    fn host_binding_refuses_degenerate_ids_and_binds_product_uuid_presence() {
        let id = "0123456789abcdef0123456789abcdef";
        // Review M4: an all-zero or one-digit machine id identifies nothing.
        for degenerate in ["0".repeat(32), format!("{}\n", "0".repeat(32)), "f".repeat(32), "7".repeat(32)] {
            assert_eq!(host_digest_of(&degenerate, None), None, "{degenerate:?}");
            assert_eq!(host_digest_of(&degenerate, Some("4c4c4544-0042-3510-8051-b3c04f4e3132\n")), None);
        }
        // The DMI product UUID: strict 8-4-4-4-12 form; case-insensitive; a trailing newline.
        let uuid = "4c4c4544-0042-3510-8051-b3c04f4e3132";
        assert_eq!(parse_product_uuid(&format!("{uuid}\n")), Ok(Some(uuid.to_owned())));
        assert_eq!(parse_product_uuid(&uuid.to_uppercase()), Ok(Some(uuid.to_owned())));
        for bad in ["", "4c4c4544004235108051b3c04f4e3132", "4c4c4544-0042-3510-8051-b3c04f4e313", "4c4c4544-0042-3510-8051-b3c04f4e3132x",
            "4c4c4544_0042_3510_8051_b3c04f4e3132", "zc4c4544-0042-3510-8051-b3c04f4e3132", "Not Settable\n"] {
            assert_eq!(parse_product_uuid(bad), Err(()), "{bad:?}");
            assert_eq!(host_digest_of(id, Some(bad)), None, "{bad:?}");
        }
        // A degenerate UUID (all zero / one digit) adds nothing: recorded as absent.
        assert_eq!(parse_product_uuid("00000000-0000-0000-0000-000000000000\n"), Ok(None));
        assert_eq!(host_digest_of(id, Some("ffffffff-ffff-ffff-ffff-ffffffffffff")), host_digest_of(id, None));
        // Presence is bound: a UUID that appears, vanishes or changes on resume is another host.
        let absent = host_digest_of(id, None).unwrap();
        let present = host_digest_of(id, Some(uuid)).unwrap();
        assert_ne!(absent, present);
        assert_eq!(host_digest_of(id, Some(&uuid.to_uppercase())), Some(present.clone()));
        assert_ne!(host_digest_of(id, Some("4c4c4544-0042-3510-8051-b3c04f4e3133")), Some(present.clone()));
        let recorded = Journal::new(&"ab".repeat(32), &absent, &nonce(), broker_dns(), 100);
        bound_to(&recorded, &"ab".repeat(32), &absent).unwrap();
        assert!(bound_to(&recorded, &"ab".repeat(32), &present).unwrap_err().to_string().contains("另一台主機"));
        let recorded = Journal::new(&"ab".repeat(32), &present, &nonce(), broker_dns(), 100);
        assert!(bound_to(&recorded, &"ab".repeat(32), &absent).is_err());
    }

    #[test]
    fn nonce_mismatch_between_journal_and_ledger_is_refused() {
        let recorded = journal();
        let ours = ledger::Ledger::new(&recorded.generation, &recorded.nonce);
        ledger::belongs_to(&ours, &recorded.generation, &recorded.nonce).unwrap();
        // A ledger of another transaction of the same generation.
        let other = ledger::Ledger::new(&recorded.generation, &"6b".repeat(32));
        assert!(ledger::belongs_to(&other, &recorded.generation, &recorded.nonce).is_err());
        // Two runs never share a nonce (256 random bits each).
        let (first, second) = (random_hex(32).unwrap(), random_hex(32).unwrap());
        assert!(parse_generation(&first).is_some() && parse_generation(&second).is_some());
        assert_ne!(first, second);
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
        let fresh_nonce = "6b".repeat(32);
        let result = begin(Some(recorded.clone()), &options, &host(), &fresh_nonce, 5, |upstream, hosts| {
            asked = Some((upstream, hosts.to_vec()));
            Err(refused("S1 failed"))
        });
        assert!(result.is_err());
        assert_eq!(asked, Some((recorded.dns.upstream, recorded.dns.host_public_addresses.clone())));
        // Passing S1 with the recorded decision resumes the same journal (not a new one, its own nonce).
        let (resumed, fresh) = begin(Some(recorded.clone()), &options, &host(), &fresh_nonce, 5, |_, _| Ok(broker_dns())).unwrap();
        assert!(!fresh && resumed == recorded && resumed.nonce == nonce());
        // A host whose DNS decision changed since the first run refuses.
        let resolved = Dns { route: DnsRoute::ResolvedVarlink, upstream: None,
            host_public_addresses: broker_dns().host_public_addresses };
        assert!(begin(Some(recorded.clone()), &options, &host(), &fresh_nonce, 5, |_, _| Ok(resolved)).is_err());
        // Given options must equal the recorded ones.
        let changed = unit_options(Some("198.51.100.53:53".parse().unwrap()));
        assert!(begin(Some(recorded), &changed, &host(), &fresh_nonce, 5, |_, _| Ok(broker_dns())).is_err());
        // A fresh run records S1's decision, this host and the nonce generated at S0.
        let (created, fresh) = begin(None, &options, &host(), &fresh_nonce, 5, |_, _| Ok(broker_dns())).unwrap();
        assert!(fresh && created.host == host() && created.step == Step::HostPreflight && created.phase == Phase::Completed);
        assert_eq!(created.nonce, fresh_nonce);
        created.validate().unwrap();
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
duduclaw:x:990:990::/var/lib/duduclaw:/usr/sbin/nologin\n\
duduclaw-runsup:x:989:989::/nonexistent:/usr/sbin/nologin\n\
duduclaw-egress:x:988:988::/nonexistent:/usr/sbin/nologin\n\
duduclaw-dl:x:987:987::/nonexistent:/usr/sbin/nologin\n\
duduclaw-dns:x:986:986::/nonexistent:/usr/sbin/nologin\n\
duduclaw-selftest:x:985:985::/nonexistent:/usr/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nduduclaw:x:990:\nduduclaw-runsup:x:989:\nduduclaw-egress:x:988:\n\
duduclaw-dl:x:987:\nduduclaw-dns:x:986:\nduduclaw-selftest:x:985:\nsudo:x:27:alice\n";

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
            // R7a: the self-test account under the same rules (no extra group, home, shell, uid alias).
            (PASSWD.into(), GROUP.replace("sudo:x:27:alice", "sudo:x:27:alice,duduclaw-selftest")),
            (PASSWD.replace("985::/nonexistent", "985::/home/duduclaw-selftest"), GROUP.into()),
            (PASSWD.replace("985::/nonexistent:/usr/sbin/nologin", "985::/nonexistent:/bin/bash"), GROUP.into()),
            (format!("{PASSWD}shadow3:x:985:42::/nonexistent:/usr/sbin/nologin\n"), GROUP.into()),
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
        // R7a: the self-test account is a required service account once created (S4, S6 check points).
        assert!(all.contains(&duduclaw_isolation::ids::SELFTEST_USER));
        let without = PASSWD.replace("duduclaw-selftest:x:985:985::/nonexistent:/usr/sbin/nologin\n", "");
        let without_group = GROUP.replace("duduclaw-selftest:x:985:\n", "");
        assert!(identities_unaliased(&without, &without_group, &all).is_err(), "self-test account missing");
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
        assert_eq!(storage_chain(&mut sysfs, (253, 7)), Err(DMSETUP_REQUIRED));
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
    // `options("install", …)` passes `--enable-now`, which is accepted and inert.

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
        for name in SPAWND_ANCHORS {
            let metadata = fs::symlink_metadata(Path::new(SPAWND_ROOT).join(name)).unwrap();
            assert!(metadata.is_dir());
            assert_eq!((metadata.uid(), metadata.gid(), metadata.mode() & 0o7777), (0, 0, 0o700), "{name}");
        }
        let journal = journal_now().unwrap();
        assert!(journal.committed && journal.step == Step::Commit);
        assert_eq!(journal.host, host_digest().unwrap());
        // The creation ledger ends with the commit.
        assert!(ledger::read(&Store::open().unwrap()).unwrap().is_none());
        assert!(duduclaw_core::isolation_install::installed_under(Path::new("/")));
        readiness::check_platform_all(&mut readiness::InstalledComposite).unwrap();
        assert!(readiness::read_launcher_receipt().unwrap().is_some());
        assert_eq!(fs::read_to_string(UNIT_PATH).unwrap(), duduclaw_core::root_key::recovery::isolated_unit_text());
        readiness::unit_loaded_active(NIGHTLY_TIMER).unwrap();
        // The gateway runs and is enabled without `--enable-now` mattering.
        readiness::unit_loaded_active(UNIT).unwrap();
        unit_enabled(UNIT).unwrap();
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
        readiness::check_platform_all(&mut readiness::InstalledComposite).unwrap();
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
                readiness::check_platform_all(&mut readiness::InstalledComposite).unwrap();
            }
            // A re-run step refused (e.g. S6's evidence wait timed out): never committed.
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

#[cfg(test)]
mod parse_lease_preflight_tests {
    use super::*;

    #[test]
    fn preflight_without_systemd_tmpfiles_refuses() {
        assert!(required_tools_present(|_| true));
        assert!(!required_tools_present(|tool| tool != "systemd-tmpfiles"));
    }
}
