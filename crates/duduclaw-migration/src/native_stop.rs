//! Native stop implementation. Unknown activation/readback state is a refusal.
use std::{collections::BTreeSet, fs, io::Read, os::unix::fs::{MetadataExt, PermissionsExt}, path::Path, process::{Command, Stdio}, time::{Duration, Instant}};
use super::{RootMigrationIntegration, RootPlan, Error, Evidence, StageOperation, MigrationState, path_metadata, trusted_parents, publish, read_private, private_dir};

const GATEWAY: &str = "duduclaw-gateway.service";
const USER: &str = "duduclaw";
const COMMAND_LIMIT: u64 = 4 * 1024 * 1024;
/// Synthetic instance through which a template (`name@.type`) is inspected:
/// `show name@.type` is refused by systemd, `show name@PROBE.type` answers with
/// the template's settings and leaves no loaded unit behind.
const PROBE: &str = "duduclaw-migration-probe";
/// The probe as an `%I` (unescaped instance) specifier resolves it.
const PROBE_UNESCAPED: &str = "duduclaw/migration/probe";
const SHOW: &str = "--property=User,Triggers,LoadState";
/// Selected units that were already masked before this migration masked anything.
const PREMASKED: &str = "premasked-units.json";

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
/// Shape of a valid unit name (systemd's `unit_name_is_valid`): `prefix.type`,
/// `prefix@.type` (a template) or `prefix@instance.type` (an instance), split at
/// the first `@`; the prefix is never empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitKind { Plain, Template, Instance }
fn unit_kind(unit: &str) -> Result<UnitKind, Error> {
    if !valid_unit(unit) { return Err(Error::Invalid); }
    let (stem, _) = unit.rsplit_once('.').ok_or(Error::Invalid)?;
    if stem.is_empty() { return Err(Error::Invalid); }
    match stem.split_once('@') {
        None => Ok(UnitKind::Plain),
        Some(("", _)) => Err(Error::Invalid),
        Some((_, "")) => Ok(UnitKind::Template),
        Some(_) => Ok(UnitKind::Instance),
    }
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
/// `prefix@PROBE.type` for a template `prefix@.type`.
fn probe_name(template: &str) -> Result<String, Error> {
    if unit_kind(template)? != UnitKind::Template { return Err(Error::Invalid); }
    let (stem, kind) = template.rsplit_once('.').ok_or(Error::Invalid)?;
    let probe = format!("{stem}{PROBE}.{kind}");
    if unit_kind(&probe)? != UnitKind::Instance { return Err(Error::Invalid); }
    Ok(probe)
}
/// The name `show` is asked about: a template only ever through its probe.
fn query_name(unit: &str) -> Result<String, Error> {
    if unit_kind(unit)? == UnitKind::Template { probe_name(unit) } else { Ok(unit.to_string()) }
}
/// A probe instance named in `Triggers=` stands for its template.
fn unprobe(unit: &str) -> String {
    unit.rsplit_once('.')
        .and_then(|(stem, kind)| Some(format!("{}@.{kind}", stem.strip_suffix(PROBE)?.strip_suffix('@')?)))
        .filter(|template| unit_kind(template) == Ok(UnitKind::Template))
        .unwrap_or_else(|| unit.to_string())
}
/// One `show <unit> --property=User,Triggers,LoadState` answer.
struct Shown { load: String, user: String, triggers: Vec<String> }
/// LoadState exactly once and non-empty; User/Triggers at most once; nothing else.
fn parse_show(show: &str) -> Result<Shown, Error> {
    let (mut load, mut user, mut triggers) = (None, None, None);
    for line in show.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once('=').ok_or(Error::Invalid)?;
        let slot = match key { "LoadState" => &mut load, "User" => &mut user, "Triggers" => &mut triggers, _ => return Err(Error::Invalid) };
        if slot.replace(value).is_some() { return Err(Error::Invalid); }
    }
    let load = load.filter(|v| !v.is_empty()).ok_or(Error::Invalid)?;
    Ok(Shown { load: load.to_string(), user: user.unwrap_or("").to_string(),
        triggers: triggers.unwrap_or("").split_whitespace().map(unprobe).collect() })
}
/// `show` one unit (a template through its probe) and refuse an unknown load
/// state: `error`/`stub`/... leave `User=` unknown. A listed template's probe
/// must resolve to its file, so `not-found` is accepted for loadable names only.
fn inspect(systemctl: &mut impl FnMut(&[&str]) -> Result<String, Error>, unit: &str) -> Result<Shown, Error> {
    let kind = unit_kind(unit)?;
    let query = query_name(unit)?;
    let shown = parse_show(&systemctl(&["show", query.as_str(), SHOW])?)?;
    let known = matches!(shown.load.as_str(), "loaded" | "masked" | "bad-setting")
        || (kind != UnitKind::Template && shown.load == "not-found");
    if known { Ok(shown) } else { Err(Error::Invalid) }
}
/// The comparison that selects a unit for the legacy account.
fn legacy_owner(plan: &RootPlan, owner: &str) -> bool {
    owner == USER || owner.parse::<u32>().ok() == Some(plan.gateway_uid)
}
/// A template whose probe reports a `User=` containing the probe string derives
/// its user from the instance name (`User=%i`, `User=prefix-%i`, `%I`). Returns
/// the exact instances that resolve to the legacy account — instantiated by
/// account name or by numeric uid (`user@.service` uses the uid) — or None for a
/// fixed user.
fn derived_instances(plan: &RootPlan, template: &str, user: &str) -> Result<Option<BTreeSet<String>>, Error> {
    let forms: Vec<&str> = [PROBE, PROBE_UNESCAPED].into_iter().filter(|form| user.contains(form)).collect();
    if forms.is_empty() { return Ok(None); }
    let (stem, kind) = template.rsplit_once('.').ok_or(Error::Invalid)?;
    let mut instances = BTreeSet::new();
    for candidate in [USER.to_string(), plan.gateway_uid.to_string()] {
        if forms.iter().any(|form| legacy_owner(plan, &user.replace(form, &candidate))) {
            let instance = format!("{stem}{candidate}.{kind}");
            if unit_kind(&instance)? != UnitKind::Instance { return Err(Error::Invalid); }
            instances.insert(instance);
        }
    }
    Ok(Some(instances))
}
fn units(plan: &RootPlan) -> Result<BTreeSet<String>, Error> {
    units_with(plan, |args| checked(plan, "systemctl", args))
}
/// `units` over an injected `systemctl` runner (tests use a fake).
fn units_with(plan: &RootPlan, systemctl: impl FnMut(&[&str]) -> Result<String, Error>) -> Result<BTreeSet<String>, Error> {
    units_seeded(plan, &BTreeSet::new(), systemctl)
}
/// `seeds` count as selected for the `Triggers=` closure (saved-set validation).
fn units_seeded(plan: &RootPlan, seeds: &BTreeSet<String>, mut systemctl: impl FnMut(&[&str]) -> Result<String, Error>)
    -> Result<BTreeSet<String>, Error> {
    let files = systemctl(&["list-unit-files", "--no-legend", "--no-pager", "--type=service,socket,timer"])?;
    // Transient systemd-run units and loaded-only services have no unit file.
    // Include them while stopped and on every readback, including a delayed
    // Restart=always service whose process is currently absent.
    let loaded = systemctl(&["list-units", "--all", "--plain", "--no-legend", "--no-pager", "--type=service,socket,timer"])?;
    let names = inventory_names(&files, &loaded)?;
    let mut selected = BTreeSet::from([GATEWAY.to_string(), format!("user@{}.service", plan.gateway_uid)]);
    selected.extend(seeds.iter().cloned());
    let mut references = Vec::new();
    let mut derived = BTreeSet::new();
    for name in &names {
        // A template (`name@.type`, list-unit-files only) can be instantiated
        // later by anything, so it is inspected through its probe instance; its
        // live instances come from `list-units --all` and are inspected as units.
        let shown = inspect(&mut systemctl, name)?;
        if unit_kind(name)? == UnitKind::Template {
            if let Some(instances) = derived_instances(plan, name, &shown.user)? {
                // Never mask an instance-derived template (stock `user@.service`
                // serves every session); the legacy instances are blocked below.
                derived.extend(instances);
                continue;
            }
        }
        // A fixed legacy user, or the Triggers= closure, selects a template
        // itself: masking `name@.type` blocks every future instance.
        if legacy_owner(plan, &shown.user) { selected.insert(name.clone()); }
        references.push((name.clone(), shown.triggers));
    }
    // Loaded legacy instances were already inspected from the listing.
    for name in derived.difference(&names) {
        let shown = inspect(&mut systemctl, name)?;
        if legacy_owner(plan, &shown.user) { selected.insert(name.clone()); }
        references.push((name.clone(), shown.triggers));
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
    saved_units_with(plan, names, |args| checked(plan, "systemctl", args))
}
/// A template is accepted only as `units` selects it (rule 2a): it either reads
/// back masked by the stop already, or is selected now — with the other saved
/// units and masked templates as seeds, so a template selected through a
/// trigger that is already masked stays accepted. Any other template refuses.
fn saved_units_with(plan: &RootPlan, names: BTreeSet<String>, mut systemctl: impl FnMut(&[&str]) -> Result<String, Error>)
    -> Result<BTreeSet<String>, Error> {
    if names.len() > 10_000 || !names.contains(GATEWAY) || names.iter().any(|n| unit_kind(n).is_err()) { return Err(Error::Invalid); }
    let mut seeds = BTreeSet::new();
    let mut pending = Vec::new();
    for name in &names {
        if unit_kind(name)? != UnitKind::Template { seeds.insert(name.clone()); continue; }
        match inspect(&mut systemctl, name)?.load.as_str() {
            "masked" => { seeds.insert(name.clone()); }
            _ => pending.push(name),
        }
    }
    if !pending.is_empty() {
        let selected = units_seeded(plan, &seeds, &mut systemctl)?;
        if pending.iter().any(|name| !selected.contains(*name)) { return Err(Error::Invalid); }
    }
    Ok(names)
}
/// Every saved unit reads back masked and not running; a template through its
/// probe instance (masking `name@.type` masks every instance).
fn masked_with(saved: &BTreeSet<String>, mut systemctl: impl FnMut(&[&str]) -> Result<String, Error>) -> Result<bool, Error> {
    for unit in saved {
        let query = query_name(unit)?;
        let show = systemctl(&["show", query.as_str(), "--property=ActiveState,LoadState"])?;
        if !show.lines().any(|l| l == "LoadState=masked")
            || !show.lines().any(|l| l == "ActiveState=inactive" || l == "ActiveState=failed") { return Ok(false); }
    }
    Ok(true)
}
fn premasked_with(selected: &BTreeSet<String>, mut systemctl: impl FnMut(&[&str]) -> Result<String, Error>) -> Result<BTreeSet<String>, Error> {
    let mut premasked = BTreeSet::new();
    for unit in selected {
        let query = query_name(unit)?;
        let show = systemctl(&["show", query.as_str(), "--property=LoadState"])?;
        if show.lines().any(|l| l == "LoadState=masked") { premasked.insert(unit.clone()); }
    }
    Ok(premasked)
}
/// The saved stop set. Nothing is masked before it is published, so a selected
/// unit that reads masked now was masked by someone else: restore keeps it.
fn record(plan: &RootPlan) -> Result<BTreeSet<String>, Error> {
    let path = plan.state_dir.join("stopped-units.json");
    if path.try_exists()? { return saved_units(plan); }
    let selected = units(plan)?;
    let premasked = premasked_with(&selected, |args| checked(plan, "systemctl", args))?;
    publish(&plan.state_dir.join(PREMASKED), &serde_json::to_vec(&premasked)?, 0, 0, true)?;
    publish(&path, &serde_json::to_vec(&selected)?, 0, 0, false)?;
    Ok(selected)
}
/// Saved units to unmask on restore, in order; one masked before is kept.
fn unmask_with(saved: &BTreeSet<String>, premasked: &BTreeSet<String>, mut systemctl: impl FnMut(&[&str]) -> Result<String, Error>)
    -> Result<Vec<String>, Error> {
    let mut unmasked = Vec::new();
    for unit in saved.difference(premasked) {
        systemctl(&["unmask", unit.as_str()])?;
        unmasked.push(unit.clone());
    }
    Ok(unmasked)
}
/// Put back the unit file `mask_unit` moved into the recovery area.
fn put_back(plan: &RootPlan, unit: &str) -> Result<(), Error> {
    let backup = plan.state_dir.join("legacy-units").join(unit);
    if !backup.try_exists()? { return Ok(()); }
    let path = Path::new("/etc/systemd/system").join(unit);
    trusted_parents(&path)?;
    match fs::symlink_metadata(&path) {
        // Still masked after `unmask`, or the original/an administrator's file is in place.
        Ok(m) => {
            if m.file_type().is_symlink() && fs::read_link(&path)? == Path::new("/dev/null") { return Err(Error::Readback); }
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let bytes = read_private(&backup, 1024 * 1024, 0)?;
    publish(&path, &bytes, 0, 0, false)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    super::sync_parent(&path)
}
/// Restore path: unmask exactly the saved units this migration masked —
/// templates (2a), legacy instances of instance-derived templates (2b) and
/// plain units — put back replaced unit files and read the unmask back.
pub fn restore(plan: &RootPlan) -> Result<(), Error> {
    super::root()?;
    let saved = saved_units(plan)?;
    let premasked: BTreeSet<String> = serde_json::from_slice(&read_private(&plan.state_dir.join(PREMASKED), COMMAND_LIMIT, 0)?)?;
    if !premasked.is_subset(&saved) { return Err(Error::Invalid); }
    let unmasked = unmask_with(&saved, &premasked, |args| checked(plan, "systemctl", args))?;
    for unit in &unmasked { put_back(plan, unit)?; }
    checked(plan, "systemctl", &["daemon-reload"])?;
    for unit in &unmasked {
        let query = query_name(unit)?;
        let show = checked(plan, "systemctl", &["show", query.as_str(), "--property=LoadState"])?;
        if show.lines().any(|l| l == "LoadState=masked") { return Err(Error::Readback); }
    }
    Ok(())
}
fn mask_unit(plan: &RootPlan, unit: &str) -> Result<(), Error> {
    // A template has no runtime state (`stop name@.type` is refused); its live
    // instances are inventory entries of their own and stopped as such.
    if unit_kind(unit)? != UnitKind::Template { checked(plan, "systemctl", &["stop", unit])?; }
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
    // Includes every template (2a) and legacy instance (2b) the stop masked.
    masked_with(&saved, |args| checked(plan, "systemctl", args))
}
fn stop(plan: &RootPlan) -> Result<(), Error> {
    account(plan)?;
    let selected = record(plan)?;
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
    fn unit_kind_classifies_templates_instances_and_plain_names_strictly() {
        for template in ["getty@.service", "apport-coredump-hook@.service", "duduclaw-migrate-expire@.timer", "a.b@.socket"] {
            assert_eq!(unit_kind(template), Ok(UnitKind::Template), "{template}");
        }
        for instance in ["getty@tty1.service", "user@999.service", r"systemd-fsck@dev-disk-by\x2duuid-ab.service", "a@b@c.service"] {
            assert_eq!(unit_kind(instance), Ok(UnitKind::Instance), "{instance}");
        }
        for plain in ["duduclaw-gateway.service", "dbus.socket", "apt-daily.timer"] {
            assert_eq!(unit_kind(plain), Ok(UnitKind::Plain), "{plain}");
        }
        // The shape is validated before anything is called a template.
        for bad in ["", "@.service", "@tty1.service", ".service", "getty@", "getty@.", "getty@.target", "-x@.service",
            "a/b@.service", "a\n@.service", r"bad\xzz@.service"] {
            assert_eq!(unit_kind(bad), Err(Error::Invalid), "{bad:?}");
        }
    }
    fn test_plan(uid: u32) -> RootPlan {
        RootPlan { migration: duduclaw_isolation::ids::RequestId([5; 16]), state_dir: "/nonexistent".into(),
            legacy_home: "/nonexistent/home".into(), root_key_path: "/nonexistent/key".into(), gateway_uid: uid,
            gateway_gid: uid, snapshot_content_limit: 1024, snapshot_encoded_limit: 4096, expires_at_unix: 2_000_000_000 }
    }
    /// A fake host behind the `systemctl` seam, shaped like systemd 255 on Ubuntu
    /// 24.04: `show` of a template NAME exits non-zero (only a probe instance
    /// answers). `answers` maps a shown name to its User/Triggers/LoadState body;
    /// anything else is a loaded root unit. Names in `masked` read back masked;
    /// `fail` makes that one `show` fail. Every call is recorded.
    #[derive(Clone, Default)]
    struct FakeHost { files: &'static str, loaded: &'static str, answers: &'static [(&'static str, &'static str)],
        masked: Vec<&'static str>, fail: Option<&'static str> }
    fn fake<'a>(host: &'a FakeHost, calls: &'a mut Vec<String>) -> impl FnMut(&[&str]) -> Result<String, Error> + 'a {
        move |args: &[&str]| {
            calls.push(args.join(" "));
            match args {
                ["list-unit-files", "--no-legend", "--no-pager", "--type=service,socket,timer"] => Ok(host.files.to_string()),
                ["list-units", "--all", "--plain", "--no-legend", "--no-pager", "--type=service,socket,timer"] => Ok(host.loaded.to_string()),
                ["show", name, property] => {
                    if unit_kind(name) == Ok(UnitKind::Template) || host.fail == Some(*name) { return Err(Error::Unavailable); }
                    let masked = host.masked.iter().any(|m| m == name);
                    match *property {
                        "--property=User,Triggers,LoadState" if masked => Ok("User=\nTriggers=\nLoadState=masked\n".to_string()),
                        "--property=User,Triggers,LoadState" => Ok(host.answers.iter().find(|(n, _)| n == name)
                            .map_or("User=\nTriggers=\nLoadState=loaded\n", |(_, a)| *a).to_string()),
                        "--property=ActiveState,LoadState" | "--property=LoadState" =>
                            Ok(format!("ActiveState=inactive\nLoadState={}\n", if masked { "masked" } else { "loaded" })),
                        _ => Err(Error::Invalid),
                    }
                }
                ["unmask", _] => Ok(String::new()),
                _ => Err(Error::Invalid),
            }
        }
    }
    /// Names inspected through `show … --property=User,Triggers,LoadState`.
    fn shown(calls: &[String]) -> Vec<&str> {
        calls.iter().filter_map(|c| c.strip_prefix("show ")?.strip_suffix(" --property=User,Triggers,LoadState")).collect()
    }
    #[test]
    fn inventory_probes_templates_inspects_instances_and_still_refuses_failed_show() {
        let plan = test_plan(999);
        let host = FakeHost {
            files: "getty@.service enabled enabled\napport-coredump-hook@.service static -\n\
                duduclaw-worker@.service disabled enabled\nduduclaw-gateway.service enabled enabled\n\
                numeric-owner.service enabled enabled\nunrelated.service enabled enabled\nworker-kick.timer enabled enabled\n",
            loaded: "getty@tty1.service loaded active running Getty on tty1\n\
                duduclaw-worker@1.service loaded activating auto-restart Worker 1\n\
                worker-kick.timer loaded active waiting Kick\nunrelated.service loaded active running Unrelated\n",
            answers: &[("duduclaw-worker@1.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                ("worker-kick.timer", "User=\nTriggers=duduclaw-worker@1.service\nLoadState=loaded\n"),
                ("numeric-owner.service", "User=999\nTriggers=\nLoadState=loaded\n")],
            ..FakeHost::default() };
        let mut calls = Vec::new();
        let selected = units_with(&plan, fake(&host, &mut calls)).unwrap();
        assert_eq!(selected, BTreeSet::from(["duduclaw-gateway.service", "user@999.service", "duduclaw-worker@1.service",
            "worker-kick.timer", "numeric-owner.service"].map(String::from)));
        // No template NAME reached `show`; each template was probed; every
        // instance and plain unit was inspected.
        let names = shown(&calls);
        assert!(calls.iter().filter_map(|c| c.strip_prefix("show ")?.split(' ').next())
            .all(|name| unit_kind(name) != Ok(UnitKind::Template)), "{calls:?}");
        for name in ["getty@duduclaw-migration-probe.service", "apport-coredump-hook@duduclaw-migration-probe.service",
            "duduclaw-worker@duduclaw-migration-probe.service", "getty@tty1.service", "duduclaw-worker@1.service",
            "unrelated.service", "duduclaw-gateway.service"] {
            assert!(names.contains(&name), "{name} not inspected");
        }
        // A genuine `show` failure — plain, instance or template probe — refuses.
        for fail in ["unrelated.service", "getty@tty1.service", "getty@duduclaw-migration-probe.service"] {
            let failing = FakeHost { fail: Some(fail), ..host.clone() };
            let mut calls = Vec::new();
            assert_eq!(units_with(&plan, fake(&failing, &mut calls)), Err(Error::Unavailable), "{fail}");
        }
        // A malformed name in a listing refuses before any `show`.
        let malformed = FakeHost { files: "@tty1.service enabled enabled\n", ..FakeHost::default() };
        let mut calls = Vec::new();
        assert_eq!(units_with(&plan, fake(&malformed, &mut calls)), Err(Error::Invalid));
        assert!(shown(&calls).is_empty());
    }
    #[test]
    fn fixed_user_legacy_template_is_masked_by_name_blocked_only_after_readback_and_unmasked_on_restore() {
        let plan = test_plan(997);
        let mut host = FakeHost {
            files: "legacy@.service disabled enabled\nlegacy-kick@.timer static -\nduduclaw-gateway.service enabled enabled\n",
            answers: &[("legacy@duduclaw-migration-probe.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                ("legacy-kick@duduclaw-migration-probe.timer", "User=\nTriggers=legacy@duduclaw-migration-probe.service\nLoadState=loaded\n")],
            ..FakeHost::default() };
        let mut calls = Vec::new();
        let selected = units_with(&plan, fake(&host, &mut calls)).unwrap();
        // The fixed-user template itself, and the template whose probe triggers it.
        assert_eq!(selected, BTreeSet::from(["duduclaw-gateway.service", "user@997.service", "legacy@.service",
            "legacy-kick@.timer"].map(String::from)));
        let mut calls = Vec::new();
        assert_eq!(premasked_with(&selected, fake(&host, &mut calls)), Ok(BTreeSet::new()));
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, selected.clone(), fake(&host, &mut calls)), Ok(selected.clone()));
        // activation_sources_blocked stays false until every template's probe reads masked.
        host.masked = vec!["duduclaw-gateway.service", "user@997.service"];
        let mut calls = Vec::new();
        assert_eq!(masked_with(&selected, fake(&host, &mut calls)), Ok(false));
        host.masked.push("legacy@duduclaw-migration-probe.service");
        let mut calls = Vec::new();
        assert_eq!(masked_with(&selected, fake(&host, &mut calls)), Ok(false));
        // An interrupted stop (one template masked, its trigger not yet) still validates.
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, selected.clone(), fake(&host, &mut calls)), Ok(selected.clone()));
        host.masked.push("legacy-kick@duduclaw-migration-probe.timer");
        let mut calls = Vec::new();
        assert_eq!(masked_with(&selected, fake(&host, &mut calls)), Ok(true));
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, selected.clone(), fake(&host, &mut calls)), Ok(selected.clone()));
        // Restore unmasks the template names; a unit masked before the migration stays masked.
        let premasked = BTreeSet::from(["duduclaw-gateway.service".to_string()]);
        let mut calls = Vec::new();
        let unmasked = unmask_with(&selected, &premasked, fake(&host, &mut calls)).unwrap();
        assert_eq!(unmasked, ["legacy-kick@.timer", "legacy@.service", "user@997.service"].map(String::from));
        assert_eq!(calls, ["unmask legacy-kick@.timer", "unmask legacy@.service", "unmask user@997.service"].map(String::from));
    }
    #[test]
    fn instance_derived_template_is_never_masked_only_its_legacy_instances() {
        let plan = test_plan(997);
        let host = FakeHost {
            files: "user@.service static -\nprefixed@.service static -\nunescaped@.service static -\nduduclaw-gateway.service enabled enabled\n",
            loaded: "user@1000.service loaded active running User Manager for UID 1000\n",
            answers: &[
                // `User=%i` echoes the probe (measured on systemd 249 and 255).
                ("user@duduclaw-migration-probe.service", "User=duduclaw-migration-probe\nTriggers=\nLoadState=loaded\n"),
                ("user@duduclaw.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                ("user@997.service", "User=997\nTriggers=\nLoadState=loaded\n"),
                ("user@1000.service", "User=1000\nTriggers=\nLoadState=loaded\n"),
                // `User=svc-%i`: no instance resolves to the legacy account.
                ("prefixed@duduclaw-migration-probe.service", "User=svc-duduclaw-migration-probe\nTriggers=\nLoadState=loaded\n"),
                // `User=%I`: the unescaped probe.
                ("unescaped@duduclaw-migration-probe.service", "User=duduclaw/migration/probe\nTriggers=\nLoadState=loaded\n"),
                ("unescaped@duduclaw.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                // A drop-in gives this instance another user: not selected.
                ("unescaped@997.service", "User=other\nTriggers=\nLoadState=loaded\n"),
            ],
            ..FakeHost::default() };
        let mut calls = Vec::new();
        let selected = units_with(&plan, fake(&host, &mut calls)).unwrap();
        assert_eq!(selected, BTreeSet::from(["duduclaw-gateway.service", "user@997.service", "user@duduclaw.service",
            "unescaped@duduclaw.service"].map(String::from)));
        assert!(!selected.iter().any(|name| unit_kind(name) == Ok(UnitKind::Template)), "{selected:?}");
        let names = shown(&calls);
        for name in ["user@duduclaw.service", "user@997.service", "unescaped@duduclaw.service", "unescaped@997.service"] {
            assert!(names.contains(&name), "{name} not inspected");
        }
        assert!(!names.iter().any(|name| name.starts_with("prefixed@") && !name.contains(PROBE)), "{names:?}");
        // The saved set holds the masked instances, never the template.
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, selected.clone(), fake(&host, &mut calls)), Ok(selected.clone()));
    }
    #[test]
    fn template_with_another_fixed_user_or_root_is_untouched() {
        let plan = test_plan(997);
        let host = FakeHost {
            files: "www@.service static -\nrooted@.service static -\nnumeric@.service static -\nduduclaw-gateway.service enabled enabled\n",
            loaded: "gone.service not-found inactive dead gone.service\n",
            answers: &[("www@duduclaw-migration-probe.service", "User=www-data\nTriggers=\nLoadState=loaded\n"),
                ("numeric@duduclaw-migration-probe.service", "User=1000\nTriggers=\nLoadState=loaded\n"),
                ("gone.service", "User=\nTriggers=\nLoadState=not-found\n")],
            ..FakeHost::default() };
        let mut calls = Vec::new();
        let selected = units_with(&plan, fake(&host, &mut calls)).unwrap();
        assert_eq!(selected, BTreeSet::from(["duduclaw-gateway.service", "user@997.service"].map(String::from)));
        // Only the probes: no instance of these templates is generated.
        assert_eq!(shown(&calls), ["duduclaw-gateway.service", "gone.service", "numeric@duduclaw-migration-probe.service",
            "rooted@duduclaw-migration-probe.service", "www@duduclaw-migration-probe.service"]);
    }
    #[test]
    fn template_probe_failure_unknown_load_state_or_unparseable_answer_refuses() {
        let plan = test_plan(997);
        let base = FakeHost { files: "legacy@.service disabled enabled\nduduclaw-gateway.service enabled enabled\n", ..FakeHost::default() };
        let failing = FakeHost { fail: Some("legacy@duduclaw-migration-probe.service"), ..base.clone() };
        let mut calls = Vec::new();
        assert_eq!(units_with(&plan, fake(&failing, &mut calls)), Err(Error::Unavailable));
        for answer in [
            "User=duduclaw\nTriggers=\nLoadState=not-found\n", // a listed template must resolve to its file
            "User=duduclaw\nTriggers=\nLoadState=error\n",
            "User=duduclaw\nTriggers=\nLoadState=\n",
            "User=duduclaw\nTriggers=\n",
            "User=duduclaw\nUser=root\nLoadState=loaded\n",
            "User=duduclaw\nLoadState=loaded\nFragmentPath=/etc/systemd/system/legacy@.service\n",
            "garbage\n",
        ] {
            let host = FakeHost { answers: Box::leak(Box::new([("legacy@duduclaw-migration-probe.service", answer)])), ..base.clone() };
            let mut calls = Vec::new();
            assert_eq!(units_with(&plan, fake(&host, &mut calls)), Err(Error::Invalid), "{answer:?}");
        }
    }
    #[test]
    fn stock_templates_are_probed_without_failure_and_none_is_masked() {
        let plan = test_plan(997);
        let host = FakeHost {
            files: "getty@.service enabled enabled\nuser@.service static -\nsystemd-fsck@.service static -\n\
                serial-getty@.service disabled enabled\napport-coredump-hook@.service static -\nautovt@.service alias -\n",
            loaded: "getty@tty1.service loaded active running Getty on tty1\n\
                user@1000.service loaded active running User Manager for UID 1000\n",
            answers: &[("user@duduclaw-migration-probe.service", "User=duduclaw-migration-probe\nTriggers=\nLoadState=loaded\n"),
                ("user@duduclaw.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                ("user@997.service", "User=997\nTriggers=\nLoadState=loaded\n"),
                ("user@1000.service", "User=1000\nTriggers=\nLoadState=loaded\n")],
            ..FakeHost::default() };
        let mut calls = Vec::new();
        let selected = units_with(&plan, fake(&host, &mut calls)).unwrap();
        assert_eq!(selected, BTreeSet::from(["duduclaw-gateway.service", "user@997.service", "user@duduclaw.service"].map(String::from)));
        // Beyond the listing and the probes, only the two legacy user@ instances.
        let generated: Vec<_> = shown(&calls).into_iter().filter(|name| !name.contains(PROBE) && !host.loaded.contains(*name)).collect();
        assert_eq!(generated, ["user@997.service", "user@duduclaw.service"]);
    }
    #[test]
    fn saved_set_accepts_exactly_the_masked_templates_and_instances() {
        let plan = test_plan(997);
        let host = FakeHost {
            files: "legacy@.service disabled enabled\nuser@.service static -\ngetty@.service enabled enabled\n\
                www@.service static -\nduduclaw-gateway.service enabled enabled\n",
            answers: &[("legacy@duduclaw-migration-probe.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                ("user@duduclaw-migration-probe.service", "User=duduclaw-migration-probe\nTriggers=\nLoadState=loaded\n"),
                ("user@duduclaw.service", "User=duduclaw\nTriggers=\nLoadState=loaded\n"),
                ("user@997.service", "User=997\nTriggers=\nLoadState=loaded\n"),
                ("www@duduclaw-migration-probe.service", "User=www-data\nTriggers=\nLoadState=loaded\n")],
            ..FakeHost::default() };
        let mut calls = Vec::new();
        let selected = units_with(&plan, fake(&host, &mut calls)).unwrap();
        assert_eq!(selected, BTreeSet::from(["duduclaw-gateway.service", "user@997.service", "user@duduclaw.service",
            "legacy@.service"].map(String::from)));
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, selected.clone(), fake(&host, &mut calls)), Ok(selected.clone()));
        // Once masked by the stop, the same set still validates.
        let masked = FakeHost { masked: vec!["legacy@duduclaw-migration-probe.service", "user@duduclaw.service",
            "user@997.service", "duduclaw-gateway.service"], ..host.clone() };
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, selected.clone(), fake(&masked, &mut calls)), Ok(selected.clone()));
        // Any other template refuses: instance-derived, root, another fixed user, unlisted.
        for other in ["user@.service", "getty@.service", "www@.service", "absent@.service"] {
            let mut saved = selected.clone();
            saved.insert(other.to_string());
            let mut calls = Vec::new();
            assert_eq!(saved_units_with(&plan, saved, fake(&host, &mut calls)), Err(Error::Invalid), "{other}");
        }
        let mut without_gateway = selected.clone();
        without_gateway.remove(GATEWAY);
        let mut calls = Vec::new();
        assert_eq!(saved_units_with(&plan, without_gateway, fake(&host, &mut calls)), Err(Error::Invalid));
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
    #[ignore = "iso_root_: root/systemd on a disposable VM (DUDUCLAW_ISO_DISPOSABLE=1); creates a temporary system account and template unit"]
    fn iso_root_fixed_user_template_is_masked_and_restored() {
        super::super::root().unwrap();
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"), "disposable VM only");
        let uid = 199_993;
        let account = "ddc-iso-tmpl";
        assert!(uid_processes(uid).unwrap().is_empty(), "test uid already in use");
        // Scratch state directory: emptied file by file below, never recursively.
        let state = tempfile::Builder::new().prefix("ddc-iso-tmpl-").tempdir_in("/root").unwrap().keep();
        let plan = RootPlan { migration: duduclaw_isolation::ids::RequestId([6; 16]), state_dir: state.clone(),
            legacy_home: state.join("unused-home"), root_key_path: state.join("unused-key"), gateway_uid: uid,
            gateway_gid: uid, snapshot_content_limit: 1024, snapshot_encoded_limit: 4096, expires_at_unix: 2_000_000_000 };
        for (database, key) in [("passwd", account), ("passwd", "199993"), ("group", account)] {
            assert!(!command(&plan, "getent", &[database, key]).unwrap().0, "{database} {key} already exists");
        }
        let prefix = format!("ddc-iso-tmpl-{}", std::process::id());
        let template = format!("{prefix}@.service");
        let instance = format!("{prefix}@x.service");
        let unit_file = Path::new("/etc/systemd/system").join(&template);
        assert!(fs::symlink_metadata(&unit_file).is_err(), "unit file already exists");
        checked(&plan, "useradd", &["--system", "--uid", "199993", "--user-group", "--no-create-home",
            "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin", account]).unwrap();
        // A numeric User= is compared with the plan's uid exactly as the legacy
        // account's (`USER` is the fixed production name and is not created here).
        let text = format!("[Unit]\nDescription=DuDuClaw iso root test fixed-user template\n[Service]\nType=simple\nUser={uid}\nExecStart=/usr/bin/sleep 300\n");
        let result = (|| -> Result<(), Error> {
            fs::write(&unit_file, &text)?;
            fs::set_permissions(&unit_file, fs::Permissions::from_mode(0o644))?;
            checked(&plan, "systemctl", &["daemon-reload"])?;
            let selected = record(&plan)?;
            if !selected.contains(&template) { return Err(Error::Readback); }
            // Only the template is masked here: the other selected names (the
            // legacy gateway and user@ units) are not this test's to change.
            mask_unit(&plan, &template)?;
            checked(&plan, "systemctl", &["daemon-reload"])?;
            if !masked_with(&BTreeSet::from([template.clone()]), |args| checked(&plan, "systemctl", args))? { return Err(Error::Readback); }
            if command(&plan, "systemctl", &["start", &instance])?.0 { return Err(Error::Readback); }
            restore(&plan)?;
            let probe = checked(&plan, "systemctl", &["show", &probe_name(&template)?, "--property=LoadState"])?;
            if !probe.lines().any(|l| l == "LoadState=loaded") || fs::read_to_string(&unit_file)? != text { return Err(Error::Readback); }
            Ok(())
        })();
        let _ = command(&plan, "systemctl", &["stop", &instance]);
        let _ = command(&plan, "systemctl", &["reset-failed", &instance]);
        // Exactly the files this test created (the unit file, or the mask left in
        // its place by a failed restore, and the stop's records), one by one.
        for path in [unit_file.clone(), state.join("legacy-units").join(&template), state.join("stopped-units.json"), state.join(PREMASKED)] {
            if fs::symlink_metadata(&path).is_ok() { checked(&plan, "rm", &["--", path.to_str().unwrap()]).unwrap(); }
        }
        checked(&plan, "systemctl", &["daemon-reload"]).unwrap();
        checked(&plan, "userdel", &[account]).unwrap();
        // userdel removes the user group itself where USERGROUPS_ENAB is set.
        let _ = command(&plan, "groupdel", &[account]);
        if state.join("legacy-units").try_exists().unwrap() { fs::remove_dir(state.join("legacy-units")).unwrap(); }
        fs::remove_dir(&state).unwrap();
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
