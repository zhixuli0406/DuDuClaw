//! Local deletion always precedes provider revocation. A Google revoke can
//! invalidate a whole grant, so uncertainty about any shared account means
//! that only the local connection is removed.

use super::{oauth, pending};

pub fn affected_agents(
    home: &Path,
    db: &duduclaw_auth::UserDb,
    user: &str,
) -> Result<Vec<String>, String> {
    let mut affected = Vec::new();
    for binding in db.get_user_agents(user)? {
        let effective =
            duduclaw_core::google_identity::effective_for(home, Some(&binding.agent_name));
        if let duduclaw_core::google_identity::Effective::Personal { selector } =
            effective.effective
        {
            if super::access::select_user(db, &binding.agent_name, &selector)
                .is_ok_and(|selected| selected.id == user)
            {
                affected.push(binding.agent_name);
            }
        }
    }
    affected.sort();
    affected.dedup();
    Ok(affected)
}

pub fn notify_affected(home: &Path, admin: &str, agents: &[String]) {
    if agents.is_empty() {
        return;
    }
    let text = format!(
        "這些 AI 員工的個人 Google 帳號已停用或離職：{}。請檢查「連線」設定並取得新負責人同意。",
        agents.join("、")
    );
    let home = home.to_path_buf();
    let admin = admin.to_owned();
    tokio::spawn(async move {
        crate::notify_push::push_person_notice(&home, &admin, &text).await;
    });
}
use crate::person_credentials::{self as vault, Metadata, Secret};
use std::path::Path;

fn shared_account(account: &Metadata, other: &Metadata) -> bool {
    account.google_sub == other.google_sub
        || account
            .account_email
            .eq_ignore_ascii_case(&other.account_email)
}

pub async fn safe_revoke(home: &Path, account: &Metadata, secret: &Secret) -> bool {
    if !cfg!(unix) {
        return revoke_result(home, account, "platform_durability_unavailable", false);
    }
    let Ok(lease) = super::grants::acquire(home).await else {
        return revoke_result(home, account, "coordination_unavailable", false);
    };
    safe_revoke_with_lease(home, account, secret, &lease).await
}

fn revoke_result(home: &Path, account: &Metadata, reason: &'static str, revoked: bool) -> bool {
    let _ = crate::decision_notify::open_user_db(home).and_then(|db| {
        if let Some(db) = db {
            db.log_action(
                Some(&account.owner_id),
                "user.google_revoke",
                Some(&account.owner_id),
                Some(&serde_json::json!({"provider_revoked":revoked,"reason":reason}).to_string()),
                None,
            )?;
        }
        Ok(())
    });
    revoked
}

pub async fn safe_revoke_with_lease(
    home: &Path,
    account: &Metadata,
    secret: &Secret,
    lease: &super::grants::Lease,
) -> bool {
    if !cfg!(unix) {
        return revoke_result(home, account, "platform_durability_unavailable", false);
    }
    if std::fs::canonicalize(home).ok().as_deref() != Some(lease.home()) {
        return revoke_result(home, account, "coordination_unavailable", false);
    }
    let Ok(others) = vault::all_metadata(home) else {
        return revoke_result(home, account, "personal_store_unreadable", false);
    };
    if others.iter().any(|other| shared_account(account, other)) {
        return revoke_result(home, account, "shared_personal_grant", false);
    }
    // A bridge may use an account that this gateway cannot inspect. Do not
    // assume that a missing OAuth token proves that no company account exists.
    if !matches!(
        crate::google_apps_script::config_for_home(home).await,
        Ok(None)
    ) {
        return revoke_result(home, account, "bridge_account_unknown", false);
    }
    // Remote OAuth may belong to the same Google project/account, but its
    // generic metadata cannot prove that. Fail locally rather than revoke
    // a whole grant on that assumption. Pending exchanges use this lease too.
    match crate::remote_mcp::store::load_all(home) {
        Ok(records)
            if records
                .iter()
                .all(|r| r.auth != crate::remote_mcp::store::AuthKind::Oauth) => {}
        _ => return revoke_result(home, account, "remote_oauth_account_unknown", false),
    }
    let token_file = home.join(crate::mcp_oauth::TOKEN_FILE);
    let has_home_oauth = match std::fs::read(&token_file) {
        Ok(bytes) => match serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) {
            Ok(tokens) => {
                if tokens
                    .iter()
                    .any(|t| t.get("provider_id").and_then(|v| v.as_str()) != Some("google"))
                {
                    return revoke_result(home, account, "other_home_oauth_account_unknown", false);
                }
                tokens
                    .iter()
                    .any(|t| t.get("provider_id").and_then(|v| v.as_str()) == Some("google"))
            }
            Err(_) => return revoke_result(home, account, "home_store_unreadable", false),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return revoke_result(home, account, "home_store_unreadable", false),
    };
    if has_home_oauth {
        // Read the home OAuth account itself, not a configured service-account
        // subject which has a different grant. A failed refresh/probe is closed.
        let Some(token) = crate::mcp_oauth::get_token(home, "google") else {
            return revoke_result(home, account, "home_account_unknown", false);
        };
        let Ok(home_account) = oauth::userinfo(&token.access_token).await else {
            return revoke_result(home, account, "home_account_unknown", false);
        };
        if home_account.sub == account.google_sub
            || home_account
                .email
                .eq_ignore_ascii_case(&account.account_email)
        {
            return revoke_result(home, account, "shared_home_grant", false);
        }
    }
    if lease.begin_revoke().is_err() {
        return revoke_result(home, account, "revocation_uncertain", false);
    }
    let revoked = oauth::revoke(
        secret
            .refresh_token
            .as_deref()
            .unwrap_or(&secret.access_token),
    )
    .await;
    if revoked && lease.confirm_revoke().is_ok() {
        revoke_result(home, account, "revoked", true)
    } else {
        revoke_result(home, account, "revocation_uncertain", false)
    }
}

pub async fn disconnect(
    home: &Path,
    db: &duduclaw_auth::UserDb,
    user: &str,
    actor: &str,
    offboard: bool,
) -> Result<bool, String> {
    pending::cancel_user(home, user);
    let record = vault::disconnect(home, user)?;
    let _ = db.log_action(
        Some(actor),
        if offboard {
            "user.google_tokens_dropped"
        } else {
            "user.google_disconnect"
        },
        Some(user),
        None,
        None,
    );
    let Some(record) = record else {
        return Ok(false);
    };
    let Ok(key) = crate::google_identity::protection_gate_async(home).await else {
        return Ok(false);
    };
    let Ok(secret) = vault::unseal(&key, &record) else {
        return Ok(false);
    };
    Ok(
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            safe_revoke(home, &record.metadata, &secret),
        )
        .await
        {
            Ok(revoked) => revoked,
            Err(_) => revoke_result(home, &record.metadata, "revocation_timeout", false),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_subject_or_email_is_a_shared_grant_even_when_client_ids_differ() {
        let a: Metadata = serde_json::from_value(serde_json::json!({"owner_kind":"user","owner_id":"a","provider":"google","credential_id":uuid::Uuid::new_v4(),"revision":1,"account_email":"A@example.com","google_sub":"sub-a","hosted_domain":null,"granted_scopes":[],"status":"connected","connected_at":"now","last_refresh_at":null,"oauth_client_id":"client-a"})).unwrap();
        let mut b = a.clone();
        b.owner_id = "b".into();
        b.oauth_client_id = "client-b".into();
        b.account_email = "different@example.com".into();
        assert!(shared_account(&a, &b));
        b.google_sub = "sub-b".into();
        b.account_email = "a@example.com".into();
        assert!(shared_account(&a, &b));
        b.account_email = "other@example.com".into();
        assert!(!shared_account(&a, &b));
    }
}
