use super::*;

#[test]
fn service_protected_unit_has_fixed_root_launcher_and_hardening() {
    let unit = unit_text();
    let lines: Vec<_> = unit.lines().collect();
    for required in ["Slice=duduclaw-tcb.slice", "Type=simple", "ExecStart=/usr/local/libexec/duduclaw/protected-launcher /opt/duduclaw/bin/duduclaw run --yes",
        "StartLimitIntervalSec=3600", "StartLimitBurst=4",
        "KillMode=control-group", "Restart=on-failure", "ProtectSystem=strict",
        "ReadWritePaths=/var/lib/duduclaw /var/lib/duduclaw-protected", "ProtectHome=yes", "PrivateTmp=yes", "UMask=0077",
        "PrivateDevices=yes", "ProtectKernelTunables=yes", "ProtectKernelModules=yes",
        "ProtectControlGroups=yes", "LockPersonality=yes",
        "SystemCallArchitectures=native",
        "InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker"] {
        assert!(lines.contains(&required), "missing {required}");
    }
    assert!(!lines.iter().any(|line| line.split_once('=').is_some_and(|(key, _)| matches!(key, "User" | "Environment" | "EnvironmentFile"))));
    let unit_section = unit.split("[Service]").next().unwrap();
    assert!(unit_section.lines().any(|line| line == "StartLimitIntervalSec=3600"));
    assert!(unit_section.lines().any(|line| line == "StartLimitBurst=4"));
    assert!(!lines.iter().any(|line| line.starts_with("RestrictNamespaces=")));
    // R7g: RestrictSUIDSGID= makes every openat2 ENOSYS; the gateway needs openat2.
    let isolated = duduclaw_core::root_key::recovery::isolated_unit_text();
    for text in [unit.as_str(), isolated.as_str()] {
        assert!(!text.lines().any(|line| line.starts_with("RestrictSUIDSGID=")));
    }
    assert!(lines.contains(&format!("RestartForceExitStatus={}",
        duduclaw_core::root_key::RECOVERY_RESTART_EXIT_CODE).as_str()));
    assert!(!lines.iter().any(|line| line.starts_with("RestartPreventExitStatus=")
        || line.starts_with("SuccessExitStatus=")));
    assert!(LAUNCHER_TEXT.starts_with("#!/bin/sh\n"));
}

#[test]
fn service_protected_launcher_key_matches_intake_contract() {
    use duduclaw_core::root_key::{KEY_BYTES, KEY_MODE};
    assert!(LAUNCHER_TEXT.contains("chmod 0400"));
    assert!(LAUNCHER_TEXT.contains("0:0:400:1"));
    assert!(LAUNCHER_TEXT.contains(&format!("chmod {KEY_MODE:04o} ")));
    assert!(LAUNCHER_TEXT.contains(&format!("= 0:0:{KEY_MODE:o}:1 ]")));
    let generation = LAUNCHER_TEXT.lines().find(|line| line.trim_start().starts_with("dd ")).unwrap();
    let argument = |name: &str| generation.split_whitespace().find_map(|field| field.strip_prefix(name))
        .unwrap().parse::<u64>().unwrap();
    assert_eq!(argument("bs=") * argument("count="), KEY_BYTES);
    for file in ["$temp", "$KEY"] {
        assert!(LAUNCHER_TEXT.contains(&format!("stat -c %s \"{file}\")\" = {KEY_BYTES} ]")));
    }
}

/// Layout entries a v1.80-style single-binary install also creates or uses:
/// `/etc/duduclaw` holds the protected-mode marker (`write_mode_marker`), and
/// the single-binary gateway unit runs in `duduclaw-tcb.slice` (child of
/// `duduclaw.slice`). They can never prove an isolation install.
const SHARED_WITH_SINGLE_BINARY: &[&str] = &["/etc/duduclaw",
    "/etc/systemd/system/duduclaw.slice", "/etc/systemd/system/duduclaw-tcb.slice"];

/// Every isolation-only binary, unit and directory the installer layout
/// publishes is covered by a fact of `duduclaw_core::isolation_install`, so an installed
/// isolation component always switches the gateway onto the isolation path.
#[test]
fn service_protected_isolation_install_facts_cover_layout() {
    let facts = duduclaw_core::isolation_install::FACTS;
    for binary in super::layout::BINARIES {
        assert!(facts.contains(binary), "layout binary missing from isolation facts: {binary}");
    }
    let mut checked = 0;
    for (unit, _) in super::layout::UNITS {
        let path = format!("/etc/systemd/system/{unit}");
        if SHARED_WITH_SINGLE_BINARY.contains(&path.as_str()) { continue; }
        assert!(facts.contains(&path.as_str()), "layout unit missing from isolation facts: {unit}");
        checked += 1;
    }
    assert_eq!(checked, super::layout::UNITS.len() - 2, "only the two shared slices are excluded");
    for (path, _) in super::layout::DIRECTORIES {
        if SHARED_WITH_SINGLE_BINARY.contains(path) { continue; }
        if *path == "/var/lib/duduclaw-spawnd/audit" {
            assert!(facts.contains(&"/var/lib/duduclaw-spawnd"));
            assert!(!facts.contains(path), "the parent already covers the audit directory");
            continue;
        }
        assert!(facts.contains(path), "layout directory missing from isolation facts: {path}");
    }
    // The single-binary protected install writes none of them.
    let single = duduclaw_core::isolation_install::SINGLE_BINARY_LAYOUT;
    // (protected_linux_install.rs VERSION_FLOOR / VERSION_PENDING /
    // RECOVERY_BOUND and fresh::FRESH_PENDING are Linux-only private consts.)
    for path in [BINARY, LAUNCHER, UNIT_PATH, duduclaw_core::protected_mode::MODE_MARKER,
        "/var/lib/duduclaw-protected/version-floor", "/var/lib/duduclaw-protected/version-pending",
        "/etc/systemd/system/duduclaw-gateway.recovery-bound",
        "/var/lib/duduclaw-protected/fresh-install-pending", "/var/lib/duduclaw", "/var/lib/duduclaw-protected"]
        .into_iter().chain(SHARED_WITH_SINGLE_BINARY.iter().copied()) {
        assert!(!facts.contains(&path), "{path}");
        assert!(single.contains(&path), "single-binary layout list misses {path}");
    }
    assert!(unit_text().lines().any(|line| line == "Slice=duduclaw-tcb.slice"));
}

/// Defect 22: the isolated gateway unit's `Slice=` and the shipped slice units'
/// nesting place the gateway exactly in the cgroup line the root-key
/// attestation accepts (`root_key::GATEWAY_CGROUP_TCB_SLICE`), so the unit
/// text and the attestation cannot drift apart. systemd derives a slice's
/// parent from its name (`a-b.slice` under `a.slice`, `a.slice` under `-.slice`).
#[test]
fn service_protected_isolated_gateway_slice_matches_attested_cgroup() {
    let isolated = duduclaw_core::root_key::recovery::isolated_unit_text();
    let slices: Vec<&str> = isolated.lines().filter_map(|line| line.strip_prefix("Slice=")).collect();
    assert_eq!(slices.len(), 1, "exactly one Slice= line");
    let stem = slices[0].strip_suffix(".slice").expect("a slice unit");
    let parts: Vec<&str> = stem.split('-').collect();
    assert!(parts.iter().all(|part| !part.is_empty()), "{stem}");
    let mut path = String::new();
    for end in 1..=parts.len() {
        let name = format!("{}.slice", parts[..end].join("-"));
        let (_, text) = super::layout::UNITS.iter().find(|(unit, _)| *unit == name)
            .unwrap_or_else(|| panic!("{name}: not a slice unit the installer ships"));
        assert!(text.contains("\n[Slice]\n"), "{name}");
        path.push('/');
        path.push_str(&name);
    }
    assert_eq!(format!("0::{path}/{UNIT}"), duduclaw_core::root_key::GATEWAY_CGROUP_TCB_SLICE);
    assert_eq!(path, "/duduclaw.slice/duduclaw-tcb.slice");
    // The single-binary unit carries the same Slice= line: the same accepted path.
    assert!(unit_text().lines().any(|line| line == format!("Slice={}", slices[0])));
}

#[test]
fn service_protected_layout_and_facts_stay_outside_var_log() {
    // Ubuntu makes /var/log root:syslog 0775; a writable ancestor must remain refused.
    for path in super::layout::DIRECTORIES.iter().map(|(path, _)| *path)
        .chain(duduclaw_core::isolation_install::FACTS.iter().copied()) {
        assert!(!std::path::Path::new(path).ancestors().any(|ancestor| ancestor == std::path::Path::new("/var/log")), "{path}");
    }
    let directories = super::layout::DIRECTORIES;
    let parent = directories.iter().position(|(path, mode)| *path == "/var/lib/duduclaw-spawnd" && *mode == 0o700).unwrap();
    let audit = directories.iter().position(|(path, mode)| *path == "/var/lib/duduclaw-spawnd/audit" && *mode == 0o700).unwrap();
    assert_eq!(audit, parent + 1, "create the audit directory after its parent");
}

/// R7a: the installer creates the account `duduclaw-run-selftest` looks up
/// (`identity()`: `duduclaw_isolation::ids::SELFTEST_USER`), as one of the
/// layout's helper accounts (same creation, lockdown, readiness and removal
/// rules), installs the unit spawnd starts by name, and publishes its binary.
#[test]
fn service_protected_selftest_account_unit_and_binary_are_in_the_layout() {
    use duduclaw_isolation::ids::{SELFTEST_USER, ISOLATION_SELFTEST_UNIT, SERVICE_USERS};
    assert_eq!(super::layout::ACCOUNTS.iter().filter(|name| **name == SELFTEST_USER).count(), 1);
    assert!(!SERVICE_USERS.contains(&SELFTEST_USER), "never a service identity it must differ from");
    assert!(super::layout::UNITS.iter().any(|(name, _)| *name == ISOLATION_SELFTEST_UNIT));
    assert!(super::layout::BINARIES.contains(&"/usr/lib/duduclaw/duduclaw-run-selftest"));
    let facts = duduclaw_core::isolation_install::FACTS;
    assert!(facts.contains(&"/etc/systemd/system/duduclaw-isolation-selftest.service"));
    assert!(facts.contains(&"/usr/lib/duduclaw/duduclaw-run-selftest"));
    // The uninstall inventory owns (fences, stops and removes) the unit.
    assert!(super::lifecycle::owned_unit(ISOLATION_SELFTEST_UNIT));
}

/// R7c: the SSH denial probe runs as root (no account of its own); its template
/// unit and binary are in the layout, the install facts and the uninstall
/// inventory, and the lifecycle's literal instance prefix is the shared constant.
#[test]
fn service_protected_isolation_probe_unit_and_binary_are_in_the_layout() {
    use duduclaw_isolation::ids::{ISOLATION_PROBE_UNIT_PREFIX, ISOLATION_PROBE_UNIT_TEMPLATE};
    assert_eq!(super::layout::UNITS.iter().filter(|(name, _)| *name == ISOLATION_PROBE_UNIT_TEMPLATE).count(), 1);
    assert!(super::layout::BINARIES.contains(&"/usr/lib/duduclaw/duduclaw-isolation-probe"));
    assert!(!super::layout::ACCOUNTS.iter().any(|name| name.contains("probe")));
    let facts = duduclaw_core::isolation_install::FACTS;
    assert!(facts.contains(&"/etc/systemd/system/duduclaw-isolation-probe@.service"));
    assert!(facts.contains(&"/usr/lib/duduclaw/duduclaw-isolation-probe"));
    assert!(super::lifecycle::owned_unit(ISOLATION_PROBE_UNIT_TEMPLATE));
    assert!(super::lifecycle::owned_unit(&format!("{ISOLATION_PROBE_UNIT_PREFIX}duduclaw-emp-70000.service")));
    assert_eq!(ISOLATION_PROBE_UNIT_PREFIX, "duduclaw-isolation-probe@");
}

#[cfg(target_os = "linux")]
#[test]
fn service_protected_isolation_install_manifest_matches_migration() {
    assert_eq!(duduclaw_core::isolation_install::DURABLE_IMPORT_MANIFEST,
        duduclaw_migration::import_manifest::DURABLE_IMPORT_MANIFEST);
    assert_eq!(duduclaw_core::isolation_install::FACTS[0], duduclaw_isolation::ids::SPAWND_SOCKET);
}

/// v1.83 R4: the four readiness call sites (managed `install`, `install`
/// after migration, `readiness`, `upgrade` after migration) use the real
/// composite check; the placeholder that always refused is gone. The
/// composite itself is tested with injected sources in
/// `protected_linux_readiness_tests.rs` (Linux only).
/// R8m 30b: only the `readiness` command itself (without `--platform-only`)
/// uses the gateway-view check; the install/upgrade returns and
/// `--platform-only` use the platform result.
#[test]
fn service_protected_readiness_call_sites_use_installed_composite() {
    let install = include_str!("protected_linux_install.rs");
    assert_eq!(install.matches("readiness::check_platform_all(&mut readiness::InstalledComposite)").count(), 4);
    assert_eq!(install.matches("readiness::check_platform_all(").count(), 4, "no other composite at any call site");
    assert_eq!(install.matches("readiness::check(&mut readiness::InstalledComposite)").count(), 1);
    assert_eq!(install.matches("readiness::check(").count(), 1, "only the readiness command reads the gateway view");
    assert!(install.contains("if platform_only { return readiness::check_platform_all(&mut readiness::InstalledComposite); }\n    readiness::check(&mut readiness::InstalledComposite)\n}"));
    let readiness = include_str!("protected_linux_readiness.rs");
    for removed in ["MissingComposite", "尚未接上完整隔離就緒檢查"] {
        assert!(!install.contains(removed) && !readiness.contains(removed), "{removed}");
    }
    assert!(readiness.contains("impl CompositeReadiness for InstalledComposite"));
}

/// R8m 30b: the installer's S7 gate keeps its platform function (no gateway
/// unit required) and S8/S9 the platform result; no bootstrap step ever
/// waits for the gateway's own readiness view.
#[test]
fn service_protected_bootstrap_steps_use_platform_readiness_only() {
    let bootstrap = include_str!("protected_linux_bootstrap.rs");
    assert!(bootstrap.contains("readiness::check_platform(&mut readiness::InstalledComposite)\n        }),"), "S7 perform");
    assert!(bootstrap.contains("Step::Readiness => with_version_lock(|| readiness::check_platform(&mut readiness::InstalledComposite)),"),
        "S7 invariants");
    assert!(bootstrap.contains("// service may still be assembling in the background.\n    readiness::check_platform_all(&mut readiness::InstalledComposite)\n}"),
        "S8");
    assert!(bootstrap.contains("verify_identities(&service_names())?;\n    readiness::check_platform_all(&mut readiness::InstalledComposite)?;"),
        "S9");
    assert_eq!(bootstrap.matches("readiness::check(").count(), 0, "no bootstrap step reads the gateway view");
    assert_eq!(bootstrap.matches("gateway_view").count(), 0);
}

#[test]
fn service_protected_account_is_nonlogin_unprivileged() {
    for shell in ["/usr/sbin/nologin", "/sbin/nologin", "/bin/false", "/usr/bin/false"] {
        assert!(validate_account(999, 999, shell, "duduclaw systemd-journal").is_ok());
    }
    assert!(validate_account(0, 999, "/bin/false", "duduclaw").is_err());
    assert!(validate_account(999, 0, "/bin/false", "duduclaw").is_err());
    for shell in ["/bin/bash", "", "/tmp/nologin", "/usr/sbin/nologin-evil"] {
        assert!(validate_account(999, 999, shell, "duduclaw").is_err());
    }
    for group in ["sudo", "wheel", "admin", "adm", "docker", "lxd", "incus", "libvirt"] {
        assert!(validate_account(999, 999, "/bin/false", &format!("duduclaw {group}")).is_err());
    }
    assert!(validate_account(999, 999, "/bin/false", "docker-readonly").is_ok());
}

#[test]
fn service_protected_operator_gate_precedes_platform_or_filesystem_work() {
    for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
        let _session = crate::ai_session_guard::test_session(&[(marker, "")]);
        assert!(install(InstallOptions { enable_now: false, migrate_from: None, force_migrate: false, i_verified_this_binary: false, allow_missing_runtime: false }).is_err());
        assert!(uninstall().is_err());
        assert!(readiness().is_err());
    }
}

#[test]
fn service_protected_cli_options_require_protected_mode() {
    use clap::Parser;
    crate::test_support::run_on_big_stack(|| {
    for args in [vec!["duduclaw", "service", "install", "--enable-now"],
        vec!["duduclaw", "service", "install", "--migrate-from", "/tmp/home"],
        vec!["duduclaw", "service", "install", "--protected-linux", "--force-migrate"]] {
        assert!(crate::Cli::try_parse_from(args).is_err());
    }
    assert!(crate::Cli::try_parse_from(["duduclaw", "service", "install", "--protected-linux", "--enable-now", "--migrate-from", "/tmp/home", "--force-migrate"]).is_ok());
    assert!(crate::Cli::try_parse_from(["duduclaw", "service", "uninstall", "--protected-linux"]).is_ok());
    assert!(crate::Cli::try_parse_from(["duduclaw", "service", "readiness"]).is_err());
    assert!(crate::Cli::try_parse_from(["duduclaw", "service", "readiness", "--protected-linux"]).is_ok());
    });
}

#[test]
fn service_protected_upgrade_cli_requires_explicit_mode() {
    use clap::Parser;
    crate::test_support::run_on_big_stack(|| {
        assert!(crate::Cli::try_parse_from(["duduclaw", "service", "upgrade"]).is_err());
        assert!(crate::Cli::try_parse_from(["duduclaw", "service", "upgrade", "--protected-linux"]).is_ok());
        assert!(crate::Cli::try_parse_from(["duduclaw", "service", "upgrade", "--protected-linux", "--version", "1.79.3"]).is_ok());
        assert!(crate::Cli::try_parse_from(["duduclaw", "service", "install", "--i-verified-this-binary"]).is_err());
        assert!(crate::Cli::try_parse_from(["duduclaw", "service", "install", "--protected-linux", "--i-verified-this-binary"]).is_ok());
    });
}

#[tokio::test]
async fn service_protected_upgrade_operator_gate_precedes_download() {
    for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
        let _session = crate::ai_session_guard::test_session(&[(marker, "")]);
        let error = upgrade(Some("1.79.3"), false).await.unwrap_err();
        assert!(error.to_string().contains("AI 員工不能"));
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn service_protected_upgrade_requires_stable_version_before_ipc() {
    let _session = crate::ai_session_guard::test_session(&[]);
    // Omitting --version is decided only after routing from installed state
    // (latest on single-binary installs, refused for updater-managed ones),
    // so an unprivileged test stops at the root gate instead.
    if unsafe { libc::geteuid() } != 0 {
        assert!(upgrade(None, false).await.unwrap_err().to_string().contains("sudo"));
    }
    for version in ["1.80.0-beta", "1.80.0+build", "../update", "https://example.com/update"] {
        assert!(upgrade(Some(version), false).await.unwrap_err().to_string().contains("正式版"));
    }
}

#[test]
fn service_protected_linux_allow_missing_runtime_cli() {
    use clap::Parser;
    crate::test_support::run_on_big_stack(|| {
        for action in ["install", "upgrade"] {
            assert!(crate::Cli::try_parse_from(["duduclaw", "service", action, "--protected-linux", "--allow-missing-runtime"]).is_ok());
            assert!(crate::Cli::try_parse_from(["duduclaw", "service", action, "--allow-missing-runtime"]).is_err());
        }
    });
}

#[test]
fn service_protected_parse_runtime_verify_does_not_lock_or_reread_state() {
    let source = include_str!("protected_linux_install.rs");
    let function = source.split("fn parse_runtime_tree(install: bool)").nth(1).unwrap()
        .split("\n}\n").next().unwrap();
    let (before, after) = function.split_once("let _update_lock = if needs_update_lock(install) {").unwrap();
    let (install, verify) = after.split_once("} else { None };").unwrap();
    assert!(!before.contains(".lock()"));
    assert_eq!(install.matches(".lock()").count(), 1);
    assert!(!verify.contains(".lock()"));
    assert_eq!(function.matches("json(\"state.json\")").count(), 1);
    assert!(verify.contains("state.validate()"));
    assert!(verify.contains("state.pending.is_some() || current != state.effective_digest()"));
    assert!(verify.contains("if install { \"--install\" } else { \"--verify\" }"));
}
