//! The person-identity decision (design §5.3, B2a subset).
//!
//! ```text
//! selector (gateway's own read of agent.toml + config.toml)
//!   home     → UseHome (the caller keeps its in-process path)
//!   none / invalid / not available → refuse
//!   delegate → requester gate (§5.2)           refused ⇒ never fallback
//!            → domain in allowed_subject_domains   config error ⇒ never fallback
//!            → colleague with that email still active    F
//!            → protection gate (B2c)            hard
//!            → sealed key                        missing F / unreadable hard
//!            → mint (sub = email, service scopes)    rejected / unreachable F
//! F + `fallback = "home"` ⇒ UseHome with a notice; otherwise the refusal.
//! ```

use std::path::Path;

use duduclaw_core::google_identity::{self as gi, Effective};
use duduclaw_knowledge::requester::RequesterIdentity;

use super::requester_gate::{self, People};
use super::{Fallback, Refusal, delegation_key};
use crate::google_service_account::{self as sa, CacheMode, MintedToken, ServiceAccountError};


/// What the caller should use.
#[derive(Debug)]
pub enum Resolved {
    /// A token minted for the delegate.
    Delegated { email: String, token: MintedToken },
    Personal {
        authority: crate::google_personal::authority::PersonalAuthority,
        kind: &'static str,
        user_id: String,
        email: String,
        token: MintedToken,
    },
    /// The home account, in the caller's own process. `fallback` is true when
    /// a delegate failed and the employee's `fallback = "home"` covered it;
    /// `notice` is then prepended to the tool result.
    UseHome {
        fallback: bool,
        notice: Option<String>,
    },
}

/// Refusal for an effective selector that is not a delegate.
pub fn refusal_for(effective: &Effective) -> Option<Refusal> {
    match effective {
        Effective::Home | Effective::Delegate { .. } | Effective::Personal { .. } => None,
        Effective::DelegateOwner => Some(Refusal::hard("attestation_required", "請管理員確認目前負責人的信箱並重新認證。")),
        Effective::External => Some(Refusal::hard("external", "這位 AI 員工的 Google 由外部工具提供。")),
        Effective::Disabled => Some(Refusal::hard(
            "disabled",
            "這位 AI 員工設定為不使用 Google。",
        )),
        Effective::Invalid(why) => Some(Refusal::hard(
            "config_invalid",
            format!(
                "這位 AI 員工的 Google 身分設定有誤（{why}），請管理員到員工的「連線」分頁修正。"
            ),
        )),
    }
}

/// Explain a Google token-endpoint failure (design §7.3).
pub fn explain_mint_error(err: &ServiceAccountError, email: &str) -> Refusal {
    match err {
        ServiceAccountError::RequestFailed(_) => {
            Refusal::soft("google_unreachable", "暫時連不上 Google，請稍後再試。")
        }
        ServiceAccountError::Rejected(_, client_id) => match sa::rejection_code(err) {
            Some("unauthorized_client") => Refusal::soft(
                "delegate_rejected",
                format!(
                    "Google 拒絕了公司授權代表：Google 管理員還沒授權這組服務，或授權的權限清單少了幾項（用戶端 ID {client_id}）。請到 Google 管理控制台的「全網域委派」確認。"
                ),
            ),
            Some("invalid_grant") => Refusal::soft(
                "delegate_rejected",
                format!("公司 Google 裡找不到 {email} 這個帳號，或這個帳號已停用。"),
            ),
            _ => Refusal::soft(
                "delegate_rejected",
                "Google 拒絕了公司授權代表，請管理員在員工的「連線」分頁按「測試連線」查看原因。",
            ),
        },
        _ => Refusal::hard(
            "credential_protection",
            "封存的服務帳號金鑰內容錯誤，請管理員重新匯入。",
        ),
    }
}

/// Everything before the mint that needs no network: domain and the
/// colleague's account state.
pub fn precheck(home: &Path, email: &str, people: &dyn People) -> Result<(), Refusal> {
    let cfg = gi::read_home_config(home).map_err(|e| Refusal::hard("config_invalid", e))?;
    if !gi::delegate_domain_allowed(email, &cfg, super::home_subject(home).as_deref()) {
        return Err(Refusal::hard(
            "config_invalid",
            format!(
                "{email} 不在允許的網域。請管理員在「整合 → Google → 公司授權代表」設定允許的網域。"
            ),
        ));
    }
    if let Some(p) = people.by_email(email)?
        && !p.active
    {
        return Err(Refusal::soft(
            "person_inactive",
            format!(
                "{email} 在 DuDuClaw 的帳號已停權或離職，這位 AI 員工不能再代表他使用 Google。"
            ),
        ));
    }
    Ok(())
}

/// Mint for `email` with the sealed key. Blocking work (keychain) runs on
/// the blocking pool.
pub async fn mint_delegate(
    home: &Path,
    email: &str,
    scopes: &[&str],
    mode: CacheMode,
) -> Result<MintedToken, Refusal> {
    super::write_scope_guard::check_requested(scopes)?;
    let key = super::protection_gate_async(home).await?;
    let home_buf = home.to_path_buf();
    let sa_key = tokio::task::spawn_blocking(move || delegation_key::load(&home_buf, &key))
        .await
        .unwrap_or_else(|_| {
            Err(Refusal::hard(
                "credential_protection",
                "讀取服務帳號金鑰中斷，請稍後再試。",
            ))
        })?;
    let token = sa::mint_for_subject(&sa_key, email, scopes, mode)
        .await
        .map_err(|e| explain_mint_error(&e, email))?;
    if let Some(scopes) = &token.granted_scopes {
        super::write_scope_guard::check_actual(scopes)?;
    }
    Ok(token)
}

/// Only gateway identity code can mint the fixed write scope group.
pub(in crate::google_identity) async fn mint_delegate_for_drive_write(
    home: &Path, email: &str, mode: CacheMode,
) -> Result<MintedToken, Refusal> {
    let key = super::protection_gate_async(home).await?;
    let home = home.to_path_buf();
    let sa_key = tokio::task::spawn_blocking(move || delegation_key::load(&home, &key))
        .await.map_err(|_| Refusal::hard("credential_protection", "無法使用公司 Google 連線，這次沒有執行，請管理員檢查連線後再試。"))??;
    validate_drive_write_mint(sa::mint_for_subject(
        &sa_key, email, super::delegated_scopes::for_drive_write(), mode,
    ).await, email)
}

pub(in crate::google_identity) fn drive_write_admin_scope_missing() -> Refusal {
    Refusal::hard("drive_write_admin_scope_missing",
        "公司管理員尚未允許雲端硬碟寫入，這次沒有執行，請公司管理員在 Google 管理控制台開放寫入權限。")
}

pub(in crate::google_identity) fn validate_drive_write_mint(
    result: Result<MintedToken, ServiceAccountError>, email: &str,
) -> Result<MintedToken, Refusal> {
    let token = result.map_err(|error| {
        if sa::rejection_code(&error) == Some("unauthorized_client") {
            drive_write_admin_scope_missing()
        } else {
            let refusal = explain_mint_error(&error, email);
            Refusal { fallback_eligible: false, ..refusal }
        }
    })?;
    if token.granted_scopes.as_ref().is_some_and(|scopes| {
        !scopes.iter().any(|scope| scope == "https://www.googleapis.com/auth/drive")
    }) {
        return Err(drive_write_admin_scope_missing());
    }
    Ok(token)
}

/// The full decision for one employee and one turn.
pub async fn resolve_person(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    endpoint: &str,
) -> Result<Resolved, Refusal> {
    let scopes = super::delegated_scopes::for_endpoint(home, endpoint)?;
    if crate::google_authorization_requests::turn::probe_for_requester(home,agent,requester,"google_remote_mcp").await {
        return Err(Refusal::hard("not_connected",crate::google_authorization_requests::DASHBOARD_HINT));
    }
    resolve_person_scoped(home, agent, requester, people, "google_remote_mcp", Some(scopes)).await
}

pub async fn resolve_person_for_tool(
    home: &Path,
    agent: &str,
    requester: &RequesterIdentity,
    people: &dyn People,
    tool: &str,
) -> Result<Resolved, Refusal> {
    if crate::google_authorization_requests::turn::probe_for_requester(home,agent,requester,tool).await {
        // A missing own-account purpose pauses this live human turn before
        // any configured home-account fallback can execute the task.
        return Err(Refusal::hard("not_connected",crate::google_authorization_requests::DASHBOARD_HINT));
    }
    resolve_person_scoped(home, agent, requester, people, tool, None).await
}

/// Scopes originate only in the gateway's pinned employee context.
pub(crate) async fn resolve_employee_scoped(home: &Path, agent: &str, requester: &RequesterIdentity,
    people: &dyn People, tool: &str, scopes: &[&str]) -> Result<Resolved, Refusal> {
    resolve_person_scoped(home, agent, requester, people, tool, Some(scopes)).await
}

async fn resolve_person_scoped(
    home: &Path, agent: &str, requester: &RequesterIdentity, people: &dyn People,
    tool: &str, endpoint_scopes: Option<&[&str]>,
) -> Result<Resolved, Refusal> {
    if !crate::google_personal::scopes::token_tool(tool) {
        return Err(Refusal::hard("scope_missing", "這項操作不能取得 Google 權杖。"));
    }
    let owner_selected = gi::effective_for(home, Some(agent)).effective == Effective::DelegateOwner;
    // Bind this request to the employee before resolving its attested owner.
    // Re-attesting the same email must not authorize an older incarnation.
    let owner_incarnation = if owner_selected {
        Some(super::delegate_owner::incarnation(home, agent).ok_or_else(|| {
            Refusal::hard("identity_changed", "負責人或信箱認證已變更，請管理員重新確認。")
        })?)
    } else {
        None
    };
    let eff = crate::google_identity::delegate_owner::effective_for(home, Some(agent));
    if matches!(eff.effective, Effective::Personal { .. }) {
        let outcome =
            crate::google_personal::access::resolve(home, agent, requester, people, &eff, tool)
                .await;
        return match outcome {
            // v1.81 E3: inside an isolated request the only home fallback is
            // the catalog-backed explicit SharedHome route (C2); this legacy
            // resolver never switches a Personal call to the home account.
            Err(r) if r.fallback_eligible && duduclaw_mcp_boundary::io_authority::is_scoped() => {
                Err(Refusal { fallback_eligible: false, ..r })
            }
            Err(r) if r.fallback_eligible && eff.fallback == Fallback::Home => {
                Ok(Resolved::UseHome {
                    fallback: true,
                    notice: Some(format!("已改用公司共用帳號處理，因為：{}", r.message_zh)),
                })
            }
            other => other,
        };
    }
    let email = match &eff.effective {
        Effective::Home => {
            return Ok(Resolved::UseHome {
                fallback: false,
                notice: None,
            });
        }
        Effective::Delegate { email } => email.clone(),
        other => {
            return Err(
                refusal_for(other).unwrap_or_else(|| Refusal::hard("config_invalid", "設定錯誤"))
            );
        }
    };
    requester_gate::check(eff.requesters, requester, agent, &email, people)?;
    let attempt = match precheck(home, &email, people) {
        Ok(()) => mint_delegate(home, &email, match endpoint_scopes { Some(scopes) => scopes, None => super::delegated_scopes::for_tool(home, tool)? }, if owner_selected { CacheMode::Bypass } else { CacheMode::Use }).await,
        Err(r) => Err(r),
    };
    if owner_selected {
        let current = super::delegate_owner::effective_for(home, Some(agent));
        if current != eff
            || gi::effective_for(home, Some(agent)).effective != Effective::DelegateOwner
            || super::delegate_owner::incarnation(home, agent) != owner_incarnation
        {
            return Err(Refusal::hard("identity_changed", "負責人或信箱認證已變更，請管理員重新確認。"));
        }
        requester_gate::check(current.requesters, requester, agent, &email, people)?;
    }
    match attempt {
        Ok(token) => Ok(Resolved::Delegated { email, token }),
        Err(r) if r.fallback_eligible && eff.fallback == Fallback::Home => Ok(Resolved::UseHome {
            fallback: true,
            notice: Some(format!("已改用公司共用帳號處理，因為：{}", r.message_zh)),
        }),
        Err(r) => Err(r),
    }
}
