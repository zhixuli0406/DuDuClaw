//! Private workspace materialization, quotas and host-held integrity guards.
use super::config::DiscoveryConfig;
use duduclaw_fork::CopyPolicy;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Host-owned discovery directories must be private independently of umask.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(not(unix))]
    return Err(io::Error::other("owner-only discovery directories require a verified platform ACL"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
        let mut current = PathBuf::new();
        let mut missing = Vec::new();
        for part in absolute.components() {
            if matches!(part, std::path::Component::ParentDir) {
                return Err(io::Error::other("parent traversal in private directory"));
            }
            current.push(part.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {},
                // macOS exposes its system-owned temporary roots through
                // /var and /tmp aliases. Application-controlled links are
                // still refused at every other path component.
                #[cfg(target_os = "macos")]
                Ok(meta) if meta.file_type().is_symlink() && meta.uid() == 0
                    && matches!(current.to_str(), Some("/var" | "/tmp"))
                    && fs::metadata(&current)?.is_dir() => {},
                Ok(_) => return Err(io::Error::other("private directory contains a link or non-directory")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(current.clone()),
                Err(error) => return Err(error),
            }
        }
        for directory in missing {
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {},
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                Err(error) => return Err(error),
            }
            let meta = fs::symlink_metadata(&directory)?;
            if !meta.is_dir() || meta.file_type().is_symlink()
                || meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o777 != 0o700 {
                return Err(io::Error::other("private directory ownership or permissions rejected"));
            }
        }
        let meta = fs::symlink_metadata(&absolute)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o777 != 0o700 {
            return Err(io::Error::other("existing discovery directory is not owner-only"));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod private_directory_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn new_discovery_directories_are_owner_only_without_relying_on_umask() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("runs/new-run");
        create_private_directory(&target).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(target.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn preexisting_shared_or_symlinked_discovery_directory_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let shared = parent.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(create_private_directory(&shared).is_err());
        let link = parent.path().join("linked");
        std::os::unix::fs::symlink(parent.path(), &link).unwrap();
        assert!(create_private_directory(&link.join("child")).is_err());
    }
}

#[derive(Debug)]
pub struct PreparedWorkspace {
    pub workspace: PathBuf,
    pub guard: IntegrityGuard,
}
#[derive(Debug, Clone)]
pub struct IntegrityGuard {
    root: PathBuf,
    hashes: BTreeMap<PathBuf, String>,
    filtered: bool,
}
impl IntegrityGuard {
    pub fn capture(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            hashes: manifest(root)?,
            filtered: false,
        })
    }
    pub fn verify(&self) -> io::Result<()> {
        let current = if self.filtered {
            filtered_manifest(&self.root)?
        } else {
            manifest(&self.root)?
        };
        if current != self.hashes {
            return Err(io::Error::other("immutable workspace changed"));
        }
        Ok(())
    }

    /// Bind a sanitized export to the original pre-score content, rather than
    /// to a fresh source snapshot taken after evaluation.
    pub(crate) fn verify_export(&self, exported: &Path) -> io::Result<()> {
        self.verify()?;
        let copy_policy = policy();
        let expected: BTreeMap<_, _> = self.hashes.iter()
            .filter(|(path, _)| !path.components().any(|part| copy_policy.is_excluded(part.as_os_str())))
            .map(|(path, hash)| (path.clone(), hash.clone()))
            .collect();
        if manifest(exported)? != expected {
            return Err(io::Error::other("export differs from the scored workspace"));
        }
        Ok(())
    }
}

fn discovery_root(run: &Path) -> io::Result<&Path> {
    run.ancestors()
        .find(|path| path.file_name() == Some(std::ffi::OsStr::new("discovery")))
        .ok_or_else(|| io::Error::other("run is outside the discovery storage tree"))
}

fn with_quota_lock<T>(root: &Path, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let _lock = super::attempt_container::quota_lock(root,
        std::time::Instant::now() + std::time::Duration::from_millis(500))?;
    operation()
}

fn association_id<'a>(root: &Path, path: &'a Path) -> io::Result<&'a std::ffi::OsStr> {
    let mut parts = path.strip_prefix(root).map_err(io::Error::other)?.components();
    let kind = parts.next().and_then(|p| p.as_os_str().to_str());
    if !matches!(kind, Some("runs" | "artifacts" | "policy-development" | "attempt-snapshots" | "retry-seeds")) {
        return Err(io::Error::other("unknown discovery run association"));
    }
    parts.next().map(|part| part.as_os_str()).ok_or_else(|| io::Error::other("missing run association"))
}

fn run_associated_bytes(root: &Path, path: &Path) -> io::Result<u64> {
    let run = association_id(root, path)?;
    let mut bytes = 0u64;
    for kind in ["runs", "artifacts", "policy-development", "attempt-snapshots", "retry-seeds"] {
        let association = root.join(kind).join(run);
        match fs::symlink_metadata(&association) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                bytes = bytes.checked_add(tree_bytes(&association)?).ok_or_else(|| io::Error::other("quota overflow"))?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {},
            Ok(_) => return Err(io::Error::other("invalid discovery run association")),
            Err(e) => return Err(e),
        }
    }
    let mut report = run.to_os_string(); report.push(".json");
    match fs::symlink_metadata(root.join("reports").join(report)) {
        Ok(meta) if regular_single_link(&meta) => bytes = bytes.checked_add(meta.len())
            .ok_or_else(|| io::Error::other("quota overflow"))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {},
        Ok(_) => return Err(io::Error::other("invalid discovery report association")),
        Err(e) => return Err(e),
    }
    Ok(bytes)
}

fn copy_private_workspace(source: &Path, destination: &Path) -> io::Result<PathBuf> {
    create_private_directory(destination)?;
    let result = policy().copy_tree(source, destination).map_err(io::Error::other)
        .and_then(|_| sanitize_links(source, destination))
        .and_then(|_| seal_materialized_directories(destination));
    if let Err(error) = result {
        let _ = fs::remove_dir_all(destination);
        return Err(error);
    }
    Ok(destination.to_path_buf())
}

fn policy() -> CopyPolicy {
    CopyPolicy::with_excludes(
        CopyPolicy::strict().excludes().iter().cloned().chain(
            [".env*", ".codex", ".gemini", "AGENTS.md", "GEMINI.md"]
                .into_iter()
                .map(str::to_owned),
        ),
    )
}
fn regular_single_link(meta: &fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() == 1
    }
    #[cfg(not(unix))]
    {
        // Link-count verification is unavailable through the portable API.
        false
    }
}
/// Reject symlinked ancestors, including a symlink supplied as the root itself.
pub fn canonical_real_directory(path: &Path) -> io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            #[cfg(target_os = "macos")]
            {
                let expected = match ancestor.to_str() {
                    Some("/var") => Some(Path::new("/private/var")),
                    Some("/tmp") => Some(Path::new("/private/tmp")),
                    _ => None,
                };
                if expected.is_some_and(|target| {
                    ancestor.canonicalize().is_ok_and(|actual| actual == target)
                }) {
                    continue;
                }
            }
            return Err(io::Error::other("symlinked directory is forbidden"));
        }
    }
    if !canonical.is_dir() {
        return Err(io::Error::other("expected directory"));
    }
    Ok(canonical)
}
fn walk(root: &Path, rel: &Path, out: &mut BTreeMap<PathBuf, String>) -> io::Result<()> {
    let mut entries = fs::read_dir(root.join(rel))?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let child = rel.join(entry.file_name());
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::other("symlink in integrity tree"));
        }
        if meta.is_dir() {
            out.insert(child.clone(), "directory".into());
            walk(root, &child, out)?;
        } else if regular_single_link(&meta) {
            let mut file = fs::File::open(entry.path())?;
            let mut hash = Sha256::new();
            let mut buf = [0; 16384];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
            }
            out.insert(child, format!("{:x}", hash.finalize()));
        } else {
            return Err(io::Error::other(
                "hardlink or special file in integrity tree",
            ));
        }
    }
    Ok(())
}
pub fn manifest(root: &Path) -> io::Result<BTreeMap<PathBuf, String>> {
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out)?;
    Ok(out)
}
pub fn directory_sha256(root: &Path) -> io::Result<String> {
    let mut hash = Sha256::new();
    for (path, digest) in manifest(root)? {
        let name = path.as_os_str().as_encoded_bytes();
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name);
        hash.update(digest.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn tree_bytes(root: &Path) -> io::Result<u64> {
    if !root.exists() {
        return Ok(0);
    }
    let mut size = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        let n = if meta.is_dir() {
            tree_bytes(&entry.path())?
        } else if meta.is_file() {
            meta.len()
        } else {
            0
        };
        size = size
            .checked_add(n)
            .ok_or_else(|| io::Error::other("size overflow"))?;
    }
    Ok(size)
}
fn sanitize_links(source: &Path, copy: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let dest = copy.join(entry.file_name());
        let meta = fs::symlink_metadata(entry.path())?;
        if policy().is_excluded(&entry.file_name()) {
            continue;
        }
        if meta.is_dir() {
            if dest.is_dir() {
                sanitize_links(&entry.path(), &dest)?;
            }
        } else if !regular_single_link(&meta) && fs::symlink_metadata(&dest).is_ok() {
            fs::remove_file(dest)?;
        }
    }
    Ok(())
}

fn seal_materialized_directories(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::other("materialized directory ownership rejected"));
        }
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if fs::symlink_metadata(entry.path())?.is_dir() {
                seal_materialized_directories(&entry.path())?;
            }
        }
    }
    Ok(())
}
/// Copy to the exact supplied `.../aN/ws` path. Integrity evidence stays in the host.
pub fn prepare_attempt_workspace(
    source: &Path,
    node_dir: &Path,
    run_dir: &Path,
    config: &DiscoveryConfig,
) -> io::Result<PreparedWorkspace> {
    #[cfg(not(unix))]
    return Err(io::Error::other(
        "workspace hardlink verification is unavailable on this platform",
    ));
    let source = canonical_real_directory(source)?;
    let run = canonical_real_directory(run_dir)?;
    let destination_parent = canonical_real_directory(
        node_dir
            .parent()
            .ok_or_else(|| io::Error::other("missing destination parent"))?,
    )?;
    if !destination_parent.starts_with(&run)
        || node_dir.file_name() != Some(std::ffi::OsStr::new("ws"))
        || node_dir.exists()
    {
        return Err(io::Error::other(
            "invalid or existing workspace destination",
        ));
    }
    let approved = config
        .approved_workspace_roots
        .iter()
        .any(|root| canonical_real_directory(root).is_ok_and(|root| source.starts_with(root)));
    if !approved && !source.starts_with(&run) {
        return Err(io::Error::other("workspace root is not operator approved"));
    }
    if ["agent.toml", "SOUL.md"]
        .iter()
        .any(|name| source.join(name).exists())
    {
        return Err(io::Error::other("agent directories cannot seed discovery"));
    }
    let total_root = discovery_root(&run)?;
    with_quota_lock(total_root, || {
        let size = tree_bytes(&source)?;
        if size > config.max_starting_workspace_bytes
            || run_associated_bytes(total_root, &run)?.saturating_add(size) > config.max_run_bytes
            || tree_bytes(total_root)?.saturating_add(size) > config.max_total_bytes
        {
            return Err(io::Error::other("discovery workspace quota exceeded"));
        }
        let guard = IntegrityGuard::capture_filtered(&source)?;
        let workspace = copy_private_workspace(&source, node_dir)?;
        if let Err(error) = guard.verify().and_then(|_| {
            if run_associated_bytes(total_root, &run)? > config.max_run_bytes
                || tree_bytes(total_root)? > config.max_total_bytes {
                return Err(io::Error::other("workspace copy changed or exceeded quota"));
            }
            Ok(())
        }) {
            let _ = fs::remove_dir_all(&workspace);
            return Err(error);
        }
        // Source snapshots are intentionally strict. A seed with excluded links is
        // guarded by its regular files, not by following the excluded entries.
        Ok(PreparedWorkspace { workspace, guard })
    })
}
impl IntegrityGuard {
    fn capture_filtered(root: &Path) -> io::Result<Self> {
        // Bind host-held hashes to the source before cloning. Links and special
        // files are fingerprinted without being followed or copied.
        Ok(Self {
            root: root.to_path_buf(),
            hashes: filtered_manifest(root)?,
            filtered: true,
        })
    }
    pub fn verify_filtered(&self) -> io::Result<()> {
        if filtered_manifest(&self.root)? != self.hashes {
            return Err(io::Error::other("immutable source changed"));
        }
        Ok(())
    }
}
fn filtered_manifest(root: &Path) -> io::Result<BTreeMap<PathBuf, String>> {
    fn visit(root: &Path, rel: &Path, out: &mut BTreeMap<PathBuf, String>) -> io::Result<()> {
        for entry in fs::read_dir(root.join(rel))? {
            let entry = entry?;
            if policy().is_excluded(&entry.file_name()) {
                continue;
            }
            let child = rel.join(entry.file_name());
            let meta = fs::symlink_metadata(entry.path())?;
            let digest = if meta.file_type().is_symlink() {
                format!("symlink:{:?}", fs::read_link(entry.path())?)
            } else if meta.is_dir() {
                visit(root, &child, out)?;
                "directory".into()
            } else if meta.is_file() {
                let mut options = fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                let mut file = options.open(entry.path())?;
                let mut hash = Sha256::new();
                let mut buffer = [0; 16384];
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buffer[..n]);
                }
                format!("file:{:x}:{}", hash.finalize(), regular_single_link(&meta))
            } else {
                "special".into()
            };
            out.insert(child, digest);
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, Path::new(""), &mut out)?;
    Ok(out)
}
/// Agent-created interpreter hooks and dynamic libraries invalidate the solution.
pub fn tamper_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
    fn visit(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(root.join(rel))? {
            let entry = entry?;
            let child = rel.join(entry.file_name());
            let meta = fs::symlink_metadata(entry.path())?;
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if meta.file_type().is_symlink()
                || !meta.is_dir() && !regular_single_link(&meta)
                || matches!(
                    name.as_str(),
                    "conftest.py" | "sitecustomize.py" | "usercustomize.py"
                )
                || name.starts_with(".env")
                || ["pth", "so", "dylib", "dll"]
                    .iter()
                    .any(|ext| Path::new(&name).extension().is_some_and(|e| e == *ext))
            {
                out.push(child);
            } else if meta.is_dir() {
                visit(root, &child, out)?;
            }
        }
        Ok(())
    }
    let mut out = vec![];
    visit(root, Path::new(""), &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (
        tempfile::TempDir,
        DiscoveryConfig,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let seed = base.join("seed");
        let run = base.join("discovery/runs/run");
        let node = run.join("r1/b0/a0");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&node).unwrap();
        let config = DiscoveryConfig {
            approved_workspace_roots: vec![seed.clone()],
            ..Default::default()
        };
        (temp, config, seed, run, node.join("ws"))
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn materializes_exact_path_and_drops_identity_and_secrets() {
        let (_temp, config, seed, run, ws) = setup();
        fs::write(seed.join("solution.json"), "{}").unwrap();
        fs::write(seed.join(".envrc"), "secret").unwrap();
        let prepared = prepare_attempt_workspace(&seed, &ws, &run, &config).unwrap();
        assert_eq!(prepared.workspace, ws);
        assert!(!ws.join(".envrc").exists());
        prepared.guard.verify_filtered().unwrap();
        fs::write(seed.join("solution.json"), "changed").unwrap();
        assert!(prepared.guard.verify_filtered().is_err());
    }
    #[test]
    fn refuses_quota_and_unapproved_source() {
        let (_temp, mut config, seed, run, ws) = setup();
        fs::write(seed.join("x"), "123").unwrap();
        config.max_starting_workspace_bytes = 2;
        assert!(prepare_attempt_workspace(&seed, &ws, &run, &config).is_err());
        assert!(!ws.exists());
        config.max_starting_workspace_bytes = 100;
        config.approved_workspace_roots.clear();
        assert!(prepare_attempt_workspace(&seed, &ws, &run, &config).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn drops_all_symlinks_and_hardlinks() {
        let (_temp, config, seed, run, ws) = setup();
        fs::write(seed.join("ordinary"), "data").unwrap();
        fs::hard_link(seed.join("ordinary"), seed.join("hard")).unwrap();
        std::os::unix::fs::symlink("ordinary", seed.join("sym")).unwrap();
        prepare_attempt_workspace(&seed, &ws, &run, &config).unwrap();
        assert!(!ws.join("sym").exists());
        assert!(!ws.join("hard").exists());
        assert!(!ws.join("ordinary").exists());
    }
    #[test]
    fn rejects_tamper_files_without_following_them() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("conftest.py"), "x").unwrap();
        fs::write(tmp.path().join("x.so"), "x").unwrap();
        assert_eq!(tamper_paths(tmp.path()).unwrap().len(), 2);
    }
}

/// Install a fresh trusted hook configuration outside the run directory.
/// The caller passes this file via `--settings` and mounts its directory read-only.
pub fn write_guard_settings(
    private_dir: &Path,
    workspace: &Path,
    run: &Path,
) -> io::Result<PathBuf> {
    write_guard_settings_with_read_paths(private_dir, workspace, run, &[])
}
pub fn write_guard_settings_with_read_paths(
    private_dir: &Path,
    workspace: &Path,
    run: &Path,
    read_workspaces: &[PathBuf],
) -> io::Result<PathBuf> {
    let private = canonical_real_directory(private_dir)?;
    let workspace = canonical_real_directory(workspace)?;
    let run = canonical_real_directory(run)?;
    if private.starts_with(&run) || !workspace.starts_with(&run) {
        return Err(io::Error::other(
            "hook configuration must live outside the run",
        ));
    }
    for path in read_workspaces {
        let path = canonical_real_directory(path)?;
        if !path.starts_with(&run)
            || path.file_name() != Some(std::ffi::OsStr::new("ws"))
            || path == workspace
        {
            return Err(io::Error::other("invalid completed read workspace"));
        }
    }
    let python = [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .ok_or_else(|| io::Error::other("python interpreter unavailable for discovery guard"))?;
    let script = private.join("discovery-file-guard.py");
    let settings = private.join("discovery-settings.json");
    let text = format!(
        "WORKSPACE = {}\nREAD_ROOTS = {}\n{}",
        serde_json::to_string(&workspace.to_string_lossy()).unwrap(),
        serde_json::to_string(
            &std::iter::once(workspace.clone())
                .chain(read_workspaces.iter().cloned())
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        )
        .unwrap(),
        GUARD_SCRIPT
    );
    fs::write(&script, text)?;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let command = format!("{} {}", quote(python), quote(&script.to_string_lossy()));
    let hooks = serde_json::json!({"hooks":{"PreToolUse":[{"matcher":"Read|Write|Edit|MultiEdit|Glob|Grep|NotebookEdit", "hooks":[{"type":"command", "command":command,"timeout":5}]}]}});
    fs::write(&settings, serde_json::to_vec(&hooks)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o400))?;
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o400))?;
    }
    Ok(settings)
}
const GUARD_SCRIPT: &str = r#"
import json
import pathlib
import sys
try:
    envelope = json.load(sys.stdin)
    name = envelope['tool_name']
    data = envelope['tool_input']
    write = name in ('Write', 'Edit', 'MultiEdit', 'NotebookEdit')
    raw = data.get('file_path', data.get('notebook_path', data.get('path')))
    if raw is None and name in ('Glob', 'Grep'):
        raw = WORKSPACE
    if not isinstance(raw, str):
        raise ValueError('missing path')
    path = pathlib.Path(raw)
    if not path.is_absolute():
        path = pathlib.Path(WORKSPACE) / path
    path = path.resolve()
    roots = [pathlib.Path(WORKSPACE)] if write else [pathlib.Path(p) for p in READ_ROOTS]
    allowed = any(path == root.resolve() or root.resolve() in path.parents for root in roots)
    if write:
        forbidden = ('conftest.py', 'sitecustomize.py', 'usercustomize.py')
        allowed = allowed and not any(p.startswith('.env') or p in forbidden or pathlib.Path(p).suffix.lower() in ('.pth', '.so', '.dylib', '.dll') for p in path.parts)
    if not allowed:
        print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse', 'permissionDecision': 'deny', 'permissionDecisionReason': 'Discovery file boundary denied this operation.'}}))
except Exception:
    print('Discovery file guard could not verify the operation.', file=sys.stderr)
    sys.exit(2)
"#;

/// Estimate the full planned copy footprint before dispatch. CoW is still
/// accounted pessimistically so a filesystem fallback cannot breach quota.
pub fn affordable_grid(
    source: &Path,
    run: &Path,
    config: &DiscoveryConfig,
    branches: u32,
    refinements: u32,
) -> io::Result<(u32, u32)> {
    if branches == 0 {
        return Err(io::Error::other("grid must include a branch"));
    }
    let bytes = tree_bytes(source)?.max(1);
    if bytes > config.max_starting_workspace_bytes {
        return Err(io::Error::other("seed exceeds quota"));
    }
    let total = discovery_root(run)?;
    let free = config
        .max_run_bytes
        .saturating_sub(run_associated_bytes(total, run)?)
        .min(config.max_total_bytes.saturating_sub(tree_bytes(total)?));
    let cells = free / bytes;
    if cells == 0 {
        return Err(io::Error::other("no room for a discovery cell"));
    }
    let width = u64::from(branches).min(cells) as u32;
    let refine = u64::from(refinements).min(cells / u64::from(width) - 1) as u32;
    Ok((width, refine))
}

/// Clean explicitly completed runs only. Evaluators and running tasks survive.
/// The orchestrator writes `.completed` after delivery and invokes this at boot.
pub fn cleanup_retained_runs(
    root: &Path,
    config: &DiscoveryConfig,
    now: std::time::SystemTime,
) -> io::Result<Vec<PathBuf>> {
    let root = canonical_real_directory(root)?;
    let quota_root = if root.file_name() == Some(std::ffi::OsStr::new("runs")) {
        discovery_root(&root)?.to_path_buf()
    } else {
        root.clone()
    };
    with_quota_lock(&quota_root, || {
        let mut completed = vec![];
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if !meta.is_dir() || entry.file_name() == "evaluators" {
                continue;
            }
            let marker = entry.path().join(".completed");
            if let Ok(meta) = fs::symlink_metadata(&marker) {
                if regular_single_link(&meta) {
                    completed.push((meta.modified()?, entry.path()));
                }
            }
        }
        completed.sort();
        let mut removed = vec![];
        for (finished, path) in completed {
            let expired = now
                .duration_since(finished)
                .is_ok_and(|age| age.as_secs() >= config.retained_hours.saturating_mul(3600));
            if expired || tree_bytes(&quota_root)? > config.max_total_bytes {
                if root.file_name() == Some(std::ffi::OsStr::new("runs")) {
                    let run_id = path
                        .file_name()
                        .ok_or_else(|| io::Error::other("completed run has no identifier"))?;
                    for kind in ["artifacts", "policy-development", "attempt-snapshots", "retry-seeds", "reports"] {
                        let owner_root = quota_root.join(kind);
                        if !owner_root.exists() {
                            continue;
                        }
                        canonical_real_directory(&owner_root)?;
                        let target = if kind == "reports" {
                            {
                                let mut filename = run_id.to_os_string();
                                filename.push(".json");
                                owner_root.join(filename)
                            }
                        } else {
                            owner_root.join(run_id)
                        };
                        match fs::symlink_metadata(&target) {
                            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&target)?,
                            Ok(_) => fs::remove_file(&target)?,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
                fs::remove_dir_all(&path)?;
                removed.push(path);
            }
        }
        Ok(removed)
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn caps_grid_to_available_non_cow_footprint() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let seed = root.join("seed");
        let run = root.join("discovery/runs/run");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&run).unwrap();
        fs::write(seed.join("data"), [0; 10]).unwrap();
        let config = DiscoveryConfig {
            max_run_bytes: 50,
            max_total_bytes: 50,
            ..Default::default()
        };
        assert_eq!(affordable_grid(&seed, &run, &config, 4, 3).unwrap(), (4, 0));
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn retention_only_removes_completed_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for name in ["evaluators", "running", "complete"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        fs::write(root.join("complete/.completed"), "done").unwrap();
        let config = DiscoveryConfig {
            retained_hours: 0,
            ..Default::default()
        };
        let removed = cleanup_retained_runs(&root, &config, std::time::SystemTime::now()).unwrap();
        assert_eq!(removed, vec![root.join("complete")]);
        assert!(root.join("running").exists());
        assert!(root.join("evaluators").exists());
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn hooks_live_outside_run_and_deny_escaping_file_operations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let private = root.join("guard");
        let run = root.join("run");
        let ws = run.join("a0/ws");
        fs::create_dir(&private).unwrap();
        fs::create_dir_all(&ws).unwrap();
        let settings = write_guard_settings(&private, &ws, &run).unwrap();
        assert!(!settings.starts_with(&run));
        assert!(write_guard_settings(&ws, &ws, &run).is_err());
        let script = private.join("discovery-file-guard.py");
        let mut command = std::process::Command::new("/usr/bin/python3");
        command
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"tool_name":"Write","tool_input":{"file_path":"../../outside"}}"#)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("deny"));
    }
}

#[cfg(all(test, unix))]
mod integrity_race_tests {
    use super::*;
    #[test]
    fn source_guard_detects_new_links_without_reading_the_target() {
        let source = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(source.path().join("data"), "good").unwrap();
        fs::write(outside.path().join("canary"), "secret").unwrap();
        let guard = IntegrityGuard::capture_filtered(source.path()).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("canary"),
            source.path().join("injected"),
        )
        .unwrap();
        assert!(guard.verify().is_err());
    }
    #[test]
    fn registry_manifest_refuses_links_and_hardlinks() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("data"), "good").unwrap();
        fs::hard_link(source.path().join("data"), source.path().join("alias")).unwrap();
        assert!(directory_sha256(source.path()).is_err());
    }
}

/// Export only into the application's artifact tree; never promote a branch
/// over an operator-owned source. Both copy backends discard every linked file.
pub fn export_workspace(
    source: &Path,
    destination: &Path,
    config: &DiscoveryConfig,
) -> io::Result<PathBuf> {
    let source = canonical_real_directory(source)?;
    let root = discovery_root(&source)?.to_path_buf();
    if !source.starts_with(root.join("runs")) {
        return Err(io::Error::other(
            "export source must belong to a discovery run",
        ));
    }
    let parent = canonical_real_directory(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("export destination has no parent"))?,
    )?;
    if !parent.starts_with(root.join("artifacts"))
        || parent == root.join("artifacts")
        || destination.file_name() != Some(std::ffi::OsStr::new("ws"))
        || fs::symlink_metadata(destination).is_ok()
    {
        return Err(io::Error::other(
            "export target is outside the artifact tree or already exists",
        ));
    }
    let destination = parent.join("ws");
    with_quota_lock(&root, || {
        let size = tree_bytes(&source)?;
        if run_associated_bytes(&root, &source)?.saturating_add(size) > config.max_run_bytes
            || tree_bytes(&root)?.saturating_add(size) > config.max_total_bytes
        {
            return Err(io::Error::other("artifact export exceeds discovery quota"));
        }
        let guard = IntegrityGuard::capture_filtered(&source)?;
        if association_id(&root, &source)? != association_id(&root, &destination)? {
            return Err(io::Error::other("artifact destination belongs to another run"));
        }
        let output = copy_private_workspace(&source, &destination)?;
        if guard.verify().is_err() || tree_bytes(&root)? > config.max_total_bytes
            || run_associated_bytes(&root, &source)? > config.max_run_bytes {
            let _ = fs::remove_dir_all(&output);
            return Err(io::Error::other("artifact copy integrity or quota changed"));
        }
        Ok(output)
    })
}

#[cfg(test)]
mod export_tests {
    use super::*;
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn exports_only_into_private_artifact_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("discovery");
        let source = root.join("runs/test/r1/b0/a0/ws");
        let artifact = root.join("artifacts/test");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&artifact).unwrap();
        fs::write(source.join("solution.json"), "{}").unwrap();
        let output =
            export_workspace(&source, &artifact.join("ws"), &DiscoveryConfig::default()).unwrap();
        assert_eq!(
            fs::read_to_string(output.join("solution.json")).unwrap(),
            "{}"
        );
        assert!(
            export_workspace(&source, &source.join("ws"), &DiscoveryConfig::default()).is_err()
        );
        assert!(
            export_workspace(&source, &artifact.join("ws"), &DiscoveryConfig::default()).is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn artifact_drops_inroot_links_and_all_hardlinked_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("discovery");
        let source = root.join("runs/test/ws");
        let artifact = root.join("artifacts/test");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&artifact).unwrap();
        fs::write(source.join("plain"), "data").unwrap();
        fs::write(source.join("hard"), "data").unwrap();
        fs::hard_link(source.join("hard"), source.join("alias")).unwrap();
        std::os::unix::fs::symlink("plain", source.join("symlink")).unwrap();
        let output =
            export_workspace(&source, &artifact.join("ws"), &DiscoveryConfig::default()).unwrap();
        assert!(!output.join("symlink").exists());
        assert!(!output.join("hard").exists());
        assert!(!output.join("alias").exists());
        assert!(output.join("plain").exists());
    }
    #[test]
    fn export_checks_total_quota_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("discovery");
        let source = root.join("runs/test/ws");
        let artifact = root.join("artifacts/test");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&artifact).unwrap();
        fs::write(source.join("data"), [0; 10]).unwrap();
        let config = DiscoveryConfig {
            max_total_bytes: 19,
            ..Default::default()
        };
        assert!(export_workspace(&source, &artifact.join("ws"), &config).is_err());
        assert!(!artifact.join("ws").exists());
    }
}

#[cfg(test)]
mod related_retention_tests {
    use super::*;
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn retention_cleans_completed_run_associations_and_keeps_running_data() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("discovery");
        for run in ["expired", "running"] {
            for kind in ["runs", "artifacts", "policy-development"] {
                fs::create_dir_all(root.join(kind).join(run)).unwrap();
                fs::write(root.join(kind).join(run).join("data"), [0; 32]).unwrap();
            }
            fs::create_dir_all(root.join("reports")).unwrap();
            fs::write(root.join("reports").join(format!("{run}.json")), "{}").unwrap();
        }
        fs::create_dir(root.join("evaluators")).unwrap();
        fs::write(root.join("evaluators/keep"), "trusted").unwrap();
        fs::write(root.join("runs/expired/.completed"), "done").unwrap();
        let config = DiscoveryConfig {
            retained_hours: 0,
            ..Default::default()
        };
        assert_eq!(
            cleanup_retained_runs(&root.join("runs"), &config, std::time::SystemTime::now())
                .unwrap(),
            vec![root.join("runs/expired")]
        );
        for kind in ["runs", "artifacts", "policy-development"] {
            assert!(!root.join(kind).join("expired").exists());
            assert!(root.join(kind).join("running").exists());
        }
        assert!(!root.join("reports/expired.json").exists());
        assert!(root.join("reports/running.json").exists());
        assert!(root.join("evaluators/keep").exists());
    }
    #[cfg(unix)]
    #[test]
    fn retention_refuses_symlinked_association_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("discovery");
        let outside = tmp.path().canonicalize().unwrap().join("outside");
        fs::create_dir_all(root.join("runs/expired")).unwrap();
        fs::write(root.join("runs/expired/.completed"), "done").unwrap();
        fs::create_dir_all(outside.join("expired")).unwrap();
        fs::write(outside.join("expired/canary"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("artifacts")).unwrap();
        let config = DiscoveryConfig {
            retained_hours: 0,
            ..Default::default()
        };
        assert!(
            cleanup_retained_runs(&root.join("runs"), &config, std::time::SystemTime::now())
                .is_err()
        );
        assert!(outside.join("expired/canary").exists());
        assert!(root.join("runs/expired").exists());
    }
}

#[cfg(test)]
mod visibility_guard_tests {
    use super::*;
    fn guard_output(script: &Path, path: &Path) -> serde_json::Value {
        use std::io::Write;
        let mut child = std::process::Command::new("/usr/bin/python3")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let input =
            serde_json::json!({"tool_name":"Read","tool_input":{"file_path":path}}).to_string();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        if output.stdout.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&output.stdout).unwrap()
        }
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn reads_own_and_explicit_completed_workspace_but_denies_ledgers_and_live_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let private = root.join("guard");
        let run = root.join("run");
        let own = run.join("r1/b0/a1/ws");
        let completed = run.join("r1/b1/a0/ws");
        let live = run.join("r1/b2/a0/ws");
        fs::create_dir(&private).unwrap();
        for path in [&own, &completed, &live] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("solution"), "data").unwrap();
        }
        fs::write(run.join("world.json"), "hidden").unwrap();
        write_guard_settings_with_read_paths(
            &private,
            &own,
            &run,
            std::slice::from_ref(&completed),
        )
        .unwrap();
        let script = private.join("discovery-file-guard.py");
        assert!(guard_output(&script, &own.join("solution")).is_null());
        assert!(guard_output(&script, &completed.join("solution")).is_null());
        for forbidden in [
            run.join("world.json"),
            run.join("tree.jsonl"),
            live.join("solution"),
        ] {
            assert_eq!(
                guard_output(&script, &forbidden)["hookSpecificOutput"]["permissionDecision"],
                "deny"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod associated_quota_tests {
    use super::*;
    #[test]
    fn workspace_and_artifact_writers_count_retained_retry_seeds_in_the_run_quota() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap().join("discovery");
        let run = root.join("runs/quota-run");
        let branch = run.join("r1/b0/a0");
        create_private_directory(&branch).unwrap();
        let seed = home.path().join("seed");
        fs::create_dir(&seed).unwrap();
        fs::write(seed.join("payload"), "x").unwrap();
        let retained = root.join("retry-seeds/quota-run/call-active");
        create_private_directory(&retained).unwrap();
        fs::write(retained.join("liability"), "12345").unwrap();
        let cfg = DiscoveryConfig { approved_workspace_roots: vec![seed.clone()], max_run_bytes: 5,
            max_total_bytes: 1024, ..Default::default() };
        assert!(prepare_attempt_workspace(&seed, &branch.join("ws"), &run, &cfg).is_err(),
            "the ordinary workspace writer must count retained retry seeds");
        create_private_directory(&branch.join("ws")).unwrap();
        fs::write(branch.join("ws/payload"), "x").unwrap();
        let artifact = root.join("artifacts/quota-run/best");
        create_private_directory(&artifact).unwrap();
        assert!(export_workspace(&branch.join("ws"), &artifact.join("ws"), &cfg).is_err(),
            "artifact publication must enforce the same per-run associations");
    }
}
