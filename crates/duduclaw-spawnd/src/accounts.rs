//! Host account lockdown and complete namespace checks (§5.1, R7-M4/M5).
use std::{collections::BTreeMap, io};
use duduclaw_isolation::ids::UidRange;
use crate::registry::{account_name, Registry};
use crate::registry::EmployeeAccount;
#[cfg(target_os = "linux")]
use crate::registry::HostUidPolicy;

fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "employee account verification unavailable") }

/// R8w: the fixed step at which an account verification or an allocation
/// failed. A closed set of identifiers, never input-derived and never file
/// contents; carried inside the `io::Error` ([`at_step`], [`step_of`]) with
/// the account it concerns ([`for_account`]). The error's kind and text are
/// unchanged, so wire codes, readiness.json and the CLI readiness text stay
/// as they were; only spawnd's journal names the step ([`verification_line`],
/// [`allocation_line`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `/etc/login.defs` unreadable or malformed.
    LoginDefs,
    /// `/etc/subuid` or `/etc/subgid` unreadable or malformed.
    Subids,
    /// The registry rows against the host uid policy (version, size,
    /// overlaps, duplicates), or a `Reserved` row no allocation can resume.
    RegistryRows,
    /// `nsswitch.conf`: passwd/group name a module other than files/systemd.
    NssModules,
    /// getent passwd/group enumeration: unreadable, alias, foreign row in range.
    Enumerate,
    /// The account's passwd row (uid, gid, home, shell).
    PasswdRow,
    /// The account's shadow row (`!*`, expired).
    ShadowRow,
    /// The account's group list (its own gid only).
    Groups,
    /// `/etc/cron.allow`/`/etc/cron.deny` do not deny the account.
    CronAcl,
    /// A user crontab of the account exists.
    CronSpool,
    /// `/etc/crontab` or `/etc/cron.d` runs a line as the account (or is unreadable).
    CronTables,
    /// `/etc/at.allow`/`/etc/at.deny` do not deny the account.
    AtAcl,
    /// The at spool holds an entry of the account, or cannot be read.
    AtSpool,
    /// A systemd linger file exists for the account.
    Linger,
    /// The account's user manager or a logind session of it is active.
    Sessions,
    /// `sshd -t` / `sshd -T` do not prove the account denied (or no port).
    SshConfig,
    /// The per-account SSH denial probe has no Denied result.
    SshProbe,
    /// `/run/systemd/userdb`: a service socket unreadable or incomplete.
    UserdbService,
    /// A userdb user record touches the employee namespace.
    UserdbUser,
    /// A userdb group record touches the employee namespace.
    UserdbGroup,
    /// The pre-creation identity check (an alias or a foreign row).
    LocalIdentity,
    /// A candidate uid's getent lookup failed.
    Candidate,
    /// spawnd's file-owner inventory of its state directories failed.
    OwnerInventory,
    /// Installing cron/at deny entries or the sshd drop-in failed.
    LoginBarriers,
    /// groupadd failed.
    CreateGroup,
    /// useradd failed, or it left a mail spool.
    CreateUser,
    /// usermod/chage/loginctl/systemctl lockdown failed.
    LockUser,
    /// Terminating the account's processes, or the no-process readback, failed.
    Quarantine,
    /// Allocation: the reservation itself (e.g. the range is exhausted).
    Reserve,
    /// Allocation: account creation, no finer step known.
    CreateAccount,
    /// Allocation: the final readback, no finer step known.
    Readback,
    /// No step was recorded (never produced by spawnd's own checks).
    Unlabelled,
}
impl Step {
    pub const ALL: [Step; 32] = [Step::LoginDefs, Step::Subids, Step::RegistryRows, Step::NssModules, Step::Enumerate,
        Step::PasswdRow, Step::ShadowRow, Step::Groups, Step::CronAcl, Step::CronSpool, Step::CronTables, Step::AtAcl,
        Step::AtSpool, Step::Linger, Step::Sessions, Step::SshConfig, Step::SshProbe, Step::UserdbService, Step::UserdbUser,
        Step::UserdbGroup, Step::LocalIdentity, Step::Candidate, Step::OwnerInventory, Step::LoginBarriers, Step::CreateGroup,
        Step::CreateUser, Step::LockUser, Step::Quarantine, Step::Reserve, Step::CreateAccount, Step::Readback, Step::Unlabelled];
    pub const fn name(self) -> &'static str {
        match self {
            Step::LoginDefs => "login_defs", Step::Subids => "subids", Step::RegistryRows => "registry_rows",
            Step::NssModules => "nss_modules", Step::Enumerate => "enumerate", Step::PasswdRow => "passwd_row",
            Step::ShadowRow => "shadow_row", Step::Groups => "groups", Step::CronAcl => "cron_acl",
            Step::CronSpool => "cron_spool", Step::CronTables => "cron_tables", Step::AtAcl => "at_acl",
            Step::AtSpool => "at_spool", Step::Linger => "linger", Step::Sessions => "sessions",
            Step::SshConfig => "ssh_config", Step::SshProbe => "ssh_probe", Step::UserdbService => "userdb_service",
            Step::UserdbUser => "userdb_user", Step::UserdbGroup => "userdb_group", Step::LocalIdentity => "local_identity",
            Step::Candidate => "candidate", Step::OwnerInventory => "owner_inventory", Step::LoginBarriers => "login_barriers",
            Step::CreateGroup => "create_group", Step::CreateUser => "create_user", Step::LockUser => "lock_user",
            Step::Quarantine => "quarantine", Step::Reserve => "reserve", Step::CreateAccount => "create_account",
            Step::Readback => "readback", Step::Unlabelled => "unlabelled",
        }
    }
}
/// The labelled error: displays exactly as the error it carries.
#[derive(Debug)]
struct Stepped { step: Step, uid: Option<u32>, error: io::Error }
impl std::fmt::Display for Stepped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { std::fmt::Display::fmt(&self.error, formatter) }
}
impl std::error::Error for Stepped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.error) }
}
fn stepped(error: &io::Error) -> Option<&Stepped> { error.get_ref().and_then(|inner| inner.downcast_ref::<Stepped>()) }
/// Labels an error with `step`, same kind and text. An error that already
/// carries a step keeps it: the innermost (most specific) step wins.
pub fn at_step(step: Step) -> impl Fn(io::Error) -> io::Error {
    move |error| if stepped(&error).is_some() { error } else {
        let kind = error.kind();
        io::Error::new(kind, Stepped { step, uid: None, error })
    }
}
/// Records the account (`uid`, spawnd's own registry value) on a labelled
/// error that names none yet; an unlabelled error is returned unchanged.
pub fn for_account(uid: u32) -> impl Fn(io::Error) -> io::Error {
    move |mut error| {
        if let Some(stepped) = error.get_mut().and_then(|inner| inner.downcast_mut::<Stepped>()) {
            if stepped.uid.is_none() { stepped.uid = Some(uid); }
        }
        error
    }
}
/// The step `error` carries, if any.
pub fn step_of(error: &io::Error) -> Option<Step> { stepped(error).map(|stepped| stepped.step) }
/// The account uid `error` names, if any.
pub fn step_account(error: &io::Error) -> Option<u32> { stepped(error).and_then(|stepped| stepped.uid) }
/// The error under the step label (itself when unlabelled).
pub fn unstepped(error: &io::Error) -> &io::Error { stepped(error).map_or(error, |stepped| &stepped.error) }
/// spawnd's journal line for a failed account verification (after the
/// `duduclaw-spawnd: ` prefix): `employee account verification failed:
/// step=<label> account=<duduclaw-emp-NNNNN|->`. Fixed identifiers only.
pub fn verification_line(error: &io::Error) -> String {
    format!("employee account verification failed: step={} account={}", step_of(error).unwrap_or(Step::Unlabelled).name(),
        step_account(error).map_or_else(|| "-".to_string(), account_name))
}
/// spawnd's journal line for a failed Allocate: `allocation failed: step=<label>`.
pub fn allocation_line(error: &io::Error) -> String {
    format!("allocation failed: step={}", step_of(error).unwrap_or(Step::Unlabelled).name())
}

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
    let get = |key, default| values.get(key).copied().unwrap_or(default);
    let uid_min = get("UID_MIN", 1000);
    let gid_min = get("GID_MIN", 1000);
    let uid_max = match values.get("SYS_UID_MAX") {
        Some(value) => *value,
        None => uid_min.checked_sub(1).ok_or_else(denied)?,
    };
    let gid_max = match values.get("SYS_GID_MAX") {
        Some(value) => *value,
        None => gid_min.checked_sub(1).ok_or_else(denied)?,
    };
    // This range excludes employee IDs; it does not allocate system accounts.
    // The compiled shadow minimum varies (100/101), so conservatively reserve
    // every positive ID below the maximum when the minimum is absent.
    let system = UidRange { first: get("SYS_UID_MIN", 1).min(get("SYS_GID_MIN", 1)), last: uid_max.max(gid_max) };
    let login = UidRange { first: uid_min.min(gid_min), last: get("UID_MAX", 60000).max(get("GID_MAX", 60000)) };
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

/// `sshd -T -C …` output denies the employee account: the shared parser
/// ([`duduclaw_isolation::ssh_config::SshdEffective`], also the probe's).
pub fn ssh_effective_denied(text: &str) -> bool {
    duduclaw_isolation::ssh_config::SshdEffective::parse(text).denies_employees()
}

/// NSS modules outside files/systemd need an explicit, permanent IAM reservation.
/// `Ok(true)`: the passwd or group line names another module (any token but
/// `files`/`systemd`, bracketed action clauses included); `Err`: not exactly
/// one passwd and one group line.
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

/// R7c: why a host whose passwd/group NSS configuration names a module other
/// than `files`/`systemd` is refused. Such a module (LDAP/SSSD, Google
/// Compute Engine OS Login, …) can introduce identities in the employee uid
/// range; only an explicit, permanent IAM reservation of that range could make
/// it safe, and v1.83 has no producer of that reservation (it used to be read
/// from the installer receipt), so the host is unsupported.
pub const EXTERNAL_NSS_REFUSED: &str = "passwd/group NSS modules other than files and systemd are not supported \
    (no permanent IAM reservation of the employee uid range exists for them)";
/// `Ok` only when `nsswitch.conf` names nothing but `files`/`systemd` for
/// passwd and group ([`external_nss`]); otherwise refused with [`EXTERNAL_NSS_REFUSED`].
pub fn nss_supported(text: &str) -> io::Result<()> {
    if external_nss(text)? { return Err(io::Error::new(io::ErrorKind::PermissionDenied, EXTERNAL_NSS_REFUSED)); }
    Ok(())
}

/// R7c: why an account's SSH denial is not proven, carried inside the
/// `io::Error` of `verify_locked` / `login_denied` ([`ssh_refusal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshRefusal {
    /// No current probe result for the account's key (none yet, expired, or
    /// under another key); the probe was requested (kind `WouldBlock`,
    /// distinct from every other refusal).
    Pending,
    /// The probe ran and proved nothing (failed stage, a client success
    /// without sshd's Accepted record, timeout, PID 1 refusal).
    Inconclusive(String),
    /// The probe proved the public-key login or the forwarding session NOT
    /// denied, with sshd's own Accepted record.
    Reachable(String),
}
impl std::fmt::Display for SshRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(formatter, "SSH denial probe pending"),
            Self::Inconclusive(reason) => write!(formatter, "SSH denial probe inconclusive: {reason}"),
            Self::Reachable(stage) => write!(formatter, "SSH denial probe: account NOT denied ({stage})"),
        }
    }
}
impl std::error::Error for SshRefusal {}
impl SshRefusal {
    pub fn error(self) -> io::Error {
        let kind = if self == Self::Pending { io::ErrorKind::WouldBlock } else { io::ErrorKind::PermissionDenied };
        io::Error::new(kind, self)
    }
}
/// The [`SshRefusal`] inside `error` (under a step label too), if any.
pub fn ssh_refusal(error: &io::Error) -> Option<&SshRefusal> {
    unstepped(error).get_ref().and_then(|inner| inner.downcast_ref::<SshRefusal>())
}
/// The account's SSH denial is neither proven nor disproven (pending,
/// inconclusive): an allocation keeps the account resumable instead of
/// tombstoning it. `Reachable` and every other error are not unresolved.
pub fn probe_unresolved(error: &io::Error) -> bool {
    matches!(ssh_refusal(error), Some(SshRefusal::Pending | SshRefusal::Inconclusive(_)))
}
/// R7c: `/usr/sbin/sshd` exists but its effective configuration for the
/// account names no usable first `port` line, so the probe has no port to
/// test: the account's SSH denial cannot be proven.
pub const SSH_PORT_UNKNOWN: &str = "sshd -T names no usable port (first `port` line missing or malformed); \
    the SSH denial probe cannot run";

/// R7c: the `useradd` argument list of `create_locked`. No
/// `--key CREATE_MAIL_SPOOL=no`: shadow 4.13 (Ubuntu 24.04) refuses that key
/// ("unknown item", exit 3); a mail spool useradd creates anyway is refused
/// afterwards ([`mail_spool_absent`]). The SUB_*_COUNT keys stay (no
/// /etc/subuid or /etc/subgid rows).
pub fn useradd_arguments<'a>(uid: &'a str, name: &'a str) -> Vec<&'a str> {
    vec!["--uid", uid, "--gid", uid, "--no-user-group", "--no-create-home", "--no-log-init",
        "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0",
        "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin", "--password", "!*", "--expiredate", "1970-01-02", name]
}
/// R7c: after useradd, neither `<root>/var/mail/<name>` nor
/// `<root>/var/spool/mail/<name>` may exist (lstat: a symlink counts). Such a
/// spool would be an employee-owned file outside the isolated tree; spawnd
/// never deletes files outside its own state, so the account is refused and
/// rolled back like any other post-create verification failure. An unreadable
/// path refuses too.
pub fn mail_spool_absent(root: &std::path::Path, name: &str) -> io::Result<()> {
    for directory in ["var/mail", "var/spool/mail"] {
        let path = root.join(directory).join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(io::Error::new(io::ErrorKind::PermissionDenied,
                format!("useradd created a mail spool {} for the employee account", path.display()))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(io::Error::new(io::ErrorKind::PermissionDenied,
                format!("mail spool path {} unreadable", path.display()))),
        }
    }
    Ok(())
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

// R7c: there is no installer receipt any more. SSH denial is proven per
// account by the probe unit run through PID 1 (`crate::ssh_evidence`), never
// inferred from a successful `sshd -T` alone; external NSS is refused
// ([`nss_supported`]). R7b: file ownership is spawnd's own scan of its state
// directories at each allocation (`AccountHost::occupied_ids`).

#[cfg(any(target_os = "linux", test))]
#[path = "accounts_userdb.rs"]
mod userdb;
/// R8w: the labelled account checks over a host seam.
#[cfg(any(target_os = "linux", test))]
#[path = "accounts_checks.rs"]
mod checks;
/// R8w: the at-spool rule (direct inspection, never `at` itself).
#[cfg(target_os = "linux")]
#[path = "accounts_spool.rs"]
pub mod spool;

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

/// `Gid:` row of /proc/<pid>/status (real, effective, saved, filesystem).
pub fn process_gids(status: &str) -> io::Result<[u32; 4]> {
    let rows: Vec<_> = status.lines().filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| *name == "Gid").collect();
    if rows.len() != 1 { return Err(denied()); }
    let values = rows[0].1.split_whitespace().map(|value| value.parse().map_err(|_| denied()))
        .collect::<io::Result<Vec<u32>>>()?;
    values.try_into().map_err(|_| denied())
}

/// H4 / F6 S14: a process belongs to the employee when ANY uid or gid field
/// equals the employee's. Unparseable status is an error, never "not ours".
pub fn employee_owned_status(status: &str, uid: u32, gid: u32) -> io::Result<bool> {
    Ok(process_uids(status)?.contains(&uid) || process_gids(status)?.contains(&gid))
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
    use crate::ssh_evidence::SshEvidence;
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
    /// `root_path` keeps root from executing a binary another account owns
    /// (the binary and every ancestor root-owned, not group/other-writable).
    pub(crate) fn command(path: &str, args: &[&str]) -> io::Result<(i32, String)> {
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

    /// R8w: the production host seam of the labelled checks
    /// (`accounts_checks.rs`). Executables only through [`command`] (fixed
    /// root-owned paths; `/usr/bin/at` is no longer one of them: the at jobs
    /// are read from the spool, `accounts_spool.rs`).
    pub(crate) struct RootSeam;
    impl checks::HostSeam for RootSeam {
        fn run(&mut self, path: &'static str, args: &[&str]) -> io::Result<(i32, String)> { command(path, args) }
        fn text(&mut self, path: &Path) -> io::Result<String> { host_text(path) }
        fn optional_text(&mut self, path: &Path) -> io::Result<Option<String>> { optional_text(path) }
        fn present(&mut self, path: &Path) -> io::Result<bool> {
            match fs::symlink_metadata(path) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        }
        fn cron_d(&mut self) -> io::Result<Vec<std::path::PathBuf>> {
            let directory = Path::new("/etc/cron.d");
            if !checks::HostSeam::present(self, directory)? { return Ok(Vec::new()); }
            root_path(directory, true)?;
            fs::read_dir(directory)?.map(|entry| entry.map(|entry| entry.path())).collect()
        }
        fn at_spool(&mut self, uid: u32) -> io::Result<()> { super::spool::installed_at_spools_clear(uid) }
        fn mail_spool(&mut self, name: &str) -> io::Result<()> { mail_spool_absent(Path::new("/"), name) }
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

    /// Host account authority. `ssh` holds the per-account SSH denial probe
    /// results (`crate::ssh_evidence`); `require_probe` decides whether a
    /// missing or failed result refuses ([`Self::admission`]) or only asks
    /// for a probe ([`Self::background`]).
    pub struct LinuxAccounts<'a> { pub ssh: &'a dyn SshEvidence, pub require_probe: bool }

    impl<'a> LinuxAccounts<'a> {
        /// Readiness refresh, start-up recovery and allocation: they walk every
        /// account, so one account's SSH probe state must not refuse them
        /// (that would make it a host-wide effect). Every host-level check
        /// still applies (`sshd -t`, the effective configuration); a missing or
        /// stale probe result is requested (non-blocking).
        pub fn background(ssh: &'a dyn SshEvidence) -> Self { Self { ssh, require_probe: false } }
        /// One employee's admission (`Core::ready`): the account's probe must
        /// have reported Denied for its current key.
        pub fn admission(ssh: &'a dyn SshEvidence) -> Self { Self { ssh, require_probe: true } }
    }

    impl LinuxAccounts<'_> {
        /// `local_identity` (the inner nss/userdb steps keep their own labels).
        fn partial_local_identity(&self, account: &EmployeeAccount) -> io::Result<(bool, bool)> {
            self.local_identity(account).map_err(at_step(Step::LocalIdentity))
        }
        fn local_identity(&self, account: &EmployeeAccount) -> io::Result<(bool, bool)> {
            let registry = Registry { employees: vec![account.clone()], ..Registry::default() };
            host_text(Path::new("/etc/nsswitch.conf")).and_then(|text| nss_supported(&text)).map_err(at_step(Step::NssModules))?;
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

        /// `lock_user` (`checks::lock_existing`), then `login_barriers`.
        fn lockdown_existing(&self, account: &EmployeeAccount) -> io::Result<()> {
            checks::lock_existing(&mut RootSeam, account)?;
            self.install_login_barriers(&account_name(account.uid)).map_err(at_step(Step::LoginBarriers))
        }

        /// `userdb_user`/`userdb_group` per record (`accounts_userdb.rs`);
        /// everything else (the directory, a socket, the local files) is
        /// `userdb_service`.
        fn userdb_identities(&self, ranges: &[UidRange], employees: &[(u32, u32)]) -> io::Result<Vec<Vec<(u32, String)>>> {
            self.userdb_records(ranges, employees).map_err(at_step(Step::UserdbService))
        }
        fn userdb_records(&self, ranges: &[UidRange], employees: &[(u32, u32)]) -> io::Result<Vec<Vec<(u32, String)>>> {
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
            for (index, values) in self.userdb_identities(&[registry.range],
                &registry.employees.iter().map(|entry| (entry.uid, entry.gid)).collect::<Vec<_>>())?.into_iter().enumerate() {
                let step = if index == 0 { Step::UserdbUser } else { Step::UserdbGroup };
                let mut seen = std::collections::BTreeSet::new();
                for (id, name) in values {
                    if id >= registry.range.first && id <= registry.range.last
                        && (!seen.insert(id) || !registry.employees.iter().any(|entry| entry.uid == id && name == account_name(id))) {
                        return Err(at_step(step)(denied()));
                    }
                }
            }
            Ok(())
        }

        /// The SSH part of `verify_locked` (the rest is
        /// `checks::verify_account`): no `/usr/sbin/sshd` ⇒ nothing to check;
        /// otherwise `ssh_config`, then (admission) `ssh_probe`.
        fn verify_ssh(&self, account: &EmployeeAccount) -> io::Result<()> {
            if !checks::ssh_present(&mut RootSeam)? { return Ok(()); }
            // R7c order: spawnd's OWN `sshd -T` must deny the account
            // (and name a port) BEFORE any probe result is consulted
            // (`ssh_account_key` refuses otherwise); the unprivileged
            // probe cannot run `sshd -T` and never checks it.
            let key = self.ssh_account_key(account)?;
            // Then the probe's result for the CURRENT key (a lookup asks
            // the worker for a missing, stale or expired one; never blocks).
            let outcome = self.ssh.lookup(&key);
            if self.require_probe {
                if let Some(refusal) = crate::ssh_evidence::refusal(outcome.as_ref()) { return Err(checks::probe_refused(refusal)); }
            }
            Ok(())
        }

        /// `ssh_config` (`checks::ssh_effective`): `sshd -t`, then the
        /// account's effective configuration
        /// (`sshd -T -C user=<name>,host=localhost,addr=127.0.0.1`) must deny
        /// it ([`ssh_effective_denied`]: the shared parser) and name a first
        /// `port` (refused with [`SSH_PORT_UNKNOWN`] otherwise); the probe key
        /// is that output's SHA-256 and the port (the output itself is never
        /// logged or kept). The port goes into the probe instance name.
        fn ssh_account_key(&self, account: &EmployeeAccount) -> io::Result<crate::ssh_evidence::AccountKey> {
            use sha2::{Digest, Sha256};
            let name = account_name(account.uid);
            let (effective, port) = checks::ssh_effective(&mut RootSeam, &name)?;
            Ok(crate::ssh_evidence::AccountKey { account: name, uid: account.uid,
                sshd_config: duduclaw_isolation::ids::Sha256Digest(Sha256::digest(effective.as_bytes()).into()), port })
        }

        /// `create_locked`'s steps: `local_identity`, `login_barriers`, then
        /// `checks::create_account` (`create_group`, `create_user`,
        /// `lock_user`) and the readback.
        fn create_steps(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            if self.partial_local_identity(account)? != (false, false) { return Err(at_step(Step::LocalIdentity)(denied())); }
            self.install_login_barriers(&account_name(account.uid)).map_err(at_step(Step::LoginBarriers))?;
            checks::create_account(&mut RootSeam, account)?;
            self.verify_locked(account)
        }

        /// `recover_locked`'s steps: `local_identity`, the lockdown of an
        /// existing user (`lock_user`, `login_barriers`), `quarantine` (no
        /// process of the uid), then the readback.
        fn recover_steps(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            let (user, group) = self.partial_local_identity(account)?;
            if user && !group { return Err(at_step(Step::LocalIdentity)(denied())); }
            if user { self.lockdown_existing(account)?; }
            // No user is ever recreated for a tombstone. A group-only residue
            // stays reserved and cannot be assigned to a future employee.
            Self::quarantine(account).map_err(at_step(Step::Quarantine))?;
            if user { self.verify_locked(account)?; }
            Ok(())
        }
        fn quarantine(account: &EmployeeAccount) -> io::Result<()> {
            for process in unexpected_processes(account.uid, None)? {
                terminate_intruder(&process, account.uid, None)?;
            }
            let deadline = Instant::now() + Duration::from_secs(COMMAND_SECONDS);
            loop {
                if unexpected_processes(account.uid, None)?.is_empty() { return Ok(()); }
                if Instant::now() >= deadline { return Err(denied()); }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// H4 `EmployeeIdle` (F6 §3.12 step 4, S14): number of processes whose uid
    /// OR gid fields equal the employee's. Read-only, no signal; a process that
    /// vanishes mid-scan is skipped, any other read error fails the scan.
    pub fn employee_process_count(uid: u32, gid: u32) -> io::Result<usize> {
        let mut count = 0usize;
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if name.parse::<u32>().is_err() { continue; }
            match fs::read_to_string(entry.path().join("status")) {
                Ok(status) => if employee_owned_status(&status, uid, gid)? { count = count.saturating_add(1); },
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        }
        Ok(count)
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
        /// `login_defs`, `subids` (`checks::host_policy`).
        fn policy(&mut self) -> io::Result<HostUidPolicy> { checks::host_policy(&mut RootSeam) }

        /// `nss_modules`, `enumerate`, every account with a passwd row in the
        /// range (tombstones included: `verify_locked`'s labels), then
        /// `userdb_*`.
        fn verify_namespace(&mut self, registry: &Registry) -> io::Result<()> {
            for account in checks::enumerated_accounts(&mut RootSeam, registry)? {
                self.verify_locked(&account)?;
            }
            self.userdb(registry)
        }

        /// `candidate`. File owners are checked once per allocation (`occupied_ids`).
        fn uid_available(&mut self, uid: u32) -> io::Result<bool> { checks::uid_free(&mut RootSeam, uid) }

        /// R7b: the employee-range uids and gids owning anything in spawnd's
        /// own state directories (`crate::owner_inventory`), scanned once per
        /// allocation; a failed or bounded-out scan refuses the allocation
        /// (`owner_inventory`).
        fn occupied_ids(&mut self, range: UidRange) -> io::Result<std::collections::BTreeSet<u32>> {
            crate::owner_inventory::scan_installed(range).map(|inventory| inventory.occupied()).map_err(at_step(Step::OwnerInventory))
        }

        fn create_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            self.create_steps(account).map_err(for_account(account.uid))
        }

        fn recover_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            self.recover_steps(account).map_err(for_account(account.uid))
        }

        /// `checks::verify_account` (`passwd_row` … `sessions`), then the
        /// SSH part (`ssh_config`, `ssh_probe`).
        fn verify_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            checks::verify_account(&mut RootSeam, account)?;
            self.verify_ssh(account).map_err(for_account(account.uid))
        }

        /// R7c allocation (F6: never waits): the account's current probe
        /// result, strict whatever `require_probe` says. Denied ⇒ `Ok`;
        /// otherwise a forced probe is queued (ahead of background requests;
        /// again after an inconclusive result, see
        /// `ssh_evidence::FORCED_RETRY_MS`) and the [`SshRefusal`] is returned
        /// at once (no result yet ⇒ `Pending`, labelled `ssh_probe`). Same
        /// order as `verify_locked`: spawnd's own `sshd -T` first
        /// (`ssh_config`). Hosts without `/usr/sbin/sshd` skip it.
        fn login_denied(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            let result = (|| -> io::Result<()> {
                if !checks::ssh_present(&mut RootSeam)? { return Ok(()); }
                let key = self.ssh_account_key(account)?;
                match crate::ssh_evidence::refusal(self.ssh.request(&key).as_ref()) {
                    None => Ok(()),
                    Some(refusal) => Err(checks::probe_refused(refusal)),
                }
            })();
            result.map_err(for_account(account.uid))
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
            // Admission strictness, without any probe result: a present sshd refuses.
            let mut host = LinuxAccounts::admission(&crate::ssh_evidence::MissingSshEvidence);
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
                let error = recovery.unwrap_err();
                assert_eq!(ssh_refusal(&error), Some(&SshRefusal::Pending), "{error}");
                assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            } else { recovery.unwrap(); }
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::{LinuxAccounts, unexpected_processes, terminate_intruder, employee_process_count};
/// Root tests elsewhere in the crate use the same fixed-argv helper.
#[cfg(all(test, target_os = "linux"))]
pub(crate) use linux::command as root_command;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_host_range_parsers() {
        let defs = "SYS_UID_MIN 100\nSYS_UID_MAX 999\nUID_MIN 1000\nUID_MAX 60000\nSYS_GID_MIN 100\nSYS_GID_MAX 999\nGID_MIN 1000\nGID_MAX 60000\n";
        assert_eq!(parse_login_defs(defs).unwrap().1, UidRange { first: 1000, last: 60000 });
        assert!(parse_login_defs("UID_MIN invalid").is_err());
        assert!(parse_login_defs(&format!("{defs}UID_MIN 1001\n")).is_err());
        assert_eq!(parse_subids("alice:100000:65536\n").unwrap(), vec![UidRange { first: 100000, last: 165535 }]);
        for text in ["a:1:0", "a:4294967295:2", "a:2", "a:-1:2"] { assert!(parse_subids(text).is_err()); }
    }
    #[test]
    fn host_ranges_use_shadow_defaults_and_conservative_system_minimum() {
        let stock = "UID_MIN 1000\nUID_MAX 60000\n#SYS_UID_MIN 100\n#SYS_UID_MAX 999\nGID_MIN 1000\nGID_MAX 60000\n#SYS_GID_MIN 100\n#SYS_GID_MAX 999\n";
        for defs in ["", stock, "UID_MIN 1000", "# SYS_UID_MAX 999\n#SYS_UID_MAX\t999\n"] {
            let (system, login) = parse_login_defs(defs).unwrap();
            assert_eq!(system, UidRange { first: 1, last: 999 });
            assert_eq!(login, UidRange { first: 1000, last: 60000 });
        }
        let (system, login) = parse_login_defs("UID_MIN 500\nGID_MIN 500\n").unwrap();
        assert_eq!(system, UidRange { first: 1, last: 499 });
        assert_eq!(login, UidRange { first: 500, last: 60000 });
        let (system, _) = parse_login_defs("SYS_UID_MIN 101\nSYS_GID_MIN 100\nSYS_UID_MAX 800\nSYS_GID_MAX 900\n").unwrap();
        assert_eq!(system, UidRange { first: 100, last: 900 });
        for key in ["SYS_UID_MIN", "SYS_UID_MAX", "UID_MIN", "UID_MAX", "SYS_GID_MIN", "SYS_GID_MAX", "GID_MIN", "GID_MAX"] {
            for value in ["", "invalid", "999 extra", "4294967296"] {
                assert!(parse_login_defs(&format!("{key} {value}\n")).is_err());
            }
            assert!(parse_login_defs(&format!("{key} 999\n{key} 999\n")).is_err());
        }
        assert!(parse_login_defs("UID_MIN 0\n").is_err());
        assert!(parse_login_defs("GID_MIN 0\n").is_err());
    }
    #[test]
    fn allow_file_overrides_deny_and_ssh_readback_is_exact() {
        assert!(!scheduler_denied("alice", Some("alice\n"), "alice\n"));
        assert!(scheduler_denied("alice", Some("bob\n"), "alice\n"));
        assert!(!scheduler_denied("alice", None, "malice\n"));
        assert!(ssh_effective_denied("denyusers duduclaw-emp-*\ndisableforwarding yes\npermittty no\n"));
        assert!(!ssh_effective_denied("denyusers fake-duduclaw-emp-*\ndisableforwarding yes\npermittty no\n"));
        // R7c: OpenSSH 9.6p1 prints one denyusers line per pattern; a later one never resets the employee pattern.
        assert!(ssh_effective_denied("port 22\npermittty no\ndisableforwarding yes\ndenyusers duduclaw-emp-*\n\
            denyusers duduclaw\ndenyusers duduclaw-runsup\ndenyusers duduclaw-egress\ndenyusers duduclaw-dl\n"));
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
    /// R7c item 3: external NSS is refused by name (no IAM reservation
    /// producer exists); the stock files/systemd configurations stay accepted.
    #[test]
    fn external_nss_is_refused_and_stock_configurations_are_accepted() {
        for accepted in [
            // Ubuntu 24.04 / Debian 12 (libnss-systemd), with the other databases and comments.
            "# /etc/nsswitch.conf\npasswd:         files systemd\ngroup:          files systemd\nshadow:         files systemd\n\
             gshadow:        files systemd\nhosts:          files dns\nnetgroup:       nis\n",
            // Debian without libnss-systemd, RHEL/Fedora authselect "minimal" profile.
            "passwd: files\ngroup: files\n",
            "passwd:     files systemd\nshadow:     files\ngroup:      files systemd\nhosts:      files myhostname dns\n",
            "passwd: files systemd # local only\ngroup: files systemd\n",
        ] {
            assert!(!external_nss(accepted).unwrap(), "{accepted:?}");
            nss_supported(accepted).unwrap();
        }
        for refused in [
            "passwd: files sss\ngroup: files sss\n",
            "passwd: sss files systemd\ngroup: sss files systemd\n",
            "passwd: files ldap\ngroup: files\n",
            // Google Compute Engine OS Login.
            "passwd: files cache_oslogin oslogin\ngroup: files cache_oslogin oslogin\n",
            "passwd: compat\ngroup: compat\n",
            "passwd: files [NOTFOUND=return] systemd\ngroup: files systemd\n",
            "passwd: files systemd\ngroup: files [SUCCESS=merge] systemd\n",
        ] {
            assert!(external_nss(refused).unwrap(), "{refused:?}");
            let error = nss_supported(refused).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), EXTERNAL_NSS_REFUSED);
        }
        // Unreadable structure is still an error, never "not external".
        for broken in ["passwd: files\n", "", "passwd: files\ngroup: files\ngroup: files\n"] {
            assert!(external_nss(broken).is_err() && nss_supported(broken).is_err(), "{broken:?}");
        }
    }
    /// R7c item 4: the useradd argument list no longer carries the key shadow
    /// 4.13 refuses; the sub-id keys stay.
    #[test]
    fn useradd_arguments_have_no_mail_spool_key_and_keep_the_subid_keys() {
        let args = useradd_arguments("70000", "duduclaw-emp-70000");
        assert!(!args.iter().any(|arg| arg.contains("CREATE_MAIL_SPOOL") || arg.contains("MAIL")), "{args:?}");
        let keys: Vec<&str> = args.windows(2).filter(|pair| pair[0] == "--key").map(|pair| pair[1]).collect();
        assert_eq!(keys, ["SUB_UID_COUNT=0", "SUB_GID_COUNT=0"]);
        assert_eq!(args, ["--uid", "70000", "--gid", "70000", "--no-user-group", "--no-create-home", "--no-log-init",
            "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0", "--home-dir", "/nonexistent",
            "--shell", "/usr/sbin/nologin", "--password", "!*", "--expiredate", "1970-01-02", "duduclaw-emp-70000"]);
    }
    /// R7c item 4: a mail spool in either directory refuses the account; the
    /// check never follows or removes anything.
    #[cfg(target_os = "linux")]
    #[test]
    fn mail_spool_check_refuses_either_spool_path() {
        let root = tempfile::tempdir().unwrap();
        let name = "duduclaw-emp-70000";
        // Neither directory exists, then both exist but hold nothing for the account.
        mail_spool_absent(root.path(), name).unwrap();
        for directory in ["var/mail", "var/spool/mail"] { std::fs::create_dir_all(root.path().join(directory)).unwrap(); }
        std::fs::write(root.path().join("var/mail/duduclaw-emp-70001"), b"").unwrap();
        mail_spool_absent(root.path(), name).unwrap();
        for directory in ["var/mail", "var/spool/mail"] {
            let spool = root.path().join(directory).join(name);
            std::fs::write(&spool, b"").unwrap();
            let error = mail_spool_absent(root.path(), name).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains(&spool.display().to_string()), "{error}");
            std::fs::remove_file(&spool).unwrap();
            assert!(mail_spool_absent(root.path(), name).is_ok());
            // A dangling symlink is still a spool entry (lstat, never followed).
            std::os::unix::fs::symlink(root.path().join("absent-target"), &spool).unwrap();
            assert!(mail_spool_absent(root.path(), name).is_err(), "{}", spool.display());
            std::fs::remove_file(&spool).unwrap();
        }
        // The check removed nothing.
        assert!(root.path().join("var/mail/duduclaw-emp-70001").exists());
    }
    /// R7c: the probe refusals carried in `io::Error`: only `Pending` has its
    /// own kind; `Reachable` is the one resolved refusal.
    #[test]
    fn ssh_refusals_are_typed_and_classified() {
        let pending = SshRefusal::Pending.error();
        assert_eq!(pending.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(ssh_refusal(&pending), Some(&SshRefusal::Pending));
        assert!(probe_unresolved(&pending));
        for refusal in [SshRefusal::Inconclusive("identity (exit 3)".into()), SshRefusal::Inconclusive("login_unverified (exit 18)".into())] {
            let error = refusal.clone().error();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(ssh_refusal(&error), Some(&refusal));
            assert!(probe_unresolved(&error));
        }
        let reachable = SshRefusal::Reachable("login_not_denied (exit 10)".into()).error();
        assert!(!probe_unresolved(&reachable));
        assert!(reachable.to_string().contains("NOT denied"), "{reachable}");
        for other in [denied(), io::Error::new(io::ErrorKind::WouldBlock, "busy"), io::ErrorKind::TimedOut.into()] {
            assert_eq!(ssh_refusal(&other), None);
            assert!(!probe_unresolved(&other));
        }
    }
    /// R8w item 2: the labels are a closed set of fixed identifiers; the
    /// journal lines have one shape, the step from that set and the account
    /// only as spawnd's own `duduclaw-emp-<digits>` name (or `-`); the
    /// innermost step wins and the error's kind and text stay unchanged.
    #[test]
    fn step_labels_and_journal_lines_have_a_fixed_shape() {
        let mut names = std::collections::BTreeSet::new();
        for step in Step::ALL {
            let name = step.name();
            assert!(!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_'), "{name}");
            assert!(names.insert(name), "duplicate label {name}");
        }
        for name in ["login_defs", "nss_modules", "enumerate", "passwd_row", "shadow_row", "groups", "cron_acl", "cron_spool",
            "cron_tables", "at_acl", "at_spool", "ssh_config", "ssh_probe", "linger", "userdb_user", "userdb_group",
            "create_group", "create_user", "lock_user"] {
            assert!(names.contains(name), "{name}");
        }
        let error = for_account(70000)(at_step(Step::AtSpool)(denied()));
        assert_eq!(verification_line(&error), "employee account verification failed: step=at_spool account=duduclaw-emp-70000");
        assert_eq!((error.kind(), error.to_string()), (io::ErrorKind::PermissionDenied, denied().to_string()));
        assert_eq!(unstepped(&error).to_string(), denied().to_string());
        // The innermost step and the first account stay.
        let outer = for_account(70001)(at_step(Step::Readback)(error));
        assert_eq!((step_of(&outer), step_account(&outer)), (Some(Step::AtSpool), Some(70000)));
        assert_eq!(allocation_line(&outer), "allocation failed: step=at_spool");
        assert_eq!(verification_line(&at_step(Step::NssModules)(denied())),
            "employee account verification failed: step=nss_modules account=-");
        // Nothing recorded: the fixed fallback, never the error text.
        let plain = for_account(70000)(io::Error::other("/etc/shadow: secret detail"));
        assert_eq!(verification_line(&plain), "employee account verification failed: step=unlabelled account=-");
        assert_eq!(allocation_line(&plain), "allocation failed: step=unlabelled");
        for step in Step::ALL {
            for uid in [None, Some(70000), Some(99999)] {
                let mut error = at_step(step)(io::Error::new(io::ErrorKind::NotFound, "input-derived text"));
                if let Some(uid) = uid { error = for_account(uid)(error); }
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
                let line = verification_line(&error);
                let rest = line.strip_prefix("employee account verification failed: step=").unwrap();
                let (label, account) = rest.split_once(" account=").unwrap();
                assert_eq!(label, step.name());
                assert!(account == "-" || account.strip_prefix("duduclaw-emp-")
                    .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())), "{line}");
                assert!(!line.contains("input-derived"), "{line}");
                assert_eq!(allocation_line(&error), format!("allocation failed: step={}", step.name()));
            }
        }
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
    fn employee_owned_status_matches_any_uid_or_gid_field() {
        let status = |uid: &str, gid: &str| format!("Name:\tx\nUid:\t{uid}\nGid:\t{gid}\n");
        assert!(!employee_owned_status(&status("1 1 1 1", "1 1 1 1"), 70000, 70000).unwrap());
        assert!(employee_owned_status(&status("1 1 70000 1", "1 1 1 1"), 70000, 70000).unwrap());
        // S14: a gid match counts even when every uid field differs.
        assert!(employee_owned_status(&status("1 1 1 1", "1 1 1 70000"), 70000, 70000).unwrap());
        assert!(employee_owned_status(&status("1 1 1 1", "1 1 1 1"), 70000, 70000).is_ok_and(|v| !v));
        assert!(employee_owned_status("Uid:\t1 1 1 1\n", 70000, 70000).is_err());
        assert!(process_gids("Gid: 1 2 3\n").is_err());
        assert_eq!(process_gids("Gid:\t5 6 7 8\n").unwrap(), [5, 6, 7, 8]);
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
