//! Per-call person gates and serialized token refresh. A rejected caller
//! never reaches credentials or a fallback account.

#[path = "access/authorized_io.rs"]
mod authorized_io;
#[path = "access/authorized_mcp.rs"]
mod authorized_mcp;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use duduclaw_auth::{AccessLevel, User, UserDb, UserStatus};
use duduclaw_core::google_identity::{self as gi, Effective, EffectiveIdentity, Requesters, Selector};
use duduclaw_knowledge::requester::RequesterIdentity;

use super::{oauth, scopes};
use crate::google_identity::resolve::Resolved;
use crate::google_identity::{
    Refusal,
    requester_gate::{self, People},
};
use crate::person_credentials::{self as vault, Metadata, Status};

fn store_error(_: impl std::fmt::Display) -> Refusal {
    Refusal::hard(
        "config_invalid",
        "無法確認個人連線或人員資料，請管理員檢查，這次沒有使用任何 Google 帳號。",
    )
}

pub fn select_user(db: &UserDb, agent: &str, selector: &Selector) -> Result<User, Refusal> {
    match selector {
        Selector::Owner => db.google_owner(agent).map_err(|e| match e {
            duduclaw_auth::google_identity::OwnerError::Missing => Refusal::soft(
                "no_owner",
                "這位 AI 員工還沒有負責人，請管理員綁定一位擁有者。",
            ),
            duduclaw_auth::google_identity::OwnerError::Ambiguous => Refusal::soft(
                "ambiguous_owner",
                "這位 AI 員工有多位負責人，請管理員在「連線」分頁指定主要負責人。",
            ),
            other => store_error(format!("{other:?}")),
        }),
        Selector::User(id) => {
            if db.check_agent_access(id, agent).map_err(store_error)? != Some(AccessLevel::Owner) {
                return Err(Refusal::hard(
                    "config_invalid",
                    "指定的同事必須是這位 AI 員工的擁有者，請管理員修正。",
                ));
            }
            db.get_user(id)
                .map_err(store_error)?
                .ok_or_else(|| Refusal::hard("config_invalid", "指定的同事不存在，請管理員修正。"))
        }
        _ => Err(Refusal::hard(
            "config_invalid",
            "個人 Google 身分設定不正確。",
        )),
    }
}

fn check_person(home: &Path, db: &UserDb, agent: &str, user: &User) -> Result<(), Refusal> {
    let outcome = check_person_metadata(db, agent, user);
    if outcome
        .as_ref()
        .is_err_and(|error| error.code == "person_inactive")
    {
        vault::sync_user_status(home, &user.id, false).map_err(store_error)?;
    }
    outcome
}
pub(super) fn check_person_metadata(db: &UserDb, agent: &str, user: &User) -> Result<(), Refusal> {
    let live = db
        .get_user(&user.id)
        .map_err(store_error)?
        .ok_or_else(|| Refusal::soft("person_inactive", "負責人的帳號已移除。"))?;
    if live.status != UserStatus::Active {
        return Err(Refusal::soft(
            "person_inactive",
            "負責人的帳號已停權或離職，不能使用他的 Google 帳號。",
        ));
    }
    if db
        .check_agent_access(&user.id, agent)
        .map_err(store_error)?
        != Some(AccessLevel::Owner)
    {
        return Err(Refusal::hard(
            "config_invalid",
            "這位同事已不再是員工的擁有者，請管理員修正連線設定。",
        ));
    }
    match db.google_consent(&user.id, agent).map_err(store_error)? {
        Some(true) => Ok(()),
        Some(false) => Err(Refusal::hard(
            "consent_declined",
            "帳號本人已關閉這位員工的使用權，只有本人可以重新開放。",
        )),
        None => Err(Refusal::soft(
            "awaiting_consent",
            "正在等帳號本人同意。請本人到「我的連線」允許這位 AI 員工使用。",
        )),
    }
}

pub fn domain_allowed(home: &Path, metadata: &Metadata) -> Result<bool, String> {
    let config = gi::read_home_config(home)?;
    if config.allowed_domains.is_empty() {
        return Ok(true);
    }
    let domain = metadata
        .hosted_domain
        .as_deref()
        .or_else(|| gi::email_domain(&metadata.account_email));
    Ok(domain.is_some_and(|domain| {
        config
            .allowed_domains
            .iter()
            .any(|allowed| allowed == domain)
    }))
}

fn check_record(home: &Path, record: &vault::Record, tool: &str) -> Result<(), Refusal> {
    check_metadata(home, &record.metadata, tool)
}
pub(super) fn check_metadata(home: &Path, metadata: &Metadata, tool: &str) -> Result<(), Refusal> {
    if metadata.status != Status::Connected {
        return Err(Refusal::soft(
            "needs_reconnect",
            "你的 Google 連線需要重新連結，請到「我的連線」處理。",
        ));
    }
    check_metadata_authority(home, metadata, tool)
}
pub(super) fn check_metadata_authority(
    home: &Path,
    metadata: &Metadata,
    tool: &str,
) -> Result<(), Refusal> {
    if !domain_allowed(home, metadata).map_err(store_error)? {
        return Err(Refusal::hard(
            "config_invalid",
            "這個 Google 帳號不在管理員允許的網域內。",
        ));
    }
    if !scopes::permitted(tool, &metadata.granted_scopes) {
        return Err(Refusal::hard(
            "scope_missing",
            "你連結 Google 時沒有開放這項服務；請重新連結，並勾選這次需要的服務。",
        ));
    }
    Ok(())
}

fn refresh_lock(home: &Path, user: &str) -> Arc<tokio::sync::Mutex<()>> {
    type Key = (PathBuf, String);
    static LOCKS: OnceLock<Mutex<HashMap<Key, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry((home.to_path_buf(), user.into()))
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub async fn resolve(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    effective: &EffectiveIdentity,
    tool: &str,
) -> Result<Resolved, Refusal> {
    if !crate::team_edition::team_grants_enabled(home) {
        return Err(Refusal::hard(
            "not_available",
            "「跟著負責人」與「指定同事」需要多人團隊版；請管理員調整版本或改用公司共用帳號。",
        ));
    }
    let Effective::Personal { selector } = &effective.effective else {
        return Err(store_error("selector"));
    };
    // Even a missing/ambiguous Owner cannot bypass the requester gate via
    // fallback. Owner-only is finalized against the selected person below.
    let preliminary = if effective.requesters == Requesters::OwnerOnly {
        Requesters::BoundPeople
    } else {
        effective.requesters
    };
    requester_gate::check(preliminary, requester, agent, "", people)?;
    let db = people
        .user_db()
        .ok_or_else(|| store_error("directory unavailable"))?;
    let user = match select_user(db, agent, selector) {
        Err(_)
            if effective.requesters == Requesters::OwnerOnly
                && !matches!(
                    requester,
                    RequesterIdentity::System {
                        on_behalf_of: None,
                        ..
                    }
                ) =>
        {
            return Err(Refusal::hard(
                "requester_refused",
                "無法確認這次發問者就是帳號本人，已拒絕使用 Google。",
            ));
        }
        other => other?,
    };
    // Use the immutable DuDuClaw person, not their possibly different
    // Google email, to implement owner_only.
    requester_gate::check(effective.requesters, requester, agent, &user.email, people)?;
    check_person(home, db, agent, &user)?;
    // A legitimately resumed person is normalized before fixing authority.
    // Keep this before the first await so later ABA cannot be recaptured.
    vault::sync_user_status(home, &user.id, true).map_err(store_error)?;
    let selected =
        super::authority::PersonalSelection::capture(home, agent, requester, people, tool)?;
    let lock = refresh_lock(home, &user.id);
    let _guard = lock.lock().await;
    check_person(home, db, agent, &user)?;
    vault::sync_user_status(home, &user.id, true).map_err(store_error)?;
    selected.validate(home, agent, requester, people, tool)?;
    let mut record = vault::get(home, &user.id)
        .map_err(store_error)?
        .ok_or_else(|| {
            Refusal::soft(
                "not_connected",
                "負責人還沒連結 Google，請本人到「我的連線」按「連結我的 Google」。",
            )
        })?;
    check_record(home, &record, tool)?;
    let config = crate::mcp_oauth::get_client_config(home, "google").ok_or_else(|| {
        Refusal::hard(
            "config_invalid",
            "管理員還沒設定 Google 連結，請聯絡管理員。",
        )
    })?;
    if config.client_id != record.metadata.oauth_client_id {
        return Err(Refusal::hard(
            "config_invalid",
            "管理員變更了 Google 連結設定，請本人重新連結。",
        ));
    }
    let key = crate::google_identity::protection_gate_async(home).await?;
    let mut secret = vault::unseal(&key, &record).map_err(|_| {
        Refusal::hard(
            "credential_protection",
            "個人連線憑證無法解開，請管理員檢查憑證保護後重新連結。",
        )
    })?;
    if secret
        .expires_at
        .is_none_or(|expiry| expiry <= chrono::Utc::now().timestamp() + 120)
    {
        let _grant_lease = super::grants::acquire(home).await.map_err(store_error)?;
        let Some(refresh_token) = secret.refresh_token.as_deref() else {
            return Err(Refusal::soft(
                "needs_reconnect",
                "Google 沒有提供背景更新連線，請本人重新連結並同意背景存取。",
            ));
        };
        match oauth::refresh(&config.client_id, &config.client_secret, refresh_token).await {
            Ok(mut tokens) => {
                if tokens.secret.refresh_token.is_none() {
                    tokens.secret.refresh_token = secret.refresh_token.take();
                }
                let current_scopes = tokens.granted_scopes.as_deref();
                if !vault::finish_refresh_with_scopes(
                    home,
                    &key,
                    &record.metadata,
                    &tokens.secret,
                    current_scopes,
                )
                .map_err(store_error)?
                {
                    return Err(Refusal::hard(
                        "config_invalid",
                        "連線已被另一個操作變更，請重試。",
                    ));
                }
                secret = tokens.secret;
                record = vault::get(home, &user.id)
                    .map_err(store_error)?
                    .ok_or_else(|| store_error("connection removed"))?;
                check_record(home, &record, tool)?;
            }
            Err(oauth::Error::InvalidGrant) => {
                if vault::mark_needs_reconnect(home, &record.metadata).map_err(store_error)? {
                    let _ = db.log_action(
                        Some(&user.id),
                        "user.google_needs_reconnect",
                        Some(agent),
                        None,
                        None,
                    );
                    let notify_home = home.to_path_buf();
                    let notify_user = user.id.clone();
                    tokio::spawn(async move {
                        crate::notify_push::push_person_notice(&notify_home, &notify_user, "你的 Google 連線已失效，AI 員工暫時不能使用。請到 DuDuClaw「我的連線」重新連結。").await;
                    });
                }
                return Err(Refusal::soft(
                    "needs_reconnect",
                    "你的 Google 連線已失效，請本人到「我的連線」重新連結。",
                ));
            }
            Err(_) => {
                return Err(Refusal::soft(
                    "google_unreachable",
                    "暫時無法更新 Google 連線，請稍後重試。",
                ));
            }
        }
    }
    check_person(home, db, agent, &user)?;
    let current_user = select_user(db, agent, selector)?;
    if current_user.id != user.id || gi::effective_for(home, Some(agent)) != *effective {
        return Err(Refusal::hard(
            "config_invalid",
            "員工的 Google 身分已變更，請重試。",
        ));
    }
    requester_gate::check(
        effective.requesters,
        requester,
        agent,
        &current_user.email,
        people,
    )?;
    let current = vault::get(home, &user.id)
        .map_err(store_error)?
        .ok_or_else(|| store_error("connection removed"))?;
    if current.metadata.credential_id != record.metadata.credential_id
        || current.metadata.revision != record.metadata.revision
    {
        return Err(Refusal::hard(
            "config_invalid",
            "連線已被另一個操作變更，請重試。",
        ));
    }
    selected.validate(home, agent, requester, people, tool)?;
    let authority = super::authority::PersonalAuthority::new(selected, record.metadata.revision);
    let expires_in = secret
        .expires_at
        .unwrap_or(0)
        .saturating_sub(chrono::Utc::now().timestamp())
        .max(0) as u64;
    Ok(Resolved::Personal {
        kind: selector.kind(),
        user_id: user.id,
        email: record.metadata.account_email,
        authority,
        token: crate::google_service_account::MintedToken {
            access_token: secret.access_token.clone(),
            expires_in,
        },
    })
}

/// Fresh material from one already-selected grant and one exact token revision.
/// There is deliberately no bearer getter, serialization or Clone implementation.
pub(crate) struct PinnedPersonalToken {
    authority: super::authority::PersonalAuthority,
    _access_token: zeroize::Zeroizing<String>,
    expires_at: i64,
}
impl PinnedPersonalToken {
    pub(crate) fn matches_selection(&self, selected: &super::authority::PersonalSelection) -> bool {
        self.authority.selection() == selected
    }
    #[cfg(test)]
    pub(crate) fn fixture_token_matches(&self, expected: &str) -> bool {
        self._access_token.as_str() == expected
    }
    pub(crate) fn validate(
        &self,
        home: &Path,
        agent: &str,
        requester: &RequesterIdentity,
        people: &dyn People,
        purpose_key: &str,
    ) -> Result<(), Refusal> {
        super::grants::check_metadata_barrier(home)
            .map_err(|m| Refusal::hard("grant_uncertain", m))?;
        self.authority.selection().ensure_healthy_personal()?;
        self.authority
            .validate(home, agent, requester, people, purpose_key)?;
        if self.expires_at <= chrono::Utc::now().timestamp() {
            return Err(pinned_changed());
        }
        Ok(())
    }
}

fn pinned_changed() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "Google 帳號或連線版本已變更，本次操作已停止。請重新選擇帳號並核准。",
    )
}
fn validate_pinned_selection(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    selected: &super::authority::PersonalSelection,
    purpose_key: &str,
) -> Result<(), Refusal> {
    super::grants::check_metadata_barrier(home).map_err(|m| Refusal::hard("grant_uncertain", m))?;
    if !crate::google_workspace::integration_enabled(home) {
        return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
    }
    selected.ensure_healthy_personal()?;
    selected.validate(home, agent, requester, people, purpose_key)
}

/// Resolve only the captured person. This path never reselects an owner,
/// normalizes status, retries a provider exchange, or returns UseHome.
pub(crate) async fn resolve_pinned(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    selected: &super::authority::PersonalSelection,
    purpose_key: &str,
) -> Result<PinnedPersonalToken, Refusal> {
    resolve_pinned_core(
        home,
        agent,
        requester,
        people,
        selected,
        purpose_key,
        PinnedReadMode::LegacyAsync,
    )
    .await
}
/// M1-only entry. The caller cannot select a read strategy or receive a key.
pub(crate) async fn resolve_pinned_mcp(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    selected: &super::authority::PersonalSelection,
    deadline: tokio::time::Instant,
) -> Result<PinnedPersonalToken, Refusal> {
    resolve_pinned_core(
        home,
        agent,
        requester,
        people,
        selected,
        "google_remote_mcp",
        PinnedReadMode::Remote(deadline),
    )
    .await
}
#[derive(Clone, Copy)]
enum PinnedReadMode {
    LegacyAsync,
    Remote(tokio::time::Instant),
}
impl PinnedReadMode {
    fn check(self) -> Result<(), Refusal> {
        if let Self::Remote(deadline) = self {
            if tokio::time::Instant::now() >= deadline {
                return Err(pinned_changed());
            }
        }
        Ok(())
    }
    async fn wait<F: std::future::Future>(self, future: F) -> Result<F::Output, Refusal> {
        self.check()?;
        let result = match self {
            Self::LegacyAsync => future.await,
            Self::Remote(deadline) => tokio::time::timeout_at(deadline, future)
                .await
                .map_err(|_| pinned_changed())?,
        };
        self.check()?;
        Ok(result)
    }
}
async fn resolve_pinned_core(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    selected: &super::authority::PersonalSelection,
    purpose_key: &str,
    read_mode: PinnedReadMode,
) -> Result<PinnedPersonalToken, Refusal> {
    read_mode.check()?;
    validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
    let db = people.user_db().ok_or_else(pinned_changed)?;
    let user_id = selected.user_id().ok_or_else(pinned_changed)?;
    let user = db
        .get_user(user_id)
        .map_err(store_error)?
        .ok_or_else(pinned_changed)?;
    check_person_metadata(db, agent, &user)?;
    let lock = refresh_lock(home, user_id);
    let guard = read_mode.wait(lock.lock()).await?;
    read_mode.check()?;
    validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
    // The id comes from the immutable selection, never from today's Owner.
    let user = db
        .get_user(user_id)
        .map_err(store_error)?
        .ok_or_else(pinned_changed)?;
    check_person_metadata(db, agent, &user)?;
    let record = vault::get(home, user_id)
        .map_err(store_error)?
        .ok_or_else(pinned_changed)?;
    check_record(home, &record, purpose_key)?;
    let prior_revision = record.metadata.revision;
    let before = super::authority::PersonalAuthority::new(selected.clone(), prior_revision);
    before.validate(home, agent, requester, people, purpose_key)?;
    let client = selected.pinned_client(home, &record.metadata.oauth_client_id)?;
    read_mode.check()?;
    validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
    #[cfg(test)]
    pinned_test::observe(home, "protection");
    let key = match read_mode {
        PinnedReadMode::LegacyAsync => crate::google_identity::protection_gate_async(home).await?,
        PinnedReadMode::Remote(_) => {
            read_mode.check()?;
            let key = crate::google_identity::protection_gate(home);
            read_mode.check()?;
            key?
        }
    };
    read_mode.check()?;
    validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
    before.validate(home, agent, requester, people, purpose_key)?;
    #[cfg(test)]
    pinned_test::observe(home, "unseal");
    let mut secret = vault::unseal(&key, &record).map_err(|_| {
        Refusal::hard(
            "credential_protection",
            "個人連線憑證無法解開，請管理員檢查憑證保護後重新連結。",
        )
    })?;
    let mut token_revision = prior_revision;
    if secret
        .expires_at
        .is_none_or(|expiry| expiry <= chrono::Utc::now().timestamp().saturating_add(120))
    {
        let published_revision = prior_revision.checked_add(1).ok_or_else(pinned_changed)?;
        let grant = read_mode
            .wait(super::grants::acquire(home))
            .await?
            .map_err(|m| Refusal::hard("grant_uncertain", m))?;
        read_mode.check()?;
        validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
        before.validate(home, agent, requester, people, purpose_key)?;
        #[cfg(test)]
        pinned_test::observe(home, "refresh_grant");
        let refresh_token = secret.refresh_token.as_deref().ok_or_else(|| {
            Refusal::hard(
                "needs_reconnect",
                "Google 沒有提供背景更新連線，請本人重新連結並同意背景存取。",
            )
        })?;
        let client = client.decrypt()?;
        // The capsule proves which bytes were decrypted. Live gates still
        // decide whether this operation may send after the decrypt boundary.
        read_mode.check()?;
        validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
        before.validate(home, agent, requester, people, purpose_key)?;
        #[cfg(test)]
        pinned_test::observe(home, "provider");
        #[cfg(test)]
        let fixture = pinned_test::take_refresh(home);
        #[cfg(not(test))]
        let fixture: Option<Result<oauth::Tokens, oauth::Error>> = None;
        let result = match fixture {
            Some(result) => result,
            None => match read_mode {
                PinnedReadMode::LegacyAsync => client.refresh(refresh_token).await,
                PinnedReadMode::Remote(deadline) => {
                    read_mode
                        .wait(client.refresh_mcp(refresh_token, deadline))
                        .await?
                }
            },
        };
        if matches!(read_mode, PinnedReadMode::Remote(_)) {
            read_mode.check()?;
            validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
            before.validate(home, agent, requester, people, purpose_key)?;
        }
        let mut tokens = result.map_err(|error| match error {
            oauth::Error::InvalidGrant => Refusal::hard(
                "needs_reconnect",
                "你的 Google 連線已失效，請本人到「我的連線」重新連結。",
            ),
            _ => Refusal::hard(
                "google_unreachable",
                "暫時無法更新 Google 連線，這次操作已停止。",
            ),
        })?;
        // A provider response is not permission to publish after a gate or
        // selection change. The store's own CAS is checked independently.
        read_mode.check()?;
        validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
        before.validate(home, agent, requester, people, purpose_key)?;
        if tokens.secret.refresh_token.is_none() {
            tokens.secret.refresh_token = secret.refresh_token.take();
        }
        #[cfg(test)]
        pinned_test::run_before_publish(home);
        read_mode.check()?;
        if !vault::finish_refresh_with_scopes(
            home,
            &key,
            &record.metadata,
            &tokens.secret,
            tokens.granted_scopes.as_deref(),
        )
        .map_err(store_error)?
        {
            return Err(pinned_changed());
        }
        #[cfg(test)]
        pinned_test::observe(home, "refresh_published");
        token_revision = published_revision;
        secret = tokens.secret;
        // Exact +1 is essential. A subsequent reload at +2 must never be
        // attached to the older response secret held in this call.
        let published =
            super::authority::PersonalAuthority::new(selected.clone(), published_revision);
        read_mode.check()?;
        validate_pinned_selection(home, agent, requester, people, selected, purpose_key)?;
        published.validate(home, agent, requester, people, purpose_key)?;
        drop(grant);
    }
    let expires_at = secret
        .expires_at
        .filter(|expiry| *expiry > chrono::Utc::now().timestamp().saturating_add(120))
        .ok_or_else(pinned_changed)?;
    let material = PinnedPersonalToken {
        authority: super::authority::PersonalAuthority::new(selected.clone(), token_revision),
        _access_token: zeroize::Zeroizing::new(std::mem::take(&mut secret.access_token)),
        expires_at,
    };
    read_mode.check()?;
    material.validate(home, agent, requester, people, purpose_key)?;
    read_mode.check()?;
    drop(guard);
    #[cfg(test)]
    pinned_test::observe(home, "resolve_locks_released");
    Ok(material)
}

/// Home-keyed owned-fixture injection, absent from production. Hooks run at
/// real authority boundaries and never replace the gates, CAS or lease.
#[cfg(test)]
pub(crate) mod pinned_test {
    use super::*;
    #[derive(Default)]
    struct Probe {
        events: Vec<&'static str>,
        refresh: Option<Result<oauth::Tokens, oauth::Error>>,
        after_publish: Option<Box<dyn FnOnce() + Send>>,
        before_publish: Option<Box<dyn FnOnce() + Send>>,
    }
    fn probes() -> &'static Mutex<HashMap<PathBuf, Probe>> {
        static PROBES: OnceLock<Mutex<HashMap<PathBuf, Probe>>> = OnceLock::new();
        PROBES.get_or_init(Default::default)
    }
    fn path(home: &Path) -> PathBuf {
        home.canonicalize().unwrap()
    }
    pub(crate) fn install(home: &Path) {
        assert!(
            probes()
                .lock()
                .unwrap()
                .insert(path(home), Probe::default())
                .is_none()
        );
    }
    pub(crate) fn remove(home: &Path) {
        probes().lock().unwrap().remove(&path(home));
    }
    pub(crate) fn events(home: &Path) -> Vec<&'static str> {
        probes()
            .lock()
            .unwrap()
            .get(&path(home))
            .unwrap()
            .events
            .clone()
    }
    pub(crate) fn refresh_result(home: &Path, result: Result<oauth::Tokens, oauth::Error>) {
        probes()
            .lock()
            .unwrap()
            .get_mut(&path(home))
            .unwrap()
            .refresh = Some(result);
    }
    pub(crate) fn after_publish(home: &Path, callback: Box<dyn FnOnce() + Send>) {
        probes()
            .lock()
            .unwrap()
            .get_mut(&path(home))
            .unwrap()
            .after_publish = Some(callback);
    }
    pub(crate) async fn hold_refresh(home: &Path, user: &str) -> tokio::sync::OwnedMutexGuard<()> {
        refresh_lock(home, user).lock_owned().await
    }
    pub(crate) fn set_before_publish(home: &Path, callback: Box<dyn FnOnce() + Send>) {
        probes()
            .lock()
            .unwrap()
            .get_mut(&path(home))
            .unwrap()
            .before_publish = Some(callback);
    }
    pub(super) fn run_before_publish(home: &Path) {
        let callback = probes()
            .lock()
            .unwrap()
            .get_mut(&path(home))
            .and_then(|p| p.before_publish.take());
        if let Some(callback) = callback {
            callback();
        }
    }
    pub(super) fn take_refresh(home: &Path) -> Option<Result<oauth::Tokens, oauth::Error>> {
        probes()
            .lock()
            .unwrap()
            .get_mut(&path(home))
            .and_then(|p| p.refresh.take())
    }
    pub(super) fn observe(home: &Path, event: &'static str) {
        let callback = {
            let mut probes = probes().lock().unwrap();
            let Some(probe) = probes.get_mut(&path(home)) else {
                return;
            };
            probe.events.push(event);
            if event == "refresh_published" {
                probe.after_publish.take()
            } else {
                None
            }
        };
        if let Some(callback) = callback {
            callback();
        }
    }
}
