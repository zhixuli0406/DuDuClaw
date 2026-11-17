//! R7c: the shipped `duduclaw-isolation-probe` binary on a disposable Linux VM
//! with a running sshd, run as a transient systemd service under every
//! `[Service]` setting of the shipped template unit. Guarded by
//! DUDUCLAW_ISO_DISPOSABLE=1 because it may create a locked employee-range
//! account and spawnd's sshd drop-in, and reloads sshd. It removes only what it
//! created. It never creates an account that can log in.
#![cfg(target_os = "linux")]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The shipped template unit (spawnd's `ISOLATION_PROBE_UNIT_TEXT`).
const PROBE_UNIT_TEXT: &str =
    include_str!("../../../scripts/linux-protected/spawnd/duduclaw-isolation-probe@.service");
/// `[Service]` settings of the unit not passed to `systemd-run`: the copy is
/// executed instead of the installed path (with the account as its argument,
/// like `%i`); `--pipe` replaces the journal so the JSON line comes back; the
/// TCB slice need not be installed for this test; an empty `Environment=` is
/// the default. Every other setting is applied.
const NOT_APPLIED: [&str; 6] = ["ExecStart", "StandardInput", "StandardOutput", "StandardError", "Slice", "Environment"];
/// Equal to spawnd's `SSH_DROPIN` (crates/duduclaw-spawnd/src/accounts.rs) at its path.
const SSH_DROPIN: &str = "DenyUsers duduclaw-emp-* duduclaw duduclaw-runsup duduclaw-egress duduclaw-dl\nMatch User duduclaw-emp-*\n    DisableForwarding yes\n    PermitTTY no\nMatch all\n";
const DROPIN_PATH: &str = "/etc/ssh/sshd_config.d/10-duduclaw.conf";
/// Disposable, locked, nologin account in the employee uid range.
const ACCOUNT: &str = "duduclaw-emp-probetest";
/// Exit codes of the probe (crates/duduclaw-isolation/src/bin/probe.rs).
const EXIT_IDENTITY: i32 = 3;
const EXIT_EFFECTIVE_CONFIG: i32 = 7;

fn unit_service_settings() -> Vec<&'static str> {
    let (_, service) = PROBE_UNIT_TEXT.split_once("\n[Service]\n").expect("[Service] section");
    service.lines().filter(|line| !line.is_empty() && !line.starts_with('#'))
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !NOT_APPLIED.contains(&line.split_once('=').expect("Key=Value").0))
        .collect()
}

/// Bounded run with a fixed environment; returns (exit code, stdout, stderr).
fn bounded(mut command: Command) -> (Option<i32>, String, String) {
    let mut child = command.env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").env("LC_ALL", "C")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("start host tool");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while child.try_wait().expect("tool status").is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("host tool exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("tool output");
    (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned())
}
fn tool(program: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = Command::new(program);
    command.args(args);
    bounded(command)
}

fn require_disposable_root() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
        "set DUDUCLAW_ISO_DISPOSABLE=1 only on a disposable root VM");
    let (_, uid, _) = tool("/usr/bin/id", &["-u"]);
    assert_eq!(uid.trim(), "0", "run iso_root_ tests as root");
    for binary in ["/usr/sbin/sshd", "/usr/bin/ssh", "/usr/bin/ssh-keygen", "/usr/bin/journalctl",
        "/usr/bin/systemd-run", "/usr/bin/systemctl", "/usr/bin/getent", "/usr/sbin/useradd", "/usr/sbin/groupadd",
        "/usr/sbin/userdel", "/usr/sbin/groupdel", "/usr/bin/chage"] {
        let meta = std::fs::metadata(binary).unwrap_or_else(|_| panic!("{binary} is required"));
        assert!(meta.is_file() && meta.mode() & 0o111 != 0, "{binary} must be executable");
    }
}

/// The active sshd service unit, if any (Debian/Ubuntu `ssh`, others `sshd`).
fn sshd_unit() -> Option<&'static str> {
    ["ssh.service", "sshd.service"].into_iter()
        .find(|unit| tool("/usr/bin/systemctl", &["is-active", "--quiet", unit]).0 == Some(0))
}
fn reload_sshd() -> bool {
    let Some(unit) = sshd_unit() else { return false };
    let reloaded = tool("/usr/bin/systemctl", &["reload", unit]).0 == Some(0);
    // sshd re-executes itself on SIGHUP; let the listener come back.
    std::thread::sleep(std::time::Duration::from_millis(500));
    reloaded
}
fn effective_config(account: &str) -> String {
    let context = format!("user={account},host=localhost,addr=127.0.0.1");
    let (code, stdout, stderr) = tool("/usr/sbin/sshd", &["-T", "-C", &context]);
    assert_eq!(code, Some(0), "sshd -T: {stderr}");
    stdout
}

/// Removes only what the test created, on every exit (panics included).
#[derive(Default)]
struct Created { account: bool, dropin: bool, copy: Option<PathBuf> }
impl Drop for Created {
    fn drop(&mut self) {
        if self.dropin && std::fs::remove_file(DROPIN_PATH).is_ok() { let _ = reload_sshd(); }
        if self.account {
            let _ = tool("/usr/sbin/userdel", &[ACCOUNT]);
            let _ = tool("/usr/sbin/groupdel", &[ACCOUNT]);
        }
        if let Some(directory) = &self.copy { let _ = std::fs::remove_dir_all(directory); }
    }
}

fn passwd_uid(name: &str) -> Option<u32> {
    let (code, stdout, _) = tool("/usr/bin/getent", &["passwd", name]);
    match code {
        Some(0) => Some(stdout.trim().split(':').nth(2).expect("passwd row").parse().expect("uid")),
        Some(2) => None,
        other => panic!("getent passwd {name}: {other:?}"),
    }
}
/// The locked, nologin employee-range account, created like spawnd's
/// `create_locked` when absent.
fn ensure_account(created: &mut Created) {
    use duduclaw_isolation::ids::{EMPLOYEE_UID_MAX, EMPLOYEE_UID_MIN};
    if let Some(uid) = passwd_uid(ACCOUNT) {
        assert!((EMPLOYEE_UID_MIN..=EMPLOYEE_UID_MAX).contains(&uid), "existing {ACCOUNT} outside the employee range");
        return;
    }
    let free = |database: &str, id: &str| tool("/usr/bin/getent", &[database, id]).0 == Some(2);
    let uid = (EMPLOYEE_UID_MIN..=EMPLOYEE_UID_MAX).rev().map(|uid| uid.to_string())
        .find(|uid| free("passwd", uid) && free("group", uid)).expect("a free employee-range uid");
    let (code, _, stderr) = tool("/usr/sbin/groupadd", &["--gid", &uid, ACCOUNT]);
    assert_eq!(code, Some(0), "groupadd: {stderr}");
    created.account = true;
    let (code, _, stderr) = tool("/usr/sbin/useradd", &["--uid", &uid, "--gid", &uid, "--no-user-group",
        "--no-create-home", "--no-log-init", "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0",
        "--key", "CREATE_MAIL_SPOOL=no", "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin",
        "--password", "!*", "--expiredate", "1970-01-02", ACCOUNT]);
    assert_eq!(code, Some(0), "useradd: {stderr}");
    let (code, _, stderr) = tool("/usr/bin/chage", &["-E", "1", ACCOUNT]);
    assert_eq!(code, Some(0), "chage: {stderr}");
}
/// spawnd's drop-in, written only when absent; a different existing file is
/// never touched (the test refuses instead).
fn ensure_dropin(created: &mut Created) {
    match std::fs::read_to_string(DROPIN_PATH) {
        Ok(existing) => assert_eq!(existing, SSH_DROPIN, "{DROPIN_PATH} exists with other content; not modified"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(DROPIN_PATH, SSH_DROPIN).expect("write sshd drop-in");
            created.dropin = true;
            std::fs::set_permissions(DROPIN_PATH, std::fs::Permissions::from_mode(0o644)).expect("drop-in mode");
            assert!(reload_sshd(), "reload the running sshd");
        }
        Err(error) => panic!("{DROPIN_PATH}: {error}"),
    }
    assert!(effective_config(ACCOUNT).lines().any(|line| line.starts_with("denyusers ") && line.contains("duduclaw-emp-*")),
        "sshd must include /etc/ssh/sshd_config.d/*.conf");
}

/// A root-owned 0755 copy of the built probe in a fresh root-owned 0700
/// directory under /var/lib (PrivateTmp/ProtectHome hide /tmp, /home, /root).
fn private_copy(created: &mut Created) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos();
    let directory = Path::new("/var/lib").join(format!("duduclaw-probe-unit-{}-{nanos}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&directory).expect("fresh private directory");
    created.copy = Some(directory.clone());
    let binary = directory.join("duduclaw-isolation-probe");
    std::fs::copy(env!("CARGO_BIN_EXE_duduclaw-isolation-probe"), &binary).expect("copy probe binary");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("binary mode");
    binary
}
/// The probe for `account` under every shipped `[Service]` setting.
fn run_unit(binary: &Path, account: &str) -> (Option<i32>, String, String) {
    let mut command = Command::new("/usr/bin/systemd-run");
    command.args(["--quiet", "--wait", "--collect", "--pipe"]);
    for setting in unit_service_settings() { command.args(["-p", setting]); }
    command.arg(binary).arg(account);
    bounded(command)
}
fn assert_failed_at((code, stdout, stderr): (Option<i32>, String, String), exit: i32, stage: &str) {
    assert_eq!(code, Some(exit), "stdout {stdout:?} stderr {stderr:?}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.lines().any(|line| line == format!(r#"{{"code":"ssh_probe_failed","stage":"{stage}"}}"#)), "{stderr}");
}

#[test]
#[ignore = "iso_root_: disposable root Linux VM with systemd, a running sshd on loopback logging to the journal; DUDUCLAW_ISO_DISPOSABLE=1; --test-threads=1"]
fn iso_root_isolation_probe_denied_account_passes_under_the_shipped_unit() {
    require_disposable_root();
    assert!(sshd_unit().is_some(), "a running ssh.service or sshd.service is required");
    let settings = unit_service_settings();
    for key in ["User", "Group", "RestrictSUIDSGID"] {
        assert!(!settings.iter().any(|setting| setting.split_once('=').unwrap().0 == key), "{key}: {settings:?}");
    }
    for expected in ["CapabilityBoundingSet=", "IPAddressDeny=any", "IPAddressAllow=localhost", "Type=oneshot"] {
        assert!(settings.contains(&expected), "{expected}: {settings:?}");
    }
    let mut created = Created::default();
    ensure_account(&mut created);
    ensure_dropin(&mut created);
    let binary = private_copy(&mut created);

    // Positive: the live sshd refuses both attempts and logs DenyUsers for them.
    let (code, stdout, stderr) = run_unit(&binary, ACCOUNT);
    assert_eq!(code, Some(0), "probe failed: stdout {stdout:?} stderr {stderr:?}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one JSON line: {stdout}");
    let value: serde_json::Value = serde_json::from_str(lines[0]).expect("JSON line");
    let mut keys: Vec<&str> = value.as_object().expect("object").keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["account", "port", "result", "version"]);
    assert_eq!(value["version"], 1);
    assert_eq!(value["account"], ACCOUNT);
    assert_eq!(value["result"], "denied");
    assert!(value["port"].as_u64().is_some_and(|port| (1..=65535).contains(&port)));

    // Negative control 1: a name outside the employee rule stops at identity.
    for account in ["root", "duduclaw-runsup", "duduclaw-emp-Probetest"] {
        assert_failed_at(run_unit(&binary, account), EXIT_IDENTITY, "identity");
    }
    // Negative control 2: without the drop-in (only when this test created it)
    // the effective configuration no longer denies, so the probe must stop
    // there and never report "denied".
    if created.dropin {
        std::fs::remove_file(DROPIN_PATH).expect("remove the test's drop-in");
        created.dropin = false;
        assert!(reload_sshd(), "reload the running sshd");
        assert!(!effective_config(ACCOUNT).lines().any(|line| line.starts_with("denyusers ") && line.contains("duduclaw-emp-*")),
            "another sshd configuration still denies duduclaw-emp-*; the negative control cannot run here");
        assert_failed_at(run_unit(&binary, ACCOUNT), EXIT_EFFECTIVE_CONFIG, "effective_config");
    } else {
        eprintln!("{DROPIN_PATH} was installed before this test; the drop-in-absent control was not run");
    }
}
