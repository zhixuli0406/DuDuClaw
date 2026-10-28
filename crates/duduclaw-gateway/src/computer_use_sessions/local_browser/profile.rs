//! Dedicated browser state and security preferences. This is never an import
//! path for the user's normal Chrome profile or a managed OS browser policy.
use crate::fs_safe::SafeDir;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    epoch: String,
    owners: String,
    hosts: Vec<String>,
    #[serde(default)]
    clean: bool,
}
pub(crate) struct LocalProfile {
    _lock: File,
    machine_lease: super::machine_lease::MachineLease,
    home: PathBuf,
    agent: String,
    stamp: Stamp,
    pub path: PathBuf,
}

fn agent_state(home: &Path, agent: &str) -> Result<SafeDir, String> {
    let root = SafeDir::open_root(home).map_err(|_| "application home unavailable")?;
    root.child_dir("agents", false)
        .map_err(|_| "employee unavailable")?
        .ok_or("employee unavailable")?
        .child_dir(agent, false)
        .map_err(|_| "employee unavailable")?
        .ok_or("employee unavailable")?
        .child_dir("state", true)
        .map_err(|_| "employee state unavailable")?
        .ok_or_else(|| "employee state unavailable".into())
}
fn epoch(home: &Path, agent: &str) -> Result<String, String> {
    let state = agent_state(home, agent)?;
    if let Some(bytes) = state
        .read_bytes_checked("local_browser_epoch", 64, true)
        .map_err(|_| "local owner epoch unavailable")?
    {
        let epoch = std::str::from_utf8(&bytes).map_err(|_| "local owner epoch invalid")?;
        if uuid::Uuid::parse_str(epoch).is_err() {
            return Err("local owner epoch invalid".into());
        }
        return Ok(epoch.into());
    }
    let epoch = uuid::Uuid::new_v4().to_string();
    state
        .write_file_atomic("local_browser_epoch", &epoch)
        .map_err(|_| "local owner epoch unavailable")?;
    Ok(epoch)
}

/// Integration hook: call on EVERY primary-owner transition, including A→B→A.
/// The live backend observes the changed epoch and stops before more input;
/// the next exclusive profile preparation erases the old dedicated state.
pub(crate) fn invalidate_owner(home: &Path, agent: &str) -> Result<(), String> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err("invalid employee".into());
    }
    agent_state(home, agent)?
        .write_file_atomic("local_browser_epoch", &uuid::Uuid::new_v4().to_string())
        .map_err(|_| "local owner epoch unavailable".into())
}

fn stamp(home: &Path, agent: &str, hosts: Vec<String>) -> Result<Stamp, String> {
    let owners = crate::computer_profiles::owners(home, agent)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&owners).map_err(|_| "local owner unavailable")?)
    );
    Ok(Stamp {
        epoch: epoch(home, agent)?,
        owners: digest,
        hosts,
        clean: false,
    })
}

pub(crate) fn has_profile(home: &Path, agent: &str) -> Result<bool, String> {
    Ok(super::policy::dedicated_profile(home, agent)?
        .parent()
        .is_some_and(Path::exists))
}
/// A first native start creates its ownership epoch before a profile exists.
/// Do not create state for employees who have never used this feature.
pub(crate) fn has_identity(home: &Path, agent: &str) -> Result<bool, String> {
    if has_profile(home, agent)? {
        return Ok(true);
    }
    let root = SafeDir::open_root(home).map_err(|_| "local identity unavailable")?;
    let Some(agents) = root
        .child_dir("agents", false)
        .map_err(|_| "local identity unavailable")?
    else {
        return Ok(false);
    };
    let Some(agent) = agents
        .child_dir(agent, false)
        .map_err(|_| "local identity unavailable")?
    else {
        return Ok(false);
    };
    let Some(state) = agent
        .child_dir("state", false)
        .map_err(|_| "local identity unavailable")?
    else {
        return Ok(false);
    };
    Ok(state
        .read_bytes_checked("local_browser_epoch", 64, true)
        .map_err(|_| "local identity unavailable")?
        .is_some())
}

pub(super) fn native_owner_binding(home: &Path, agent: &str) -> Result<String, String> {
    let owners = crate::computer_profiles::owners(home, agent)?
        .ok_or("native_start_active_owner_required")?;
    if !owners.iter().any(|(_, metadata)| {
        serde_json::from_str::<Value>(metadata).is_ok_and(|row| row[1].as_str() == Some("active"))
    }) {
        return Err("native_start_active_owner_required".into());
    }
    let binding = stamp(home, agent, Vec::new())?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&binding).map_err(|_| "local owner unavailable")?)
    ))
}
pub(super) fn read_binding(home: &Path, agent: &str) -> Result<String, String> {
    let stamp = stamp(home, agent, Vec::new())?;
    let policy = crate::approval::policy_revision(home, agent)?;
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(stamp, policy)).map_err(|_| "local read binding unavailable")?
        )
    ))
}

#[cfg(unix)]
pub(crate) fn forget_profile(home: &Path, agent: &str) -> Result<(), String> {
    let path = super::policy::dedicated_profile(home, agent)?;
    if !path.exists() {
        return Ok(());
    }
    let _lease = lock(home, agent)?;
    // No user-selected paths; links in the entire derived path were refused.
    std::fs::remove_dir_all(path).map_err(|_| "local profile cleanup pending".into())
}

#[cfg(unix)]
fn lock(home: &Path, agent: &str) -> Result<File, String> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let root = SafeDir::open_root(home).map_err(|_| "local profile unavailable")?;
    let local = root
        .child_dir("local_browser", true)
        .map_err(|_| "local profile unavailable")?
        .ok_or("local profile unavailable")?;
    let _locks = local
        .child_dir("locks", true)
        .map_err(|_| "local profile lock unavailable")?
        .ok_or("local profile lock unavailable")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(
            home.join("local_browser/locks")
                .join(format!("{agent}.lock")),
        )
        .map_err(|_| "local profile lock unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "local profile lock unavailable")?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("unsafe local profile lock".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("local profile busy".into());
    }
    Ok(file)
}

impl LocalProfile {
    #[cfg(unix)]
    pub(crate) fn prepare(home: &Path, agent: &str, hosts: Vec<String>) -> Result<Self, String> {
        let home = home
            .canonicalize()
            .map_err(|_| "application home unavailable")?;
        let path = super::policy::dedicated_profile(&home, agent)?;
        let machine_lock = machine_lock()?;
        let lease = lock(&home, agent)?;
        let root = SafeDir::open_root(&home).map_err(|_| "local profile unavailable")?;
        let directory = root
            .child_dir("local_browser", true)
            .map_err(|_| "local profile unavailable")?
            .ok_or("local profile unavailable")?
            .child_dir(agent, true)
            .map_err(|_| "local profile unavailable")?
            .ok_or("local profile unavailable")?;
        let stamp = stamp(&home, agent, hosts)?;
        let previous = directory
            .read_bytes_checked("identity.json", 8192, true)
            .map_err(|_| "local identity unavailable")?
            .map(|bytes| {
                serde_json::from_slice::<Stamp>(&bytes).map_err(|_| "local identity invalid")
            })
            .transpose()?;
        let reset = previous.as_ref().is_none_or(|prior| {
            !prior.clean
                || prior.epoch != stamp.epoch
                || prior.owners != stamp.owners
                || prior.hosts.iter().any(|h| !stamp.hosts.contains(h))
        });
        if reset && path.exists() {
            // The path is derived internally, checked against default browser
            // trees and symlinks, and held behind the exclusive employee lease.
            std::fs::remove_dir_all(&path).map_err(|_| "local profile reset failed")?;
        }
        let profile = directory
            .child_dir("profile", true)
            .map_err(|_| "local profile unavailable")?
            .ok_or("local profile unavailable")?;
        let default = profile
            .child_dir("Default", true)
            .map_err(|_| "local preferences unavailable")?
            .ok_or("local preferences unavailable")?;
        let mut preferences = default
            .read_bytes_checked("Preferences", 2 * 1024 * 1024, true)
            .map_err(|_| "local preferences unavailable")?
            .map(|bytes| {
                serde_json::from_slice::<Value>(&bytes).map_err(|_| "local preferences invalid")
            })
            .transpose()?
            .unwrap_or_else(|| json!({}));
        apply_security_preferences(&mut preferences)?;
        // Defense in depth for a broken/changed download-deny implementation:
        // even its fallback path is an owned directory, never OS Downloads.
        directory
            .child_dir("downloads-disabled", true)
            .map_err(|_| "local download fence unavailable")?
            .ok_or("local download fence unavailable")?;
        let download = preferences
            .as_object_mut()
            .ok_or("local preferences invalid")?
            .entry("download")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("local download preferences invalid")?;
        download.insert(
            "default_directory".into(),
            json!(
                path.parent()
                    .ok_or("local profile unavailable")?
                    .join("downloads-disabled")
            ),
        );
        download.insert("prompt_for_download".into(), json!(false));
        default
            .write_file_atomic(
                "Preferences",
                &serde_json::to_string(&preferences).map_err(|_| "local preferences invalid")?,
            )
            .map_err(|_| "local preferences unavailable")?;
        directory
            .write_file_atomic(
                "identity.json",
                &serde_json::to_string(&stamp).map_err(|_| "local identity invalid")?,
            )
            .map_err(|_| "local identity unavailable")?;
        Ok(Self {
            _lock: lease,
            machine_lease: machine_lock,
            home,
            agent: agent.into(),
            stamp,
            path,
        })
    }
    pub(crate) fn valid(&self) -> bool {
        let caps = super::super::agent_capabilities(&self.home, &self.agent);
        let current = caps.computer_use_config.navigation_hosts();
        super::policy::dedicated_profile(&self.home, &self.agent)
            .is_ok_and(|path| path == self.path && path.is_dir())
            && self
                .stamp
                .hosts
                .iter()
                .all(|host| current.hosts.contains(host))
            && stamp(&self.home, &self.agent, self.stamp.hosts.clone())
                .is_ok_and(|now| now == self.stamp)
    }
    pub(crate) fn begin_browser_launch(&self) -> Result<(), String> {
        self.machine_lease.begin_launch()
    }
    pub(crate) fn acknowledge_browser_exit(&self) -> Result<(), String> {
        self.machine_lease.acknowledge_exit()
    }
    /// Only after the owned browser tree is gone and the session did not
    /// request discard. Crashes/failed cleanup leave the durable dirty stamp.
    pub(crate) fn mark_clean(&self) -> Result<(), String> {
        if !self.valid() {
            return Err("local owner changed".into());
        }
        let root = SafeDir::open_root(&self.home).map_err(|_| "local profile unavailable")?;
        let directory = root
            .child_dir("local_browser", false)
            .map_err(|_| "local profile unavailable")?
            .ok_or("local profile unavailable")?
            .child_dir(&self.agent, false)
            .map_err(|_| "local profile unavailable")?
            .ok_or("local profile unavailable")?;
        let mut stamp = self.stamp.clone();
        stamp.clean = true;
        directory
            .write_file_atomic(
                "identity.json",
                &serde_json::to_string(&stamp).map_err(|_| "local identity invalid")?,
            )
            .map_err(|_| "local identity unavailable".into())
    }
}

pub(crate) fn apply_security_preferences(preferences: &mut Value) -> Result<(), String> {
    let object = preferences
        .as_object_mut()
        .ok_or("local preferences must be an object")?;
    object.insert("credentials_enable_service".into(), json!(false));
    for (section, entries) in [
        ("printing", vec![("enabled", json!(false))]),
        (
            "webrtc",
            vec![
                ("ip_handling_policy", json!("disable_non_proxied_udp")),
                ("ip_handling_url", json!([])),
            ],
        ),
        (
            "autofill",
            vec![
                ("profile_enabled", json!(false)),
                ("credit_card_enabled", json!(false)),
            ],
        ),
        (
            "profile",
            vec![
                ("password_manager_enabled", json!(false)),
                ("password_manager_leak_detection", json!(false)),
            ],
        ),
    ] {
        let table = object
            .entry(section)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("invalid security preference section")?;
        for (key, value) in entries {
            table.insert(key.into(), value);
        }
    }
    let profile = object
        .get_mut("profile")
        .and_then(Value::as_object_mut)
        .ok_or("invalid profile settings")?;
    profile.insert("default_content_setting_values".into(),json!({"notifications":2,"geolocation":2,"media_stream_mic":2,"media_stream_camera":2,"automatic_downloads":2,"clipboard":2,"midi_sysex":2,"usb_guard":2,"serial_guard":2,"hid_guard":2,"file_system_read_guard":2,"file_system_write_guard":2,"local_fonts":2}));
    // A previous interactive login may have left per-origin grants. Never
    // restore them over the dedicated profile's default-deny settings.
    let content = profile
        .entry("content_settings")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid content settings")?;
    content.insert("exceptions".into(), json!({}));
    Ok(())
}

#[cfg(unix)]
fn machine_lock() -> Result<super::machine_lease::MachineLease, String> {
    super::machine_lease::MachineLease::acquire(&super::machine_lease::path())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn security_preferences_remove_webrtc_overrides_without_destroying_other_preferences() {
        let mut preferences = json!({"webrtc":{"ip_handling_policy":"default","ip_handling_url":[{"url":"*","handling":"default"}]},"unrelated":{"keep":true}});
        apply_security_preferences(&mut preferences).unwrap();
        assert_eq!(
            preferences["webrtc"]["ip_handling_policy"],
            "disable_non_proxied_udp"
        );
        assert_eq!(preferences["webrtc"]["ip_handling_url"], json!([]));
        assert_eq!(preferences["unrelated"]["keep"], true);
        assert_eq!(preferences["printing"]["enabled"], false);
        assert_eq!(
            preferences["profile"]["default_content_setting_values"]["file_system_read_guard"],
            2
        );
        assert_eq!(
            preferences["profile"]["content_settings"]["exceptions"],
            json!({})
        );
        assert!(apply_security_preferences(&mut json!([])).is_err());
        assert!(apply_security_preferences(&mut json!({"webrtc":false})).is_err());
    }
    #[test]
    fn stale_origin_grants_are_removed_before_chrome_starts() {
        let mut preferences = json!({"profile":{"content_settings":{"exceptions":{"file_system_read_guard":{"https://allowed.example,*":{"setting":1}}},"keep":true}}});
        apply_security_preferences(&mut preferences).unwrap();
        assert_eq!(
            preferences["profile"]["content_settings"]["exceptions"],
            json!({})
        );
        assert_eq!(preferences["profile"]["content_settings"]["keep"], true);
    }
    #[test]
    #[cfg(unix)]
    fn only_a_verified_clean_shutdown_preserves_the_dedicated_login() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("agents/alice")).unwrap();
        std::fs::write(
            root.path().join("agents/alice/agent.toml"),
            "[capabilities]\ncomputer_use=true\n",
        )
        .unwrap();
        let profile = LocalProfile::prepare(root.path(), "alice", vec![]).unwrap();
        let sentinel = profile.path.join("Default/Cookies");
        std::fs::write(&sentinel, "synthetic login").unwrap();
        profile.mark_clean().unwrap();
        drop(profile);
        let profile = LocalProfile::prepare(root.path(), "alice", vec![]).unwrap();
        assert!(sentinel.exists());
        drop(profile); // crash/unknown cleanup: no clean acknowledgement
        let profile = LocalProfile::prepare(root.path(), "alice", vec![]).unwrap();
        assert!(!sentinel.exists());
        std::fs::write(&sentinel, "synthetic next login").unwrap();
        profile.mark_clean().unwrap();
        drop(profile);
        invalidate_owner(root.path(), "alice").unwrap();
        let _profile = LocalProfile::prepare(root.path(), "alice", vec![]).unwrap();
        assert!(!sentinel.exists());
    }
}
