//! Final policy lease: re-evaluate live policy while holding its writers' locks.
use super::*;
use duduclaw_agent::registry::AgentRegistry;
use std::fs::{File, OpenOptions};

/// The account snapshot is valid only inside the caller's users.db generation
/// lease and the edition lease captured when team grants were filtered.
/// Config and registry are NOT cached: the final synchronous operation
/// reads both under their existing shared locks and keeps them through writing.
/// This is the lock-based equivalent of a checked policy revision; there is no
/// counter that a config writer, registry scan or replacement can omit.
/// Edition changes are serialized separately by team_edition::record.
pub(crate) struct TargetPolicy {
    home: PathBuf,
    registry: Arc<tokio::sync::RwLock<AgentRegistry>>,
    sender: Option<SenderAccess>,
    edition: Option<crate::team_edition::EditionSnapshot>,
    /// `None`: no target employee (the no-responder notice).
    agent: Option<String>,
    public: bool,
}

impl WebChatState {
    /// Call AFTER capturing acl_generation, never while holding a users.db
    /// lease (acl_snapshot itself reads that database).
    pub(in crate::webchat) async fn target_policy(&self, user: &str, agent: &str, public: bool) -> Result<TargetPolicy, String> {
        self.turn_policy(user, Some(agent), public).await
    }

    /// [`Self::target_policy`], or with `None` the target-less policy of the
    /// no-responder notice: the account is checked Active here, after the
    /// caller captured acl_generation, so any later status change fails that
    /// generation's lease at the final write. Same calling rules.
    pub(in crate::webchat) async fn turn_policy(&self, user: &str, agent: Option<&str>, public: bool) -> Result<TargetPolicy, String> {
        let (sender, edition) = if is_widget_visitor(user) { (None, None) } else {
            if agent.is_none() {
                self.authorize_no_target(user).await.map_err(|_| "account unavailable".to_owned())?;
            }
            let snapshot = self.acl_snapshot(user).await?;
            (Some(SenderAccess::from_effective(snapshot.access)
                .for_webchat_converse(&self.ctx.home_dir).await), Some(snapshot.edition))
        };
        Ok(TargetPolicy {
            home: self.ctx.home_dir.clone(), registry: self.ctx.registry.clone(),
            sender, edition, agent: agent.map(str::to_owned), public,
        })
    }
}

impl TargetPolicy {
    /// No await, no blocking lock acquisition. Contention denies this delivery.
    /// All guards survive the callback, including socket start_send / SQL UPDATE.
    /// Never hold these guards while polling a socket or acquiring a session pool
    /// connection: writers may themselves await the registry or touch users.db.
    pub(crate) fn with_current<T>(&self, write: impl FnOnce() -> T) -> Result<T, String> {
        match &self.edition {
            Some(edition) => edition.with_current(|| self.with_agent_current(write))?,
            None => self.with_agent_current(write),
        }
    }

    fn with_agent_current<T>(&self, write: impl FnOnce() -> T) -> Result<T, String> {
        // Preserve authorize_target's explicit Admin and anonymous-widget rules.
        let Some(sender) = &self.sender else { return Ok(write()); };
        if matches!(sender.sender, crate::trigger_route::Sender::Admin { .. }) { return Ok(write()); }
        // No target employee: nothing here depends on one. The account was
        // Active when this policy was built; the caller's generation lease
        // refuses any later change to it.
        let Some(agent) = &self.agent else { return Ok(write()); };
        let path = self.home.join("config.toml");
        // Same sidecar as duduclaw_core::with_file_lock, including cross-process
        // dashboard/raw-editor/CLI commits. Atomic rename does not replace it.
        let lock: File = OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(self.home.join("config.toml.lock")).map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_shared(&lock).map_err(|e| e.to_string())?;
        let registry = self.registry.try_read().map_err(|e| e.to_string())?;
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let sender = sender.clone().for_webchat(Ok(parse_gateway_default(&raw)?));
        let loaded = registry.get(agent).ok_or("unknown agent")?;
        if !target_allowed(&sender, loaded, self.public) { return Err("agent policy changed".into()); }
        // File unlocks on drop, even when write returns an error or panics.
        Ok(write())
    }

    #[cfg(test)]
    pub(crate) fn test_admin() -> Self {
        Self {
            home: PathBuf::new(), registry: Arc::new(tokio::sync::RwLock::new(AgentRegistry::new(PathBuf::new()))),
            sender: Some(SenderAccess { sender: crate::trigger_route::Sender::Admin { user_id: None }, webchat_scope: None }),
            edition: None, agent: None, public: true,
        }
    }
}
