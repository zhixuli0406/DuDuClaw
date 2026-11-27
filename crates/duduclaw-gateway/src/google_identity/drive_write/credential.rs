//! Gateway-only credential selection. A refusal never changes identity routes.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use duduclaw_core::google_identity::{self as gi, Effective};
use duduclaw_knowledge::requester::{is_public_visitor_channel, RequesterIdentity};
use zeroize::Zeroizing;

use crate::google_identity::{delegate_owner, home_source, resolve, Refusal};
use crate::google_personal::{grants, oauth};
use crate::google_service_account::CacheMode;
use crate::person_credentials::{self as vault, write_store, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::google_identity) enum IdentityRoute {
    Personal { owner_user_id: String, account_email: String },
    Delegate { email: String },
    DelegateOwner { owner_user_id: String, email: String },
    HomeServiceAccount,
    HomeOAuth,
    Unsupported,
}

fn hard(error: Refusal) -> Refusal {
    Refusal { fallback_eligible: false, ..error }
}
fn changed() -> Refusal {
    Refusal::hard("changed", "Google 連線已變更，這次沒有執行，請重新操作。")
}
fn unsupported() -> Refusal {
    Refusal::hard("drive_write_unsupported_identity", "這種 Google 連線方式不支援雲端硬碟寫入。")
}
fn consent(route: &IdentityRoute) -> Refusal {
    match route {
        IdentityRoute::HomeOAuth => Refusal::hard("drive_write_needs_home_consent", "公司 Google 帳號需要重新連結雲端硬碟寫入，請聯絡管理員。"),
        _ => Refusal::hard("drive_write_needs_member_consent", "帳號本人需要到「我的連線」重新連結雲端硬碟寫入。"),
    }
}

/// The caller retains and revalidates this route in its execution snapshot.
/// No tool argument can select an owner, address or credential namespace.
pub(in crate::google_identity) fn resolve_route(home: &Path, agent: &str) -> Result<IdentityRoute, Refusal> {
    if !duduclaw_core::is_valid_agent_id(agent) { return Err(changed()); }
    // This executor does not carry protected-mode dedicated authority.
    // Never mint through an ordinary adapter on that deployment path.
    if crate::runtime_readiness::is_protected_linux() {
        return Ok(IdentityRoute::Unsupported);
    }
    match gi::effective_for(home, Some(agent)).effective {
        Effective::Personal { selector } => {
            let db = crate::decision_notify::open_user_db_read_only(home)
                .map_err(|_| changed())?.ok_or_else(changed)?;
            let user = crate::google_personal::access::select_user(&db, agent, &selector).map_err(hard)?;
            if user.status != duduclaw_auth::UserStatus::Active {
                return Err(Refusal::hard("person_inactive", "帳號本人目前無法使用 Google 連線。"));
            }
            // The login address and the linked Google account may differ.
            let account_email = vault::get(home, &user.id).map_err(|_| changed())?
                .map(|r| r.metadata.account_email).unwrap_or_default();
            Ok(IdentityRoute::Personal { owner_user_id: user.id, account_email })
        }
        Effective::Delegate { email } => Ok(IdentityRoute::Delegate { email }),
        Effective::DelegateOwner => {
            let db = crate::decision_notify::open_user_db_read_only(home)
                .map_err(|_| changed())?.ok_or_else(changed)?;
            let owner = delegate_owner::owner(&db, agent).map_err(hard)?;
            let email = delegate_owner::subject(home, agent, &db).map_err(hard)?;
            if delegate_owner::owner(&db, agent).map_err(hard)?.id != owner.id { return Err(changed()); }
            Ok(IdentityRoute::DelegateOwner { owner_user_id: owner.id, email })
        }
        Effective::Home => Ok(match crate::google_workspace::selected_path(home).ok().as_deref() {
            Some("service_account") => IdentityRoute::HomeServiceAccount,
            Some("oauth") => IdentityRoute::HomeOAuth,
            _ => IdentityRoute::Unsupported,
        }),
        _ => Ok(IdentityRoute::Unsupported),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::google_identity) enum Readiness {
    Ready,
    NeedsMemberConsent { user_id: String },
    /// Discovered at mint time, never guessed from local delegation settings.
    NeedsAdminScope,
    NeedsHomeConsent,
    Unsupported,
}

fn owner_key(route: &IdentityRoute) -> Option<(&str, &str)> {
    match route {
        IdentityRoute::Personal { owner_user_id, .. } => Some(("user", owner_user_id)),
        IdentityRoute::HomeOAuth => Some(("home", "home")),
        _ => None,
    }
}

fn ready_record(home: &Path, route: &IdentityRoute) -> Result<write_store::Record, Refusal> {
    let (kind, id) = owner_key(route).ok_or_else(unsupported)?;
    let record = write_store::get(home, kind, id).map_err(|_| consent(route))?
        .ok_or_else(|| consent(route))?;
    let m = &record.metadata;
    if m.status != Status::Connected || m.purpose != "drive_write"
        || !m.granted_scopes.iter().any(|s| s == oauth::DRIVE_WRITE_SCOPE) {
        return Err(consent(route));
    }
    if let IdentityRoute::Personal { owner_user_id, account_email } = route {
        let ordinary = vault::get(home, owner_user_id).map_err(|_| consent(route))?
            .ok_or_else(|| consent(route))?;
        if ordinary.metadata.status != Status::Connected
            || ordinary.metadata.google_sub != m.google_sub
            || ordinary.metadata.account_email != *account_email
            || m.account_email != *account_email {
            return Err(consent(route));
        }
    }
    Ok(record)
}

pub(in crate::google_identity) fn readiness(home: &Path, route: &IdentityRoute) -> Readiness {
    match route {
        IdentityRoute::Personal { owner_user_id, .. } if ready_record(home, route).is_err() =>
            Readiness::NeedsMemberConsent { user_id: owner_user_id.clone() },
        IdentityRoute::HomeOAuth if ready_record(home, route).is_err() => Readiness::NeedsHomeConsent,
        IdentityRoute::Unsupported => Readiness::Unsupported,
        _ => Readiness::Ready,
    }
}

/// Safe metadata for audit and CAS invalidation; never includes secret material.
#[derive(Clone)]
pub(in crate::google_identity) struct TokenMeta {
    pub identity_kind: &'static str,
    pub account_email: String,
    pub credential_revision: Option<u64>,
    write_record: Option<write_store::Metadata>,
}

/// Deliberately not Debug/Serialize. The bearer stays in gateway memory.
pub(in crate::google_identity) struct WriteToken {
    pub bearer: Zeroizing<String>,
    pub meta: TokenMeta,
}

pub(in crate::google_identity) async fn write_token(home: &Path, route: &IdentityRoute) -> Result<WriteToken, Refusal> {
    match route {
        IdentityRoute::Personal { .. } | IdentityRoute::HomeOAuth => oauth_token(home, route).await,
        IdentityRoute::Delegate { email } | IdentityRoute::DelegateOwner { email, .. } => {
            let token = resolve::mint_delegate_for_drive_write(home, email, CacheMode::Use).await.map_err(hard)?;
            Ok(WriteToken {
                bearer: Zeroizing::new(token.access_token),
                meta: TokenMeta {
                    identity_kind: if matches!(route, IdentityRoute::DelegateOwner { .. }) { "delegate_owner" } else { "delegate" },
                    account_email: email.clone(), credential_revision: None, write_record: None,
                },
            })
        }
        IdentityRoute::HomeServiceAccount => {
            let email = crate::google_identity::home_subject(home).ok_or_else(changed)?;
            let token = home_source::home_sa_token_for_drive_write(home).await.map_err(hard)?;
            let bearer = Zeroizing::new(token.access_token);
            if crate::google_identity::home_subject(home).as_deref() != Some(email.as_str()) { return Err(changed()); }
            Ok(WriteToken { bearer, meta: TokenMeta {
                identity_kind: "home_sa", account_email: email, credential_revision: None, write_record: None,
            } })
        }
        IdentityRoute::Unsupported => Err(unsupported()),
    }
}

async fn oauth_token(home: &Path, route: &IdentityRoute) -> Result<WriteToken, Refusal> {
    let _lease = grants::acquire(home).await.map_err(|_| changed())?;
    let record = ready_record(home, route)?;
    let mut metadata = record.metadata.clone();
    let key = crate::google_identity::protection_gate_async(home).await.map_err(hard)?;
    let mut secret = write_store::unseal(&key, &record).map_err(|_| consent(route))?;
    // Validate the configured client even for a fresh token. Client changes
    // cannot silently continue using a previous OAuth application.
    let client_home = home.to_path_buf();
    let client = tokio::task::spawn_blocking(move || crate::mcp_oauth::get_client_config(&client_home, "google"))
        .await.map_err(|_| changed())?.ok_or_else(changed)?;
    let client_secret = Zeroizing::new(client.client_secret);
    if client.client_id != metadata.oauth_client_id { return Err(changed()); }
    if !secret.expires_at.is_some_and(|expires| expires > chrono::Utc::now().timestamp().saturating_add(120)) {
        let refresh = secret.refresh_token.as_deref().ok_or_else(|| consent(route))?;
        let request = oauth::refresh(&client.client_id, &client_secret, refresh);
        #[cfg(test)]
        let request = oauth::TEST_REFRESH_URL.scope(
            oauth::mcp_refresh_test::endpoint(home).ok_or_else(changed)?, request,
        );
        let mut updated = request.await.map_err(|_| consent(route))?;
        let scopes = updated.granted_scopes.as_deref().ok_or_else(||
            Refusal::hard("scope_unverified", "無法確認 Google 寫入授權範圍，這次沒有執行。"))?;
        if !scopes.iter().any(|scope| scope == oauth::DRIVE_WRITE_SCOPE) {
            if !write_store::mark_needs_reconnect(home, &metadata).map_err(|_| changed())? {
                return Err(changed());
            }
            return Err(Refusal::hard("drive_write_needs_member_consent",
                "這個 Google 帳號需要重新連結雲端硬碟寫入，沒有執行。"));
        }
        if updated.secret.refresh_token.is_none() {
            updated.secret.refresh_token = secret.refresh_token.take();
        }
        if !write_store::finish_refresh_with_scopes(home, &key, &metadata, &updated.secret, Some(scopes))
            .map_err(|_| changed())? {
            return Err(changed());
        }
        // Re-read and compare exactly what the CAS published. Never select a
        // replacement grant if a reconnect raced the refresh.
        let current = ready_record(home, route)?;
        if current.metadata.credential_id != metadata.credential_id
            || current.metadata.revision != metadata.revision.checked_add(1).ok_or_else(changed)? {
            return Err(changed());
        }
        metadata = current.metadata;
        secret = updated.secret;
    }
    if ready_record(home, route)?.metadata != metadata { return Err(changed()); }
    Ok(WriteToken {
        bearer: Zeroizing::new(std::mem::take(&mut secret.access_token)),
        meta: TokenMeta {
            identity_kind: if matches!(route, IdentityRoute::HomeOAuth) { "home_oauth" } else { "personal" },
            account_email: metadata.account_email.clone(), credential_revision: Some(metadata.revision),
            write_record: Some(metadata),
        },
    })
}

/// A late 403 must not invalidate a newer reconnect or another route's grant.
pub(in crate::google_identity) fn on_insufficient_permissions(home: &Path, route: &IdentityRoute, token_meta: &TokenMeta) -> Result<(), Refusal> {
    let Some((kind, id)) = owner_key(route) else { return Ok(()); };
    let expected = token_meta.write_record.as_ref().ok_or_else(changed)?;
    if expected.owner_kind != kind || expected.owner_id != id { return Err(changed()); }
    if !write_store::mark_needs_reconnect(home, expected).map_err(|_| changed())? { return Err(changed()); }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::google_identity) enum RequesterOutcome { LiveMember, Unattended }
impl RequesterOutcome {
    /// The caller checks this flag on the result, including the refusal path.
    pub(in crate::google_identity) fn notify_unattended(result: &Result<Self, Refusal>) -> bool {
        result.as_ref().is_err_and(|r| r.code == "approval_unavailable_unattended")
    }
}

pub(in crate::google_identity) fn requester_check(route: &IdentityRoute, who: &RequesterIdentity, approval_required: bool) -> Result<RequesterOutcome, Refusal> {
    if let RequesterIdentity::ChannelUser { channel, .. } = who {
        if is_public_visitor_channel(channel) {
            return Err(Refusal::hard("public_visitor", "這個對話無法執行這項操作。"));
        }
    }
    if matches!(route, IdentityRoute::Unsupported) { return Err(unsupported()); }
    let holder = match route {
        IdentityRoute::Personal { owner_user_id, .. } | IdentityRoute::DelegateOwner { owner_user_id, .. } => Some(owner_user_id.as_str()),
        _ => None,
    };
    let (user, unattended) = match who {
        RequesterIdentity::DashboardUser { user_id } | RequesterIdentity::ChannelUser { user_id: Some(user_id), .. }
            if !user_id.is_empty() => (Some(user_id.as_str()), false),
        RequesterIdentity::System { on_behalf_of, .. } => (on_behalf_of.as_deref(), true),
        _ => return Err(Refusal::hard("requester_refused", "無法確認這次請求的身分，沒有執行。")),
    };
    if unattended && approval_required {
        return Err(Refusal::hard("approval_unavailable_unattended", "這項寫入需要本人確認，背景工作無法等待確認，沒有執行。"));
    }
    if holder.is_some_and(|id| id.is_empty() || user != Some(id)) {
        return Err(Refusal::hard("requester_not_account_holder", "這項寫入只能由帳號本人提出，沒有執行。"));
    }
    Ok(if unattended { RequesterOutcome::Unattended } else { RequesterOutcome::LiveMember })
}

/// Claim before awaiting delivery; a poisoned limiter suppresses notification.
pub(super) fn claim_notice<K: Eq + std::hash::Hash>(map: &Mutex<HashMap<K, Instant>>, key: K, ttl: Duration) -> bool {
    let Ok(mut entries) = map.lock() else { return false; };
    let now = Instant::now();
    entries.retain(|_, at| now.saturating_duration_since(*at) < ttl);
    if entries.contains_key(&key) { return false; }
    entries.insert(key, now);
    true
}

pub(in crate::google_identity) async fn notify_unattended(home: &Path, agent: &str) {
    static SENT: OnceLock<Mutex<HashMap<(PathBuf, String), Instant>>> = OnceLock::new();
    let owner = crate::decision_notify::open_user_db_read_only(home).ok().flatten()
        .and_then(|db| db.google_owner(agent).ok())
        .filter(|u| u.status == duduclaw_auth::UserStatus::Active);
    let Some(owner) = owner else {
        tracing::warn!(agent, "Drive write unattended notice: primary Owner unavailable");
        return;
    };
    let Ok(home_key) = home.canonicalize() else { return; };
    if !claim_notice(SENT.get_or_init(Default::default), (home_key, agent.into()), Duration::from_secs(3600)) { return; }
    crate::notify_push::push_person_notice(home, &owner.id,
        &format!("AI 員工「{agent}」的背景工作需要本人確認雲端硬碟寫入，這次沒有執行。請到對話中提出操作並確認。")).await;
}
