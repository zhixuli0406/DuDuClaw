//! C1 SourceOwner lexical capsule. Only this owner opens captured material;
//! the legacy public adapter consumes it here. C2 recognizes only an E1-owned
//! same-material borrowed permit; the native sender remains owner-local.
use super::*;
use crate::google_service_account::{self, ServiceAccountConfig, ServiceAccountError};
use crate::google_workspace::{GoogleAuthError, REQUIRED_SCOPES};
use crate::mcp_oauth::home_store::{OAuthMaterialMetadata, RefreshIdentity, StoreSnapshot};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use zeroize::{Zeroize, Zeroizing};

#[path = "oauth_material/authorized_io.rs"]
mod authorized_io;
#[path = "oauth_material/authorized_mcp.rs"]
mod authorized_mcp;
#[path = "oauth_refresh_io.rs"]
mod refresh_io;

pub(crate) struct HomeOAuthResolved {
    access: Zeroizing<String>,
    proof: HomeOAuthMaterialProof,
    w3_expiry: Option<W3MaterialExpiry>,
}
pub(crate) struct HomeOAuthMaterialProof {
    original: HomeSourceReceipt,
    purpose_scopes: Vec<String>,
    row: OAuthMaterialMetadata,
}
impl HomeOAuthResolved {
    pub(crate) fn proof(&self) -> &HomeOAuthMaterialProof {
        &self.proof
    }
}
/// No bearer getter or authenticated builder escapes the lexical owner.
/// The permit is owned by value so this never borrows a function-local permit.
pub(crate) struct HomeOAuthBearer<'a> {
    material: &'a HomeOAuthResolved,
    permit: crate::google_identity::effect_authority::HomeOAuthEffectPermit<'a>,
}
impl HomeOAuthResolved {
    pub(crate) fn borrow_for_effect<'a>(
        &'a self,
        permit: crate::google_identity::effect_authority::HomeOAuthEffectPermit<'a>,
    ) -> std::result::Result<HomeOAuthBearer<'a>, crate::google_identity::Refusal> {
        permit.validate_home_oauth(self)?;
        Ok(HomeOAuthBearer {
            material: self,
            permit,
        })
    }
}
impl HomeOAuthBearer<'_> {
    pub(crate) fn revalidate(&self) -> std::result::Result<(), crate::google_identity::Refusal> {
        self.permit.validate_home_oauth(self.material)
    }
}

/// Not a material/proof constructor and never accepted as resolution input.
pub(crate) struct HomeOAuthCapture {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    row: OAuthMaterialMetadata,
    identity: RefreshIdentity,
    original: StoreSnapshot,
    client: Option<RawClientSnapshot>,
}
pub(crate) fn capture_from_store(
    raw: crate::mcp_oauth::McpOAuthToken,
    row: OAuthMaterialMetadata,
    identity: RefreshIdentity,
    original: StoreSnapshot,
) -> HomeOAuthCapture {
    HomeOAuthCapture {
        access: Zeroizing::new(raw.access_token),
        refresh: raw.refresh_token.map(Zeroizing::new),
        row,
        identity,
        original,
        client: None,
    }
}
/// Erase owned JSON string values, including unknown provider response fields.
/// reqwest/serde may have internal copies: this does not claim heap-wide erasure.
fn scrub(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(scrub),
        Value::Object(o) => {
            for (mut key, mut value) in std::mem::take(o) {
                key.zeroize();
                scrub(&mut value);
            }
        }
        _ => {}
    }
}
impl Drop for HomeOAuthCapture {
    fn drop(&mut self) {
        if let Some(client) = &mut self.client {
            scrub(&mut client._raw);
        }
    }
}
fn refresh_lock(home: &Path) -> Result<Arc<tokio::sync::Mutex<()>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| Error::Io)?;
    locks.retain(|_, weak| weak.strong_count() != 0);
    if let Some(lock) = locks.get(home).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    if locks.len() >= 1024 {
        return Err(Error::Invalid);
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(home.to_owned(), Arc::downgrade(&lock));
    Ok(lock)
}
fn purpose(receipt: &HomeSourceReceipt, scopes: &[&str]) -> Result<Vec<String>> {
    let scopes = home_store::normalized_scopes(scopes.iter().map(|s| s.to_string()).collect())?;
    if receipt.backend != HomeBackend::OAuth || scopes.iter().any(|s| !receipt.scopes.contains(s)) {
        return Err(Error::Invalid);
    }
    Ok(scopes)
}
fn barrier(home: &Path) -> Result<()> {
    crate::google_personal::grants::check_metadata_barrier(home)
        .map_err(|_| Error::PublicationBlocked)
}
fn require_receipt(
    g: &SourceGuard,
    source: &ObservedSource,
    receipt: &HomeSourceReceipt,
) -> Result<()> {
    let scopes: Vec<_> = receipt.scopes.iter().map(String::as_str).collect();
    if selection(g, source, &scopes, false)? == *receipt {
        Ok(())
    } else {
        Err(Error::Stale)
    }
}
fn validate_capture(
    home: &Path,
    receipt: &HomeSourceReceipt,
    capture: &HomeOAuthCapture,
) -> Result<()> {
    barrier(home)?;
    with_metadata(home, |g, source| {
        require_receipt(g, source, receipt)?;
        home_store::require_home_oauth_capture(g, source, &capture.original, &capture.identity)
    })
}
fn unseal(home: &Path, stored: &str, empty_allowed: bool) -> Result<Zeroizing<String>> {
    if let Some(cipher) = stored.strip_prefix("enc:v1:") {
        #[cfg(test)]
        {
            home_store::test_probe::crypto(home);
            home_store::test_probe::google_crypto(home);
        }
        crate::config_crypto::decrypt_value(cipher, home)
            .map(Zeroizing::new)
            .ok_or(Error::Io)
    } else if stored.starts_with("enc:") || (!empty_allowed && stored.is_empty()) {
        Err(Error::Invalid)
    } else {
        Ok(Zeroizing::new(stored.to_owned()))
    }
}
fn open_captured(
    home: &Path,
    receipt: &HomeSourceReceipt,
    capture: &HomeOAuthCapture,
    stored: &str,
    empty_allowed: bool,
) -> Result<Zeroizing<String>> {
    validate_capture(home, receipt, capture)?;
    let opened = unseal(home, stored, empty_allowed);
    #[cfg(test)]
    testing::boundary(home, 3);
    validate_capture(home, receipt, capture)?;
    let opened = opened?;
    if opened.len() > refresh_io::MAX_TOKEN
        || (!empty_allowed && opened.is_empty())
        || opened.chars().any(char::is_control)
        || opened.starts_with("enc:")
    {
        return Err(Error::Invalid);
    }
    Ok(opened)
}
pub(crate) fn validate_home_oauth_material(
    home: &Path,
    receipt: &HomeSourceReceipt,
    proof: &HomeOAuthMaterialProof,
    purpose_scopes: &[&str],
) -> Result<()> {
    // Pure metadata path: no lock-table/grant acquisition, crypto or issuance.
    barrier(home)?;
    let scopes = purpose(receipt, purpose_scopes)?;
    if proof.original != *receipt || proof.purpose_scopes != scopes {
        return Err(Error::Stale);
    }
    with_metadata(home, |g, source| {
        require_receipt(g, source, receipt)?;
        let actual = home_store::observe_home_oauth(g, source)?;
        if actual != proof.row
            || scopes.iter().any(|s| !actual.scopes.contains(s))
            || home_store::home_oauth_expired(actual.expires_at, home_store::host_time_now(g))
        {
            return Err(Error::Stale);
        }
        Ok(())
    })
}
pub(crate) async fn resolve_home_oauth_pinned(
    home: &Path,
    receipt: &HomeSourceReceipt,
    purpose_scopes: &[&str],
) -> Result<HomeOAuthResolved> {
    resolve_home_oauth_pinned_core(home, receipt, purpose_scopes, ResolveBudget::Legacy).await
}
/// Remote-only entry; a consumer cannot choose the legacy mode.
pub(crate) async fn resolve_home_oauth_pinned_mcp(
    home: &Path,
    receipt: &HomeSourceReceipt,
    purpose_scopes: &[&str],
    deadline: tokio::time::Instant,
) -> Result<HomeOAuthResolved> {
    resolve_home_oauth_pinned_core(
        home,
        receipt,
        purpose_scopes,
        ResolveBudget::remote(home, deadline),
    )
    .await
}
#[derive(Clone, Copy)]
enum ResolveBudget<'a> {
    Legacy,
    Remote(tokio::time::Instant, #[cfg(test)] &'a Path),
    Preflight(
        &'a crate::google_identity::effect_authority::GooglePreflightPermit<'a>,
        tokio::time::Instant,
    ),
}
impl<'a> ResolveBudget<'a> {
    fn remote(home: &'a Path, deadline: tokio::time::Instant) -> Self {
        #[cfg(test)]
        {
            Self::Remote(deadline, home)
        }
        #[cfg(not(test))]
        {
            let _ = home;
            Self::Remote(deadline)
        }
    }
    fn check(self) -> Result<()> {
        match self {
            Self::Legacy => Ok(()),
            Self::Remote(deadline, ..) => {
                #[cfg(test)]
                let now = match self {
                    Self::Remote(_, home) => testing::remote_now(home),
                    _ => unreachable!(),
                };
                #[cfg(not(test))]
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    Err(Error::Stale)
                } else {
                    Ok(())
                }
            }
            Self::Preflight(permit, deadline) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Stale);
                }
                permit.validate_oauth_resolution().map_err(|_| Error::Stale)
            }
        }
    }
    async fn wait<F: std::future::Future>(self, future: F) -> Result<F::Output> {
        self.check()?;
        let result = match self {
            Self::Legacy => future.await,
            Self::Remote(deadline, ..) | Self::Preflight(_, deadline) => {
                tokio::time::timeout_at(deadline, future)
                    .await
                    .map_err(|_| Error::Stale)?
            }
        };
        self.check()?;
        Ok(result)
    }
}

async fn resolve_home_oauth_pinned_core(
    home: &Path,
    receipt: &HomeSourceReceipt,
    purpose_scopes: &[&str],
    budget: ResolveBudget<'_>,
) -> Result<HomeOAuthResolved> {
    budget.check()?;
    let scopes = purpose(receipt, purpose_scopes)?;
    barrier(home)?;
    validate_home_source(home, receipt)?;
    let canonical = home.canonicalize().map_err(|_| Error::Io)?;
    let lock = refresh_lock(&canonical)?;
    let local = budget.wait(lock.lock()).await?;
    barrier(&canonical)?;
    // FIRST actual material/revision capture is after the local mutex await.
    // No SourceGuard or lower lock survives this synchronous closure.
    let capture = with_metadata(&canonical, |g, source| {
        require_receipt(g, source, receipt)?;
        let mut capture = home_store::capture_home_oauth(g, source)?;
        if home_store::home_oauth_expired(capture.row.expires_at, home_store::host_time_now(g)) {
            capture.client = Some(source.take_raw_client()?);
        }
        Ok(capture)
    })?;
    #[cfg(test)]
    testing::boundary(&canonical, 1);
    budget.check()?;
    let result = if home_store::home_oauth_expired(
        capture.row.expires_at,
        home_store::wall_time_now(&canonical),
    ) {
        let grant = budget
            .wait(crate::google_personal::grants::acquire(&canonical))
            .await?
            .map_err(|_| Error::Io)?;
        #[cfg(test)]
        testing::boundary(&canonical, 2);
        // Exact original raw bytes/identity after waiting; never recapture.
        let result = async {
            validate_capture(&canonical, receipt, &capture)?;
            let refreshed = match budget {
                ResolveBudget::Legacy => {
                    refresh_io::refresh_once(&canonical, receipt, &capture).await?
                }
                ResolveBudget::Remote(..) | ResolveBudget::Preflight(_, _) => {
                    refresh_io::refresh_once_budget(&canonical, receipt, &capture, budget).await?
                }
            };
            validate_capture(&canonical, receipt, &capture)?;
            let expected = home_store::expected_refresh_metadata(&capture.row, &refreshed.payload)?;
            budget.check()?;
            with_metadata(&canonical, |g, source| {
                require_receipt(g, source, receipt)?;
                budget.check()?;
                home_store::publish_home_refresh(
                    g,
                    source,
                    &capture.original,
                    &capture.identity,
                    refreshed.payload,
                )
            })?;
            #[cfg(test)]
            testing::boundary(&canonical, 10);
            // Unit producer confirmed Ok is required BEFORE any material.
            let row = expected.ok_or(Error::Stale)?;
            let material = HomeOAuthResolved {
                w3_expiry: None,
                access: refreshed.access,
                proof: HomeOAuthMaterialProof {
                    original: receipt.clone(),
                    purpose_scopes: scopes.clone(),
                    row,
                },
            };
            validate_home_oauth_material(&canonical, receipt, material.proof(), purpose_scopes)?;
            Ok(material)
        }
        .await;
        drop(grant);
        result
    } else {
        let access = open_captured(&canonical, receipt, &capture, &capture.access, false)?;
        budget.check()?;
        if !refresh_io::bearer(&access) {
            return Err(Error::Invalid);
        }
        let material = HomeOAuthResolved {
            w3_expiry: None,
            access,
            proof: HomeOAuthMaterialProof {
                original: receipt.clone(),
                purpose_scopes: scopes,
                row: capture.row.clone(),
            },
        };
        validate_home_oauth_material(&canonical, receipt, material.proof(), purpose_scopes)?;
        Ok(material)
    };
    drop(local);
    budget.check()?;
    result
}

/// Read the service-account section. This is the original ordinary adapter's
/// parser/helper and preserves configured-but-broken SA precedence.
fn service_account_config(
    home_dir: &Path,
) -> std::result::Result<Option<ServiceAccountConfig>, ServiceAccountError> {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return Ok(None);
    };
    google_service_account::parse_config(&raw, home_dir)
}
/// Existing public bare-token adapter, re-exported at google_workspace's
/// original path. Closed effect consumers must use C2 rather than this API.
pub async fn get_valid_google_token(
    home_dir: &Path,
) -> std::result::Result<String, GoogleAuthError> {
    if matches!(crate::google_workspace::selected_path(home_dir)?.as_str(), "apps_script" | "none") {
        return Err(GoogleAuthError::NotConnected { vault: home_dir.join(crate::mcp_oauth::TOKEN_FILE) });
    }
    match service_account_config(home_dir) {
        Ok(Some(cfg)) => {
            return google_service_account::get_token(&cfg, REQUIRED_SCOPES)
                .await
                .map_err(|e| GoogleAuthError::ServiceAccount(e.to_string()));
        }
        Ok(None) => {}
        Err(e) => return Err(GoogleAuthError::ServiceAccount(e.to_string())),
    }
    let failed = |error: Error| GoogleAuthError::RefreshFailed(error.to_string());
    barrier(home_dir).map_err(failed)?;
    // Presence/expiry only: no material or revision captured before mutex.
    let (present, expired, refresh, client) = with_metadata(home_dir, |g, source| {
        let (present, expired, refresh) = home_store::ordinary_home_oauth_presence(g, source)?;
        Ok((present, expired, refresh, source.raw_client.is_some()))
    })
    .map_err(failed)?;
    if !present {
        return Err(GoogleAuthError::NotConnected {
            vault: home_dir.join(crate::mcp_oauth::TOKEN_FILE),
        });
    }
    if expired && !refresh {
        return Err(GoogleAuthError::NoRefreshToken);
    }
    if expired && !client {
        return Err(GoogleAuthError::ClientConfigMissing);
    }
    let receipt = prepare_home_selection(home_dir, &[]).map_err(failed)?;
    let mut resolved = resolve_home_oauth_pinned(home_dir, &receipt, &[])
        .await
        .map_err(failed)?;
    Ok(std::mem::take(&mut *resolved.access))
}

#[cfg(test)]
#[path = "oauth_material_test_hooks.rs"]
mod testing;
#[cfg(test)]
#[path = "oauth_material_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) mod mcp_budget_testing {
    use super::*;
    pub(crate) fn install(home: &Path, endpoint: &str) -> impl Drop {
        refresh_io::testing::EndpointGuard::owned_loopback(home, endpoint)
    }
    fn fixture() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("config.toml"),
            "[integrations]\ngoogle_workspace=true\n",
        )
        .unwrap();
        let google_home = crate::mcp_oauth::google_home_test_fixture::current_control("test-admin");
        std::fs::write(directory.path().join(crate::mcp_oauth::TOKEN_FILE),serde_json::to_vec(&json!([{"provider_id":"google","access_token":"owned-expired","refresh_token":"owned-refresh","expires_at":"2000-01-01T00:00:00Z","scopes":["scope-a"],"google_home":google_home}])).unwrap()).unwrap();
        std::fs::write(directory.path().join(crate::mcp_oauth::CLIENT_CONFIG_FILE),serde_json::to_vec(&json!([{"provider_id":"google","client_id":"owned-client","client_secret":"owned-secret","auth_url":"https://accounts.google.com/o/oauth2/v2/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":["scope-a"]}])).unwrap()).unwrap();
        directory
    }
    #[tokio::test(start_paused = true)]
    async fn m1_home_sync_crypto_send_and_publication_recheck_the_original_deadline() {
        for phase in [3, 4, 8, 10] {
            let home = fixture();
            let receipt = prepare_home_selection(home.path(), &["scope-a"]).unwrap();
            let wire = refresh_io::testing::Wire::json(home.path(), None);
            let clock = testing::RemoteClockGuard::fixed(home.path());
            let elapsed = clock.elapsed_handle();
            let hook = testing::Guard::at(home.path(), phase, move || {
                elapsed.store(80, std::sync::atomic::Ordering::SeqCst);
            });
            let result = resolve_home_oauth_pinned_mcp(
                home.path(),
                &receipt,
                &["scope-a"],
                tokio::time::Instant::now() + std::time::Duration::from_millis(30),
            )
            .await;
            assert!(result.is_err());
            assert_eq!(hook.fired(), 1, "the intended phase must expire the original deadline");
            assert_eq!(
                wire.requests(),
                if phase == 3 || phase == 4 { 0 } else { 1 }
            );
        }
    }
    #[tokio::test]
    async fn m1_home_held_source_and_rotated_client_never_start_a_refresh_after_expiry_or_change() {
        for rotate in [false, true] {
            let home = fixture();
            let receipt = prepare_home_selection(home.path(), &["scope-a"]).unwrap();
            let wire = refresh_io::testing::Wire::json(home.path(), None);
            let guard = if rotate {
                let path = home.path().join(crate::mcp_oauth::CLIENT_CONFIG_FILE);
                Some(testing::Guard::at(home.path(), 4, move || {
                    let text = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(path, text.replace("owned-client", "rotated-client")).unwrap();
                }))
            } else {
                None
            };
            let holder = if rotate {
                None
            } else {
                let path = home.path().to_owned();
                let (tx, rx) = std::sync::mpsc::channel();
                let thread = std::thread::spawn(move || {
                    with_source_guard(&path, |_| {
                        tx.send(()).unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        Ok(())
                    })
                    .unwrap()
                });
                rx.recv().unwrap();
                Some(thread)
            };
            let result = resolve_home_oauth_pinned_mcp(
                home.path(),
                &receipt,
                &["scope-a"],
                tokio::time::Instant::now()
                    + std::time::Duration::from_millis(if rotate { 500 } else { 30 }),
            )
            .await;
            if let Some(holder) = holder {
                holder.join().unwrap();
            }
            drop(guard);
            assert!(result.is_err());
            assert_eq!(wire.requests(), 0);
        }
    }
}

impl HomeOAuthResolved {
    /// W3's original read-only snapshot validates source/row identity outside
    /// this owner; immutable wall and captured monotonic expiry are checked here, without locks.
    pub(crate) fn validate_w3_snapshot(
        &self,
    ) -> std::result::Result<(), crate::google_identity::Refusal> {
        let clock = W3ExpiryClock::now();
        if self
            .w3_expiry
            .as_ref()
            .is_some_and(|expiry| !expiry.live(clock))
            || home_store::home_oauth_expired(self.proof.row.expires_at, clock.wall)
        {
            Err(crate::google_identity::Refusal::hard(
                "identity_changed",
                "公司 Google 憑證已過期，請重新選擇。",
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn w3_weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.access.capacity()
            + self.proof.original.storage_weight()
            + self.proof.purpose_scopes.capacity() * std::mem::size_of::<String>()
            + self
                .proof
                .purpose_scopes
                .iter()
                .map(String::capacity)
                .sum::<usize>()
            + self.proof.row.w3_weight()
    }
}

#[cfg(test)]
pub(crate) use refresh_io::testing::EndpointGuard as W3RefreshEndpointGuard;

impl HomeOAuthResolved {
    pub(crate) fn validate_w3_control(
        &self,
        home: &Path,
    ) -> std::result::Result<(), crate::google_identity::Refusal> {
        self.validate_w3_snapshot()?;
        let control = home_store::closed_readonly_control(home).map_err(|_| {
            crate::google_identity::Refusal::hard("identity_changed", "公司 Google 來源已變更。")
        })?;
        if self.proof.row.w3_matches_control(&control) {
            Ok(())
        } else {
            Err(crate::google_identity::Refusal::hard(
                "identity_changed",
                "公司 Google 憑證版本已變更。",
            ))
        }
    }
}

pub(crate) async fn resolve_home_oauth_preflight<'a>(
    home: &Path,
    receipt: &HomeSourceReceipt,
    scopes: &[&str],
    deadline: tokio::time::Instant,
    permit: &'a crate::google_identity::effect_authority::GooglePreflightPermit<'a>,
) -> Result<HomeOAuthResolved> {
    let mut material = resolve_home_oauth_pinned_core(
        home,
        receipt,
        scopes,
        ResolveBudget::Preflight(permit, deadline),
    )
    .await?;
    // Capture only in the W3 owner entry. Legacy and M1 keep their original policy.
    material.w3_expiry =
        W3MaterialExpiry::capture(material.proof.row.expires_at, W3ExpiryClock::now())?;
    material.validate_w3_snapshot().map_err(|_| Error::Stale)?;
    Ok(material)
}

#[cfg(test)]
mod w3_oauth_expiry_oracles {
    use super::*;
    #[test]
    fn w3_oauth_short_ttl_uses_capture_wall_and_monotonic_clocks() {
        let mono = std::time::Instant::now();
        let created = W3ExpiryClock { wall: 1000, mono };
        let expiry = W3MaterialExpiry::capture(chrono::DateTime::from_timestamp(1300, 0), created)
            .unwrap()
            .unwrap();
        assert!(expiry.live(created));
        // A rollback still above creation cannot extend the captured 240-second lifetime.
        let rollback = W3ExpiryClock {
            wall: 1100,
            mono: mono + std::time::Duration::from_secs(400),
        };
        assert!(rollback.wall >= created.wall);
        assert!(!home_store::home_oauth_expired(
            chrono::DateTime::from_timestamp(1300, 0),
            rollback.wall
        ));
        assert!(!expiry.live(rollback));
        assert!(expiry.live(W3ExpiryClock {
            wall: 1100,
            mono: mono + std::time::Duration::from_secs(239)
        }));
        assert!(!expiry.live(W3ExpiryClock { wall: 1240, mono }));
        assert!(!expiry.live(W3ExpiryClock {
            wall: 1100,
            mono: mono + std::time::Duration::from_secs(240)
        }));
        assert!(
            W3MaterialExpiry::capture(chrono::DateTime::from_timestamp(1060, 0), created).is_err()
        );
        assert!(W3MaterialExpiry::capture(None, created).unwrap().is_none());
    }
}

#[derive(Clone, Copy)]
struct W3ExpiryClock {
    wall: u64,
    mono: std::time::Instant,
}
impl W3ExpiryClock {
    fn now() -> Self {
        Self {
            wall: crate::google_personal::authority::now(),
            mono: std::time::Instant::now(),
        }
    }
}
struct W3MaterialExpiry {
    wall: u64,
    mono: std::time::Instant,
}
impl W3MaterialExpiry {
    fn capture(
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
        clock: W3ExpiryClock,
    ) -> Result<Option<Self>> {
        let Some(expires_at) = expires_at else {
            return Ok(None);
        };
        // Floor fractional seconds and retain the original sixty-second grace.
        let wall = expires_at
            .timestamp()
            .checked_sub(60)
            .and_then(|v| u64::try_from(v).ok())
            .ok_or(Error::Stale)?;
        let remaining = wall
            .checked_sub(clock.wall)
            .filter(|v| *v != 0)
            .ok_or(Error::Stale)?;
        let mono = clock
            .mono
            .checked_add(std::time::Duration::from_secs(remaining))
            .ok_or(Error::Stale)?;
        Ok(Some(Self { wall, mono }))
    }
    fn live(&self, clock: W3ExpiryClock) -> bool {
        clock.wall < self.wall && clock.mono < self.mono
    }
}
#[cfg(test)]
impl HomeOAuthResolved {
    pub(crate) fn expire_w3_mono_for_test(&mut self) {
        self.w3_expiry
            .as_mut()
            .expect("expected captured W3 token expiry")
            .mono = std::time::Instant::now();
    }
}
