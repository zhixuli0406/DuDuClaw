//! Per-spawn upstream-env files for third-party MCP proxies.
//!
//! Everything lives under `<home>/tmp/mcp-proxy-env` (0700 on Unix):
//!
//! - Codex: one `duduclaw-mcp-env-*.json` file per proxy, named in the `-c`
//!   overrides through `DUDUCLAW_MCP_PROXY_ENV_FILE` ([`write_env_file`]);
//! - Grok and Antigravity: one `duduclaw-mcp-env-*` directory per spawn with a
//!   `<server>.json` per proxy, named through `DUDUCLAW_MCP_PROXY_ENV_DIR`
//!   ([`write_spawn_env_dir`]). A directory per spawn means two spawns of the
//!   same employee never share or overwrite each other's files.
//!
//! Both are deleted when the spawn's guard drops; [`sweep_stale_env_files`]
//! removes what a stopped gateway left behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::ScopedServer;

/// Directory under the DuDuClaw home holding the per-spawn upstream-env files.
pub(crate) const ENV_FILE_DIR: &str = "mcp-proxy-env";
/// Name prefix of a per-spawn upstream-env file or directory.
pub(crate) const ENV_FILE_PREFIX: &str = "duduclaw-mcp-env-";
/// An entry older than this at gateway start belongs to no running spawn (a
/// Codex run is cut off after two minutes, Grok after five plus a 90-second
/// retry, Antigravity after 330 seconds).
pub(crate) const ENV_FILE_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// `<home>/tmp/mcp-proxy-env`, created owner-only (0700 on Unix). A symbolic
/// link or a non-directory there is refused rather than followed.
pub(crate) fn env_file_dir(home_dir: &Path) -> Result<PathBuf, String> {
    let dir = home_dir.join("tmp").join(ENV_FILE_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Err(format!("{} is not a plain directory", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return env_file_dir(home_dir);
                }
                Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
            }
        }
        Err(e) => return Err(format!("cannot inspect {}: {e}", dir.display())),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    }
    Ok(dir)
}

/// Remove upstream-env files and directories left behind by a gateway that
/// stopped while a spawn was running (a dropped guard normally deletes them).
/// Called once at gateway start; only entries with this module's prefix and
/// older than [`ENV_FILE_STALE_AFTER`] are removed. Returns how many.
pub fn sweep_stale_env_files(home_dir: &Path) -> usize {
    sweep_env_files_older_than(home_dir, ENV_FILE_STALE_AFTER)
}

pub(crate) fn sweep_env_files_older_than(home_dir: &Path, max_age: std::time::Duration) -> usize {
    let dir = home_dir.join("tmp").join(ENV_FILE_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(ENV_FILE_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= max_age);
        if !old {
            continue;
        }
        // `symlink_metadata`: a real directory is removed with its contents,
        // anything else (a file, a symbolic link) as a single entry — a link
        // is never followed.
        let gone = if meta.file_type().is_dir() {
            std::fs::remove_dir_all(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
        if gone.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Write one proxy's upstream env JSON to an owner-only temp file in `dir`
/// (`tempfile` creates it 0600 before anything is written).
pub(crate) fn write_env_file(dir: &Path, body: &str) -> Result<tempfile::TempPath, String> {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix(ENV_FILE_PREFIX)
        .suffix(".json")
        .tempfile_in(dir)
        .map_err(|e| format!("temp file creation failed: {e}"))?;
    file.write_all(body.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|e| format!("temp file write failed: {e}"))?;
    let path = file.into_temp_path();
    duduclaw_core::platform::set_owner_only(&path).ok();
    Ok(path)
}

/// One spawn's upstream-env directory; deleted (with its files) on drop, so
/// the caller holds it until the CLI has exited.
#[derive(Debug)]
pub(crate) struct SpawnEnvDir {
    dir: tempfile::TempDir,
}

impl SpawnEnvDir {
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `(DUDUCLAW_MCP_PROXY_ENV_DIR, <path>)` for the CLI process.
    pub(crate) fn env_pair(&self) -> (String, String) {
        (
            duduclaw_core::mcp_proxy_rewrite::PROXY_UPSTREAM_ENV_DIR_VAR.to_string(),
            self.path().to_string_lossy().into_owned(),
        )
    }
}

/// Write `servers`' upstream env into a fresh per-spawn directory under
/// [`env_file_dir`]: a new randomly named directory (0700), one
/// `<server>.json` per server created exclusively (0600 before the first
/// byte, never following a link). `Ok(None)` when there is nothing to write.
/// `Err` when anything cannot be written; the caller refuses the spawn rather
/// than start a server without its credentials.
pub(crate) fn write_spawn_env_dir(
    home_dir: &Path,
    servers: &BTreeMap<String, ScopedServer>,
) -> Result<Option<SpawnEnvDir>, String> {
    if servers.is_empty() {
        return Ok(None);
    }
    // `Foo.json` and `foo.json` are one file on a case-insensitive file
    // system (macOS, Windows): refuse rather than hand one server the other's
    // credentials.
    if let Some((a, b)) = case_fold_collision(servers.keys()) {
        return Err(format!(
            "MCP server names {a:?} and {b:?} differ only in letter case"
        ));
    }
    let parent = env_file_dir(home_dir)?;
    let dir = tempfile::Builder::new()
        .prefix(ENV_FILE_PREFIX)
        .tempdir_in(&parent)
        .map_err(|e| format!("temp directory creation failed: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict the temp directory: {e}"))?;
    }
    for (name, server) in servers {
        let file = duduclaw_core::mcp_proxy_rewrite::spawn_env_file_name(name)
            .ok_or_else(|| format!("server name {name:?} is not a plain name"))?;
        write_new_owner_only(&dir.path().join(file), server.upstream_env.as_bytes())?;
    }
    Ok(Some(SpawnEnvDir { dir }))
}

/// The first two names that are equal ignoring ASCII case, if any.
pub(crate) fn case_fold_collision<'a>(
    names: impl Iterator<Item = &'a String>,
) -> Option<(String, String)> {
    let mut seen: BTreeMap<String, &String> = BTreeMap::new();
    for name in names {
        if let Some(prev) = seen.insert(name.to_ascii_lowercase(), name) {
            return Some((prev.clone(), name.clone()));
        }
    }
    None
}

fn write_new_owner_only(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("env file creation failed: {e}"))?;
    duduclaw_core::platform::set_owner_only(path)
        .map_err(|e| format!("env file restriction failed: {e}"))?;
    f.write_all(bytes)
        .and_then(|_| f.flush())
        .map_err(|e| format!("env file write failed: {e}"))
}
