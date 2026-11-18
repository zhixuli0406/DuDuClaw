//! R7c SSH denial probe, shipped as `/usr/lib/duduclaw/duduclaw-isolation-probe`
//! and started only by PID 1 through the template unit
//! `duduclaw-isolation-probe@<account>.service` (ExecStart passes `%i`).
//!
//! It proves, against the LIVE sshd, that one employee account is refused a
//! real public-key login and a real `-N -R` port-forward session: the ssh
//! client must fail with `Permission denied (publickey).` AND the journal must
//! hold sshd's own root record, from the session that served this probe's
//! source port, saying the account is listed in `DenyUsers`. A successful
//! `sshd -T` alone is never accepted.
//!
//! Invocation: exactly `duduclaw-isolation-probe <account>`. The only other
//! accepted argv is `duduclaw-isolation-probe --relay <source-port> <dest-port>`,
//! used solely as ssh's ProxyCommand: it connects 127.0.0.1:<source-port> →
//! 127.0.0.1:<dest-port> (SO_REUSEADDR off) and relays stdin/stdout until
//! either side closes. Children get an empty environment (plus `LC_ALL=C`),
//! `/` as working directory, no stdin, and fixed absolute tool paths.
//!
//! Output: on success exit 0 and one JSON line on stdout,
//! `{"version":1,"account":"…","port":N,"result":"denied"}`. On failure one
//! JSON line `{"code":"ssh_probe_failed","stage":"<name>"}` on stderr and the
//! stage's exit code (table below; mirrored in the unit file and README). No
//! key material, sshd output or ssh output is ever printed.
#![allow(unsafe_code)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// Exit codes (one per stage): the shared table in
/// `duduclaw_isolation::probe_exit` (spawnd maps the same constants). 0 =
/// denied (pass). Never 1 (generic), 101 (Rust panic), 125 (unsupported
/// platform), 200–243 (systemd's own exec failures) or 255 (ssh's own failure code).
#[allow(unused_imports)]
pub use duduclaw_isolation::probe_exit::{EXIT_USAGE, EXIT_IDENTITY, EXIT_TOOLS, EXIT_NO_SSHD, EXIT_WORKSPACE,
    EXIT_EFFECTIVE_CONFIG, EXIT_KEYGEN, EXIT_LOGIN_INCONCLUSIVE, EXIT_LOGIN_NOT_DENIED, EXIT_LOGIN_JOURNAL,
    EXIT_FORWARD_INCONCLUSIVE, EXIT_FORWARD_NOT_DENIED, EXIT_FORWARD_JOURNAL, EXIT_DEADLINE, EXIT_OUTPUT, EXIT_RELAY};

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("{{\"code\":\"not_ready\",\"reason\":\"unsupported_platform\"}}");
    std::process::exit(125);
}

#[cfg(target_os = "linux")]
fn main() { linux::main() }

/// Fixed stage vocabulary and pure decision functions (host-testable).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod logic {
    use super::*;
    use duduclaw_isolation::ids::{employee_account_name_valid, EMPLOYEE_UID_MAX, EMPLOYEE_UID_MIN};
    use std::ffi::OsString;

    pub(crate) const VERSION: u32 = 1;
    pub(crate) const SSHD: &str = "/usr/sbin/sshd";
    pub(crate) const SSH: &str = "/usr/bin/ssh";
    pub(crate) const SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";
    pub(crate) const JOURNALCTL: &str = "/usr/bin/journalctl";
    pub(crate) const TOOLS: [&str; 4] = [SSHD, SSH, SSH_KEYGEN, JOURNALCTL];
    /// `sshd` before OpenSSH 9.8; `sshd-session` logs the session from 9.8 on.
    pub(crate) const SSHD_IDENTIFIERS: [&str; 2] = ["sshd", "sshd-session"];

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Stage {
        Usage, Identity, Tools, NoSshd, Workspace, EffectiveConfig, Keygen, LoginInconclusive,
        LoginNotDenied, LoginJournal, ForwardInconclusive, ForwardNotDenied, ForwardJournal,
        Deadline, Output, Relay,
    }
    impl Stage {
        #[cfg(test)]
        pub(crate) const ALL: [Stage; 16] = [Stage::Usage, Stage::Identity, Stage::Tools, Stage::NoSshd,
            Stage::Workspace, Stage::EffectiveConfig, Stage::Keygen, Stage::LoginInconclusive,
            Stage::LoginNotDenied, Stage::LoginJournal, Stage::ForwardInconclusive, Stage::ForwardNotDenied,
            Stage::ForwardJournal, Stage::Deadline, Stage::Output, Stage::Relay];
        pub(crate) fn code(self) -> i32 {
            match self {
                Stage::Usage => EXIT_USAGE, Stage::Identity => EXIT_IDENTITY, Stage::Tools => EXIT_TOOLS,
                Stage::NoSshd => EXIT_NO_SSHD, Stage::Workspace => EXIT_WORKSPACE,
                Stage::EffectiveConfig => EXIT_EFFECTIVE_CONFIG, Stage::Keygen => EXIT_KEYGEN,
                Stage::LoginInconclusive => EXIT_LOGIN_INCONCLUSIVE, Stage::LoginNotDenied => EXIT_LOGIN_NOT_DENIED,
                Stage::LoginJournal => EXIT_LOGIN_JOURNAL, Stage::ForwardInconclusive => EXIT_FORWARD_INCONCLUSIVE,
                Stage::ForwardNotDenied => EXIT_FORWARD_NOT_DENIED, Stage::ForwardJournal => EXIT_FORWARD_JOURNAL,
                Stage::Deadline => EXIT_DEADLINE, Stage::Output => EXIT_OUTPUT, Stage::Relay => EXIT_RELAY,
            }
        }
        pub(crate) fn name(self) -> &'static str {
            match self {
                Stage::Usage => "usage", Stage::Identity => "identity", Stage::Tools => "tools",
                Stage::NoSshd => "no_sshd", Stage::Workspace => "workspace",
                Stage::EffectiveConfig => "effective_config", Stage::Keygen => "keygen",
                Stage::LoginInconclusive => "login_inconclusive", Stage::LoginNotDenied => "login_not_denied",
                Stage::LoginJournal => "login_journal", Stage::ForwardInconclusive => "forward_inconclusive",
                Stage::ForwardNotDenied => "forward_not_denied", Stage::ForwardJournal => "forward_journal",
                Stage::Deadline => "deadline", Stage::Output => "output", Stage::Relay => "relay",
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Mode { Probe(String), Relay { source: u16, dest: u16 } }

    /// `args` excludes argv[0]. Exactly one account argument, or exactly
    /// `--relay <source> <dest>` with plain decimal ports; anything else is a
    /// usage error. The account name rule itself is the identity stage's.
    pub(crate) fn parse_args(args: &[OsString]) -> Option<Mode> {
        let args: Vec<&str> = args.iter().map(|arg| arg.to_str()).collect::<Option<_>>()?;
        match args.as_slice() {
            ["--relay", source, dest] => Some(Mode::Relay { source: port_number(source)?, dest: port_number(dest)? }),
            [account] if !account.is_empty() && !account.starts_with('-') => Some(Mode::Probe((*account).to_owned())),
            _ => None,
        }
    }
    /// 1..=65535 written as plain decimal digits without a leading zero.
    pub(crate) fn port_number(text: &str) -> Option<u16> {
        if text.is_empty() || text.len() > 5 || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse::<u16>().ok()
    }

    /// uid of the single /etc/passwd row named `name`; `None` when absent,
    /// duplicated or malformed.
    pub(crate) fn passwd_uid(text: &str, name: &str) -> Option<u32> {
        let mut found = None;
        for line in text.lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.first() != Some(&name) { continue; }
            if fields.len() != 7 || found.is_some() { return None; }
            found = Some(fields[2].parse::<u32>().ok()?);
        }
        found
    }
    /// Stage `identity`: root, the employee name rule, and a local account in
    /// the employee uid range.
    pub(crate) fn identity_allowed(euid: u32, name: &str, passwd: &str) -> bool {
        euid == 0 && employee_account_name_valid(name)
            && passwd_uid(passwd, name).is_some_and(|uid| (EMPLOYEE_UID_MIN..=EMPLOYEE_UID_MAX).contains(&uid))
    }
    /// Stage `tools` (fstat of the opened file): a regular, executable file
    /// owned by root and writable by neither group nor other.
    pub(crate) fn tool_metadata_safe(is_file: bool, uid: u32, mode: u32) -> bool {
        is_file && uid == 0 && mode & 0o022 == 0 && mode & 0o111 != 0
    }
    /// This binary's own path is embedded in ssh's ProxyCommand (run by
    /// /bin/sh, with `%` token expansion): absolute, and only `[A-Za-z0-9/._-]`.
    pub(crate) fn proxy_path_safe(path: &str) -> bool {
        path.starts_with('/') && !path.contains("..")
            && path.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
    }

    /// Stage `effective_config`: the same three settings accounts.rs
    /// `ssh_effective_denied` requires (last occurrence wins, as there), and
    /// the FIRST `port` line as the port to probe.
    pub(crate) fn effective_port(text: &str) -> Option<u16> {
        let (mut deny, mut forwarding, mut tty) = (false, false, false);
        let mut port: Option<Option<u16>> = None;
        for line in text.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.first().copied() {
                Some("denyusers") => deny = words.iter().skip(1).any(|word| *word == "duduclaw-emp-*"),
                Some("disableforwarding") => forwarding = words.as_slice() == ["disableforwarding", "yes"],
                Some("permittty") => tty = words.as_slice() == ["permittty", "no"],
                Some("port") if port.is_none() => port = Some(match words.as_slice() {
                    [_, value] => port_number(value), _ => None }),
                _ => {}
            }
        }
        if deny && forwarding && tty { port.flatten() } else { None }
    }

    /// The ssh client argv: the root-test harness option set
    /// (installed_account_root_tests.rs `ssh_base`) with a fresh, never
    /// authorised key, no host-key state, and this binary as ProxyCommand.
    pub(crate) fn ssh_arguments(key: &str, proxy: &str, account: &str, forwarding: bool) -> Vec<String> {
        let mut args: Vec<String> = vec!["-F".into(), "/dev/null".into()];
        let mut options = vec!["BatchMode=yes".to_owned(), "IdentitiesOnly=yes".into(), format!("IdentityFile={key}"),
            "PreferredAuthentications=publickey".into(), "PubkeyAuthentication=yes".into(),
            "PasswordAuthentication=no".into(), "KbdInteractiveAuthentication=no".into(),
            "StrictHostKeyChecking=no".into(), "UserKnownHostsFile=/dev/null".into(),
            "GlobalKnownHostsFile=/dev/null".into(), "ConnectTimeout=5".into(), format!("ProxyCommand={proxy}")];
        if forwarding {
            options.extend(["ControlMaster=no".to_owned(), "ControlPath=none".into(), "ExitOnForwardFailure=yes".into()]);
        }
        for option in options { args.push("-o".into()); args.push(option); }
        if forwarding { args.extend(["-N".to_owned(), "-R".into(), "127.0.0.1:0:127.0.0.1:9".into()]); }
        args.extend(["-l".to_owned(), account.to_owned(), "127.0.0.1".into()]);
        if !forwarding { args.push("/usr/bin/true".into()); }
        args
    }
    pub(crate) fn proxy_command(own: &str, source: u16, dest: u16) -> String { format!("{own} --relay {source} {dest}") }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum ClientExit { Code(i32), Signal, TimedOut }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Verdict { Denied, NotDenied, Inconclusive }
    /// The harness's `ssh_denied`: exit 255 AND a line ending
    /// `: Permission denied (publickey).`. Exit 0 is the one result meaning the
    /// account is reachable. A `-N` forwarding client still connected at its
    /// kill timer had its session accepted. Anything else is inconclusive.
    pub(crate) fn client_verdict(exit: ClientExit, stderr: &str, forwarding: bool) -> Verdict {
        match exit {
            ClientExit::Code(0) => Verdict::NotDenied,
            ClientExit::Code(255) if stderr.lines().any(|line| line.ends_with(": Permission denied (publickey).")) =>
                Verdict::Denied,
            ClientExit::TimedOut if forwarding => Verdict::NotDenied,
            _ => Verdict::Inconclusive,
        }
    }

    /// The harness's `ssh_policy_journal`, restricted to sshd/sshd-session
    /// records: a root (_UID 0) record from the same sshd PID that closed or
    /// failed the session of THIS source port, saying the account from
    /// 127.0.0.1/localhost is not allowed because listed in DenyUsers. Any
    /// non-JSON line refuses.
    pub(crate) fn journal_denies(text: &str, user: &str, port: u16) -> bool {
        let mut rows = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<serde_json::Value>(line) { Ok(row) => rows.push(row), Err(_) => return false }
        }
        let field = |row: &serde_json::Value, name: &str| row.get(name).and_then(serde_json::Value::as_str).map(str::to_owned);
        let sshd_root = |row: &serde_json::Value| field(row, "_UID").as_deref() == Some("0")
            && field(row, "SYSLOG_IDENTIFIER").is_some_and(|id| SSHD_IDENTIFIERS.contains(&id.as_str()));
        let port = port.to_string();
        let mut sessions = std::collections::BTreeSet::new();
        for row in rows.iter().filter(|row| sshd_root(*row)) {
            let Some(message) = field(row, "MESSAGE") else { continue };
            let words: Vec<&str> = message.split_whitespace().collect();
            let connection_closed = words.as_slice() == ["Connection", "closed", "by", "invalid", "user", user, "127.0.0.1", "port", &port, "[preauth]"];
            let disconnected = words.as_slice() == ["Disconnected", "from", "invalid", "user", user, "127.0.0.1", "port", &port, "[preauth]"];
            let failed_key = words.get(..11) == Some(["Failed", "publickey", "for", "invalid", "user", user, "from", "127.0.0.1", "port", &port, "ssh2:"].as_slice());
            if connection_closed || disconnected || failed_key { if let Some(pid) = field(row, "_PID") { sessions.insert(pid); } }
        }
        rows.iter().any(|row| {
            if !sshd_root(row) || !field(row, "_PID").is_some_and(|pid| sessions.contains(&pid)) { return false; }
            let Some(message) = field(row, "MESSAGE") else { return false };
            let message = message.strip_suffix(" [preauth]").unwrap_or(&message);
            ["127.0.0.1", "localhost"].into_iter().any(|host|
                message == format!("User {user} from {host} not allowed because listed in DenyUsers"))
        })
    }

    /// The single success line. `account` already passed the name rule, so it
    /// needs no JSON escaping.
    pub(crate) fn success_line(account: &str, port: u16) -> String {
        format!("{{\"version\":{VERSION},\"account\":\"{account}\",\"port\":{port},\"result\":\"denied\"}}\n")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::logic::*;
    use std::ffi::OsString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// The unit's TimeoutStartSec is 30 s; the probe gives up first.
    const DEADLINE: Duration = Duration::from_secs(25);
    const TOOL_LIMIT: Duration = Duration::from_secs(5);
    const LOGIN_LIMIT: Duration = Duration::from_secs(10);
    const FORWARD_LIMIT: Duration = Duration::from_secs(10);
    const MAX_TEXT: u64 = 4 * 1024 * 1024;
    const JOURNAL_ATTEMPTS: usize = 3;

    fn fail(stage: Stage) -> ! {
        let _ = writeln!(io::stderr(), "{{\"code\":\"ssh_probe_failed\",\"stage\":\"{}\"}}", stage.name());
        std::process::exit(stage.code())
    }
    fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "refused") }

    pub fn main() {
        // A panic never prints a message; it exits 101.
        std::panic::set_hook(Box::new(|_| {}));
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        match parse_args(&args) {
            None => fail(Stage::Usage),
            // ProxyCommand mode: stdout is the SSH stream, so nothing but relayed
            // bytes is ever written there.
            Some(Mode::Relay { source, dest }) => match relay(source, dest, 0, 1) {
                Ok(()) => std::process::exit(0),
                Err(_) => fail(Stage::Relay),
            },
            Some(Mode::Probe(account)) => match probe(&account, Instant::now() + DEADLINE) {
                Ok(port) => {
                    let mut out = io::stdout().lock();
                    if out.write_all(success_line(&account, port).as_bytes()).and_then(|()| out.flush()).is_err() {
                        fail(Stage::Output);
                    }
                    std::process::exit(0)
                }
                Err(stage) => fail(stage),
            },
        }
    }

    /// Every step; the workspace (and the ephemeral key in it) is removed when
    /// this returns, before the caller exits.
    fn probe(account: &str, deadline: Instant) -> Result<u16, Stage> {
        // 1. identity
        // SAFETY: argument-free query.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 { return Err(Stage::Identity); }
        let passwd = read_text(Path::new("/etc/passwd")).map_err(|_| Stage::Identity)?;
        if !identity_allowed(euid, account, &passwd) { return Err(Stage::Identity); }
        // 2. tools (a missing sshd means SSH checks do not apply to this host)
        match fs::metadata(SSHD) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Stage::NoSshd),
            _ => {}
        }
        for tool in TOOLS { trusted_executable(Path::new(tool)).map_err(|_| Stage::Tools)?; }
        let own = own_executable().map_err(|_| Stage::Tools)?;
        let work = Workspace::create().map_err(|_| Stage::Workspace)?;
        // 3. effective configuration for this account
        let context = format!("user={account},host=localhost,addr=127.0.0.1");
        let config = run_tool(&work, SSHD, &["-T", "-C", &context], limit(deadline, TOOL_LIMIT)?)
            .map_err(|_| Stage::EffectiveConfig)?;
        if config.exit != ClientExit::Code(0) { return Err(Stage::EffectiveConfig); }
        let port = effective_port(&config.stdout).ok_or(Stage::EffectiveConfig)?;
        // 4. ephemeral key, never authorised anywhere
        let key = work.dir.join("key");
        let key_text = key.to_str().ok_or(Stage::Keygen)?.to_owned();
        let keygen = run_tool(&work, SSH_KEYGEN, &["-q", "-t", "ed25519", "-N", "", "-C", "duduclaw-isolation-probe",
            "-f", &key_text], limit(deadline, TOOL_LIMIT)?).map_err(|_| Stage::Keygen)?;
        if keygen.exit != ClientExit::Code(0) { return Err(Stage::Keygen); }
        let meta = fs::symlink_metadata(&key).map_err(|_| Stage::Keygen)?;
        if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o077 != 0 { return Err(Stage::Keygen); }
        // 5.–6. login attempt + journal corroboration
        attempt(&work, &own, &key_text, account, port, false, deadline)?;
        // 7. forwarding attempt + journal corroboration
        attempt(&work, &own, &key_text, account, port, true, deadline)?;
        Ok(port)
    }

    fn limit(deadline: Instant, own: Duration) -> Result<Duration, Stage> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { Err(Stage::Deadline) } else { Ok(remaining.min(own)) }
    }

    fn attempt(work: &Workspace, own: &str, key: &str, account: &str, port: u16, forwarding: bool,
        deadline: Instant) -> Result<(), Stage> {
        let (inconclusive, not_denied, journal_stage, own_limit) = if forwarding {
            (Stage::ForwardInconclusive, Stage::ForwardNotDenied, Stage::ForwardJournal, FORWARD_LIMIT)
        } else {
            (Stage::LoginInconclusive, Stage::LoginNotDenied, Stage::LoginJournal, LOGIN_LIMIT)
        };
        let source = free_port().map_err(|_| inconclusive)?;
        let started = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| inconclusive)?.as_secs();
        let args = ssh_arguments(key, &proxy_command(own, source, port), account, forwarding);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let bound = limit(deadline, own_limit)?;
        let result = run_tool(work, SSH, &args, bound).map_err(|_| inconclusive)?;
        // A timer shortened by the overall deadline proves nothing either way.
        if result.exit == ClientExit::TimedOut && bound < own_limit { return Err(Stage::Deadline); }
        match client_verdict(result.exit, &result.stderr, forwarding) {
            Verdict::Denied => {}
            Verdict::NotDenied => return Err(not_denied),
            Verdict::Inconclusive => return Err(inconclusive),
        }
        if journal_confirms(work, account, source, started, deadline)? { Ok(()) } else { Err(journal_stage) }
    }

    /// `Ok(false)`: no corroborating record (or journalctl failed) after the
    /// bounded retries; `Err(Deadline)`: the overall deadline expired.
    fn journal_confirms(work: &Workspace, account: &str, source: u16, started: u64, deadline: Instant) -> Result<bool, Stage> {
        match run_tool(work, JOURNALCTL, &["--sync"], limit(deadline, TOOL_LIMIT)?) {
            Ok(result) if result.exit == ClientExit::Code(0) => {}
            _ => return Ok(false),
        }
        let since = format!("@{started}");
        for attempt in 0..JOURNAL_ATTEMPTS {
            let journal = run_tool(work, JOURNALCTL, &["--boot=0", "--identifier=sshd", "--identifier=sshd-session",
                "--since", &since, "--no-pager", "--output=json", "--output-fields=MESSAGE,_PID,_UID,SYSLOG_IDENTIFIER"],
                limit(deadline, TOOL_LIMIT)?);
            if let Ok(journal) = journal {
                if journal.exit == ClientExit::Code(0) && journal_denies(&journal.stdout, account, source) { return Ok(true); }
            }
            if attempt + 1 < JOURNAL_ATTEMPTS { std::thread::sleep(Duration::from_millis(100)); }
        }
        Ok(false)
    }

    /// A loopback port free at this instant (bind 127.0.0.1:0, read, close).
    fn free_port() -> io::Result<u16> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        Ok(listener.local_addr()?.port())
    }

    fn read_text(path: &Path) -> io::Result<String> {
        let mut bytes = Vec::new();
        File::open(path)?.take(MAX_TEXT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_TEXT { return Err(denied()); }
        String::from_utf8(bytes).map_err(|_| denied())
    }

    /// fstat of the opened file, never a path-based check.
    fn trusted_executable(path: &Path) -> io::Result<()> {
        let meta = File::open(path)?.metadata()?;
        if tool_metadata_safe(meta.is_file(), meta.uid(), meta.mode()) { Ok(()) } else { Err(denied()) }
    }
    fn own_executable() -> io::Result<String> {
        let path = std::env::current_exe()?;
        let text = path.to_str().ok_or_else(denied)?.to_owned();
        if !proxy_path_safe(&text) { return Err(denied()); }
        trusted_executable(&path)?;
        Ok(text)
    }

    /// 0700 root-owned directory on the unit's private /tmp; removed on drop.
    struct Workspace { dir: PathBuf, sequence: std::cell::Cell<u32> }
    impl Workspace {
        fn create() -> io::Result<Self> {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.subsec_nanos()).unwrap_or(0);
            let dir = PathBuf::from(format!("/tmp/duduclaw-isolation-probe-{}-{nanos}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
            let work = Workspace { dir, sequence: std::cell::Cell::new(0) };
            let meta = fs::symlink_metadata(&work.dir)?;
            if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o777 != 0o700 { return Err(denied()); }
            Ok(work)
        }
        fn output_file(&self, suffix: &str) -> io::Result<(PathBuf, File)> {
            let id = self.sequence.get();
            self.sequence.set(id + 1);
            let path = self.dir.join(format!("{id}.{suffix}"));
            let file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            Ok((path, file))
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            // Best effort; PrivateTmp removes the whole /tmp at unit end anyway.
            if let Ok(entries) = fs::read_dir(&self.dir) {
                for entry in entries.flatten() { let _ = fs::remove_file(entry.path()); }
            }
            let _ = fs::remove_dir(&self.dir);
        }
    }

    struct ToolResult { exit: ClientExit, stdout: String, stderr: String }

    /// The only process creation of the probe: fixed absolute tool path, fixed
    /// argv, empty environment (plus LC_ALL=C), cwd `/`, stdin /dev/null,
    /// stdout/stderr to 0600 files in the workspace, its own process group.
    /// Waited with a timer; on expiry the whole group (ssh and its ProxyCommand)
    /// is killed before the child is reaped.
    fn run_tool(work: &Workspace, program: &str, args: &[&str], timeout: Duration) -> io::Result<ToolResult> {
        let (out_path, out) = work.output_file("out")?;
        let (err_path, err) = work.output_file("err")?;
        let mut child = Command::new(program).args(args).env_clear().env("LC_ALL", "C").current_dir("/")
            .stdin(Stdio::null()).stdout(Stdio::from(out)).stderr(Stdio::from(err)).process_group(0).spawn()?;
        let deadline = Instant::now() + timeout;
        let exit = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().map_or(ClientExit::Signal, ClientExit::Code),
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
            if Instant::now() >= deadline {
                let group = i32::try_from(child.id()).map_err(|_| denied())?;
                // SAFETY: the child is not reaped yet, so its pid (= its process
                // group id) cannot have been reused.
                unsafe { libc::kill(-group, libc::SIGKILL); }
                let _ = child.kill();
                let _ = child.wait();
                break ClientExit::TimedOut;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let stdout = read_text(&out_path);
        let stderr = read_text(&err_path);
        let _ = fs::remove_file(&out_path);
        let _ = fs::remove_file(&err_path);
        Ok(ToolResult { exit, stdout: stdout?, stderr: stderr? })
    }

    /// `--relay`: connect from 127.0.0.1:`source` to 127.0.0.1:`dest`, then
    /// copy `input` → socket and socket → `output` until either side closes.
    pub(super) fn relay(source: u16, dest: u16, input: RawFd, output: RawFd) -> io::Result<()> {
        let socket = connect_from(source, dest)?;
        pump(input, output, socket.as_raw_fd())
    }

    fn loopback(port: u16) -> libc::sockaddr_in {
        libc::sockaddr_in { sin_family: libc::AF_INET as libc::sa_family_t, sin_port: port.to_be(),
            sin_addr: libc::in_addr { s_addr: u32::from(Ipv4Addr::LOCALHOST).to_be() }, sin_zero: [0; 8] }
    }
    fn connect_from(source: u16, dest: u16) -> io::Result<TcpStream> {
        // SAFETY: plain socket creation; the descriptor is owned immediately.
        let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if raw < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: just returned to this process only.
        let socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let off: libc::c_int = 0;
        // SAFETY: valid option pointer and length for SO_REUSEADDR.
        if unsafe { libc::setsockopt(raw, libc::SOL_SOCKET, libc::SO_REUSEADDR, (&off as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t) } != 0 { return Err(io::Error::last_os_error()); }
        let (local, remote) = (loopback(source), loopback(dest));
        let length = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        // SAFETY: fully initialized sockaddr_in with its exact length.
        if unsafe { libc::bind(raw, (&local as *const libc::sockaddr_in).cast(), length) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        if unsafe { libc::connect(raw, (&remote as *const libc::sockaddr_in).cast(), length) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = TcpStream::from(socket);
        if stream.local_addr()? != SocketAddr::from((Ipv4Addr::LOCALHOST, source))
            || stream.peer_addr()? != SocketAddr::from((Ipv4Addr::LOCALHOST, dest)) { return Err(denied()); }
        Ok(stream)
    }
    fn read_fd(fd: RawFd, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            // SAFETY: writes at most buffer.len() bytes into the buffer.
            let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if count >= 0 { return Ok(count as usize); }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
        }
    }
    fn write_all_fd(fd: RawFd, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            // SAFETY: reads at most data.len() bytes from the slice.
            let count = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted { continue; }
                return Err(error);
            }
            if count == 0 { return Err(io::ErrorKind::WriteZero.into()); }
            data = &data[count as usize..];
        }
        Ok(())
    }
    fn pump(input: RawFd, output: RawFd, socket: RawFd) -> io::Result<()> {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let mut fds = [libc::pollfd { fd: input, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: socket, events: libc::POLLIN, revents: 0 }];
            // SAFETY: two initialized pollfd entries.
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted { continue; }
                return Err(error);
            }
            for (index, (from, to)) in [(input, socket), (socket, output)].into_iter().enumerate() {
                let events = fds[index].revents;
                if events & libc::POLLNVAL != 0 { return Err(denied()); }
                if events & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 { continue; }
                let count = read_fd(from, &mut buffer)?;
                if count == 0 { return Ok(()); }
                write_all_fd(to, &buffer[..count])?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn probe_exit_codes_are_unique_nonzero_and_outside_reserved_ranges() {
        let mut codes: Vec<i32> = Stage::ALL.iter().map(|stage| stage.code()).collect();
        let mut names: Vec<&str> = Stage::ALL.iter().map(|stage| stage.name()).collect();
        codes.sort(); codes.dedup(); names.sort(); names.dedup();
        assert_eq!(codes.len(), Stage::ALL.len());
        assert_eq!(names.len(), Stage::ALL.len());
        for code in &codes {
            assert!(*code > 1 && *code < 126, "{code}");
            assert!(!(200..=243).contains(code) && *code != 255 && *code != 101 && *code != 125, "{code}");
        }
        assert!(names.iter().all(|name| name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
        // spawnd logs the shared table's names: they are the probe's own.
        for stage in Stage::ALL {
            assert_eq!(duduclaw_isolation::probe_exit::stage_name(stage.code()), Some(stage.name()), "{stage:?}");
        }
        // The codes spawnd distinguishes (crates/duduclaw-spawnd/src/ssh_evidence.rs).
        assert_eq!(Stage::NoSshd.code(), EXIT_NO_SSHD);
        assert_eq!(Stage::LoginNotDenied.code(), EXIT_LOGIN_NOT_DENIED);
        assert_eq!(Stage::ForwardNotDenied.code(), EXIT_FORWARD_NOT_DENIED);
        assert_eq!(Stage::Identity.code(), EXIT_IDENTITY);
    }

    #[test]
    fn probe_effective_config_requires_all_three_settings_and_takes_the_first_port() {
        let denied = "port 2222\nport 22\ndenyusers duduclaw-emp-* duduclaw\ndisableforwarding yes\npermittty no\n";
        assert_eq!(effective_port(denied), Some(2222));
        assert_eq!(effective_port(&denied.replace("port 2222\n", "")), Some(22));
        assert_eq!(effective_port("denyusers duduclaw-emp-*\ndisableforwarding yes\npermittty no\n"), None);
        for missing in ["denyusers duduclaw-emp-* duduclaw\n", "disableforwarding yes\n", "permittty no\n"] {
            assert_eq!(effective_port(&denied.replace(missing, "")), None, "{missing}");
        }
        assert_eq!(effective_port(&denied.replace("denyusers duduclaw-emp-*", "denyusers fake-duduclaw-emp-*")), None);
        assert_eq!(effective_port(&denied.replace("disableforwarding yes", "disableforwarding no")), None);
        assert_eq!(effective_port(&denied.replace("permittty no", "permittty yes")), None);
        // The FIRST port line decides, even when malformed.
        assert_eq!(effective_port(&denied.replace("port 2222", "port 0")), None);
        assert_eq!(effective_port(&denied.replace("port 2222", "port 70000")), None);
        assert_eq!(effective_port(&denied.replace("port 2222", "port 22 23")), None);
        // Last occurrence of a setting wins, as in accounts.rs ssh_effective_denied.
        assert_eq!(effective_port(&format!("{denied}permittty yes\n")), None);
        assert_eq!(effective_port(""), None);
    }

    #[test]
    fn probe_client_verdict_matrix() {
        let refused = "Warning: Permanently added '127.0.0.1' (ED25519) to the list of known hosts.\r\n\
            duduclaw-emp-70000@127.0.0.1: Permission denied (publickey).\r\n";
        for forwarding in [false, true] {
            assert_eq!(client_verdict(ClientExit::Code(255), refused, forwarding), Verdict::Denied);
            assert_eq!(client_verdict(ClientExit::Code(0), "", forwarding), Verdict::NotDenied);
            assert_eq!(client_verdict(ClientExit::Code(0), refused, forwarding), Verdict::NotDenied);
            assert_eq!(client_verdict(ClientExit::Code(255), "ssh: connect to host 127.0.0.1 port 22: Connection refused\n",
                forwarding), Verdict::Inconclusive);
            assert_eq!(client_verdict(ClientExit::Code(255), "Permission denied (publickey,password).\n", forwarding),
                Verdict::Inconclusive);
            assert_eq!(client_verdict(ClientExit::Code(255), "", forwarding), Verdict::Inconclusive);
            assert_eq!(client_verdict(ClientExit::Code(124), refused, forwarding), Verdict::Inconclusive);
            assert_eq!(client_verdict(ClientExit::Code(1), refused, forwarding), Verdict::Inconclusive);
            assert_eq!(client_verdict(ClientExit::Signal, refused, forwarding), Verdict::Inconclusive);
        }
        // A login attempt hanging is inconclusive; a -N session still open was accepted.
        assert_eq!(client_verdict(ClientExit::TimedOut, refused, false), Verdict::Inconclusive);
        assert_eq!(client_verdict(ClientExit::TimedOut, "", true), Verdict::NotDenied);
    }

    #[test]
    fn probe_journal_requires_root_sshd_record_of_the_same_session_and_source_port() {
        fn row(pid: &str, uid: &str, id: &str, message: &str) -> String {
            serde_json::json!({"_PID": pid, "_UID": uid, "SYSLOG_IDENTIFIER": id, "MESSAGE": message}).to_string()
        }
        let user = "duduclaw-emp-70000";
        let deny_text = "User duduclaw-emp-70000 from 127.0.0.1 not allowed because listed in DenyUsers";
        for id in ["sshd", "sshd-session"] {
            let close = row("44", "0", id, "Connection closed by invalid user duduclaw-emp-70000 127.0.0.1 port 45678 [preauth]");
            let deny = row("44", "0", id, deny_text);
            assert!(journal_denies(&format!("{close}\n{deny}"), user, 45678), "{id}");
            assert!(journal_denies(&format!("{deny}\n{close}\n"), user, 45678), "{id}");
            // Corroboration only, or wrong source port.
            assert!(!journal_denies(&close, user, 45678));
            assert!(!journal_denies(&format!("{close}\n{deny}"), user, 45679));
            // Another sshd session (wrong PID), a non-root record, another user.
            let other = row("45", "0", id, deny_text);
            assert!(!journal_denies(&format!("{close}\n{other}"), user, 45678));
            let forged = row("44", "70000", id, deny_text);
            assert!(!journal_denies(&format!("{close}\n{forged}"), user, 45678));
            assert!(!journal_denies(&format!("{close}\n{deny}"), "duduclaw-emp-70001", 45678));
            // `[preauth]` suffix and `localhost` host are accepted on the DenyUsers line.
            let preauth = row("44", "0", id, &format!("{deny_text} [preauth]"));
            assert!(journal_denies(&format!("{close}\n{preauth}"), user, 45678));
            let localhost = row("44", "0", id,
                "User duduclaw-emp-70000 from localhost not allowed because listed in DenyUsers");
            assert!(journal_denies(&format!("{close}\n{localhost}"), user, 45678));
            // The other session markers of the harness.
            let disconnected = row("44", "0", id, "Disconnected from invalid user duduclaw-emp-70000 127.0.0.1 port 45678 [preauth]");
            assert!(journal_denies(&format!("{disconnected}\n{deny}"), user, 45678));
            let failed = row("44", "0", id,
                "Failed publickey for invalid user duduclaw-emp-70000 from 127.0.0.1 port 45678 ssh2: ED25519 SHA256:x");
            assert!(journal_denies(&format!("{failed}\n{deny}"), user, 45678));
            // Garbage in the stream refuses.
            assert!(!journal_denies(&format!("{close}\n-- No entries --\n{deny}"), user, 45678));
        }
        // A record from another program, even root with the same PID, is not sshd's.
        let close = row("44", "0", "sshd", "Connection closed by invalid user duduclaw-emp-70000 127.0.0.1 port 45678 [preauth]");
        let spoofed = row("44", "0", "logger", deny_text);
        assert!(!journal_denies(&format!("{close}\n{spoofed}"), user, 45678));
        let no_id = serde_json::json!({"_PID": "44", "_UID": "0", "MESSAGE": deny_text}).to_string();
        assert!(!journal_denies(&format!("{close}\n{no_id}"), user, 45678));
        assert!(!journal_denies("", user, 45678));
    }

    #[test]
    fn probe_argv_accepts_only_an_account_or_the_numeric_relay_form() {
        fn args(list: &[&str]) -> Vec<OsString> { list.iter().map(OsString::from).collect() }
        assert_eq!(parse_args(&args(&["duduclaw-emp-70000"])), Some(Mode::Probe("duduclaw-emp-70000".into())));
        // Name validity is the identity stage's decision, not a usage error.
        assert_eq!(parse_args(&args(&["root"])), Some(Mode::Probe("root".into())));
        assert_eq!(parse_args(&args(&["--relay", "40000", "22"])), Some(Mode::Relay { source: 40000, dest: 22 }));
        assert_eq!(parse_args(&args(&["--relay", "65535", "1"])), Some(Mode::Relay { source: 65535, dest: 1 }));
        for bad in [vec![], vec!["a", "b"], vec!["--relay"], vec!["--relay", "1"], vec!["--relay", "1", "2", "3"],
            vec!["--relay", "0", "22"], vec!["--relay", "65536", "22"], vec!["--relay", "022", "22"],
            vec!["--relay", "+1", "22"], vec!["--relay", "1", "-22"], vec!["--relay", " 1", "22"],
            vec!["--relay", "1e3", "22"], vec!["--relay", "", "22"], vec!["--relay", "40000", "ssh"],
            vec!["--Relay", "1", "2"], vec!["-x"], vec!["--help"], vec![""]] {
            assert_eq!(parse_args(&args(&bad)), None, "{bad:?}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            assert_eq!(parse_args(&[OsString::from_vec(vec![0xff, b'a'])]), None);
        }
    }

    #[test]
    fn probe_identity_requires_root_the_name_rule_and_an_employee_uid() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
            duduclaw-emp-70000:x:70000:70000::/nonexistent:/usr/sbin/nologin\n\
            duduclaw-emp-low:x:1000:1000::/nonexistent:/usr/sbin/nologin\n\
            duduclaw-emp-dup:x:70001:70001::/nonexistent:/usr/sbin/nologin\n\
            duduclaw-emp-dup:x:70002:70002::/nonexistent:/usr/sbin/nologin\n\
            duduclaw-emp-short:x:70003:70003\n\
            duduclaw-emp-high:x:100000:100000::/nonexistent:/usr/sbin/nologin\n";
        assert!(identity_allowed(0, "duduclaw-emp-70000", passwd));
        assert!(!identity_allowed(1000, "duduclaw-emp-70000", passwd));
        for name in ["duduclaw-emp-low", "duduclaw-emp-dup", "duduclaw-emp-short", "duduclaw-emp-high",
            "duduclaw-emp-absent", "root", "duduclaw-emp-", "duduclaw-emp-70000:x"] {
            assert!(!identity_allowed(0, name, passwd), "{name}");
        }
        assert_eq!(passwd_uid(passwd, "duduclaw-emp-70000"), Some(70000));
        assert_eq!(passwd_uid(passwd, "duduclaw-emp-dup"), None);
    }

    #[test]
    fn probe_ssh_arguments_pin_the_harness_option_set() {
        let proxy = proxy_command("/usr/lib/duduclaw/duduclaw-isolation-probe", 40000, 22);
        assert_eq!(proxy, "/usr/lib/duduclaw/duduclaw-isolation-probe --relay 40000 22");
        let login = ssh_arguments("/tmp/w/key", &proxy, "duduclaw-emp-70000", false);
        let options: Vec<&str> = login.windows(2).filter(|pair| pair[0] == "-o").map(|pair| pair[1].as_str()).collect();
        assert_eq!(options, ["BatchMode=yes", "IdentitiesOnly=yes", "IdentityFile=/tmp/w/key",
            "PreferredAuthentications=publickey", "PubkeyAuthentication=yes", "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no", "StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null",
            "GlobalKnownHostsFile=/dev/null", "ConnectTimeout=5",
            "ProxyCommand=/usr/lib/duduclaw/duduclaw-isolation-probe --relay 40000 22"]);
        assert_eq!(&login[..2], ["-F", "/dev/null"]);
        assert_eq!(&login[login.len() - 4..], ["-l", "duduclaw-emp-70000", "127.0.0.1", "/usr/bin/true"]);
        assert!(!login.iter().any(|arg| arg == "-N" || arg == "-R" || arg == "-M" || arg.starts_with("Control")));
        let forward = ssh_arguments("/tmp/w/key", &proxy, "duduclaw-emp-70000", true);
        for option in ["ControlMaster=no", "ControlPath=none", "ExitOnForwardFailure=yes"] {
            assert!(forward.windows(2).any(|pair| pair[0] == "-o" && pair[1] == option), "{option}");
        }
        assert!(forward.windows(3).any(|w| w[0] == "-N" && w[1] == "-R" && w[2] == "127.0.0.1:0:127.0.0.1:9"));
        assert_eq!(&forward[forward.len() - 3..], ["-l", "duduclaw-emp-70000", "127.0.0.1"]);
        assert!(!forward.iter().any(|arg| arg == "-M" || arg == "-f" || arg == "/usr/bin/true"));
    }

    #[test]
    fn probe_tool_metadata_and_proxy_path_rules() {
        assert!(tool_metadata_safe(true, 0, 0o100755));
        assert!(tool_metadata_safe(true, 0, 0o100711));
        for (is_file, uid, mode) in [(false, 0, 0o040755), (true, 1000, 0o100755), (true, 0, 0o100775),
            (true, 0, 0o100757), (true, 0, 0o100644)] {
            assert!(!tool_metadata_safe(is_file, uid, mode), "{is_file} {uid} {mode:o}");
        }
        assert_eq!(TOOLS, ["/usr/sbin/sshd", "/usr/bin/ssh", "/usr/bin/ssh-keygen", "/usr/bin/journalctl"]);
        assert!(proxy_path_safe("/usr/lib/duduclaw/duduclaw-isolation-probe"));
        assert!(proxy_path_safe("/var/lib/duduclaw-probe-test/duduclaw-isolation-probe"));
        for bad in ["duduclaw-isolation-probe", "/usr/lib/a b", "/usr/lib/a;b", "/usr/lib/%h", "/x/../y",
            "/usr/lib/probe (deleted)", "/usr/lib/$x", ""] {
            assert!(!proxy_path_safe(bad), "{bad}");
        }
    }

    #[test]
    fn probe_success_line_is_the_fixed_json_shape() {
        let line = success_line("duduclaw-emp-70000", 22);
        assert_eq!(line, "{\"version\":1,\"account\":\"duduclaw-emp-70000\",\"port\":22,\"result\":\"denied\"}\n");
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value, serde_json::json!({"version": 1, "account": "duduclaw-emp-70000", "port": 22, "result": "denied"}));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_relay_connects_from_the_requested_source_port_and_passes_bytes_both_ways() {
        use std::io::{Read, Write};
        use std::net::{Ipv4Addr, SocketAddr, TcpListener};
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        use std::time::Duration;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let dest = listener.local_addr().unwrap().port();
        let source = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        let (mut feed, input) = UnixStream::pair().unwrap();
        let (output, mut drain) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let result = super::linux::relay(source, dest, input.as_raw_fd(), output.as_raw_fd());
            drop((input, output));
            result.map_err(|error| error.to_string())
        });
        let (mut server, peer) = listener.accept().unwrap();
        assert_eq!(peer, SocketAddr::from((Ipv4Addr::LOCALHOST, source)));
        server.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        drain.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        feed.write_all(b"client-to-sshd").unwrap();
        let mut forward = [0u8; 14];
        server.read_exact(&mut forward).unwrap();
        assert_eq!(&forward, b"client-to-sshd");
        server.write_all(b"sshd-to-client").unwrap();
        let mut backward = [0u8; 14];
        drain.read_exact(&mut backward).unwrap();
        assert_eq!(&backward, b"sshd-to-client");
        // The client side closing ends the relay, which closes the connection.
        drop(feed);
        assert_eq!(worker.join().unwrap(), Ok(()));
        let mut rest = Vec::new();
        assert_eq!(server.read_to_end(&mut rest).unwrap(), 0);
    }
}
