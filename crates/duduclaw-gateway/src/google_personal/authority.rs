//! Nonsecret, host-owned account selection. A selection survives ordinary
//! access-token refresh, but never a new grant, consent or owner generation.
use crate::google_identity::{
    Refusal,
    requester_gate::{self, People},
};
use crate::person_credentials as vault;
use duduclaw_core::google_identity::{self as gi, CredentialRef, Effective, Requesters};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;

fn changed() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "Google 帳號或授權設定已變更，本次操作已停止。請重新選擇帳號並核准。",
    )
}
fn store_error(_: impl std::fmt::Display) -> Refusal {
    changed()
}

/// Validate only the client metadata; never unseal an OAuth secret. A
/// normalization write must not precede this fallible configuration check.
fn check_client_metadata(home: &Path, metadata: &vault::Metadata) -> Result<(), Refusal> {
    let raw = std::fs::read(home.join("mcp-oauth-configs.json")).map_err(store_error)?;
    let configs: Vec<serde_json::Value> = serde_json::from_slice(&raw).map_err(store_error)?;
    let client = configs
        .iter()
        .find(|c| c["provider_id"] == "google")
        .ok_or_else(changed)?;
    if client["client_id"].as_str() != Some(metadata.oauth_client_id.as_str()) {
        return Err(changed());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStamp {
    digest: String,
    modified_ns: u128,
    created_ns: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
struct FileSnapshot {
    bytes: zeroize::Zeroizing<Vec<u8>>,
    stamp: FileStamp,
}

// The pre-existing bounded stamp reader now retains its already-owned bytes.
// Metadata and path checks are unchanged; ordinary stamp callers discard bytes.
fn file_snapshot(path: &Path) -> Result<Option<FileSnapshot>, Refusal> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(changed()),
    };
    let before = file.metadata().map_err(store_error)?;
    if !before.is_file() || before.len() > 4 * 1024 * 1024 {
        return Err(changed());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.by_ref()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(store_error)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(changed());
    }
    let stamp = |meta: &std::fs::Metadata| -> Result<FileStamp, Refusal> {
        let ns = |time: std::io::Result<std::time::SystemTime>| {
            time.map_err(store_error)?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|t| t.as_nanos())
                .map_err(store_error)
        };
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(FileStamp {
            digest: format!("{:x}", Sha256::digest(bytes.as_slice())),
            modified_ns: ns(meta.modified())?,
            created_ns: {
                #[cfg(unix)]
                {
                    meta.created()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|t| t.as_nanos())
                        .unwrap_or(0)
                }
                #[cfg(not(unix))]
                {
                    ns(meta.created())?
                }
            },
            #[cfg(unix)]
            device: meta.dev(),
            #[cfg(unix)]
            inode: meta.ino(),
        })
    };
    let selected = stamp(&before)?;
    if selected != stamp(&file.metadata().map_err(store_error)?)?
        || selected != stamp(&std::fs::metadata(path).map_err(store_error)?)?
    {
        return Err(changed());
    }
    Ok(Some(FileSnapshot {
        bytes,
        stamp: selected,
    }))
}
fn file_stamp(path: &Path) -> Result<Option<FileStamp>, Refusal> {
    Ok(file_snapshot(path)?.map(|snapshot| snapshot.stamp))
}

/// Contains account metadata only. Token revisions are carried separately by
/// PersonalAuthority so an ordinary refresh does not invalidate approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalSelection {
    home_digest: String,
    agent: String,
    tool: String,
    selector: String,
    requesters: String,
    fallback_home: bool,
    agent_file: FileStamp,
    home_file: FileStamp,
    client_file: Option<FileStamp>,
    acl_generation: u64,
    primary_generation: u64,
    user_id: Option<String>,
    person_email: Option<String>,
    grant: Option<vault::CredentialAuthority>,
    grant_epoch: u64,
    unavailable: Option<String>,
}
impl PersonalSelection {
    pub fn capture(
        home: &Path,
        agent: &str,
        who: &RequesterIdentity,
        people: &dyn People,
        tool: &str,
    ) -> Result<Self, Refusal> {
        Self::capture_impl(home, agent, who, people, tool, None)
    }
    /// Gateway-only preparation may explicitly normalize a resumed person's
    /// metadata. Read-only capture/validate never perform that write. Facts
    /// and generations are fixed BEFORE the CAS; it never reselects a user.
    pub(crate) fn prepare_dedicated(
        home: &Path,
        agent: &str,
        who: &RequesterIdentity,
        people: &dyn People,
        tool: &str,
        authorize_host: impl Fn() -> Result<(), Refusal>,
    ) -> Result<Self, Refusal> {
        Self::capture_impl(home, agent, who, people, tool, Some(&authorize_host))
    }
    fn capture_impl(
        home: &Path,
        agent: &str,
        who: &RequesterIdentity,
        people: &dyn People,
        tool: &str,
        authorize_host: Option<&dyn Fn() -> Result<(), Refusal>>,
    ) -> Result<Self, Refusal> {
        if !duduclaw_core::is_valid_agent_id(agent) {
            return Err(changed());
        }
        let home = home.canonicalize().map_err(store_error)?;
        let agent_path = home.join("agents").join(agent).join("agent.toml");
        let agent_file = file_stamp(&agent_path)?.ok_or_else(changed)?;
        let home_file = file_stamp(&home.join("config.toml"))?.ok_or_else(changed)?;
        let client_file = file_stamp(&home.join("mcp-oauth-configs.json"))?;
        let effective = gi::effective_for(&home, Some(agent));
        let Effective::Personal { selector } = &effective.effective else {
            return Err(changed());
        };
        if !crate::team_edition::team_grants_enabled(&home) {
            return Err(Refusal::hard(
                "not_available",
                "個人 Google 帳號需要多人團隊版。",
            ));
        }
        let preliminary = if effective.requesters == Requesters::OwnerOnly {
            Requesters::BoundPeople
        } else {
            effective.requesters
        };
        requester_gate::check(preliminary, who, agent, "", people)?;
        let db = people.user_db().ok_or_else(changed)?;
        let acl_generation = db.acl_generation().map_err(store_error)?;
        let primary_generation = db
            .google_primary_owner_generation(agent)
            .map_err(store_error)?;
        let allow_soft = |error: Refusal| -> Result<String, Refusal> {
            if error.fallback_eligible && effective.fallback == gi::Fallback::Home {
                Ok(error.code.to_owned())
            } else {
                Err(error)
            }
        };
        let mut unavailable = None;
        let user = match super::access::select_user(db, agent, selector) {
            Ok(user) => Some(user),
            Err(error) => {
                unavailable = Some(allow_soft(error)?);
                None
            }
        };
        let (mut grant, mut grant_epoch) = (None, 0);
        if let Some(user) = &user {
            requester_gate::check(effective.requesters, who, agent, &user.email, people)?;
            if let Err(error) = super::access::check_person_metadata(db, agent, user) {
                unavailable = Some(allow_soft(error)?);
            }
            let mut state = vault::authority_state(&home, &user.id).map_err(store_error)?;
            let mut client_validated = false;
            if authorize_host.is_some() && unavailable.is_none() {
                if let Some(metadata) = state
                    .metadata
                    .as_ref()
                    .filter(|m| m.status == vault::Status::Suspended)
                {
                    // Only the already validated active Owner/consent and
                    // actual tool/domain may normalize metadata. No key or
                    // grant lease is acquired, and a failed CAS is final.
                    super::access::check_metadata_authority(&home, metadata, tool)?;
                    check_client_metadata(&home, metadata)?;
                    client_validated = true;
                    state = vault::resume_status_if_current(&home, metadata, state.epoch, || {
                        let authorize = authorize_host.ok_or_else(|| changed().message_zh)?;
                        authorize().map_err(|e| e.message_zh)?;
                        if !crate::team_edition::team_grants_enabled(&home) {
                            return Err(changed().message_zh);
                        }
                        // No re-selection and no vault access in this locked
                        // callback: fixed person plus original generations.
                        if agent_file
                            != file_stamp(&agent_path)
                                .map_err(|e| e.message_zh)?
                                .ok_or_else(|| changed().message_zh)?
                            || home_file
                                != file_stamp(&home.join("config.toml"))
                                    .map_err(|e| e.message_zh)?
                                    .ok_or_else(|| changed().message_zh)?
                            || client_file
                                != file_stamp(&home.join("mcp-oauth-configs.json"))
                                    .map_err(|e| e.message_zh)?
                            || acl_generation
                                != db.acl_generation().map_err(|_| changed().message_zh)?
                            || primary_generation
                                != db
                                    .google_primary_owner_generation(agent)
                                    .map_err(|_| changed().message_zh)?
                            || effective != gi::effective_for(&home, Some(agent))
                        {
                            return Err(changed().message_zh);
                        }
                        requester_gate::check(
                            effective.requesters,
                            who,
                            agent,
                            &user.email,
                            people,
                        )
                        .map_err(|e| e.message_zh)?;
                        super::access::check_person_metadata(db, agent, user)
                            .map_err(|e| e.message_zh)
                    })
                    .map_err(store_error)?
                    .ok_or_else(changed)?;
                }
            }
            grant_epoch = state.epoch;
            grant = state.authority();
            if unavailable.is_none() {
                match state.metadata {
                    None => {
                        unavailable = Some(allow_soft(Refusal::soft(
                            "not_connected",
                            "負責人還沒連結 Google，這次只能依設定改用公司帳號。",
                        ))?)
                    }
                    Some(metadata) => {
                        if let Err(error) = super::access::check_metadata(&home, &metadata, tool) {
                            unavailable = Some(allow_soft(error)?);
                        }
                        if unavailable.is_none() && !client_validated {
                            check_client_metadata(&home, &metadata)?;
                        }
                    }
                }
            }
        }
        if agent_file != file_stamp(&agent_path)?.ok_or_else(changed)?
            || home_file != file_stamp(&home.join("config.toml"))?.ok_or_else(changed)?
            || client_file != file_stamp(&home.join("mcp-oauth-configs.json"))?
            || effective != gi::effective_for(&home, Some(agent))
            || acl_generation != db.acl_generation().map_err(store_error)?
            || primary_generation
                != db
                    .google_primary_owner_generation(agent)
                    .map_err(store_error)?
        {
            return Err(changed());
        }
        if !crate::team_edition::team_grants_enabled(&home) {
            return Err(changed());
        }
        if let Some(authorize_host) = authorize_host {
            authorize_host()?;
        }
        if let Some(user) = &user {
            let live = vault::authority_state(&home, &user.id).map_err(store_error)?;
            if live.epoch != grant_epoch || live.authority() != grant {
                return Err(changed());
            }
        }
        Ok(Self {
            home_digest: format!("{:x}", Sha256::digest(home.as_os_str().as_encoded_bytes())),
            agent: agent.into(),
            tool: tool.into(),
            selector: selector.to_config_value(),
            requesters: effective.requesters.as_str().into(),
            fallback_home: effective.fallback == gi::Fallback::Home,
            agent_file,
            home_file,
            client_file,
            acl_generation,
            primary_generation,
            user_id: user.as_ref().map(|u| u.id.clone()),
            person_email: user.map(|u| u.email),
            grant,
            grant_epoch,
            unavailable,
        })
    }
    pub fn validate(
        &self,
        home: &Path,
        agent: &str,
        who: &RequesterIdentity,
        people: &dyn People,
        tool: &str,
    ) -> Result<(), Refusal> {
        let now = Self::capture(home, agent, who, people, tool).map_err(|_| changed())?;
        if *self == now { Ok(()) } else { Err(changed()) }
    }
    /// Validate the original unavailable fact without selecting a replacement,
    /// normalizing a status or treating a healthy resolver error as fallback.
    pub(crate) fn validate_original_unavailable(
        &self,
        home: &Path,
        agent: &str,
        who: &RequesterIdentity,
        people: &dyn People,
        tool: &str,
    ) -> Result<(), Refusal> {
        if !self.fallback_home || self.unavailable.is_none() {
            return Err(changed());
        }
        self.validate(home, agent, who, people, tool)
    }
    /// Exact retained heap accounting for the metadata stored in a private
    /// pending entry. No token, raw source or mutable authority is returned.
    pub(crate) fn storage_weight(&self) -> Result<usize, Refusal> {
        let mut bytes = std::mem::size_of_val(self);
        let mut add = |n: usize| -> Result<(), Refusal> {
            bytes = bytes.checked_add(n).ok_or_else(changed)?;
            Ok(())
        };
        for value in [
            &self.home_digest,
            &self.agent,
            &self.tool,
            &self.selector,
            &self.requesters,
            &self.agent_file.digest,
            &self.home_file.digest,
        ] {
            add(value.capacity())?;
        }
        if let Some(v) = &self.client_file {
            add(v.digest.capacity())?;
        }
        for v in [&self.user_id, &self.person_email, &self.unavailable]
            .into_iter()
            .flatten()
        {
            add(v.capacity())?;
        }
        if let Some(g) = &self.grant {
            for v in [
                &g.owner_id,
                &g.credential_id,
                &g.account_email,
                &g.google_sub,
                &g.oauth_client_id,
            ] {
                add(v.capacity())?;
            }
            if let Some(v) = &g.hosted_domain {
                add(v.capacity())?;
            }
            add(g
                .granted_scopes
                .capacity()
                .checked_mul(std::mem::size_of::<String>())
                .ok_or_else(changed)?)?;
            for v in &g.granted_scopes {
                add(v.capacity())?;
            }
        }
        Ok(bytes)
    }
    pub(crate) fn fallback_description(&self) -> Result<(String, String), Refusal> {
        if !self.fallback_home {
            return Err(changed());
        }
        let code = self.unavailable.as_ref().ok_or_else(changed)?;
        let kind = if self.selector == "owner" {
            "owner"
        } else if self.selector.starts_with("user:") {
            "user"
        } else {
            return Err(changed());
        };
        Ok((kind.into(), code.clone()))
    }
    pub fn account_email(&self) -> &str {
        self.grant
            .as_ref()
            .map(|g| g.account_email.as_str())
            .unwrap_or("")
    }
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
    pub fn configured_home_fallback(&self) -> bool {
        self.fallback_home
    }
    pub(crate) fn ensure_dedicated_personal(&self) -> Result<(), Refusal> {
        if self.unavailable.is_some()
            || self.user_id.is_none()
            || !self.grant.as_ref().is_some_and(|g| {
                g.status == vault::Status::Connected && Some(g.owner_id.as_str()) == self.user_id()
            })
        {
            return Err(Refusal::hard(
                "personal_home_fallback_unsupported",
                "個人帳號目前無法選用；這條專用連線不支援直接改用公司帳號。請先完成本人同意或重新連結，再重新選擇並核准。",
            ));
        }
        Ok(())
    }
    /// Read exactly one bounded owned snapshot, bound to this selection's
    /// original private stamp. This creates no decrypted client material.
    pub(super) fn pinned_client(
        &self,
        home: &Path,
        record_client_id: &str,
    ) -> Result<PinnedClient, Refusal> {
        let home = home.canonicalize().map_err(store_error)?;
        if self.home_digest != format!("{:x}", Sha256::digest(home.as_os_str().as_encoded_bytes()))
        {
            return Err(changed());
        }
        let grant = self.grant.as_ref().ok_or_else(changed)?;
        if grant.oauth_client_id != record_client_id {
            return Err(changed());
        }
        let expected_stamp = self.client_file.as_ref().ok_or_else(changed)?;
        #[cfg(test)]
        pinned_client_test::run_hook(&home, "before_read");
        #[cfg(test)]
        pinned_client_test::read_attempt(&home);
        let snapshot = file_snapshot(&home.join("mcp-oauth-configs.json"))?.ok_or_else(changed)?;
        if &snapshot.stamp != expected_stamp {
            return Err(changed());
        }
        #[cfg(test)]
        pinned_client_test::run_hook(&home, "after_read");
        // Only metadata/stored ciphertext is parsed here. Never decrypt all
        // providers and never reopen the path to retrieve a later secret.
        let rows: Vec<StoredPinnedClient> =
            serde_json::from_slice(&snapshot.bytes).map_err(store_error)?;
        let mut google = rows.into_iter().filter(|row| row.provider_id == "google");
        let mut selected = google.next().ok_or_else(changed)?;
        if google.next().is_some() || selected.client_id != grant.oauth_client_id {
            return Err(changed());
        }
        Ok(PinnedClient {
            home,
            client_id: std::mem::take(&mut selected.client_id),
            stored_secret: zeroize::Zeroizing::new(std::mem::take(&mut selected.client_secret)),
        })
    }
    /// Pinned effects require a healthy selected grant, never implicit Home.
    pub(crate) fn ensure_healthy_personal(&self) -> Result<(), Refusal> {
        self.ensure_dedicated_personal().map_err(|_| changed())
    }
    pub(crate) fn dedicated_approval(
        &self,
        account_binding_digest: String,
    ) -> Result<gi::GoogleAccountApproval, Refusal> {
        // A Personal approval never includes a different Home account. Pure
        // metadata cannot prove the latter's specific credential/account.
        self.ensure_dedicated_personal()?;
        Ok(gi::GoogleAccountApproval {
            selected_identity: gi::ApprovedGoogleIdentity::Personal,
            selected_google_account: self.account_email().into(),
            selected_credential_owner: self.user_id.clone().ok_or_else(changed)?,
            selected_person_login: self.person_email.clone().ok_or_else(changed)?,
            requester_kind: "unknown".into(),
            configured_home_fallback: self.fallback_home,
            home_fallback_authorized: false,
            home_connection: None,
            home_source: None,
            account_binding_digest,
        })
    }
}

#[derive(Deserialize)]
struct StoredPinnedClient {
    provider_id: String,
    client_id: String,
    client_secret: String,
}
impl Drop for StoredPinnedClient {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.client_secret);
    }
}

/// Owner-only capsules: neither raw file proof nor an unchecked secret getter
/// is available to effect consumers. Decryption consumes the stored capsule.
pub(super) struct PinnedClient {
    home: std::path::PathBuf,
    client_id: String,
    stored_secret: zeroize::Zeroizing<String>,
}
pub(super) struct RefreshClient {
    home: std::path::PathBuf,
    client_id: String,
    client_secret: zeroize::Zeroizing<String>,
}
impl PinnedClient {
    pub(super) fn decrypt(self) -> Result<RefreshClient, Refusal> {
        #[cfg(test)]
        pinned_client_test::run_hook(&self.home, "before_decrypt");
        let client_secret = match self.stored_secret.strip_prefix("enc:v1:") {
            Some(cipher) => {
                // Count the real crypto call, not merely entry to this helper.
                #[cfg(test)]
                let observation =
                    pinned_client_test::begin_decrypt(&self.home, "google", &self.stored_secret);
                let clear = crate::config_crypto::decrypt_value(cipher, &self.home);
                #[cfg(test)]
                pinned_client_test::finish_decrypt(&self.home, observation, clear.as_deref());
                zeroize::Zeroizing::new(clear.ok_or_else(changed)?)
            }
            None if self.stored_secret.starts_with("enc:") => return Err(changed()),
            None => zeroize::Zeroizing::new(self.stored_secret.to_string()),
        };
        Ok(RefreshClient {
            home: self.home,
            client_id: self.client_id,
            client_secret,
        })
    }
}
impl RefreshClient {
    pub(super) async fn refresh(
        &self,
        refresh_token: &str,
    ) -> Result<super::oauth::Tokens, super::oauth::Error> {
        super::oauth::refresh_scoped(&self.client_id, &self.client_secret, refresh_token).await
    }
    pub(super) async fn refresh_mcp(
        &self,
        refresh_token: &str,
        deadline: tokio::time::Instant,
    ) -> Result<super::oauth::Tokens, super::oauth::Error> {
        super::oauth::refresh_mcp(
            &self.home,
            &self.client_id,
            &self.client_secret,
            refresh_token,
            deadline,
        )
        .await
    }
}

#[derive(Debug, Clone)]
pub struct PersonalAuthority {
    selection: PersonalSelection,
    token_revision: u64,
}
impl PersonalAuthority {
    pub fn new(selection: PersonalSelection, token_revision: u64) -> Self {
        Self {
            selection,
            token_revision,
        }
    }
    pub fn selection(&self) -> &PersonalSelection {
        &self.selection
    }
    pub fn validate(
        &self,
        home: &Path,
        agent: &str,
        who: &RequesterIdentity,
        people: &dyn People,
        tool: &str,
    ) -> Result<(), Refusal> {
        self.selection.validate(home, agent, who, people, tool)?;
        let state = vault::authority_state(home, self.selection.user_id().ok_or_else(changed)?)
            .map_err(store_error)?;
        if state.authority() != self.selection.grant
            || state.metadata.as_ref().map(|m| m.revision) != Some(self.token_revision)
        {
            return Err(changed());
        }
        Ok(())
    }
}

/// This field is never accepted from model params. Only the authenticated
/// dispatcher can construct it and place it in a signed internal request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalCallBinding {
    selection: PersonalSelection,
    agent: String,
    credential_ref: CredentialRef,
    tool: String,
    arguments_digest: String,
    created_at: u64,
    expires_at: u64,
}
impl PersonalCallBinding {
    pub fn new(
        selection: PersonalSelection,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        arguments: &serde_json::Value,
        now: u64,
    ) -> Self {
        Self {
            selection,
            agent: agent.into(),
            credential_ref: reference.clone(),
            tool: tool.into(),
            arguments_digest: gi::google_arguments_digest(arguments),
            created_at: now,
            expires_at: now.saturating_add(1800),
        }
    }
    pub fn matches(
        &self,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        args: &serde_json::Value,
        now: u64,
    ) -> bool {
        self.agent == agent
            && self.credential_ref == *reference
            && self.tool == tool
            && self.arguments_digest == gi::google_arguments_digest(args)
            && self.created_at <= now
            && now < self.expires_at
            && self.expires_at.checked_sub(self.created_at) == Some(1800)
    }
    pub fn selection(&self) -> &PersonalSelection {
        &self.selection
    }
    pub(crate) fn agent(&self) -> &str {
        &self.agent
    }
    pub(crate) fn expires_at(&self) -> u64 {
        self.expires_at
    }
    pub(crate) fn original_digest(&self) -> &str {
        &self.arguments_digest
    }
    pub(crate) fn matches_context(
        &self,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        original_digest: &str,
        now: u64,
    ) -> bool {
        self.agent == agent
            && self.credential_ref == *reference
            && self.tool == tool
            && self.arguments_digest == original_digest
            && self.created_at <= now
            && now < self.expires_at
            && self.expires_at.checked_sub(self.created_at) == Some(gi::GOOGLE_SELECTION_TTL_SECS)
    }
    pub(crate) fn validate_context(
        &self,
        home: &Path,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        original_digest: &str,
        now: u64,
        people: &dyn People,
    ) -> Result<(), Refusal> {
        if !self.matches_context(agent, reference, tool, original_digest, now) {
            return Err(changed());
        }
        if !crate::google_workspace::integration_enabled(home) {
            return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
        }
        crate::google_identity::credential_registry::authorize(home, reference, agent, tool)
            .map_err(store_error)?;
        self.selection
            .validate(home, agent, &RequesterIdentity::Unknown, people, tool)
    }
    pub fn validate(
        &self,
        home: &Path,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        args: &serde_json::Value,
        now: u64,
        people: &dyn People,
    ) -> Result<(), Refusal> {
        if !self.matches(agent, reference, tool, args, now) {
            return Err(changed());
        }
        crate::google_identity::credential_registry::authorize(home, reference, agent, tool)
            .map_err(store_error)?;
        self.selection
            .validate(home, agent, &RequesterIdentity::Unknown, people, tool)
    }
}

pub use duduclaw_core::google_identity::GOOGLE_ACCOUNT_APPROVAL_KEY as APPROVAL_ACCOUNT_KEY;
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Dedicated account capture belongs exclusively to the live gateway's
// selection route. A CLI process has no verified edition context and must
// never call a local capture helper or record its own Enterprise edition.

/// Owned-fixture observations at the actual scoped file/crypto boundaries.
/// Absent from production; hooks never replace parsing, stamps or decryption.
#[cfg(test)]
pub(crate) mod pinned_client_test {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    #[derive(Clone, Debug)]
    pub(crate) struct DecryptObservation {
        pub provider: String,
        pub stored_digest: String,
        pub clear_digest: Option<String>,
    }
    #[derive(Default)]
    struct Probe {
        reads: usize,
        decrypts: Vec<DecryptObservation>,
        hooks: HashMap<&'static str, Box<dyn FnOnce() + Send>>,
    }
    fn probes() -> &'static Mutex<HashMap<PathBuf, Probe>> {
        static PROBES: OnceLock<Mutex<HashMap<PathBuf, Probe>>> = OnceLock::new();
        PROBES.get_or_init(Default::default)
    }
    fn home(home: &Path) -> PathBuf {
        home.canonicalize().unwrap()
    }
    pub(crate) fn install(path: &Path) {
        assert!(
            probes()
                .lock()
                .unwrap()
                .insert(home(path), Probe::default())
                .is_none()
        );
    }
    pub(crate) fn remove(path: &Path) {
        probes().lock().unwrap().remove(&home(path));
    }
    pub(crate) fn counts(path: &Path) -> (usize, Vec<DecryptObservation>) {
        let probes = probes().lock().unwrap();
        let p = probes.get(&home(path)).unwrap();
        (p.reads, p.decrypts.clone())
    }
    pub(crate) fn hook(path: &Path, stage: &'static str, callback: Box<dyn FnOnce() + Send>) {
        assert!(["before_read", "after_read", "before_decrypt"].contains(&stage));
        assert!(
            probes()
                .lock()
                .unwrap()
                .get_mut(&home(path))
                .unwrap()
                .hooks
                .insert(stage, callback)
                .is_none()
        );
    }
    pub(super) fn run_hook(path: &Path, stage: &'static str) {
        let callback = probes()
            .lock()
            .unwrap()
            .get_mut(&home(path))
            .and_then(|p| p.hooks.remove(stage));
        if let Some(callback) = callback {
            callback();
        }
    }
    pub(super) fn read_attempt(path: &Path) {
        if let Some(p) = probes().lock().unwrap().get_mut(&home(path)) {
            p.reads += 1;
        }
    }
    pub(super) fn begin_decrypt(path: &Path, provider: &str, stored: &str) -> Option<usize> {
        let mut probes = probes().lock().unwrap();
        let p = probes.get_mut(&home(path))?;
        let index = p.decrypts.len();
        p.decrypts.push(DecryptObservation {
            provider: provider.into(),
            stored_digest: format!("{:x}", Sha256::digest(stored.as_bytes())),
            clear_digest: None,
        });
        Some(index)
    }
    pub(super) fn finish_decrypt(path: &Path, index: Option<usize>, clear: Option<&str>) {
        if let Some(index) = index {
            if let Some(p) = probes().lock().unwrap().get_mut(&home(path)) {
                p.decrypts[index].clear_digest =
                    clear.map(|s| format!("{:x}", Sha256::digest(s.as_bytes())));
            }
        }
    }
}
