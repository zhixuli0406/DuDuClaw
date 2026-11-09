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
        "ProtectControlGroups=yes", "RestrictSUIDSGID=yes", "LockPersonality=yes",
        "SystemCallArchitectures=native",
        "InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker"] {
        assert!(lines.contains(&required), "missing {required}");
    }
    assert!(!lines.iter().any(|line| line.split_once('=').is_some_and(|(key, _)| matches!(key, "User" | "Environment" | "EnvironmentFile"))));
    let unit_section = unit.split("[Service]").next().unwrap();
    assert!(unit_section.lines().any(|line| line == "StartLimitIntervalSec=3600"));
    assert!(unit_section.lines().any(|line| line == "StartLimitBurst=4"));
    assert!(!lines.iter().any(|line| line.starts_with("RestrictNamespaces=")));
    assert!(lines.contains(&format!("RestartForceExitStatus={}",
        duduclaw_core::root_key::RECOVERY_RESTART_EXIT_CODE).as_str()));
    assert!(!lines.iter().any(|line| line.starts_with("RestartPreventExitStatus=")
        || line.starts_with("SuccessExitStatus=")));
    assert!(LAUNCHER_TEXT.starts_with("#!/bin/sh\n"));
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
    assert!(upgrade(None, false).await.unwrap_err().to_string().contains("--version"));
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
