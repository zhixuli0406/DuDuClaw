use super::*;

// Only the new copy is disposable while the original config remains intact.
// An unsuccessful migration never prunes previously retained backups.
struct PendingBackup {
    path: PathBuf,
    published: bool,
}

impl Drop for PendingBackup {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Migrate one employee config while the caller holds the shared MCP config
/// lock. Key, backup, and store durability precede config publication; a failed
/// publication restores the original bytes before returning the error.
/// Existing references never trigger a backup or a rewrite.
pub fn migrate_agent(home: &Path, agent: &str, path: &Path) -> io::Result<bool> {
    let owned_path = path == home.join("agents").join(agent).join(".mcp.json")
        || path == home.join("agents/.ephemeral").join(agent).join(".mcp.json");
    let original = match regular_bytes(path) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut config: Value = serde_json::from_slice(&original)
        .map_err(|_| invalid("invalid MCP configuration"))?;
    let servers = config.get_mut("mcpServers").and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("MCP configuration has no server map"))?;
    let mut needs_migration = false;
    for (server, entry) in servers.iter() {
        if duduclaw_core::mcp_proxy_rewrite::is_host_owned_entry(
            server, entry, &duduclaw_core::resolve_duduclaw_bin(),
        ) { continue }
        if let Some(env) = entry.get("env") {
            let env = env.as_object().ok_or_else(|| invalid("MCP env must be an object"))?;
            for (name, value) in env {
                validate_names(server, name)?;
                let value = value.as_str().ok_or_else(|| invalid("MCP env value must be a string"))?;
                needs_migration |= !keeps_plaintext_env(name, value);
            }
        }
    }
    if !needs_migration {
        // Retry retention cleanup after a previous successful publication
        // whose cleanup failed. No config/store writes are needed.
        if owned_path && duduclaw_core::is_valid_agent_id(agent) {
            let backups = home.join("backups/mcp-migrate");
            if backups.is_dir() && prune_backups(&backups, agent).is_err() {
                tracing::warn!(agent, "MCP migration backup retention cleanup still pending");
            }
        }
        return Ok(false);
    }
    validate_agent(agent)?;
    if !owned_path {
        return Err(invalid("MCP migration requires an employee configuration path"));
    }

    // Durability order: key -> backup -> store -> config. Each write must
    // finish its parent-directory barrier before advancing to the next step.
    prepare_store(home)?;
    // Refuse a wrong/missing key before creating a backup with it.
    read_entries(home, &store_path(home, agent)?)?;
    private_dir(&home.join("backups"))?;
    let backups = home.join("backups/mcp-migrate");
    private_dir(&backups)?;
    let engine = crypto(home, !store_path(home, agent)?.exists())?;
    let encrypted = engine.encrypt(&original).map_err(|_| invalid("cannot encrypt MCP migration backup"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| invalid("MCP backup clock unavailable"))?.as_nanos();
    let backup = backups.join(format!("{agent}-{stamp:039}.enc"));
    atomic_private_write(&backup, &encrypted)?;
    let mut pending = PendingBackup { path: backup.clone(), published: false };
    let verified = Zeroizing::new(engine.decrypt(&regular_bytes(&backup)?)
        .map_err(|_| invalid("cannot verify MCP migration backup"))?);
    if *verified != *original { return Err(invalid("MCP migration backup readback failed")) }

    for (server, entry) in servers.iter_mut() {
        if duduclaw_core::mcp_proxy_rewrite::is_host_owned_entry(
            server, entry, &duduclaw_core::resolve_duduclaw_bin(),
        ) { continue }
        if let Some(env) = entry.get_mut("env") {
            let source = env.as_object().ok_or_else(|| invalid("MCP env must be an object"))?;
            let protected = store_mcp_env(home, agent, server, source)?;
            // Verify newly replaced values, without requiring unrelated legacy
            // references (possibly intentionally unset) to be resolvable.
            let newly_sealed: Map<String, Value> = protected.iter()
                .filter(|(name, value)| source.get(*name) != Some(*value))
                .map(|(name, value)| (name.clone(), value.clone())).collect();
            let resolved = resolve_mcp_env(home, agent, server, &newly_sealed)?;
            if resolved.iter().any(|(name, value)| source.get(name) != Some(value)) {
                return Err(invalid("MCP migration secret readback failed"));
            }
            *env = Value::Object(protected);
        }
    }
    let bytes = serde_json::to_vec_pretty(&config).map_err(io::Error::other)?;
    // A parent sync failure happens after rename. Restore the original bytes
    // on failure so plaintext remains at the original config path.
    // Retain the encrypted recovery copy once config publication can begin.
    pending.published = true;
    publish_config(path, &bytes, &original)?;
    // Publication succeeded. Retention failure is reported separately, never
    // as a claim that the original config was left untouched.
    if prune_backups(&backups, agent).is_err() {
        tracing::warn!(agent, "MCP migration succeeded; backup retention cleanup failed");
    }
    Ok(true)
}

fn publish_config(path: &Path, bytes: &[u8], original: &[u8]) -> io::Result<()> {
    if let Err(error) = atomic_private_write(path, bytes) {
        // Do not create a long-lived plaintext recovery file beside a
        // successfully migrated config. The encrypted backup is already durable.
        if !regular_bytes(path).is_ok_and(|current| current == original)
            && atomic_private_write(path, original).is_err()
        {
            return Err(invalid("MCP config publication or rollback durability failed; encrypted backup retained"));
        }
        return Err(error);
    }
    Ok(())
}

fn prune_backups(dir: &Path, agent: &str) -> io::Result<()> {
    let prefix = format!("{agent}-");
    let mut backups = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Match the timestamp, not another employee with an overlapping id.
        let Some(stamp) = name.strip_prefix(&prefix).and_then(|s| s.strip_suffix(".enc")) else { continue };
        if stamp.len() == 39 && stamp.bytes().all(|b| b.is_ascii_digit()) && entry.file_type()?.is_file() {
            backups.push(entry.path());
        }
    }
    backups.sort();
    let remove = backups.len().saturating_sub(3);
    for backup in backups.into_iter().take(remove) { fs::remove_file(backup)?; }
    // Sync the backup directory once, including on retry after a failed sync.
    sync_parent(&dir.join(".retention-sync"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_pruning_reports_directory_sync_failure_and_retries_barrier() {
        let dir = std::env::temp_dir().join(format!("mcp-retention-sync-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for stamp in 0..5 {
            fs::write(dir.join(format!("alice-{stamp:039}.enc")), b"encrypted backup").unwrap();
        }
        // Use the same thread-local, one-shot seam as the store durability tests.
        FAIL_PARENT_SYNC.with(|failure| *failure.borrow_mut() = Some((dir.clone(), 0)));
        let error = prune_backups(&dir, "alice").unwrap_err();
        assert!(error.to_string().contains("injected MCP parent sync failure"));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 3);
        assert!(!dir.join(format!("alice-{:039}.enc", 0)).exists());
        assert!(!dir.join(format!("alice-{:039}.enc", 1)).exists());
        // Even with no more files to prune, retry the failed directory barrier.
        FAIL_PARENT_SYNC.with(|failure| *failure.borrow_mut() = Some((dir.clone(), 0)));
        assert!(prune_backups(&dir, "alice").is_err());
        prune_backups(&dir, "alice").unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
