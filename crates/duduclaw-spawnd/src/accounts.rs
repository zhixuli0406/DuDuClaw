//! Host account lockdown and complete namespace checks (§5.1, R7-M4/M5).
use std::{collections::BTreeMap, io};
use duduclaw_isolation::ids::UidRange;
use crate::registry::{account_name, Registry};
use crate::registry::EmployeeAccount;
#[cfg(target_os = "linux")]
use crate::registry::HostUidPolicy;

fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "employee account verification unavailable") }

pub fn parse_login_defs(text: &str) -> io::Result<(UidRange, UidRange)> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        let Some(key) = words.next() else { continue };
        if !matches!(key, "SYS_UID_MIN" | "SYS_UID_MAX" | "UID_MIN" | "UID_MAX"
            | "SYS_GID_MIN" | "SYS_GID_MAX" | "GID_MIN" | "GID_MAX") { continue; }
        let value: u32 = words.next().ok_or_else(denied)?.parse().map_err(|_| denied())?;
        if words.next().is_some() || values.insert(key, value).is_some() { return Err(denied()); }
    }
    let get = |key| values.get(key).copied().ok_or_else(denied);
    let system = UidRange { first: get("SYS_UID_MIN")?.min(get("SYS_GID_MIN")?), last: get("SYS_UID_MAX")?.max(get("SYS_GID_MAX")?) };
    let login = UidRange { first: get("UID_MIN")?.min(get("GID_MIN")?), last: get("UID_MAX")?.max(get("GID_MAX")?) };
    if system.first > system.last || login.first > login.last { return Err(denied()); }
    Ok((system, login))
}

pub fn parse_subids(text: &str) -> io::Result<Vec<UidRange>> {
    let mut ranges = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() != 3 || fields[0].is_empty() { return Err(denied()); }
        let first: u32 = fields[1].parse().map_err(|_| denied())?;
        let count: u32 = fields[2].parse().map_err(|_| denied())?;
        let last = first.checked_add(count.checked_sub(1).ok_or_else(denied)?).ok_or_else(denied)?;
        ranges.push(UidRange { first, last });
    }
    Ok(ranges)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswdEntry { pub name: String, pub uid: u32, pub gid: u32, pub home: String, pub shell: String }
pub fn parse_passwd(text: &str) -> io::Result<Vec<PasswdEntry>> {
    text.lines().map(|line| {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() != 7 || fields[0].is_empty() { return Err(denied()); }
        Ok(PasswdEntry { name: fields[0].into(), uid: fields[2].parse().map_err(|_| denied())?,
            gid: fields[3].parse().map_err(|_| denied())?, home: fields[5].into(), shell: fields[6].into() })
    }).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupEntry { pub name: String, pub gid: u32, pub members: Vec<String> }
pub fn parse_group(text: &str) -> io::Result<Vec<GroupEntry>> {
    text.lines().map(|line| {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() != 4 || fields[0].is_empty() { return Err(denied()); }
        Ok(GroupEntry { name: fields[0].into(), gid: fields[2].parse().map_err(|_| denied())?,
            members: fields[3].split(',').filter(|member| !member.is_empty()).map(str::to_owned).collect() })
    }).collect()
}

pub fn verify_shadow(text: &str, name: &str) -> io::Result<()> {
    let rows: Vec<_> = text.lines().map(|line| line.split(':').collect::<Vec<_>>()).collect();
    if rows.len() != 1 || rows[0].len() != 9 || rows[0][0] != name || rows[0][1] != "!*" || rows[0][7] != "1" {
        return Err(denied());
    }
    Ok(())
}

/// cron/at allow files override deny files. Both sources must be inspected.
pub fn scheduler_denied(name: &str, allow: Option<&str>, deny: &str) -> bool {
    let has = |text: &str| text.lines().any(|line| line.trim() == name);
    !allow.is_some_and(has) && has(deny)
}

pub fn ssh_effective_denied(text: &str) -> bool {
    let mut deny = false;
    let mut forwarding = false;
    let mut tty = false;
    for line in text.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        match words.first().copied() {
            Some("denyusers") => deny = words.iter().skip(1).any(|word| *word == "duduclaw-emp-*"),
            Some("disableforwarding") => forwarding = words.as_slice() == ["disableforwarding", "yes"],
            Some("permittty") => tty = words.as_slice() == ["permittty", "no"],
            _ => {},
        }
    }
    deny && forwarding && tty
}

/// NSS modules outside files/systemd need an explicit, permanent IAM reservation.
pub fn external_nss(text: &str) -> io::Result<bool> {
    let mut seen = 0;
    let mut external = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((database, modules)) = line.split_once(':') else { continue };
        if !matches!(database.trim(), "passwd" | "group") { continue; }
        seen += 1;
        for module in modules.split_whitespace() {
            // Bracketed return clauses are not modules, but unknown syntax denies.
            if module == "files" || module == "systemd" { continue; }
            external = true;
        }
    }
    if seen != 2 { return Err(denied()); }
    Ok(external)
}

/// UID aliases are checked by enumeration, not just getpwuid's first result.
pub fn verify_enumerated(registry: &Registry, users: &[PasswdEntry], groups: &[GroupEntry]) -> io::Result<()> {
    let in_range = |id| id >= registry.range.first && id <= registry.range.last;
    let mut user_ids = std::collections::BTreeSet::new();
    let mut group_ids = std::collections::BTreeSet::new();
    for user in users.iter().filter(|user| in_range(user.uid)) {
        if !user_ids.insert(user.uid) || !registry.employees.iter().any(|entry|
            entry.uid == user.uid && entry.gid == user.gid && user.name == account_name(entry.uid)) { return Err(denied()); }
    }
    for group in groups.iter().filter(|group| in_range(group.gid)) {
        if !group_ids.insert(group.gid) || !registry.employees.iter().any(|entry|
            entry.gid == group.gid && group.name == account_name(entry.uid)) { return Err(denied()); }
    }
    Ok(())
}

/// Recovery may alter only the exact reserved local name/uid pair. Check names
/// outside the employee range too; getpwuid alone hides duplicate NSS entries.
pub fn partial_identity(account: &EmployeeAccount, users: &[PasswdEntry], groups: &[GroupEntry]) -> io::Result<(bool, bool)> {
    let name = account_name(account.uid);
    let users: Vec<_> = users.iter().filter(|entry| entry.uid == account.uid || entry.name == name).collect();
    let groups: Vec<_> = groups.iter().filter(|entry| entry.gid == account.gid || entry.name == name).collect();
    if account.uid != account.gid || users.len() > 1 || groups.len() > 1
        || users.iter().any(|entry| entry.name != name || entry.uid != account.uid || entry.gid != account.gid)
        || groups.iter().any(|entry| entry.name != name || entry.gid != account.gid || !entry.members.is_empty()) {
        return Err(denied());
    }
    Ok((!users.is_empty(), !groups.is_empty()))
}

/// Deployment evidence is supplied by the privileged installer/readiness harness.
/// It cannot be supplied by SpawnRequest or inferred from a successful `sshd -T`.
pub trait InstallationEvidence: Send + Sync {
    fn external_nss_range_reserved_and_enumerable(&self, range: UidRange) -> io::Result<()>;
    fn no_existing_file_owner(&self, uid: u32) -> io::Result<()>;
    fn ssh_public_key_and_forwarding_denied(&self, account: &str) -> io::Result<()>;
}

/// The binary never substitutes an operator assertion for installation evidence.
pub struct MissingInstallationEvidence;
impl InstallationEvidence for MissingInstallationEvidence {
    fn external_nss_range_reserved_and_enumerable(&self, _: UidRange) -> io::Result<()> { Err(denied()) }
    fn no_existing_file_owner(&self, _: u32) -> io::Result<()> { Err(denied()) }
    fn ssh_public_key_and_forwarding_denied(&self, _: &str) -> io::Result<()> { Err(denied()) }
}

#[cfg(any(target_os = "linux", test))]
#[path = "accounts_userdb.rs"]
mod userdb;

/// All four credential IDs matter: a different effective uid does not make a
/// process with the employee's saved uid safe to ignore.
pub fn process_uids(status: &str) -> io::Result<[u32; 4]> {
    let rows: Vec<_> = status.lines().filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| *name == "Uid").collect();
    if rows.len() != 1 { return Err(denied()); }
    let values = rows[0].1.split_whitespace().map(|value| value.parse().map_err(|_| denied()))
        .collect::<io::Result<Vec<u32>>>()?;
    values.try_into().map_err(|_| denied())
}

pub fn unified_cgroup(text: &str) -> io::Result<&str> {
    let mut rows = text.lines();
    let row = rows.next().ok_or_else(denied)?;
    if rows.next().is_some() { return Err(denied()); }
    let mut fields = row.splitn(3, ':');
    if fields.next() != Some("0") || fields.next() != Some("") { return Err(denied()); }
    let path = fields.next().ok_or_else(denied)?;
    if !valid_cgroup_path(path) { return Err(denied()); }
    Ok(path)
}

pub fn cgroup_member(actual: &str, expected: &str) -> bool {
    // Parse Linux paths identically in portable policy tests, including Windows.
    if !valid_cgroup_path(actual) || !valid_cgroup_path(expected) || expected == "/" { return false; }
    let mut components = actual.split('/');
    expected.split('/').all(|component| components.next() == Some(component))
}

fn valid_cgroup_path(path: &str) -> bool {
    if path == "/" { return true; }
    path.strip_prefix('/').is_some_and(|relative| relative.split('/').all(|part|
        !part.is_empty() && !matches!(part, "." | "..") && !part.chars().any(|ch| ch == '\\' || ch.is_control())))
}

/// A detector record is NOT a safe signal target. The caller must acquire a
/// pidfd, re-read start_ticks/credentials/cgroup, then signal via that pidfd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnexpectedProcess { pub pid: u32, pub start_ticks: u64 }

#[cfg(any(target_os = "linux", test))]
fn start_ticks(stat: &str) -> io::Result<u64> {
    stat.rsplit_once(')').ok_or_else(denied)?.1.split_whitespace().nth(19)
        .ok_or_else(denied)?.parse().map_err(|_| denied())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::registry::{root_path, AccountHost};
    use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, os::{fd::OwnedFd, unix::{fs::OpenOptionsExt, net::UnixStream}},
        path::Path, process::{Command, Stdio}, time::{Duration, Instant}};

    use super::userdb::{MAX_HOST_BYTES, COMMAND_SECONDS};
    const SSH_DROPIN: &str = "DenyUsers duduclaw-emp-* duduclaw duduclaw-runsup duduclaw-egress duduclaw-dl\nMatch User duduclaw-emp-*\n    DisableForwarding yes\n    PermitTTY no\nMatch all\n";

    fn host_text(path: &Path) -> io::Result<String> {
        root_path(path, false)?;
        let mut bytes = Vec::new();
        File::open(path)?.take(MAX_HOST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_HOST_BYTES { return Err(denied()); }
        String::from_utf8(bytes).map_err(|_| denied())
    }
    fn optional_text(path: &Path) -> io::Result<Option<String>> {
        match fs::symlink_metadata(path) {
            Ok(_) => host_text(path).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Fixed executable paths and argv only; bounded output, deadline, no inherited env.
    fn command(path: &str, args: &[&str]) -> io::Result<(i32, String)> {
        root_path(Path::new(path), false)?;
        let (mut output, writer) = UnixStream::pair()?;
        output.set_nonblocking(true)?;
        let mut child = Command::new(path).args(args).env_clear().env("LC_ALL", "C")
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").stdin(Stdio::null())
            .stdout(Stdio::from(OwnedFd::from(writer))).stderr(Stdio::null()).spawn()?;
        let deadline = Instant::now() + Duration::from_secs(COMMAND_SECONDS);
        let mut bytes = Vec::new();
        let mut eof = false;
        loop {
            let mut buffer = [0; 8192];
            match output.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                Err(_) => { let _ = child.kill(); let _ = child.wait(); return Err(denied()); },
            }
            if bytes.len() as u64 > MAX_HOST_BYTES { let _ = child.kill(); let _ = child.wait(); return Err(denied()); }
            if eof {
                if let Some(status) = child.try_wait()? {
                    return Ok((status.code().ok_or_else(denied)?, String::from_utf8(bytes).map_err(|_| denied())?));
                }
            }
            if Instant::now() >= deadline { let _ = child.kill(); let _ = child.wait(); return Err(denied()); }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn success(path: &str, args: &[&str]) -> io::Result<String> {
        let (status, text) = command(path, args)?;
        if status != 0 { return Err(denied()); }
        Ok(text)
    }

    fn replace_root_file(path: &Path, text: &str) -> io::Result<()> {
        let parent = path.parent().ok_or_else(denied)?;
        root_path(parent, true)?;
        if fs::symlink_metadata(path).is_ok() { root_path(path, false)?; }
        let temporary = parent.join(format!(".duduclaw-{}.next", path.file_name().ok_or_else(denied)?.to_str().ok_or_else(denied)?));
        // A crash may leave our own stage. Only a root-owned single-link file in
        // the trusted parent can be removed; no employee-controlled pathname.
        match fs::symlink_metadata(&temporary) {
            Ok(_) => { root_path(&temporary, false)?; fs::remove_file(&temporary)?; File::open(parent)?.sync_all()?; },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        File::open(parent)?.sync_all()
    }

    pub struct LinuxAccounts<'a> { pub evidence: &'a dyn InstallationEvidence }

    impl LinuxAccounts<'_> {
        fn partial_local_identity(&self, account: &EmployeeAccount) -> io::Result<(bool, bool)> {
            let registry = Registry { employees: vec![account.clone()], ..Registry::default() };
            if external_nss(&host_text(Path::new("/etc/nsswitch.conf"))?)? {
                self.evidence.external_nss_range_reserved_and_enumerable(registry.range)?;
            }
            let nss = partial_identity(account, &parse_passwd(&success("/usr/bin/getent", &["passwd"])?)?,
                &parse_group(&success("/usr/bin/getent", &["group"])?)?)?;
            let local = partial_identity(account, &parse_passwd(&host_text(Path::new("/etc/passwd"))?)?,
                &parse_group(&host_text(Path::new("/etc/group"))?)?)?;
            if nss != local { return Err(denied()); }
            let ranges = [registry.range, UidRange { first: account.uid, last: account.uid }];
            for records in self.userdb_identities(&ranges, &[(account.uid, account.gid)])? {
                let name = account_name(account.uid);
                let mut matched = false;
                for (id, actual) in records {
                    if id == account.uid || actual == name {
                        if matched || id != account.uid || actual != name { return Err(denied()); }
                        matched = true;
                    }
                }
            }
            Ok(local)
        }

        fn install_login_barriers(&self, name: &str) -> io::Result<()> {
            for family in ["cron", "at"] {
                let allow = format!("/etc/{family}.allow");
                if let Some(content) = optional_text(Path::new(&allow))? {
                    if content.lines().any(|line| line.trim() == name) {
                        let filtered = content.lines().filter(|line| line.trim() != name).collect::<Vec<_>>().join("\n") + "\n";
                        replace_root_file(Path::new(&allow), &filtered)?;
                    }
                }
                let deny = format!("/etc/{family}.deny");
                let mut content = optional_text(Path::new(&deny))?.unwrap_or_default();
                if !content.lines().any(|line| line.trim() == name) {
                    if !content.is_empty() && !content.ends_with('\n') { content.push('\n'); }
                    content.push_str(name); content.push('\n');
                    replace_root_file(Path::new(&deny), &content)?;
                }
            }
            if Path::new("/usr/sbin/sshd").exists() {
                replace_root_file(Path::new("/etc/ssh/sshd_config.d/10-duduclaw.conf"), SSH_DROPIN)?;
                success("/usr/sbin/sshd", &["-t"])?;
                let mut reloaded = false;
                for unit in ["ssh.service", "sshd.service"] {
                    let (status, state) = command("/usr/bin/systemctl", &["show", "--property=ActiveState", "--value", unit])?;
                    if status == 0 && state.trim() == "active" {
                        success("/usr/bin/systemctl", &["reload", unit])?;
                        reloaded = true;
                    }
                }
                if !reloaded { return Err(denied()); }
            }
            Ok(())
        }

        fn lockdown_existing(&self, account: &EmployeeAccount) -> io::Result<()> {
            let name = account_name(account.uid);
            success("/usr/sbin/usermod", &["--password", "!*", "--expiredate", "1970-01-02",
                "--shell", "/usr/sbin/nologin", "--home", "/nonexistent", "--groups", "", &name])?;
            success("/usr/bin/chage", &["-E", "1", &name])?;
            success("/usr/bin/loginctl", &["disable-linger", &name])?;
            let uid = account.uid.to_string();
            if success("/usr/bin/loginctl", &["list-sessions", "--no-legend", "--no-pager"])?.lines()
                .any(|line| line.split_whitespace().nth(1) == Some(uid.as_str())) {
                success("/usr/bin/loginctl", &["terminate-user", &uid])?;
            }
            success("/usr/bin/systemctl", &["stop", &format!("user@{}.service", account.uid)])?;
            self.install_login_barriers(&name)
        }

        fn userdb_identities(&self, ranges: &[UidRange], employees: &[(u32, u32)]) -> io::Result<Vec<Vec<(u32, String)>>> {
            match fs::symlink_metadata("/run/systemd/userdb") {
                Ok(_) => root_path(Path::new("/run/systemd/userdb"), true)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            }
            let users = parse_passwd(&host_text(Path::new("/etc/passwd"))?)?;
            let groups = parse_group(&host_text(Path::new("/etc/group"))?)?;
            let shadow = host_text(Path::new("/etc/shadow"))?;
            userdb::identities(Path::new("/run/systemd/userdb"), &userdb::LocalIdentity {
                ranges, users: &users, groups: &groups, shadow: &shadow,
            }, employees)
        }

        fn userdb(&self, registry: &Registry) -> io::Result<()> {
            for values in self.userdb_identities(&[registry.range],
                &registry.employees.iter().map(|entry| (entry.uid, entry.gid)).collect::<Vec<_>>())? {
                let mut seen = std::collections::BTreeSet::new();
                for (id, name) in values {
                    if id >= registry.range.first && id <= registry.range.last
                        && (!seen.insert(id) || !registry.employees.iter().any(|entry| entry.uid == id && name == account_name(id))) {
                        return Err(denied());
                    }
                }
            }
            Ok(())
        }

        fn verify_schedulers(&self, name: &str) -> io::Result<()> {
            for family in ["cron", "at"] {
                let allow = optional_text(Path::new(&format!("/etc/{family}.allow")))?;
                let deny = host_text(Path::new(&format!("/etc/{family}.deny")))?;
                if !scheduler_denied(name, allow.as_deref(), &deny) { return Err(denied()); }
            }
            for path in [format!("/var/spool/cron/crontabs/{name}"), format!("/var/spool/cron/{name}")] {
                match fs::symlink_metadata(path) { Ok(_) => return Err(denied()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}, Err(error) => return Err(error) }
            }
            let mut cron_files = vec![Path::new("/etc/crontab").to_path_buf()];
            if Path::new("/etc/cron.d").exists() {
                root_path(Path::new("/etc/cron.d"), true)?;
                for file in fs::read_dir("/etc/cron.d")? { cron_files.push(file?.path()); }
            }
            for file in cron_files {
                let Some(text) = optional_text(&file)? else { continue };
                for line in text.lines() {
                    let words: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
                    let owner = if words.first().is_some_and(|word| word.as_bytes().first() == Some(&b'@')) {
                        words.get(1)
                    } else { words.get(5) };
                    if owner.is_some_and(|owner| *owner == name) { return Err(denied()); }
                }
            }
            if Path::new("/usr/bin/at").exists() {
                if success("/usr/bin/at", &["-l"])?.lines().any(|line| line.split_whitespace().last() == Some(name)) { return Err(denied()); }
            } else if Path::new("/var/spool/cron/atjobs").exists() || Path::new("/var/spool/at").exists() {
                return Err(denied());
            }
            Ok(())
        }

        fn verify_login(&self, account: &EmployeeAccount) -> io::Result<()> {
            let name = account_name(account.uid);
            if Path::new(&format!("/var/lib/systemd/linger/{name}")).exists() { return Err(denied()); }
            let unit = format!("user@{}.service", account.uid);
            let state = success("/usr/bin/systemctl", &["show", "--property=ActiveState", "--value", &unit])?;
            if !matches!(state.trim(), "inactive" | "failed") { return Err(denied()); }
            let uid = account.uid.to_string();
            if success("/usr/bin/loginctl", &["list-sessions", "--no-legend", "--no-pager"])?.lines()
                .any(|line| line.split_whitespace().nth(1) == Some(uid.as_str())) { return Err(denied()); }
            if Path::new("/usr/sbin/sshd").exists() {
                success("/usr/sbin/sshd", &["-t"])?;
                let context = format!("user={name},host=localhost,addr=127.0.0.1");
                if !ssh_effective_denied(&success("/usr/sbin/sshd", &["-T", "-C", &context])?) { return Err(denied()); }
                self.evidence.ssh_public_key_and_forwarding_denied(&name)?;
            }
            Ok(())
        }
    }

    /// Run before admission and every two seconds while the lease is held.
    /// None means this uid must have no processes anywhere on the host.
    pub fn unexpected_processes(uid: u32, allowed_cgroup: Option<&str>) -> io::Result<Vec<UnexpectedProcess>> {
        let mut found = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let Ok(pid) = name.parse::<u32>() else { continue };
            let inspect = || -> io::Result<Option<UnexpectedProcess>> {
                let before = start_ticks(&fs::read_to_string(entry.path().join("stat"))?)?;
                let uids = process_uids(&fs::read_to_string(entry.path().join("status"))?)?;
                if !uids.iter().any(|value| *value == uid) { return Ok(None); }
                let cgroups = fs::read_to_string(entry.path().join("cgroup"))?;
                let actual = unified_cgroup(&cgroups)?;
                if allowed_cgroup.is_some_and(|allowed| cgroup_member(actual, allowed)) { return Ok(None); }
                let after = start_ticks(&fs::read_to_string(entry.path().join("stat"))?)?;
                if before != after { return Err(denied()); }
                Ok(Some(UnexpectedProcess { pid, start_ticks: after }))
            };
            match inspect() {
                Ok(Some(process)) => found.push(process), Ok(None) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        }
        Ok(found)
    }

    /// Signal a freshly verified intruder through a pidfd, never through its PID.
    /// A disappearing process is harmless; mismatched identity fails closed and
    /// must fence admission until the caller obtains another complete host scan.
    pub fn terminate_intruder(process: &UnexpectedProcess, uid: u32, allowed_cgroup: Option<&str>) -> io::Result<bool> {
        use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
        // The caller must supply an identity from its validated permanent
        // registry; configured employee ranges need not equal the defaults.
        if process.pid <= 1 || duduclaw_isolation::ids::PERMANENT_UID_EXCLUSIONS.iter()
            .any(|range| uid >= range.first && uid <= range.last) { return Err(denied()); }
        let raw: i32 = process.pid.try_into().map_err(|_| denied())?;
        let pid = Pid::from_raw(raw).ok_or_else(denied)?;
        let descriptor = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(error) if error == rustix::io::Errno::SRCH => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let root = Path::new("/proc").join(process.pid.to_string());
        let verify = || -> io::Result<()> {
            if start_ticks(&fs::read_to_string(root.join("stat"))?)? != process.start_ticks { return Err(denied()); }
            if !process_uids(&fs::read_to_string(root.join("status"))?)?.iter().any(|value| *value == uid) { return Err(denied()); }
            let cgroups = fs::read_to_string(root.join("cgroup"))?;
            let actual = unified_cgroup(&cgroups)?;
            if allowed_cgroup.is_some_and(|allowed| cgroup_member(actual, allowed)) { return Err(denied()); }
            if start_ticks(&fs::read_to_string(root.join("stat"))?)? != process.start_ticks { return Err(denied()); }
            Ok(())
        };
        match verify() {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
        match pidfd_send_signal(&descriptor, Signal::KILL) {
            Ok(()) => Ok(true),
            Err(error) if error == rustix::io::Errno::SRCH => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    impl AccountHost for LinuxAccounts<'_> {
        fn policy(&mut self) -> io::Result<HostUidPolicy> {
            let (system, login) = parse_login_defs(&host_text(Path::new("/etc/login.defs"))?)?;
            Ok(HostUidPolicy { system, login,
                subuids: parse_subids(&host_text(Path::new("/etc/subuid"))?)?,
                subgids: parse_subids(&host_text(Path::new("/etc/subgid"))?)?,
                systemd: duduclaw_isolation::ids::PERMANENT_UID_EXCLUSIONS.to_vec() })
        }

        fn verify_namespace(&mut self, registry: &Registry) -> io::Result<()> {
            if external_nss(&host_text(Path::new("/etc/nsswitch.conf"))?)? {
                self.evidence.external_nss_range_reserved_and_enumerable(registry.range)?;
            }
            let users = parse_passwd(&success("/usr/bin/getent", &["passwd"])?)?;
            let groups = parse_group(&success("/usr/bin/getent", &["group"])?)?;
            if users.is_empty() || groups.is_empty() { return Err(denied()); }
            verify_enumerated(registry, &users, &groups)?;
            // A retired/partially-created account must not regain a login entry
            // merely because its permanent registry row is a tombstone.
            for user in users.iter().filter(|user| user.uid >= registry.range.first && user.uid <= registry.range.last) {
                let account = registry.employees.iter().find(|entry| entry.uid == user.uid).ok_or_else(denied)?;
                self.verify_locked(account)?;
            }
            self.userdb(registry)
        }

        fn uid_available(&mut self, uid: u32) -> io::Result<bool> {
            let id = uid.to_string();
            let name = account_name(uid);
            for database in ["passwd", "group"] {
                for key in [&id, &name] {
                    match command("/usr/bin/getent", &[database, key])? {
                        (0, _) => return Ok(false), (2, text) if text.is_empty() => {}, _ => return Err(denied()),
                    }
                }
            }
            self.evidence.no_existing_file_owner(uid)?;
            Ok(true)
        }

        fn create_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            let uid = account.uid.to_string();
            let name = account_name(account.uid);
            if self.partial_local_identity(account)? != (false, false) { return Err(denied()); }
            self.install_login_barriers(&name)?;
            success("/usr/sbin/groupadd", &["--gid", &uid, &name])?;
            success("/usr/sbin/useradd", &["--uid", &uid, "--gid", &uid, "--no-user-group", "--no-create-home",
                "--no-log-init", "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0", "--key", "CREATE_MAIL_SPOOL=no",
                "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin", "--password", "!*", "--expiredate", "1970-01-02", &name])?;
            success("/usr/bin/chage", &["-E", "1", &name])?;
            success("/usr/bin/loginctl", &["disable-linger", &name])?;
            self.verify_locked(account)
        }

        fn recover_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            let (user, group) = self.partial_local_identity(account)?;
            if user && !group { return Err(denied()); }
            if user { self.lockdown_existing(account)?; }
            // No user is ever recreated for a tombstone. A group-only residue
            // stays reserved and cannot be assigned to a future employee.
            for process in unexpected_processes(account.uid, None)? {
                terminate_intruder(&process, account.uid, None)?;
            }
            let deadline = Instant::now() + Duration::from_secs(COMMAND_SECONDS);
            loop {
                if unexpected_processes(account.uid, None)?.is_empty() { break; }
                if Instant::now() >= deadline { return Err(denied()); }
                std::thread::sleep(Duration::from_millis(10));
            }
            if user { self.verify_locked(account)?; }
            Ok(())
        }

        fn verify_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            let name = account_name(account.uid);
            let users = parse_passwd(&success("/usr/bin/getent", &["passwd", &name])?)?;
            if users.len() != 1 || users[0].uid != account.uid || users[0].gid != account.gid
                || users[0].home != "/nonexistent" || users[0].shell != "/usr/sbin/nologin" { return Err(denied()); }
            verify_shadow(&success("/usr/bin/getent", &["shadow", &name])?, &name)?;
            let groups = success("/usr/bin/id", &["--groups", &name])?;
            let actual: Vec<u32> = groups.split_whitespace().map(|value| value.parse().map_err(|_| denied())).collect::<io::Result<_>>()?;
            if actual != [account.gid] { return Err(denied()); }
            self.verify_schedulers(&name)?;
            self.verify_login(account)
        }
    }

    #[cfg(test)]
    mod root_tests {
        use super::*;
        use crate::registry::AccountState;

        struct FixtureAccount(String);
        impl Drop for FixtureAccount {
            fn drop(&mut self) {
                let _ = command("/usr/sbin/userdel", &[&self.0]);
                let _ = command("/usr/sbin/groupdel", &[&self.0]);
            }
        }

        #[test]
        #[ignore = "requires disposable root Linux VM with systemd/logind; DUDUCLAW_ISO_DISPOSABLE=1; mutates uid 99997 and login ACLs"]
        fn iso_root_half_created_account_is_locked_and_read_back() {
            assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
            let account = EmployeeAccount { employee: duduclaw_isolation::ids::EmployeeId([97; 16]),
                uid: 99997, gid: 99997, state: AccountState::Tombstone };
            let mut host = LinuxAccounts { evidence: &MissingInstallationEvidence };
            assert_eq!(host.partial_local_identity(&account).unwrap(), (false, false));
            let name = account_name(account.uid);
            success("/usr/sbin/groupadd", &["--gid", "99997", &name]).unwrap();
            let _fixture = FixtureAccount(name.clone());
            // Reproduce death after group creation, then after user creation.
            host.recover_locked(&account).unwrap();
            success("/usr/sbin/useradd", &["--uid", "99997", "--gid", "99997", "--no-user-group",
                "--no-create-home", "--home-dir", "/nonexistent", "--shell", "/bin/sh", &name]).unwrap();
            let recovery = host.recover_locked(&account);
            let passwd = parse_passwd(&success("/usr/bin/getent", &["passwd", &name]).unwrap()).unwrap();
            assert_eq!(passwd[0].shell, "/usr/sbin/nologin");
            assert_eq!(passwd[0].home, "/nonexistent");
            verify_shadow(&success("/usr/bin/getent", &["shadow", &name]).unwrap(), &name).unwrap();
            assert!(unexpected_processes(account.uid, None).unwrap().is_empty());
            if Path::new("/usr/sbin/sshd").exists() {
                // Host settings alone never stand in for live SSH negative proof.
                assert!(recovery.is_err());
            } else { recovery.unwrap(); }
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::{LinuxAccounts, unexpected_processes, terminate_intruder};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_host_range_parsers() {
        let defs = "SYS_UID_MIN 100\nSYS_UID_MAX 999\nUID_MIN 1000\nUID_MAX 60000\nSYS_GID_MIN 100\nSYS_GID_MAX 999\nGID_MIN 1000\nGID_MAX 60000\n";
        assert_eq!(parse_login_defs(defs).unwrap().1, UidRange { first: 1000, last: 60000 });
        assert!(parse_login_defs("UID_MIN 1000").is_err());
        assert!(parse_login_defs(&format!("{defs}UID_MIN 1001\n")).is_err());
        assert_eq!(parse_subids("alice:100000:65536\n").unwrap(), vec![UidRange { first: 100000, last: 165535 }]);
        for text in ["a:1:0", "a:4294967295:2", "a:2", "a:-1:2"] { assert!(parse_subids(text).is_err()); }
    }
    #[test]
    fn allow_file_overrides_deny_and_ssh_readback_is_exact() {
        assert!(!scheduler_denied("alice", Some("alice\n"), "alice\n"));
        assert!(scheduler_denied("alice", Some("bob\n"), "alice\n"));
        assert!(!scheduler_denied("alice", None, "malice\n"));
        assert!(ssh_effective_denied("denyusers duduclaw-emp-*\ndisableforwarding yes\npermittty no\n"));
        assert!(!ssh_effective_denied("denyusers fake-duduclaw-emp-*\ndisableforwarding yes\npermittty no\n"));
    }
    #[test]
    fn shadow_must_be_locked_and_expired() {
        assert!(verify_shadow("u:!*:1:0:99999:7::1:\n", "u").is_ok());
        assert!(verify_shadow("u:!:1:0:99999:7::1:\n", "u").is_err());
        assert!(verify_shadow("u:!*:1:0:99999:7:::\n", "u").is_err());
    }
    #[test]
    fn nss_alias_with_same_uid_is_refused() {
        let registry = Registry { employees: vec![EmployeeAccount { employee: duduclaw_isolation::ids::EmployeeId([1; 16]),
            uid: 70000, gid: 70000, state: crate::registry::AccountState::Ready }], ..Registry::default() };
        let users = parse_passwd("duduclaw-emp-70000:x:70000:70000::/nonexistent:/usr/sbin/nologin\nalias:x:70000:70000::/:/bin/sh\n").unwrap();
        assert!(verify_enumerated(&registry, &users, &[]).is_err());
        assert!(external_nss("passwd: files systemd\ngroup: files systemd\n").unwrap() == false);
        assert!(external_nss("passwd: files sss\ngroup: files sss\n").unwrap());
        assert!(external_nss("passwd: files\n").is_err());
    }
    #[test]
    fn recovery_rejects_aliases_and_accepts_group_only_residue() {
        let account = EmployeeAccount { employee: duduclaw_isolation::ids::EmployeeId([3; 16]),
            uid: 70000, gid: 70000, state: crate::registry::AccountState::Tombstone };
        let group = parse_group("duduclaw-emp-70000:x:70000:\n").unwrap();
        assert_eq!(partial_identity(&account, &[], &group).unwrap(), (false, true));
        let aliases = parse_passwd("alias:x:70000:70000::/:/bin/sh\n").unwrap();
        assert!(partial_identity(&account, &aliases, &group).is_err());
        let occupied_name = parse_passwd("duduclaw-emp-70000:x:1234:1234::/:/bin/sh\n").unwrap();
        assert!(partial_identity(&account, &occupied_name, &group).is_err());
        assert!(partial_identity(&account, &[], &parse_group("duduclaw-emp-70000:x:70000:foreign\n").unwrap()).is_err());
    }
    #[test]
    fn process_identity_and_cgroup_require_exact_components() {
        assert_eq!(process_uids("Name:\tx\nUid:\t1 70000 70000 1\n").unwrap(), [1, 70000, 70000, 1]);
        assert!(process_uids("Uid: 1 2 3\n").is_err());
        assert!(process_uids("Uid: 1 2 3 4\nUid: 1 2 3 4\n").is_err());
        assert_eq!(unified_cgroup("0::/a/b\n").unwrap(), "/a/b");
        assert!(unified_cgroup("2:cpu:/a\n0::/b\n").is_err());
        assert!(cgroup_member("/a/b/c", "/a/b"));
        assert!(!cgroup_member("/a/b-evil/c", "/a/b"));
        assert!(!cgroup_member("/a/b/../c", "/a/b"));
        assert!(!cgroup_member("/a", "/"));
    }
    #[test]
    fn proc_stat_starttime_handles_parentheses_and_spaces() {
        let fields = (4..=22).map(|number| number.to_string()).collect::<Vec<_>>().join(" ");
        assert_eq!(start_ticks(&format!("123 (name ) with spaces) S {fields}")).unwrap(), 22);
        assert!(start_ticks("123 (short) S 4").is_err());
        assert!(start_ticks("no delimiter").is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root and a disposable Linux process namespace; kills only its own fixture child"]
    fn iso_root_intruder_is_signalled_through_pidfd() {
        use std::os::unix::process::CommandExt;
        // The test must run in a disposable PID namespace with no production uid.
        assert_eq!(std::fs::read_to_string("/proc/self/uid_map").unwrap().split_whitespace().next(), Some("0"));
        let mut child = std::process::Command::new("/usr/bin/sleep").arg("30").uid(99999).gid(99999).spawn().unwrap();
        let path = format!("/proc/{}/stat", child.id());
        let identity = UnexpectedProcess { pid: child.id(), start_ticks: start_ticks(&std::fs::read_to_string(path).unwrap()).unwrap() };
        let result = terminate_intruder(&identity, 99999, None);
        if result.is_err() { let _ = child.kill(); }
        let status = child.wait().unwrap();
        assert_eq!(result.unwrap(), true);
        assert!(!status.success());
    }
}
