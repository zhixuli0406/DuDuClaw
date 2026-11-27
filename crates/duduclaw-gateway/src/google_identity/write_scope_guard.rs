//! Bearers with full Drive access stay inside the dedicated write owner.
use std::path::Path;
use super::Refusal;
use crate::person_credentials::{self as vault, Metadata};

pub(crate) fn carries_write_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|scope| scope == crate::google_personal::oauth::DRIVE_WRITE_SCOPE)
}

pub(crate) fn write_scope_isolated() -> Refusal {
    Refusal::hard("write_scope_isolated", "這個 Google 連線已帶有雲端硬碟寫入權限，不能交給工具使用。請到「我的連線」重新連結。")
}

pub(crate) fn scope_unverified() -> Refusal {
    Refusal::hard("scope_unverified", "Google 沒有回傳這次權杖的授權範圍，為了安全沒有交給工具使用；請稍後再試，若一直發生請重新連結。")
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

pub(crate) fn check_personal_refresh(home: &Path, metadata: &Metadata, scopes: Option<&[String]>) -> Result<(), Refusal> {
    match scopes {
        Some(scopes) => check_personal(home, metadata, scopes),
        // A refresh without scope evidence cannot prove bearer isolation.
        None => Err(scope_unverified()),
    }
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
