//! Native stop implementation. Unknown activation/readback state is a refusal.
use std::{collections::BTreeSet, fs, io::Read, os::unix::fs::MetadataExt, path::Path, process::{Command, Stdio}, time::{Duration, Instant}};
use super::{RootMigrationIntegration, RootPlan, Error, Evidence, StageOperation, MigrationState, path_metadata, trusted_parents, publish, read_private, private_dir};

const GATEWAY: &str = "duduclaw-gateway.service";
const USER: &str = "duduclaw";
const COMMAND_LIMIT: u64 = 4 * 1024 * 1024;

/// Installer-native implementation of the early root stages. Later stages are
/// deliberately unavailable until the gateway/import/isolation adapters are wired.
pub struct NativeIntegration;
impl RootMigrationIntegration for NativeIntegration {
    fn readback(&mut self, op: StageOperation, plan: &RootPlan) -> Result<Option<Evidence>, Error> {
        if op.target != MigrationState::OldEnvironmentStopped { return Err(Error::Unavailable); }
        if !plan.state_dir.join("stopped-units.json").try_exists()? { return Ok(None); }
        if verify_stopped(plan)? { Ok(Some(Evidence::Stopped { activation_sources_blocked: true, uid_empty: true })) }
        else { Ok(None) }
    }
    fn perform(&mut self, op: StageOperation, plan: &RootPlan) -> Result<(), Error> {
        if op.target != MigrationState::OldEnvironmentStopped { return Err(Error::Unavailable); }
        stop(plan)
    }
    fn gateway_denied_legacy(&mut self, plan: &RootPlan) -> Result<bool, Error> {
        // The stopped legacy service is masked. Installers must provide their new
        // unit namespace readback before importing; early quarantine is protected
        // by a root-only ancestor, proven by a real credential-dropped open probe.
        let m = path_metadata(&plan.state_dir, true, 0)?;
        if m.mode() & 0o7777 != 0o700 { return Ok(false); }
        deny_probe(plan)
    }
}

fn executable(name: &str) -> Result<std::path::PathBuf, Error> {
    for prefix in ["/usr/sbin", "/usr/bin", "/sbin", "/bin"] {
        let path = Path::new(prefix).join(name);
        if path.try_exists()? {
            let path = fs::canonicalize(path)?;
            trusted_parents(&path)?; path_metadata(&path, false, 0)?;
            return Ok(path);
        }
    }
    Err(Error::Unavailable)
}
/// File-backed capture bounds memory and avoids stdout pipe deadlocks. A timeout
/// kills and reaps the child; callers receive no command output in public errors.
fn command(plan: &RootPlan, name: &str, args: &[&str]) -> Result<(bool, String), Error> {
    let mut output = tempfile::NamedTempFile::new_in(&plan.state_dir)?;
    let mut child = Command::new(executable(name)?).args(args).env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").env("LC_ALL", "C")
        .stdin(Stdio::null()).stdout(output.reopen()?).stderr(Stdio::null()).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if output.as_file().metadata()?.len() > COMMAND_LIMIT {
            let _ = child.kill(); let _ = child.wait(); return Err(Error::Limit);
        }
        if let Some(status) = child.try_wait()? { break status; }
        if Instant::now() >= deadline { let _ = child.kill(); let _ = child.wait(); return Err(Error::Unavailable); }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut bytes = Vec::new();
    output.as_file_mut().take(COMMAND_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > COMMAND_LIMIT { return Err(Error::Limit); }
    Ok((status.success(), String::from_utf8(bytes).map_err(|_| Error::Invalid)?))
}
pub(super) fn checked(plan: &RootPlan, name: &str, args: &[&str]) -> Result<String, Error> {
    let (success, out) = command(plan, name, args)?;
    if success { Ok(out) } else { Err(Error::Unavailable) }
}
fn account(plan: &RootPlan) -> Result<(), Error> {
    let out = checked(plan, "getent", &["passwd", USER])?;
    let fields: Vec<_> = out.trim_end_matches('\n').split(':').collect();
    if fields.len() != 7 || fields[0] != USER || fields[2].parse::<u32>().ok() != Some(plan.gateway_uid)
        || fields[3].parse::<u32>().ok() != Some(plan.gateway_gid) { return Err(Error::Invalid); }
    Ok(())
}
fn valid_unit(unit: &str) -> bool {
    if unit.is_empty() || unit.len() > 255 || unit.as_bytes().first() == Some(&b'-')
        || !["service", "socket", "timer"].contains(&unit.rsplit('.').next().unwrap_or("")) {
        return false;
    }
    let mut bytes = unit.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' {
            if bytes.next() != Some(b'x') || !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|b| b.is_ascii_hexdigit()) { return false; }
        } else if !byte.is_ascii_alphanumeric() && !b"_.@:-".contains(&byte) { return false; }
    }
    true
}
fn inventory_names(files: &str, loaded: &str) -> Result<BTreeSet<String>, Error> {
    let mut names = BTreeSet::new();
    for name in files.lines().chain(loaded.lines()).filter_map(|line| line.split_whitespace().next()) {
        if !valid_unit(name) { return Err(Error::Invalid); }
        names.insert(name.to_string());
        if names.len() > 10_000 { return Err(Error::Limit); }
    }
    Ok(names)
}
fn units(plan: &RootPlan) -> Result<BTreeSet<String>, Error> {
    let files = checked(plan, "systemctl", &["list-unit-files", "--no-legend", "--no-pager", "--type=service,socket,timer"])?;
    // Transient systemd-run units and loaded-only services have no unit file.
    // Include them while stopped and on every readback, including a delayed
    // Restart=always service whose process is currently absent.
    let loaded = checked(plan, "systemctl", &["list-units", "--all", "--plain", "--no-legend", "--no-pager", "--type=service,socket,timer"])?;
    let names = inventory_names(&files, &loaded)?;
    let mut selected = BTreeSet::from([GATEWAY.to_string(), format!("user@{}.service", plan.gateway_uid)]);
    let mut references = Vec::new();
    for name in names {
        let show = checked(plan, "systemctl", &["show", &name, "--property=User,Triggers"])?;
        let mut owner = "";
        let mut triggers = Vec::new();
        for line in show.lines() {
            if let Some(v) = line.strip_prefix("User=") { owner = v; }
            if let Some(v) = line.strip_prefix("Triggers=") { triggers = v.split_whitespace().map(str::to_string).collect(); }
        }
        if owner == USER || owner.parse::<u32>().ok() == Some(plan.gateway_uid) { selected.insert(name.to_string()); }
        references.push((name.to_string(), triggers));
    }
    loop {
        let before = selected.len();
        for (name, targets) in &references {
            if targets.iter().any(|target| selected.contains(target)) { selected.insert(name.clone()); }
        }
        if before == selected.len() { break; }
    }
    Ok(selected)
}
fn saved_units(plan: &RootPlan) -> Result<BTreeSet<String>, Error> {
    let names: BTreeSet<String> = serde_json::from_slice(&read_private(&plan.state_dir.join("stopped-units.json"), COMMAND_LIMIT, 0)?)?;
    if names.len() > 10_000 || !names.contains(GATEWAY) || names.iter().any(|n| !valid_unit(n)) { return Err(Error::Invalid); }
    Ok(names)
}
fn mask_unit(plan: &RootPlan, unit: &str) -> Result<(), Error> {
    checked(plan, "systemctl", &["stop", unit])?;
    let path = Path::new("/etc/systemd/system").join(unit);
    trusted_parents(&path)?;
    if let Ok(m) = fs::symlink_metadata(&path) {
        if m.file_type().is_symlink() && fs::read_link(&path)? == Path::new("/dev/null") { return Ok(()); }
        // Keep the original unit in the root recovery area before replacing it.
        path_metadata(&path, false, 0)?;
        if m.len() > 1024 * 1024 { return Err(Error::Limit); }
        let backup_dir = plan.state_dir.join("legacy-units"); private_dir(&backup_dir)?;
        let bytes = fs::read(&path)?;
        publish(&backup_dir.join(unit), &bytes, 0, 0, false)?;
        fs::remove_file(&path)?;
    }
    std::os::unix::fs::symlink("/dev/null", &path)?;
    super::sync_parent(&path)
}
fn block_scheduler(plan: &RootPlan, stem: &str) -> Result<(), Error> {
    for (suffix, add) in [("deny", true), ("allow", false)] {
        let path = Path::new("/etc").join(format!("{stem}.{suffix}"));
        trusted_parents(&path)?;
        if !path.try_exists()? && !add { continue; }
        let content = if path.try_exists()? {
            let m = path_metadata(&path, false, 0)?;
            if m.len() > 1024 * 1024 { return Err(Error::Limit); }
            fs::read_to_string(&path)?
        } else { String::new() };
        let mut lines: Vec<_> = content.lines().filter(|line| line.trim() != USER).map(str::to_string).collect();
        if add { lines.push(USER.to_string()); }
        let mut value = lines.join("\n"); value.push('\n');
        publish(&path, value.as_bytes(), 0, 0, true)?;
    }
    // Existing jobs are not made safe merely by adding a deny entry.
    if stem == "cron" {
        let _ = command(plan, "crontab", &["-u", USER, "-r"])?;
    } else {
        let out = checked(plan, "atq", &[])?;
        for line in out.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.last() == Some(&USER) {
                let id = fields.first().ok_or(Error::Invalid)?;
                if !id.bytes().all(|b| b.is_ascii_digit()) { return Err(Error::Invalid); }
                checked(plan, "atrm", &[id])?;
            }
        }
    }
    Ok(())
}
fn schedulers_blocked(plan: &RootPlan) -> Result<bool, Error> {
    for stem in ["cron", "at"] {
        let allow = Path::new("/etc").join(format!("{stem}.allow"));
        let deny = Path::new("/etc").join(format!("{stem}.deny"));
        for path in [&allow, &deny] { if path.try_exists()? { path_metadata(path, false, 0)?; } }
        if allow.try_exists()? && fs::read_to_string(allow)?.lines().any(|l| l.trim() == USER) { return Ok(false); }
        if !deny.try_exists()? || !fs::read_to_string(deny)?.lines().any(|l| l.trim() == USER) { return Ok(false); }
    }
    for path in ["/var/spool/cron/crontabs/duduclaw", "/var/spool/cron/duduclaw"] {
        if fs::symlink_metadata(path).is_ok() { return Ok(false); }
    }
    if checked(plan, "atq", &[])?.lines().any(|line| line.split_whitespace().last() == Some(USER)) { return Ok(false); }
    // Root cron configuration is trusted TCB, but explicit jobs naming the old
    // account must be removed by the operator instead of silently rewritten.
    let mut paths = vec![std::path::PathBuf::from("/etc/crontab")];
    for dir in ["/etc/cron.d", "/etc/cron.hourly", "/etc/cron.daily", "/etc/cron.weekly", "/etc/cron.monthly"] {
        if !Path::new(dir).try_exists()? { continue; }
        path_metadata(Path::new(dir), true, 0)?;
        for entry in fs::read_dir(dir)? { paths.push(entry?.path()); }
    }
    if paths.len() > 4096 { return Err(Error::Limit); }
    for path in paths {
        if !path.try_exists()? { continue; }
        let m = path_metadata(&path, false, 0)?;
        if m.len() > 1024 * 1024 { return Err(Error::Limit); }
        let text = fs::read_to_string(path)?;
        if text.lines().filter(|line| !line.trim_start().starts_with('#'))
            .flat_map(|line| line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-'))
            .any(|word| word == USER || word == plan.gateway_uid.to_string()) { return Ok(false); }
    }
    Ok(true)
}
fn uid_processes(uid: u32) -> Result<Vec<i32>, Error> {
    let mut processes = Vec::new();
    for item in fs::read_dir("/proc")? {
        let item = item?;
        let name = item.file_name();
        let Some(pid) = name.to_str().and_then(|v| v.parse::<i32>().ok()) else { continue; };
        let status = match fs::read_to_string(item.path().join("status")) {
            Ok(v) => v, Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, Err(e) => return Err(e.into()),
        };
        let line = status.lines().find_map(|line| line.strip_prefix("Uid:")).ok_or(Error::Readback)?;
        let values: Vec<_> = line.split_whitespace().map(str::parse::<u32>).collect::<Result<_, _>>().map_err(|_| Error::Readback)?;
        if values.len() != 4 { return Err(Error::Readback); }
        if values.contains(&uid) { processes.push(pid); }
        if processes.len() > 100_000 { return Err(Error::Limit); }
    }
    Ok(processes)
}
fn terminate_uid(uid: u32) -> Result<(), Error> {
    for pid in uid_processes(uid)? {
        // pidfd pins the target; numeric PID reuse never authorizes a signal.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
        if fd < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) { continue; }
            return Err(Error::Unavailable);
        }
        use std::os::fd::FromRawFd;
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        if uid_processes(uid)?.contains(&pid) {
            let result = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd, libc::SIGKILL, std::ptr::null::<libc::siginfo_t>(), 0) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) { return Err(Error::Unavailable); }
        }
        drop(owned);
    }
    for _ in 0..100 {
        if uid_processes(uid)?.is_empty() { return Ok(()); }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(Error::Readback)
}
fn login_blocked(plan: &RootPlan) -> Result<bool, Error> {
    let passwd = checked(plan, "getent", &["passwd", USER])?;
    let fields: Vec<_> = passwd.trim_end_matches('\n').split(':').collect();
    if fields.len() != 7 || !["/usr/sbin/nologin", "/sbin/nologin"].contains(&fields[6]) { return Ok(false); }
    let shadow = checked(plan, "getent", &["shadow", USER])?;
    let fields: Vec<_> = shadow.trim_end_matches('\n').split(':').collect();
    if fields.len() != 9 || fields[0] != USER || fields[7] != "1"
        || !fields[1].as_bytes().first().is_some_and(|c| *c == b'!' || *c == b'*') { return Ok(false); }
    let linger = Path::new("/var/lib/systemd/linger").join(USER);
    Ok(!linger.try_exists()?)
}
fn verify_stopped(plan: &RootPlan) -> Result<bool, Error> {
    account(plan)?;
    if !login_blocked(plan)? || !schedulers_blocked(plan)? || !uid_processes(plan.gateway_uid)?.is_empty() { return Ok(false); }
    let saved = saved_units(plan)?;
    if !units(plan)?.is_subset(&saved) { return Err(Error::Conflict); }
    for unit in saved {
        let show = checked(plan, "systemctl", &["show", &unit, "--property=ActiveState,LoadState"])?;
        if !show.lines().any(|l| l == "LoadState=masked")
            || !show.lines().any(|l| l == "ActiveState=inactive" || l == "ActiveState=failed") { return Ok(false); }
    }
    Ok(true)
}
fn stop(plan: &RootPlan) -> Result<(), Error> {
    account(plan)?;
    let path = plan.state_dir.join("stopped-units.json");
    let selected = if path.try_exists()? { saved_units(plan)? } else {
        let selected = units(plan)?;
        publish(&path, &serde_json::to_vec(&selected)?, 0, 0, false)?;
        selected
    };
    checked(plan, "usermod", &["--lock", "--expiredate", "1", "--shell", "/usr/sbin/nologin", USER])?;
    checked(plan, "loginctl", &["disable-linger", USER])?;
    // No current login session is a normal outcome; readback is authoritative.
    let _ = command(plan, "loginctl", &["terminate-user", USER])?;
    for unit in selected { mask_unit(plan, &unit)?; }
    checked(plan, "systemctl", &["daemon-reload"])?;
    block_scheduler(plan, "cron")?;
    block_scheduler(plan, "at")?;
    terminate_uid(plan.gateway_uid)?;
    if !verify_stopped(plan)? { return Err(Error::Readback); }
    Ok(())
}
fn deny_probe(plan: &RootPlan) -> Result<bool, Error> {
    use std::os::unix::process::CommandExt;
    let program = executable("test")?;
    let result = Command::new(program).args(["-r"]).arg(&plan.state_dir).env_clear()
        .uid(plan.gateway_uid).gid(plan.gateway_gid).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    Ok(result.code() == Some(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_names_are_not_command_arguments() {
        assert!(valid_unit("duduclaw-gateway.service"));
        assert!(valid_unit("user@999.service"));
        assert!(valid_unit(r"systemd-fsck@dev-disk-by\x2duuid-ab.service"));
        for bad in ["--help", "--help.service", "a/b.service", "a\n.service", "x.target", r"bad\xzz.service", r"bad\x2.service"] { assert!(!valid_unit(bad)); }
    }
    #[test]
    fn inventory_includes_loaded_only_and_deduplicates() {
        let found = inventory_names("installed.service enabled\n", "installed.service loaded active running\ntransient.service loaded activating auto-restart\n").unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.contains("transient.service"));
        assert!(inventory_names("--help enabled", "").is_err());
    }
    #[test]
    #[ignore = "root/systemd required; creates a disposable transient unit with a delayed restart"]
    fn iso_root_inventory_captures_transient_restart_unit() {
        super::super::root().unwrap();
        let tmp = tempfile::tempdir_in("/root").unwrap();
        let uid = 199_992;
        assert!(uid_processes(uid).unwrap().is_empty(), "test uid already in use");
        let plan = RootPlan { migration: duduclaw_isolation::ids::RequestId([5;16]), state_dir: tmp.path().to_path_buf(),
            legacy_home: tmp.path().join("unused-home"), root_key_path: tmp.path().join("unused-key"),
            gateway_uid: uid, gateway_gid: uid, snapshot_content_limit: 1024, snapshot_encoded_limit: 4096, expires_at_unix: 2_000_000_000 };
        let unit = format!("duduclaw-migrate-test-{}.service", std::process::id());
        checked(&plan, "systemd-run", &["--quiet", "--unit", &unit, "--property=User=199992", "--property=Group=199992",
            "--property=Restart=always", "--property=RestartSec=30s", "--", "/usr/bin/false"]).unwrap();
        let result = (|| -> Result<(), Error> {
            if !units(&plan)?.contains(&unit) { return Err(Error::Readback); }
            mask_unit(&plan, &unit)?;
            checked(&plan, "systemctl", &["daemon-reload"])?;
            let state = checked(&plan, "systemctl", &["show", &unit, "--property=LoadState,ActiveState"])?;
            if !state.lines().any(|v| v == "LoadState=masked")
                || !state.lines().any(|v| v == "ActiveState=inactive" || v == "ActiveState=failed") { return Err(Error::Readback); }
            Ok(())
        })();
        let _ = command(&plan, "systemctl", &["stop", &unit]);
        let mask = Path::new("/etc/systemd/system").join(&unit);
        if fs::read_link(&mask).ok().as_deref() == Some(Path::new("/dev/null")) { fs::remove_file(mask).unwrap(); }
        let _ = command(&plan, "systemctl", &["reset-failed", &unit]);
        checked(&plan, "systemctl", &["daemon-reload"]).unwrap();
        result.unwrap();
    }
    #[test]
    #[ignore = "root and procfs required; creates and kills only an isolated test uid child"]
    fn iso_root_pidfd_stop_reads_all_uid_fields() {
        use std::os::unix::process::CommandExt;
        super::super::root().unwrap();
        let uid = 199_991;
        assert!(uid_processes(uid).unwrap().is_empty(), "test uid already in use");
        let mut child = Command::new("/usr/bin/sleep").arg("60").uid(uid).gid(uid).spawn().unwrap();
        assert!(uid_processes(uid).unwrap().contains(&(child.id() as i32)));
        // Reap concurrently: a zombie remains a process until its parent waits.
        let waiter = std::thread::spawn(move || child.wait().unwrap());
        terminate_uid(uid).unwrap();
        assert!(!waiter.join().unwrap().success());
        assert!(uid_processes(uid).unwrap().is_empty());
    }
}
