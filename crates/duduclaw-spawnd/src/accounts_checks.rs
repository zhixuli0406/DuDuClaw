//! R8w: the account checks of `LinuxAccounts` over a host seam ([`HostSeam`]),
//! every failure labelled with its fixed [`Step`] (and the account where one
//! applies, [`for_account`]). Production: `accounts.rs::linux::RootSeam`
//! (fixed root-owned executables through `command`, root-owned host files,
//! `lstat` presence, the direct at-spool scan of `accounts_spool.rs`).
//! Tests: a recording fake. The checks and their order are those of the
//! previous inline code, except that the at check reads the spool instead of
//! executing `/usr/bin/at`, and a presence check that could not be read
//! (`linger`, `/usr/sbin/sshd`) now fails closed instead of reading "absent".
use super::*;
use crate::registry::HostUidPolicy;
use std::path::{Path, PathBuf};

pub(crate) trait HostSeam {
    /// A fixed executable path and argv: `(exit code, stdout)`.
    fn run(&mut self, path: &'static str, args: &[&str]) -> io::Result<(i32, String)>;
    /// A required root-owned host file.
    fn text(&mut self, path: &Path) -> io::Result<String>;
    /// An optional root-owned host file (`None`: nothing at `path`).
    fn optional_text(&mut self, path: &Path) -> io::Result<Option<String>>;
    /// Whether anything (a dangling symlink included) is at `path`; an
    /// unreadable path is an error, never "absent".
    fn present(&mut self, path: &Path) -> io::Result<bool>;
    /// Every entry of `/etc/cron.d` (none when it does not exist); the
    /// directory must be root-owned.
    fn cron_d(&mut self) -> io::Result<Vec<PathBuf>>;
    /// No at spool entry is owned by `uid` (`accounts_spool.rs`).
    fn at_spool(&mut self, uid: u32) -> io::Result<()>;
    /// useradd left no mail spool for `name` ([`mail_spool_absent`]).
    fn mail_spool(&mut self, name: &str) -> io::Result<()>;
}

fn success(seam: &mut dyn HostSeam, path: &'static str, args: &[&str]) -> io::Result<String> {
    let (status, text) = seam.run(path, args)?;
    if status != 0 { return Err(denied()); }
    Ok(text)
}

fn subids(seam: &mut dyn HostSeam, path: &str) -> io::Result<Vec<UidRange>> {
    seam.text(Path::new(path)).and_then(|text| parse_subids(&text)).map_err(at_step(Step::Subids))
}
/// `AccountHost::policy`: `login_defs`, `subids`.
pub(crate) fn host_policy(seam: &mut dyn HostSeam) -> io::Result<HostUidPolicy> {
    let (system, login) = seam.text(Path::new("/etc/login.defs")).and_then(|text| parse_login_defs(&text))
        .map_err(at_step(Step::LoginDefs))?;
    Ok(HostUidPolicy { system, login, subuids: subids(seam, "/etc/subuid")?, subgids: subids(seam, "/etc/subgid")?,
        systemd: duduclaw_isolation::ids::PERMANENT_UID_EXCLUSIONS.to_vec() })
}

fn enumerate(seam: &mut dyn HostSeam, registry: &Registry) -> io::Result<Vec<EmployeeAccount>> {
    let users = parse_passwd(&success(seam, "/usr/bin/getent", &["passwd"])?)?;
    let groups = parse_group(&success(seam, "/usr/bin/getent", &["group"])?)?;
    if users.is_empty() || groups.is_empty() { return Err(denied()); }
    verify_enumerated(registry, &users, &groups)?;
    // A retired/partially-created account must not regain a login entry
    // merely because its permanent registry row is a tombstone: every
    // passwd row in the range is verified, whatever the row's state.
    users.iter().filter(|user| user.uid >= registry.range.first && user.uid <= registry.range.last)
        .map(|user| registry.employees.iter().find(|entry| entry.uid == user.uid).cloned().ok_or_else(denied))
        .collect()
}
/// `verify_namespace` before the per-account and userdb checks: `nss_modules`,
/// then `enumerate` (complete getent enumeration, no alias). Returns every
/// account with a passwd row in the employee range.
pub(crate) fn enumerated_accounts(seam: &mut dyn HostSeam, registry: &Registry) -> io::Result<Vec<EmployeeAccount>> {
    seam.text(Path::new("/etc/nsswitch.conf")).and_then(|text| nss_supported(&text)).map_err(at_step(Step::NssModules))?;
    enumerate(seam, registry).map_err(at_step(Step::Enumerate))
}

/// `uid_available`: `candidate`.
pub(crate) fn uid_free(seam: &mut dyn HostSeam, uid: u32) -> io::Result<bool> {
    let id = uid.to_string();
    let name = account_name(uid);
    for database in ["passwd", "group"] {
        for key in [&id, &name] {
            match seam.run("/usr/bin/getent", &[database, key]).map_err(at_step(Step::Candidate))? {
                (0, _) => return Ok(false),
                (2, text) if text.is_empty() => {},
                _ => return Err(at_step(Step::Candidate)(denied())),
            }
        }
    }
    Ok(true)
}

fn passwd_row(seam: &mut dyn HostSeam, account: &EmployeeAccount, name: &str) -> io::Result<()> {
    let users = parse_passwd(&success(seam, "/usr/bin/getent", &["passwd", name])?)?;
    if users.len() != 1 || users[0].uid != account.uid || users[0].gid != account.gid
        || users[0].home != "/nonexistent" || users[0].shell != "/usr/sbin/nologin" { return Err(denied()); }
    Ok(())
}
fn groups(seam: &mut dyn HostSeam, account: &EmployeeAccount, name: &str) -> io::Result<()> {
    let groups = success(seam, "/usr/bin/id", &["--groups", name])?;
    let actual: Vec<u32> = groups.split_whitespace().map(|value| value.parse().map_err(|_| denied())).collect::<io::Result<_>>()?;
    if actual != [account.gid] { return Err(denied()); }
    Ok(())
}
/// `verify_locked` before SSH, for one account: `passwd_row`, `shadow_row`,
/// `groups`, the scheduler barriers ([`verify_schedulers`]), then `linger`
/// and `sessions` ([`verify_sessions`]). Every error names the account.
pub(crate) fn verify_account(seam: &mut dyn HostSeam, account: &EmployeeAccount) -> io::Result<()> {
    let name = account_name(account.uid);
    (|| -> io::Result<()> {
        passwd_row(seam, account, &name).map_err(at_step(Step::PasswdRow))?;
        success(seam, "/usr/bin/getent", &["shadow", &name]).and_then(|text| verify_shadow(&text, &name))
            .map_err(at_step(Step::ShadowRow))?;
        groups(seam, account, &name).map_err(at_step(Step::Groups))?;
        verify_schedulers(seam, &name, account.uid)?;
        verify_sessions(seam, account)
    })().map_err(for_account(account.uid))
}

fn scheduler_acl(seam: &mut dyn HostSeam, family: &str, name: &str) -> io::Result<()> {
    let allow = seam.optional_text(Path::new(&format!("/etc/{family}.allow")))?;
    let deny = seam.text(Path::new(&format!("/etc/{family}.deny")))?;
    if !scheduler_denied(name, allow.as_deref(), &deny) { return Err(denied()); }
    Ok(())
}
fn cron_spool(seam: &mut dyn HostSeam, name: &str) -> io::Result<()> {
    for path in [format!("/var/spool/cron/crontabs/{name}"), format!("/var/spool/cron/{name}")] {
        if seam.present(Path::new(&path))? { return Err(denied()); }
    }
    Ok(())
}
fn cron_tables(seam: &mut dyn HostSeam, name: &str) -> io::Result<()> {
    let mut files = vec![PathBuf::from("/etc/crontab")];
    files.extend(seam.cron_d()?);
    for file in files {
        let Some(text) = seam.optional_text(&file)? else { continue };
        for line in text.lines() {
            let words: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
            let owner = if words.first().is_some_and(|word| word.as_bytes().first() == Some(&b'@')) {
                words.get(1)
            } else { words.get(5) };
            if owner.is_some_and(|owner| *owner == name) { return Err(denied()); }
        }
    }
    Ok(())
}
/// cron and at: `cron_acl`, `at_acl` (allow files override deny files),
/// `cron_spool` (no user crontab), `cron_tables` (no system crontab line
/// runs as the account), `at_spool` (no at job, by the spool's owners; `at`
/// itself is never executed).
pub(crate) fn verify_schedulers(seam: &mut dyn HostSeam, name: &str, uid: u32) -> io::Result<()> {
    for (family, step) in [("cron", Step::CronAcl), ("at", Step::AtAcl)] {
        scheduler_acl(seam, family, name).map_err(at_step(step))?;
    }
    cron_spool(seam, name).map_err(at_step(Step::CronSpool))?;
    cron_tables(seam, name).map_err(at_step(Step::CronTables))?;
    seam.at_spool(uid).map_err(at_step(Step::AtSpool))
}

fn sessions(seam: &mut dyn HostSeam, account: &EmployeeAccount) -> io::Result<()> {
    let unit = format!("user@{}.service", account.uid);
    let state = success(seam, "/usr/bin/systemctl", &["show", "--property=ActiveState", "--value", &unit])?;
    if !matches!(state.trim(), "inactive" | "failed") { return Err(denied()); }
    let uid = account.uid.to_string();
    if success(seam, "/usr/bin/loginctl", &["list-sessions", "--no-legend", "--no-pager"])?.lines()
        .any(|line| line.split_whitespace().nth(1) == Some(uid.as_str())) { return Err(denied()); }
    Ok(())
}
/// `linger` (no linger file), `sessions` (user manager inactive, no logind
/// session of the uid).
pub(crate) fn verify_sessions(seam: &mut dyn HostSeam, account: &EmployeeAccount) -> io::Result<()> {
    let linger = format!("/var/lib/systemd/linger/{}", account_name(account.uid));
    if seam.present(Path::new(&linger)).map_err(at_step(Step::Linger))? { return Err(at_step(Step::Linger)(denied())); }
    sessions(seam, account).map_err(at_step(Step::Sessions))
}

/// Whether `/usr/sbin/sshd` is installed (no sshd: the SSH checks do not
/// apply); unreadable is `ssh_config`.
pub(crate) fn ssh_present(seam: &mut dyn HostSeam) -> io::Result<bool> {
    seam.present(Path::new("/usr/sbin/sshd")).map_err(at_step(Step::SshConfig))
}
fn effective(seam: &mut dyn HostSeam, name: &str) -> io::Result<(String, u16)> {
    success(seam, "/usr/sbin/sshd", &["-t"])?;
    let context = format!("user={name},host=localhost,addr=127.0.0.1");
    let effective = success(seam, "/usr/sbin/sshd", &["-T", "-C", &context])?;
    let facts = duduclaw_isolation::ssh_config::SshdEffective::parse(&effective);
    if !facts.denies_employees() { return Err(denied()); }
    let port = facts.port.ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, SSH_PORT_UNKNOWN))?;
    Ok((effective, port))
}
/// `ssh_config`: `sshd -t`, then the account's effective configuration must
/// deny it ([`ssh_effective_denied`]'s parser) and name a first `port`
/// ([`SSH_PORT_UNKNOWN`] otherwise). Returns that output and the port (the
/// probe key's inputs; the output is never logged).
pub(crate) fn ssh_effective(seam: &mut dyn HostSeam, name: &str) -> io::Result<(String, u16)> {
    effective(seam, name).map_err(at_step(Step::SshConfig))
}
/// `ssh_probe`: the probe's refusal, still found by [`ssh_refusal`].
pub(crate) fn probe_refused(refusal: SshRefusal) -> io::Error { at_step(Step::SshProbe)(refusal.error()) }

/// `create_locked` after the identity and login-barrier steps:
/// `create_group` (groupadd), `create_user` (useradd, then no mail spool),
/// `lock_user` (chage, disable-linger).
pub(crate) fn create_account(seam: &mut dyn HostSeam, account: &EmployeeAccount) -> io::Result<()> {
    let uid = account.uid.to_string();
    let name = account_name(account.uid);
    success(seam, "/usr/sbin/groupadd", &["--gid", &uid, &name]).map_err(at_step(Step::CreateGroup))?;
    success(seam, "/usr/sbin/useradd", &useradd_arguments(&uid, &name)).and_then(|_| seam.mail_spool(&name))
        .map_err(at_step(Step::CreateUser))?;
    success(seam, "/usr/bin/chage", &["-E", "1", &name])
        .and_then(|_| success(seam, "/usr/bin/loginctl", &["disable-linger", &name]))
        .map_err(at_step(Step::LockUser))?;
    Ok(())
}

fn lock_steps(seam: &mut dyn HostSeam, account: &EmployeeAccount) -> io::Result<()> {
    let name = account_name(account.uid);
    success(seam, "/usr/sbin/usermod", &["--password", "!*", "--expiredate", "1970-01-02",
        "--shell", "/usr/sbin/nologin", "--home", "/nonexistent", "--groups", "", &name])?;
    success(seam, "/usr/bin/chage", &["-E", "1", &name])?;
    success(seam, "/usr/bin/loginctl", &["disable-linger", &name])?;
    let uid = account.uid.to_string();
    if success(seam, "/usr/bin/loginctl", &["list-sessions", "--no-legend", "--no-pager"])?.lines()
        .any(|line| line.split_whitespace().nth(1) == Some(uid.as_str())) {
        success(seam, "/usr/bin/loginctl", &["terminate-user", &uid])?;
    }
    success(seam, "/usr/bin/systemctl", &["stop", &format!("user@{}.service", account.uid)])?;
    Ok(())
}
/// `recover_locked`'s lockdown of an existing user (before its login
/// barriers): `lock_user`.
pub(crate) fn lock_existing(seam: &mut dyn HostSeam, account: &EmployeeAccount) -> io::Result<()> {
    lock_steps(seam, account).map_err(at_step(Step::LockUser))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    const NAME: &str = "duduclaw-emp-70000";
    const EFFECTIVE: &str = "port 22\npermittty no\ndisableforwarding yes\ndenyusers duduclaw-emp-*\n";

    /// A host whose files, presence and command replies are scripted; every
    /// command and at-spool check is recorded.
    #[derive(Default)]
    struct Fake {
        files: BTreeMap<PathBuf, String>,
        present: BTreeSet<PathBuf>,
        replies: BTreeMap<String, (i32, String)>,
        cron_d: Vec<PathBuf>,
        at_job: bool,
        mail: bool,
        runs: Vec<String>,
        spool_checks: Vec<u32>,
    }
    impl HostSeam for Fake {
        fn run(&mut self, path: &'static str, args: &[&str]) -> io::Result<(i32, String)> {
            let key = std::iter::once(path).chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
            self.runs.push(key.clone());
            self.replies.get(&key).cloned().ok_or_else(denied)
        }
        fn text(&mut self, path: &Path) -> io::Result<String> {
            self.files.get(path).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
        }
        fn optional_text(&mut self, path: &Path) -> io::Result<Option<String>> { Ok(self.files.get(path).cloned()) }
        fn present(&mut self, path: &Path) -> io::Result<bool> { Ok(self.files.contains_key(path) || self.present.contains(path)) }
        fn cron_d(&mut self) -> io::Result<Vec<PathBuf>> { Ok(self.cron_d.clone()) }
        fn at_spool(&mut self, uid: u32) -> io::Result<()> {
            self.spool_checks.push(uid);
            if self.at_job { Err(denied()) } else { Ok(()) }
        }
        fn mail_spool(&mut self, _: &str) -> io::Result<()> { if self.mail { Err(denied()) } else { Ok(()) } }
    }
    impl Fake {
        fn file(&mut self, path: &str, text: &str) { self.files.insert(PathBuf::from(path), text.into()); }
        fn reply(&mut self, command: &str, status: i32, text: &str) { self.replies.insert(command.into(), (status, text.into())); }
        /// The Ubuntu 24.04 host of the R8w report, account 70000 created and locked.
        fn healthy() -> Self {
            let mut host = Self::default();
            host.file("/etc/login.defs", "UID_MIN 1000\nUID_MAX 60000\nGID_MIN 1000\nGID_MAX 60000\n");
            host.file("/etc/subuid", "ubuntu:100000:65536\n");
            host.file("/etc/subgid", "ubuntu:100000:65536\n");
            host.file("/etc/nsswitch.conf", "passwd: files systemd\ngroup: files systemd\n");
            host.file("/etc/cron.deny", &format!("{NAME}\n"));
            host.file("/etc/at.deny", &format!("alias\n{NAME}\n"));
            host.file("/etc/crontab", "SHELL=/bin/sh\n17 * * * * root cd / && run-parts --report /etc/cron.hourly\n");
            host.file("/etc/cron.d/sysstat", "5-55/10 * * * * root command -v debian-sa1 > /dev/null && debian-sa1 1 1\n");
            host.file("/etc/cron.d/.placeholder", "# DO NOT EDIT OR REMOVE\n");
            host.cron_d = vec![PathBuf::from("/etc/cron.d/sysstat"), PathBuf::from("/etc/cron.d/.placeholder")];
            host.present.insert(PathBuf::from("/usr/sbin/sshd"));
            host.reply("/usr/bin/getent passwd", 0, &format!("root:x:0:0:root:/root:/bin/bash\n{NAME}:x:70000:70000::/nonexistent:/usr/sbin/nologin\n"));
            host.reply("/usr/bin/getent group", 0, &format!("root:x:0:\n{NAME}:x:70000:\n"));
            host.reply(&format!("/usr/bin/getent passwd {NAME}"), 0, &format!("{NAME}:x:70000:70000::/nonexistent:/usr/sbin/nologin\n"));
            host.reply(&format!("/usr/bin/getent shadow {NAME}"), 0, &format!("{NAME}:!*:20000:0:99999:7::1:\n"));
            host.reply(&format!("/usr/bin/id --groups {NAME}"), 0, "70000\n");
            host.reply("/usr/bin/systemctl show --property=ActiveState --value user@70000.service", 0, "inactive\n");
            host.reply("/usr/bin/loginctl list-sessions --no-legend --no-pager", 0, "");
            host.reply("/usr/sbin/sshd -t", 0, "");
            host.reply(&format!("/usr/sbin/sshd -T -C user={NAME},host=localhost,addr=127.0.0.1"), 0, EFFECTIVE);
            for (database, key) in [("passwd", "70001"), ("passwd", "duduclaw-emp-70001"), ("group", "70001"), ("group", "duduclaw-emp-70001")] {
                host.reply(&format!("/usr/bin/getent {database} {key}"), 2, "");
            }
            host.reply("/usr/sbin/groupadd --gid 70000 duduclaw-emp-70000", 0, "");
            host.reply(&format!("/usr/sbin/useradd {}", useradd_arguments("70000", NAME).join(" ")), 0, "");
            host.reply(&format!("/usr/bin/chage -E 1 {NAME}"), 0, "");
            host.reply(&format!("/usr/bin/loginctl disable-linger {NAME}"), 0, "");
            host
        }
    }
    fn account() -> EmployeeAccount {
        EmployeeAccount { employee: duduclaw_isolation::ids::EmployeeId([1; 16]), uid: 70000, gid: 70000,
            state: crate::registry::AccountState::Tombstone }
    }
    fn registry() -> Registry { Registry { employees: vec![account()], ..Registry::default() } }

    /// The healthy host passes every check (a tombstoned row included).
    #[test]
    fn healthy_host_passes_every_labelled_check() {
        let mut host = Fake::healthy();
        host_policy(&mut host).unwrap();
        assert_eq!(enumerated_accounts(&mut host, &registry()).unwrap(), vec![account()]);
        verify_account(&mut host, &account()).unwrap();
        assert!(ssh_present(&mut host).unwrap());
        assert_eq!(ssh_effective(&mut host, NAME).unwrap(), (EFFECTIVE.to_string(), 22));
        assert!(uid_free(&mut host, 70001).unwrap());
        create_account(&mut host, &account()).unwrap();
    }

    /// R8w item 1: the scheduler check never executes `/usr/bin/at` (nor
    /// anything else); the at jobs are the spool rule's, for the account's uid.
    #[test]
    fn verify_schedulers_never_executes_at() {
        let mut host = Fake::healthy();
        host.present.insert(PathBuf::from("/usr/bin/at"));
        verify_schedulers(&mut host, NAME, 70000).unwrap();
        assert!(host.runs.is_empty(), "{:?}", host.runs);
        assert_eq!(host.spool_checks, [70000]);
        verify_account(&mut host, &account()).unwrap();
        assert!(host.runs.iter().all(|run| !run.starts_with("/usr/bin/at")), "{:?}", host.runs);
        assert_eq!(host.spool_checks, [70000, 70000]);
        // A job in the spool fails with its own label.
        host.at_job = true;
        assert_eq!(step_of(&verify_schedulers(&mut host, NAME, 70000).unwrap_err()), Some(Step::AtSpool));
    }

    /// R8w item 2: every failing branch carries its fixed label; account
    /// checks name the account; the error kind and text are unchanged.
    #[test]
    fn each_failing_branch_yields_its_label() {
        type Break = fn(&mut Fake);
        let account_rows: [(Step, Break); 12] = [
            (Step::PasswdRow, |host| host.reply(&format!("/usr/bin/getent passwd {NAME}"), 0, &format!("{NAME}:x:70000:70000::/nonexistent:/bin/sh\n"))),
            (Step::PasswdRow, |host| { host.replies.remove(&format!("/usr/bin/getent passwd {NAME}")); }),
            (Step::ShadowRow, |host| host.reply(&format!("/usr/bin/getent shadow {NAME}"), 0, &format!("{NAME}:!:20000:0:99999:7::1:\n"))),
            (Step::Groups, |host| host.reply(&format!("/usr/bin/id --groups {NAME}"), 0, "70000 27\n")),
            (Step::CronAcl, |host| host.file("/etc/cron.deny", "someone\n")),
            (Step::AtAcl, |host| { host.files.remove(Path::new("/etc/at.deny")); }),
            (Step::CronSpool, |host| { host.present.insert(PathBuf::from(format!("/var/spool/cron/crontabs/{NAME}"))); }),
            (Step::CronTables, |host| host.file("/etc/cron.d/sysstat", &format!("* * * * * {NAME} /bin/true\n"))),
            (Step::AtSpool, |host| host.at_job = true),
            (Step::Linger, |host| { host.present.insert(PathBuf::from(format!("/var/lib/systemd/linger/{NAME}"))); }),
            (Step::Sessions, |host| host.reply("/usr/bin/systemctl show --property=ActiveState --value user@70000.service", 0, "active\n")),
            (Step::Sessions, |host| host.reply("/usr/bin/loginctl list-sessions --no-legend --no-pager", 0, "3 70000 duduclaw-emp-70000 seat0\n")),
        ];
        for (index, (step, broken)) in account_rows.into_iter().enumerate() {
            let mut host = Fake::healthy();
            broken(&mut host);
            let error = verify_account(&mut host, &account()).unwrap_err();
            assert_eq!((step_of(&error), step_account(&error)), (Some(step), Some(70000)), "row {index}: {error}");
            assert!(matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound), "row {index}");
        }
        let check = |step: Step, broken: Break, run: fn(&mut Fake) -> io::Result<()>| {
            let mut host = Fake::healthy();
            broken(&mut host);
            let error = run(&mut host).unwrap_err();
            assert_eq!(step_of(&error), Some(step), "{error}");
            error
        };
        check(Step::LoginDefs, |host| host.file("/etc/login.defs", "UID_MIN invalid\n"), |host| host_policy(host).map(|_| ()));
        check(Step::Subids, |host| host.file("/etc/subgid", "a:1:0\n"), |host| host_policy(host).map(|_| ()));
        let nss = check(Step::NssModules, |host| host.file("/etc/nsswitch.conf", "passwd: files sss\ngroup: files sss\n"),
            |host| enumerated_accounts(host, &registry()).map(|_| ()));
        assert_eq!(nss.to_string(), EXTERNAL_NSS_REFUSED, "the reason text is unchanged");
        check(Step::Enumerate, |host| host.reply("/usr/bin/getent passwd", 0,
            &format!("{NAME}:x:70000:70000::/nonexistent:/usr/sbin/nologin\nalias:x:70000:70000::/:/bin/sh\n")),
            |host| enumerated_accounts(host, &registry()).map(|_| ()));
        check(Step::Enumerate, |host| host.reply("/usr/bin/getent group", 2, ""), |host| enumerated_accounts(host, &registry()).map(|_| ()));
        check(Step::Candidate, |host| host.reply("/usr/bin/getent passwd 70001", 1, ""), |host| uid_free(host, 70001).map(|_| ()));
        check(Step::SshConfig, |host| host.reply("/usr/sbin/sshd -t", 255, ""), |host| ssh_effective(host, NAME).map(|_| ()));
        check(Step::SshConfig, |host| host.reply(&format!("/usr/sbin/sshd -T -C user={NAME},host=localhost,addr=127.0.0.1"), 0,
            "port 22\npermittty no\ndisableforwarding yes\n"), |host| ssh_effective(host, NAME).map(|_| ()));
        let port = check(Step::SshConfig, |host| host.reply(&format!("/usr/sbin/sshd -T -C user={NAME},host=localhost,addr=127.0.0.1"), 0,
            "permittty no\ndisableforwarding yes\ndenyusers duduclaw-emp-*\n"), |host| ssh_effective(host, NAME).map(|_| ()));
        assert_eq!(port.to_string(), SSH_PORT_UNKNOWN);
        check(Step::CreateGroup, |host| host.reply("/usr/sbin/groupadd --gid 70000 duduclaw-emp-70000", 9, ""), |host| create_account(host, &account()));
        check(Step::CreateUser, |host| { host.replies.retain(|key, _| !key.starts_with("/usr/sbin/useradd")); }, |host| create_account(host, &account()));
        check(Step::CreateUser, |host| host.mail = true, |host| create_account(host, &account()));
        check(Step::LockUser, |host| host.reply(&format!("/usr/bin/chage -E 1 {NAME}"), 1, ""), |host| create_account(host, &account()));
        check(Step::LockUser, |_| {}, |host| lock_existing(host, &account()));
        // The probe's refusal: labelled, same kind, still the typed refusal.
        let pending = probe_refused(SshRefusal::Pending);
        assert_eq!((step_of(&pending), pending.kind()), (Some(Step::SshProbe), io::ErrorKind::WouldBlock));
        assert_eq!(ssh_refusal(&pending), Some(&SshRefusal::Pending));
        assert!(probe_unresolved(&pending));
        let reachable = for_account(70000)(probe_refused(SshRefusal::Reachable("login_not_denied (exit 10)".into())));
        assert!(matches!(ssh_refusal(&reachable), Some(SshRefusal::Reachable(_))) && !probe_unresolved(&reachable));
        assert_eq!(step_account(&reachable), Some(70000));
        assert!(reachable.to_string().contains("NOT denied"), "{reachable}");
    }
}
