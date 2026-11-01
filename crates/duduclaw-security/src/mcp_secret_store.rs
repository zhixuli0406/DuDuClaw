//! Encrypted third-party MCP env, scoped to one employee.
//!
//! This removes credentials from model-readable config files. It does not
//! isolate processes running as the same OS user (in particular Bash).

use crate::crypto::CryptoEngine;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

type Entries = BTreeMap<String, String>;
const PUBLIC_ENV: &[&str] = &["PATH", "HOME", "NODE_ENV", "LOG_LEVEL"];

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
thread_local! {
    // Thread-local and one-shot: concurrent tests cannot affect one another,
    // and rollback can still perform its own durability barrier.
    static FAIL_PARENT_SYNC: std::cell::RefCell<Option<(PathBuf, usize)>> = const {
        std::cell::RefCell::new(None)
    };
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| invalid("missing MCP file parent"))?;
    #[cfg(test)]
    FAIL_PARENT_SYNC.with(|failure| -> io::Result<()> {
        let mut failure = failure.borrow_mut();
        if let Some((target, remaining)) = failure.as_mut() {
            if target.as_path() == path || target.as_path() == parent {
                if *remaining == 0 {
                    *failure = None;
                    return Err(io::Error::other("injected MCP parent sync failure"));
                }
                *remaining -= 1;
            }
        }
        Ok(())
    })?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // CreateFileW needs BACKUP_SEMANTICS for directories; flushing via
        // FlushFileBuffers also requires a writable handle.
        fs::OpenOptions::new().read(true).write(true).custom_flags(0x0200_0000)
            .open(parent)?.sync_all()?;
    }
    Ok(())
}

fn sync_existing_file(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    let file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    #[cfg(not(windows))]
    let file = fs::File::open(path)?;
    // The Windows handle is writable for FlushFileBuffers, without truncate
    // or create: an existing machine key must never be replaced here.
    file.sync_all()
}

fn validate_agent(agent: &str) -> io::Result<()> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err(invalid("invalid MCP secret owner"));
    }
    Ok(())
}

fn validate_names(server: &str, env: &str) -> io::Result<()> {
    // Same alphabet and length as mcp_scan::is_valid_mcp_server_name.
    if !duduclaw_core::is_valid_agent_id(&server.replace('.', "_")) {
        return Err(invalid("invalid MCP server name"));
    }
    let mut bytes = env.bytes();
    if !bytes.next().is_some_and(|c| c.is_ascii_uppercase() || c == b'_')
        || !bytes.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err(invalid("invalid MCP environment name"));
    }
    Ok(())
}

fn private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(invalid("MCP secret directory is not a real directory"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return private_dir(path),
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    duduclaw_core::platform::set_owner_only(path)?;
    // Repeat on existing directories too: a previous failed barrier may have
    // left a visible but not yet durable directory entry.
    sync_parent(path)
}

fn store_path(home: &Path, agent: &str) -> io::Result<PathBuf> {
    validate_agent(agent)?;
    Ok(home.join("secrets/mcp").join(format!("{agent}.enc")))
}

fn prepare_store(home: &Path) -> io::Result<()> {
    fs::create_dir_all(home)?;
    private_dir(&home.join("secrets"))?;
    private_dir(&home.join("secrets/mcp"))
}

fn regular_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(invalid("MCP secret file is not a regular file"));
    }
    fs::read(path)
}

fn crypto(home: &Path, create: bool) -> io::Result<CryptoEngine> {
    let path = home.join(".keyfile");
    let load = || -> io::Result<CryptoEngine> {
        let key = Zeroizing::new(regular_bytes(&path)?);
        let key: &[u8; 32] = key.as_slice().try_into()
            .map_err(|_| invalid("machine encryption key must contain 32 bytes"))?;
        CryptoEngine::new(key).map_err(|_| invalid("machine encryption key unavailable"))
    };
    if !create {
        return load();
    }
    duduclaw_core::with_file_lock(&path, || {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                // Retry the barrier even for a key left by a failed creation.
                // No ciphertext may depend on a merely visible directory entry.
                let engine = load()?;
                sync_existing_file(&path)?;
                sync_parent(&path)?;
                return Ok(engine);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Losing the machine key must never silently rotate existing stores.
        for dir in [home.join("secrets/mcp"), home.join("backups/mcp-migrate")] {
            match fs::read_dir(dir) {
                Ok(files) => {
                    for file in files {
                        if file?.path().extension().is_some_and(|ext| ext == "enc") {
                            return Err(invalid("machine encryption key missing for existing MCP secrets"));
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let key = Zeroizing::new(CryptoEngine::generate_key()
            .map_err(|_| invalid("machine encryption key generation failed"))?);
        // Never replace an existing (possibly corrupt) machine key.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        duduclaw_core::platform::set_owner_only(&path)?;
        file.write_all(key.as_ref())?;
        file.sync_all()?;
        sync_parent(&path)?;
        load()
    })
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| invalid("missing MCP file parent"))?;
    let nonce = CryptoEngine::generate_key().map_err(|_| invalid("MCP temporary name unavailable"))?;
    let suffix: String = nonce[..12].iter().map(|b| format!("{b:02x}")).collect();
    let temporary = parent.join(format!(".mcp-secret-{suffix}.tmp"));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        duduclaw_core::platform::set_owner_only(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_parent(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_entries(home: &Path, path: &Path) -> io::Result<Entries> {
    let bytes = match regular_bytes(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Entries::new()),
        Err(e) => return Err(e),
    };
    let encrypted = std::str::from_utf8(&bytes).map_err(|_| invalid("invalid MCP secret ciphertext"))?;
    let plain = Zeroizing::new(crypto(home, false)?.decrypt_string(encrypted)
        .map_err(|_| invalid("cannot decrypt MCP secrets"))?);
    serde_json::from_str(&plain).map_err(|_| invalid("invalid MCP secret store"))
}

fn write_entries(home: &Path, path: &Path, entries: &Entries) -> io::Result<()> {
    let plain = Zeroizing::new(serde_json::to_string(entries).map_err(io::Error::other)?);
    // A missing key must not replace the key of an existing encrypted store.
    let engine = crypto(home, !path.exists())?;
    let encrypted = engine.encrypt_string(&plain).map_err(|_| invalid("cannot encrypt MCP secrets"))?;
    atomic_private_write(path, encrypted.as_bytes())?;
    if read_entries(home, path)? != *entries {
        return Err(invalid("MCP secret readback failed"));
    }
    Ok(())
}

/// The only transformation for env written into employee MCP configuration.
/// All third-party values except PUBLIC_ENV and existing references are sealed.
/// Callers may skip a fully verified built-in definition; an env-only API
/// cannot infer trust from a server name and therefore never exempts one.
pub fn store_mcp_env(home: &Path, agent: &str, server: &str, env: &Map<String, Value>) -> io::Result<Map<String, Value>> {
    validate_agent(agent)?;
    let mut output = env.clone();
    let mut additions = Entries::new();
    for (name, value) in env {
        validate_names(server, name)?;
        let value = value.as_str().ok_or_else(|| invalid("MCP env value must be a string"))?;
        if PUBLIC_ENV.contains(&name.as_str()) || value.starts_with("secret://") {
            continue;
        }
        additions.insert(format!("{server}/{name}"), value.to_owned());
        output.insert(name.clone(), Value::String(format!("secret://mcp/{server}/{name}")));
    }
    if additions.is_empty() {
        return Ok(output);
    }
    prepare_store(home)?;
    let path = store_path(home, agent)?;
    duduclaw_core::with_file_lock(&path, || {
        let mut entries = read_entries(home, &path)?;
        entries.extend(additions);
        write_entries(home, &path, &entries)
    })?;
    Ok(output)
}

/// Resolve only exact references to this server/env in this employee's store.
/// No external backend or caller-selected employee path is accepted. Errors
/// identify the variable but never include a reference or credential value.
pub fn resolve_mcp_env(home: &Path, agent: &str, server: &str, env: &Map<String, Value>) -> io::Result<Map<String, Value>> {
    let mut output = env.clone();
    let mut entries = None;
    for (name, value) in env {
        let Some(value) = value.as_str().filter(|v| v.starts_with("secret://")) else { continue };
        let failure = || invalid(&format!("MCP server {server}: cannot resolve environment variable {name}"));
        validate_names(server, name).map_err(|_| failure())?;
        if value != format!("secret://mcp/{server}/{name}") {
            return Err(failure());
        }
        if entries.is_none() {
            let path = store_path(home, agent).map_err(|_| failure())?;
            // Atomic replacement makes lock-free reads see a complete snapshot.
            entries = Some(read_entries(home, &path).map_err(|_| failure())?);
        }
        let resolved = entries.as_ref().and_then(|map| map.get(&format!("{server}/{name}")))
            .filter(|value| !value.starts_with("secret://")).ok_or_else(failure)?;
        output.insert(name.clone(), Value::String(resolved.clone()));
    }
    Ok(output)
}

/// Remove a server's live secrets; encrypted migration backups are retained.
pub fn remove_server(home: &Path, agent: &str, server: &str) -> io::Result<()> {
    validate_names(server, "ENV")?;
    let path = store_path(home, agent)?;
    if !path.exists() { return Ok(()) }
    duduclaw_core::with_file_lock(&path, || {
        let mut entries = read_entries(home, &path)?;
        entries.retain(|key, _| !key.starts_with(&format!("{server}/")));
        write_entries(home, &path, &entries)
    })
}

/// Remove the employee's live store without requiring a working decryption key.
pub fn remove_agent(home: &Path, agent: &str) -> io::Result<()> {
    let path = store_path(home, agent)?;
    if !path.exists() { return Ok(()) }
    duduclaw_core::with_file_lock(&path, || match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    })
}

mod migration;
pub use migration::migrate_agent;

#[cfg(test)]
mod tests;
