use super::*;
use std::{fs, io::Write, os::unix::{fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt}, io::AsRawFd}, path::Path, process::{Command, Output}};

fn gate() -> Result<()> {
    if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
        return Err(refused("run with sudo as root"));
    }
    duduclaw_core::root_key::native_host_check().map_err(refused)?;
    if duduclaw_core::ns_guard::is_containerized() {
        return Err(refused("use the Docker protected-key launcher in containers"));
    }
    Ok(())
}

// Canonicalization permits distro-owned /bin -> /usr/bin layouts, but each
// resolved component must be root-owned and not writable by another account.
fn trusted(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file()) || metadata.uid() != 0
        || metadata.gid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(refused(format!("unsafe root-managed path: {}", path.display())));
    }
    Ok(())
}

fn parents(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| refused("missing parent"))?;
    for ancestor in parent.ancestors() { trusted(ancestor, true)?; }
    Ok(())
}

fn mkdir(path: &str, mode: u32) -> Result<()> {
    let path = Path::new(path);
    parents(path)?;
    // Do not briefly expose a writable directory when the invoking umask is 0.
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => trusted(path, true)?,
        Err(error) => return Err(error.into()),
    }
    let directory = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    if unsafe { libc::fchown(directory.as_raw_fd(), 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    directory.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn command(name: &str, args: &[&str]) -> Result<Output> {
    let executable = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"].iter()
        .map(|dir| Path::new(dir).join(name)).find(|path| path.exists())
        .ok_or_else(|| refused(format!("required command missing: {name}")))?;
    let executable = fs::canonicalize(executable)?;
    parents(&executable)?;
    trusted(&executable, false)?;
    Ok(Command::new(executable).args(args).env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").env("LC_ALL", "C").output()?)
}

fn checked(name: &str, args: &[&str]) -> Result<Output> {
    let output = command(name, args)?;
    if !output.status.success() {
        return Err(refused(format!("{name} failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }
    Ok(output)
}

fn account() -> Result<(u32, u32)> {
    let mut output = command("getent", &["passwd", "duduclaw"])?;
    if output.status.code() == Some(2) {
        checked("useradd", &["--system", "--home-dir", "/var/lib/duduclaw", "--create-home", "--shell", "/usr/sbin/nologin", "duduclaw"])?;
        output = checked("getent", &["passwd", "duduclaw"])?;
    } else if !output.status.success() { return Err(refused("cannot query duduclaw account")); }
    let text = String::from_utf8(output.stdout).map_err(|_| refused("invalid passwd response"))?;
    let fields: Vec<_> = text.trim_end_matches('\n').split(':').collect();
    if fields.len() != 7 || fields[0] != "duduclaw" || fields[5] != "/var/lib/duduclaw" {
        return Err(refused("duduclaw account must have home /var/lib/duduclaw"));
    }
    let uid = fields[2].parse().map_err(|_| refused("invalid uid"))?;
    let gid = fields[3].parse().map_err(|_| refused("invalid gid"))?;
    let groups = checked("id", &["-Gn", "duduclaw"])?;
    let groups = String::from_utf8(groups.stdout).map_err(|_| refused("invalid groups"))?;
    validate_account(uid, gid, fields[6], &groups)?;
    let home = Path::new("/var/lib/duduclaw");
    parents(home)?;
    let metadata = fs::symlink_metadata(home)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.gid() != gid || metadata.mode() & 0o022 != 0 {
        return Err(refused("service home must be a real directory owned by duduclaw, not group/other writable"));
    }
    Ok((uid, gid))
}

fn atomic_install(path: &str, mode: u32, write: impl FnOnce(&mut fs::File) -> Result<()>) -> Result<()> {
    let path = Path::new(path);
    parents(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => trusted(path, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let parent = path.parent().unwrap();
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    write(temp.as_file_mut())?;
    if unsafe { libc::fchown(temp.as_raw_fd(), 0, 0) } != 0 { return Err(std::io::Error::last_os_error().into()); }
    temp.as_file().set_permissions(fs::Permissions::from_mode(mode))?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

const VERSION_FLOOR: &str = "/var/lib/duduclaw-protected/version-floor";
const VERSION_PENDING: &str = "/var/lib/duduclaw-protected/version-pending";

const SPAWND_BINARY: &str = "/usr/lib/duduclaw/duduclaw-spawnd";
const RUN_BINARY: &str = "/usr/lib/duduclaw/duduclaw-run";
const SPAWND_UNITS: &[(&str, &str)] = &[
    ("duduclaw-spawnd.service", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.service")),
    ("duduclaw-spawnd.socket", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-spawnd.socket")),
    ("duduclaw.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw.slice")),
    ("duduclaw-tcb.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-tcb.slice")),
    ("duduclaw-employees.slice", include_str!("../../../../scripts/linux-protected/spawnd/duduclaw-employees.slice")),
];

// F/G publish the signature-verified binaries first. This hook stages support
// only: it cannot declare readiness or enable a partial isolation generation.
fn install_spawnd_support_if_published() -> Result<()> {
    let present = [SPAWND_BINARY, RUN_BINARY].map(|path| fs::symlink_metadata(path));
    if present.iter().all(|entry| entry.as_ref().is_err_and(|error|
        error.kind() == std::io::ErrorKind::NotFound)) { return Ok(()); }
    for path in [SPAWND_BINARY, RUN_BINARY] {
        parents(Path::new(path))?;
        trusted(Path::new(path), false)?;
        let metadata = fs::metadata(path)?;
        if metadata.mode() & 0o7000 != 0 || metadata.mode() & 0o111 == 0 || metadata.nlink() != 1 {
            return Err(refused("unsafe isolation component"));
        }
    }
    let query = command("getent", &["passwd", "duduclaw-runsup"])?;
    match query.status.code() {
        Some(2) => {
            checked("useradd", &["--system", "--user-group", "--no-create-home",
                "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin",
                "--password", "!*", "--expiredate", "1970-01-02", "duduclaw-runsup"])?;
        }
        Some(0) => (),
        _ => return Err(refused("cannot query run supervisor account")),
    }
    let passwd = checked("getent", &["passwd", "duduclaw-runsup"])?;
    let passwd = String::from_utf8(passwd.stdout).map_err(|_| refused("invalid supervisor account"))?;
    let fields: Vec<_> = passwd.trim_end_matches('\n').split(':').collect();
    if fields.len() != 7 || fields[0] != "duduclaw-runsup" || fields[5] != "/nonexistent"
        || fields[6] != "/usr/sbin/nologin" { return Err(refused("unsafe supervisor account")); }
    let uid: u32 = fields[2].parse().map_err(|_| refused("invalid supervisor uid"))?;
    let gid: u32 = fields[3].parse().map_err(|_| refused("invalid supervisor gid"))?;
    let defs = fs::read_to_string("/etc/login.defs")?;
    let mut sys_max = None;
    for line in defs.lines() {
        let values: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        if values.first() == Some(&"SYS_UID_MAX") {
            if values.len() != 2 || sys_max.is_some() { return Err(refused("invalid system uid range")); }
            sys_max = Some(values[1].parse::<u32>().map_err(|_| refused("invalid system uid range"))?);
        }
    }
    let max = sys_max.ok_or_else(|| refused("system uid range unavailable"))?;
    if uid == 0 || gid == 0 || uid >= max || (61184..=65519).contains(&uid)
        || (61184..=65519).contains(&gid) { return Err(refused("supervisor must be a static system account")); }
    let groups = checked("id", &["-G", "duduclaw-runsup"])?;
    let groups = String::from_utf8(groups.stdout).map_err(|_| refused("invalid supervisor groups"))?;
    if groups.split_whitespace().collect::<Vec<_>>() != vec![fields[3]] {
        return Err(refused("supervisor has supplementary groups"));
    }
    let shadow = checked("getent", &["shadow", "duduclaw-runsup"])?;
    let shadow = String::from_utf8(shadow.stdout).map_err(|_| refused("invalid supervisor lockdown"))?;
    let shadow: Vec<_> = shadow.trim_end_matches('\n').split(':').collect();
    if shadow.len() != 9 || shadow[0] != "duduclaw-runsup" || shadow[1] != "!*" || shadow[7] != "1" {
        return Err(refused("supervisor account lockdown mismatch"));
    }
    checked("loginctl", &["disable-linger", "duduclaw-runsup"])?;
    match fs::symlink_metadata("/var/lib/systemd/linger/duduclaw-runsup") {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        _ => return Err(refused("supervisor linger remains enabled")),
    }
    mkdir("/var/lib/duduclaw-spawnd", 0o700)?;
    mkdir("/var/lib/duduclaw-spawnd/etc", 0o700)?;
    mkdir("/var/log/duduclaw-spawnd", 0o700)?;
    for (name, text) in SPAWND_UNITS {
        atomic_install(&format!("/etc/systemd/system/{name}"), 0o644,
            |file| { file.write_all(text.as_bytes())?; Ok(()) })?;
    }
    Ok(())
}

// Serialize binary and floor changes across install, upgrade and uninstall.
// This file remains after uninstall so waiters always lock the same inode.
fn version_lock() -> Result<fs::File> {
    mkdir("/var/lib/duduclaw-protected", 0o700)?;
    let path = Path::new("/var/lib/duduclaw-protected/version-lock");
    parents(path)?;
    let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    trusted(path, false)?;
    let metadata = lock.metadata()?;
    if metadata.uid() != 0 || metadata.gid() != 0 || metadata.mode() & 0o7777 != 0o600 || metadata.nlink() != 1 {
        return Err(refused("unsafe version lock"));
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(lock)
}

fn read_version_file(path: &str) -> Result<Option<String>> {
    let path = Path::new(path);
    parents(path)?;
    let value = match fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path) {
        Ok(mut file) => {
            use std::io::Read;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.uid() != 0 || metadata.gid() != 0
                || metadata.mode() & 0o7777 != 0o600 || metadata.nlink() != 1 || metadata.len() > 128 {
                return Err(refused("unsafe version floor"));
            }
            let mut value = String::new();
            file.read_to_string(&mut value)?;
            Some(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(value)
}

fn check_floor(target: &str, upgrade: bool) -> Result<()> {
    let floor = read_version_file(VERSION_FLOOR)?;
    // The atomic writer uses one version without whitespace. Do not normalize
    // malformed existing files into an accepted floor.
    duduclaw_core::root_key::check_protected_version(target,
        duduclaw_gateway::updater::current_version(), floor.as_deref(), upgrade).map_err(refused)?;
    if let Some(pending) = read_version_file(VERSION_PENDING)? {
        duduclaw_core::root_key::check_protected_version(target,
            duduclaw_gateway::updater::current_version(), Some(&pending), upgrade).map_err(refused)?;
    }
    Ok(())
}

fn prepare_version(version: &str) -> Result<()> {
    // Persist intent before replacing the binary: a crash or full /var after
    // rename must not permit a downgrade via a still-old floor. A failed swap
    // conservatively reserves this version until retry or deliberate uninstall.
    atomic_install(VERSION_PENDING, 0o600, |file| { file.write_all(version.as_bytes())?; Ok(()) })
}

fn record_floor(version: &str) -> Result<()> {
    atomic_install(VERSION_FLOOR, 0o600, |file| { file.write_all(version.as_bytes())?; Ok(()) })?;
    fs::remove_file(VERSION_PENDING)?;
    fs::File::open("/var/lib/duduclaw-protected")?.sync_all()?;
    Ok(())
}

pub(super) fn install(options: InstallOptions) -> Result<()> {
    gate()?;
    let _lock = version_lock()?;
    let version = duduclaw_gateway::updater::current_version();
    check_floor(version, false)?;
    let executable = fs::canonicalize("/proc/self/exe")?;
    if parents(&executable).and_then(|()| trusted(&executable, false)).is_err() {
        eprintln!("警告：正在執行的程式或其上層目錄可由非 root 帳號修改。首次安裝前必須驗證官方簽章，並從 root 管理的位置執行；這個檢查不能撤回已經以 root 執行的程式。");
        if !options.i_verified_this_binary {
            return Err(refused("請先驗證執行檔；確認後才可使用 --i-verified-this-binary"));
        }
    }
    let (uid, gid) = account()?;
    for dir in ["/opt/duduclaw", "/opt/duduclaw/bin", "/usr/local/libexec", "/usr/local/libexec/duduclaw"] {
        mkdir(dir, 0o755)?;
    }
    mkdir("/var/lib/duduclaw-protected", 0o700)?;
    // Unknown systemctl errors are never mistaken for an inactive service.
    let active = command("systemctl", &["is-active", "--quiet", UNIT])?;
    let was_active = match active.status.code() {
        Some(0) => true,
        Some(3 | 4) => false,
        _ => return Err(refused("cannot determine system service state")),
    };
    if options.migrate_from.is_some() && was_active { checked("systemctl", &["stop", UNIT])?; }
    if let Some(source) = options.migrate_from {
        super::migrate::copy(&source, uid, gid, options.force_migrate)?;
        println!("個人連線使用不同的金鑰，搬移後必須重新連結。請先停止來源 Gateway，避免資料在複製時變動。");
    }
    prepare_version(version)?;
    atomic_install(BINARY, 0o755, |target| {
        let mut source = fs::OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC).open("/proc/self/exe")?;
        if !source.metadata()?.is_file() { return Err(refused("invoking executable is not a regular file")); }
        std::io::copy(&mut source, target)?;
        Ok(())
    })?;
    atomic_install(LAUNCHER, 0o755, |file| { file.write_all(LAUNCHER_TEXT.as_bytes())?; Ok(()) })?;
    atomic_install(UNIT_PATH, 0o644, |file| { file.write_all(unit_text().as_bytes())?; Ok(()) })?;
    install_spawnd_support_if_published()?;
    record_floor(version)?;
    checked("systemctl", &["daemon-reload"])?;
    if was_active { checked("systemctl", &["restart", UNIT])?; }
    if options.enable_now { checked("systemctl", &["enable", "--now", UNIT])?; }
    println!("已安裝原生 Linux 憑證保護服務。啟動：sudo systemctl start {UNIT}");
    println!("請由原本使用者執行，停止並停用舊的使用者服務：\nsystemctl --user stop duduclaw.service\nsystemctl --user disable duduclaw.service");
    Ok(())
}

pub(super) fn uninstall() -> Result<()> {
    gate()?;
    let _lock = version_lock()?;
    // Validate every removal before stopping anything; never follow a replaced path.
    for file in [UNIT_PATH, BINARY, LAUNCHER, VERSION_FLOOR, VERSION_PENDING] {
        let path = Path::new(file);
        parents(path)?;
        match fs::symlink_metadata(path) {
            Ok(_) => trusted(path, false)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    if Path::new(UNIT_PATH).exists() { checked("systemctl", &["disable", "--now", UNIT])?; }
    for file in [UNIT_PATH, BINARY, LAUNCHER, VERSION_FLOOR, VERSION_PENDING] {
        match fs::remove_file(file) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    checked("systemctl", &["daemon-reload"])?;
    println!("已移除服務、執行檔、啟動器與版本下限。資料保留於 /var/lib/duduclaw；根金鑰保留於 /var/lib/duduclaw-protected/root.key；duduclaw 系統帳號保留。");
    Ok(())
}

// Upgrades must start from the installed trust anchor, never from a download.
fn installed_executable() -> Result<()> {
    let executable = fs::read_link("/proc/self/exe")?;
    if executable != Path::new(BINARY) {
        return Err(refused("upgrade must run as sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux"));
    }
    parents(Path::new(BINARY))?;
    trusted(Path::new(BINARY), false)?;
    let running = fs::metadata("/proc/self/exe")?;
    let installed = fs::metadata(BINARY)?;
    if running.dev() != installed.dev() || running.ino() != installed.ino() {
        return Err(refused("installed executable changed; invoke the installed binary again"));
    }
    Ok(())
}

pub(super) async fn upgrade(version: Option<&str>) -> Result<()> {
    gate()?;
    installed_executable()?;
    let _lock = version_lock()?;
    if let Some(version) = version { check_floor(version, true)?; }
    // Pin a root-only staging directory in the trusted installation hierarchy.
    let staging = tempfile::Builder::new().permissions(fs::Permissions::from_mode(0o700)).prefix(".upgrade-").tempdir_in("/opt/duduclaw")?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    trusted(staging.path(), true)?;
    let extracted = staging.path().join("duduclaw");
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&extracted)?;
    let (version, _, _) = duduclaw_gateway::updater::download_protected_release(version, &mut file)
        .await.map_err(refused)?;
    check_floor(&version, true)?;
    drop(file);
    installed_executable()?;
    let active = command("systemctl", &["is-active", "--quiet", UNIT])?;
    let was_active = match active.status.code() {
        Some(0) => true,
        Some(3 | 4) => false,
        _ => return Err(refused("cannot determine system service state")),
    };
    // Only bytes are installed. Never run the downloaded executable as root,
    // even for a version probe or to obtain its embedded launcher/unit.
    prepare_version(&version)?;
    atomic_install(BINARY, 0o755, |target| {
        std::io::copy(&mut fs::File::open(&extracted)?, target)?;
        Ok(())
    })?;
    record_floor(&version)?;
    if was_active { checked("systemctl", &["restart", UNIT])?; }
    println!("已驗證並安裝官方版本 {version}。現有資料與根金鑰保留。");
    Ok(())
}

#[cfg(test)]
mod spawnd_install_tests {
    use super::*;

    #[test]
    fn spawnd_templates_use_fixed_root_seqpacket_and_bounded_hierarchy() {
        let lookup = |name| SPAWND_UNITS.iter().find(|(unit, _)| *unit == name).unwrap().1;
        let socket = lookup("duduclaw-spawnd.socket");
        assert!(socket.lines().any(|line| line == "ListenSequentialPacket=/run/duduclaw-spawnd/control.sock"));
        assert!(socket.lines().any(|line| line == "SocketUser=root"));
        assert!(socket.lines().any(|line| line == "SocketGroup=duduclaw"));
        assert!(socket.lines().any(|line| line == "SocketMode=0660"));
        let service = lookup("duduclaw-spawnd.service");
        assert!(service.lines().any(|line| line == format!("ExecStart={SPAWND_BINARY}")));
        assert!(service.lines().any(|line| line == "StandardOutput=null"));
        assert!(service.lines().any(|line| line == "StandardError=null"));
        assert!(service.lines().any(|line| line == "Slice=duduclaw-tcb.slice"));
        assert!(service.lines().any(|line| line == "ReadWritePaths=/var/lib/duduclaw-spawnd /var/log/duduclaw-spawnd /run/duduclaw-spawnd /etc -/var/lib/systemd/linger"));
        assert!(lookup("duduclaw-employees.slice").lines().any(|line| line == "TasksMax=1024"));
        assert!(lookup("duduclaw.slice").lines().any(|line| line == "MemoryMin=512M"));
    }
}
