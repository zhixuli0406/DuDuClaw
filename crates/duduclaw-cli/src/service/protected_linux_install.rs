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

// The privileged update service owns verification, installation and restart.
// This command only submits a stable version; it never supplies paths or URLs.
pub(super) async fn upgrade(version: Option<&str>) -> Result<()> {
    use duduclaw_isolation::ids::RequestId;
    use duduclaw_isolation::update::{UpdateRequest, UpdateRequestV1, UpdateResponse, UpdateResponseV1};
    let version = duduclaw_updated::parse_version(version.ok_or_else(||
        refused("請指定 --version x.y.z，或從儀表板檢查並安裝更新。"))?)
        .map_err(|_| refused("版本必須是正式版 x.y.z。"))?;
    let request_id = RequestId(*uuid::Uuid::new_v4().as_bytes());
    let response = tokio::task::spawn_blocking(move ||
        duduclaw_updated::client::request(UpdateRequest::V1(UpdateRequestV1::Apply { version, request_id })))
        .await.map_err(|_| refused("更新服務無法連線，未執行舊版升級流程。"))?
        .map_err(|_| refused("更新服務無法連線，未執行舊版升級流程。"))?;
    match response {
        UpdateResponse::V1(UpdateResponseV1::Accepted { request_id: received }) if received == request_id => {
            println!("已送出更新要求 {}，尚未確認安裝完成。請在儀表板查看結果。", uuid::Uuid::from_bytes(request_id.0));
            Ok(())
        }
        UpdateResponse::V1(UpdateResponseV1::Error { .. }) => Err(refused("更新服務拒絕要求，未變更安裝。")),
        _ => Err(refused("更新服務回應不符，未確認更新結果。")),
    }
}
