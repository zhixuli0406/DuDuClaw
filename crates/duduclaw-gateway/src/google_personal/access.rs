//! Per-call person gates and serialized token refresh. A rejected caller
//! never reaches credentials or a fallback account.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use duduclaw_auth::{AccessLevel, User, UserDb, UserStatus};
use duduclaw_core::google_identity::{
    self as gi, Effective, EffectiveIdentity, Requesters, Selector,
};
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
    let live = db
        .get_user(&user.id)
        .map_err(store_error)?
        .ok_or_else(|| Refusal::soft("person_inactive", "負責人的帳號已移除。"))?;
    if live.status != UserStatus::Active {
        vault::sync_user_status(home, &user.id, false).map_err(store_error)?;
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
    if record.metadata.status != Status::Connected {
        return Err(Refusal::soft(
            "needs_reconnect",
            "你的 Google 連線需要重新連結，請到「我的連線」處理。",
        ));
    }
    if !domain_allowed(home, &record.metadata).map_err(store_error)? {
        return Err(Refusal::hard(
            "config_invalid",
            "這個 Google 帳號不在管理員允許的網域內。",
        ));
    }
    if !scopes::permitted(tool, &record.metadata.granted_scopes) {
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
    let lock = refresh_lock(home, &user.id);
    let _guard = lock.lock().await;
    check_person(home, db, agent, &user)?;
    vault::sync_user_status(home, &user.id, true).map_err(store_error)?;
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
    let expires_in = secret
        .expires_at
        .unwrap_or(0)
        .saturating_sub(chrono::Utc::now().timestamp())
        .max(0) as u64;
    Ok(Resolved::Personal {
        kind: selector.kind(),
        user_id: user.id,
        email: record.metadata.account_email,
        token: crate::google_service_account::MintedToken {
            access_token: secret.access_token.clone(),
            expires_in,
        },
    })
}
