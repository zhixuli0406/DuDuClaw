//! Real login/scheduler attacks against a disposable installed employee account.
//! The fixed localhost fixture and positive controls are mandatory. These tests
//! never equate a missing executable or unreachable service with account denial.
use super::*;
use std::{fs::OpenOptions, io::{Read, Write}, os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf, process::{Command, Stdio}, sync::atomic::{AtomicUsize, Ordering}};
const FIXTURE: &str = "/run/duduclaw-iso-account-test";
const AUTHORIZED: &str = "/run/duduclaw-iso-account-test/authorized_keys/%u";
const EMPTY_CRON: &str = "# duduclaw isolation acceptance control\n";
struct Output { code: i32, stdout: String, stderr: String }
struct Harness { directory: PathBuf, sequence: AtomicUsize, ssh_base: Vec<String>, sockets: Vec<PathBuf> }
impl Harness {
    fn run(&self, binary: &str, args: &[&str], input: &[u8]) -> Output {
        // All programs are explicit trusted host executables; no PATH lookup or
        // shell interpolation. Coreutils timeout bounds both control and attack.
        let id = self.sequence.fetch_add(1, Ordering::Relaxed);
        let out = self.directory.join(format!("{id}.stdout"));
        let err = self.directory.join(format!("{id}.stderr"));
        let stdin = self.directory.join(format!("{id}.stdin"));
        let create = |p: &Path| OpenOptions::new().write(true).create_new(true).mode(0o600).open(p).unwrap();
        create(&stdin).write_all(input).unwrap();
        let status = Command::new("/usr/bin/timeout").args(["--signal=TERM", "--kill-after=2", "12", binary])
            .args(args).env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").env("LC_ALL", "C")
            .stdin(Stdio::from(File::open(&stdin).unwrap())).stdout(Stdio::from(create(&out)))
            .stderr(Stdio::from(create(&err))).status().expect("execute installed acceptance tool");
        let read = |p: &Path| {
            let mut bytes = Vec::new(); File::open(p).unwrap().take(128 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            assert!(bytes.len() <= 128 * 1024, "unexpected tool output bound");
            String::from_utf8(bytes).expect("C locale tool output")
        };
        Output { code: status.code().unwrap_or(-1), stdout: read(&out), stderr: read(&err) }
    }
    fn as_user(&self, user: &str, uid: u32, binary: &str, args: &[&str], input: &[u8]) -> Output {
        let runtime = format!("XDG_RUNTIME_DIR=/run/user/{uid}");
        let bus = format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus");
        let uid_text = uid.to_string();
        let gid_text = account(user).gid.to_string();
        let mut invocation = vec!["--reuid", &uid_text, "--regid", &gid_text, "--clear-groups", "--", "/usr/bin/env", &runtime, &bus, "/bin/sh", "-c",
            "printf 'ISO_ACCOUNT_EXEC:%s\\n' \"$(/usr/bin/id -u)\"; exec \"$@\"", "iso-account-test", binary];
        invocation.extend_from_slice(args);
        let result = self.run("/usr/bin/setpriv", &invocation, input);
        assert_eq!(result.stdout.lines().next(), Some(format!("ISO_ACCOUNT_EXEC:{uid}").as_str()),
            "the requested tool must actually start under the target uid; PAM/spawn failure is not a denial result");
        result
    }
    fn ssh(&self, user: &str, extra: &[&str]) -> Output {
        let mut args: Vec<_> = self.ssh_base.iter().map(String::as_str).collect();
        args.extend(["-l", user]); args.extend_from_slice(extra);
        self.run("/usr/bin/ssh", &args, b"")
    }
    fn forwarding(&mut self, user: &str, proxy: Option<&str>) -> Output {
        let path = self.directory.join(format!("master-{}.sock", self.sockets.len()));
        self.sockets.push(path.clone());
        let mut args = vec!["-M", "-S", path.to_str().unwrap(), "-f", "-N", "-R", "127.0.0.1:0:127.0.0.1:9"];
        if let Some(proxy) = proxy { args.extend(["-o", proxy]); }
        args.push("127.0.0.1");
        let result = self.ssh(user, &args);
        // Always close an unexpectedly successful employee master before the
        // assertion. No remote command is executed by this forwarding probe.
        if path.exists() {
            let close = self.ssh(user, &["-S", path.to_str().unwrap(), "-O", "exit", "127.0.0.1"]);
            assert_eq!(close.code, 0, "SSH test master must be closed");
        }
        result
    }
    fn actual_ssh_denial(&mut self, user: &str, forwarding: bool) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("allocate a fresh local source port");
        let port = listener.local_addr().unwrap().port(); drop(listener);
        let proxy = format!("ProxyCommand=/usr/bin/nc -p {port} 127.0.0.1 22");
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let result = if forwarding { self.forwarding(user, Some(&proxy)) }
            else { self.ssh(user, &["-o", &proxy, "127.0.0.1", "/usr/bin/true"]) };
        ssh_denied(&result);
        // The live listener may have stale configuration or use sshd -f. A
        // client auth refusal plus offline sshd -T is insufficient. Require a
        // root journal record for THIS source port and same sshd PID explicitly
        // rejecting this employee through DenyUsers, not merely a missing key.
        success(&self.run("/usr/bin/journalctl", &["--sync"], b""), "flush actual sshd evidence");
        let since = format!("@{started}");
        for attempt in 0..3 {
            let journal = self.run("/usr/bin/journalctl", &["--boot=0", "--identifier=sshd", "--since", &since,
                "--no-pager", "--output=json", "--output-fields=MESSAGE,_PID,_UID"], b"");
            success(&journal, "read actual sshd journal");
            if ssh_policy_journal(&journal.stdout, user, port) { return; }
            if attempt < 2 { std::thread::sleep(std::time::Duration::from_millis(100)); }
        }
        panic!("live SSH daemon must log an exact DenyUsers rejection for this employee/source-port/session PID");
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        for socket in &self.sockets {
            if socket.exists() {
                let _ = self.run("/usr/bin/ssh", &["-F", "/dev/null", "-S", socket.to_str().unwrap(), "-O", "exit", "127.0.0.1"], b"");
            }
        }
        if let Ok(entries) = fs::read_dir(&self.directory) {
            for entry in entries.flatten() { let _ = fs::remove_file(entry.path()); }
        }
        let _ = fs::remove_dir(&self.directory);
    }
}
fn success(output: &Output, label: &str) { assert_eq!(output.code, 0, "{label}: positive control must succeed"); }
fn ssh_denied(output: &Output) {
    assert_eq!(output.code, 255, "SSH denial must be an authentication refusal");
    assert!(output.stderr.lines().any(|line| line.ends_with(": Permission denied (publickey).")),
        "require actual server authentication refusal, not connection, executable or timeout failure");
}
fn ssh_policy_journal(text: &str, user: &str, port: u16) -> bool {
    let rows: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("journal JSON")).collect();
    let field = |row: &serde_json::Value, name: &str| row.get(name).and_then(serde_json::Value::as_str).map(str::to_owned);
    let port = port.to_string();
    let mut sessions = std::collections::BTreeSet::new();
    for row in &rows {
        if field(row, "_UID").as_deref() != Some("0") { continue; }
        let Some(message) = field(row, "MESSAGE") else { continue; };
        let words: Vec<_> = message.split_whitespace().collect();
        let connection_closed = words.as_slice() == ["Connection", "closed", "by", "invalid", "user", user, "127.0.0.1", "port", &port, "[preauth]"];
        let disconnected = words.as_slice() == ["Disconnected", "from", "invalid", "user", user, "127.0.0.1", "port", &port, "[preauth]"];
        let failed_key = words.get(..11) == Some(["Failed", "publickey", "for", "invalid", "user", user, "from", "127.0.0.1", "port", &port, "ssh2:"].as_slice());
        if connection_closed || disconnected || failed_key { if let Some(pid) = field(row, "_PID") { sessions.insert(pid); } }
    }
    rows.iter().any(|row| {
        if field(row, "_UID").as_deref() != Some("0") || !field(row, "_PID").is_some_and(|pid| sessions.contains(&pid)) { return false; }
        let Some(message) = field(row, "MESSAGE") else { return false; };
        let message = message.strip_suffix(" [preauth]").unwrap_or(&message);
        ["127.0.0.1", "localhost"].into_iter().any(|host|
            message == format!("User {user} from {host} not allowed because listed in DenyUsers"))
    })
}
fn account(name: &str) -> crate::accounts::PasswdEntry {
    assert!(!name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')));
    let users = crate::accounts::parse_passwd(&fs::read_to_string("/etc/passwd").unwrap()).unwrap();
    let mut rows = users.into_iter().filter(|entry| entry.name == name);
    let user = rows.next().expect("fixture account exists locally");
    assert!(rows.next().is_none()); user
}
fn known_file(variable: &str, private: bool) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(variable).expect("explicit SSH fixture path is required"));
    crate::registry::root_path(&path, false).expect("root-owned nonsymlink SSH fixture file");
    if private { assert_eq!(fs::metadata(&path).unwrap().mode() & 0o077, 0, "fixture key is root-private"); }
    path
}
fn check_authorized_key(h: &Harness, user: &str, public_key: &str) {
    let config = h.run("/usr/sbin/sshd", &["-T", "-C", &format!("user={user},host=localhost,addr=127.0.0.1")], b"");
    success(&config, "sshd effective configuration");
    assert!(config.stdout.lines().any(|line| line == format!("authorizedkeysfile {AUTHORIZED}")),
        "the live SSH fixture must use the fixed root-owned authorized-key directory for BOTH accounts");
    let path = Path::new(FIXTURE).join("authorized_keys").join(user);
    crate::registry::root_path(&path, false).expect("root-owned authorized key fixture");
    let actual = fs::read_to_string(path).unwrap();
    assert_eq!(actual.lines().count(), 1, "one exact shared fixture key per account");
    assert_eq!(actual.split_whitespace().take(2).collect::<Vec<_>>(), public_key.split_whitespace().take(2).collect::<Vec<_>>());
}
fn remove_at_job(h: &Harness, result: &Output, user: &str) {
    let jobs: Vec<_> = result.stderr.lines().filter_map(|line| {
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() >= 4 && words[0] == "job" && words[2] == "at" { Some(words[1].to_owned()) } else { None }
    }).collect();
    if result.code == 0 { assert_eq!(jobs.len(), 1, "successful at must return exactly one actual job ID"); }
    for job in jobs {
        assert!(!job.is_empty() && job.bytes().all(|b| b.is_ascii_digit()));
        let queue = h.run("/usr/bin/atq", &[], b""); success(&queue, "root at queue readback");
        assert!(queue.stdout.lines().any(|line| {
            let words: Vec<_> = line.split_whitespace().collect();
            words.first() == Some(&job.as_str()) && words.last() == Some(&user)
        }), "clean only the newly created job owned by the target fixture user");
        success(&h.run("/usr/bin/atrm", &[&job], b""), "remove harmless at probe");
        let queue = h.run("/usr/bin/atq", &[], b""); success(&queue, "at cleanup readback");
        assert!(!queue.stdout.lines().any(|line| line.split_whitespace().next() == Some(job.as_str())));
    }
}

#[test]
#[ignore = "iso_root_: installed disposable VM plus explicit localhost SSH/control-user/cron/user-manager fixture; executes real denial attempts"]
fn iso_root_e2e_installed_account_real_denials() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
    assert_eq!(unsafe { libc::geteuid() }, 0);
    for binary in ["/usr/bin/timeout", "/usr/bin/ssh", "/usr/bin/ssh-keygen", "/usr/sbin/sshd", "/usr/bin/su",
        "/usr/bin/setpriv", "/usr/bin/nc", "/usr/bin/journalctl", "/usr/bin/systemd-run", "/usr/bin/systemctl", "/usr/bin/crontab", "/usr/bin/at", "/usr/bin/atq", "/usr/bin/atrm"] {
        let meta = fs::metadata(binary).expect("all acceptance tools must be installed");
        assert!(meta.is_file() && meta.mode() & 0o111 != 0, "required executable missing");
    }
    let b = bundle().or_else(|_| bundle()).expect("actual signed F/root publication required");
    let registry: crate::registry::Registry = read(ROOT, "registry.json").expect("actual installed UID registry");
    let employee = registry.lookup(b.host.entitlements.first().expect("confirmed employee entitlement required").employee).unwrap();
    let name = crate::registry::account_name(employee.uid);
    let employee_user = account(&name);
    assert_eq!(employee_user.uid, employee.uid); assert_eq!(employee_user.gid, employee.gid);
    assert_eq!(employee_user.shell, "/usr/sbin/nologin"); assert_eq!(employee_user.home, "/nonexistent");
    let control = account(&std::env::var("ISO_ACCOUNT_CONTROL_USER").expect("dedicated unprivileged control account required"));
    assert!(control.uid >= 1000 && control.uid < 60000 && control.name != name);
    let key = known_file("ISO_ACCOUNT_SSH_KEY", true);
    let known_hosts = known_file("ISO_ACCOUNT_SSH_KNOWN_HOSTS", false);
    crate::registry::root_path(Path::new(FIXTURE), true).expect("root-owned localhost fixture directory");
    let nonce = crate::server::random_id().unwrap();
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let directory = Path::new(FIXTURE).join(format!("client-{suffix}"));
    fs::DirBuilder::new().mode(0o700).create(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let mut h = Harness { directory, sequence: AtomicUsize::new(0), sockets: vec![], ssh_base: vec![
        "-F".into(), "/dev/null".into(), "-p".into(), "22".into(), "-i".into(), key.to_str().unwrap().into(),
        "-o".into(), format!("UserKnownHostsFile={}", known_hosts.display()),
        "-o".into(), "StrictHostKeyChecking=yes".into(), "-o".into(), "BatchMode=yes".into(),
        "-o".into(), "IdentitiesOnly=yes".into(), "-o".into(), "PreferredAuthentications=publickey".into(),
        "-o".into(), "PasswordAuthentication=no".into(), "-o".into(), "KbdInteractiveAuthentication=no".into(),
        "-o".into(), "ConnectTimeout=5".into(), "-o".into(), "ExitOnForwardFailure=yes".into()] };
    let public = h.run("/usr/bin/ssh-keygen", &["-y", "-P", "", "-f", key.to_str().unwrap()], b"");
    success(&public, "read unencrypted dedicated test key");
    check_authorized_key(&h, &control.name, &public.stdout); check_authorized_key(&h, &name, &public.stdout);
    success(&h.ssh(&control.name, &["127.0.0.1", "/usr/bin/true"]), "real SSH public-key login");
    h.actual_ssh_denial(&name, false);
    success(&h.forwarding(&control.name, None), "real SSH remote forwarding");
    h.actual_ssh_denial(&name, true);

    success(&h.run("/usr/bin/su", &["--login", &control.name, "--command", "/usr/bin/true"], b""), "real su login");
    let su = h.run("/usr/bin/su", &["--login", &name, "--command", "/usr/bin/true"], b"");
    assert_eq!(su.code, 1, "employee su must refuse");
    assert!(su.stdout.lines().chain(su.stderr.lines()).any(|line| matches!(line,
        "This account is currently not available." | "Your account has expired; please contact your system administrator." | "su: Authentication failure")),
        "require account/shell denial, not generic su failure");

    let unit = format!("iso-account-{suffix}");
    success(&h.as_user(&control.name, control.uid, "/usr/bin/systemd-run",
        &["--user", "--wait", "--collect", "--pipe", "--unit", &unit, "/usr/bin/true"], b""), "real user-manager transient unit");
    assert!(!Path::new(&format!("/var/lib/systemd/linger/{name}")).exists(), "employee linger must be absent");
    assert!(!Path::new(&format!("/run/user/{}/bus", employee.uid)).exists(), "employee user bus must be absent");
    let state = h.run("/usr/bin/systemctl", &["show", "--property=ActiveState", "--value", &format!("user@{}.service", employee.uid)], b"");
    success(&state, "employee user-manager readback"); assert!(matches!(state.stdout.trim(), "inactive" | "failed"));
    let user_unit = h.as_user(&name, employee.uid, "/usr/bin/systemd-run",
        &["--user", "--wait", "--collect", "--pipe", "--unit", &unit, "/usr/bin/true"], b"");
    assert_eq!(user_unit.code, 1, "employee user-manager command must refuse");
    assert!(user_unit.stderr.lines().any(|line| matches!(line,
        "Failed to connect to bus: No such file or directory" |
        "Failed to connect to user scope bus via local transport: No such file or directory")),
        "expected absent verified user manager, not arbitrary connection failure");

    let before = h.run("/usr/bin/crontab", &["-u", &name, "-l"], b"");
    assert_eq!(before.code, 1); assert_eq!(before.stderr.trim(), format!("no crontab for {name}"), "never overwrite an existing employee crontab");
    let control_cron = h.run("/usr/bin/crontab", &["-u", &control.name, "-l"], b"");
    success(&control_cron, "dedicated empty control crontab"); assert_eq!(control_cron.stdout, EMPTY_CRON, "control account must have only the documented harmless fixture table");
    success(&h.as_user(&control.name, control.uid, "/usr/bin/crontab", &["-"], EMPTY_CRON.as_bytes()), "real crontab installation");
    let cron = h.as_user(&name, employee.uid, "/usr/bin/crontab", &["-"], EMPTY_CRON.as_bytes());
    if cron.code == 0 { success(&h.run("/usr/bin/crontab", &["-u", &name, "-r"], b""), "remove unexpectedly accepted harmless table"); }
    let after = h.run("/usr/bin/crontab", &["-u", &name, "-l"], b"");
    assert_eq!(after.code, 1); assert_eq!(after.stderr.trim(), format!("no crontab for {name}"));
    assert_eq!(cron.code, 1); assert!(cron.stderr.lines().any(|line| line == format!("You ({name}) are not allowed to use this program (crontab)")), "require actual crontab policy refusal");

    for service in ["cron.service", "atd.service"] {
        let state = h.run("/usr/bin/systemctl", &["show", "--property=ActiveState", "--value", service], b"");
        success(&state, "scheduler service readback"); assert_eq!(state.stdout.trim(), "active", "actual scheduler service must be running");
    }
    let at_control = h.as_user(&control.name, control.uid, "/usr/bin/at", &["-M", "now", "+", "5", "minutes"], b"/usr/bin/true\n");
    remove_at_job(&h, &at_control, &control.name); success(&at_control, "real at scheduling");
    let at = h.as_user(&name, employee.uid, "/usr/bin/at", &["-M", "now", "+", "5", "minutes"], b"/usr/bin/true\n");
    remove_at_job(&h, &at, &name);
    assert_eq!(at.code, 1); assert!(at.stderr.lines().any(|line| line == "You do not have permission to use at."), "require actual at policy refusal");
}

#[test]
fn ssh_policy_evidence_requires_same_root_session_and_source_port() {
    let row = |pid: &str, uid: &str, message: &str| serde_json::json!({"_PID":pid,"_UID":uid,"MESSAGE":message}).to_string();
    let close = row("44", "0", "Connection closed by invalid user duduclaw-emp-70000 127.0.0.1 port 45678 [preauth]");
    let deny = row("44", "0", "User duduclaw-emp-70000 from 127.0.0.1 not allowed because listed in DenyUsers");
    assert!(ssh_policy_journal(&format!("{close}\n{deny}"), "duduclaw-emp-70000", 45678));
    assert!(!ssh_policy_journal(&close, "duduclaw-emp-70000", 45678));
    assert!(!ssh_policy_journal(&format!("{close}\n{deny}"), "duduclaw-emp-70000", 45679));
    let other = row("45", "0", "User duduclaw-emp-70000 from 127.0.0.1 not allowed because listed in DenyUsers");
    assert!(!ssh_policy_journal(&format!("{close}\n{other}"), "duduclaw-emp-70000", 45678));
    let forged = row("44", "70000", "User duduclaw-emp-70000 from 127.0.0.1 not allowed because listed in DenyUsers");
    assert!(!ssh_policy_journal(&format!("{close}\n{forged}"), "duduclaw-emp-70000", 45678));
}
