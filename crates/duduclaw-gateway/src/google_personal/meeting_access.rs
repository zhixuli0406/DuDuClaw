//! Dashboard-only access to the caller's own selected Drive files and Meet
//! recording metadata. No bearer escapes this owner, including to the Picker.
use super::{access, flow, grants, oauth};
use crate::person_credentials::{self as vault, CredentialAuthority, Metadata, Status};
use duduclaw_auth::UserDb;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use zeroize::Zeroizing;

pub(crate) const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
pub(crate) const MEET_SCOPE: &str = "https://www.googleapis.com/auth/meetings.space.readonly";
const CHANGED: &str = "Google 連線或授權已變更，請重新選擇錄影。";
const UNAVAILABLE: &str = "暫時無法讀取 Google 檔案，請稍後重試。";

pub(crate) fn has_scopes(scopes: &[String]) -> bool {
    [DRIVE_SCOPE, MEET_SCOPE].iter().all(|wanted| scopes.iter().any(|scope| scope.as_str() == *wanted))
}

pub(crate) fn check_metadata(home: &Path, db: &UserDb, user: &str, metadata: &Metadata) -> Result<(), String> {
    flow::active_user(db, user)?;
    if !crate::team_edition::team_grants_enabled(home) {
        return Err("請使用團隊版的個人 Google 連線。".into());
    }
    grants::check_metadata_barrier(home)?;
    if !crate::google_workspace::integration_enabled(home) {
        return Err("管理員尚未開啟 Google 整合。".into());
    }
    if metadata.owner_id != user || metadata.status != Status::Connected {
        return Err(CHANGED.into());
    }
    crate::google_data_consent::require_current(metadata.google_data_consent.as_ref())?;
    if !access::domain_allowed(home, metadata)? {
        return Err("這個 Google 帳號不在管理員允許的網域內。".into());
    }
    if !metadata.local_purposes.iter().any(|p| p == "meeting_recordings")
        || !has_scopes(&metadata.granted_scopes) {
        return Err("請先開放所選雲端硬碟檔案及 Meet 錄影的讀取權限。".into());
    }
    Ok(())
}

fn client_fingerprint(client: &crate::mcp_oauth::McpOAuthClientConfig) -> String {
    let mut hash = Sha256::new();
    for part in [&client.client_id, &client.client_secret] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hex::encode(hash.finalize())
}

pub(crate) struct MeetingAccess {
    authority: CredentialAuthority,
    token_revision: u64,
    client_fingerprint: String,
    token: Zeroizing<String>,
    expires_at: i64,
}
impl MeetingAccess {
    pub(crate) fn owner_id(&self) -> &str { &self.authority.owner_id }
    pub(crate) fn account_email(&self) -> &str { &self.authority.account_email }

    pub(crate) fn validate(&self, home: &Path, db: &UserDb) -> Result<(), String> {
        let current = vault::authority_state(home, self.owner_id())?;
        let metadata = current.metadata.as_ref().ok_or(CHANGED)?;
        check_metadata(home, db, self.owner_id(), metadata)?;
        let client = crate::mcp_oauth::get_client_config(home, "google").ok_or(CHANGED)?;
        if current.authority().as_ref() != Some(&self.authority)
            || metadata.revision != self.token_revision
            || self.expires_at <= chrono::Utc::now().timestamp()
            || client_fingerprint(&client) != self.client_fingerprint
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    pub(crate) async fn get(&self, home: &Path, db: &UserDb, url: reqwest::Url) -> Result<reqwest::Response, String> {
        self.validate(home, db)?;
        if !allowed_endpoint(&url) { return Err(UNAVAILABLE.into()); }
        #[cfg(test)]
        let url = test_endpoint(home, &url)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30 * 60))
            .build().map_err(|_| UNAVAILABLE)?;
        let request = client.get(url).bearer_auth(self.token.as_str())
            .build().map_err(|_| UNAVAILABLE)?;
        self.validate(home, db)?;
        let response = client.execute(request).await.map_err(|_| UNAVAILABLE)?;
        self.validate(home, db)?;
        // No redirect target or error body may enter a follow-up request.
        if !response.status().is_success() { return Err(UNAVAILABLE.into()); }
        Ok(response)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn allowed_endpoint(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some()
        || url.port().is_some_and(|p| p != 443) || url.fragment().is_some()
    { return false; }
    let Some(segments) = url.path_segments() else { return false; };
    let parts: Vec<_> = segments.collect();
    match (url.host_str(), parts.as_slice()) {
        (Some("www.googleapis.com"), ["drive", "v3", "files"]) => true,
        (Some("www.googleapis.com"), ["drive", "v3", "files", id]) => valid_id(id),
        (Some("meet.googleapis.com"), ["v2", "conferenceRecords"]) => true,
        (Some("meet.googleapis.com"), ["v2", "conferenceRecords", id, "recordings"]) => valid_id(id),
        _ => false,
    }
}

pub(crate) async fn resolve(home: &Path, db: &UserDb, user: &str) -> Result<MeetingAccess, String> {
    // Re-read after the cross-process lease. This also serializes refreshes
    // with the existing personal OAuth owner and provider revocations.
    flow::active_user(db, user)?;
    let _lease = grants::acquire(home).await?;
    let state = vault::authority_state(home, user)?;
    let authority = state.authority().ok_or(CHANGED)?;
    let metadata = state.metadata.as_ref().ok_or(CHANGED)?;
    check_metadata(home, db, user, metadata)?;
    let client = crate::mcp_oauth::get_client_config(home, "google").ok_or(CHANGED)?;
    if client.client_id != metadata.oauth_client_id { return Err(CHANGED.into()); }
    let fingerprint = client_fingerprint(&client);
    let key = crate::google_identity::protection_gate_async(home).await.map_err(|e| e.message_zh)?;
    let record = vault::get(home, user)?.ok_or(CHANGED)?;
    if vault::authority_snapshot(home, user)?.as_ref() != Some(&authority)
        || record.metadata.revision != metadata.revision
    { return Err(CHANGED.into()); }
    let mut secret = vault::unseal(&key, &record)?;
    let mut material = MeetingAccess {
        authority,
        token_revision: record.metadata.revision,
        client_fingerprint: fingerprint,
        token: Zeroizing::new(String::new()),
        // Expired credentials may only cross the controlled refresh boundary.
        expires_at: i64::MAX,
    };
    material.validate(home, db)?;
    if secret.expires_at.is_none_or(|expiry| expiry <= chrono::Utc::now().timestamp().saturating_add(120)) {
        let refresh = secret.refresh_token.as_deref().ok_or(CHANGED)?;
        let result = oauth::refresh_meeting(&client.client_id, &client.client_secret, refresh).await;
        material.validate(home, db)?;
        let mut tokens = match result {
            Ok(tokens) => tokens,
            Err(oauth::Error::InvalidGrant) => {
                let _ = vault::mark_needs_reconnect(home, &record.metadata)?;
                return Err("Google 連線已失效，請到「我的連線」重新連結。".into());
            }
            Err(_) => return Err(UNAVAILABLE.into()),
        };
        if tokens.secret.refresh_token.is_none() {
            tokens.secret.refresh_token = secret.refresh_token.take();
        }
        if !vault::finish_refresh_with_scopes(home, &key, &record.metadata, &tokens.secret, tokens.granted_scopes.as_deref())? {
            return Err(CHANGED.into());
        }
        material.token_revision = material.token_revision.checked_add(1).ok_or(CHANGED)?;
        // A scope change advances the authority epoch and stops this import;
        // it cannot be hidden by adopting a fresh post-refresh snapshot.
        material.validate(home, db)?;
        secret = tokens.secret;
    }
    material.expires_at = secret.expires_at.ok_or(CHANGED)?;
    material.token = Zeroizing::new(std::mem::take(&mut secret.access_token));
    material.validate(home, db)?;
    Ok(material)
}

#[cfg(test)]
fn test_endpoints() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, String>> {
    static ENDPOINTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, String>>> = std::sync::OnceLock::new();
    ENDPOINTS.get_or_init(Default::default)
}
#[cfg(test)]
pub(crate) struct TestEndpointGuard(std::path::PathBuf);
#[cfg(test)]
impl Drop for TestEndpointGuard {
    fn drop(&mut self) { test_endpoints().lock().unwrap().remove(&self.0); }
}
#[cfg(test)]
pub(crate) fn install_test_endpoint(home: &Path, base: &str) -> TestEndpointGuard {
    let url = reqwest::Url::parse(base).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert!(url.port().is_some());
    assert_eq!(url.path(), "/");
    assert!(url.query().is_none() && url.fragment().is_none() && url.username().is_empty() && url.password().is_none());
    let path = home.canonicalize().unwrap();
    assert!(test_endpoints().lock().unwrap().insert(path.clone(), base.trim_end_matches('/').into()).is_none());
    TestEndpointGuard(path)
}
#[cfg(test)]
fn test_endpoint(home: &Path, url: &reqwest::Url) -> Result<reqwest::Url, String> {
    let path = home.canonicalize().map_err(|_| UNAVAILABLE)?;
    let base = test_endpoints().lock().unwrap().get(&path).cloned().ok_or(UNAVAILABLE)?;
    let mut target = reqwest::Url::parse(&format!("{base}/{}{}", url.host_str().ok_or(UNAVAILABLE)?, url.path())).map_err(|_| UNAVAILABLE)?;
    target.set_query(url.query());
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_transport_only_allows_fixed_google_read_paths() {
        for url in ["https://www.googleapis.com/drive/v3/files", "https://www.googleapis.com/drive/v3/files/a-B_1?alt=media", "https://meet.googleapis.com/v2/conferenceRecords", "https://meet.googleapis.com/v2/conferenceRecords/abc/recordings"] {
            assert!(allowed_endpoint(&url.parse().unwrap()), "{url}");
        }
        for url in ["http://www.googleapis.com/drive/v3/files", "https://www.googleapis.com.evil.test/drive/v3/files", "https://www.googleapis.com/drive/v3/files/a%2fb", "https://www.googleapis.com/drive/v3/files/a/permissions", "https://user@www.googleapis.com/drive/v3/files", "https://www.googleapis.com:8443/drive/v3/files", "https://meet.googleapis.com/v2/spaces"] {
            assert!(!allowed_endpoint(&url.parse().unwrap()), "{url}");
        }
    }

    #[test]
    fn meeting_scopes_need_selected_files_and_meet_metadata() {
        assert!(!has_scopes(&[DRIVE_SCOPE.into()]));
        assert!(!has_scopes(&[MEET_SCOPE.into()]));
        assert!(!has_scopes(&["https://www.googleapis.com/auth/drive.readonly".into(), MEET_SCOPE.into()]));
        assert!(has_scopes(&[DRIVE_SCOPE.into(), MEET_SCOPE.into()]));
    }

    async fn connected_fixture() -> (super::super::tests::Fixture, MeetingAccess) {
        let f = super::super::tests::Fixture::new();
        f.connect().await;
        let start = flow::start_with_meetings(&f.home, &f.db, &f.alice.id, "meeting-login", "http://127.0.0.1:18889", &[], true, true).await.unwrap();
        let state = start["state"].as_str().unwrap();
        super::super::pending::receive_code(&f.home, state, "meeting-alice").unwrap();
        flow::finish(&f.home, &f.db, &f.alice.id, "meeting-login", state).await.unwrap();
        let material = resolve(&f.home, &f.db, &f.alice.id).await.unwrap();
        (f, material)
    }

    #[tokio::test]
    async fn meeting_access_stops_after_disconnect_even_when_same_account_reconnects() {
        let (f, material) = connected_fixture().await;
        material.validate(&f.home, &f.db).unwrap();
        vault::disconnect(&f.home, &f.alice.id).unwrap();
        assert!(material.validate(&f.home, &f.db).is_err());
        f.connect().await;
        assert!(material.validate(&f.home, &f.db).is_err());
    }

    #[tokio::test]
    async fn meeting_access_stops_after_scope_revocation_or_disabled_integration() {
        let (f, material) = connected_fixture().await;
        std::fs::write(f.home.join("config.toml"), "[integrations]\ngoogle_workspace=false\n").unwrap();
        assert!(material.validate(&f.home, &f.db).is_err());
        std::fs::write(f.home.join("config.toml"), "[integrations]\ngoogle_workspace=true\n").unwrap();
        let record = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
        let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
        let secret = vault::unseal(&key, &record).unwrap();
        assert!(vault::finish_refresh_with_scopes(&f.home, &key, &record.metadata, &secret, Some(&[MEET_SCOPE.into()])).unwrap());
        assert!(material.validate(&f.home, &f.db).is_err());
        assert!(resolve(&f.home, &f.db, &f.alice.id).await.is_err());
    }

    #[tokio::test]
    async fn meeting_access_refresh_respects_actual_scopes_and_suspended_user() {
        let (f, material) = connected_fixture().await;
        f.db.set_user_status(&f.alice.id, duduclaw_auth::UserStatus::Suspended).unwrap();
        assert!(material.validate(&f.home, &f.db).is_err());
        assert!(resolve(&f.home, &f.db, &f.alice.id).await.is_err());
        f.db.set_user_status(&f.alice.id, duduclaw_auth::UserStatus::Active).unwrap();
        let record = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
        let key = crate::google_identity::protection_gate_async(&f.home).await.unwrap();
        let mut secret = vault::unseal(&key, &record).unwrap();
        secret.expires_at = Some(1);
        assert!(vault::finish_refresh(&f.home, &key, &record.metadata, &secret).unwrap());
        // The fixture refresh endpoint returns only identity/Gmail scopes.
        assert!(resolve(&f.home, &f.db, &f.alice.id).await.is_err());
        let refreshed = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
        assert!(!has_scopes(&refreshed.metadata.granted_scopes));
    }

    #[tokio::test]
    async fn meeting_access_refuses_unknown_edition_and_downgrade() {
        let (f, material) = connected_fixture().await;
        let unknown = tempfile::tempdir().unwrap();
        let record = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
        assert!(check_metadata(unknown.path(), &f.db, &f.alice.id, &record.metadata).is_err());
        crate::team_edition::record(&f.home, duduclaw_core::EditionProfile::Personal);
        assert!(material.validate(&f.home, &f.db).is_err());
        assert!(resolve(&f.home, &f.db, &f.alice.id).await.is_err());
    }
}
