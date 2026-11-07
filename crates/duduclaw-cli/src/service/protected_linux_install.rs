use super::*;
use std::{fs, io::Write, os::unix::{fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt}, io::AsRawFd}, path::Path, process::{Command, Output}};
#[path = "protected_linux_readiness.rs"]
mod readiness;
#[path = "protected_linux_uninstall.rs"]
mod removal;

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
    use std::{io::Read, process::Stdio, time::{Duration, Instant}};
    struct ReapOnError(std::process::Child);
    impl Drop for ReapOnError {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill(); let _ = self.0.wait();
            }
        }
    }
    trusted(Path::new("/run"), true)?;
    let mut stdout = tempfile::NamedTempFile::new_in("/run")?;
    let mut stderr = tempfile::NamedTempFile::new_in("/run")?;
    // Fixed cwd, PATH and HOME (origin/main) inside the bounded, reaped spawn (isolation).
    let mut child = ReapOnError(Command::new(executable).args(args).current_dir("/").env_clear()
        .env("PATH", super::runtime::SERVICE_PATH).env("HOME", "/var/lib/duduclaw").env("LC_ALL", "C")
        .stdin(Stdio::null()).stdout(stdout.reopen()?).stderr(stderr.reopen()?).spawn()?);
    let deadline = Instant::now() + Duration::from_secs(30);
    let limit = 4 * 1024 * 1024;
    let status = loop {
        if stdout.as_file().metadata()?.len() > limit || stderr.as_file().metadata()?.len() > limit
            || Instant::now() >= deadline {
            return Err(refused("system command exceeded its time or output limit"));
        }
        if let Some(status) = child.0.try_wait()? { break status; }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut out = Vec::new(); let mut err = Vec::new();
    stdout.as_file_mut().take(limit + 1).read_to_end(&mut out)?;
    stderr.as_file_mut().take(limit + 1).read_to_end(&mut err)?;
    if out.len() as u64 > limit || err.len() as u64 > limit { return Err(refused("system command output exceeded limit")); }
    Ok(Output { status, stdout: out, stderr: err })
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
        let mut args = vec!["--system", "--user-group", "--home-dir", "/var/lib/duduclaw",
            "--create-home", "--shell", "/usr/sbin/nologin"];
        let limits;
        if layout::published_components(Path::new("/"), 0, 0)? {
            let defs = fs::read_to_string("/etc/login.defs")?;
            limits = [format!("SYS_UID_MAX={}", layout::system_id_limit(&defs, "SYS_UID_MAX").map_err(refused)? - 1),
                format!("SYS_GID_MAX={}", layout::system_id_limit(&defs, "SYS_GID_MAX").map_err(refused)? - 1)];
            args.extend(["--key", limits[0].as_str(), "--key", limits[1].as_str(),
                "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0"]);
        }
        args.push("duduclaw");
        checked("useradd", &args)?;
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

const PARSE_RUNTIME_TREE: &str = "/usr/lib/duduclaw/parse-root-tree";
const PARSE_WORKER: &str = "/usr/libexec/duduclaw/duduclaw-parse-worker";
const PARSE_RUNTIME_BUILDER: &str = include_str!("../../../duduclaw-parse-worker/scripts/build-runtime-tree.py");

// The published worker's own digest cannot establish trust. Read the current
// generation through F's root-owned state and authenticate its recorded signed
// manifest before asking the embedded helper to inspect/copy any executable.
fn parse_runtime_tree(install: bool) -> Result<()> {
    use duduclaw_updated::linux::{files::Directory, activation, trust_admin};
    use duduclaw_isolation::update::InstallTarget;
    let state_root = Directory::open(Path::new(duduclaw_updated::linux::STATE_ROOT))
        .map_err(|_| refused("缺少已驗證的隔離安裝狀態，未建立解析執行環境。"))?;
    let _update_lock = state_root.lock().map_err(|_| refused("隔離更新進行中，稍後再準備解析執行環境。"))?;
    let state: duduclaw_updated::state::State = state_root.json("state.json").map_err(refused)?;
    state.validate().map_err(refused)?;
    let current = activation::read_current().map_err(refused)?;
    if state.pending.is_some() || current != state.effective_digest() {
        return Err(refused("隔離世代尚未完成更新，未變更解析執行環境。"));
    }
    let generation = Directory::open(Path::new(duduclaw_updated::linux::INSTALL_ROOT)).map_err(refused)?
        .child("generations").map_err(refused)?.child(&hex::encode(current.0)).map_err(refused)?;
    let bytes = generation.bytes("manifest.json", duduclaw_updated::metadata::MAX_METADATA_BYTES).map_err(refused)?;
    if duduclaw_updated::publication::manifest_digest(&bytes) != current {
        return Err(refused("隔離世代的簽章文件不符。"));
    }
    let signature = generation.bytes("manifest.json.sig", 16 * 1024).map_err(refused)?;
    let signature = std::str::from_utf8(&signature).map_err(refused)?;
    let manifest = trust_admin::verify_recorded(&bytes, signature).map_err(refused)?;
    if manifest.product != state.product || manifest.arch != state.arch
        || manifest.version != state.running_version.unwrap_or(state.floor) {
        return Err(refused("解析元件與已啟用世代不符。"));
    }
    let worker = manifest.artifacts.iter().find(|artifact| artifact.install_target == InstallTarget::ParseWorker)
        .ok_or_else(|| refused("簽章文件缺少解析元件。"))?;
    if worker.mode != 0o755 || worker.size_bytes == 0 || worker.size_bytes > 256 * 1024 * 1024 {
        return Err(refused("簽章文件的解析元件規格不符。"));
    }
    let source = Path::new(PARSE_WORKER);
    parents(source)?;
    trusted(source, false)?;
    let metadata = fs::symlink_metadata(source)?;
    if metadata.len() != worker.size_bytes || metadata.nlink() != 1 || metadata.mode() & 0o7777 != 0o755 {
        return Err(refused("已發布解析元件與簽章文件不符。"));
    }
    if install {
        mkdir("/usr/lib/duduclaw/parse-root", 0o755)?;
        if !layout::parse_root_is_reusable(Path::new("/usr/lib/duduclaw/parse-root"), 0, 0)? {
            return Err(refused("解析服務掛載目錄含有非預期內容。"));
        }
    }
    let digest = hex::encode(worker.sha256.0);
    // -I ignores Python user paths and environment. The program is part of this
    // signed CLI, not a mutable helper selected from the caller's filesystem.
    checked("python3", &["-I", "-c", PARSE_RUNTIME_BUILDER,
        "--worker", PARSE_WORKER, "--worker-sha256", &digest,
        "--destination", PARSE_RUNTIME_TREE, if install { "--install" } else { "--verify" }])?;
    Ok(())
}

// Read and compare through the same pinned descriptor that was validated.
// MERGE-NOTE: only the legacy in-process upgrade used this; `upgrade` now
// hands off to the privileged updater (see the note on `upgrade`).
#[allow(dead_code)]
fn update_text_if_different(path: &str, mode: u32, expected: &str) -> Result<bool> {
    use std::io::Read;
    parents(Path::new(path))?;
    let installed = match fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => {
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.uid() != 0 || metadata.gid() != 0
                || metadata.mode() & 0o7022 != 0 || metadata.nlink() != 1 {
                return Err(refused("unsafe installed service contract"));
            }
            let mut bytes = Vec::new();
            file.take(262145).read_to_end(&mut bytes)?;
            if bytes.len() > 262144 { return Err(refused("installed service contract too large")); }
            Some(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if !duduclaw_core::root_key::recovery::protected_content_differs(installed.as_deref(), expected.as_bytes()) {
        return Ok(false);
    }
    atomic_install(path, mode, |file| { file.write_all(expected.as_bytes())?; Ok(()) })?;
    Ok(true)
}

const RECOVERY_BOUND: &str = "/etc/systemd/system/duduclaw-gateway.recovery-bound";

fn invalidate_recovery_bound() -> Result<()> {
    parents(Path::new(RECOVERY_BOUND))?;
    match fs::symlink_metadata(RECOVERY_BOUND) {
        Ok(_) => { trusted(Path::new(RECOVERY_BOUND), false)?; fs::remove_file(RECOVERY_BOUND)?; }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    fs::File::open("/etc/systemd/system")?.sync_all()?;
    Ok(())
}

fn reload_and_certify_recovery() -> Result<()> {
    checked("systemctl", &["daemon-reload"])?;
    let output = checked("systemctl", &["show", UNIT, "-p", "StartLimitIntervalUSec",
        "-p", "StartLimitBurst", "-p", "RestartForceExitStatus"])?;
    let properties = std::str::from_utf8(&output.stdout).map_err(|_| refused("invalid unit properties"))?;
    if !duduclaw_core::root_key::recovery::protected_properties_bounded(properties) {
        return Err(refused("effective unit restart bounds differ; remove conflicting systemd overrides before upgrading"));
    }
    atomic_install(RECOVERY_BOUND, 0o644, |file| { file.write_all(unit_text().as_bytes())?; Ok(()) })
}

const VERSION_FLOOR: &str = "/var/lib/duduclaw-protected/version-floor";
const VERSION_PENDING: &str = "/var/lib/duduclaw-protected/version-pending";

#[cfg(test)]
const SPAWND_BINARY: &str = "/usr/lib/duduclaw/duduclaw-spawnd";
// Signed bootstrap publishes the entire component set before this hook runs.
// Ownership checks are installation checks, not signature verification. Never
// enable services, invent trust/state, or copy companions from an untrusted cwd.
fn install_isolation_support_if_published() -> Result<()> {
    // Validate every file and ancestor before any accounts or units change.
    if !layout::published_components(Path::new("/"), 0, 0)? { return Ok(()); }
    parse_runtime_tree(true)?;
    let defs = fs::read_to_string("/etc/login.defs")?;
    // Validate explicit host ranges before useradd can allocate anything.
    layout::system_id_limit(&defs, "SYS_UID_MAX").map_err(refused)?;
    layout::system_id_limit(&defs, "SYS_GID_MAX").map_err(refused)?;
    let (uid, gid) = account()?;
    layout::validate_system_ids(uid, gid, &defs).map_err(refused)?;
    let mut identities = vec![(uid, gid)];
    for name in layout::ACCOUNTS {
        let identity = isolation_account(name, &defs)?;
        if identities.iter().any(|(uid, gid)| *uid == identity.0 || *gid == identity.1) {
            return Err(refused("isolation services must have distinct uid and gid values"));
        }
        identities.push(identity);
    }
    for (path, mode) in layout::DIRECTORIES {
        mkdir(path, *mode)?;
        if *path == "/usr/lib/duduclaw/parse-root"
            && !layout::parse_root_is_reusable(Path::new(path), 0, 0)? {
            return Err(refused("parse worker roots contain unexpected content; stop workers and inspect the directory"));
        }
    }
    for (name, text) in layout::UNITS {
        atomic_install(&format!("/etc/systemd/system/{name}"), 0o644,
            |file| { file.write_all(text.as_bytes())?; Ok(()) })?;
    }
    println!("隔離服務的帳號、目錄與單元已備妥，尚未啟用。簽章世代、DNS 政策、遷移與完整就緒檢查仍須由已驗證的安裝流程完成。");
    Ok(())
}

fn isolation_account(name: &str, defs: &str) -> Result<(u32, u32)> {
    let query = command("getent", &["passwd", name])?;
    match query.status.code() {
        Some(2) => {
            let uid_max = format!("SYS_UID_MAX={}", layout::system_id_limit(defs, "SYS_UID_MAX").map_err(refused)? - 1);
            let gid_max = format!("SYS_GID_MAX={}", layout::system_id_limit(defs, "SYS_GID_MAX").map_err(refused)? - 1);
            checked("useradd", &["--system", "--user-group", "--no-create-home", "--no-log-init",
                "--key", "SUB_UID_COUNT=0", "--key", "SUB_GID_COUNT=0",
                "--key", &uid_max, "--key", &gid_max,
                "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin",
                "--password", "!*", "--expiredate", "1970-01-02", name])?;
        }
        Some(0) => (),
        _ => return Err(refused("cannot query isolation service account")),
    }
    let passwd = checked("getent", &["passwd", name])?;
    let passwd = String::from_utf8(passwd.stdout).map_err(|_| refused("invalid service account"))?;
    let fields: Vec<_> = passwd.trim_end_matches('\n').split(':').collect();
    if fields.len() != 7 || fields[0] != name || fields[5] != "/nonexistent"
        || fields[6] != "/usr/sbin/nologin" { return Err(refused("unsafe isolation service account")); }
    let uid: u32 = fields[2].parse().map_err(|_| refused("invalid service uid"))?;
    let gid: u32 = fields[3].parse().map_err(|_| refused("invalid service gid"))?;
    layout::validate_system_ids(uid, gid, defs).map_err(refused)?;
    let groups = checked("id", &["-G", name])?;
    let groups = String::from_utf8(groups.stdout).map_err(|_| refused("invalid service groups"))?;
    if groups.split_whitespace().collect::<Vec<_>>() != vec![fields[3]] {
        return Err(refused("isolation service has supplementary groups"));
    }
    let shadow = checked("getent", &["shadow", name])?;
    let shadow = String::from_utf8(shadow.stdout).map_err(|_| refused("invalid service lockdown"))?;
    let shadow: Vec<_> = shadow.trim_end_matches('\n').split(':').collect();
    if shadow.len() != 9 || shadow[0] != name || shadow[1] != "!*" || shadow[7] != "1" {
        return Err(refused("isolation service account lockdown mismatch"));
    }
    checked("loginctl", &["disable-linger", name])?;
    match fs::symlink_metadata(format!("/var/lib/systemd/linger/{name}")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        _ => return Err(refused("isolation service linger remains enabled")),
    }
    Ok((uid, gid))
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
    require_isolation_migration(floor.as_deref(), target)?;
    if upgrade {
        // A newer floor can survive an interrupted upgrade while the old
        // executable still runs; both must authorize the migration boundary.
        require_isolation_migration(Some(duduclaw_gateway::updater::current_version()), target)?;
    }
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

// The old single-binary updater cannot establish D9's independent bootstrap
// trust or quarantine authority. Never cross that boundary by replacing bytes.
fn require_isolation_migration(installed: Option<&str>, target: &str) -> Result<()> {
    let target = semver::Version::parse(target).map_err(|_| refused("invalid target version"))?;
    if installed.is_none() && (target.major, target.minor) >= (1, 80) {
        return Err(refused("無法確認這個安裝是否已完成資料遷移，已停止升級。請使用官方遷移流程。"));
    }
    if let Some(installed) = installed {
        let installed = semver::Version::parse(installed).map_err(|_| refused("invalid installed version"))?;
        if (installed.major, installed.minor) < (1, 80) && (target.major, target.minor) >= (1, 80) {
            return Err(refused("此安裝需要先完成經獨立驗證的資料遷移，不能直接替換程式升級。請使用已驗證的新版安裝套件執行 duduclaw-migrate run --plan，完成後再升級。"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod isolation_migration_tests {
    use super::require_isolation_migration;

    #[test]
    fn legacy_protected_upgrade_cannot_bypass_migration() {
        for target in ["1.80.0", "1.80.0-rc.1", "1.80.1", "2.0.0"] {
            assert!(require_isolation_migration(Some("1.79.3"), target).is_err());
        }
        assert!(require_isolation_migration(Some("1.79.3"), "1.79.4").is_ok());
        assert!(require_isolation_migration(Some("1.80.0"), "1.80.1").is_ok());
        assert!(require_isolation_migration(Some("invalid"), "1.80.0").is_err());
        assert!(require_isolation_migration(None, "1.80.0").is_err());
        assert!(require_isolation_migration(None, "1.79.3").is_ok());
    }
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

// This decides which coordinator owns the request, not whether the generation
// is trusted. F still authenticates metadata and live readbacks before any apply.
fn managed_install() -> Result<bool> {
    let state = Path::new("/var/lib/duduclaw-updated/state.json");
    let current = Path::new("/opt/duduclaw-isolated/current");
    let exists = |path: &Path| -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    };
    match (exists(state)?, exists(current)?) {
        (false, false) => Ok(false),
        (true, true) => {
            let directory = duduclaw_updated::linux::files::Directory::open(Path::new("/var/lib/duduclaw-updated"))
                .map_err(|_| refused("無法確認隔離更新服務的狀態擁有者。"))?;
            let saved: duduclaw_updated::state::State = directory.json("state.json")
                .map_err(|_| refused("隔離更新狀態無法讀取。"))?;
            saved.validate().map_err(|_| refused("隔離更新狀態不完整。"))?;
            let selected = duduclaw_updated::linux::activation::read_current()
                .map_err(|_| refused("隔離世代選擇器無法驗證。"))?;
            if selected != saved.effective_digest() && !saved.pending.as_ref().is_some_and(|pending| pending.digest == selected) {
                return Err(refused("隔離世代與更新狀態不符。"));
            }
            Ok(true)
        }
        _ => Err(refused("隔離更新狀態不完整；請先恢復更新交易，未執行舊版安裝或遷移。")),
    }
}

pub(super) fn install(options: InstallOptions) -> Result<()> {
    gate()?;
    let _lock = version_lock()?;
    let version = duduclaw_gateway::updater::current_version();
    if managed_install()? {
        // The invoking CLI's templates may be older than F's committed
        // generation. A reinstall cannot replace signed service policy.
        parse_runtime_tree(true)?;
        return readiness::check(&mut readiness::MissingComposite);
    }
    let floor = read_version_file(VERSION_FLOOR)?;
    if native_migration::run_if_required(floor.as_deref(), version)? {
        return readiness::check(&mut readiness::MissingComposite);
    }
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
    install_isolation_support_if_published()?;
    invalidate_recovery_bound()?;
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
    reload_and_certify_recovery()?;
    if was_active { checked("systemctl", &["restart", UNIT])?; }
    if options.enable_now { checked("systemctl", &["enable", "--now", UNIT])?; }
    println!("已安裝原生 Linux 憑證保護服務。啟動：sudo systemctl start {UNIT}");
    println!("請由原本使用者執行，停止並停用舊的使用者服務：\nsystemctl --user stop duduclaw.service\nsystemctl --user disable duduclaw.service");
    check_runtimes(uid, gid, options.allow_missing_runtime)
}

pub(super) fn uninstall() -> Result<()> {
    gate()?;
    let _lock = version_lock()?;
    for path in layout::BINARIES.iter().copied().chain([
        "/var/lib/duduclaw-updated/state.json", "/opt/duduclaw-isolated/current",
        "/var/lib/duduclaw-protected/uninstall-units",
    ]) {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => return removal::run(),
        }
    }
    // Validate every removal before stopping anything; never follow a replaced path.
    for file in [UNIT_PATH, BINARY, LAUNCHER, VERSION_FLOOR, VERSION_PENDING, RECOVERY_BOUND] {
        let path = Path::new(file);
        parents(path)?;
        match fs::symlink_metadata(path) {
            Ok(_) => trusted(path, false)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    if Path::new(UNIT_PATH).exists() { checked("systemctl", &["disable", "--now", UNIT])?; }
    for file in [UNIT_PATH, BINARY, LAUNCHER, VERSION_FLOOR, VERSION_PENDING, RECOVERY_BOUND] {
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

pub(super) fn readiness() -> Result<()> {
    gate()?;
    readiness::check(&mut readiness::MissingComposite)
}

// The privileged update service owns verification, installation and restart.
// This command only submits a stable version; it never supplies paths or URLs.
//
// MERGE-NOTE: origin/main extended the legacy in-process upgrade (download,
// byte replacement, launcher/unit refresh via `update_text_if_different`,
// `invalidate_recovery_bound` + `reload_and_certify_recovery`, then a runtime
// check). The isolation branch replaced that flow with a request to the
// privileged updater, which must not be bypassed by replacing bytes from the
// CLI. The stricter updater handoff is kept; main's `--allow-missing-runtime`
// runtime check runs as a preflight before the request is submitted. The
// updater must apply main's unit/launcher refresh and restart-bound
// certification for updater-managed generations.
pub(super) async fn upgrade(version: Option<&str>, allow_missing_runtime: bool) -> Result<()> {
    use duduclaw_isolation::ids::RequestId;
    use duduclaw_isolation::update::{UpdateRequest, UpdateRequestV1, UpdateResponse, UpdateResponseV1};
    let requested = version.ok_or_else(|| refused("請指定 --version x.y.z，或從儀表板檢查並安裝更新。"))?;
    let version = duduclaw_updated::parse_version(requested)
        .map_err(|_| refused("版本必須是正式版 x.y.z。"))?;
    gate()?;
    let _lock = version_lock()?;
    if !managed_install()? {
        let floor = read_version_file(VERSION_FLOOR)?;
        if native_migration::run_if_required(floor.as_deref(), requested)? {
            return readiness::check(&mut readiness::MissingComposite);
        }
    }
    // A configured AI CLI the service account cannot reach refuses the
    // request unless the operator passed --allow-missing-runtime.
    let (uid, gid) = account()?;
    check_runtimes(uid, gid, allow_missing_runtime)?;
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

#[cfg(test)]
mod spawnd_install_tests {
    use super::*;

    #[test]
    fn spawnd_templates_use_fixed_root_seqpacket_and_bounded_hierarchy() {
        let lookup = |name| layout::UNITS.iter().find(|(unit, _)| *unit == name).unwrap().1;
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

#[cfg(test)]
mod isolation_install_root_tests {
    use super::*;

    #[test]
    #[ignore = "requires a disposable native Linux VM and the full root-published binary set"]
    fn iso_root_install_support_staging_and_idempotent_readback() {
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
        gate().unwrap();
        assert!(layout::published_components(Path::new("/"), 0, 0).unwrap());
        install_isolation_support_if_published().unwrap();
        install_isolation_support_if_published().unwrap();
        for (path, mode) in layout::DIRECTORIES {
            let meta = fs::symlink_metadata(path).unwrap();
            assert!(meta.is_dir());
            assert_eq!((meta.uid(), meta.gid(), meta.mode() & 0o7777), (0, 0, *mode));
        }
        for (name, text) in layout::UNITS {
            let path = format!("/etc/systemd/system/{name}");
            assert_eq!(fs::read_to_string(&path).unwrap(), *text);
            let meta = fs::symlink_metadata(path).unwrap();
            assert_eq!((meta.uid(), meta.gid(), meta.mode() & 0o7777), (0, 0, 0o644));
        }
        // This fixture proves staging only: it never enables services or certifies readiness.
    }
}

// Probe only fixed system tools after dropping every supplemental group, just
// like the launcher. Never run the discovered CLI (or migrated files) as root.
fn service_access(path: &Path, uid: u32, gid: u32) -> Result<bool> {
    let uid = uid.to_string();
    let gid = gid.to_string();
    let path = path.to_str().ok_or_else(|| refused("non-UTF8 runtime path"))?;
    let result = command("setpriv", &["--reuid", &uid, "--regid", &gid, "--clear-groups",
        "--inh-caps=-all", "--bounding-set=-all", "--no-new-privs",
        "/usr/bin/test", "-f", path, "-a", "-r", path, "-a", "-x", path])?;
    match result.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(refused("cannot check runtime access as duduclaw")),
    }
}

fn operator_home() -> Option<PathBuf> {
    // SUDO_UID is used only to obtain diagnostic paths, never as authority.
    let uid = std::env::var("SUDO_UID").ok()?.parse::<u32>().ok()?;
    let output = command("getent", &["passwd", &uid.to_string()]).ok()?;
    if !output.status.success() { return None; }
    let text = String::from_utf8(output.stdout).ok()?;
    let home = PathBuf::from(text.trim_end().split(':').nth(5)?);
    home.is_absolute().then_some(home)
}

fn check_runtimes(uid: u32, gid: u32, allow_missing: bool) -> Result<()> {
    let runtimes = super::runtime::configured(Path::new("/var/lib/duduclaw/.duduclaw"))?;
    let operator = operator_home();
    let mut missing_primary = false;
    for (provider, required) in runtimes {
        let binaries = super::runtime::binary_names(&provider)?;
        if binaries.is_empty() { continue; }
        let mut resolution = super::runtime::Resolution::Missing;
        for binary in &binaries {
            // Propagate failed account probes; never mistake them for success.
            let access_error = std::cell::RefCell::new(None);
            let candidate = super::runtime::resolve(binary, |path| fs::canonicalize(path).ok(), |path| {
                if duduclaw_gateway::runtime_readiness::protected_canonical_path(path).is_none() { return false; }
                match service_access(path, uid, gid) {
                    Ok(access) => access,
                    Err(error) => { *access_error.borrow_mut() = Some(error); false }
                }
            }, operator.as_deref());
            if let Some(error) = access_error.into_inner() { return Err(error); }
            match candidate {
                super::runtime::Resolution::Found(_) => { resolution = candidate; break; }
                super::runtime::Resolution::Hidden(_) => resolution = candidate,
                super::runtime::Resolution::Missing => (),
            }
        }
        if matches!(resolution, super::runtime::Resolution::Found(_)) {
            if provider == "claude" {
                eprintln!("Claude CLI 已可供服務使用；登入資料不會搬移。請確認服務帳號已登入 / CLI login is not migrated; authenticate the service account if needed:\nsudo -u duduclaw -H claude auth login");
            }
        } else {
            eprintln!("{}", super::runtime::missing_message(binaries[0], &resolution));
            missing_primary |= required;
        }
    }
    super::runtime::enforce(missing_primary, allow_missing)
}
