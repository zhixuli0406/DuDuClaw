//! Per-call Codex directories (v1.84 security review H1, re-review F3/F4).
//!
//! Every per-call Codex directory — the employee's owned `CODEX_HOME` with
//! its `AGENTS.md` and the `auth.json` link, the `--output-schema` file, and
//! the utility working directory — lives under the gateway-private
//! `<DUDUCLAW_HOME>/run/codex/`. It used to live in the user temp directory
//! under a fixed prefix, and the employee permission profile grants
//! `:tmpdir = write` and `/tmp = write` to every Codex employee's tools (all
//! employees share one OS user), so one employee could replace another call's
//! instructions before Codex read them.
//!
//! * `<home>/run` and `<home>/run/codex` are opened once per call, each
//!   relative to the previous handle with `O_DIRECTORY|O_NOFOLLOW` (Unix):
//!   a symbolic link there fails the open. Owner (the gateway user) and mode
//!   (0700, set with `fchmod`) are verified on the opened handles, and the
//!   call directory is created relative to the parent handle (`mkdirat`).
//! * Each call gets a fresh `call-<32 hex>` / `utility-<32 hex>` directory
//!   named from the OS random source, created exclusively (0700).
//! * Each call directory holds [`LOCK_FILE`] with a cross-process exclusive
//!   advisory lock (`flock`, via `fs2`) for its whole lifetime (F3), so a
//!   second gateway (rolling restart) never removes a running call's files.
//! * The directory is removed when its [`OwnedCallDir`] guard drops: success,
//!   error, timeout and cancellation (a dropped future drops its locals).
//!   On Unix the removal is relative to the parent handle and never follows
//!   a link; the lock is released only after the removal.
//! * [`sweep_stale`] removes what a stopped gateway left behind: at gateway
//!   start and at most once an hour afterwards, only entries of the parent
//!   that match the naming scheme, are real directories (symbolic links are
//!   ignored, never followed), whose lock can be taken without waiting (no
//!   process, in this gateway or another, is using them) AND that are older
//!   than [`STALE_AFTER`].
//!
//! The employee permission profile additionally marks `<home>/run` and the
//! parent read-only for tools (see `employee_permission_args` in `codex.rs`),
//! and a working root that contains `<home>/run` is refused before a call
//! starts ([`check_work_root`]), so tools can neither write there nor rename
//! an ancestor of it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Name prefix of an employee call's directory (holds `codex-home/`).
pub(crate) const CALL_PREFIX: &str = "call-";
/// Name prefix of a utility call's directory (its working directory; holds `codex-home/`).
pub(crate) const UTILITY_PREFIX: &str = "utility-";
/// Hex characters after the prefix (16 random bytes).
const NAME_HEX_LEN: usize = 32;
/// An unlocked entry this old belongs to no running call.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
/// Minimum time between two sweeps of one parent.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Lock file inside every call directory, exclusively locked by the guard.
pub(crate) const LOCK_FILE: &str = ".duduclaw-call.lock";
/// Nesting limit of a removal (a call directory is a few levels deep).
#[cfg(unix)]
const MAX_REMOVE_DEPTH: usize = 32;

/// Plain refusal when a Codex call's working root contains the system data
/// folder (F4). No path is included.
pub(crate) const WORK_ROOT_CONTAINS_HOME: &str = "Codex 不會啟動：這位員工的工作資料夾包含了 DuDuClaw 的系統資料夾。工作資料夾不可以是系統資料夾本身、它的上層資料夾，或任何包含系統資料夾的資料夾；請改用員工自己的資料夾或另一個專案資料夾。";

/// Preserve the engine-local fallback class without disclosing a host path.
#[cfg(windows)]
pub(crate) const WORK_ROOT_MUST_BE_ABSOLUTE: &str = "為保護系統資料夾，這位員工的工作資料夾必須是完整路徑（例如 C:\\Work\\project），已停止啟動 Codex。";
#[cfg(windows)]
pub(crate) const WORK_ROOT_NETWORK_FOLDER: &str = "網路資料夾或無法確認為本機的磁碟不能用作 Codex 的工作資料夾，已停止啟動 Codex。";

/// `<home>/run/codex`.
pub(crate) fn run_parent(home_dir: &Path) -> PathBuf {
    home_dir.join("run").join("codex")
}

/// The host Codex credential store directory: the gateway process's
/// `$CODEX_HOME`, or `~/.codex`.
pub(crate) fn host_codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

/// Directories the employee permission profile must keep away from tools:
/// `<home>/run` itself (F4: renaming it, or `codex` inside it, would let a
/// tool plant a link), the owned-home parent and the host credential store,
/// each also in its canonical form when that differs (a link in the path must
/// not let the other spelling escape the entry). The profile is Unix-only.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn protected_roots(home_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_absolute() && !roots.contains(&path) { roots.push(path); }
    };
    let run = home_dir.join("run");
    if let Ok(canonical) = std::fs::canonicalize(&run) { push(canonical); }
    push(run);
    let parent = run_parent(home_dir);
    if let Ok(canonical) = std::fs::canonicalize(&parent) { push(canonical); }
    push(parent);
    if let Some(host) = host_codex_home() {
        if let Ok(auth) = std::fs::canonicalize(host.join("auth.json")) {
            if let Some(dir) = auth.parent() { push(dir.to_path_buf()); }
        }
        if let Ok(canonical) = std::fs::canonicalize(&host) { push(canonical); }
        push(host);
    }
    roots
}

/// A successful gate retains the canonical snapshot for security checks and
/// supplies an equivalent child-facing spelling for argv, cwd and sandbox.
/// The snapshot is rechecked immediately before handing the command to spawn.
/// The remaining replacement window starts after the last handle snapshot /
/// path resolution and lasts until the OS/CLI reopens the pathname. Windows
/// handles allow FILE_SHARE_DELETE, so replacement can happen immediately after
/// the final-path query, even while the snapshot handle is still open.
#[derive(Debug)]
pub(crate) struct CheckedWorkRoot {
    path: PathBuf,
    identity: duduclaw_core::path_compare::FileIdentity,
}

impl CheckedWorkRoot {
    pub(crate) fn canonical_path(&self) -> &Path { &self.path }

    pub(crate) fn spawn_path(&self) -> &Path {
        self.path()
    }

    /// Child-facing path; use `canonical_path` to inspect the security snapshot.
    pub(crate) fn path(&self) -> &Path {
        duduclaw_core::path_compare::spawn_path(&self.path)
    }

    pub(crate) fn apply_to_command(&self, cmd: &mut tokio::process::Command) {
        cmd.current_dir(self.spawn_path()).arg("--cd").arg(self.spawn_path());
    }

    pub(crate) fn reverify(&self) -> Result<(), String> {
        validate_windows_work_root(&self.path)?;
        let snapshot = directory_identity(&self.path).map_err(directory_identity_refusal)?;
        validate_directory_snapshot(&self.path, &snapshot)?;
        let identity = snapshot.identity;
        if !identity.is_strong() || identity != self.identity {
            return Err(work_root_refusal());
        }
        Ok(())
    }
}

fn work_root_refusal() -> String {
    duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(WORK_ROOT_CONTAINS_HOME)
}

#[cfg(windows)]
fn validate_windows_work_root(path: &Path) -> Result<(), String> {
    use duduclaw_core::path_compare::{classify_windows_path, is_local_windows_path, WindowsPathKind};
    let kind = path.to_str().map(classify_windows_path);
    let text = if kind == Some(WindowsPathKind::Unc) {
        Some(WORK_ROOT_NETWORK_FOLDER)
    } else if kind != Some(WindowsPathKind::Absolute) || !path.is_absolute() {
        Some(WORK_ROOT_MUST_BE_ABSOLUTE)
    } else if !is_local_windows_path(path).unwrap_or(false) {
        Some(WORK_ROOT_NETWORK_FOLDER)
    } else {
        None
    };
    match text {
        Some(text) => Err(duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(text)),
        None => Ok(()),
    }
}

#[cfg(not(windows))]
fn validate_windows_work_root(_path: &Path) -> Result<(), String> { Ok(()) }

struct DirectorySnapshot {
    identity: duduclaw_core::path_compare::FileIdentity,
    #[cfg(windows)]
    final_path: PathBuf,
}

fn directory_identity_refusal(error: std::io::Error) -> String {
    #[cfg(windows)]
    if error.kind() == std::io::ErrorKind::Unsupported {
        return duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(WORK_ROOT_NETWORK_FOLDER);
    }
    let _ = error;
    work_root_refusal()
}

fn validate_directory_snapshot(path: &Path, snapshot: &DirectorySnapshot) -> Result<(), String> {
    #[cfg(windows)]
    {
        use duduclaw_core::path_compare::same_final_path_exact;
        let matches = snapshot.final_path.to_str().zip(path.to_str())
            .and_then(|(final_path, canonical)| same_final_path_exact(final_path, canonical));
        match matches {
            Some(true) => {},
            Some(false) => return Err(work_root_refusal()),
            None => return Err(duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(
                WORK_ROOT_NETWORK_FOLDER)),
        }
    }
    #[cfg(not(windows))]
    let _ = (path, snapshot);
    Ok(())
}

#[cfg(windows)]
fn directory_identity(path: &Path) -> std::io::Result<DirectorySnapshot> {
    let (final_path, identity) = duduclaw_core::path_compare::windows_directory_identity(path)?;
    Ok(DirectorySnapshot { final_path, identity })
}

#[cfg(not(windows))]
fn directory_identity(path: &Path) -> std::io::Result<DirectorySnapshot> {
    if !std::fs::metadata(path)?.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a directory"));
    }
    Ok(DirectorySnapshot { identity: duduclaw_core::path_compare::file_identity(path)? })
}

/// Refuse roots containing `<home>/run`, ambiguous Windows paths, network
/// namespaces and incomplete identity evidence. Only the protected run side
/// may have missing components; the workspace must exist and have a strong
/// identity. Keep both lexical and resolved run ancestors for alias denials.
pub(crate) fn check_work_root(home_dir: &Path, work_root: &Path) -> Result<CheckedWorkRoot, String> {
    check_work_root_with_identity(home_dir, work_root, directory_identity)
}

fn check_work_root_with_identity(
    home_dir: &Path,
    work_root: &Path,
    mut identify: impl FnMut(&Path) -> std::io::Result<DirectorySnapshot>,
) -> Result<CheckedWorkRoot, String> {
    use duduclaw_core::path_compare::{canonicalize, canonicalize_allow_missing, starts_with};
    validate_windows_work_root(work_root)?;
    let root = canonicalize(work_root).map_err(|_| work_root_refusal())?;
    validate_windows_work_root(&root)?;
    // Never permit NotFound here, including disappearance after canonicalize.
    let snapshot = identify(&root).map_err(directory_identity_refusal)?;
    validate_directory_snapshot(&root, &snapshot)?;
    let root_id = snapshot.identity;
    if !root_id.is_strong() { return Err(work_root_refusal()); }
    #[cfg(windows)]
    let root = snapshot.final_path;
    let home = canonicalize_allow_missing(home_dir).map_err(|_| work_root_refusal())?;
    let runs = [
        std::path::absolute(home_dir).map_err(|_| work_root_refusal())?.join("run"),
        home.join("run"),
        canonicalize_allow_missing(&home_dir.join("run")).map_err(|_| work_root_refusal())?,
    ];
    for run in &runs {
        // Local and SMB identities must not be compared even when both APIs
        // return 128-bit IDs. This also refuses a local home redirected to SMB.
        validate_windows_work_root(run)?;
        for candidate in [work_root, root.as_path()] {
            if starts_with(run, candidate).map_err(|_| work_root_refusal())? {
                return Err(work_root_refusal());
            }
        }
        for ancestor in run.ancestors() {
            // Resolve aliases before opening, then require the handle's final
            // local path to still match. Only absent protected ancestors may
            // be skipped, never failures obtaining an opened target's ID.
            #[cfg(windows)]
            let resolved = match canonicalize(ancestor) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(directory_identity_refusal(error)),
            };
            #[cfg(windows)]
            let ancestor = resolved.as_path();
            match identify(ancestor) {
                Ok(snapshot) => {
                    validate_directory_snapshot(ancestor, &snapshot)?;
                    if !snapshot.identity.is_strong() || snapshot.identity == root_id {
                        return Err(work_root_refusal());
                    }
                },
                #[cfg(not(windows))]
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(directory_identity_refusal(error)),
            }
        }
    }
    Ok(CheckedWorkRoot { path: root, identity: root_id })
}

/// Descriptor-relative directory access that never follows a symbolic link
/// in the final component (`openat`/`mkdirat`/`fstatat`/`unlinkat`).
#[cfg(unix)]
mod at {
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    #[derive(Debug)]
    pub(super) struct Dir(File);

    fn component(name: &OsStr) -> io::Result<CString> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a single path component"));
        }
        CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in a path component"))
    }

    fn check(rc: libc::c_int) -> io::Result<libc::c_int> {
        if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(rc) }
    }

    /// Does the opened handle's `meta` belong to the entry `stat` described?
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn same_entry(meta: &std::fs::Metadata, stat: &libc::stat) -> bool {
        use std::os::unix::fs::MetadataExt;
        meta.dev() == stat.st_dev as u64 && meta.ino() == stat.st_ino as u64
    }

    impl Dir {
        /// Open `path` as a directory; `follow_last = false` refuses a
        /// symbolic link as its final component (`O_NOFOLLOW`).
        pub(super) fn open(path: &Path, follow_last: bool) -> io::Result<Self> {
            let mut flags = libc::O_DIRECTORY | libc::O_CLOEXEC;
            if !follow_last { flags |= libc::O_NOFOLLOW; }
            Ok(Self(std::fs::OpenOptions::new().read(true).custom_flags(flags).open(path)?))
        }

        pub(super) fn file(&self) -> &File {
            &self.0
        }

        fn open_at(&self, name: &OsStr, flags: libc::c_int, mode: u32) -> io::Result<File> {
            let name = component(name)?;
            // SAFETY: a valid directory descriptor and a NUL-terminated single component.
            let fd = check(unsafe { libc::openat(self.0.as_raw_fd(), name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC, mode as libc::c_uint) })?;
            // SAFETY: openat returned a new descriptor nobody else owns.
            Ok(unsafe { File::from_raw_fd(fd) })
        }

        /// The child directory `name`; a link there fails (`ELOOP`/`ENOTDIR`).
        pub(super) fn child(&self, name: &OsStr) -> io::Result<Self> {
            Ok(Self(self.open_at(name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?))
        }

        pub(super) fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
            let name = component(name)?;
            // SAFETY: as in `open_at`.
            check(unsafe { libc::mkdirat(self.0.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) }).map(drop)
        }

        /// Create `name` exclusively (never an existing entry or a link).
        pub(super) fn create_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
            self.open_at(name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, mode)
        }

        /// Open an existing non-link entry for reading (non-blocking: a FIFO
        /// planted there cannot stall the caller).
        pub(super) fn open_file(&self, name: &OsStr) -> io::Result<File> {
            self.open_at(name, libc::O_RDONLY | libc::O_NONBLOCK, 0)
        }

        /// `lstat` of the entry `name` (a link is described, not followed).
        pub(super) fn lstat(&self, name: &OsStr) -> io::Result<libc::stat> {
            let name = component(name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: as in `open_at`; `stat` is written by a successful call.
            check(unsafe { libc::fstatat(self.0.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) })?;
            // SAFETY: fstatat succeeded.
            Ok(unsafe { stat.assume_init() })
        }

        fn unlink(&self, name: &OsStr, directory: bool) -> io::Result<()> {
            let name = component(name)?;
            let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
            // SAFETY: as in `open_at`.
            check(unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), flags) }).map(drop)
        }

        /// Entry names of this directory (without `.` and `..`).
        pub(super) fn list(&self) -> io::Result<Vec<OsString>> {
            let dot = CString::new(".").expect("no NUL");
            // A separate open file description keeps the listing offset private.
            // SAFETY: a valid directory descriptor and a NUL-terminated name.
            let fd = check(unsafe { libc::openat(self.0.as_raw_fd(), dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) })?;
            // SAFETY: `fd` is a directory descriptor; the stream owns it from here.
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: fdopendir failed, so `fd` is still ours to close.
                unsafe { libc::close(fd); }
                return Err(error);
            }
            let mut names = Vec::new();
            loop {
                // End of stream and a read error both return NULL. A short
                // listing only leaves entries behind: the final
                // `unlinkat(AT_REMOVEDIR)` then fails as not empty and the
                // next sweep retries.
                // SAFETY: `stream` is a valid open directory stream.
                let entry = unsafe { libc::readdir(stream) };
                if entry.is_null() { break; }
                // SAFETY: readdir returned a valid entry with a NUL-terminated name.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if name != b"." && name != b".." {
                    names.push(OsString::from_vec(name.to_vec()));
                }
            }
            // SAFETY: closes the stream and its descriptor exactly once.
            unsafe { libc::closedir(stream); }
            Ok(names)
        }

        /// Remove the entry `name` and, when it is a real directory,
        /// everything in it. A symbolic link is unlinked as a link, never
        /// followed. A directory is entered only through an `O_NOFOLLOW` open
        /// whose device and inode match the entry just inspected, so an entry
        /// swapped in between is not followed. `expected`, when given, is the
        /// identity the entry must still have.
        pub(super) fn remove_tree(&self, name: &OsStr, expected: Option<&std::fs::Metadata>) -> io::Result<()> {
            self.remove_tree_at(name, expected, 0)
        }

        fn remove_tree_at(&self, name: &OsStr, expected: Option<&std::fs::Metadata>, depth: usize) -> io::Result<()> {
            let changed = || io::Error::other("Codex call directory entry changed during removal");
            let stat = self.lstat(name)?;
            if expected.is_some_and(|meta| !same_entry(meta, &stat)) {
                return Err(changed());
            }
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return self.unlink(name, false);
            }
            if depth >= super::MAX_REMOVE_DEPTH {
                return Err(io::Error::other("Codex call directory nested too deeply"));
            }
            let child = self.child(name)?;
            if !same_entry(&child.0.metadata()?, &stat) {
                return Err(changed());
            }
            for entry in child.list()? {
                child.remove_tree_at(&entry, None, depth + 1)?;
            }
            self.unlink(name, true)
        }
    }
}

/// Unix: open `<home>` (the operator's choice, which may itself be a link),
/// then `run` and `run/codex` relative to it with `O_NOFOLLOW`, each created
/// 0700 when absent, owned by the gateway user and restricted to 0700 on the
/// opened handle (F4 c).
#[cfg(unix)]
fn open_private_parent(home_dir: &Path) -> Result<at::Dir, String> {
    let home = at::Dir::open(home_dir, true)
        .map_err(|e| format!("cannot open the DuDuClaw data folder: {e}"))?;
    let run = private_child(&home, "run")?;
    private_child(&run, "codex")
}

#[cfg(unix)]
fn private_child(dir: &at::Dir, name: &str) -> Result<at::Dir, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    match dir.mkdir(std::ffi::OsStr::new(name), 0o700) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create Codex run directory: {e}")),
    }
    let child = dir.child(std::ffi::OsStr::new(name))
        .map_err(|e| format!("Codex run directory is not a plain directory — refusing to use it ({e})"))?;
    let meta = child.file().metadata()
        .map_err(|e| format!("cannot inspect Codex run directory: {e}"))?;
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err("Codex run directory is not owned by the gateway user — refusing to use it".into());
    }
    // `fchmod` on the handle: the mode lands on exactly this directory.
    child.file().set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot restrict Codex run directory: {e}"))?;
    let mode = child.file().metadata()
        .map_err(|e| format!("cannot inspect Codex run directory: {e}"))?.mode();
    if mode & 0o777 != 0o700 {
        return Err("Codex run directory could not be restricted to its owner — refusing to use it".into());
    }
    Ok(child)
}

/// Non-Unix platforms have no `openat`-style calls in this module: the
/// parent is created and checked by pathname (the previous behaviour).
#[cfg(not(unix))]
fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    match std::fs::DirBuilder::new().create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create Codex run directory: {e}")),
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot inspect Codex run directory: {e}"))?;
    if !meta.file_type().is_dir() {
        return Err("Codex run directory is not a plain directory — refusing to use it".into());
    }
    Ok(())
}

/// `<home>/run/codex`, created and restricted to 0700. (Unix calls open the
/// parent handle directly; this pathname form serves the tests there.)
#[cfg_attr(all(unix, not(test)), allow(dead_code))]
pub(crate) fn ensure_run_parent(home_dir: &Path) -> Result<PathBuf, String> {
    #[cfg(unix)]
    open_private_parent(home_dir)?;
    #[cfg(not(unix))]
    {
        ensure_private_dir(&home_dir.join("run"))?;
        ensure_private_dir(&run_parent(home_dir))?;
    }
    Ok(run_parent(home_dir))
}

fn random_name(prefix: &str) -> String {
    use rand::RngCore;
    let mut bytes = [0u8; NAME_HEX_LEN / 2];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

/// Does `name` follow this module's naming scheme exactly?
pub(crate) fn matches_scheme(name: &str) -> bool {
    [CALL_PREFIX, UTILITY_PREFIX].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|rest| rest.len() == NAME_HEX_LEN
            && rest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    })
}

fn last_sweeps() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Instant>> {
    static LAST: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, Instant>>> = std::sync::OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// Claim the sweep slot of `parent` when the last sweep is at least
/// [`SWEEP_INTERVAL`] old (or there was none).
fn claim_sweep(parent: &Path) -> bool {
    let mut last = last_sweeps().lock().unwrap_or_else(|e| e.into_inner());
    match last.get(parent) {
        Some(at) if at.elapsed() < SWEEP_INTERVAL => false,
        _ => {
            last.insert(parent.to_path_buf(), Instant::now());
            true
        }
    }
}

/// Take the call lock on a freshly opened `lock` file without waiting.
fn try_lock(file: &std::fs::File) -> std::io::Result<()> {
    fs2::FileExt::try_lock_exclusive(file)
}

/// One call's private directory under `<home>/run/codex`; removed on drop.
/// Holds the directory's exclusive [`LOCK_FILE`] lock until then.
#[derive(Debug)]
pub(crate) struct OwnedCallDir {
    path: PathBuf,
    #[cfg(unix)]
    name: String,
    /// The parent handle every operation on this directory is relative to.
    #[cfg(unix)]
    parent: at::Dir,
    /// Held (never read) for the directory's lifetime; released after removal.
    #[cfg_attr(unix, allow(dead_code))]
    lock: Option<std::fs::File>,
}

impl OwnedCallDir {
    pub(crate) fn create(home_dir: &Path, prefix: &str) -> Result<Self, String> {
        let parent_path = run_parent(home_dir);
        #[cfg(unix)]
        {
            let parent = open_private_parent(home_dir)?;
            if claim_sweep(&parent_path) {
                sweep_dir(&parent, STALE_AFTER);
            }
            for _ in 0..8 {
                let name = random_name(prefix);
                // `mkdirat`, not `create_all`: an existing entry (or a planted
                // link) is never reused.
                match parent.mkdir(std::ffi::OsStr::new(&name), 0o700) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(format!("cannot create Codex call directory: {e}")),
                }
                return match lock_new_call_dir(&parent, &name) {
                    Ok(lock) => Ok(Self { path: parent_path.join(&name), name, parent, lock: Some(lock) }),
                    Err(e) => {
                        let _ = parent.remove_tree(std::ffi::OsStr::new(&name), None);
                        Err(format!("cannot prepare Codex call directory: {e}"))
                    }
                };
            }
        }
        #[cfg(not(unix))]
        {
            ensure_run_parent(home_dir)?;
            if claim_sweep(&parent_path) {
                sweep_older_than(&parent_path, STALE_AFTER);
            }
            for _ in 0..8 {
                let path = parent_path.join(random_name(prefix));
                match std::fs::DirBuilder::new().create(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(format!("cannot create Codex call directory: {e}")),
                }
                let lock = std::fs::OpenOptions::new().read(true).write(true).create_new(true)
                    .open(path.join(LOCK_FILE))
                    .and_then(|file| try_lock(&file).map(|()| file));
                return match lock {
                    Ok(lock) => Ok(Self { path, lock: Some(lock) }),
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&path);
                        Err(format!("cannot prepare Codex call directory: {e}"))
                    }
                };
            }
        }
        Err("cannot create a unique Codex call directory".into())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Restrict the new directory, create its lock file and take the lock.
#[cfg(unix)]
fn lock_new_call_dir(parent: &at::Dir, name: &str) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::PermissionsExt;
    let dir = parent.child(std::ffi::OsStr::new(name))?;
    dir.file().set_permissions(std::fs::Permissions::from_mode(0o700))?;
    let lock = dir.create_file(std::ffi::OsStr::new(LOCK_FILE), 0o600)?;
    try_lock(&lock)?;
    Ok(lock)
}

impl Drop for OwnedCallDir {
    fn drop(&mut self) {
        // Unix: relative to the parent handle, links unlinked as links and
        // never followed; `self.lock` is released after this (field drop),
        // so no sweeper ever sees this directory unlocked while it exists.
        #[cfg(unix)]
        let result = self.parent.remove_tree(std::ffi::OsStr::new(&self.name), None);
        // Elsewhere the open lock file would block its own deletion: release
        // first. `remove_dir_all` does not follow links or junctions.
        #[cfg(not(unix))]
        let result = {
            drop(self.lock.take());
            std::fs::remove_dir_all(&self.path)
        };
        if let Err(e) = result {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, "could not remove a Codex call directory; the next sweep retries");
            }
        }
    }
}

/// Remove call directories a stopped gateway left behind. Called at gateway
/// start; returns how many were removed. A directory whose lock another
/// running gateway holds (rolling restart) is skipped whatever its age.
pub fn sweep_stale(home_dir: &Path) -> usize {
    let parent = run_parent(home_dir);
    last_sweeps().lock().unwrap_or_else(|e| e.into_inner()).insert(parent.clone(), Instant::now());
    sweep_older_than(&parent, STALE_AFTER)
}

fn is_older_than(meta: &std::fs::Metadata, max_age: Duration) -> bool {
    meta.modified().ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= max_age)
}

pub(crate) fn sweep_older_than(parent: &Path, max_age: Duration) -> usize {
    #[cfg(unix)]
    {
        // `O_NOFOLLOW`: a link as the parent itself is not followed.
        let Ok(dir) = at::Dir::open(parent, false) else { return 0 };
        sweep_dir(&dir, max_age)
    }
    #[cfg(not(unix))]
    {
        sweep_by_path(parent, max_age)
    }
}

#[cfg(all(test, unix))]
thread_local! {
    /// Test hook run right after the sweeper listed the parent.
    static AFTER_LISTING: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
fn sweep_dir(parent: &at::Dir, max_age: Duration) -> usize {
    let Ok(names) = parent.list() else { return 0 };
    #[cfg(test)]
    AFTER_LISTING.with(|hook| if let Some(hook) = hook.borrow_mut().as_mut() { hook() });
    names.iter()
        .filter(|name| name.to_str().is_some_and(matches_scheme))
        .filter(|name| sweep_entry(parent, name, max_age))
        .count()
}

/// Remove one listed entry when it is a real directory, nobody holds its
/// lock and it is older than `max_age`. Everything is relative to `parent`.
#[cfg(unix)]
fn sweep_entry(parent: &at::Dir, name: &std::ffi::OsStr, max_age: Duration) -> bool {
    // Links (and plain files) are ignored, never followed or removed.
    let Ok(stat) = parent.lstat(name) else { return false };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return false;
    }
    // Entered only through an O_NOFOLLOW open that is the entry just inspected.
    let Ok(dir) = parent.child(name) else { return false };
    let Ok(meta) = dir.file().metadata() else { return false };
    if !at::same_entry(&meta, &stat) || !is_older_than(&meta, max_age) {
        return false;
    }
    // A call in any gateway process holds this lock for its whole lifetime:
    // take it without waiting, or leave the directory alone. No lock file at
    // all (a crash between `mkdirat` and its creation) leaves the age alone
    // to decide.
    let _held = match dir.open_file(std::ffi::OsStr::new(LOCK_FILE)) {
        Ok(file) => match try_lock(&file) {
            Ok(()) => Some(file),
            Err(_) => return false,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return false,
    };
    parent.remove_tree(name, Some(&meta)).is_ok()
}

/// Non-Unix: by pathname (no `*at` calls); the lock is checked the same way.
#[cfg(not(unix))]
fn sweep_by_path(parent: &Path, max_age: Duration) -> usize {
    // The parent itself must be a real directory: a link there is not followed.
    match std::fs::symlink_metadata(parent) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return 0,
    }
    let Ok(entries) = std::fs::read_dir(parent) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(matches_scheme) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_dir() || !is_older_than(&meta, max_age) {
            continue;
        }
        match std::fs::OpenOptions::new().read(true).open(path.join(LOCK_FILE)) {
            // Released before removal: an open file cannot be deleted here.
            Ok(file) => if try_lock(&file).is_err() { continue },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => continue,
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_directory_is_private_random_and_under_the_run_parent() {
        let home = tempfile::tempdir().unwrap();
        let a = OwnedCallDir::create(home.path(), CALL_PREFIX).unwrap();
        let b = OwnedCallDir::create(home.path(), CALL_PREFIX).unwrap();
        for dir in [&a, &b] {
            assert_eq!(dir.path().parent(), Some(run_parent(home.path()).as_path()));
            let name = dir.path().file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with(CALL_PREFIX) && matches_scheme(name), "{name}");
            assert!(dir.path().is_dir());
            assert!(dir.path().join(LOCK_FILE).is_file(), "each call directory holds its lock file");
        }
        assert_ne!(a.path(), b.path(), "names are random, not sequential");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [a.path(), run_parent(home.path()).as_path(), home.path().join("run").as_path()] {
                assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o700, "{path:?}");
            }
        }
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists(), "removed when the guard drops");
    }

    #[test]
    fn naming_scheme_is_exact() {
        let hex = "0123456789abcdef0123456789abcdef";
        assert!(matches_scheme(&format!("call-{hex}")));
        assert!(matches_scheme(&format!("utility-{hex}")));
        for name in [format!("call-{}", &hex[1..]), format!("call-{hex}0"), format!("call-{}", hex.to_uppercase()),
            format!("other-{hex}"), "duduclaw-codex-config-abc".to_string(), format!("call-{hex}/x")] {
            assert!(!matches_scheme(&name), "{name}");
        }
    }

    #[tokio::test]
    async fn cancelled_call_removes_its_directory() {
        let home = tempfile::tempdir().unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let call = {
            let home = home.path().to_path_buf();
            async move {
                let dir = OwnedCallDir::create(&home, CALL_PREFIX).unwrap();
                sender.send(dir.path().to_path_buf()).unwrap();
                std::future::pending::<()>().await;
                drop(dir);
            }
        };
        let task = tokio::spawn(call);
        let path = receiver.await.unwrap();
        assert!(path.is_dir());
        task.abort();
        let _ = task.await;
        assert!(!path.exists(), "a cancelled call leaves nothing behind");
    }

    #[test]
    fn a_planted_parent_link_is_refused() {
        #[cfg(unix)]
        {
            let home = tempfile::tempdir().unwrap();
            let elsewhere = tempfile::tempdir().unwrap();
            std::fs::create_dir(home.path().join("run")).unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), run_parent(home.path())).unwrap();
            assert!(OwnedCallDir::create(home.path(), CALL_PREFIX).is_err());
            assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
            // A link as `<home>/run` itself is refused the same way (F4).
            let home = tempfile::tempdir().unwrap();
            let elsewhere = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), home.path().join("run")).unwrap();
            assert!(OwnedCallDir::create(home.path(), CALL_PREFIX).is_err());
            assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn sweeper_removes_only_matching_stale_directories_and_ignores_links() {
        let home = tempfile::tempdir().unwrap();
        let parent = ensure_run_parent(home.path()).unwrap();
        let hex = "00112233445566778899aabbccddeeff";
        let stale_call = parent.join(format!("call-{hex}"));
        let stale_utility = parent.join(format!("utility-{hex}"));
        for dir in [&stale_call, &stale_utility] {
            std::fs::create_dir_all(dir.join("codex-home")).unwrap();
            std::fs::write(dir.join("codex-home/AGENTS.md"), "old prompt").unwrap();
        }
        // An unlocked lock file left by a stopped gateway does not keep a directory.
        std::fs::write(stale_call.join(LOCK_FILE), "").unwrap();
        let foreign = parent.join("keep-me");
        std::fs::create_dir(&foreign).unwrap();
        let file = parent.join(format!("call-{}", "f".repeat(32)));
        std::fs::write(&file, "not a directory").unwrap();
        let live_dir = OwnedCallDir::create(home.path(), CALL_PREFIX).unwrap();
        #[cfg(unix)]
        let (target, link, inner_target) = {
            let target = tempfile::tempdir().unwrap();
            std::fs::write(target.path().join("precious"), "x").unwrap();
            let link = parent.join(format!("call-{}", "e".repeat(32)));
            std::os::unix::fs::symlink(target.path(), &link).unwrap();
            // A link INSIDE a stale directory is removed as a link only.
            let inner_target = tempfile::tempdir().unwrap();
            std::fs::write(inner_target.path().join("auth.json"), "token").unwrap();
            std::os::unix::fs::symlink(inner_target.path().join("auth.json"), stale_call.join("codex-home/auth.json")).unwrap();
            std::os::unix::fs::symlink(inner_target.path(), stale_utility.join("codex-home/linked-dir")).unwrap();
            (target, link, inner_target)
        };
        // Fresh entries survive the real threshold.
        assert_eq!(sweep_older_than(&parent, STALE_AFTER), 0);
        assert!(stale_call.exists() && stale_utility.exists());
        // Everything is "old" at a zero threshold: only the matching, unlocked directories go.
        assert_eq!(sweep_older_than(&parent, Duration::ZERO), 2);
        assert!(!stale_call.exists() && !stale_utility.exists());
        assert!(foreign.is_dir() && file.is_file() && live_dir.path().is_dir());
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
            assert!(target.path().join("precious").exists(), "a link is never followed");
            assert!(inner_target.path().join("auth.json").exists(), "a link inside is unlinked, not followed");
        }
        // A link as the parent itself is not followed either.
        #[cfg(unix)]
        {
            let other = tempfile::tempdir().unwrap();
            let victim = other.path().join(format!("call-{hex}"));
            std::fs::create_dir(&victim).unwrap();
            let linked_parent = home.path().join("linked");
            std::os::unix::fs::symlink(other.path(), &linked_parent).unwrap();
            assert_eq!(sweep_older_than(&linked_parent, Duration::ZERO), 0);
            assert!(victim.exists());
        }
    }

    /// F3: a call directory whose lock another holder has (a second gateway
    /// during a rolling restart, simulated by a second file handle) is skipped
    /// whatever its age; once released, the stale directory is removed.
    #[test]
    fn a_locked_call_directory_is_skipped_regardless_of_age() {
        let home = tempfile::tempdir().unwrap();
        let parent = ensure_run_parent(home.path()).unwrap();
        let dir = parent.join(format!("call-{}", "1".repeat(32)));
        std::fs::create_dir_all(dir.join("codex-home")).unwrap();
        std::fs::write(dir.join("codex-home/AGENTS.md"), "running call").unwrap();
        std::fs::write(dir.join(LOCK_FILE), "").unwrap();
        let holder = std::fs::OpenOptions::new().read(true).write(true).open(dir.join(LOCK_FILE)).unwrap();
        fs2::FileExt::try_lock_exclusive(&holder).unwrap();
        assert_eq!(sweep_older_than(&parent, Duration::ZERO), 0);
        assert!(dir.join("codex-home/AGENTS.md").is_file(), "a locked call keeps its files");
        assert_eq!(sweep_stale(home.path()), 0);
        assert!(dir.is_dir());
        drop(holder);
        assert_eq!(sweep_older_than(&parent, Duration::ZERO), 1);
        assert!(!dir.exists(), "an unlocked stale directory is removed");
        // A live guard's own lock protects it the same way, from this process too.
        let live = OwnedCallDir::create(home.path(), UTILITY_PREFIX).unwrap();
        assert_eq!(sweep_older_than(&parent, Duration::ZERO), 0);
        assert!(live.path().join(LOCK_FILE).is_file());
    }

    /// F3: an entry replaced by a symbolic link after the sweeper listed the
    /// parent is not followed: nothing behind the link is touched.
    #[cfg(unix)]
    #[test]
    fn a_link_planted_between_listing_and_removal_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        let parent = ensure_run_parent(home.path()).unwrap();
        let entry = parent.join(format!("call-{}", "a".repeat(32)));
        std::fs::create_dir_all(entry.join("codex-home")).unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::create_dir(target.path().join("codex-home")).unwrap();
        std::fs::write(target.path().join("codex-home/precious"), "x").unwrap();
        let moved = home.path().join("moved-away");
        let (from, to, link_target) = (entry.clone(), moved.clone(), target.path().to_path_buf());
        AFTER_LISTING.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::rename(&from, &to).unwrap();
            std::os::unix::fs::symlink(&link_target, &from).unwrap();
        })));
        let removed = sweep_older_than(&parent, Duration::ZERO);
        AFTER_LISTING.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(removed, 0);
        assert!(std::fs::symlink_metadata(&entry).unwrap().file_type().is_symlink(), "the link is left as it is");
        assert!(target.path().join("codex-home/precious").is_file(), "nothing behind the link is removed");
        assert!(moved.join("codex-home").is_dir());
    }

    #[test]
    fn startup_sweep_claims_the_hourly_slot() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(sweep_stale(home.path()), 0, "absent parent: nothing to do");
        assert!(!claim_sweep(&run_parent(home.path())), "at most once an hour");
        let other = tempfile::tempdir().unwrap();
        assert!(claim_sweep(&run_parent(other.path())), "per parent");
    }

    #[test]
    fn protected_roots_name_the_run_directory_the_run_parent_and_the_host_store() {
        let home = tempfile::tempdir().unwrap();
        let roots = protected_roots(home.path());
        assert!(roots.contains(&home.path().join("run")), "{roots:?}");
        assert!(roots.contains(&run_parent(home.path())));
        if let Some(host) = host_codex_home().filter(|h| h.is_absolute()) {
            assert!(roots.contains(&host), "{roots:?}");
        }
        assert!(roots.iter().all(|root| root.is_absolute()));
        // Once it exists, the canonical spelling of `<home>/run` is listed too.
        ensure_run_parent(home.path()).unwrap();
        let canonical = std::fs::canonicalize(home.path().join("run")).unwrap();
        assert!(protected_roots(home.path()).contains(&canonical));
    }

    /// F4 (a): a working root equal to, above, or otherwise containing the
    /// system data folder is refused; an employee directory is accepted.
    #[test]
    fn a_working_root_that_contains_the_system_folder_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("duduclaw");
        let agent = home.join("agents").join("worker");
        std::fs::create_dir_all(&agent).unwrap();
        ensure_run_parent(&home).unwrap();
        let mut refused = vec![home.clone(), base.path().to_path_buf(), home.join("run"), PathBuf::from("/")];
        #[cfg(unix)]
        {
            // Through a link: resolved before the comparison.
            let link = base.path().join("alias");
            std::os::unix::fs::symlink(&home, &link).unwrap();
            refused.push(link);
        }
        for root in &refused {
            let error = check_work_root(&home, root).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error), "{root:?}: {error}");
            assert!(error.contains("系統資料夾") && !error.contains(home.to_str().unwrap()), "{error}");
        }
        check_work_root(&home, &agent).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        check_work_root(&home, elsewhere.path()).unwrap();
        // The utility working directory lives inside `<home>/run/codex`: accepted.
        let utility = OwnedCallDir::create(&home, UTILITY_PREFIX).unwrap();
        check_work_root(&home, utility.path()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_work_roots_match_case_verbatim_and_the_home_drive_root() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("MixedCaseHome");
        let agent = home.join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        ensure_run_parent(&home).unwrap();
        let canonical = std::fs::canonicalize(&home).unwrap();
        let upper = PathBuf::from(canonical.to_str().unwrap().to_uppercase());
        for root in [canonical.clone(), upper.clone(), upper.join("RUN"),
            canonical.ancestors().last().unwrap().to_path_buf()] {
            let error = check_work_root(&home, &root).unwrap_err();
            assert_eq!(error, duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(WORK_ROOT_CONTAINS_HOME));
        }
        check_work_root(&upper, &agent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_missing_run_is_resolved_through_existing_home() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("MixedCaseHome");
        let agent = home.join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        let canonical = std::fs::canonicalize(&home).unwrap();
        let upper = PathBuf::from(canonical.to_str().unwrap().to_uppercase());
        assert!(!home.join("run").exists());
        for root in [upper.clone(), upper.join("RUN"), base.path().to_path_buf()] {
            assert!(check_work_root(&home, &root).is_err());
        }
        check_work_root(&upper, &agent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_unresolvable_home_or_work_root_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let invalid = home.path().join("invalid\0path");
        for (home, root) in [(invalid.as_path(), other.path()), (home.path(), invalid.as_path())] {
            let error = check_work_root(home, root).unwrap_err();
            assert_eq!(error, duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(WORK_ROOT_CONTAINS_HOME));
        }
    }

    #[test]
    fn work_root_removed_between_resolution_and_identity_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        let canonical = duduclaw_core::path_compare::canonicalize(&root).unwrap();
        let mut first = true;
        let error = check_work_root_with_identity(&home, &root, |path| {
            if first {
                first = false;
                assert_eq!(path, canonical);
                std::fs::remove_dir(path).unwrap();
            }
            directory_identity(path)
        }).unwrap_err();
        assert!(!first, "identity lookup must follow canonicalization");
        assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
    }

    #[test]
    fn checked_work_root_refuses_replacement_before_spawn() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        let checked = check_work_root(&home, &root).unwrap();
        checked.reverify().unwrap();
        // Keep the original inode/file ID live to prevent immediate ID reuse.
        std::fs::rename(&root, base.path().join("original")).unwrap();
        assert!(checked.reverify().is_err(), "missing root must refuse");
        std::fs::create_dir(&root).unwrap();
        let error = checked.reverify().unwrap_err();
        assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_work_root_stores_handle_final_path() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        let checked = check_work_root(&home, &root).unwrap();
        let (final_path, identity) = duduclaw_core::path_compare::windows_directory_identity(&root).unwrap();
        assert_eq!(checked.canonical_path(), final_path);
        assert!(checked.canonical_path().to_str().unwrap().starts_with(r"\\?\"));
        assert_eq!(checked.identity, identity);
        assert!(identity.is_strong());
        assert_eq!(checked.spawn_path(), duduclaw_core::path_compare::spawn_path(&final_path));
        let mut command = tokio::process::Command::new("fixture-never-spawned");
        checked.apply_to_command(&mut command);
        assert_eq!(command.as_std().get_current_dir(), Some(checked.spawn_path()));
        assert_eq!(command.as_std().get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("--cd"), checked.spawn_path().as_os_str()]);
        assert_eq!(checked.canonical_path(), final_path);
        checked.reverify().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_symlink_work_root_checks_final_target() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        let link = base.path().join("alias");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::os::windows::fs::symlink_dir(&root, &link)
            .expect("directory symlink test requires Windows Developer Mode or symlink privilege");
        let checked = check_work_root(&home, &link).unwrap();
        let (final_path, _) = duduclaw_core::path_compare::windows_directory_identity(&root).unwrap();
        assert_eq!(checked.canonical_path(), final_path);
        checked.reverify().unwrap();
        std::fs::remove_dir(&link).unwrap();
        std::os::windows::fs::symlink_dir(base.path(), &link).unwrap();
        let error = check_work_root(&home, &link).unwrap_err();
        assert_eq!(error, work_root_refusal());
        std::fs::remove_dir(&link).unwrap();
    }

    #[cfg(windows)]
    fn create_test_junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"]).arg(link).arg(target).output().unwrap();
        assert!(output.status.success(), "mklink /J failed: {} {}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    #[cfg(windows)]
    #[test]
    fn windows_reverify_refuses_junction_to_home_ancestor() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        let checked = check_work_root(&home, &root).unwrap();
        std::fs::rename(&root, base.path().join("original")).unwrap();
        create_test_junction(&root, base.path());
        assert_eq!(checked.reverify().unwrap_err(), work_root_refusal());
        std::fs::remove_dir(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_reverify_refuses_same_identity_at_changed_final_path() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        let moved = base.path().join("moved");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        let checked = check_work_root(&home, &root).unwrap();
        std::fs::rename(&root, &moved).unwrap();
        create_test_junction(&root, &moved);
        let snapshot = directory_identity(&root).unwrap();
        assert_eq!(snapshot.identity, checked.identity, "identity alone would still accept");
        assert_eq!(checked.reverify().unwrap_err(), work_root_refusal());
        std::fs::remove_dir(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_work_root_redirected_after_canonicalization_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        let other = base.path().join("other");
        for path in [&home, &root, &other] { std::fs::create_dir(path).unwrap(); }
        let mut calls = 0;
        let error = check_work_root_with_identity(&home, &root, |path| {
            calls += 1;
            if calls == 1 {
                std::fs::rename(&root, base.path().join("original")).unwrap();
                create_test_junction(&root, &other);
            }
            directory_identity(path)
        }).unwrap_err();
        assert_eq!(calls, 1, "final path mismatch must refuse before ancestor comparison");
        assert_eq!(error, work_root_refusal());
        std::fs::remove_dir(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_ancestor_redirected_after_canonicalization_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let root = base.path().join("workspace");
        let other = base.path().join("other");
        for path in [&home, &root, &other] { std::fs::create_dir(path).unwrap(); }
        // `run` is absent: the first existing ancestor opened is `home`.
        let mut calls = 0;
        let error = check_work_root_with_identity(&home, &root, |path| {
            calls += 1;
            if calls == 2 {
                assert!(duduclaw_core::path_compare::equal(path, &home).unwrap());
                std::fs::rename(&home, base.path().join("original-home")).unwrap();
                create_test_junction(&home, &other);
            }
            directory_identity(path)
        }).unwrap_err();
        assert_eq!(calls, 2, "ancestor final path mismatch must refuse immediately");
        assert_eq!(error, work_root_refusal());
        std::fs::remove_dir(&home).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_relative_and_root_relative_work_roots_are_refused() {
        let home = tempfile::tempdir().unwrap();
        for root in ["C:", r"C:foo", r"\foo", "/foo", "foo", "/", r"\"] {
            let error = check_work_root(home.path(), Path::new(root)).unwrap_err();
            assert_eq!(error, duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(
                WORK_ROOT_MUST_BE_ABSOLUTE));
            assert!(!error.contains(home.path().to_str().unwrap()));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_unc_work_roots_are_refused_before_filesystem_access() {
        let home = tempfile::tempdir().unwrap();
        for root in [r"\\server\share\x", r"\\?\UNC\s\sh\x", r"\\localhost\C$\home"] {
            let error = check_work_root(home.path(), Path::new(root)).unwrap_err();
            assert_eq!(error, duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(
                WORK_ROOT_NETWORK_FOLDER));
            assert!(!error.contains(root));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_weak_identity_cannot_accept_a_work_root() {
        use duduclaw_core::path_compare::FileIdentity;
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        for weak_at in [0, 1] {
            let mut calls = 0;
            let error = check_work_root_with_identity(home.path(), workspace.path(), |path| {
                let weak = calls == weak_at;
                calls += 1;
                let mut snapshot = directory_identity(path)?;
                if weak { snapshot.identity = FileIdentity::WindowsLegacy { volume: 1, index: 2 }; }
                Ok(snapshot)
            }).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
        }
    }
}
