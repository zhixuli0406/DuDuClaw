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
    ReleaseManifest { schema_version: 1, product: Product::Duduclaw, version: version(),
        channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
        minimum_version: version(), serial: 1, created_at_unix: 100, expires_at_unix: 5000,
        artifacts: component_paths().map(|path| {
            let target = component_target(path).unwrap();
            ReleaseArtifact { id: duduclaw_updated::publication::artifact_name(&target).unwrap(), sha256: digest(9),
                size_bytes: 7, mode: 0o755, install_target: target }
        }).collect(),
        protocols: protocols(1), migrations: Vec::new() }
}

fn all_checks() -> HostChecks {
    HostChecks { protocol: duduclaw_isolation::ipc::PROTOCOL_VERSION, registry_verified: true, accounts_locked: true,
        ssh_live_negative_checks: true, cron_at_acl_and_jobs_verified: true, external_iam_reservation_verified: true,
        trampoline_verified: true, landlock_self_test: true, namespace_self_test: true, egress_verified: true,
        dns_route_verified: true, storage_verified: true, resources_verified: true, startup_reconciled: true,
        process_scan_verified: true, egress_second_layer: EgressSecondLayer::Verified }
}

/// `None` = the source cannot be read.
struct Fake {
    probe: Option<GenerationEvidence>,
    state: Option<UpdateStateView>,
    current: Option<Sha256Digest>,
    manifest: Option<ReleaseManifest>,
    broken: Vec<&'static str>,
    evidence: Option<HostEvidence>,
    inactive: Vec<&'static str>,
    egress: Option<EgressView>,
    broker: Option<bool>,
    cloud: bool,
    trust: Option<TrustView>,
    now: Option<u64>,
    migration: Option<Option<MigrationState>>,
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
            evidence: Some(HostEvidence { verdict_ok: true, checks: all_checks(), landlock: Some((6, 6)) }),
            inactive: Vec::new(),
            egress: Some(EgressView { host_public_addresses_empty: false }),
            broker: Some(false),
            cloud: false,
            trust: Some(TrustView { expires_at_unix: 2000, frozen: false }),
            now: Some(1000),
            migration: Some(None),
            team: Some(true),
        }
    }
    fn unreadable() -> Self {
        Self { probe: None, state: None, current: None, manifest: None, broken: Vec::new(), evidence: None,
            inactive: vec!["duduclaw-egress.socket", "duduclaw-egress.service", "duduclaw-parse.socket",
                "duduclaw-dns-broker.socket"],
            egress: None, broker: None, cloud: true, trust: None, now: None, migration: None, team: None }
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
        if self.broken.contains(&path) || artifact.sha256 != digest(9) { return Err(refused("mismatch")); }
        Ok(())
    }
    fn host_evidence(&mut self) -> Result<HostEvidence> { unavailable(self.evidence.clone()) }
    fn unit_active(&mut self, unit: &'static str) -> Result<()> {
        if self.inactive.contains(&unit) { Err(refused("inactive")) } else { Ok(()) }
    }
    fn egress_config(&mut self) -> Result<EgressView> { unavailable(self.egress) }
    fn dns_broker_configured(&mut self) -> Result<bool> { unavailable(self.broker) }
    fn cloud_host(&mut self) -> bool { self.cloud }
    fn trust_root(&mut self) -> Result<TrustView> { unavailable(self.trust) }
    fn now(&mut self) -> Result<u64> { unavailable(self.now) }
    fn migration_state(&mut self) -> Result<Option<MigrationState>> { unavailable(self.migration) }
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
        assert!(!targets.contains(&target), "{path}");
        let name = duduclaw_updated::publication::artifact_name(&target).unwrap();
        let file = Path::new(path).file_name().unwrap().to_str().unwrap();
        match path {
            "/usr/libexec/duduclaw-updated-launch" => assert_eq!(name, "duduclaw-updated"),
            BINARY => assert_eq!(name, "duduclaw"),
            _ => assert_eq!(name, file, "{path}"),
        }
        targets.push(target);
    }
    assert_eq!(targets.len(), layout::BINARIES.len() + 1);
    assert!(component_target("/usr/lib/duduclaw/duduclaw-spawnd.bak").is_none());
    assert!(component_target("/usr/lib/duduclaw/duduclaw-spawnd/").is_none());
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
    for (flip, code) in cases {
        let mut fake = Fake::healthy();
        let evidence = fake.evidence.as_mut().unwrap();
        flip(&mut evidence.checks);
        evidence.verdict_ok = false;
        assert_eq!(failures(&fake.assess()), [(Item::HostEvidence, code)]);
    }
    let messages: Vec<_> = host_checks(&HostChecks::default()).into_iter().map(|(_, _, message)| message).collect();
    assert!(messages.contains(&"員工帳號的 SSH 登入封鎖未通過實測。"));
    let mut fake = Fake::healthy();
    fake.evidence.as_mut().unwrap().checks.protocol = 2;
    fake.evidence.as_mut().unwrap().verdict_ok = false;
    assert_eq!(failures(&fake.assess()), [(Item::HostEvidence, "host_protocol_mismatch")]);
    // Every boolean true but stale/other-boot evidence: still not ready.
    let mut fake = Fake::healthy(); fake.evidence.as_mut().unwrap().verdict_ok = false;
    assert_eq!(failures(&fake.assess()), [(Item::HostEvidence, "host_evidence_stale")]);
    let mut fake = Fake::healthy(); fake.evidence = None;
    assert_eq!(failures(&fake.assess()),
        [(Item::HostEvidence, "host_evidence_unavailable"), (Item::Landlock, "landlock_unverified")]);
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
        assert_eq!(failures(&findings), [(Item::Landlock, "landlock_self_test_failed")], "{landlock:?} {self_test}");
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
    // The resolved route does not need the broker socket.
    let mut fake = Fake::healthy(); fake.inactive = vec!["duduclaw-dns-broker.socket"];
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

#[test]
fn service_protected_readiness_migration_pending_changes_summary_only() {
    use MigrationState as S;
    for state in [S::AwaitingAdministrator, S::ContentImported] {
        let mut fake = Fake::healthy(); fake.migration = Some(Some(state));
        let findings = fake.assess();
        assert!(failures(&findings).is_empty(), "{state:?}");
        assert_eq!(codes(&findings, Severity::Pending), [Reason::MIGRATION_PENDING_CODE]);
        let (lines, failed, pending) = render(&findings);
        assert_eq!((failed, pending), (0, true));
        assert!(lines.contains(&"[待確認] 資料遷移".to_owned()), "{lines:?}");
        assert!(FakeComposite(fake).verify().is_ok());
    }
    for state in [None, Some(S::Complete)] {
        let mut fake = Fake::healthy(); fake.migration = Some(state);
        assert!(fake.assess().is_empty());
    }
    // Before the new platform was verified for the migration: not ready.
    for state in [S::Planned, S::OldEnvironmentStopped, S::LegacyQuarantined, S::KeysRotated,
        S::TokenRevocationRecorded, S::EmployeeProposalsStaged, S::ContentArchived, S::ConfirmationReady,
        S::IsolationVerified] {
        let mut fake = Fake::healthy(); fake.migration = Some(Some(state));
        assert_eq!(failures(&fake.assess()), [(Item::Migration, "migration_incomplete")], "{state:?}");
    }
    let mut fake = Fake::healthy(); fake.migration = None;
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
