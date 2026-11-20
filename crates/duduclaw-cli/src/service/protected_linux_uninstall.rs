//! Coordinated removal. Keep identities, trust, data and recovery generations.
//!
//! R8z-8 order: write the persistent masks (fixed units and templates; never
//! spawnd's transient runs and per-employee slices, which an `/etc` mask does
//! not reach), `daemon-reload`, stop EVERY unit inside out (activation
//! sources, gateway and updater, employee runs, other services, per-employee
//! slices, employees/TCB slices, `duduclaw.slice`), then prove: no new unit,
//! each fixed unit masked, each unit inactive (bounded wait), no process, no
//! mount. A refusal names the first unit that blocked the proof and leaves
//! every other unit stopped and masked; a retry repeats every step.
use super::*;
use std::{collections::BTreeSet, io::Read, path::PathBuf, process::Output, time::{Duration, Instant}};

/// R8z-8: how long the proof waits for every stopped unit to read inactive.
const PROOF_WAIT: Duration = Duration::from_secs(60);
const PROOF_STEP: Duration = Duration::from_millis(250);

pub(super) fn read_bounded(path: &Path, limit: u64) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut text = String::new();
    Read::by_ref(&mut file).take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit { return Err(refused("移除檢查資料超過上限。")); }
    Ok(text)
}

/// R8z-8: the systemd side of the removal (test seam; production: [`NativeUnits`]).
trait Units {
    /// `systemctl <args>`; the exit status is not checked here.
    fn systemctl(&mut self, args: &[&str]) -> Result<Output>;
    /// The persistent `/etc` mask of a fixed unit or template ([`mask_unit`]).
    fn mask(&mut self, unit: &str) -> Result<()>;
    /// Whether `/etc/systemd/system/<unit>` is the persistent mask.
    fn mask_present(&mut self, unit: &str) -> Result<bool>;
    fn now(&mut self) -> Instant;
    fn pause(&mut self, pause: Duration);
}

struct NativeUnits;
impl Units for NativeUnits {
    fn systemctl(&mut self, args: &[&str]) -> Result<Output> { command("systemctl", args) }
    fn mask(&mut self, unit: &str) -> Result<()> { mask_unit(unit) }
    fn mask_present(&mut self, unit: &str) -> Result<bool> {
        let path = Path::new("/etc/systemd/system").join(unit);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
            Ok(_) => persistent_mask(&path),
        }
    }
    fn now(&mut self) -> Instant { Instant::now() }
    fn pause(&mut self, pause: Duration) { std::thread::sleep(pause) }
}

/// A refusal's text without the outer prefix (the caller adds it once).
fn reason(error: DuDuClawError) -> String {
    match error {
        DuDuClawError::Config(text) => match text.strip_prefix("Protected Linux service refused: ") {
            Some(inner) => inner.to_owned(),
            None => text,
        },
        other => other.to_string(),
    }
}

fn systemctl_checked(units: &mut impl Units, args: &[&str]) -> Result<Output> {
    checked_with("systemctl", args, &mut |_: &str, args: &[&str]| units.systemctl(args))
}

fn unit_inventory(units: &mut impl Units) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for args in [vec!["list-unit-files", "--no-legend", "--no-pager", "--type=service,socket,timer,slice"],
        vec!["list-units", "--all", "--plain", "--no-legend", "--no-pager", "--type=service,socket,timer,slice"]] {
        let bytes = systemctl_checked(units, &args)?.stdout;
        if bytes.len() > 4 * 1024 * 1024 { return Err(refused("服務清單超過上限。")); }
        let text = String::from_utf8(bytes).map_err(|_| refused("無法讀取服務清單。"))?;
        found.extend(lifecycle::inventory(&text).map_err(refused)?);
    }
    Ok(found)
}

/// R8z-8: persistent masks for every unit a mask reaches, then one
/// `daemon-reload` (also after a failed mask: those written apply to the stop
/// and the proof). Nothing is proven here.
fn fence_units(units: &mut impl Units, names: &[String]) -> std::result::Result<(), String> {
    let mut failed = None;
    for name in names.iter().filter(|name| !lifecycle::runtime_unit(name)) {
        if let Err(error) = units.mask(name) {
            failed = Some(format!("無法封鎖 {name}：{}", reason(error)));
            break;
        }
    }
    let reloaded = systemctl_checked(units, &["daemon-reload"]).map(drop).map_err(reason);
    match failed { Some(message) => Err(message), None => reloaded }
}

/// R8z-8: every unit is asked to stop, inside out ([`lifecycle::stop_order`]).
/// A stop is not judged here: a run that already exited is no longer loaded
/// (its `stop` fails) and a unit that does not stop is named by the proof.
fn stop_units(units: &mut impl Units, names: &[String]) -> std::result::Result<(), String> {
    for name in lifecycle::stop_order(names) {
        // Templates denote no running unit; their instances are listed by name.
        if lifecycle::template_unit(&name) { continue; }
        let _ = units.systemctl(&["stop", name.as_str()]);
    }
    Ok(())
}

/// A systemd state word: lowercase letters and `-`.
fn state_word(text: &str) -> bool { !text.is_empty() && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') }

/// `(LoadState, ActiveState)` of `unit` as PID 1 reports it now.
fn unit_state(units: &mut impl Units, unit: &str) -> Result<(String, String)> {
    let output = systemctl_checked(units, &["show", unit, "--property=ActiveState,LoadState"])?;
    let text = String::from_utf8(output.stdout).map_err(|_| refused("無效的服務狀態。"))?;
    let value = |key: &str| text.lines().find_map(|line| line.strip_prefix(key)).filter(|value| state_word(value)).map(str::to_owned);
    match (value("LoadState="), value("ActiveState=")) {
        (Some(load), Some(active)) => Ok((load, active)),
        _ => Err(refused("無效的服務狀態。")),
    }
}

/// What the proof found first, in stop order.
enum Blocker { Unmasked(String, String), Active(String, String) }

fn first_blocker(units: &mut impl Units, names: &[String]) -> Result<Option<Blocker>> {
    for name in lifecycle::stop_order(names) {
        if lifecycle::template_unit(&name) {
            if !units.mask_present(&name)? { return Ok(Some(Blocker::Unmasked(name, "範本在 /etc 沒有 mask".into()))); }
            continue;
        }
        let (load, active) = unit_state(units, &name)?;
        // A transient unit cannot be masked; it is fenced by its masked parent
        // slice and its masked, stopped creator (spawnd), and must be inactive
        // (or already gone: `not-found`).
        if !lifecycle::runtime_unit(&name) && load != "masked" {
            return Ok(Some(Blocker::Unmasked(name, format!("LoadState={load}"))));
        }
        if !matches!(active.as_str(), "inactive" | "failed") { return Ok(Some(Blocker::Active(name, active))); }
    }
    Ok(None)
}

/// R8z-8: the live unit proof after every stop. No unit may have appeared;
/// every fixed unit and template is masked; every unit reads inactive within
/// `wait`. A refusal names the first unit that blocked it.
fn prove_units(units: &mut impl Units, names: &[String], wait: Duration) -> std::result::Result<(), String> {
    let deadline = units.now() + wait;
    loop {
        let current = unit_inventory(units).map_err(reason)?;
        if let Some(new) = current.iter().find(|unit| !names.contains(*unit)) {
            return Err(format!("移除期間出現新的服務 {new}；保留封鎖並停止移除，未移除執行檔。請確認它的來源後再執行同一個移除命令。"));
        }
        match first_blocker(units, names).map_err(reason)? {
            None => return Ok(()),
            Some(Blocker::Unmasked(unit, detail)) => return Err(format!(
                "無法證明服務啟動已封鎖：{unit} 未被封鎖（{detail}）；其他服務已停止並保持封鎖，未移除執行檔。")),
            Some(Blocker::Active(unit, active)) => {
                if units.now() >= deadline {
                    return Err(format!("無法證明服務已停止：{unit} 仍未停止（ActiveState={active}）；其他服務已停止並保持封鎖，未移除執行檔。請先停止它（sudo systemctl stop {unit}），確認已停止後再執行同一個移除命令。"));
                }
                units.pause(PROOF_STEP);
            }
        }
    }
}

fn removal_files() -> Vec<PathBuf> {
    // Persistent floors, keys, identity tombstones, update journal and generation
    // receipts deliberately survive. They are needed for recovery and anti-rollback.
    // Review R8q F2: the parse worker's tmpfiles configuration is not listed
    // here (a trusted shape does not make it ours); the parse lease step
    // removes it only when its content is a known DuDuClaw text.
    let mut files: Vec<_> = layout::BINARIES.iter().copied().chain([BINARY, LAUNCHER,
        "/usr/local/sbin/duduclaw-auto-upgrade"]).map(PathBuf::from).collect();
    for name in layout::UNITS.iter().map(|(name, _)| *name).chain(lifecycle::EXTRA_UNITS.iter().copied()) {
        files.push(Path::new("/etc/systemd/system").join(name));
    }
    files
}

fn validate_removals(files: &[PathBuf]) -> Result<()> {
    for path in files {
        // Check every existing ancestor, including when the final name is absent.
        // A failed partial removal cannot turn a symlink into a new destination.
        for parent in path.parent().ok_or_else(|| refused("無效的移除路徑。"))?.ancestors() {
            match fs::symlink_metadata(parent) {
                Ok(_) => trusted(parent, true)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if persistent_mask(path)? { continue; }
                trusted(path, false)?;
                if meta.nlink() != 1 { return Err(refused("移除目標含有硬連結。")); }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn persistent_mask(path: &Path) -> Result<bool> {
    if path.parent() != Some(Path::new("/etc/systemd/system"))
        || !path.file_name().and_then(|s| s.to_str()).is_some_and(lifecycle::owned_unit) { return Ok(false); }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_symlink() { return Ok(false); }
    if meta.uid() != 0 || meta.gid() != 0 || fs::read_link(path)? != Path::new("/dev/null") {
        return Err(refused("非預期的服務封鎖連結。"));
    }
    Ok(true)
}

fn mask_unit(unit: &str) -> Result<()> {
    mkdir("/var/lib/duduclaw-protected/uninstall-units", 0o700)?;
    lifecycle::fence_unit_files(Path::new("/"), 0, 0, unit)?;
    Ok(())
}

fn service_uids(deadline: Option<std::time::Instant>) -> Result<BTreeSet<u32>> {
    let mut uids = BTreeSet::new();
    for name in std::iter::once("duduclaw").chain(layout::ACCOUNTS.iter().copied()) {
        let output = match deadline {
            Some(deadline) => command_within("getent", &["passwd", name], deadline)?,
            None => command("getent", &["passwd", name])?,
        };
        if output.status.code() == Some(2) { continue; }
        if !output.status.success() { return Err(refused("無法確認服務身分。")); }
        let text = String::from_utf8(output.stdout).map_err(|_| refused("無效的帳號資料。"))?;
        let parts: Vec<_> = text.trim_end_matches('\n').split(':').collect();
        if parts.len() != 7 || parts[0] != name { return Err(refused("無效的帳號資料。")); }
        let uid = parts[2].parse::<u32>().map_err(|_| refused("無效的帳號身分。"))?;
        if uid == 0 { return Err(refused("服務帳號不能是 root。")); }
        uids.insert(uid);
    }
    Ok(uids)
}

pub(super) fn processes_clear() -> Result<()> { processes_clear_until(None) }

/// The same scan bounded by an absolute `deadline` (v1.83 R4 item 4): every
/// account lookup gets `min(30 s, what is left)` and no command is spawned and
/// no `/proc` entry read once it has passed (refused).
pub(super) fn processes_clear_until(deadline: Option<std::time::Instant>) -> Result<()> {
    let expired = || refused("程序檢查超過時限。");
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) { return Err(expired()); }
    let uids = service_uids(deadline)?;
    let mut count = 0;
    for entry in fs::read_dir("/proc")? {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) { return Err(expired()); }
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue; };
        if pid == std::process::id() { continue; }
        count += 1;
        if count > 100_000 { return Err(refused("程序檢查超過上限。")); }
        let status = match read_bounded(&entry.path().join("status"), 64 * 1024) {
            Ok(value) => value,
            Err(error) if entry.path().try_exists().is_ok_and(|exists| !exists) => { let _ = error; continue; }
            Err(error) => return Err(error),
        };
        let uid_line = status.lines().find_map(|line| line.strip_prefix("Uid:"))
            .ok_or_else(|| refused("無法確認程序身分。"))?;
        let ids: Vec<_> = uid_line.split_whitespace().map(str::parse::<u32>).collect::<std::result::Result<_, _>>()
            .map_err(|_| refused("無效的程序身分。"))?;
        if ids.len() != 4 { return Err(refused("無效的程序身分。")); }
        if ids.iter().any(|uid| uids.contains(uid) || (70000..=99999).contains(uid)) {
            return Err(refused("仍有服務或員工程序，未移除執行檔。"));
        }
        match fs::read_link(entry.path().join("exe")) {
            Ok(path) => {
                if ["/opt/duduclaw-isolated", "/opt/duduclaw", "/usr/lib/duduclaw", "/usr/libexec/duduclaw",
                    "/usr/libexec/duduclaw-updated-launch", "/usr/local/libexec/duduclaw"]
                    .iter().any(|root| path.starts_with(root)) {
                    return Err(refused("仍有隔離元件正在執行，未移除執行檔。"));
                }
            }
            // Kernel threads and zombies have no executable. Their UIDs were
            // checked above; any unreadable live executable is a refusal.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct NativeRemoval {
    files: Vec<PathBuf>,
    /// R8z-8: spawnd's transient names in the inventory (never masked here).
    runtime: Vec<String>,
    units: NativeUnits,
}
impl lifecycle::RemovalHost for NativeRemoval {
    fn fence(&mut self, units: &[String]) -> std::result::Result<(), String> { fence_units(&mut self.units, units) }
    fn stop(&mut self, units: &[String]) -> std::result::Result<(), String> { stop_units(&mut self.units, units) }
    fn quiescent(&mut self, units: &[String]) -> std::result::Result<(), String> {
        prove_units(&mut self.units, units, PROOF_WAIT)?;
        let verify = || -> Result<()> {
            for root in ["/sys/fs/cgroup/duduclaw.slice", "/sys/fs/cgroup/system.slice/duduclaw-gateway.service"] {
                let path = Path::new(root).join("cgroup.events");
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                    Ok(_) => if !lifecycle::cgroup_empty(&read_bounded(&path, 4096)?).map_err(refused)? {
                        return Err(refused("服務控制群組仍有程序。"));
                    },
                }
            }
            processes_clear()?;
            if !lifecycle::mounts_clear(&read_bounded(Path::new("/proc/self/mountinfo"), 16 * 1024 * 1024)?).map_err(refused)? {
                return Err(refused("仍有隔離掛載；請先透過儲存服務清理，未強制卸載或刪除資料。"));
            }
            Ok(())
        };
        verify().map_err(reason)
    }
    fn remove(&mut self) -> std::result::Result<(), String> {
        let remove = || -> Result<()> {
            validate_removals(&self.files)?;
            // Review R8q F2: after quiescence (lifecycle order) and before any
            // other removal. It checks the configuration's content and lists
            // the whole runtime tree first; a refusal removes nothing at all.
            parse_lease::remove()?;
            for file in &self.files {
                // Keep persistent masks so a vendor unit or still-present
                // enablement link cannot resurrect a retained recovery binary.
                if fs::symlink_metadata(file).is_ok() && persistent_mask(file)? { continue; }
                match fs::remove_file(file) {
                    Ok(()) => fs::File::open(file.parent().unwrap())?.sync_all()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
            }
            // R8z-8: an older build's refused removal left `/etc` masks at
            // spawnd's transient names; PID 1 would refuse that employee's
            // runs after a reinstall. Only a root-owned link to /dev/null at an
            // exact transient name (`lifecycle::runtime_unit`) is removed; any
            // other object there is left untouched.
            for name in &self.runtime {
                let path = Path::new("/etc/systemd/system").join(name);
                if fs::symlink_metadata(&path).is_ok() && matches!(persistent_mask(&path), Ok(true)) {
                    fs::remove_file(&path)?;
                    fs::File::open("/etc/systemd/system")?.sync_all()?;
                }
            }
            checked("systemctl", &["daemon-reload"])?;
            Ok(())
        };
        remove().map_err(reason)
    }
}

pub(super) fn run() -> Result<()> {
    let files = removal_files();
    validate_removals(&files)?;
    // Review R8q F2: an unrecognised parse configuration refuses before any
    // unit is fenced or stopped (repeated in the removal step itself).
    parse_lease::config_known()?;
    let mut native = NativeUnits;
    let units: Vec<_> = unit_inventory(&mut native)?.into_iter().collect();
    mkdir("/var/lib/duduclaw-protected/uninstall-units", 0o700)?;
    let runtime = units.iter().filter(|unit| lifecycle::runtime_unit(unit)).cloned().collect();
    let mut host = NativeRemoval { files, runtime, units: native };
    lifecycle::remove_installation(&mut host, &units).map_err(refused)?;
    println!("已停止並移除隔離服務與固定啟動器，服務保持封鎖。資料、員工身分紀錄、金鑰、更新信任、單元備份與復原世代皆保留；未刪除員工磁碟。");
    Ok(())
}

#[cfg(test)]
mod root_tests {
    use super::*;
    #[test]
    #[ignore = "requires a disposable native Linux VM with the complete trusted isolation fixture; removes its services"]
    fn iso_root_install_uninstall_preserves_recovery_and_is_repeatable() {
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
        gate().unwrap();
        assert!(layout::published_components(Path::new("/"), 0, 0).unwrap());
        let key = Path::new("/var/lib/duduclaw-protected/root.key");
        let original = fs::read(key).unwrap();
        run().unwrap();
        run().unwrap();
        // Do not print root-key bytes in an assertion failure.
        assert!(fs::read(key).unwrap() == original);
        for binary in layout::BINARIES { assert!(!Path::new(binary).exists()); }
        assert!(Path::new("/var/lib/duduclaw-protected/uninstall-units").is_dir());
    }
}

/// R8z-8: the removal's unit steps against a fake PID 1 (`Units`). Transient
/// units (spawnd's runs and per-employee slices) behave as on the real host:
/// an `/etc` mask does not reach them, and a stopped one is collected.
#[cfg(test)]
mod r8z8_tests {
    use super::*;
    use std::{collections::BTreeMap, os::unix::process::ExitStatusExt, process::ExitStatus};

    const SLICE: &str = "duduclaw-employees-0123456789abcdef0123456789abcdef.slice";
    const RUN: &str = "duduclaw-run-fedcba9876543210fedcba9876543210.service";
    /// The protected install of the real-host evidence (all `loaded`).
    const FIXED: [(&str, &str, &str); 13] = [
        ("duduclaw-auto-upgrade.service", "static", "inactive"), ("duduclaw-auto-upgrade.timer", "enabled", "active"),
        ("duduclaw-egress.service", "enabled", "active"), ("duduclaw-egress.socket", "enabled", "active"),
        ("duduclaw-employees.slice", "static", "active"), ("duduclaw-gateway.service", "enabled", "active"),
        ("duduclaw-isolation-ready.service", "static", "active"), ("duduclaw-parse.socket", "enabled", "active"),
        ("duduclaw-spawnd.service", "enabled", "active"), ("duduclaw-spawnd.socket", "enabled", "active"),
        ("duduclaw-tcb.slice", "static", "active"), ("duduclaw-updated.service", "enabled", "active"),
        ("duduclaw.slice", "static", "active")];
    const TEMPLATE: &str = "duduclaw-parse@.service";

    struct Host {
        /// Unit files (list-unit-files state).
        files: BTreeMap<String, &'static str>,
        /// `/etc/systemd/system/<name>` -> /dev/null.
        masks: BTreeSet<String>,
        /// Loaded units: (LoadState, ActiveState).
        loaded: BTreeMap<String, (String, String)>,
        /// Loaded from /run/systemd/transient (outranks an /etc mask; a test
        /// may list a fixed name here to model a mask PID 1 does not apply).
        transient: BTreeSet<String>,
        /// Units whose stop job fails (they stay active).
        stuck: BTreeSet<String>,
        /// A unit that appears (loaded, active) at the first daemon-reload.
        appears: Option<&'static str>,
        log: Vec<String>,
        start: Instant,
        clock: Duration,
    }

    impl Host {
        fn installed() -> Self {
            let mut host = Host { files: BTreeMap::new(), masks: BTreeSet::new(), loaded: BTreeMap::new(),
                transient: BTreeSet::new(), stuck: BTreeSet::new(), appears: None, log: Vec::new(),
                start: Instant::now(), clock: Duration::ZERO };
            for (name, state, active) in FIXED {
                host.files.insert(name.into(), state);
                host.loaded.insert(name.into(), ("loaded".into(), active.into()));
            }
            host.files.insert(TEMPLATE.into(), "static");
            host
        }
        /// After one employee ran: its slice active, one exited run unit still loaded.
        fn employee_ran() -> Self {
            let mut host = Self::installed();
            for (name, active) in [(SLICE, "active"), (RUN, "inactive")] {
                host.files.insert(name.into(), "transient");
                host.transient.insert(name.into());
                host.loaded.insert(name.into(), ("loaded".into(), active.into()));
            }
            host
        }
        fn unit_files(&self) -> String {
            let names: BTreeSet<&String> = self.files.keys().chain(self.masks.iter()).collect();
            names.into_iter().map(|name| {
                let state = if self.transient.contains(name) { "transient" }
                    else if self.masks.contains(name) { "masked" } else { self.files[name] };
                format!("{name} {state} -\n")
            }).collect()
        }
        fn units(&self) -> String {
            self.loaded.iter().map(|(name, (load, active))| {
                format!("{name} {load} {active} {} x\n", if active == "active" { "running" } else { "dead" })
            }).collect()
        }
        fn reload(&mut self) {
            if let Some(name) = self.appears.take() {
                self.files.insert(name.into(), "static");
                self.loaded.insert(name.into(), ("loaded".into(), "active".into()));
            }
            for (name, (load, _)) in self.loaded.iter_mut() {
                if self.masks.contains(name) && !self.transient.contains(name) { *load = "masked".into(); }
            }
        }
        fn stop(&mut self, unit: &str) -> i32 {
            let parent_of_stuck = matches!(unit, "duduclaw-employees.slice" | "duduclaw.slice")
                && self.stuck.iter().any(|name| lifecycle::runtime_unit(name));
            if self.stuck.contains(unit) || parent_of_stuck { return 1; }
            if !self.loaded.contains_key(unit) { return 5; }
            // A stopped transient run or per-employee slice is collected.
            if lifecycle::runtime_unit(unit) && self.transient.remove(unit) {
                self.loaded.remove(unit);
                self.files.remove(unit);
            } else if let Some((_, active)) = self.loaded.get_mut(unit) {
                *active = "inactive".into();
            }
            0
        }
        fn show(&self, unit: &str) -> String {
            let (load, active) = match self.loaded.get(unit) {
                Some((load, active)) => (load.as_str(), active.as_str()),
                None if self.masks.contains(unit) => ("masked", "inactive"),
                None => ("not-found", "inactive"),
            };
            format!("ActiveState={active}\nLoadState={load}\n")
        }
        /// The unit named by every `<verb> <unit>` entry, in order.
        fn named(&self, verb: &str) -> Vec<String> {
            self.log.iter().filter_map(|line| line.strip_prefix(verb)).map(str::to_owned).collect()
        }
    }

    impl Units for Host {
        fn systemctl(&mut self, args: &[&str]) -> Result<Output> {
            self.log.push(format!("systemctl {}", args.join(" ")));
            let (code, stdout) = match args {
                ["list-unit-files", ..] => (0, self.unit_files()),
                ["list-units", ..] => (0, self.units()),
                ["daemon-reload"] => { self.reload(); (0, String::new()) }
                ["stop", unit] => (self.stop(unit), String::new()),
                ["show", unit, "--property=ActiveState,LoadState"] => (0, self.show(unit)),
                _ => (1, String::new()),
            };
            Ok(Output { status: ExitStatus::from_raw(code << 8), stdout: stdout.into_bytes(), stderr: Vec::new() })
        }
        fn mask(&mut self, unit: &str) -> Result<()> {
            self.log.push(format!("mask {unit}"));
            self.masks.insert(unit.into());
            Ok(())
        }
        fn mask_present(&mut self, unit: &str) -> Result<bool> { Ok(self.masks.contains(unit)) }
        fn now(&mut self) -> Instant { self.start + self.clock }
        fn pause(&mut self, pause: Duration) { self.clock += pause; }
    }

    struct Removal<'a> { host: &'a mut Host }
    impl lifecycle::RemovalHost for Removal<'_> {
        fn fence(&mut self, units: &[String]) -> std::result::Result<(), String> { fence_units(&mut *self.host, units) }
        fn stop(&mut self, units: &[String]) -> std::result::Result<(), String> { stop_units(&mut *self.host, units) }
        fn quiescent(&mut self, units: &[String]) -> std::result::Result<(), String> {
            prove_units(&mut *self.host, units, PROOF_WAIT)
        }
        fn remove(&mut self) -> std::result::Result<(), String> { self.host.log.push("remove".into()); Ok(()) }
    }

    /// `run()`'s unit path: the inventory, then the lifecycle.
    fn uninstall(host: &mut Host) -> std::result::Result<(), String> {
        let units: Vec<String> = unit_inventory(&mut *host).map_err(reason)?.into_iter().collect();
        lifecycle::remove_installation(&mut Removal { host }, &units)
    }

    fn expected_stops(employee: bool) -> Vec<&'static str> {
        let mut stops = vec!["duduclaw-auto-upgrade.timer", "duduclaw-egress.socket", "duduclaw-parse.socket",
            "duduclaw-spawnd.socket", "duduclaw-gateway.service", "duduclaw-updated.service"];
        if employee { stops.push(RUN); }
        stops.extend(["duduclaw-auto-upgrade.service", "duduclaw-egress.service", "duduclaw-isolation-ready.service",
            "duduclaw-spawnd.service"]);
        if employee { stops.push(SLICE); }
        stops.extend(["duduclaw-employees.slice", "duduclaw-tcb.slice", "duduclaw.slice"]);
        stops
    }

    fn expected_masks() -> Vec<&'static str> {
        let mut masks: Vec<&str> = FIXED.iter().map(|(name, _, _)| *name).chain([TEMPLATE]).collect();
        masks.sort();
        masks
    }

    /// Nothing proven before every stop was issued (the R8z-8 defect: the
    /// proof ran first and its refusal skipped every stop).
    fn proof_after_stops(host: &Host) {
        let last_stop = host.log.iter().rposition(|line| line.starts_with("systemctl stop ")).unwrap();
        let first_show = host.log.iter().position(|line| line.starts_with("systemctl show ")).unwrap();
        assert!(last_stop < first_show, "{:?}", host.log);
    }

    #[test]
    fn r8z8_employee_slice_and_exited_run_are_stopped_inside_out_and_removal_completes() {
        let mut host = Host::employee_ran();
        uninstall(&mut host).unwrap();
        assert_eq!(host.named("mask "), expected_masks(), "transient units are never masked");
        assert_eq!(host.named("systemctl stop "), expected_stops(true));
        proof_after_stops(&host);
        assert_eq!(host.log.last().map(String::as_str), Some("remove"));
        assert!(!host.loaded.contains_key(SLICE) && !host.loaded.contains_key(RUN));
        assert!(host.loaded.values().all(|(load, active)| load == "masked" && active == "inactive"));
    }

    #[test]
    fn r8z8_slice_that_does_not_stop_is_named_and_nothing_masked_is_left_running() {
        let mut host = Host::employee_ran();
        host.stuck.insert(SLICE.into());
        let error = uninstall(&mut host).unwrap_err();
        assert!(error.contains(SLICE) && error.contains("仍未停止") && error.contains("未移除執行檔"), "{error}");
        assert!(error.contains(&format!("sudo systemctl stop {SLICE}")), "{error}");
        // Every unit was asked to stop, in order, before the proof; no removal.
        assert_eq!(host.named("systemctl stop "), expected_stops(true));
        proof_after_stops(&host);
        assert!(!host.log.iter().any(|line| line == "remove"), "{:?}", host.log);
        assert!(host.clock >= PROOF_WAIT, "the proof waited its bound");
        // Left: every service, socket and timer stopped and masked; only the
        // named slice (unmasked: transient) and the slices containing it active.
        for (name, (load, active)) in &host.loaded {
            if name == SLICE { assert_eq!((load.as_str(), active.as_str()), ("loaded", "active")); continue; }
            assert_eq!(load, "masked", "{name}");
            if !matches!(name.as_str(), "duduclaw-employees.slice" | "duduclaw.slice") {
                assert_eq!(active, "inactive", "{name}");
            }
        }
    }

    #[test]
    fn r8z8_second_attempt_after_a_refusal_completes() {
        let mut host = Host::employee_ran();
        host.stuck.insert(SLICE.into());
        assert!(uninstall(&mut host).is_err());
        // The cause is gone (the operator stopped the named unit).
        host.stuck.clear();
        host.log.clear();
        uninstall(&mut host).unwrap();
        assert_eq!(host.log.last().map(String::as_str), Some("remove"));
        assert!(host.loaded.values().all(|(load, active)| load == "masked" && active == "inactive"));
        assert!(!host.loaded.contains_key(SLICE));
    }

    /// A host an older build left masked-and-running (masks also at the
    /// transient slice's name): the current removal completes.
    #[test]
    fn r8z8_host_left_masked_and_running_by_an_older_build_completes() {
        let mut host = Host::employee_ran();
        for name in expected_masks().into_iter().chain([SLICE]) { host.masks.insert(name.into()); }
        host.reload();
        assert_eq!(host.loaded[SLICE].0, "loaded", "an /etc mask does not reach a transient unit");
        assert_eq!(host.loaded["duduclaw-gateway.service"], ("masked".to_owned(), "active".to_owned()));
        uninstall(&mut host).unwrap();
        assert_eq!(host.named("systemctl stop "), expected_stops(true));
        assert!(!host.named("mask ").iter().any(|name| name == SLICE || name == RUN));
        assert_eq!(host.log.last().map(String::as_str), Some("remove"));
    }

    #[test]
    fn r8z8_names_outside_the_strict_shapes_are_never_stopped_or_masked() {
        for foreign in ["duduclaw-employees-evil.slice", "duduclaw-run-../../x.service", "duduclaw-foo.service",
            "duduclaw-employees-0123456789ABCDEF0123456789abcdef.slice"] {
            let mut host = Host::employee_ran();
            host.files.insert(foreign.into(), "static");
            host.loaded.insert(foreign.into(), ("loaded".into(), "active".into()));
            let error = uninstall(&mut host).unwrap_err();
            assert!(error.contains(foreign), "{error}");
            assert!(host.log.iter().all(|line| line.starts_with("systemctl list-unit-files")
                || line.starts_with("systemctl list-units")), "{foreign}: {:?}", host.log);
        }
        // One that appears only after the masks: every owned unit is still
        // stopped, the newcomer is never acted on, and the proof refuses by name.
        let mut host = Host::employee_ran();
        host.appears = Some("duduclaw-foo.service");
        let error = uninstall(&mut host).unwrap_err();
        assert!(error.contains("duduclaw-foo.service"), "{error}");
        assert_eq!(host.named("systemctl stop "), expected_stops(true));
        assert!(!host.log.iter().any(|line| line.ends_with("duduclaw-foo.service") && !line.starts_with("systemctl list")));
        assert!(!host.log.iter().any(|line| line == "remove"));
    }

    #[test]
    fn r8z8_no_employee_ran_removal_is_unchanged() {
        let mut host = Host::installed();
        uninstall(&mut host).unwrap();
        assert_eq!(host.named("mask "), expected_masks());
        assert_eq!(host.named("systemctl stop "), expected_stops(false));
        proof_after_stops(&host);
        assert_eq!(host.log.last().map(String::as_str), Some("remove"));
        // Every fixed unit masked and inactive, the template's mask present.
        assert!(host.loaded.values().all(|(load, active)| load == "masked" && active == "inactive"));
        assert!(host.masks.contains(TEMPLATE));
    }

    #[test]
    fn r8z8_a_fixed_unit_the_mask_does_not_reach_is_named() {
        let mut host = Host::installed();
        // Simulate a mask that PID 1 does not apply (as for a transient unit).
        host.transient.insert("duduclaw-egress.service".into());
        let error = uninstall(&mut host).unwrap_err();
        assert!(error.contains("duduclaw-egress.service") && error.contains("未被封鎖"), "{error}");
        assert_eq!(host.named("systemctl stop "), expected_stops(false), "stopped before the refusal");
        assert!(!host.log.iter().any(|line| line == "remove"));
    }
}
