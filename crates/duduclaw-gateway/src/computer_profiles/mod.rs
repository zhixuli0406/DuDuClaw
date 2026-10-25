//! Encrypted, bounded browser login snapshots. Archive bytes are opaque here;
//! only the unprivileged in-container helper parses them.

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::fs_safe::SafeDir;

pub const ROOT: &str = "computer_profiles";
pub const HARD_MAX_BYTES: u64 = 128 * 1024 * 1024;
const HEADER_MAX: u64 = 16 * 1024;
const IDENTITY: &str = "computer_profile.json";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilesConfig {
    pub enabled: bool,
    pub max_bytes: u64,
    pub retention_days: u32,
}

impl Default for ProfilesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bytes: 64 * 1024 * 1024,
            retention_days: 30,
        }
    }
}

impl ProfilesConfig {
    pub fn load(home: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("profile configuration unreadable".into()),
        };
        let table = text
            .parse::<toml::Table>()
            .map_err(|_| "profile configuration invalid")?;
        Self::parse(&table)
    }

    pub(crate) fn parse(table: &toml::Table) -> Result<Self, String> {
        let Some(cu) = table.get("computer_use") else {
            return Ok(Self::default());
        };
        let cu = cu.as_table().ok_or("computer_use must be a table")?;
        let Some(value) = cu.get("profiles") else {
            return Ok(Self::default());
        };
        let config: Self = value
            .clone()
            .try_into()
            .map_err(|_| "profile configuration invalid")?;
        if !(1024 * 1024..=HARD_MAX_BYTES).contains(&config.max_bytes)
            || config.retention_days > 3650
        {
            return Err("profile limits invalid".into());
        }
        Ok(config)
    }

    pub fn active(home: &Path) -> Option<Self> {
        cfg!(unix)
            .then(|| Self::load(home).ok())
            .flatten()
            .filter(|c| c.enabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    profile_id: String,
    /// Including binding timestamps also detects a transfer away and back.
    owners: Option<Vec<(String, String)>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileState {
    pub version: u32,
    pub profile_id: String,
    pub agent_id: String,
    pub hosts: Vec<String>,
    pub generation: u64,
    pub sealed_bytes: u64,
    pub updated_at: i64,
    pub last_used_at: i64,
}

/// Context captured at start and checked again before saving. A handover,
/// employee replacement or disabled capability cannot preserve old cookies.
pub(crate) struct ProfileSession {
    _lock: std::fs::File,
    home: PathBuf,
    agent: String,
    identity: Identity,
    hosts: Vec<String>,
    config: ProfilesConfig,
}

fn failure(_: impl std::fmt::Display) -> String {
    "browser login state unavailable".into()
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(unix)]
fn lock_profile(home: &Path, agent: &str) -> Result<std::fs::File, String> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err(failure("agent"));
    }
    let directory = std::fs::canonicalize(home)
        .map_err(failure)?
        .join(ROOT)
        .join(".locks");
    crate::discovery::workspace::create_private_directory(&directory).map_err(failure)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(directory.join(format!("{agent}.lock")))
        .map_err(failure)?;
    let meta = file.metadata().map_err(failure)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o777 != 0o600
    {
        return Err(failure("unsafe profile lock"));
    }
    // SAFETY: file owns a valid descriptor. A nonblocking lock avoids
    // holding up gateway threads while another process owns this profile.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(failure("profile busy"));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn lock_profile(_home: &Path, _agent: &str) -> Result<std::fs::File, String> {
    Err("browser login persistence is unavailable on this platform".into())
}

fn owners(home: &Path, agent: &str) -> Result<Option<Vec<(String, String)>>, String> {
    let path = home.join("users.db");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        _ => return Err(failure("users db")),
    }
    let mut conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(failure)?;
    // Version and Owner rows must share one database snapshot. Final row
    // equality alone cannot detect overlapping A→B→A ownership writes.
    let tx = conn.transaction().map_err(failure)?;
    let generation = duduclaw_auth::google_identity::read_primary_owner_generation(&tx, agent)
        .map_err(failure)?;
    // B2b's primary-owner column may be installed later. Both layouts use
    // the same ownership check, including suspension/offboarding status.
    let primary = tx
        .prepare("PRAGMA table_info(user_agent_bindings)")
        .map_err(failure)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?
        .iter()
        .any(|name| name == "is_primary");
    let sql = if primary {
        "SELECT b.user_id, b.bound_at, u.status, b.is_primary, u.updated_at FROM user_agent_bindings b JOIN users u ON u.id=b.user_id WHERE b.agent_name=?1 AND b.access_level='owner' ORDER BY b.user_id"
    } else {
        "SELECT b.user_id, b.bound_at, u.status, 0, u.updated_at FROM user_agent_bindings b JOIN users u ON u.id=b.user_id WHERE b.agent_name=?1 AND b.access_level='owner' ORDER BY b.user_id"
    };
    let mut query = tx.prepare(sql).map_err(failure)?;
    let rows = query
        .query_map([agent], |r| {
            Ok((
                r.get::<_, String>(0)?,
                serde_json::json!([
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    generation
                ])
                .to_string(),
            ))
        })
        .map_err(failure)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map(Some)
        .map_err(failure)
}

fn root(home: &Path) -> Result<SafeDir, String> {
    let home = std::fs::canonicalize(home).map_err(failure)?;
    SafeDir::open_root(&home).map_err(failure)
}

fn profile_dir(home: &Path, agent: &str, create: bool) -> Result<Option<SafeDir>, String> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err(failure("agent"));
    }
    let Some(profiles) = root(home)?.child_dir(ROOT, create).map_err(failure)? else {
        return Ok(None);
    };
    profiles.child_dir(agent, create).map_err(failure)
}

fn agent_state(home: &Path, agent: &str) -> Result<SafeDir, String> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err(failure("agent"));
    }
    root(home)?
        .child_dir("agents", false)
        .map_err(failure)?
        .ok_or_else(|| failure("agents"))?
        .child_dir(agent, false)
        .map_err(failure)?
        .ok_or_else(|| failure("agent"))?
        .child_dir("state", true)
        .map_err(failure)?
        .ok_or_else(|| failure("state"))
}

fn clear(home: &Path, agent: &str) -> Result<(), String> {
    if let Some(dir) = profile_dir(home, agent, false)? {
        for entry in dir.list().map_err(failure)? {
            if entry.name == "state.json"
                || entry.name == "profile.sealed"
                || blob_generation(&entry.name).is_some()
            {
                dir.remove_file(&entry.name).map_err(failure)?;
            }
        }
    }
    Ok(())
}

fn blob_name(generation: u64) -> String {
    format!("profile-{generation}.sealed")
}
fn blob_generation(name: &str) -> Option<u64> {
    name.strip_prefix("profile-")?
        .strip_suffix(".sealed")?
        .parse()
        .ok()
}

pub(crate) fn reset(home: &Path, agent: &str) -> Result<(), String> {
    let _lock = lock_profile(home, agent)?;
    clear(home, agent)
}

pub fn state(home: &Path, agent: &str) -> Result<Option<ProfileState>, String> {
    let Some(dir) = profile_dir(home, agent, false)? else {
        return Ok(None);
    };
    dir.read_bytes_checked("state.json", HEADER_MAX, true)
        .map_err(failure)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(failure))
        .transpose()
}

/// Non-secret saved-state metadata for the local operator; no decryption.
pub fn list(home: &Path) -> Result<Vec<ProfileState>, String> {
    let Some(dir) = root(home)?.child_dir(ROOT, false).map_err(failure)? else {
        return Ok(Vec::new());
    };
    let mut entries = dir.list().map_err(failure)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|e| {
            e.kind == crate::fs_safe::EntryKind::Dir && duduclaw_core::is_valid_agent_id(&e.name)
        })
        .take(1000)
    {
        if let Some(record) = state(home, &entry.name)? {
            if record.agent_id != entry.name {
                return Err(failure("profile owner mismatch"));
            }
            out.push(record);
        }
    }
    Ok(out)
}

/// Expire only saved ciphertext, even when persistence is disabled. Active
/// profiles hold a process-wide file lock and are skipped. Invalid config
/// removes nothing. No directory or database is created on an unused home.
pub(crate) fn sweep(home: &Path) -> Result<usize, String> {
    let config = ProfilesConfig::load(home)?;
    if config.retention_days == 0 || !cfg!(unix) {
        return Ok(0);
    }
    let mut removed = 0;
    for record in list(home)? {
        if now().saturating_sub(record.last_used_at) <= i64::from(config.retention_days) * 86400 {
            continue;
        }
        let Ok(_lock) = lock_profile(home, &record.agent_id) else {
            continue;
        };
        if state(home, &record.agent_id)?.is_some_and(|s| {
            s.generation == record.generation
                && now().saturating_sub(s.last_used_at) > i64::from(config.retention_days) * 86400
        }) {
            clear(home, &record.agent_id)?;
            removed += 1;
        }
    }
    Ok(removed)
}

impl ProfileSession {
    pub(crate) fn prepare(
        home: &Path,
        agent: &str,
        hosts: &[String],
        config: ProfilesConfig,
    ) -> Result<Self, String> {
        let home = std::fs::canonicalize(home).map_err(failure)?;
        let lock = lock_profile(&home, agent)?;
        let state = agent_state(&home, agent)?;
        let owners = owners(&home, agent)?;
        let old: Option<Identity> = state
            .read_bytes_checked(IDENTITY, HEADER_MAX, true)
            .map_err(failure)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(failure))
            .transpose()?;
        let identity = match old {
            Some(id) if id.owners == owners && uuid::Uuid::parse_str(&id.profile_id).is_ok() => id,
            _ => {
                clear(&home, agent)?;
                let id = Identity {
                    profile_id: uuid::Uuid::new_v4().to_string(),
                    owners,
                };
                state
                    .write_file_atomic(IDENTITY, &serde_json::to_string(&id).map_err(failure)?)
                    .map_err(failure)?;
                id
            }
        };
        let mut hosts = hosts.to_vec();
        hosts.sort();
        hosts.dedup();
        Ok(Self {
            _lock: lock,
            home,
            agent: agent.into(),
            identity,
            hosts,
            config,
        })
    }

    fn aad(&self) -> Vec<u8> {
        serde_json::to_vec(&(
            "duduclaw/computer-profile/v1",
            &self.home,
            &self.agent,
            &self.identity.profile_id,
        ))
        .expect("profile context is serializable")
    }

    pub(crate) fn valid_owner(&self) -> Result<bool, String> {
        Self::owner_matches(&self.home, &self.agent, &self.identity)
    }

    fn owner_matches(home: &Path, agent: &str, expected: &Identity) -> Result<bool, String> {
        let identity: Identity = serde_json::from_slice(
            &agent_state(home, agent)?
                .read_bytes_checked(IDENTITY, HEADER_MAX, true)
                .map_err(failure)?
                .ok_or_else(|| failure("identity"))?,
        )
        .map_err(failure)?;
        Ok(identity == *expected && owners(home, agent)? == expected.owners)
    }

    /// A read-only owner proof without cloning the writer's lease or lock.
    pub(crate) fn owner_check(&self) -> std::sync::Arc<dyn Fn() -> bool + Send + Sync> {
        let (home, agent, identity) = (self.home.clone(), self.agent.clone(), self.identity.clone());
        std::sync::Arc::new(move || Self::owner_matches(&home, &agent, &identity).unwrap_or(false))
    }

    pub(crate) fn restore(&self) -> Result<(Option<Zeroizing<Vec<u8>>>, &'static str), String> {
        if !self.valid_owner()? {
            clear(&self.home, &self.agent)?;
            return Ok((None, "reset_owner_changed"));
        }
        let Some(meta) = state(&self.home, &self.agent)? else {
            return Ok((None, "empty"));
        };
        let expired = self.config.retention_days > 0
            && now().saturating_sub(meta.last_used_at)
                > i64::from(self.config.retention_days) * 86400;
        let reason = if expired {
            Some("reset_expired")
        } else if meta.hosts.iter().any(|h| !self.hosts.contains(h)) {
            Some("reset_allowlist_narrowed")
        } else if meta.version != 1
            || meta.agent_id != self.agent
            || meta.profile_id != self.identity.profile_id
        {
            Some("reset_unreadable")
        } else {
            None
        };
        if let Some(reason) = reason {
            clear(&self.home, &self.agent)?;
            return Ok((None, reason));
        }
        let dir = profile_dir(&self.home, &self.agent, false)?.ok_or_else(|| failure("profile"))?;
        let restored = (|| {
            let sealed = dir
                .read_bytes_checked(
                    &blob_name(meta.generation),
                    self.config.max_bytes + HEADER_MAX + 32,
                    true,
                )
                .map_err(failure)?
                .ok_or_else(|| failure("sealed profile"))?;
            let plain = Zeroizing::new(crate::config_crypto::open_bytes(
                &self.home,
                &self.aad(),
                &sealed,
            )?);
            let prefix: [u8; 4] = plain
                .get(..4)
                .ok_or_else(|| failure("header"))?
                .try_into()
                .map_err(failure)?;
            let len = u32::from_be_bytes(prefix) as usize;
            if len as u64 > HEADER_MAX || plain.len() < 4 + len {
                return Err(failure("header"));
            }
            let header: ProfileState =
                serde_json::from_slice(&plain[4..4 + len]).map_err(failure)?;
            if header.version != 1
                || header.agent_id != meta.agent_id
                || header.profile_id != meta.profile_id
                || header.generation != meta.generation
                || header.hosts != meta.hosts
                || (plain.len() - 4 - len) as u64 > self.config.max_bytes
            {
                return Err(failure("header context"));
            }
            Ok(Zeroizing::new(plain[4 + len..].to_vec()))
        })();
        match restored {
            Ok(bytes) => {
                let mut meta = meta;
                meta.last_used_at = now();
                dir.write_file_atomic(
                    "state.json",
                    &serde_json::to_string(&meta).map_err(failure)?,
                )
                .map_err(failure)?;
                Ok((Some(bytes), "restored"))
            }
            Err(_) => {
                clear(&self.home, &self.agent)?;
                Ok((None, "reset_unreadable"))
            }
        }
    }

    #[cfg(test)]
    fn save(&self, archive: &[u8]) -> Result<(), String> {
        self.save_before(
            archive,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        )
    }

    #[cfg(test)]
    fn save_before(&self, archive: &[u8], deadline: std::time::Instant) -> Result<(), String> {
        self.save_guarded_before(archive, deadline, &|| true)
    }

    pub(crate) fn save_guarded_before(
        &self,
        archive: &[u8],
        deadline: std::time::Instant,
        external_fence: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        self.save_with_pointer_fault(archive, deadline, external_fence, &|_| Ok(()))
    }

    fn save_with_pointer_fault(
        &self,
        archive: &[u8],
        deadline: std::time::Instant,
        external_fence: &dyn Fn() -> bool,
        pointer_fault: &dyn Fn(crate::fs_safe::WriteStep) -> io::Result<()>,
    ) -> Result<(), String> {
        let within_budget = || {
            external_fence()
                && std::time::Instant::now() < deadline
                && crate::computer_use_orchestrator::read_threat_level_sync(&self.home)
                    == crate::computer_use_orchestrator::ThreatLevel::Green
        };
        if !within_budget() {
            return Err(failure("unsafe or expired snapshot"));
        }
        if archive.len() as u64 > self.config.max_bytes {
            return Err(failure("archive too large"));
        }
        if !self.valid_owner()? {
            clear(&self.home, &self.agent)?;
            return Err(failure("owner changed"));
        }
        let current =
            ProfilesConfig::active(&self.home).ok_or_else(|| failure("profiles disabled"))?;
        if archive.len() as u64 > current.max_bytes {
            return Err(failure("archive too large"));
        }
        let caps = duduclaw_core::agent_toml::load(&self.home.join("agents").join(&self.agent))
            .capabilities;
        if !caps.computer_use || !caps.computer_use_config.persist_login {
            return Err(failure("capability disabled"));
        }
        let generation = state(&self.home, &self.agent)?
            .map_or(0, |s| s.generation)
            .checked_add(1)
            .ok_or_else(|| failure("generation overflow"))?;
        let mut meta = ProfileState {
            version: 1,
            profile_id: self.identity.profile_id.clone(),
            agent_id: self.agent.clone(),
            hosts: self.hosts.clone(),
            generation,
            sealed_bytes: 0,
            updated_at: now(),
            last_used_at: now(),
        };
        let header = serde_json::to_vec(&meta).map_err(failure)?;
        if header.len() as u64 > HEADER_MAX {
            return Err(failure("header too large"));
        }
        let mut plain = Zeroizing::new(Vec::with_capacity(4 + header.len() + archive.len()));
        plain.extend_from_slice(&(header.len() as u32).to_be_bytes());
        plain.extend_from_slice(&header);
        plain.extend_from_slice(archive);
        let sealed = crate::config_crypto::seal_bytes(&self.home, &self.aad(), &plain)?;
        meta.sealed_bytes = sealed.len() as u64;
        let dir = profile_dir(&self.home, &self.agent, true)?.ok_or_else(|| failure("profile"))?;
        let blob = blob_name(generation);
        dir.write_bytes_atomic_with(
            &blob,
            &sealed,
            &crate::fs_safe::tmp_name("profile"),
            &|_| Ok(()),
        )
        .map_err(failure)?;
        // The metadata is the atomic commit pointer. A failed candidate
        // write or pointer rename leaves the previous generation intact.
        let commit_guard = |step| {
            if !within_budget() {
                return Err(io::Error::other("snapshot deadline or threat fence"));
            }
            if step == crate::fs_safe::WriteStep::Rename {
                let caps =
                    duduclaw_core::agent_toml::load(&self.home.join("agents").join(&self.agent))
                        .capabilities;
                if !self.valid_owner().unwrap_or(false)
                    || ProfilesConfig::active(&self.home).is_none()
                    || !caps.computer_use
                    || !caps.computer_use_config.persist_login
                {
                    return Err(io::Error::other("snapshot permission changed"));
                }
            }
            pointer_fault(step)
        };
        let committed = dir.write_bytes_atomic_with(
            "state.json",
            &serde_json::to_vec(&meta).map_err(failure)?,
            &crate::fs_safe::tmp_name("state.json"),
            &commit_guard,
        );
        if let Err(error) = committed {
            let _ = dir.remove_file(&blob);
            return Err(failure(error));
        }
        // A crash before this cleanup can only leave extra encrypted
        // generations, never a pointer to partly written data.
        if let Ok(entries) = dir.list() {
            for entry in entries {
                if blob_generation(&entry.name).is_some_and(|g| g != generation) {
                    if let Err(error) = dir.remove_file(&entry.name) {
                        tracing::warn!(error = %error, "obsolete browser snapshot cleanup failed");
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests;
