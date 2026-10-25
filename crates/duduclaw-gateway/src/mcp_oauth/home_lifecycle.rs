//! H1 private captured Home start producer (B1). Desired-client start uses
//! the existing Pending row and single publication gate. Public handler/map,
//! callback activation and the builtin template upgrade remain B2 work.
#[cfg(test)]
use super::McpOAuthConfig;
use super::home_store;
use crate::google_identity::home_source;
use home_store::{Error, GoogleConnectTicket, RawDigest, Result};
use serde_json::{Value, json};
use std::path::Path;

/// H1-owned encrypted material, not a raw client DTO returned to consumers.
/// It has no Debug/Clone/Serialize/Deserialize or public constructor.
pub(super) struct PreparedGoogleClient {
    raw: Value,
}
impl PreparedGoogleClient {
    pub(super) fn from_encrypted_raw(mut raw: Value) -> Result<Self> {
        home_source::validate_google_client_raw(&raw)?;
        const FIELDS: [&str; 7] = [
            "provider_id",
            "client_id",
            "client_secret",
            "auth_url",
            "token_url",
            "redirect_uri",
            "scopes",
        ];
        if raw
            .as_object()
            .ok_or(Error::Invalid)?
            .keys()
            .any(|k| !FIELDS.contains(&k.as_str()))
        {
            return Err(Error::Invalid);
        }
        let secret = raw["client_secret"].as_str().ok_or(Error::Invalid)?;
        if !secret.is_empty() && !secret.starts_with(super::ENC_PREFIX) {
            return Err(Error::Invalid);
        }
        let scopes: Vec<String> =
            serde_json::from_value(raw["scopes"].clone()).map_err(|_| Error::Invalid)?;
        raw["scopes"] = serde_json::to_value(home_store::normalized_scopes(scopes)?)
            .map_err(|_| Error::Invalid)?;
        Ok(Self { raw })
    }
}
/// Test-only W1 trusted producer seam. Production starts must supply the
/// captured B1 plan; this fixture seam never emits a URL or RAM pending.
/// Static validation precedes real crypto; failure has no plaintext fallback.
#[cfg(test)]
pub(crate) fn begin_google_home_connect(
    home: &Path,
    desired: &McpOAuthConfig,
    state_hash: RawDigest,
    expires_at: u64,
) -> Result<GoogleConnectTicket> {
    if desired.provider_id != "google" {
        return Err(Error::Invalid);
    }
    // Validate the original count before deduplication and use this exact
    // normalized scope set through preflight, crypto and the lower recheck.
    let scopes = home_store::normalized_scopes(desired.scopes.clone())?;
    let mut raw = json!({"provider_id":"google","client_id":desired.client_id,"client_secret":desired.client_secret,
        "auth_url":desired.auth_url,"token_url":desired.token_url,"redirect_uri":desired.redirect_uri,"scopes":scopes});
    home_source::validate_google_client_raw(&raw)?;
    let expected = home_source::with_metadata(home, |g, observed| {
        observed.preview_google_client(&raw)?;
        check_expiry(g, expires_at)?;
        ExpectedStart::capture(g, observed)
    })?;
    raw["client_secret"] = if desired.client_secret.is_empty() {
        json!("")
    } else {
        #[cfg(test)]
        home_store::test_probe::crypto(home);
        let encrypted =
            crate::config_crypto::encrypt_value(&desired.client_secret, home).ok_or(Error::Io)?;
        json!(format!("{}{encrypted}", super::ENC_PREFIX))
    };
    publish_expected_start(
        home,
        &expected,
        PreparedGoogleClient::from_encrypted_raw(raw)?,
        state_hash,
        expires_at,
    )
}
/// Test-only W1 encrypted metadata seam. It captures an exact original
/// snapshot and uses the same private publication core; not a production
/// no-snapshot bypass or a provider/callback success implementation.
#[cfg(test)]
pub(super) fn begin_prepared_google_home_connect(
    home: &Path,
    desired: PreparedGoogleClient,
    state_hash: RawDigest,
    expires_at: u64,
) -> Result<GoogleConnectTicket> {
    let expected = home_source::with_metadata(home, |g, source| {
        check_expiry(g, expires_at)?;
        ExpectedStart::capture(g, source)
    })?;
    publish_expected_start(home, &expected, desired, state_hash, expires_at)
}

use home_source::{ObservedSource, SourceBinding, SourceGuard};
use home_store::raw_array::RawArray;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// Host-owned request primitives only. No endpoint, scope, stamp or ticket can
/// enter from the RPC/model. Kept private until B2 wires the reviewed handler.
pub(crate) struct GoogleHomeStartInput {
    client_id: Option<String>,
    secret: Option<Zeroizing<String>>,
}
fn primitive(s: &str) -> Result<()> {
    if s.len() > 64 * 1024 || s.chars().any(char::is_control) {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
}
impl GoogleHomeStartInput {
    pub(crate) fn new(client_id: Option<&str>, secret: Option<&str>) -> Result<Self> {
        if let Some(s) = client_id {
            primitive(s)?;
        }
        if let Some(s) = secret {
            primitive(s)?;
        }
        Ok(Self {
            client_id: client_id.filter(|s| !s.is_empty()).map(str::to_owned),
            secret: secret
                .filter(|s| !s.is_empty())
                .map(|s| Zeroizing::new(s.to_owned())),
        })
    }
}
/// Original byte snapshots, never a replacement authority captured after an
/// await/crypto. All constructors run under source -> config -> client -> token.
struct ExpectedStart {
    binding: SourceBinding,
    config: Option<Zeroizing<Vec<u8>>>,
    clients: RawArray,
    tokens: home_store::StoreSnapshot,
}
impl ExpectedStart {
    fn capture(g: &SourceGuard, source: &ObservedSource) -> Result<Self> {
        Ok(Self {
            binding: source.binding().clone(),
            config: home_store::read_config_snapshot(g)?,
            clients: RawArray::read(&g.home().join(super::CLIENT_CONFIG_FILE))?,
            tokens: home_store::connect_start_snapshot(g)?,
        })
    }
    fn validate(&self, g: &SourceGuard, source: &ObservedSource) -> Result<()> {
        if self.binding != *source.binding()
            || self.config != home_store::read_config_snapshot(g)?
            || !self
                .clients
                .same_bytes(&RawArray::read(&g.home().join(super::CLIENT_CONFIG_FILE))?)
        {
            return Err(Error::Stale);
        }
        home_store::transaction(g, |r| r.require_snapshot(&self.tokens))
    }
}
fn check_expiry(g: &SourceGuard, expires_at: u64) -> Result<()> {
    let now = home_store::host_time_now(g);
    if expires_at <= now || expires_at > now.saturating_add(super::PENDING_TTL_SECS) {
        Err(Error::Stale)
    } else {
        Ok(())
    }
}
fn client_rows(original: &RawArray, desired: &Value) -> Result<Vec<Value>> {
    let mut rows = original.values().to_vec();
    let index = rows.iter().position(|r| r["provider_id"] == "google");
    let mut target = index
        .map(|i| rows[i].clone())
        .unwrap_or_else(|| json!({"provider_id":"google"}));
    for (key, value) in desired.as_object().ok_or(Error::Invalid)? {
        target[key] = value.clone();
    }
    if let Some(i) = index {
        rows[i] = target;
    } else {
        rows.push(target);
    }
    // Actual opaque emitter, preserving every unchanged row's internal bytes.
    original.render(&rows)?;
    Ok(rows)
}
fn publish_expected_start(
    home: &Path,
    expected: &ExpectedStart,
    desired: PreparedGoogleClient,
    state_hash: RawDigest,
    expires_at: u64,
) -> Result<GoogleConnectTicket> {
    let (start, final_clients) = home_source::with_metadata(home, |g, observed| {
        expected.validate(g, observed)?;
        check_expiry(g, expires_at)?;
        let rows = client_rows(&expected.clients, &desired.raw)?;
        let target = rows
            .iter()
            .find(|r| r["provider_id"] == "google")
            .ok_or(Error::Invalid)?;
        let binding = observed.preview_google_client(target)?;
        let bytes = expected.clients.render(&rows)?;
        let write = home_store::prepare_client_write(expected.clients.bytes(), bytes.to_vec())?;
        let now = home_store::host_time_now(g);
        let start = home_store::publication_transaction(g, |r| {
            r.require_snapshot(&expected.tokens)?;
            r.begin_connect_to(observed, binding, Some(write), state_hash, expires_at, now)
        })?;
        Ok((start, bytes))
    })?;
    #[cfg(test)]
    testing::between_publications(home);
    // Same StartConnect producer only: failure never starts/relabels a new one.
    home_source::with_metadata(home, |g, observed| {
        check_expiry(g, expires_at)?;
        if expected.config != home_store::read_config_snapshot(g)?
            || home_store::read_client_snapshot(g)?.as_deref() != Some(&*final_clients)
        {
            return Err(Error::Stale);
        }
        let now = home_store::host_time_now(g);
        home_store::publication_transaction(g, |r| r.publish_start(&start, observed, now))
    })
}

enum StartSecret {
    Plain(Zeroizing<String>),
    Stored(Zeroizing<String>),
}
/// No Debug/Clone/serialization or raw secret accessor. A plan may cross the
/// grant await, but never retains any short lock or observed-source capability.
pub(crate) struct GoogleHomeStartPlan {
    home: PathBuf,
    expected: ExpectedStart,
    metadata: Value,
    secret: StartSecret,
    state: String,
    state_hash: RawDigest,
    verifier: Zeroizing<String>,
    authorization_url: String,
    expires_at: u64,
    created_at: Instant,
}
pub(crate) struct GoogleHomePending {
    home: PathBuf,
    ticket: GoogleConnectTicket,
    state_hash: RawDigest,
    verifier: Zeroizing<String>,
    requested_scopes: Vec<String>,
    expires_at: u64,
    created_at: Instant,
}
impl GoogleHomePending {
    pub(crate) fn is_expired_in_ram(&self) -> bool {
        ram_elapsed(&self.home, self.created_at) >= Duration::from_secs(super::PENDING_TTL_SECS)
    }
    pub(crate) fn is_expired(&self) -> bool {
        self.is_expired_in_ram() || home_store::wall_time_now(&self.home) >= self.expires_at
    }
}
pub(crate) struct StartedGoogleHomeAuthorization {
    authorization_url: String,
    state: String,
    pending: GoogleHomePending,
}
impl StartedGoogleHomeAuthorization {
    /// B2 host seam only; Pending remains opaque, never a serialized response.
    pub(crate) fn into_response_parts(self) -> (String, String, GoogleHomePending) {
        (self.authorization_url, self.state, self.pending)
    }
}
fn secret_size(secret: &StartSecret) -> Result<usize> {
    match secret {
        StartSecret::Plain(s) => Ok(s.len()),
        StartSecret::Stored(s) => {
            if let Some(cipher) = s.strip_prefix(super::ENC_PREFIX) {
                use base64::Engine as _;
                let bytes = Zeroizing::new(
                    base64::engine::general_purpose::STANDARD
                        .decode(cipher)
                        .map_err(|_| Error::Invalid)?,
                );
                let len = bytes.len().checked_sub(28).ok_or(Error::Invalid)?;
                if len > 64 * 1024 {
                    return Err(Error::Invalid);
                }
                Ok(len)
            } else if s.starts_with("enc:") {
                Err(Error::Invalid)
            } else {
                primitive(s)?;
                Ok(s.len())
            }
        }
    }
}
/// Pure metadata capture and exact capacity projection; no key access, token
/// unseal, provider, gate, generation or ticket allocation happens here.
pub(crate) fn capture_google_home_start(
    home: &Path,
    input: GoogleHomeStartInput,
) -> Result<GoogleHomeStartPlan> {
    crate::google_personal::grants::check_metadata_barrier(home).map_err(|_| Error::NotReady)?;
    home_source::with_metadata(home, |g, source| {
        let expected = ExpectedStart::capture(g, source)?;
        let saved = expected
            .clients
            .values()
            .iter()
            .find(|v| v["provider_id"] == "google");
        let client_id = input
            .client_id
            .or_else(|| {
                saved
                    .and_then(|v| v["client_id"].as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .ok_or(Error::Invalid)?;
        primitive(&client_id)?;
        let secret = match input.secret {
            Some(s) => StartSecret::Plain(s),
            None => StartSecret::Stored(Zeroizing::new(
                saved
                    .and_then(|v| v["client_secret"].as_str())
                    .unwrap_or("")
                    .to_owned(),
            )),
        };
        let size = secret_size(&secret)?;
        let scopes = home_store::normalized_scopes(
            crate::google_workspace::REQUIRED_SCOPES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )?;
        let redirect = super::redirect_uri(g.home());
        let template = super::builtin_providers(&redirect)
            .into_iter()
            .find(|p| p.provider_id == "google")
            .ok_or(Error::Invalid)?;
        let metadata = json!({"provider_id":"google","client_id":client_id,"client_secret":"", "auth_url":template.auth_url,"token_url":template.token_url,"redirect_uri":redirect,"scopes":scopes});
        home_source::validate_google_client_raw(&metadata)?;
        let mut projection = metadata.clone();
        // Size-only placeholder, never authority or persisted payload. Strict
        // Google encryption has no plaintext fallback and this predicts its
        // exact base64 envelope size (including stored prefill plaintext).
        projection["client_secret"] = json!(super::encrypted_size_placeholder(&"A".repeat(size))?);
        home_source::validate_google_client_raw(&projection)?;
        client_rows(&expected.clients, &projection)?;
        let state = uuid::Uuid::new_v4().to_string();
        let state_hash = RawDigest::of(state.as_bytes());
        let (verifier, challenge) = super::generate_pkce();
        let mut url = url::Url::parse(metadata["auth_url"].as_str().ok_or(Error::Invalid)?)
            .map_err(|_| Error::Invalid)?;
        url.query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("response_type", "code")
            .append_pair("scope", &scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        let expires_at = home_store::host_time_now(g)
            .checked_add(super::PENDING_TTL_SECS)
            .ok_or(Error::Invalid)?;
        Ok(GoogleHomeStartPlan {
            home: g.home().to_path_buf(),
            expected,
            metadata,
            secret,
            state,
            state_hash,
            verifier: Zeroizing::new(verifier),
            authorization_url: url.into(),
            expires_at,
            created_at: Instant::now(),
        })
    })
}
fn validate_plan(home: &Path, plan: &GoogleHomeStartPlan) -> Result<()> {
    if std::fs::canonicalize(home).map_err(|_| Error::Io)? != plan.home
        || plan.created_at.elapsed() >= Duration::from_secs(super::PENDING_TTL_SECS)
    {
        return Err(Error::Stale);
    }
    crate::google_personal::grants::check_metadata_barrier(home).map_err(|_| Error::NotReady)?;
    home_source::with_metadata(home, |g, source| {
        check_expiry(g, plan.expires_at)?;
        plan.expected.validate(g, source)
    })
}
/// Private B1 producer, not called by the public handler until B2. Await owns
/// only the existing global grant, no source/config/client/token short lock.
pub(crate) async fn begin_google_home_authorization(
    home: &Path,
    plan: GoogleHomeStartPlan,
) -> Result<StartedGoogleHomeAuthorization> {
    validate_plan(home, &plan)?;
    let pinned_home = plan.home.clone();
    let home = pinned_home.as_path();
    let grant = crate::google_personal::grants::acquire(home)
        .await
        .map_err(|_| Error::NotReady)?;
    validate_plan(home, &plan)?;
    let plain = match &plan.secret {
        StartSecret::Plain(s) => Zeroizing::new(s.to_string()),
        StartSecret::Stored(s) => {
            Zeroizing::new(super::decode_secret(home, "google", s).ok_or(Error::Io)?)
        }
    };
    primitive(&plain)?;
    let mut raw = plan.metadata.clone();
    raw["client_secret"] = if plain.is_empty() {
        json!("")
    } else {
        #[cfg(test)]
        {
            home_store::test_probe::crypto(home);
            home_store::test_probe::google_crypto(home);
        }
        let encrypted = crate::config_crypto::encrypt_value(&plain, home).ok_or(Error::Io)?;
        json!(format!("{}{encrypted}", super::ENC_PREFIX))
    };
    drop(plain);
    #[cfg(test)]
    home_store::test_probe::preparation_boundary(home);
    validate_plan(home, &plan)?;
    let desired = PreparedGoogleClient::from_encrypted_raw(raw)?;
    let ticket = publish_expected_start(
        home,
        &plan.expected,
        desired,
        plan.state_hash.clone(),
        plan.expires_at,
    )?;
    let requested_scopes =
        serde_json::from_value(plan.metadata["scopes"].clone()).map_err(|_| Error::Invalid)?;
    let pending = GoogleHomePending {
        home: plan.home,
        ticket,
        state_hash: plan.state_hash,
        verifier: plan.verifier,
        requested_scopes,
        expires_at: plan.expires_at,
        created_at: plan.created_at,
    };
    validate_google_home_pending(home, &pending)?;
    drop(grant);
    Ok(StartedGoogleHomeAuthorization {
        authorization_url: plan.authorization_url,
        state: plan.state,
        pending,
    })
}
pub(crate) fn validate_google_home_pending(home: &Path, pending: &GoogleHomePending) -> Result<()> {
    validate_google_home_pending_host(home, pending)?;
    crate::google_personal::grants::check_metadata_barrier(home).map_err(|_| Error::NotReady)?;
    validate_google_home_pending_store(home, pending)
}
/// Pure same-ticket metadata before a bounded grant wait, never permission to
/// claim or exchange. Full validation remains mandatory after that await.
fn validate_google_home_pending_metadata(home: &Path, pending: &GoogleHomePending) -> Result<()> {
    validate_google_home_pending_host(home, pending)?;
    validate_google_home_pending_store(home, pending)
}
fn validate_google_home_pending_host(home: &Path, pending: &GoogleHomePending) -> Result<()> {
    if pending.is_expired_in_ram()
        || std::fs::canonicalize(home).map_err(|_| Error::Io)? != pending.home
    {
        return Err(Error::Stale);
    }
    Ok(())
}
fn validate_google_home_pending_store(home: &Path, pending: &GoogleHomePending) -> Result<()> {
    home_source::with_metadata(home, |g, source| {
        check_expiry(g, pending.expires_at)?;
        home_store::transaction(g, |r| {
            r.validate_pending_connect(
                &pending.ticket,
                source,
                &pending.state_hash,
                home_store::host_time_now(g),
            )
        })
    })
}
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    thread_local! {static BETWEEN:std::cell::RefCell<Option<(PathBuf,String,Box<dyn FnOnce()>)>>=const {std::cell::RefCell::new(None)};}
    pub(crate) struct BetweenGuard {
        id: String,
    }
    impl BetweenGuard {
        pub(crate) fn at(home: &Path, hook: impl FnOnce() + 'static) -> Self {
            let id = uuid::Uuid::new_v4().to_string();
            BETWEEN.with(|s| {
                assert!(s.borrow().is_none());
                *s.borrow_mut() = Some((home.canonicalize().unwrap(), id.clone(), Box::new(hook)));
            });
            Self { id }
        }
    }
    impl Drop for BetweenGuard {
        fn drop(&mut self) {
            BETWEEN.with(|s| {
                if s.borrow().as_ref().is_some_and(|(_, id, _)| id == &self.id) {
                    s.borrow_mut().take();
                }
            });
        }
    }
    pub(super) fn between_publications(home: &Path) {
        let hook = BETWEEN.with(|s| {
            if s.borrow().as_ref().is_some_and(|(h, _, _)| h == home) {
                s.borrow_mut().take().map(|(_, _, f)| f)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook();
        }
    }
    thread_local! {static FINISH:std::cell::RefCell<Option<(PathBuf,String,i32,Box<dyn FnOnce()>)>>=const{std::cell::RefCell::new(None)};}
    pub(crate) struct FinishBoundaryGuard {
        id: String,
        fired: std::rc::Rc<std::cell::Cell<bool>>,
    }
    impl FinishBoundaryGuard {
        pub(crate) fn at(home: &Path, phase: i32, hook: impl FnOnce() + 'static) -> Self {
            let id = uuid::Uuid::new_v4().to_string();
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = fired.clone();
            FINISH.with(|s| {
                assert!(s.borrow().is_none());
                *s.borrow_mut() = Some((
                    home.canonicalize().unwrap(),
                    id.clone(),
                    phase,
                    Box::new(move || {
                        observed.set(true);
                        hook();
                    }),
                ));
            });
            Self { id, fired }
        }
        pub(crate) fn triggered(&self) -> bool {
            self.fired.get()
        }
    }
    impl Drop for FinishBoundaryGuard {
        fn drop(&mut self) {
            FINISH.with(|s| {
                if s.borrow()
                    .as_ref()
                    .is_some_and(|(_, id, _, _)| id == &self.id)
                {
                    s.borrow_mut().take();
                }
            });
        }
    }
    pub(crate) fn finish_boundary(home: &Path, phase: i32) {
        let hook = FINISH.with(|s| {
            if s.borrow()
                .as_ref()
                .is_some_and(|(h, _, p, _)| h == home && *p == phase)
            {
                s.borrow_mut().take().map(|(_, _, _, f)| f)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook();
        }
    }
    pub(crate) fn pkce_matches(start: &StartedGoogleHomeAuthorization) -> bool {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};
        let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(start.pending.verifier.as_bytes()));
        let url = url::Url::parse(&start.authorization_url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        query.get("code_challenge") == Some(&hash)
            && query.get("scope") == Some(&start.pending.requested_scopes.join(" "))
            && start.pending.state_hash == RawDigest::of(start.state.as_bytes())
    }
    fn ram_offsets() -> &'static std::sync::Mutex<
        std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicU64>>,
    > {
        static OFFSETS: std::sync::OnceLock<
            std::sync::Mutex<
                std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicU64>>,
            >,
        > = std::sync::OnceLock::new();
        OFFSETS.get_or_init(Default::default)
    }
    pub(crate) struct RamClockGuard {
        home: PathBuf,
        elapsed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    }
    impl RamClockGuard {
        pub(crate) fn at(home: &Path) -> Self {
            let home = home.canonicalize().unwrap();
            let elapsed = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            assert!(
                ram_offsets()
                    .lock()
                    .unwrap()
                    .insert(home.clone(), elapsed.clone())
                    .is_none()
            );
            Self { home, elapsed }
        }
        pub(crate) fn elapsed_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
            self.elapsed.clone()
        }
    }
    impl Drop for RamClockGuard {
        fn drop(&mut self) {
            let mut offsets = ram_offsets().lock().unwrap();
            if offsets
                .get(&self.home)
                .is_some_and(|v| std::sync::Arc::ptr_eq(v, &self.elapsed))
            {
                offsets.remove(&self.home);
            }
        }
    }
    pub(super) fn ram_elapsed_offset(home: &Path) -> Duration {
        let offset = ram_offsets()
            .lock()
            .unwrap()
            .get(home)
            .map(|v| v.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(0);
        Duration::from_secs(offset)
    }
    pub(crate) fn ram_created_at_for_diagnostics(pending: &GoogleHomePending) -> Instant {
        pending.created_at
    }
    pub(crate) fn age_ram(pending: &mut GoogleHomePending, age: Duration) {
        pending.created_at -= age;
    }
}

/// Callback-only RAM authority. The original start Instant and durable wall
/// expiry move from Pending unchanged; claim never starts a new timer. No
/// serialization, Clone, clock setter, raw client or ticket constructor.
pub(crate) struct GoogleHomeCallbackContext {
    claim: home_store::ClaimedGoogleConnect,
    created_at: Instant,
    expires_at: u64,
}
impl GoogleHomeCallbackContext {
    pub(crate) fn durable(&self) -> &home_store::ClaimedGoogleConnect {
        &self.claim
    }
    pub(crate) fn validate_deadlines(&self, home: &Path) -> Result<()> {
        if ram_elapsed(home, self.created_at) >= Duration::from_secs(super::PENDING_TTL_SECS)
            || home_store::wall_time_now(home) >= self.expires_at
        {
            Err(Error::Stale)
        } else {
            Ok(())
        }
    }
}
fn ram_elapsed(home: &Path, created_at: Instant) -> Duration {
    let elapsed = created_at.elapsed();
    #[cfg(test)]
    return elapsed.saturating_add(testing::ram_elapsed_offset(home));
    #[cfg(not(test))]
    {
        let _ = home;
        elapsed
    }
}
/// Only the callback path adds deadline checks to the existing publication
/// protocol. No extra durable authority or gate; unchanged producers still
/// validate their original plan/source/ticket and bytes.
fn callback_publication<T>(
    home: &Path,
    g: &SourceGuard,
    context: &GoogleHomeCallbackContext,
    _stage: i32,
    f: impl FnOnce(&mut home_store::Rows) -> Result<home_store::Planned<T>>,
) -> Result<T> {
    #[cfg(test)]
    let checks = std::cell::Cell::new(0);
    home_store::publication_transaction_checked(
        g,
        || {
            #[cfg(test)]
            {
                checks.set(checks.get() + 1);
                testing::finish_boundary(home, _stage * 100 + checks.get());
            }
            context.validate_deadlines(home)
        },
        f,
    )
}
/// Pure same-claim revalidation: no key, token refresh, provider or recapture.
pub(crate) fn validate_google_home_claim(
    home: &Path,
    claim: &GoogleHomeCallbackContext,
) -> Result<()> {
    claim.validate_deadlines(home)?;
    crate::google_personal::grants::check_metadata_barrier(home).map_err(|_| Error::NotReady)?;
    home_source::with_metadata(home, |g, source| {
        home_store::transaction(g, |r| {
            claim.validate_deadlines(home)?;
            r.validate_claim(claim.durable(), source, home_store::host_time_now(g))
        })
    })
}
/// Callback consumes the original opaque RAM flow. Grant spans the durable
/// claim, exactly one provider request, and all three same-ticket finish points.
pub(crate) async fn complete_google_home_authorization(
    home: &Path,
    pending: GoogleHomePending,
    returned_state: &str,
    code: Zeroizing<String>,
) -> Result<()> {
    if pending.state_hash != RawDigest::of(returned_state.as_bytes())
        || code.is_empty()
        || code.len() > 16 * 1024
        || code.chars().any(char::is_control)
    {
        return Err(Error::Invalid);
    }
    // An in-flight revoke owns this same grant lock. Wait within the existing
    // bound; a persisted/cancelled marker is still refused by acquire and by
    // full same-pending validation immediately after the await.
    validate_google_home_pending_metadata(home, &pending)?;
    let pinned = pending.home.clone();
    let home = pinned.as_path();
    let grant = crate::google_personal::grants::acquire(home).await;
    validate_google_home_pending(home, &pending)?;
    let grant = grant.map_err(|_| Error::NotReady)?;
    let claim = home_source::with_metadata(home, |g, source| {
        let now = home_store::host_time_now(g);
        home_store::publication_transaction_checked(
            g,
            || {
                if pending.is_expired() {
                    Err(Error::Stale)
                } else {
                    Ok(())
                }
            },
            |r| {
                r.validate_pending_connect(&pending.ticket, source, &pending.state_hash, now)?;
                r.claim(&pending.ticket, source, now)
            },
        )
    })?;
    let claim = GoogleHomeCallbackContext {
        claim,
        created_at: pending.created_at,
        expires_at: pending.expires_at,
    };
    validate_google_home_claim(home, &claim)?;
    let candidate = home_source::exchange_claimed_google_code_once(
        home,
        &claim,
        &code,
        &pending.verifier,
        &pending.requested_scopes,
    )
    .await;
    validate_google_home_claim(home, &claim)?;
    let candidate = candidate?;
    finish_claimed_callback(home, &claim, candidate)?;
    drop(grant);
    Ok(())
}
fn finish_claimed_callback(
    home: &Path,
    claim: &GoogleHomeCallbackContext,
    candidate: home_store::PreparedGoogleCallback,
) -> Result<()> {
    #[cfg(test)]
    testing::finish_boundary(home, 1);
    claim.validate_deadlines(home)?;
    let (change, write) = home_source::with_metadata(home, |g, source| {
        let before = home_store::read_config_snapshot(g)?;
        let current = match &before {
            Some(b) => std::str::from_utf8(b).map_err(|_| Error::Invalid)?,
            None => "",
        };
        let table: toml::Table = current.parse().map_err(|_| Error::Invalid)?;
        let already = table
            .get("integrations")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("google_workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true);
        let next = Zeroizing::new(if already {
            current.to_owned()
        } else {
            let mut doc: toml_edit::DocumentMut = current.parse().map_err(|_| Error::Invalid)?;
            doc.as_table_mut()
                .entry("integrations")
                .or_insert(toml_edit::table())
                .as_table_mut()
                .ok_or(Error::Invalid)?
                .insert("google_workspace", toml_edit::value(true));
            doc.to_string()
        });
        crate::product_policy::validate_write(
            g.home(),
            &g.home().join("config.toml"),
            current,
            &next,
        )
        .map_err(|_| Error::Invalid)?;
        let write = if already {
            None
        } else {
            Some(home_store::prepare_config_write(
                before.as_ref().map(|v| v.as_slice()),
                next.as_bytes().to_vec(),
            )?)
        };
        let now = home_store::host_time_now(g);
        let change = callback_publication(home, g, claim, 1, |r| {
            candidate.require_original(r)?;
            r.begin_claimed_finish(
                claim.durable(),
                source,
                candidate.payload(),
                write.as_ref(),
                now,
            )
        })?;
        Ok((change, write))
    })?;
    #[cfg(test)]
    testing::finish_boundary(home, 11);
    claim.validate_deadlines(home)?;
    #[cfg(test)]
    testing::finish_boundary(home, 2);
    claim.validate_deadlines(home)?;
    home_source::with_metadata(home, |g, source| {
        let now = home_store::host_time_now(g);
        callback_publication(home, g, claim, 2, |r| {
            r.publish_claimed_payload(
                &change,
                claim.durable(),
                source,
                candidate.into_payload(),
                write,
                now,
            )
        })
    })?;
    #[cfg(test)]
    testing::finish_boundary(home, 12);
    claim.validate_deadlines(home)?;
    #[cfg(test)]
    testing::finish_boundary(home, 3);
    claim.validate_deadlines(home)?;
    home_source::with_metadata(home, |g, source| {
        let now = home_store::host_time_now(g);
        callback_publication(home, g, claim, 3, |r| r.finish_change(&change, source, now))
    })?;
    #[cfg(test)]
    testing::finish_boundary(home, 13);
    claim.validate_deadlines(home)
}
