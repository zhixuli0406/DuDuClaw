//! v1.83 R4 composite readiness over injected sources: every item is tested
//! passing, failing and unreadable. No test here reads the host.
use super::*;
use duduclaw_isolation::update::{OperatingSystem, ProtocolCompatibility, ReleaseChannel};

const VERSION: &str = "1.83.0";

fn digest(byte: u8) -> Sha256Digest { Sha256Digest([byte; 32]) }
fn version() -> ReleaseVersion { duduclaw_updated::parse_version(VERSION).unwrap() }
fn protocols(value: u16) -> ProtocolCompatibility {
    ProtocolCompatibility { gateway: value, spawnd: value, trampoline: value, egress: value, updated: value,
        mcp_bridge: value, parse_worker: value, importer: value }
}

fn manifest() -> ReleaseManifest {
    // One artifact per signed target (both gateway copies share the Gateway artifact).
    let mut artifacts: Vec<ReleaseArtifact> = Vec::new();
    for path in component_paths() {
        let target = component_target(path).unwrap();
        if artifacts.iter().any(|artifact| artifact.install_target == target) { continue; }
        artifacts.push(ReleaseArtifact { id: duduclaw_updated::publication::artifact_name(&target).unwrap(),
            sha256: digest(9), size_bytes: 7, mode: 0o755, install_target: target });
    }
    ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: version(),
        channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
        minimum_version: version(), serial: 1, created_at_unix: 100, expires_at_unix: 5000,
        artifacts, protocols: protocols(1), migrations: Vec::new() }
}

fn all_checks() -> HostChecks {
    HostChecks { protocol: duduclaw_isolation::ipc::PROTOCOL_VERSION, registry_verified: true, accounts_locked: true,
        ssh_live_negative_checks: true, cron_at_acl_and_jobs_verified: true, external_iam_reservation_verified: true,
        trampoline_verified: true, landlock_self_test: true, namespace_self_test: true, egress_verified: true,
        dns_route_verified: true, storage_verified: true, resources_verified: true, startup_reconciled: true,
        process_scan_verified: true, egress_second_layer: EgressSecondLayer::Verified }
}

fn launcher_receipt() -> LauncherReceipt { LauncherReceipt { sha256: digest(4), size_bytes: 7, mode: 0o755 } }

/// `None` = the source cannot be read.
struct Fake {
    probe: Option<GenerationEvidence>,
    state: Option<UpdateStateView>,
    current: Option<Sha256Digest>,
    manifest: Option<ReleaseManifest>,
    broken: Vec<&'static str>,
    /// The bootstrap receipt of the frozen launcher (`Some(None)` = absent).
    receipt: Option<Option<LauncherReceipt>>,
    /// What the installed launcher hashes to.
    launcher: Sha256Digest,
    evidence: Option<HostEvidence>,
    inactive: Vec<&'static str>,
    egress: Option<EgressView>,
    broker: Option<bool>,
    /// The broker answers its health request in time.
    broker_healthy: bool,
    cloud: bool,
    trust: Option<TrustView>,
    now: Option<u64>,
    migration: std::result::Result<Option<MigrationState>, &'static str>,
    team: Option<bool>,
}

impl Fake {
    fn healthy() -> Self {
        Self {
            probe: Some(GenerationEvidence { generation: digest(1), product: Product::Duduclaw, version: version(),
                protocols: protocols(1) }),
            state: Some(UpdateStateView { committed: digest(1), pending: false, maintenance: false, running_other: false,
                product: Product::Duduclaw, arch: Architecture::X86_64, floor: version() }),
            current: Some(digest(1)),
            manifest: Some(manifest()),
            broken: Vec::new(),
            receipt: Some(Some(launcher_receipt())),
            launcher: digest(4),
            evidence: Some(HostEvidence { verdict_ok: true, checks: all_checks(), landlock: Some((6, 6)) }),
            inactive: Vec::new(),
            egress: Some(EgressView { host_public_addresses_empty: false }),
            broker: Some(false),
            broker_healthy: true,
            cloud: false,
            trust: Some(TrustView { expires_at_unix: 2000, frozen: false }),
            now: Some(1000),
            migration: Ok(None),
            team: Some(true),
        }
    }
    fn unreadable() -> Self {
        Self { probe: None, state: None, current: None, manifest: None, broken: Vec::new(), receipt: None,
            launcher: digest(0), evidence: None,
            inactive: vec!["duduclaw-egress.socket", "duduclaw-egress.service", "duduclaw-parse.socket",
                "duduclaw-dns-broker.socket"],
            egress: None, broker: None, broker_healthy: false, cloud: true, trust: None, now: None,
            migration: Err("migration_state_unreadable"), team: None }
    }
    fn assess(&mut self) -> Vec<Finding> { assess_host(self, VERSION) }
}

fn unavailable<T>(value: Option<T>) -> Result<T> { value.ok_or_else(|| refused("fake source unavailable")) }

impl HostSources for Fake {
    fn update_probe(&mut self) -> Result<GenerationEvidence> { unavailable(self.probe.clone()) }
    fn update_state(&mut self) -> Result<UpdateStateView> { unavailable(self.state) }
    fn current_generation(&mut self) -> Result<Sha256Digest> { unavailable(self.current) }
    fn generation_manifest(&mut self, generation: Sha256Digest) -> Result<ReleaseManifest> {
        // The live source authenticates the manifest of exactly this generation.
        if Some(generation) != self.current { return Err(refused("other generation")); }
        unavailable(self.manifest.clone())
    }
    fn component(&mut self, path: &'static str, artifact: &ReleaseArtifact) -> Result<()> {
        // The frozen launcher is never compared with the generation.
        assert_ne!(path, UPDATER_LAUNCHER);
        if self.broken.contains(&path) || artifact.sha256 != digest(9) { return Err(refused("mismatch")); }
        Ok(())
    }
    fn launcher_receipt(&mut self) -> Result<Option<LauncherReceipt>> { unavailable(self.receipt) }
    fn launcher(&mut self, receipt: &LauncherReceipt) -> Result<()> {
        if receipt.sha256 != self.launcher { return Err(refused("launcher mismatch")); }
        Ok(())
    }
    fn host_evidence(&mut self) -> Result<HostEvidence> { unavailable(self.evidence.clone()) }
    fn unit_active(&mut self, unit: &'static str) -> Result<()> {
        if self.inactive.contains(&unit) { Err(refused("inactive")) } else { Ok(()) }
    }
    fn egress_config(&mut self) -> Result<EgressView> { unavailable(self.egress) }
    fn dns_broker_configured(&mut self) -> Result<bool> { unavailable(self.broker) }
    fn dns_broker_health(&mut self) -> Result<()> {
        if self.broker_healthy { Ok(()) } else { Err(refused("broker health timed out")) }
    }
    fn cloud_host(&mut self) -> bool { self.cloud }
    fn trust_root(&mut self) -> Result<TrustView> { unavailable(self.trust) }
    fn now(&mut self) -> Result<u64> { unavailable(self.now) }
    fn migration_state(&mut self) -> std::result::Result<Option<MigrationState>, &'static str> { self.migration }
    fn edition_is_team(&mut self) -> Result<bool> { unavailable(self.team) }
}

fn failures(findings: &[Finding]) -> Vec<(Item, &'static str)> {
    findings.iter().filter(|finding| finding.is_failure()).map(|finding| (finding.item, finding.code)).collect()
}
fn codes(findings: &[Finding], severity: Severity) -> Vec<&'static str> {
    findings.iter().filter(|finding| finding.severity == severity).map(|finding| finding.code).collect()
}

struct FakeComposite(Fake);
impl CompositeReadiness for FakeComposite {
    fn assess(&mut self) -> Vec<Finding> { self.0.assess() }
}

#[test]
fn service_protected_readiness_healthy_host_passes_every_item() {
    let findings = Fake::healthy().assess();
    assert!(findings.is_empty(), "{findings:?}");
    // The seam the four call sites use no longer refuses a verified host.
    assert!(FakeComposite(Fake::healthy()).verify().is_ok());
    let (lines, failed, pending) = render(&findings);
    assert_eq!((failed, pending), (0, false));
    assert_eq!(lines.len(), Item::ALL.len());
    assert!(lines.iter().all(|line| line.starts_with("[通過] ")), "{lines:?}");
}

#[test]
fn service_protected_readiness_every_item_fails_closed_when_unreadable() {
    let findings = Fake::unreadable().assess();
    for item in Item::ALL {
        assert!(findings.iter().any(|finding| finding.item == item && finding.is_failure()), "{item:?}: {findings:?}");
    }
    assert!(FakeComposite(Fake::unreadable()).verify().is_err());
    let (_, failed, _) = render(&findings);
    assert_eq!(failed, Item::ALL.len());
    // Unreadable is never mistaken for an absent migration or a notice.
    assert!(codes(&findings, Severity::Notice).is_empty() && codes(&findings, Severity::Pending).is_empty());
}

#[test]
fn service_protected_readiness_update_generation_pass_fail_unreadable() {
    let check = |fake: Fake| failures(&{ let mut fake = fake; fake.assess() }).into_iter()
        .filter(|(item, _)| *item == Item::UpdateGeneration).map(|(_, code)| code).collect::<Vec<_>>();
    assert!(check(Fake::healthy()).is_empty());
    let mut fake = Fake::healthy(); fake.probe = None;
    assert_eq!(check(fake), ["update_probe_failed"]);
    let incompatible: [fn(&mut GenerationEvidence); 4] = [|e| e.protocols.importer = 2, |e| e.protocols.updated = 0,
        |e| e.version.patch += 1, |e| e.generation = digest(0)];
    for broken in incompatible {
        let mut fake = Fake::healthy();
        broken(fake.probe.as_mut().unwrap());
        assert_eq!(check(fake), ["update_incompatible"]);
    }
    let mismatched: [fn(&mut Fake); 6] = [|f| f.probe.as_mut().unwrap().generation = digest(2),
        |f| f.state.as_mut().unwrap().committed = digest(2),
        |f| f.state.as_mut().unwrap().pending = true,
        |f| f.state.as_mut().unwrap().maintenance = true,
        |f| f.state.as_mut().unwrap().running_other = true,
        |f| f.probe.as_mut().unwrap().product = Product::DuduclawPro];
    for broken in mismatched {
        let mut fake = Fake::healthy();
        broken(&mut fake);
        assert_eq!(check(fake), ["update_generation_mismatch"]);
    }
    let mut fake = Fake::healthy(); fake.state = None;
    assert_eq!(check(fake), ["update_state_unreadable"]);
    let mut fake = Fake::healthy(); fake.current = None;
    assert_eq!(check(fake), ["update_selector_unreadable"]);
    assert_eq!(MSG_UPDATE, "更新服務的世代與安裝不一致，或更新服務沒有回應。");
}

#[test]
fn service_protected_readiness_components_match_the_signed_generation() {
    // One altered executable: a component failure, nothing else.
    let mut fake = Fake::healthy(); fake.broken = vec!["/usr/lib/duduclaw/duduclaw-run"];
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    // R7a: the self-test binary is compared like the trampoline, and its artifact must be signed.
    let mut fake = Fake::healthy(); fake.broken = vec!["/usr/lib/duduclaw/duduclaw-run-selftest"];
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    let mut fake = Fake::healthy();
    fake.manifest.as_mut().unwrap().artifacts.retain(|artifact| artifact.install_target != InstallTarget::TrampolineSelftest);
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    // R7c: the SSH denial probe binary likewise.
    let mut fake = Fake::healthy(); fake.broken = vec!["/usr/lib/duduclaw/duduclaw-isolation-probe"];
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    let mut fake = Fake::healthy();
    fake.manifest.as_mut().unwrap().artifacts.retain(|artifact| artifact.install_target != InstallTarget::IsolationProbe);
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    // The gateway executable is compared too.
    let mut fake = Fake::healthy(); fake.broken = vec![BINARY];
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    // The parse worker also fails item #7.
    let mut fake = Fake::healthy(); fake.broken = vec!["/usr/libexec/duduclaw/duduclaw-parse-worker"];
    assert_eq!(failures(&fake.assess()),
        [(Item::ParseWorker, "parse_worker_mismatch"), (Item::Components, "component_mismatch")]);
    // Missing or duplicated signed artifact.
    let mut fake = Fake::healthy();
    fake.manifest.as_mut().unwrap().artifacts.retain(|artifact| artifact.install_target != InstallTarget::Egress);
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    let mut fake = Fake::healthy();
    let duplicate = fake.manifest.as_ref().unwrap().artifacts[0].clone();
    fake.manifest.as_mut().unwrap().artifacts.push(duplicate);
    assert_eq!(failures(&fake.assess()), [(Item::Components, "component_mismatch")]);
    // Unverifiable manifest, or one that is not the committed generation's.
    let cases: [fn(&mut Fake); 4] = [|f| f.manifest = None, |f| f.manifest.as_mut().unwrap().version.minor += 1,
        |f| f.manifest.as_mut().unwrap().arch = Architecture::Aarch64,
        |f| f.manifest.as_mut().unwrap().product = Product::DuduclawPro];
    for broken in cases {
        let mut fake = Fake::healthy();
        broken(&mut fake);
        assert_eq!(failures(&fake.assess()),
            [(Item::Components, "generation_manifest_unverified"), (Item::ParseWorker, "parse_worker_unverified")]);
    }
}

#[test]
fn service_protected_readiness_component_paths_map_to_distinct_signed_targets() {
    let mut targets = Vec::new();
    for path in component_paths() {
        let target = component_target(path).unwrap_or_else(|| panic!("unmapped published executable {path}"));
        // Only the gateway has two fixed-path copies of one signed artifact.
        assert!(!targets.contains(&target) || target == InstallTarget::Gateway, "{path}");
        let name = duduclaw_updated::publication::artifact_name(&target).unwrap();
        let file = Path::new(path).file_name().unwrap().to_str().unwrap();
        match path {
            BINARY => assert_eq!(name, "duduclaw"),
            _ => assert_eq!(name, file, "{path}"),
        }
        targets.push(target);
    }
    // Every published executable except the frozen launcher, plus the gateway.
    assert_eq!(targets.len(), layout::BINARIES.len());
    assert_eq!(targets.iter().filter(|target| **target == InstallTarget::Gateway).count(), 2);
    assert_eq!(component_target("/usr/lib/duduclaw/current/duduclaw"), Some(InstallTarget::Gateway));
    assert_eq!(component_target("/usr/lib/duduclaw/duduclaw-run-selftest"), Some(InstallTarget::TrampolineSelftest));
    assert_eq!(component_target(duduclaw_spawnd::systemd::SELFTEST_EXECUTABLE), Some(InstallTarget::TrampolineSelftest));
    assert_eq!(component_target("/usr/lib/duduclaw/duduclaw-isolation-probe"), Some(InstallTarget::IsolationProbe));
    assert_eq!(component_target(duduclaw_spawnd::systemd::PROBE_EXECUTABLE), Some(InstallTarget::IsolationProbe));
    assert!(layout::BINARIES.contains(&UPDATER_LAUNCHER));
    assert!(!component_paths().any(|path| path == UPDATER_LAUNCHER));
    assert!(component_target(UPDATER_LAUNCHER).is_none());
    assert!(!targets.contains(&InstallTarget::UpdateSlot));
    assert!(component_target("/usr/lib/duduclaw/duduclaw-spawnd.bak").is_none());
    assert!(component_target("/usr/lib/duduclaw/duduclaw-spawnd/").is_none());
}

/// Item 5: the frozen launcher is compared with its bootstrap receipt, never
/// with the current generation. A missing receipt fails closed.
#[test]
fn service_protected_readiness_frozen_launcher_matches_its_bootstrap_receipt() {
    // A legitimate update changes the generation's UpdateSlot artifact; the
    // launcher still matches its receipt, so nothing fails.
    let mut fake = Fake::healthy();
    fake.manifest.as_mut().unwrap().artifacts.push(ReleaseArtifact {
        id: duduclaw_updated::publication::artifact_name(&InstallTarget::UpdateSlot).unwrap(),
        sha256: digest(5), size_bytes: 9, mode: 0o700, install_target: InstallTarget::UpdateSlot });
    assert!(fake.assess().is_empty());
    // Receipt missing (a host the v1.83 bootstrap did not install), unreadable, mismatched.
    let mut fake = Fake::healthy(); fake.receipt = Some(None);
    let findings = fake.assess();
    assert_eq!(failures(&findings), [(Item::Components, "launcher_receipt_missing")]);
    assert_eq!(findings[0].message, MSG_LAUNCHER_RECEIPT);
    assert!(FakeComposite(fake).verify().is_err());
    let mut fake = Fake::healthy(); fake.receipt = None;
    assert_eq!(failures(&fake.assess()), [(Item::Components, "launcher_receipt_unreadable")]);
    let mut fake = Fake::healthy(); fake.launcher = digest(5);
    assert_eq!(failures(&fake.assess()), [(Item::Components, "launcher_mismatch")]);
    // A host without the update-service config root (this test host) reports "missing".
    assert_eq!(LiveSources::default().launcher_receipt().unwrap(), None);
}

/// The receipt the bootstrap writes (R3 S4) is exactly what this reader accepts.
#[test]
fn service_protected_readiness_launcher_receipt_record_is_strict() {
    let record = LauncherReceiptRecord { schema_version: 1, generation: digest(2), sha256: digest(4),
        size_bytes: 7, mode: 0o755, installed_at_unix: 10 };
    let bytes = serde_json::to_vec(&record).unwrap();
    assert_eq!(LauncherReceiptRecord::parse(&bytes).unwrap(), record);
    assert_eq!(record.receipt(), launcher_receipt());
    let mutate = |change: fn(&mut serde_json::Value)| {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        change(&mut value);
        serde_json::to_vec(&value).unwrap()
    };
    for bad in [mutate(|v| v["extra"] = 1.into()), mutate(|v| v["schema_version"] = 2.into()),
        mutate(|v| v["size_bytes"] = 0.into()), mutate(|v| v["mode"] = 0o700.into()),
        mutate(|v| v["sha256"] = serde_json::to_value(digest(0)).unwrap()),
        mutate(|v| v["generation"] = serde_json::to_value(digest(0)).unwrap()),
        mutate(|v| { v.as_object_mut().unwrap().remove("generation"); }), b"null".to_vec(), Vec::new()] {
        assert!(LauncherReceiptRecord::parse(&bad).is_err());
    }
    let mut padded = bytes.clone();
    padded.extend(vec![b' '; MAX_LAUNCHER_RECEIPT_BYTES]);
    assert!(LauncherReceiptRecord::parse(&padded).is_err());
}

/// C4: the bootstrap's S6 refusal names each false field in the composite's wording.
#[test]
fn unmet_host_checks_use_the_composite_wording() {
    assert!(unmet_host_checks(&all_checks()).is_empty());
    let mut checks = all_checks();
    checks.startup_reconciled = false;
    assert_eq!(unmet_host_checks(&checks), ["啟動時的執行紀錄對帳尚未完成。"]);
    checks.landlock_self_test = false;
    checks.protocol = 2;
    assert_eq!(unmet_host_checks(&checks), [MSG_HOST_PROTOCOL, "啟動時的執行紀錄對帳尚未完成。", MSG_LANDLOCK]);
    let primary: Vec<_> = host_checks(&HostChecks::default()).into_iter().map(|(_, _, message)| message).collect();
    let mut expected = primary.clone();
    expected.push(MSG_LANDLOCK);
    assert_eq!(unmet_host_checks(&HostChecks::default()), expected);
}

#[test]
fn service_protected_readiness_host_evidence_names_each_false_check() {
    type Flip = fn(&mut HostChecks);
    let cases: [(Flip, &str); 13] = [
        (|c| c.registry_verified = false, "host_registry_unverified"),
        (|c| c.accounts_locked = false, "host_accounts_unlocked"),
        (|c| c.ssh_live_negative_checks = false, "host_ssh_lockout_unverified"),
        (|c| c.cron_at_acl_and_jobs_verified = false, "host_cron_at_unverified"),
        (|c| c.external_iam_reservation_verified = false, "host_external_iam_unverified"),
        (|c| c.trampoline_verified = false, "host_trampoline_unverified"),
        (|c| c.namespace_self_test = false, "host_namespace_self_test_failed"),
        (|c| c.egress_verified = false, "host_egress_unverified"),
        (|c| c.dns_route_verified = false, "host_dns_route_unverified"),
        (|c| c.storage_verified = false, "host_storage_unverified"),
        (|c| c.resources_verified = false, "host_resources_unverified"),
        (|c| c.startup_reconciled = false, "host_startup_unreconciled"),
        (|c| c.process_scan_verified = false, "host_process_scan_unverified"),
    ];
    // A false boolean also fails spawnd's combined verdict; both are reported.
    for (flip, code) in cases {
        let mut fake = Fake::healthy();
        let evidence = fake.evidence.as_mut().unwrap();
        flip(&mut evidence.checks);
        evidence.verdict_ok = false;
        assert_eq!(failures(&fake.assess()), [(Item::HostEvidence, code), (Item::HostEvidence, "host_evidence_stale")]);
    }
    let messages: Vec<_> = host_checks(&HostChecks::default()).into_iter().map(|(_, _, message)| message).collect();
    assert!(messages.contains(&"員工帳號的 SSH 登入封鎖未通過實測。"));
    let mut fake = Fake::healthy();
    fake.evidence.as_mut().unwrap().checks.protocol = 2;
    fake.evidence.as_mut().unwrap().verdict_ok = false;
    assert_eq!(failures(&fake.assess()),
        [(Item::HostEvidence, "host_protocol_mismatch"), (Item::HostEvidence, "host_evidence_stale")]);
    // Every boolean true but stale/other-boot evidence: still not ready.
    let mut fake = Fake::healthy(); fake.evidence.as_mut().unwrap().verdict_ok = false;
    assert_eq!(failures(&fake.assess()), [(Item::HostEvidence, "host_evidence_stale")]);
    let mut fake = Fake::healthy(); fake.evidence = None;
    assert_eq!(failures(&fake.assess()),
        [(Item::HostEvidence, "host_evidence_unavailable"), (Item::Landlock, "landlock_unverified")]);
}

/// Item 7: stale evidence is reported even when another item (Landlock, a
/// named host check) fails too; HostEvidence is never shown as passed then.
#[test]
fn service_protected_readiness_stale_evidence_always_reported() {
    let mut fake = Fake::healthy();
    let evidence = fake.evidence.as_mut().unwrap();
    evidence.verdict_ok = false;
    evidence.checks.landlock_self_test = false;
    let findings = fake.assess();
    assert_eq!(failures(&findings), [(Item::Landlock, "landlock_self_test_failed"), (Item::HostEvidence, "host_evidence_stale")]);
    let (lines, failed, _) = render(&findings);
    assert_eq!(failed, 2);
    assert!(lines.contains(&format!("[未通過] {}", Item::HostEvidence.label())), "{lines:?}");
    assert!(!lines.contains(&format!("[通過] {}", Item::HostEvidence.label())), "{lines:?}");
    // With a named host failure the stale verdict is still its own finding.
    let mut fake = Fake::healthy();
    let evidence = fake.evidence.as_mut().unwrap();
    evidence.verdict_ok = false;
    evidence.checks.storage_verified = false;
    evidence.landlock = Some((9, 9));
    assert_eq!(failures(&fake.assess()), [(Item::HostEvidence, "host_storage_unverified"),
        (Item::Landlock, "landlock_self_test_failed"), (Item::HostEvidence, "host_evidence_stale")]);
}

#[test]
fn service_protected_readiness_landlock_clamp_is_a_notice_and_refusals_are_explicit() {
    // HWE-style kernel: detected 8, clamped to 7, self test passed.
    let mut fake = Fake::healthy(); fake.evidence.as_mut().unwrap().landlock = Some((8, 7));
    let findings = fake.assess();
    assert!(failures(&findings).is_empty(), "{findings:?}");
    assert_eq!(findings, [Finding { item: Item::Landlock, severity: Severity::Notice,
        code: "landlock_abi_clamped", message: "核心 Landlock ABI 8，套用 ABI 7 規則集。".into() }]);
    assert!(FakeComposite(fake).verify().is_ok(), "a notice never makes the host not ready");
    // Known ABIs: no notice.
    for abi in [1, 6, 7] {
        let mut fake = Fake::healthy(); fake.evidence.as_mut().unwrap().landlock = Some((abi, abi));
        assert!(fake.assess().is_empty(), "{abi}");
    }
    // Clamp not applied, missing evidence or a failed self test: refused.
    for (landlock, self_test) in [(Some((8, 8)), true), (Some((8, 6)), true), (Some((0, 0)), true), (None, true),
        (Some((8, 7)), false), (Some((6, 6)), false)] {
        let mut fake = Fake::healthy();
        let evidence = fake.evidence.as_mut().unwrap();
        evidence.landlock = landlock;
        evidence.checks.landlock_self_test = self_test;
        evidence.verdict_ok = false;
        let findings = fake.assess();
        assert_eq!(failures(&findings), [(Item::Landlock, "landlock_self_test_failed"),
            (Item::HostEvidence, "host_evidence_stale")], "{landlock:?} {self_test}");
        assert!(codes(&findings, Severity::Notice).is_empty());
        assert!(findings[0].message.contains("Ubuntu 24.04 HWE"));
    }
}

#[test]
fn service_protected_readiness_egress_dns_and_informational_notices() {
    // Unit inactive, config unreadable/invalid.
    for unit in ["duduclaw-egress.socket", "duduclaw-egress.service"] {
        let mut fake = Fake::healthy(); fake.inactive = vec![unit];
        assert_eq!(failures(&fake.assess()), [(Item::EgressDns, "egress_inactive")]);
    }
    let mut fake = Fake::healthy(); fake.egress = None;
    assert_eq!(failures(&fake.assess()), [(Item::EgressDns, "egress_config_invalid")]);
    // Broker route: config must read back and its socket must be active.
    let mut fake = Fake::healthy(); fake.broker = Some(true);
    assert!(fake.assess().is_empty());
    let mut fake = Fake::healthy(); fake.broker = Some(true); fake.inactive = vec!["duduclaw-dns-broker.socket"];
    assert_eq!(failures(&fake.assess()), [(Item::EgressDns, "dns_broker_inactive")]);
    let mut fake = Fake::healthy(); fake.broker = None;
    assert_eq!(failures(&fake.assess()), [(Item::EgressDns, "dns_broker_config_invalid")]);
    // Item 2: an active socket is not enough; the broker must answer its
    // health request in time (timeout or no answer ⇒ unhealthy).
    let mut fake = Fake::healthy(); fake.broker = Some(true); fake.broker_healthy = false;
    let findings = fake.assess();
    assert_eq!(failures(&findings), [(Item::EgressDns, "dns_broker_unhealthy")]);
    assert_eq!(findings[0].message, MSG_DNS_BROKER_UNHEALTHY);
    let mut fake = Fake::healthy(); fake.broker = Some(true); fake.broker_healthy = false;
    fake.inactive = vec!["duduclaw-dns-broker.socket"];
    assert_eq!(failures(&fake.assess()),
        [(Item::EgressDns, "dns_broker_inactive"), (Item::EgressDns, "dns_broker_unhealthy")]);
    // The resolved route does not need the broker socket nor probes it.
    let mut fake = Fake::healthy(); fake.inactive = vec!["duduclaw-dns-broker.socket"]; fake.broker_healthy = false;
    assert!(fake.assess().is_empty());
    // Notices: second layer, cloud without configured public addresses.
    let mut fake = Fake::healthy();
    fake.evidence.as_mut().unwrap().checks.egress_second_layer = EgressSecondLayer::Unavailable;
    fake.egress = Some(EgressView { host_public_addresses_empty: true });
    fake.cloud = true;
    let findings = fake.assess();
    assert_eq!(codes(&findings, Severity::Notice), ["cloud_public_address_unset", "egress_second_layer_unavailable"]);
    assert!(failures(&findings).is_empty());
    assert!(FakeComposite(fake).verify().is_ok());
    let (lines, failed, _) = render(&findings);
    assert_eq!(failed, 0);
    assert!(lines.contains(&format!("[資訊] {MSG_SECOND_LAYER}")));
    // Addresses configured, or not a cloud host: no reminder.
    for (empty, cloud) in [(false, true), (true, false)] {
        let mut fake = Fake::healthy();
        fake.egress = Some(EgressView { host_public_addresses_empty: empty });
        fake.cloud = cloud;
        assert!(fake.assess().is_empty());
    }
    // Notices never rescue a failure.
    let mut fake = Fake::unreadable(); fake.cloud = true;
    assert!(FakeComposite(fake).verify().is_err());
}

/// Item 2: the broker config is validated by the broker's own type and
/// validator; values the real broker refuses at start are refused here too.
#[test]
fn service_protected_readiness_broker_config_uses_the_brokers_validator() {
    use duduclaw_egress::daemon::BrokerConfig;
    for refused_value in ["224.0.0.1:53", "8.8.8.8:5353", "0.0.0.0:53", "[::]:53", "[ff02::fb]:53", "255.255.255.255:53"] {
        let config: BrokerConfig = serde_json::from_str(&format!(r#"{{"upstream":"{refused_value}"}}"#)).unwrap();
        assert!(config.validate().is_err(), "{refused_value}");
    }
    let config: BrokerConfig = serde_json::from_str(r#"{"upstream":"9.9.9.9:53"}"#).unwrap();
    assert!(config.validate().is_ok());
    assert!(serde_json::from_str::<BrokerConfig>(r#"{"upstream":"9.9.9.9:53","port":53}"#).is_err());
    // No private copy of the config type is left in this module.
    let source = include_str!("protected_linux_readiness.rs");
    assert!(!source.contains(concat!("struct ", "BrokerConfigView")));
}

/// The broker unit is checked against the text generated for the upstream
/// the broker config records (the shipped text without a config), through
/// the broker's own reader; its fixed allow-rule mismatch remains a group detail.
#[test]
fn service_protected_readiness_broker_unit_expects_the_recorded_upstream() {
    let source = include_str!("protected_linux_readiness.rs");
    let body = |signature: &str| source.split(signature).nth(1).unwrap().split("\n}\n").next().unwrap();
    let units = body("fn platform_units()");
    assert!(units.contains("let upstream = recorded_broker_upstream()"));
    assert!(units.contains("layout::dns_broker_unit_check(&fs::read(&path).map_err(|_| failure())?, upstream)"));
    let recorded = body("fn recorded_broker_upstream()");
    assert!(recorded.contains("if absent(BROKER_CONFIG_PATH)? { return Ok(None); }"));
    assert!(recorded.contains("read_root_config(BROKER_CONFIG_PATH)") && recorded.contains("config.validate()"));
    let upstream = Some("192.0.2.53:53".parse().unwrap());
    let mismatch = layout::dns_broker_unit_check(layout::DNS_BROKER_UNIT_TEXT.as_bytes(), upstream).unwrap_err();
    assert_eq!(group_detail(&Err(refused(mismatch.clone()))), Some(mismatch));
    assert_eq!(group_detail(&Err(refused("隔離服務單元與安裝版本不符。"))), Some("隔離服務單元與安裝版本不符。".to_owned()));
    assert_eq!(group_detail(&Ok(())), None);
}

#[test]
fn service_protected_readiness_parse_worker_socket_required() {
    let mut fake = Fake::healthy(); fake.inactive = vec!["duduclaw-parse.socket"];
    assert_eq!(failures(&fake.assess()), [(Item::ParseWorker, "parse_worker_inactive")]);
    assert_eq!(MSG_PARSE, "文件解析服務未就緒。");
}

#[test]
fn service_protected_readiness_trust_root_expiry_frozen_unreadable() {
    let mut fake = Fake::healthy(); fake.trust = Some(TrustView { expires_at_unix: 1000, frozen: false });
    assert_eq!(failures(&fake.assess()), [(Item::TrustRoot, "trust_root_expired")]);
    let mut fake = Fake::healthy(); fake.trust = None;
    assert_eq!(failures(&fake.assess()), [(Item::TrustRoot, "trust_root_unverified")]);
    let mut fake = Fake::healthy(); fake.now = None;
    assert_eq!(failures(&fake.assess()), [(Item::TrustRoot, "trust_root_unverified")]);
    let mut fake = Fake::healthy(); fake.trust = Some(TrustView { expires_at_unix: 2000, frozen: true });
    let findings = fake.assess();
    assert_eq!(codes(&findings, Severity::Notice), ["trust_root_frozen"]);
    assert!(failures(&findings).is_empty());
}

/// Item 1: only a verified `AwaitingAdministrator` (the source guarantees no
/// pending intent and every stage receipt) is pending; `ContentImported`
/// blocks until `Complete`'s final readback; every source error blocks with
/// its own code.
#[test]
fn service_protected_readiness_migration_pending_changes_summary_only() {
    use MigrationState as S;
    let mut fake = Fake::healthy(); fake.migration = Ok(Some(S::AwaitingAdministrator));
    let findings = fake.assess();
    assert!(failures(&findings).is_empty());
    assert_eq!(codes(&findings, Severity::Pending), [Reason::MIGRATION_PENDING_CODE]);
    let (lines, failed, pending) = render(&findings);
    assert_eq!((failed, pending), (0, true));
    assert!(lines.contains(&"[待確認] 資料遷移".to_owned()), "{lines:?}");
    assert!(FakeComposite(fake).verify().is_ok());
    for state in [None, Some(S::Complete)] {
        let mut fake = Fake::healthy(); fake.migration = Ok(state);
        assert!(fake.assess().is_empty());
    }
    // Content imported, final completion readback not yet passed: not ready.
    let mut fake = Fake::healthy(); fake.migration = Ok(Some(S::ContentImported));
    let findings = fake.assess();
    assert_eq!(failures(&findings), [(Item::Migration, "migration_completion_unverified")]);
    assert!(codes(&findings, Severity::Pending).is_empty());
    assert!(FakeComposite(fake).verify().is_err());
    // Before the new platform was verified for the migration: not ready.
    for state in [S::Planned, S::OldEnvironmentStopped, S::LegacyQuarantined, S::KeysRotated,
        S::TokenRevocationRecorded, S::EmployeeProposalsStaged, S::ContentArchived, S::ConfirmationReady,
        S::IsolationVerified] {
        let mut fake = Fake::healthy(); fake.migration = Ok(Some(state));
        assert_eq!(failures(&fake.assess()), [(Item::Migration, "migration_incomplete")], "{state:?}");
    }
    // Every verifier error blocks with its own code (never pending, never "no migration").
    use duduclaw_migration::root_host::readiness::ReadinessError as E;
    for error in [E::Unavailable, E::StateDirUnsafe, E::PlanMissing, E::PlanInvalid, E::JournalMissing,
        E::JournalUnsafe, E::JournalOversized, E::JournalSidecar, E::JournalChanged, E::JournalSchema,
        E::JournalUnreadable, E::JournalConflict, E::IntentPending, E::ReceiptsInvalid, E::JournalLegacy] {
        let mut fake = Fake::healthy(); fake.migration = Err(error.code());
        let findings = fake.assess();
        assert_eq!(failures(&findings), [(Item::Migration, error.code())], "{error:?}");
        assert!(codes(&findings, Severity::Pending).is_empty());
        assert!(!findings[0].message.contains('/'));
    }
    assert_eq!(migration_error("migration_plan_missing").1, MSG_MIGRATION_PLAN_MISSING);
    assert_eq!(migration_error("migration_intent_pending").1, MSG_MIGRATION_IN_PROGRESS);
    assert_eq!(migration_error("migration_journal_legacy"), ("migration_journal_legacy", MSG_MIGRATION_LEGACY));
    assert_eq!(MSG_MIGRATION_LEGACY, "資料遷移紀錄為舊版格式；請繼續執行遷移流程，將遷移紀錄更新為目前格式。");
    // An unknown code collapses to the generic one.
    let mut fake = Fake::healthy(); fake.migration = Err("something_else");
    assert_eq!(failures(&fake.assess()), [(Item::Migration, "migration_state_unreadable")]);
    assert_eq!(Reason::MIGRATION_PENDING_MESSAGE, "主機就緒；資料遷移尚待管理員確認，未確認的員工不能執行。");
}

#[test]
fn service_protected_readiness_edition_requires_team() {
    let mut fake = Fake::healthy(); fake.team = Some(false);
    let findings = fake.assess();
    assert_eq!(failures(&findings), [(Item::Edition, "edition_not_team")]);
    assert_eq!(findings[0].message, "此版本不支援員工隔離。");
    let mut fake = Fake::healthy(); fake.team = None;
    assert_eq!(failures(&fake.assess()), [(Item::Edition, "edition_unverified")]);
}

fn license(tier: &str) -> duduclaw_license::License {
    serde_json::from_value(serde_json::json!({ "version": 1, "subscription_id": "s", "customer_id": "c", "tier": tier,
        "machine_fingerprint": "m", "issued_at": "2026-01-01T00:00:00Z", "expires_at": "2027-01-01T00:00:00Z",
        "last_phone_home": "2026-01-01T00:00:00Z", "public_key_id": "k", "signature": "" })).unwrap()
}

/// Item 4a: the tier rule always uses self-host rules on a protected Linux
/// install; the deployment type is never read from this process's
/// environment, so a cloud-only tier is refused however root's shell is set.
#[test]
fn service_protected_readiness_edition_uses_self_host_rules_regardless_of_environment() {
    assert!(PROTECTED_LINUX_SELF_HOST);
    // Cloud-only tiers would pass the cloud rules; they never pass here.
    for tier in ["business", "studio", "solo", "hobby"] {
        assert!(!license_admits_isolation(&license(tier)), "{tier}");
    }
    // Self-host TEAM tiers pass (they would fail the cloud rules).
    for tier in ["self_host_pro", "oem", "partner"] {
        assert!(license(tier).validate_tier_deployment_binding(false).is_err(), "{tier} is self-host only");
        assert!(license_admits_isolation(&license(tier)), "{tier}");
    }
    // Personal profiles never pass.
    for tier in ["personal_pro_self_host", "open_source"] {
        assert!(!license_admits_isolation(&license(tier)), "{tier}");
    }
    // The module never consults the deployment environment helper.
    let source = include_str!("protected_linux_readiness.rs");
    assert!(!source.contains(concat!("is_self_host_", "deployment")));
    assert!(!source.contains(concat!("DUDUCLAW_", "DEPLOYMENT")));
}

/// Item 8: the composite check and the gateway call the same updater rule.
#[test]
fn service_protected_readiness_update_rule_is_the_shared_function() {
    let evidence = GenerationEvidence { generation: digest(1), product: Product::Duduclaw, version: version(),
        protocols: protocols(1) };
    let mut zero = evidence.clone(); zero.generation = digest(0);
    let mut protocol = evidence.clone(); protocol.protocols.egress = 2;
    for (evidence, expected) in [(evidence, true), (zero, false), (protocol, false)] {
        assert_eq!(compatible_update(&evidence, VERSION), expected);
        assert_eq!(duduclaw_updated::readiness::compatible_update(&evidence, VERSION), expected);
    }
    let source = include_str!("protected_linux_readiness.rs");
    assert!(source.contains("duduclaw_updated::readiness::compatible_update(evidence, installed_version)"));
}

#[test]
fn service_protected_readiness_output_is_fixed_text_with_stable_codes() {
    // Everything broken and every notice present.
    let mut fake = Fake::unreadable();
    fake.evidence = Some(HostEvidence { verdict_ok: false, checks: HostChecks::default(), landlock: Some((9, 7)) });
    fake.egress = Some(EgressView { host_public_addresses_empty: true });
    fake.trust = Some(TrustView { expires_at_unix: 1, frozen: true });
    fake.now = Some(2);
    let findings = fake.assess();
    assert!(!findings.is_empty());
    for finding in &findings {
        assert!(!finding.code.is_empty() && finding.code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{}", finding.code);
        // No paths or raw service output: fixed sentences only.
        assert!(!finding.message.contains('/') && !finding.message.contains('\n'), "{}", finding.message);
    }
    let (lines, failed, _) = render(&findings);
    assert_eq!(failed, Item::ALL.len());
    // One line per distinct message, under its item.
    let landlock = lines.iter().position(|line| line == "[未通過] Landlock").unwrap();
    assert_eq!(lines[landlock + 1], format!("  - {MSG_LANDLOCK}"));
    for line in &lines {
        assert!(line.starts_with("[未通過] ") || line.starts_with("  - ") || line.starts_with("[資訊] ")
            || line.starts_with("[通過] ") || line.starts_with("[待確認] "), "{line}");
    }
}

#[test]
fn service_protected_readiness_cloud_detection_is_exact_match() {
    for value in ["Google", "Google Compute Engine", "Amazon EC2", "DigitalOcean", "OracleCloud.com"] {
        assert!(cloud_dmi_value(value), "{value}");
    }
    for value in ["", "QEMU", "Googlex", "Not Google", "Amazon EC2 ", "innotek GmbH", "Dell Inc."] {
        assert!(!cloud_dmi_value(value), "{value}");
    }
}

/// Plan §4.4 root acceptance: stop spawnd, egress and the updater one at a
/// time and alter one published executable; each must fail exactly its item,
/// and the host must return to ready afterwards.
#[test]
#[ignore = "requires root native Linux with the complete isolation install (disposable VM, DUDUCLAW_ISO_DISPOSABLE=1); stops isolation services and temporarily replaces one published executable"]
fn iso_root_install_readiness_reports_each_failed_component() {
    use std::{io::Write, time::{Duration, Instant}};
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let live = || assess_host(&mut LiveSources::default(), env!("CARGO_PKG_VERSION"));
    let wait_ready = || {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let findings = live();
            if !findings.iter().any(Finding::is_failure) { return; }
            assert!(Instant::now() < deadline, "host did not return to ready: {findings:?}");
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    wait_ready();
    for (units, item) in [(["duduclaw-spawnd.service", "duduclaw-spawnd.socket"], Item::HostEvidence),
        (["duduclaw-egress.service", "duduclaw-egress.socket"], Item::EgressDns),
        (["duduclaw-updated.service", "duduclaw-updated.service"], Item::UpdateGeneration)] {
        for unit in units { checked("systemctl", &["stop", unit]).unwrap(); }
        // spawnd evidence is valid for at most one second.
        std::thread::sleep(Duration::from_millis(1500));
        let findings = live();
        for unit in units.iter().rev() { checked("systemctl", &["start", unit]).unwrap(); }
        assert!(findings.iter().any(|finding| finding.item == item && finding.is_failure()), "{units:?}: {findings:?}");
        wait_ready();
    }
    let path = "/usr/lib/duduclaw/current/duduclaw-mcp-bridge";
    let original = fs::read(path).unwrap();
    let mut altered = original.clone();
    altered.push(0);
    atomic_install(path, 0o755, |file| { file.write_all(&altered)?; Ok(()) }).unwrap();
    let findings = live();
    atomic_install(path, 0o755, |file| { file.write_all(&original)?; Ok(()) }).unwrap();
    assert!(findings.iter().any(|finding| finding.item == Item::Components && finding.code == "component_mismatch"),
        "{findings:?}");
    wait_ready();
}

#[test]
fn service_protected_readiness_group_detail_hides_non_refusal_errors() {
    let error = std::io::Error::other("private OS error and stderr");
    assert_eq!(group_detail(&Err(error.into())), Some("無法完成這項檢查。".to_owned()));
    assert_eq!(group_detail(&Err(DuDuClawError::Config("internal error".to_owned()))),
        Some("無法完成這項檢查。".to_owned()));
}

#[test]
fn service_protected_parse_runtime_tree_only_install_needs_update_lock() {
    assert!(needs_update_lock(true));
    assert!(!needs_update_lock(false));
}

#[test]
fn service_protected_readiness_directory_failures_name_fixed_paths() {
    for (path, _) in layout::DIRECTORIES {
        for mode_mismatch in [false, true] {
            let detail = group_detail(&Err(directory_failure(path, mode_mismatch))).unwrap();
            assert!(detail.ends_with(path), "{detail}");
            assert_eq!(detail, if mode_mismatch { format!("隔離服務目錄權限不符：{path}") }
                else { format!("隔離服務目錄不符或無法讀取：{path}") });
        }
    }
    let source = include_str!("protected_linux_readiness.rs");
    let directories = source.split("fn directories()").nth(1).unwrap().split("fn accounts()").next().unwrap();
    for message in ["解析服務目錄含有非預期內容：/usr/lib/duduclaw/parse-root",
        "解析執行環境驗證未通過：/usr/lib/duduclaw/parse-root-tree"] {
        assert!(directories.contains(message), "{message}");
    }
    assert_eq!(directories.matches("directory_failure(path, false)").count(), 3);
    assert!(directories.contains("directory_failure(path, true)"));
}

/// R8m 30b: after every platform check passed, the three gateway-view cases.
/// Only a fresh "ready" view keeps the employees-can-run sentence; the other
/// two are the same refusal (non-zero exit) as a failed check, with their
/// own message.
#[test]
fn service_protected_readiness_final_sentence_follows_the_gateway_view() {
    use duduclaw_gateway::isolation_readiness::published::{ComponentView, View, COMPONENTS};
    let message = |result: Result<()>| match result.unwrap_err() {
        DuDuClawError::Config(text) => text,
        other => panic!("{other:?}"),
    };
    // Ready: the existing sentence (or the migration-pending one), exit 0.
    let (lines, result) = gateway_verdict(false, GatewayView::Ready);
    assert_eq!((lines, result.is_ok()), (vec![MSG_READY.to_owned()], true));
    let (lines, result) = gateway_verdict(true, GatewayView::Ready);
    assert_eq!((lines, result.is_ok()), (vec![Reason::MIGRATION_PENDING_MESSAGE.to_owned()], true));
    // Not ready: the components and missing producers, then a refusal.
    let view = View { schema: 1, boot_id: "0123abcd-4567-89ef-0123-456789abcdef".into(), written_unix: 1, ready: false,
        components: COMPONENTS.iter().map(|name| ComponentView { name: (*name).into(),
            state: if *name == "mcp_boundary" { "pending".into() } else { "ready".into() },
            reason: (*name == "mcp_boundary").then(|| "pending_integration".into()) }).collect(),
        assembly_missing: vec!["connector_providers".into()] };
    for pending in [false, true] {
        let (lines, result) = gateway_verdict(pending, GatewayView::NotReady(view.clone()));
        assert_eq!(lines, ["[資訊] 平台檢查均已通過。Gateway 回報尚未就緒：mcp_boundary(pending_integration)；啟動組裝尚缺：connector_providers。"]);
        let text = message(result);
        assert!(text.ends_with(MSG_GATEWAY_NOT_READY), "{text}");
        // Unavailable (missing, stale, other boot, malformed): a refusal too.
        let (lines, result) = gateway_verdict(pending, GatewayView::Unavailable);
        assert_eq!(lines, [MSG_GATEWAY_UNAVAILABLE_INFO]);
        assert!(message(result).ends_with(MSG_GATEWAY_UNCONFIRMED));
    }
    assert_eq!(MSG_GATEWAY_UNAVAILABLE_INFO, "[資訊] 無法取得 Gateway 的就緒狀態（Gateway 未執行、剛啟動，或狀態檔無效）。");
    assert_eq!(MSG_GATEWAY_NOT_READY, "平台檢查均已通過，但 Gateway 尚未就緒，員工工作仍暫停。");
    // The platform-only result never claims that employees can run.
    assert_eq!(MSG_PLATFORM_READY, "平台檢查均已通過。");
    let source = include_str!("protected_linux_readiness.rs");
    let platform = source.split("pub(super) fn check_platform_all(").nth(1).unwrap().split("pub(super) fn check(").next().unwrap();
    assert!(platform.contains("MSG_PLATFORM_READY") && !platform.contains("MSG_READY }") && !platform.contains("gateway_view"));
}
