//! Bounded, read-only source observations. Never creates a lock or opens a key store.
use crate::google_identity::Refusal;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    io::Read,
};
use zeroize::Zeroizing;
fn changed() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "Google 帳號或來源已變更，請重新選擇並核准。",
    )
}
#[derive(PartialEq, Eq)]
struct Identity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    ctime: (i64, i64),
}
impl Identity {
    fn of(m: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: m.len(),
            modified: m.modified().ok(),
            readonly: m.permissions().readonly(),
            #[cfg(unix)]
            dev: m.dev(),
            #[cfg(unix)]
            ino: m.ino(),
            #[cfg(unix)]
            mode: m.mode(),
            #[cfg(unix)]
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }
}
pub(crate) struct FileProof {
    path: PathBuf,
    canonical: PathBuf,
    identity: Identity,
    digest: [u8; 32],
    limit: usize,
}
impl FileProof {
    pub(crate) fn capture(
        path: &Path,
        limit: usize,
    ) -> Result<(Self, Zeroizing<Vec<u8>>), Refusal> {
        let canonical = path.canonicalize().map_err(|_| changed())?;
        let before = std::fs::metadata(&canonical).map_err(|_| changed())?;
        if !before.is_file() || before.len() > limit as u64 {
            return Err(changed());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&canonical).map_err(|_| changed())?;
        let opened = file.metadata().map_err(|_| changed())?;
        if Identity::of(&before) != Identity::of(&opened) {
            return Err(changed());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        (&mut file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| changed())?;
        let after = file.metadata().map_err(|_| changed())?;
        let path_after = std::fs::metadata(path).map_err(|_| changed())?;
        let identity = Identity::of(&opened);
        if bytes.len() > limit
            || identity != Identity::of(&after)
            || identity != Identity::of(&path_after)
            || path.canonicalize().map_err(|_| changed())? != canonical
        {
            return Err(changed());
        }
        let digest = Sha256::digest(&*bytes).into();
        Ok((
            Self {
                path: path.into(),
                canonical,
                identity,
                digest,
                limit,
            },
            bytes,
        ))
    }
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        let (actual, _) = Self::capture(&self.path, self.limit)?;
        if self.canonical != actual.canonical
            || self.identity != actual.identity
            || self.digest != actual.digest
        {
            Err(changed())
        } else {
            Ok(())
        }
    }
    pub(crate) fn weight(&self) -> usize {
        std::mem::size_of_val(self) + self.path.capacity() + self.canonical.capacity()
    }
    pub(crate) fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.digest);
        h.update(self.canonical.as_os_str().to_string_lossy().as_bytes());
        h.update(self.identity.len.to_be_bytes());
        h.update(format!("{:?}:{}", self.identity.modified, self.identity.readonly).as_bytes());
        #[cfg(unix)]
        {
            h.update(self.identity.dev.to_be_bytes());
            h.update(self.identity.ino.to_be_bytes());
            h.update(self.identity.mode.to_be_bytes());
            h.update(self.identity.ctime.0.to_be_bytes());
            h.update(self.identity.ctime.1.to_be_bytes());
        }
        format!("{:x}", h.finalize())
    }
}
struct OptionalFile {
    path: PathBuf,
    proof: Option<FileProof>,
}
impl OptionalFile {
    fn capture(path: PathBuf, limit: usize) -> Result<Self, Refusal> {
        let proof = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(FileProof::capture(&path, limit)?.0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(changed()),
        };
        Ok(Self { path, proof })
    }
    fn validate(&self) -> Result<(), Refusal> {
        match &self.proof {
            Some(p) => p.validate(),
            None => match std::fs::symlink_metadata(&self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(changed()),
            },
        }
    }
    fn weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.path.capacity()
            + self.proof.as_ref().map_or(0, FileProof::weight)
    }
}
// Projection drops unrelated settings; raw config never becomes a wire receipt.
fn projection(home: &Path) -> Result<(String, String), Refusal> {
    let (_, bytes) = FileProof::capture(&home.join("config.toml"), 1024 * 1024)?;
    let raw = std::str::from_utf8(&bytes).map_err(|_| changed())?;
    let base = super::config_projection(raw).map_err(|_| changed())?;
    let table: toml::Table = toml::from_str(raw).map_err(|_| changed())?;
    let identity = table
        .get("integrations")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("google_identity"));
    let projection = serde_json::to_vec(&serde_json::json!({"base":base,"home_identity":identity}))
        .map_err(|_| changed())?;
    Ok((format!("{:x}", Sha256::digest(&projection)), raw.to_owned()))
}
pub(crate) struct SourceSnapshot {
    home: PathBuf,
    home_identity: Identity,
    agent: String,
    agent_dir: PathBuf,
    agent_identity: Identity,
    config_projection: String,
    policy: duduclaw_core::google_identity::EffectiveIdentity,
    control: serde_json::Value,
    client: OptionalFile,
    marker: OptionalFile,
    keyfile: OptionalFile,
}
impl SourceSnapshot {
    pub(crate) fn capture(home: &Path, agent: &str) -> Result<Self, Refusal> {
        crate::google_personal::grants::check_metadata_barrier(home).map_err(|_| changed())?;
        let home = home.canonicalize().map_err(|_| changed())?;
        let dir = home
            .join("agents")
            .join(agent)
            .canonicalize()
            .map_err(|_| changed())?;
        if dir.parent()
            != Some(
                home.join("agents")
                    .canonicalize()
                    .map_err(|_| changed())?
                    .as_path(),
            )
        {
            return Err(changed());
        }
        let config_projection = projection(&home)?.0;
        let policy = duduclaw_core::google_identity::effective_for(&home, Some(agent));
        let snapshot = Self {
            home_identity: directory_identity(&std::fs::metadata(&home).map_err(|_| changed())?),
            agent_identity: directory_identity(&std::fs::metadata(&dir).map_err(|_| changed())?),
            agent_dir: dir,
            agent: agent.into(),
            control: crate::mcp_oauth::home_store::closed_readonly_control(&home)
                .map_err(|_| changed())?,
            client: OptionalFile::capture(
                home.join(crate::mcp_oauth::CLIENT_CONFIG_FILE),
                1024 * 1024,
            )?,
            marker: OptionalFile::capture(home.join("credential_protection.json"), 64 * 1024)?,
            keyfile: OptionalFile::capture(home.join(".keyfile"), 64 * 1024)?,
            home,
            config_projection,
            policy,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        crate::google_personal::grants::check_metadata_barrier(&self.home)
            .map_err(|_| changed())?;
        if self.home.canonicalize().map_err(|_| changed())? != self.home
            || directory_identity(&std::fs::metadata(&self.home).map_err(|_| changed())?)
                != self.home_identity
            || self.agent_dir.canonicalize().map_err(|_| changed())? != self.agent_dir
            || directory_identity(&std::fs::metadata(&self.agent_dir).map_err(|_| changed())?)
                != self.agent_identity
            || projection(&self.home)?.0 != self.config_projection
            || duduclaw_core::google_identity::effective_for(&self.home, Some(&self.agent))
                != self.policy
            || crate::mcp_oauth::home_store::closed_readonly_control(&self.home)
                .map_err(|_| changed())?
                != self.control
        {
            return Err(changed());
        }
        self.client.validate()?;
        self.marker.validate()?;
        self.keyfile.validate()?;
        crate::google_personal::grants::check_metadata_barrier(&self.home)
            .map_err(|_| changed())?;
        Ok(())
    }
    pub(crate) fn validate_without_oauth_control(&self) -> Result<(), Refusal> {
        crate::google_personal::grants::check_metadata_barrier(&self.home)
            .map_err(|_| changed())?;
        if self.home.canonicalize().map_err(|_| changed())? != self.home
            || directory_identity(&std::fs::metadata(&self.home).map_err(|_| changed())?)
                != self.home_identity
            || self.agent_dir.canonicalize().map_err(|_| changed())? != self.agent_dir
            || directory_identity(&std::fs::metadata(&self.agent_dir).map_err(|_| changed())?)
                != self.agent_identity
            || projection(&self.home)?.0 != self.config_projection
            || duduclaw_core::google_identity::effective_for(&self.home, Some(&self.agent))
                != self.policy
        {
            return Err(changed());
        }
        self.client.validate()?;
        self.marker.validate()?;
        self.keyfile.validate()?;
        Ok(())
    }
    pub(crate) fn refresh_after_resolution(&mut self) -> Result<(), Refusal> {
        self.validate_without_oauth_control()?;
        self.control = crate::mcp_oauth::home_store::closed_readonly_control(&self.home)
            .map_err(|_| changed())?;
        self.validate()
    }
    pub(crate) fn adopt_prepared_oauth(
        &mut self,
        id: &str,
        epoch: u64,
        generation: &str,
    ) -> Result<(), Refusal> {
        self.validate_without_oauth_control()?;
        let control = crate::mcp_oauth::home_store::closed_readonly_control(&self.home)
            .map_err(|_| changed())?;
        if control
            .get("source_generation")
            .and_then(serde_json::Value::as_str)
            != Some(generation)
            || control
                .pointer("/credential/credential_id")
                .and_then(serde_json::Value::as_str)
                != Some(id)
            || control
                .pointer("/credential/logical_epoch")
                .and_then(serde_json::Value::as_u64)
                != Some(epoch)
        {
            return Err(changed());
        }
        self.control = control;
        self.validate()
    }
    pub(crate) fn generation(&self) -> Option<String> {
        self.control
            .get("source_generation")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    }
    pub(crate) fn digest(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "{}|{}|{:?}|{}",
                    self.home.display(),
                    self.config_projection,
                    self.policy,
                    self.control
                )
                .as_bytes()
            )
        )
    }
    pub(crate) fn weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.home.capacity()
            + self.agent.capacity()
            + self.agent_dir.capacity()
            + self.config_projection.capacity()
            + policy_weight(&self.policy)
            + value_weight(&self.control)
            + self.client.weight()
            + self.marker.weight()
            + self.keyfile.weight()
    }
}

/// A failed direct attempt still binds its locally observable key dependency.
/// Unreadable stays unreadable; recovery to readable or missing changes state.
pub(crate) struct LocalDependency {
    path: PathBuf,
    limit: usize,
    state: String,
}
impl LocalDependency {
    pub(crate) fn capture(path: &Path, limit: usize) -> Self {
        Self {
            path: path.into(),
            limit,
            state: Self::state(path, limit),
        }
    }
    fn state(path: &Path, limit: usize) -> String {
        match FileProof::capture(path, limit) {
            Ok((p, _)) => format!("ready:{}:{}", p.digest(), p.canonical.display()),
            Err(_) => match std::fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".into(),
                Err(e) => format!("unreadable:{:?}", e.kind()),
                Ok(m) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        format!(
                            "invalid:{}:{}:{}:{}:{}",
                            m.dev(),
                            m.ino(),
                            m.mode(),
                            m.len(),
                            m.mtime()
                        )
                    }
                    #[cfg(not(unix))]
                    {
                        format!("invalid:{}:{:?}", m.len(), m.modified().ok())
                    }
                }
            },
        }
    }
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        if Self::state(&self.path, self.limit) == self.state {
            Ok(())
        } else {
            Err(changed())
        }
    }
    pub(crate) fn weight(&self) -> usize {
        std::mem::size_of_val(self) + self.path.as_os_str().len() + self.state.capacity()
    }
}

fn directory_identity(meta: &std::fs::Metadata) -> Identity {
    let mut identity = Identity::of(meta);
    identity.len = 0;
    identity.modified = None;
    #[cfg(unix)]
    {
        identity.ctime = (0, 0);
    }
    identity
}
fn policy_weight(policy: &duduclaw_core::google_identity::EffectiveIdentity) -> usize {
    use duduclaw_core::google_identity::{Effective, Selector};
    match &policy.effective {
        Effective::Delegate { email } | Effective::Invalid(email) => email.capacity(),
        Effective::Personal {
            selector: Selector::User(value) | Selector::Delegate(value),
        } => value.capacity(),
        _ => 0,
    }
}
fn value_weight(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    std::mem::size_of_val(value)
        + match value {
            Value::String(s) => s.capacity(),
            Value::Array(a) => {
                a.capacity() * std::mem::size_of::<Value>()
                    + a.iter().map(value_weight).sum::<usize>()
            }
            Value::Object(o) => {
                o.len()
                    * (std::mem::size_of::<(String, Value)>() + 4 * std::mem::size_of::<usize>())
                    + o.iter()
                        .map(|(k, v)| k.capacity() + value_weight(v))
                        .sum::<usize>()
            }
            _ => 0,
        }
}

#[cfg(test)]
mod w3_source_oracles {
    use super::*;
    #[test]
    fn w3_source_capture_detects_equal_bytes_replaced_inode() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("key.json");
        std::fs::write(&path, b"same").unwrap();
        let (proof, _) = FileProof::capture(&path, 64).unwrap();
        let replacement = temp.path().join("new");
        std::fs::write(&replacement, b"same").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(proof.validate().is_err());
    }
    #[test]
    fn w3_source_capture_enforces_bound_while_reading_and_no_lock_creation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("key.json");
        std::fs::write(&path, b"12345").unwrap();
        assert!(FileProof::capture(&path, 4).is_err());
        let (proof, _) = FileProof::capture(&path, 5).unwrap();
        proof.validate().unwrap();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
