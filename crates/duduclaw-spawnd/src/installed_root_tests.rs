//! Installed-publication acceptance only. No fixture manifests, acceptance
//! records, accounts, or run receipts are manufactured by these tests.
use super::*;
use duduclaw_mcp_boundary::config::{RuntimeBridgeRegistry, runtime_projection, prepare_runtime_bridge};
use crate::{accounts::LinuxAccounts, registry::AccountHost};

fn require_disposable_root() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
        "set DUDUCLAW_ISO_DISPOSABLE=1 only on the installed acceptance VM");
    assert_eq!(unsafe { libc::geteuid() }, 0, "installed publication checks require root");
}
fn installed_bundle() -> Bundle {
    require_disposable_root();
    // Cold verification can exceed the freshness budget and only warm the
    // immutable cache. The second attempt must obtain fresh live F evidence;
    // missing files, signatures or daemons are failures, never skipped tests.
    match bundle() {
        Ok(value) => value,
        Err(_) => bundle().expect("fresh verified F generation and root publication are required"),
    }
}
fn projected_registration(record: &RuntimeRecord, manifest: &ReleaseManifest)
    -> duduclaw_mcp_boundary::config::RuntimeRegistration {
    let runtime_digest = match &record.executable {
        ExecutableRecord::Elf { digest, .. } => *digest,
        ExecutableRecord::Script { .. } => panic!("installed gateway MCP consumer requires an accepted ELF runtime"),
    };
    let bridge = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::McpBridge)
        .expect("signed bridge artifact");
    duduclaw_mcp_boundary::config::RuntimeRegistration { runtime: record.runtime.clone(),
        version: record.pinned_version.as_str().into(), runtime_digest,
        bridge_digest: bridge.sha256, bridge_executable: "/runtime/duduclaw-mcp-bridge".into() }
}

#[test]
#[ignore = "iso_root_: installed disposable Linux TEAM VM, signed F runtime policy and real MCP acceptance; publication layer only"]
fn iso_root_e2e_installed_publication_signed_policy_and_gateway_consumer() {
    let b = installed_bundle();
    let live = live_generation().expect("authenticated live updater probe");
    assert!(cache_matches(&VerifiedGeneration { digest: b.generation, manifest: b.manifest.clone() }, &live));
    assert!(!b.host.entitlements.is_empty(), "at least one confirmed employee entitlement is required");
    let (_, gid) = crate::server::gateway_identity().expect("installed gateway account");
    let report = GatewayPublication::read(gid).expect("fresh root-owned gateway publication");
    assert_eq!(report.generation, b.generation);
    assert_eq!(serde_json::to_value(&report.runtimes).unwrap(), serde_json::to_value(&b.policy.runtimes).unwrap());
    assert_eq!(serde_json::to_value(&report.entitlements).unwrap(), serde_json::to_value(&b.host.entitlements).unwrap());
    let policy = report.policy().expect("root policy validation");
    let registry = crate::gateway_mcp::InstalledRuntimeRegistry::new(b.generation, gid).expect("gateway publication consumer");
    let mut claude = 0;
    for entitlement in &report.entitlements {
        for profile in &entitlement.profiles {
            let record = b.policy.runtimes.iter().find(|r| r.profile == *profile).expect("signed entitled runtime");
            let (derived, _) = policy.derive(entitlement.employee, &record.runtime, false).expect("unambiguous entitled runtime");
            assert_eq!(derived.profile, *profile);
            if !record.mcp { continue; }
            let registration = projected_registration(record, &b.manifest);
            assert_eq!(registry.registration(&record.runtime).expect("accepted runtime bridge"), registration);
            let projection = runtime_projection(registration).unwrap();
            let acceptance = record.mcp_acceptance.as_ref().expect("real signed runtime MCP acceptance required");
            acceptance.validate(&projection).expect("exact CLI MCP child acceptance");
            let native = b.policy.native.iter().find(|n| n.profile == *profile).expect("signed native profile");
            acceptance.validate_launch(record, native).expect("acceptance binds actual argv and native sandbox");
            let arguments: Vec<_> = record.fixed_args.iter().map(|a| a.as_str()).collect();
            let expected: Vec<_> = projection.arguments.iter().map(String::as_str).collect();
            assert!(arguments.ends_with(&expected), "root argv must select the exact D config");
            if record.runtime == RuntimeKind::Claude { claude += 1; }
        }
    }
    assert!(claude > 0, "at least one confirmed MCP-enabled Claude runtime is required");
    let mut wrong = b.generation; wrong.0[0] ^= 1;
    assert!(crate::gateway_mcp::InstalledRuntimeRegistry::new(wrong, gid).is_err(), "mixed generations must refuse");
}

#[test]
#[ignore = "iso_root_: installed disposable Linux TEAM VM (files/systemd NSS only, the signed probe binary and template unit installed); publication layer only"]
fn iso_root_e2e_installed_publication_live_install_evidence() {
    let b = installed_bundle();
    let registry: crate::registry::Registry = read(ROOT, "registry.json").expect("actual root account registry");
    // R7c: no receipt. SSH denial comes from the probe unit through PID 1
    // (this process's own worker); external NSS is refused outright.
    let evidence = crate::ssh_evidence::installed();
    let mut accounts = LinuxAccounts::background(&evidence);
    registry.verify(&mut accounts).expect("current NSS, account lock, scheduler, sshd configuration and process readback");
    assert!(!b.host.entitlements.is_empty(), "confirmed employee required");
    for entitlement in &b.host.entitlements {
        let account = registry.lookup(entitlement.employee).expect("entitlement must have a ready permanent UID");
        // Allocation never waits for the probe (`login_denied` returns
        // pending at once); this test polls it, bounded, as a gateway retry would.
        let until = std::time::Instant::now() + std::time::Duration::from_millis(2 * crate::ssh_evidence::UNIT_DEADLINE_MS + 5_000);
        loop {
            match LinuxAccounts::admission(&evidence).login_denied(account) {
                Ok(()) => break,
                Err(error) if crate::accounts::probe_unresolved(&error) && std::time::Instant::now() < until =>
                    std::thread::sleep(std::time::Duration::from_millis(500)),
                Err(error) => panic!("the probe unit did not prove public-key login and forwarding denied for this account: {error}"),
            }
        }
        LinuxAccounts::admission(&evidence).verify_locked(account).expect("live account lockdown and SSH denial readback");
    }
    // R7a: the trampoline facts come from spawnd's own self-test through PID 1.
    let mut selftest = crate::selftest::SelfTest::default();
    let facts = selftest.ensure(&mut crate::selftest::InstalledHost::default())
        .expect("installed trampoline and self-test binaries match the signed generation");
    crate::selftest::readiness(&facts).check().expect("isolation self-test unit passed on this kernel");
    // R7b: the DNS route is no longer a receipt fact. Its probe inputs must be
    // installed: a policy v4 probe name and one configured route. (The probe
    // itself needs spawnd's own control connection; egress serves one
    // controller, so this process cannot send it.)
    let name = installed_dns_probe_name().expect("verified installed signed policy");
    assert!(name.is_some(), "the installed signed policy must be v4 (dns_probe_name)");
    configured_dns_route().expect("root-owned /etc/duduclaw/egress.json with one DNS route");
}

fn installed_run() -> RunHandle {
    let value = std::env::var("ISO_INSTALLED_RUN_ID").expect("ISO_INSTALLED_RUN_ID must name a real current MCP run (32 lower-case hex)");
    assert!(value.len() == 32 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid run ID");
    let mut bytes = [0u8; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let digit = |b: u8| if b.is_ascii_digit() { b - b'0' } else { b - b'a' + 10 };
        bytes[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    assert_ne!(bytes, [0; 16], "zero run IDs are not installed evidence");
    RunHandle(duduclaw_isolation::ids::RunId(bytes))
}

#[test]
#[ignore = "iso_root_: actual installed MCP run created within 60 seconds; ISO_INSTALLED_RUN_ID required; receipt consumption only"]
fn iso_root_e2e_installed_publication_actual_run_mcp_receipt() {
    let b = installed_bundle();
    let run = installed_run();
    let (_, gid) = crate::server::gateway_identity().expect("installed gateway identity");
    let receipt = McpRunReceipt::read(run, gid).expect("actual fresh root per-run mount receipt; no fixture is created");
    assert_eq!(receipt.generation, b.generation);
    let journal: Vec<crate::host_state::RunRecord> = read(ROOT, "runs.json").expect("actual root run journal");
    assert!(journal.iter().any(|entry| entry.run == run.0 && entry.employee == receipt.employee),
        "receipt must still belong to a journaled run");
    assert_eq!(receipt.runtime, RuntimeKind::Claude, "this acceptance requires an actual Claude run");
    let registry = crate::gateway_mcp::InstalledRuntimeRegistry::new(b.generation, gid).expect("live installed gateway registry");
    let prepared = prepare_runtime_bridge(&registry, &receipt.runtime, run, receipt.employee)
        .expect("D must accept exact run/employee/root mount and signed inheritance receipts");
    assert_eq!(prepared.receipt().run, run);
    assert_eq!(prepared.receipt().employee, receipt.employee);
    assert_eq!(prepared.projection().digest(), receipt.projection_digest);
    // This test proves receipt consumption, not that this process invoked a tool
    // or that the external run completed a model turn. Full-chain acceptance is separate.
}
