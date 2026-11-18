//! Installer checks and the composite isolation readiness (v1.83 R4).
//!
//! Inputs: root-owned files opened without following links (owner, mode,
//! link count and size checked), root-verified sockets (the updater's
//! authenticated probe, the DNS broker's fixed health request) and fixed
//! system tools. No argument or environment variable of a spawned tool is
//! attacker-influenced: `getent`/`id`/`systemctl` run through `command()`
//! with fixed arguments, a cleared environment (fixed `PATH`, `HOME`,
//! `LC_ALL`), `/` as working directory, a 30 s deadline and 4 MiB output
//! limits; their output is parsed, never echoed. Every read is bounded in time
//! and size. A source that cannot be read, an unexpected owner/mode, a
//! protocol mismatch or a timeout is "not ready" with a stable reason code,
//! never "ready". Nothing here publishes an isolation fact.
//!
//! Side effects that remain (all pre-existing, none in the composite sources
//! except the first and the last):
//! - `command()` creates two temporary output files under `/run` for each
//!   tool run and removes them afterwards;
//! - the item #1 `directories()` group calls `parse_runtime_tree(false)`, which
//!   takes the update lock (`/var/lib/duduclaw-updated/update.lock`, which the
//!   lock call may create) for the duration of that check;
//! - the DNS broker health probe makes the broker open (and immediately
//!   close) one TCP connection to its configured upstream.
use super::*;
use duduclaw_gateway::isolation_readiness::Reason;
use duduclaw_isolation::{ids::Sha256Digest, update::{Architecture, InstallTarget, Product, ReleaseArtifact,
    ReleaseManifest, ReleaseVersion}};
use duduclaw_migration::contract::MigrationState;
use duduclaw_spawnd::readiness::{EgressSecondLayer, Readiness as HostChecks};
use duduclaw_updated::readiness::GenerationEvidence;

/// The seam every call site uses. `assess` reports every composite finding;
/// `verify` is `Ok` only when none of them is a failure.
pub(super) trait CompositeReadiness {
    fn assess(&mut self) -> Vec<Finding>;
    // The pass/fail seam for the install orchestration (R3 S7); `check`
    // reports through `assess` instead.
    #[cfg_attr(not(test), allow(dead_code))]
    fn verify(&mut self) -> Result<()> {
        if self.assess().iter().any(Finding::is_failure) {
            return Err(refused("完整隔離就緒檢查未通過；尚未確認可安全執行員工工作。"));
        }
        Ok(())
    }
}

/// The installed host: plan §4.4 items 2–10 over the live, read-only sources.
pub(super) struct InstalledComposite;
impl CompositeReadiness for InstalledComposite {
    fn assess(&mut self) -> Vec<Finding> {
        assess_host(&mut LiveSources::default(), env!("CARGO_PKG_VERSION"))
    }
}

/// One composite item (plan §4.4 #2–#10; #1 is the four groups in [`check`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Item { UpdateGeneration, Components, HostEvidence, Landlock, EgressDns, ParseWorker, TrustRoot, Migration, Edition }

impl Item {
    const ALL: [Item; 9] = [Item::UpdateGeneration, Item::Components, Item::HostEvidence, Item::Landlock,
        Item::EgressDns, Item::ParseWorker, Item::TrustRoot, Item::Migration, Item::Edition];
    fn label(self) -> &'static str {
        match self {
            Item::UpdateGeneration => "更新服務世代",
            Item::Components => "隔離元件",
            Item::HostEvidence => "隔離執行服務的主機證據",
            Item::Landlock => "Landlock",
            Item::EgressDns => "對外連線與 DNS",
            Item::ParseWorker => "文件解析服務",
            Item::TrustRoot => "更新信任根",
            Item::Migration => "資料遷移",
            Item::Edition => "版本",
        }
    }
}

/// `Notice` and `Pending` never make the composite fail and never make it
/// pass: only the absence of every `Failed` finding does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Severity { Failed, Notice, Pending }

/// `code` is a stable ASCII identifier; `message` is fixed zh-TW text without
/// paths, account names or subprocess output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) item: Item,
    pub(super) severity: Severity,
    pub(super) code: &'static str,
    pub(super) message: String,
}

impl Finding {
    fn failed(item: Item, code: &'static str, message: &str) -> Self {
        Self { item, severity: Severity::Failed, code, message: message.to_owned() }
    }
    fn notice(item: Item, code: &'static str, message: String) -> Self {
        Self { item, severity: Severity::Notice, code, message }
    }
    fn is_failure(&self) -> bool { self.severity == Severity::Failed }
}

const MSG_UPDATE: &str = "更新服務的世代與安裝不一致，或更新服務沒有回應。";
const MSG_COMPONENTS: &str = "隔離元件與目前世代不符。";
const MSG_LAUNCHER: &str = "更新服務的固定啟動程式與安裝時記錄的版本不符。";
const MSG_LAUNCHER_RECEIPT: &str = "缺少安裝時記錄的更新服務啟動程式收據，無法確認啟動程式未被更換。";
const MSG_HOST_EVIDENCE: &str = "隔離執行服務的主機證據無法讀取、已過期或屬於其他開機。";
const MSG_HOST_PROTOCOL: &str = "隔離執行服務的協定版本與此版本不符。";
const MSG_LANDLOCK: &str = "此核心的 Landlock 自我測試未通過；Ubuntu 24.04 HWE 核心請改用 GA 核心或回報原廠。";
const MSG_LANDLOCK_UNVERIFIED: &str = "無法取得此核心的 Landlock 自我測試結果。";
const MSG_EGRESS: &str = "對外連線代理設定無效或未啟動。";
const MSG_DNS_BROKER: &str = "DNS 轉送服務的設定無效或未啟動。";
const MSG_DNS_BROKER_UNHEALTHY: &str = "DNS 轉送服務沒有在時限內回應健康檢查，或無法連到設定的上游 DNS。";
const MSG_SECOND_LAYER: &str = "第二層 IP 過濾不可用。";
const MSG_CLOUD_ADDRESS: &str = "偵測到雲端主機，但未設定主機的公開位址；若主機使用 1:1 NAT，員工可能連得到本機的公開 IP。";
const MSG_PARSE: &str = "文件解析服務未就緒。";
const MSG_TRUST: &str = "更新信任根無法驗證，需要人工修復。";
const MSG_TRUST_FROZEN: &str = "更新信任根已凍結；解除凍結前不會套用新版本。";
const MSG_MIGRATION_UNREADABLE: &str = "資料遷移進度無法驗證。";
const MSG_MIGRATION_INCOMPLETE: &str = "資料遷移尚未完成隔離驗證；請先完成遷移流程。";
const MSG_MIGRATION_COMPLETION: &str = "資料遷移的內容已匯入，但最終完成檢查尚未通過；請再次執行遷移流程。";
const MSG_MIGRATION_IN_PROGRESS: &str = "資料遷移有尚未完成的步驟；請再次執行遷移流程。";
const MSG_MIGRATION_RECEIPTS: &str = "資料遷移的步驟紀錄不完整或無效，需要人工檢查。";
const MSG_MIGRATION_PLAN_MISSING: &str = "偵測到資料遷移的檔案，但缺少遷移計畫，需要人工檢查。";
const MSG_MIGRATION_LEGACY: &str = "資料遷移紀錄為舊版格式；請繼續執行遷移流程，將遷移紀錄更新為目前格式。";
const MSG_EDITION: &str = "此版本不支援員工隔離。";
const MSG_EDITION_UNVERIFIED: &str = "無法確認團隊版授權；員工隔離維持停用。";
const MSG_READY: &str = "所有就緒檢查均已通過，可以執行員工工作。";

/// The updater's committed state, reduced to what readiness compares.
#[derive(Clone, Copy, Debug)]
pub(super) struct UpdateStateView {
    committed: Sha256Digest,
    pending: bool,
    maintenance: bool,
    /// An emergency fallback runs another generation than the committed one.
    running_other: bool,
    product: Product,
    arch: Architecture,
    floor: ReleaseVersion,
}

/// spawnd's published host evidence: the combined verdict of
/// `EvidenceReport::check` (freshness, boot, budget, the Landlock binding and
/// every boolean), the boolean checks and Landlock `(detected_abi, selected_abi)`.
#[derive(Clone, Debug)]
pub(super) struct HostEvidence {
    verdict_ok: bool,
    checks: HostChecks,
    landlock: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EgressView { host_public_addresses_empty: bool }

#[derive(Clone, Copy, Debug)]
pub(super) struct TrustView { expires_at_unix: u64, frozen: bool }

/// The frozen updater launcher as recorded at bootstrap in a root-owned
/// receipt (never the current generation's `UpdateSlot` artifact: only the
/// bootstrap/transition program replaces the launcher, so a legitimate update
/// of `duduclaw-updated` must not change this comparison).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LauncherReceipt { sha256: Sha256Digest, size_bytes: u64, mode: u32 }

/// The fixed updater launcher path (`layout::BINARIES`).
pub(super) const UPDATER_LAUNCHER: &str = "/usr/libexec/duduclaw-updated-launch";

/// `/etc/duduclaw-updated/launcher-receipt.json`: written once by the v1.83
/// isolation bootstrap (R3 S4, `protected_linux_bootstrap.rs`) after it
/// published the frozen launcher from the verified bootstrap generation's
/// `UpdateSlot` artifact. Root 0600, one link, at most
/// [`MAX_LAUNCHER_RECEIPT_BYTES`], read through the update-service config
/// root's verified dirfd (no symlinks in any component). It lives in an
/// isolation-only root directory the gateway account cannot write, and is
/// removed with it by the bootstrap rollback.
pub(super) const LAUNCHER_RECEIPT_FILE: &str = "launcher-receipt.json";
pub(super) const MAX_LAUNCHER_RECEIPT_BYTES: usize = 4096;

/// On-disk form of [`LauncherReceipt`]. `generation` is the bootstrap
/// generation the launcher bytes were taken from (provenance only).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
// `installed_at_unix` is provenance for the administrator, not a readiness input.
#[allow(dead_code)]
pub(super) struct LauncherReceiptRecord {
    pub(super) schema_version: u16,
    pub(super) generation: Sha256Digest,
    pub(super) sha256: Sha256Digest,
    pub(super) size_bytes: u64,
    pub(super) mode: u32,
    pub(super) installed_at_unix: u64,
}

impl LauncherReceiptRecord {
    /// Strict: bounded, `deny_unknown_fields`, schema 1, a non-empty file and
    /// exactly the 0755 mode every fixed-path binary has (`layout`).
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_LAUNCHER_RECEIPT_BYTES { return Err(refused("launcher receipt")); }
        let record: Self = serde_json::from_slice(bytes).map_err(|_| refused("launcher receipt"))?;
        if record.schema_version != 1 || record.size_bytes == 0 || record.mode != 0o755
            || record.sha256.0 == [0; 32] || record.generation.0 == [0; 32] {
            return Err(refused("launcher receipt"));
        }
        Ok(record)
    }
    pub(super) fn receipt(&self) -> LauncherReceipt {
        LauncherReceipt { sha256: self.sha256, size_bytes: self.size_bytes, mode: self.mode }
    }
}

/// Read-only data sources of the composite check (plan §4.4 data sources).
/// Production reads root-owned state only; tests inject fakes.
pub(super) trait HostSources {
    /// Authenticated F probe (root peer, fresh nonce, 5 s socket timeouts).
    fn update_probe(&mut self) -> Result<GenerationEvidence>;
    fn update_state(&mut self) -> Result<UpdateStateView>;
    /// The root-owned `current` selector.
    fn current_generation(&mut self) -> Result<Sha256Digest>;
    /// The generation's manifest, authenticated through the trust ledger.
    fn generation_manifest(&mut self, generation: Sha256Digest) -> Result<ReleaseManifest>;
    /// The published file at `path` is root-owned, nlink 1, has the artifact's
    /// mode, size and SHA-256.
    fn component(&mut self, path: &'static str, artifact: &ReleaseArtifact) -> Result<()>;
    /// The bootstrap's root-owned record of the frozen updater launcher.
    /// `Ok(None)`: no such receipt exists on this host.
    fn launcher_receipt(&mut self) -> Result<Option<LauncherReceipt>>;
    /// The launcher at [`UPDATER_LAUNCHER`] is root-owned, nlink 1, has the
    /// receipt's mode, size and SHA-256 (compared by descriptor).
    fn launcher(&mut self, receipt: &LauncherReceipt) -> Result<()>;
    fn host_evidence(&mut self) -> Result<HostEvidence>;
    /// Loaded from the installed fragment, active, no unverified drop-ins.
    fn unit_active(&mut self, unit: &'static str) -> Result<()>;
    fn egress_config(&mut self) -> Result<EgressView>;
    /// `Ok(true)`: the broker DNS route is configured and its config passes
    /// the broker's own validator (`duduclaw_egress::daemon::BrokerConfig`).
    fn dns_broker_configured(&mut self) -> Result<bool>;
    /// The broker answered its fixed health request within 2 s.
    fn dns_broker_health(&mut self) -> Result<()>;
    /// Heuristic for an informational notice only; never a security input.
    fn cloud_host(&mut self) -> bool;
    fn trust_root(&mut self) -> Result<TrustView>;
    fn now(&mut self) -> Result<u64>;
    /// `Ok(None)`: no migration on this host. `Ok(Some(state))`: the verified
    /// journal state with no pending intent and every stage receipt up to it
    /// (`duduclaw_migration::root_host::readiness`). `Err(code)`: a stable
    /// reason code of why it cannot be reported.
    fn migration_state(&mut self) -> std::result::Result<Option<MigrationState>, &'static str>;
    /// `Ok(true)` only for a verified TEAM (non-Personal) license.
    fn edition_is_team(&mut self) -> Result<bool>;
}

/// The one rule shared with the gateway's update admission.
fn compatible_update(evidence: &GenerationEvidence, installed_version: &str) -> bool {
    duduclaw_updated::readiness::compatible_update(evidence, installed_version)
}

/// Plan §4.4 items 2–10, each reported on its own; every item always runs.
pub(super) fn assess_host(sources: &mut dyn HostSources, installed_version: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    update_findings(sources, installed_version, &mut out);
    component_findings(sources, &mut out);
    launcher_findings(sources, &mut out);
    let evidence = host_findings(sources, &mut out);
    egress_findings(sources, evidence.as_ref(), &mut out);
    parse_findings(sources, &mut out);
    trust_findings(sources, &mut out);
    migration_findings(sources, &mut out);
    edition_findings(sources, &mut out);
    out
}

// #2: F's live probe, the selector and the committed state name one generation.
fn update_findings(sources: &mut dyn HostSources, installed_version: &str, out: &mut Vec<Finding>) {
    let item = Item::UpdateGeneration;
    let state = sources.update_state();
    if state.is_err() { out.push(Finding::failed(item, "update_state_unreadable", MSG_UPDATE)); }
    let current = sources.current_generation();
    if current.is_err() { out.push(Finding::failed(item, "update_selector_unreadable", MSG_UPDATE)); }
    match sources.update_probe() {
        Err(_) => out.push(Finding::failed(item, "update_probe_failed", MSG_UPDATE)),
        Ok(evidence) if !compatible_update(&evidence, installed_version) =>
            out.push(Finding::failed(item, "update_incompatible", MSG_UPDATE)),
        Ok(evidence) => if let (Ok(state), Ok(current)) = (state, current) {
            if evidence.generation != current || current != state.committed || evidence.product != state.product
                || state.pending || state.maintenance || state.running_other {
                out.push(Finding::failed(item, "update_generation_mismatch", MSG_UPDATE));
            }
        },
    }
}

/// Every fixed-path executable the installer layout publishes that follows
/// the current generation, plus the gateway. The frozen updater launcher is
/// not one of them: [`launcher_findings`] compares it with its bootstrap receipt.
fn component_paths() -> impl Iterator<Item = &'static str> {
    layout::BINARIES.iter().copied().filter(|path| *path != UPDATER_LAUNCHER).chain(std::iter::once(BINARY))
}

/// The signed artifact of the current generation a fixed-path executable must
/// equal. Exact paths only; the frozen launcher has none.
pub(super) fn component_target(path: &str) -> Option<InstallTarget> {
    Some(match path {
        "/usr/lib/duduclaw/duduclaw-spawnd" => InstallTarget::Spawnd,
        "/usr/lib/duduclaw/duduclaw-run" => InstallTarget::Trampoline,
        "/usr/lib/duduclaw/duduclaw-run-selftest" => InstallTarget::TrampolineSelftest,
        "/usr/lib/duduclaw/duduclaw-isolation-probe" => InstallTarget::IsolationProbe,
        "/opt/duduclaw/bin/duduclaw-egress" => InstallTarget::Egress,
        "/usr/libexec/duduclaw/duduclaw-parse-worker" => InstallTarget::ParseWorker,
        "/usr/lib/duduclaw/current/duduclaw-import" => InstallTarget::Importer,
        "/usr/lib/duduclaw/current/duduclaw-migrate" => InstallTarget::Migration,
        "/usr/lib/duduclaw/current/duduclaw-mcp-bridge" => InstallTarget::McpBridge,
        "/usr/lib/duduclaw/current/duduclaw" => InstallTarget::Gateway,
        BINARY => InstallTarget::Gateway,
        _ => return None,
    })
}

// #3, frozen launcher: equal to what the bootstrap recorded, whatever the
// current generation is. No receipt is a failure, never "matches".
fn launcher_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) {
    let item = Item::Components;
    match sources.launcher_receipt() {
        Err(_) => out.push(Finding::failed(item, "launcher_receipt_unreadable", MSG_LAUNCHER_RECEIPT)),
        Ok(None) => out.push(Finding::failed(item, "launcher_receipt_missing", MSG_LAUNCHER_RECEIPT)),
        Ok(Some(receipt)) => if sources.launcher(&receipt).is_err() {
            out.push(Finding::failed(item, "launcher_mismatch", MSG_LAUNCHER));
        },
    }
}

/// The authenticated manifest of the selected generation, only while it is
/// the committed, non-emergency generation of the recorded product.
fn current_manifest(sources: &mut dyn HostSources) -> Result<ReleaseManifest> {
    let current = sources.current_generation()?;
    let state = sources.update_state()?;
    let manifest = sources.generation_manifest(current)?;
    if current != state.committed || state.running_other || manifest.product != state.product
        || manifest.arch != state.arch || manifest.version != state.floor {
        return Err(refused("generation"));
    }
    Ok(manifest)
}

// #3 (and #7's executable): each published file equals the signed artifact.
fn component_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) {
    let manifest = match current_manifest(sources) {
        Ok(manifest) => manifest,
        Err(_) => {
            out.push(Finding::failed(Item::Components, "generation_manifest_unverified", MSG_COMPONENTS));
            out.push(Finding::failed(Item::ParseWorker, "parse_worker_unverified", MSG_PARSE));
            return;
        }
    };
    let mut mismatch = false;
    for path in component_paths() {
        let Some(target) = component_target(path) else {
            out.push(Finding::failed(Item::Components, "component_unmapped", MSG_COMPONENTS));
            continue;
        };
        let mut matching = manifest.artifacts.iter().filter(|artifact| artifact.install_target == target);
        let verified = match (matching.next(), matching.next()) {
            (Some(artifact), None) => sources.component(path, artifact).is_ok(),
            _ => false,
        };
        if !verified {
            mismatch = true;
            if target == InstallTarget::ParseWorker {
                out.push(Finding::failed(Item::ParseWorker, "parse_worker_mismatch", MSG_PARSE));
            }
        }
    }
    if mismatch { out.push(Finding::failed(Item::Components, "component_mismatch", MSG_COMPONENTS)); }
}

/// spawnd's primary checks except Landlock (#5 reports that one).
fn host_checks(checks: &HostChecks) -> [(bool, &'static str, &'static str); 13] {
    [
        (checks.registry_verified, "host_registry_unverified", "員工身分登錄未通過驗證。"),
        (checks.accounts_locked, "host_accounts_unlocked", "員工帳號尚未全部鎖定。"),
        (checks.ssh_live_negative_checks, "host_ssh_lockout_unverified", "員工帳號的 SSH 登入封鎖未通過實測。"),
        (checks.cron_at_acl_and_jobs_verified, "host_cron_at_unverified", "員工帳號的 cron／at 封鎖未通過驗證。"),
        (checks.external_iam_reservation_verified, "host_external_iam_unverified", "外部帳號服務（LDAP／SSSD）的員工 uid 保留範圍未確認。"),
        (checks.trampoline_verified, "host_trampoline_unverified", "員工啟動程式未通過驗證。"),
        (checks.namespace_self_test, "host_namespace_self_test_failed", "此核心的命名空間隔離自我測試未通過。"),
        (checks.egress_verified, "host_egress_unverified", "對外連線代理未通過主機驗證。"),
        (checks.dns_route_verified, "host_dns_route_unverified", "DNS 解析路徑未通過驗證。"),
        (checks.storage_verified, "host_storage_unverified", "員工儲存空間未通過驗證。"),
        (checks.resources_verified, "host_resources_unverified", "資源限制未通過驗證。"),
        (checks.startup_reconciled, "host_startup_unreconciled", "啟動時的執行紀錄對帳尚未完成。"),
        (checks.process_scan_verified, "host_process_scan_unverified", "員工程序掃描未通過。"),
    ]
}

/// The administrator text of every primary check `checks` still reports
/// false, in the composite's wording ([`host_checks`], and #5's Landlock and
/// the protocol line). Used by the bootstrap's S6 refusal (C4).
pub(super) fn unmet_host_checks(checks: &HostChecks) -> Vec<&'static str> {
    let mut unmet = Vec::new();
    if checks.protocol != duduclaw_isolation::ipc::PROTOCOL_VERSION { unmet.push(MSG_HOST_PROTOCOL); }
    unmet.extend(host_checks(checks).into_iter().filter(|(passed, _, _)| !passed).map(|(_, _, message)| message));
    if !checks.landlock_self_test { unmet.push(MSG_LANDLOCK); }
    unmet
}

// #4 and #5. Returns the evidence for #6's informational second layer.
fn host_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) -> Option<HostEvidence> {
    let evidence = match sources.host_evidence() {
        Ok(evidence) => evidence,
        Err(_) => {
            out.push(Finding::failed(Item::HostEvidence, "host_evidence_unavailable", MSG_HOST_EVIDENCE));
            out.push(Finding::failed(Item::Landlock, "landlock_unverified", MSG_LANDLOCK_UNVERIFIED));
            return None;
        }
    };
    if evidence.checks.protocol != duduclaw_isolation::ipc::PROTOCOL_VERSION {
        out.push(Finding::failed(Item::HostEvidence, "host_protocol_mismatch", MSG_HOST_PROTOCOL));
    }
    for (passed, code, message) in host_checks(&evidence.checks) {
        if !passed { out.push(Finding::failed(Item::HostEvidence, code, message)); }
    }
    // Q4 working default: a newer kernel is accepted only through spawnd's
    // existing clamp (selected = min(detected, highest known)) with a passing
    // self test. Required even when the trampoline check itself failed. The
    // highest known ABI is the shared contract constant spawnd's clamp uses.
    let highest = duduclaw_isolation::LANDLOCK_HIGHEST_KNOWN_ABI;
    let clamp_valid = evidence.landlock
        .is_some_and(|(detected, selected)| detected > 0 && selected == detected.min(highest));
    if !evidence.checks.landlock_self_test || !clamp_valid {
        out.push(Finding::failed(Item::Landlock, "landlock_self_test_failed", MSG_LANDLOCK));
    } else if let (Some(Reason::LandlockAbiClamped), Some((detected, _))) =
        (Reason::landlock_notice(evidence.landlock), evidence.landlock) {
        out.push(Finding::notice(Item::Landlock, Reason::LANDLOCK_ABI_CLAMPED_CODE,
            Reason::landlock_abi_clamped_message(detected)));
    }
    // spawnd's combined verdict (stale, future, other-boot or zero-budget
    // evidence included) is always reported when it fails, even when every
    // boolean reads true and even alongside other named failures: another
    // failing item must never hide stale evidence.
    if !evidence.verdict_ok {
        out.push(Finding::failed(Item::HostEvidence, "host_evidence_stale", MSG_HOST_EVIDENCE));
    }
    Some(evidence)
}

// #6: proxy units and root configs; second layer and NAT are notices only.
fn egress_findings(sources: &mut dyn HostSources, evidence: Option<&HostEvidence>, out: &mut Vec<Finding>) {
    let item = Item::EgressDns;
    for unit in ["duduclaw-egress.socket", "duduclaw-egress.service"] {
        if sources.unit_active(unit).is_err() {
            out.push(Finding::failed(item, "egress_inactive", MSG_EGRESS));
            break;
        }
    }
    match sources.egress_config() {
        Err(_) => out.push(Finding::failed(item, "egress_config_invalid", MSG_EGRESS)),
        Ok(view) => if view.host_public_addresses_empty && sources.cloud_host() {
            out.push(Finding::notice(item, "cloud_public_address_unset", MSG_CLOUD_ADDRESS.to_owned()));
        },
    }
    match sources.dns_broker_configured() {
        Err(_) => out.push(Finding::failed(item, "dns_broker_config_invalid", MSG_DNS_BROKER)),
        Ok(true) => {
            if sources.unit_active("duduclaw-dns-broker.socket").is_err() {
                out.push(Finding::failed(item, "dns_broker_inactive", MSG_DNS_BROKER));
            }
            // An active socket proves nothing about the broker behind it:
            // the broker itself must answer its fixed health request.
            if sources.dns_broker_health().is_err() {
                out.push(Finding::failed(item, "dns_broker_unhealthy", MSG_DNS_BROKER_UNHEALTHY));
            }
        }
        // The resolved route: spawnd's dns_route_verified (#4) covers it.
        Ok(false) => (),
    }
    if evidence.is_some_and(|evidence| evidence.checks.egress_second_layer != EgressSecondLayer::Verified) {
        out.push(Finding::notice(item, "egress_second_layer_unavailable", MSG_SECOND_LAYER.to_owned()));
    }
}

// #7: the socket; the executable is compared in #3.
fn parse_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) {
    if sources.unit_active("duduclaw-parse.socket").is_err() {
        out.push(Finding::failed(Item::ParseWorker, "parse_worker_inactive", MSG_PARSE));
    }
}

// #8: the ledger's current root verifies and has not expired.
fn trust_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) {
    let item = Item::TrustRoot;
    match (sources.trust_root(), sources.now()) {
        (Ok(root), Ok(now)) if root.expires_at_unix <= now =>
            out.push(Finding::failed(item, "trust_root_expired", MSG_TRUST)),
        (Ok(root), Ok(_)) => if root.frozen {
            out.push(Finding::notice(item, "trust_root_frozen", MSG_TRUST_FROZEN.to_owned()));
        },
        _ => out.push(Finding::failed(item, "trust_root_unverified", MSG_TRUST)),
    }
}

// #9 (design §10.3). Only `AwaitingAdministrator` is "pending" (not a
// failure): the host is ready and unconfirmed employees are refused by the
// gateway. The source reports a state only with no pending intent and every
// stage receipt up to it. `ContentImported` blocks until the final live
// readback of `Complete` succeeded (failed applies, outstanding rotations or
// a missing isolation readback stop it there). Every other state, a pending
// intent, missing receipts and artefacts without a plan block as well.
fn migration_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) {
    let item = Item::Migration;
    match sources.migration_state() {
        Ok(None) | Ok(Some(MigrationState::Complete)) => (),
        Ok(Some(MigrationState::AwaitingAdministrator)) =>
            out.push(Finding { item, severity: Severity::Pending, code: Reason::MIGRATION_PENDING_CODE,
                message: Reason::MIGRATION_PENDING_MESSAGE.to_owned() }),
        Ok(Some(MigrationState::ContentImported)) =>
            out.push(Finding::failed(item, "migration_completion_unverified", MSG_MIGRATION_COMPLETION)),
        Ok(Some(_)) => out.push(Finding::failed(item, "migration_incomplete", MSG_MIGRATION_INCOMPLETE)),
        Err(code) => {
            let (code, message) = migration_error(code);
            out.push(Finding::failed(item, code, message));
        }
    }
}

/// A source's reason code and its fixed message. Unknown codes collapse to
/// the generic unreadable one, so a finding code is always from this list.
fn migration_error(code: &str) -> (&'static str, &'static str) {
    match code {
        "migration_intent_pending" => ("migration_intent_pending", MSG_MIGRATION_IN_PROGRESS),
        "migration_receipts_invalid" => ("migration_receipts_invalid", MSG_MIGRATION_RECEIPTS),
        "migration_journal_legacy" => ("migration_journal_legacy", MSG_MIGRATION_LEGACY),
        "migration_plan_missing" => ("migration_plan_missing", MSG_MIGRATION_PLAN_MISSING),
        "migration_plan_invalid" => ("migration_plan_invalid", MSG_MIGRATION_UNREADABLE),
        "migration_state_dir_unsafe" => ("migration_state_dir_unsafe", MSG_MIGRATION_UNREADABLE),
        "migration_journal_missing" => ("migration_journal_missing", MSG_MIGRATION_UNREADABLE),
        "migration_journal_unsafe" => ("migration_journal_unsafe", MSG_MIGRATION_UNREADABLE),
        "migration_journal_oversized" => ("migration_journal_oversized", MSG_MIGRATION_UNREADABLE),
        "migration_journal_sidecar" => ("migration_journal_sidecar", MSG_MIGRATION_IN_PROGRESS),
        "migration_journal_changed" => ("migration_journal_changed", MSG_MIGRATION_IN_PROGRESS),
        "migration_journal_schema" => ("migration_journal_schema", MSG_MIGRATION_UNREADABLE),
        "migration_journal_unreadable" => ("migration_journal_unreadable", MSG_MIGRATION_UNREADABLE),
        "migration_journal_conflict" => ("migration_journal_conflict", MSG_MIGRATION_UNREADABLE),
        _ => ("migration_state_unreadable", MSG_MIGRATION_UNREADABLE),
    }
}

// #10 (Q9 working default A): isolation is installed for TEAM only.
fn edition_findings(sources: &mut dyn HostSources, out: &mut Vec<Finding>) {
    match sources.edition_is_team() {
        Ok(true) => (),
        Ok(false) => out.push(Finding::failed(Item::Edition, "edition_not_team", MSG_EDITION)),
        Err(_) => out.push(Finding::failed(Item::Edition, "edition_unverified", MSG_EDITION_UNVERIFIED)),
    }
}

/// Exact DMI values of common public clouds. Notice-only heuristic: a miss or
/// a false positive changes no verdict.
fn cloud_dmi_value(value: &str) -> bool {
    matches!(value, "Google" | "Google Compute Engine" | "Amazon EC2" | "DigitalOcean" | "Alibaba Cloud"
        | "Alibaba Cloud ECS" | "OracleCloud.com" | "Hetzner" | "Linode" | "Akamai" | "Vultr" | "Tencent Cloud"
        | "OpenStack Foundation" | "OpenStack Nova" | "Microsoft Corporation")
}

// ── Live sources ──────────────────────────────────────────────────────────

/// Fixed install locations (they match `protected_linux_migration.rs` and the
/// protected launcher's HOME); nothing is taken from the environment.
const MIGRATION_STATE_DIR: &str = "/var/lib/duduclaw-migrate";
const MIGRATION_LEGACY_HOME: &str = "/var/lib/duduclaw/.duduclaw";
const MIGRATION_ROOT_KEY: &str = "/var/lib/duduclaw-protected/root.key";
const GATEWAY_HOME: &str = "/var/lib/duduclaw/.duduclaw";
/// The whole DNS broker health exchange, connect included.
const DNS_BROKER_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// A protected Linux install is always self-hosted: the edition rules are
/// never chosen from this process's environment (root's shell could carry a
/// deployment-type variable the systemd gateway does not have).
const PROTECTED_LINUX_SELF_HOST: bool = true;

/// The tier part of the edition rule: allowed on a self-hosted deployment and
/// not a Personal profile. Pure; the signature, machine binding, grace period
/// and revocation are checked by the caller.
fn license_admits_isolation(license: &duduclaw_license::License) -> bool {
    license.validate_tier_deployment_binding(PROTECTED_LINUX_SELF_HOST).is_ok()
        && !duduclaw_core::EditionProfile::from_tier_key(license.tier.as_toml_key()).is_personal()
}

#[derive(Default)]
struct LiveSources { gateway: Option<(u32, u32)> }

impl LiveSources {
    fn gateway_ids(&mut self) -> Result<(u32, u32)> {
        if let Some(ids) = self.gateway { return Ok(ids); }
        let output = checked("getent", &["passwd", "duduclaw"])?;
        let text = String::from_utf8(output.stdout).map_err(|_| refused("無法讀取服務帳號。"))?;
        let fields: Vec<_> = text.trim_end_matches('\n').split(':').collect();
        if fields.len() != 7 || fields[0] != "duduclaw" { return Err(refused("無法讀取服務帳號。")); }
        let uid = fields[2].parse::<u32>().map_err(|_| refused("無效的服務身分。"))?;
        let gid = fields[3].parse::<u32>().map_err(|_| refused("無效的服務群組。"))?;
        if uid == 0 || gid == 0 { return Err(refused("無效的服務身分。")); }
        self.gateway = Some((uid, gid));
        Ok((uid, gid))
    }
}

/// At most `limit` bytes of a regular file opened without following links.
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > limit {
        return Err(refused("unexpected file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit { return Err(refused("unexpected file")); }
    Ok(bytes)
}

fn absent(path: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

impl HostSources for LiveSources {
    fn update_probe(&mut self) -> Result<GenerationEvidence> {
        let nonce = duduclaw_isolation::ids::RequestId(*uuid::Uuid::new_v4().as_bytes());
        duduclaw_updated::client::probe(nonce).map_err(refused)
    }

    fn update_state(&mut self) -> Result<UpdateStateView> {
        use duduclaw_updated::linux::{files::Directory, STATE_ROOT};
        // Atomic snapshot read; the update lock is never taken (it may create a file).
        let state = Directory::open(Path::new(STATE_ROOT))
            .and_then(|directory| directory.json::<duduclaw_updated::state::State>("state.json"))
            .map_err(refused)?;
        state.validate().map_err(refused)?;
        Ok(UpdateStateView { committed: state.committed_digest, pending: state.pending.is_some(),
            maintenance: state.maintenance, running_other: state.running_generation.is_some(),
            product: state.product, arch: state.arch, floor: state.floor })
    }

    fn current_generation(&mut self) -> Result<Sha256Digest> {
        duduclaw_updated::linux::activation::read_current().map_err(refused)
    }

    fn generation_manifest(&mut self, generation: Sha256Digest) -> Result<ReleaseManifest> {
        authenticated_manifest(generation)
    }

    fn component(&mut self, path: &'static str, artifact: &ReleaseArtifact) -> Result<()> {
        let path = Path::new(path);
        parents(path)?;
        trusted(path, false)?;
        let file = fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)?;
        if file.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) {
            return Err(refused("component mode"));
        }
        duduclaw_updated::linux::files::verify_fd(file, artifact.size_bytes, artifact.sha256).map_err(refused)?;
        Ok(())
    }

    // The bootstrap's receipt (R3 S4, see `LauncherReceiptRecord`): neither
    // R1's `verified.json` (deleted with the staging area at commit) nor
    // `slot-a.json` (changes with every update) can stand in for it. Only a
    // missing file or a missing update-service config root is "absent"; any
    // other error, owner, mode, link count, size or content refuses.
    fn launcher_receipt(&mut self) -> Result<Option<LauncherReceipt>> { read_launcher_receipt() }

    fn launcher(&mut self, receipt: &LauncherReceipt) -> Result<()> {
        let path = Path::new(UPDATER_LAUNCHER);
        parents(path)?;
        trusted(path, false)?;
        let file = fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != 0 || metadata.nlink() != 1 || metadata.mode() & 0o7777 != receipt.mode {
            return Err(refused("launcher"));
        }
        duduclaw_updated::linux::files::verify_fd(file, receipt.size_bytes, receipt.sha256).map_err(refused)?;
        Ok(())
    }

    fn host_evidence(&mut self) -> Result<HostEvidence> {
        let (_, gid) = self.gateway_ids()?;
        let report = duduclaw_spawnd::host::EvidenceReport::read(gid)?;
        let verdict_ok = report.check().is_ok();
        let landlock = report.landlock.as_ref().map(|landlock| (landlock.detected_abi, landlock.selected_abi));
        Ok(HostEvidence { verdict_ok, landlock, checks: report.checks })
    }

    fn unit_active(&mut self, unit: &'static str) -> Result<()> { unit_loaded_active(unit) }

    fn egress_config(&mut self) -> Result<EgressView> {
        use duduclaw_egress::daemon::{read_root_config, Config, CONFIG_PATH};
        let config: Config = read_root_config(CONFIG_PATH).map_err(|_| refused("egress config"))?;
        config.validate().map_err(|_| refused("egress config"))?;
        Ok(EgressView { host_public_addresses_empty: config.host_public_addresses.is_empty() })
    }

    // The broker's own config type and its single validator: whatever the
    // broker refuses at start is refused here.
    fn dns_broker_configured(&mut self) -> Result<bool> {
        use duduclaw_egress::daemon::{read_root_config, BrokerConfig, BROKER_CONFIG_PATH};
        if absent(BROKER_CONFIG_PATH)? { return Ok(false); }
        let config: BrokerConfig = read_root_config(BROKER_CONFIG_PATH).map_err(|_| refused("dns broker config"))?;
        config.validate().map_err(|_| refused("dns broker config"))?;
        Ok(true)
    }

    fn dns_broker_health(&mut self) -> Result<()> {
        duduclaw_egress::resolver::probe_dns_broker_health(DNS_BROKER_HEALTH_TIMEOUT)
            .map_err(|_| refused("dns broker health"))
    }

    fn cloud_host(&mut self) -> bool {
        use std::io::Read;
        // sysfs reports a fixed 4096-byte size, so only the read is bounded.
        ["sys_vendor", "product_name", "bios_vendor", "chassis_asset_tag"].into_iter().any(|name| {
            let mut bytes = Vec::new();
            fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(Path::new("/sys/class/dmi/id").join(name))
                .and_then(|file| file.take(256).read_to_end(&mut bytes)).is_ok()
                && std::str::from_utf8(&bytes).is_ok_and(|value| cloud_dmi_value(value.trim()))
        })
    }

    fn trust_root(&mut self) -> Result<TrustView> {
        let root = duduclaw_updated::linux::trust_admin::load_current().map_err(refused)?;
        Ok(TrustView { expires_at_unix: root.expires_at_unix, frozen: root.frozen })
    }

    fn now(&mut self) -> Result<u64> { duduclaw_updated::linux::now().map_err(refused) }

    // Never `RootMigration::open`: it takes the migration lock and creates the
    // plan, lock file and journal. The migration crate's one read-only
    // verifier opens everything by descriptor and the journal immutable.
    fn migration_state(&mut self) -> std::result::Result<Option<MigrationState>, &'static str> {
        use duduclaw_migration::root_host::readiness::{verify_read_only, ExpectedLayout};
        verify_read_only(&ExpectedLayout { state_dir: Path::new(MIGRATION_STATE_DIR),
            legacy_home: Path::new(MIGRATION_LEGACY_HOME), root_key_path: Path::new(MIGRATION_ROOT_KEY) })
            .map_err(|error| error.code())
    }

    // The gateway's own rule (license_runtime): signed by a trusted issuer,
    // bound to this machine, within grace, tier allowed for a self-hosted
    // deployment (always: `PROTECTED_LINUX_SELF_HOST`, never the environment),
    // no revocation marker. Read-only: the license lock is never taken (it
    // would create a root-owned file in the gateway home). The gateway account
    // cannot forge a TEAM license (issuer signature, machine binding).
    //
    // KNOWN LIMITATION (decision for the owner / package R3): the license file
    // and its revocation marker live in the gateway-owned home, so the gateway
    // account can swap in another validly signed TEAM license bound to this
    // machine, or delete the revocation marker. Closing this needs a
    // root-owned entitlement receipt written by the installer; no receipt
    // format is invented here and this check otherwise stays as is.
    fn edition_is_team(&mut self) -> Result<bool> {
        use duduclaw_license::storage;
        let home = Path::new(GATEWAY_HOME);
        let parent = home.parent().ok_or_else(|| refused("gateway home"))?;
        parents(parent)?;
        trusted(parent, true)?;
        let metadata = fs::symlink_metadata(home)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() { return Err(refused("gateway home")); }
        match fs::symlink_metadata(home.join(storage::REVOKED_FILENAME)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Ok(_) => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        // No license: the gateway runs OpenSource, i.e. Personal.
        let license_path = home.join(storage::DEFAULT_LICENSE_FILENAME);
        match fs::symlink_metadata(&license_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
            Ok(_) => (),
        }
        let bytes = read_bounded(&license_path, MAX_LICENSE_BYTES)?;
        license_bytes_admit_isolation(&bytes)
    }
}

/// Size bound of a license file read by the installer and this check.
pub(super) const MAX_LICENSE_BYTES: u64 = 64 * 1024;

/// The gateway's own license rule over license bytes (signed by a trusted
/// issuer, bound to this machine, within grace, a tier allowed for a
/// self-hosted deployment and not Personal). Malformed JSON is "not TEAM".
/// Shared by item #10 and the bootstrap's preflight (R3 S1, Q9), so both use
/// one rule.
pub(super) fn license_bytes_admit_isolation(bytes: &[u8]) -> Result<bool> {
    use duduclaw_license::{fingerprint_candidates, FeatureGate, License, EMBEDDED_FEATURES_TOML};
    let Ok(license) = serde_json::from_slice::<License>(bytes) else { return Ok(false) };
    let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).map_err(|_| refused("license policy"))?;
    let valid = duduclaw_gateway::license_runtime::production_registry().verify(&license).is_ok()
        && license.validate_any(&fingerprint_candidates(), gate.phone_home_interval_days(license.tier),
            gate.grace_period_days(license.tier)).is_ok();
    Ok(valid && license_admits_isolation(&license))
}

/// The manifest of `generation`, authenticated through the trust ledger (same
/// authentication as `parse_runtime_tree`, without the update lock).
pub(super) fn authenticated_manifest(generation: Sha256Digest) -> Result<ReleaseManifest> {
    use duduclaw_updated::linux::{files::Directory, trust_admin, INSTALL_ROOT};
    let directory = Directory::open(Path::new(INSTALL_ROOT)).map_err(refused)?
        .child("generations").map_err(refused)?.child(&hex::encode(generation.0)).map_err(refused)?;
    let bytes = directory.bytes("manifest.json", duduclaw_updated::metadata::MAX_METADATA_BYTES).map_err(refused)?;
    if duduclaw_updated::publication::manifest_digest(&bytes) != generation {
        return Err(refused("generation manifest"));
    }
    let signature = directory.bytes("manifest.json.sig", 16 * 1024).map_err(refused)?;
    let signature = std::str::from_utf8(&signature).map_err(refused)?;
    trust_admin::verify_recorded(&bytes, signature).map_err(refused)
}

/// The frozen launcher's bootstrap receipt ([`LauncherReceiptRecord`]).
/// `Ok(None)` only when the update-service config root or the receipt file
/// does not exist.
pub(super) fn read_launcher_receipt() -> Result<Option<LauncherReceipt>> {
    use duduclaw_updated::{Error as UpdateError, linux::{files::Directory, CONFIG_ROOT}};
    let missing = |error: &UpdateError| matches!(error, UpdateError::Io(error) if error.kind() == std::io::ErrorKind::NotFound);
    let directory = match Directory::open(Path::new(CONFIG_ROOT)) {
        Ok(directory) => directory,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(refused(error)),
    };
    // `Directory::read`: openat2 without symlinks, root:root, regular, one
    // link, not group/other writable; the exact mode is checked here.
    let file = match directory.read(LAUNCHER_RECEIPT_FILE) {
        Ok(file) => file,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(refused(error)),
    };
    let metadata = file.metadata()?;
    if metadata.mode() & 0o7777 != 0o600 || metadata.len() > MAX_LAUNCHER_RECEIPT_BYTES as u64 {
        return Err(refused("launcher receipt"));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_LAUNCHER_RECEIPT_BYTES as u64 + 1), &mut bytes)?;
    Ok(Some(LauncherReceiptRecord::parse(&bytes)?.receipt()))
}

// ── Installer groups (item #1) ────────────────────────────────────────────

fn binaries() -> Result<()> {
    if !layout::published_components(Path::new("/"), 0, 0)? {
        return Err(refused("缺少隔離元件。"));
    }
    for name in [BINARY, LAUNCHER] {
        let path = Path::new(name);
        parents(path)?;
        trusted(path, false)?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.mode() & 0o7777 != 0o755 || metadata.nlink() != 1 || metadata.len() == 0 {
            return Err(refused("服務執行檔權限或內容不符。"));
        }
    }
    Ok(())
}

fn directories() -> Result<()> {
    for (path, mode) in layout::DIRECTORIES {
        parents(Path::new(path))?;
        trusted(Path::new(path), true)?;
        if fs::symlink_metadata(path)?.mode() & 0o7777 != *mode {
            return Err(refused("隔離服務目錄權限不符。"));
        }
    }
    for path in ["/usr/lib/duduclaw/parse-root"] {
        if !layout::parse_root_is_reusable(Path::new(path), 0, 0)? {
            return Err(refused("解析服務目錄含有非預期內容。"));
        }
    }
    parse_runtime_tree(false)?;
    Ok(())
}

fn accounts() -> Result<()> {
    let defs = fs::read_to_string("/etc/login.defs")?;
    let mut identities = Vec::new();
    for name in std::iter::once("duduclaw").chain(layout::ACCOUNTS.iter().copied()) {
        let passwd = String::from_utf8(checked("getent", &["passwd", name])?.stdout)
            .map_err(|_| refused("無法讀取服務帳號。"))?;
        let fields: Vec<_> = passwd.trim_end_matches('\n').split(':').collect();
        let expected_home = if name == "duduclaw" { "/var/lib/duduclaw" } else { "/nonexistent" };
        if fields.len() != 7 || fields[0] != name || fields[5] != expected_home
            || !matches!(fields[6], "/usr/sbin/nologin" | "/sbin/nologin") {
            return Err(refused("服務帳號的家目錄或登入設定不符。"));
        }
        let uid = fields[2].parse::<u32>().map_err(|_| refused("無效的服務身分。"))?;
        let gid = fields[3].parse::<u32>().map_err(|_| refused("無效的服務群組。"))?;
        layout::validate_system_ids(uid, gid, &defs).map_err(refused)?;
        if identities.iter().any(|(u, g)| *u == uid || *g == gid) { return Err(refused("服務身分重複。")); }
        identities.push((uid, gid));
        if name == "duduclaw" {
            let home = Path::new(expected_home);
            parents(home)?;
            if !layout::migrated_home_is_private(home, 0, 0, uid, gid)? {
                return Err(refused("遷移後的服務資料目錄擁有者或權限不符。"));
            }
        }
        let groups = String::from_utf8(checked("id", &["-G", name])?.stdout)
            .map_err(|_| refused("無法讀取服務群組。"))?;
        if groups.split_whitespace().collect::<Vec<_>>() != vec![fields[3]] {
            return Err(refused("服務帳號帶有額外群組。"));
        }
        let shadow = String::from_utf8(checked("getent", &["shadow", name])?.stdout)
            .map_err(|_| refused("無法確認服務帳號已鎖定。"))?;
        let shadow: Vec<_> = shadow.trim_end_matches('\n').split(':').collect();
        if shadow.len() != 9 || shadow[0] != name || !matches!(shadow[1].as_bytes().first(), Some(b'!' | b'*')) {
            return Err(refused("服務帳號尚未鎖定密碼。"));
        }
        if name != "duduclaw" && shadow[7] != "1" { return Err(refused("輔助服務帳號的到期封鎖設定不符。")); }
        match fs::symlink_metadata(format!("/var/lib/systemd/linger/{name}")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(refused("服務帳號仍可啟動背景登入工作階段。")),
        }
    }
    Ok(())
}

/// Loaded from the installed fragment, active, no drop-in and no pending
/// daemon-reload. Unknown systemctl output is "not active".
pub(super) fn unit_loaded_active(name: &str) -> Result<()> {
    let output = checked("systemctl", &["show", name,
        "--property=LoadState,ActiveState,FragmentPath,NeedDaemonReload,DropInPaths"])?;
    let text = String::from_utf8(output.stdout).map_err(|_| refused("無法讀取服務狀態。"))?;
    let mut fields: Vec<_> = text.lines().collect(); fields.sort_unstable();
    let fragment = format!("FragmentPath=/etc/systemd/system/{name}");
    if fields != ["ActiveState=active", "DropInPaths=", fragment.as_str(), "LoadState=loaded", "NeedDaemonReload=no"] {
        return Err(refused("隔離服務尚未載入預期設定，或含有尚未驗證的覆寫設定。"));
    }
    Ok(())
}

fn units() -> Result<()> {
    platform_units()?;
    unit_loaded_active(UNIT)
}

/// Every layout unit file as installed, and every isolation service active;
/// the gateway unit is not part of it (the bootstrap starts it at S6 and
/// enables it at S8, after this check).
/// The R7a self-test unit's bytes are checked with the others, but it is never
/// required active (a `RemainAfterExit=no` oneshot spawnd starts; inactive after its run).
/// The DNS broker unit must be the text generated for the upstream the
/// root-owned broker config records (the shipped text when there is none).
fn platform_units() -> Result<()> {
    let upstream = recorded_broker_upstream()?;
    for (name, expected) in layout::UNITS {
        let path = Path::new("/etc/systemd/system").join(name);
        parents(&path)?;
        trusted(&path, false)?;
        if fs::symlink_metadata(&path)?.mode() & 0o7777 != 0o644 {
            return Err(refused("隔離服務單元與安裝版本不符。"));
        }
        if *name == layout::DNS_BROKER_UNIT {
            layout::dns_broker_unit_check(&fs::read(&path)?, upstream).map_err(refused)?;
        } else if fs::read_to_string(&path)? != *expected {
            return Err(refused("隔離服務單元與安裝版本不符。"));
        }
    }
    for name in ["duduclaw-spawnd.service", "duduclaw-spawnd.socket", "duduclaw-isolation-ready.service", "duduclaw-egress.service",
        "duduclaw-egress.socket", "duduclaw-updated.service", "duduclaw-parse.socket"] {
        unit_loaded_active(name)?;
    }
    Ok(())
}

/// This install's DNS broker upstream: `None` without the broker config (the
/// resolved route), else the config's upstream through the broker's own
/// reader and validator.
fn recorded_broker_upstream() -> Result<Option<std::net::SocketAddr>> {
    use duduclaw_egress::daemon::{read_root_config, BrokerConfig, BROKER_CONFIG_PATH};
    if absent(BROKER_CONFIG_PATH)? { return Ok(None); }
    let config: BrokerConfig = read_root_config(BROKER_CONFIG_PATH).map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
    config.validate().map_err(|_| refused("DNS 轉送服務設定讀回失敗。"))?;
    Ok(Some(config.upstream))
}

/// The broker unit's allow-rule mismatch is the one failure detail printed
/// (`layout::dns_broker_unit_mismatch`: fixed wording naming the rule only).
fn broker_unit_detail(result: &Result<()>) -> Option<String> {
    let text = result.as_ref().err()?.to_string();
    text.find(layout::DNS_BROKER_MISMATCH_HEAD).map(|start| text[start..].to_owned())
}

/// The bootstrap's step S7 (plan §4.3): the composite check before the
/// gateway is enabled. Identical to [`check`] except that the gateway unit
/// itself is not required to be active; every other group and every
/// composite item must pass. Prints the same fixed lines.
pub(super) fn check_platform(composite: &mut impl CompositeReadiness) -> Result<()> {
    let results = [("執行檔", binaries()), ("服務帳號", accounts()),
        ("目錄權限", directories()), ("隔離服務單元", platform_units())];
    let mut failed = 0;
    for (label, result) in results {
        if result.is_ok() { println!("[通過] {label}"); }
        else { failed += 1; println!("[未通過] {label}"); }
        if let Some(detail) = broker_unit_detail(&result) { println!("  - {detail}"); }
    }
    let (lines, composite_failed, _) = render(&composite.assess());
    for line in lines { println!("{line}"); }
    failed += composite_failed;
    if failed != 0 {
        return Err(refused(format!("{failed} 項就緒檢查未通過；Gateway 尚未設為開機自動啟動。修正後請再執行同一個安裝命令續接。")));
    }
    Ok(())
}

/// Administrator lines for the composite findings, the number of failed
/// items and whether a migration waits for confirmation. Fixed text only.
fn render(findings: &[Finding]) -> (Vec<String>, usize, bool) {
    let mut lines = Vec::new();
    let mut failed = 0;
    for item in Item::ALL {
        let mut messages: Vec<&str> = Vec::new();
        let mut waiting = false;
        for finding in findings.iter().filter(|finding| finding.item == item) {
            match finding.severity {
                Severity::Failed => if !messages.contains(&finding.message.as_str()) {
                    messages.push(finding.message.as_str());
                },
                Severity::Pending => waiting = true,
                Severity::Notice => (),
            }
        }
        if !messages.is_empty() {
            failed += 1;
            lines.push(format!("[未通過] {}", item.label()));
            lines.extend(messages.into_iter().map(|message| format!("  - {message}")));
        } else if waiting {
            lines.push(format!("[待確認] {}", item.label()));
        } else {
            lines.push(format!("[通過] {}", item.label()));
        }
    }
    for finding in findings.iter().filter(|finding| finding.severity == Severity::Notice) {
        lines.push(format!("[資訊] {}", finding.message));
    }
    let pending = findings.iter().any(|finding| finding.severity == Severity::Pending);
    (lines, failed, pending)
}

// Check every group even if one fails, so the administrator sees a useful
// summary. Never echo subprocess output (getent shadow includes credentials).
pub(super) fn check(composite: &mut impl CompositeReadiness) -> Result<()> {
    let results = [("執行檔", binaries()), ("服務帳號", accounts()),
        ("目錄權限", directories()), ("服務單元", units())];
    let mut failed = 0;
    for (label, result) in results {
        if result.is_ok() { println!("[通過] {label}"); }
        else { failed += 1; println!("[未通過] {label}"); }
        if let Some(detail) = broker_unit_detail(&result) { println!("  - {detail}"); }
    }
    let (lines, composite_failed, pending) = render(&composite.assess());
    for line in lines { println!("{line}"); }
    failed += composite_failed;
    if failed != 0 { return Err(refused(format!("{failed} 項就緒檢查未通過；尚未確認可安全執行員工工作。"))); }
    println!("{}", if pending { Reason::MIGRATION_PENDING_MESSAGE } else { MSG_READY });
    Ok(())
}

#[cfg(test)]
#[path = "protected_linux_readiness_tests.rs"]
mod tests;
