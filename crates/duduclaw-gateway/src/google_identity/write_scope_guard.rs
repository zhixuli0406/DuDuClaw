//! Bearers with full Drive access stay inside the dedicated write owner.
use std::path::Path;
use super::Refusal;
use crate::person_credentials::{self as vault, Metadata};

pub(crate) fn carries_write_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|scope| scope == crate::google_personal::oauth::DRIVE_WRITE_SCOPE)
}

pub(crate) fn write_scope_isolated() -> Refusal {
    Refusal::hard("write_scope_isolated", "這個 Google 連線目前無法供 AI 使用，這次沒有執行，請帳號本人到「我的連線」重新連結。")
}

pub(crate) fn scope_unverified() -> Refusal {
    Refusal::hard("scope_unverified", "無法確認這個 Google 連線允許哪些操作，這次沒有執行。請稍後再試，若持續發生，請帳號本人重新連結。")
}

pub(crate) fn check_requested(scopes: &[&str]) -> Result<(), Refusal> {
    if scopes.iter().any(|scope| *scope == crate::google_personal::oauth::DRIVE_WRITE_SCOPE) {
        return Err(write_scope_isolated());
    }
    Ok(())
}

pub(crate) fn check_actual(scopes: &[String]) -> Result<(), Refusal> {
    if carries_write_scope(scopes) { return Err(write_scope_isolated()); }
    Ok(())
}

/// Only ordinary credentials enter here; never the separate write vault.
/// A failed CAS/storage operation still refuses this bearer. Never reselect.
pub(crate) fn check_personal(home: &Path, metadata: &Metadata, scopes: &[String]) -> Result<(), Refusal> {
    if !carries_write_scope(scopes) { return Ok(()); }
    isolate_personal(home, metadata);
    Err(write_scope_isolated())
}

/// Omitted scopes retain the credential's grant unless a past or present write grant
/// makes a merged grant plausible. Store and tokeninfo failures never fall back.
pub(crate) async fn refresh_scopes(
    home: &Path,
    owner_kind: &str,
    owner_id: &str,
    stored_scopes: &[String],
    scopes: Option<&[String]>,
    access_token: &str,
    client_id: &str,
) -> Result<Vec<String>, Refusal> {
    if let Some(scopes) = scopes {
        return Ok(scopes.to_vec());
    }
    match vault::write_store::ever_granted(home, owner_kind, owner_id) {
        Ok(false) => Ok(stored_scopes.to_vec()),
        Ok(true) => crate::google_personal::oauth::tokeninfo(access_token, client_id)
            .await.map_err(|_| scope_unverified()),
        Err(_) => Err(scope_unverified()),
    }
}

pub(crate) async fn check_personal_refresh(
    home: &Path,
    metadata: &Metadata,
    scopes: Option<&[String]>,
    access_token: &str,
) -> Result<Vec<String>, Refusal> {
    let scopes = refresh_scopes(home, "user", &metadata.owner_id,
        &metadata.granted_scopes, scopes, access_token, &metadata.oauth_client_id).await?;
    check_personal(home, metadata, &scopes)?;
    Ok(scopes)
}

fn isolate_personal(home: &Path, metadata: &Metadata) {
    match vault::mark_needs_reconnect(home, metadata) {
        Ok(true) => {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let home = home.to_path_buf();
                let owner = metadata.owner_id.clone();
                handle.spawn(async move {
                    crate::notify_push::push_person_notice(&home, &owner, &write_scope_isolated().message_zh).await;
                });
            }
        }
        Ok(false) => {},
        Err(_) => tracing::warn!("Could not mark isolated Google credential for reconnect"),
    }
}

#[cfg(test)]
#[path = "write_scope_test_support.rs"]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carries_write_scope_matches_only_exact_items() {
        let drive = crate::google_personal::oauth::DRIVE_WRITE_SCOPE;
        assert!(carries_write_scope(&["openid".into(), drive.into()]));
        for scope in ["", "https://www.googleapis.com/auth/drive.readonly", "https://www.googleapis.com/auth/drive.file", "https://www.googleapis.com/auth/drive.metadata", "https://www.googleapis.com/auth/drive/", " https://www.googleapis.com/auth/drive", "https://www.googleapis.com/auth/drive email"] {
            assert!(!carries_write_scope(&[scope.into()]));
        }
        assert!(!carries_write_scope(&[]));
        assert!(!write_scope_isolated().fallback_eligible);
        assert!(!scope_unverified().fallback_eligible);
        assert!(check_requested(&[drive]).is_err());
    }
}
