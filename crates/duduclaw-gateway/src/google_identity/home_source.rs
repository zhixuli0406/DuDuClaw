//! H1 metadata authority. Ordinary selection/validation performs no unseal or provider.
//! Private lexical I/O children open only original claimed/captured material.
//! C1 retains its OAuth receipt API. W3 uses distinct private SA/Bridge
//! observations and credential owners, without synthetic OAuth identifiers.
use crate::mcp_oauth::home_store::{self, Error, RawDigest, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

#[path = "home_source/home_callback_io.rs"]
mod home_callback_io;
#[path = "home_source/native_selection.rs"]
mod native_selection;
#[path = "home_source/oauth_material.rs"]
mod oauth_material;
pub(crate) use home_callback_io::exchange_claimed_google_code_once;
#[cfg(test)]
pub(crate) use home_callback_io::testing as callback_testing;
pub(crate) use native_selection::{
    NativeHomeSelection, prepare_home_native_selection, resolve_home_native_pinned,
    validate_home_native_material, validate_home_native_selection,
};
pub use oauth_material::get_valid_google_token;
pub(crate) use oauth_material::{
    HomeOAuthBearer, HomeOAuthCapture, HomeOAuthMaterialProof, HomeOAuthResolved,
    capture_from_store, resolve_home_oauth_pinned, resolve_home_oauth_pinned_mcp,
    validate_home_oauth_material,
};

/// A short, cross-process source lock, never held across network/grant awaits.
/// The Rc marker deliberately makes the guard neither Send nor Sync.
pub(crate) struct SourceGuard {
    home: PathBuf,
    _lock: File,
    _not_send: PhantomData<Rc<()>>,
}
impl SourceGuard {
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }
}
pub(crate) fn with_source_guard<T>(
    home: &Path,
    f: impl FnOnce(&SourceGuard) -> Result<T>,
) -> Result<T> {
    let home = home.canonicalize().map_err(|_| Error::Io)?;
    if !home.is_dir() {
        return Err(Error::Invalid);
    }
    let path = home.join(".google-home-source.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => return Err(Error::Invalid),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error::Io),
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|_| Error::Io)?;
    file.lock_exclusive().map_err(|_| Error::Io)?;
    f(&SourceGuard {
        home,
        _lock: file,
        _not_send: PhantomData,
    })
}
fn locked<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    duduclaw_core::with_file_lock(path, || Ok(f())).map_err(|_| Error::Io)?
}
/// All lower store operations run source -> config -> client -> token.
/// Already-config-locked callers must be adapted at their outer entry point;
/// this helper must not be called recursively from config_commit's render.
pub(crate) fn with_metadata<T>(
    home: &Path,
    f: impl FnOnce(&SourceGuard, &mut ObservedSource) -> Result<T>,
) -> Result<T> {
    with_source_guard(home, |g| {
        // Every ordinary metadata closure is gated before reading config or
        // client metadata. SourceGuard serializes every gate writer; token
        // transactions recheck under their own token lock as well.
        home_store::ensure_publication_unblocked(g)?;
        locked_metadata(g, f)
    })
}
fn locked_metadata<T>(
    g: &SourceGuard,
    f: impl FnOnce(&SourceGuard, &mut ObservedSource) -> Result<T>,
) -> Result<T> {
    locked(&g.home.join("config.toml"), || {
        locked(
            &g.home.join(super::super::mcp_oauth::CLIENT_CONFIG_FILE),
            || {
                let mut source = ObservedSource::read(g)?;
                f(g, &mut source)
            },
        )
    })
}
/// H1 explicit recovery only. Its caller must hold an opaque inspection of
/// the exact gate, verify that gate under the token lock, and cannot issue a
/// receipt/claim or replay a provider. Ordinary readers use with_metadata.
pub(crate) fn with_exact_publication_recovery<T>(
    home: &Path,
    _inspection: &home_store::PublicationInspection,
    f: impl FnOnce(&SourceGuard, &mut ObservedSource) -> Result<T>,
) -> Result<T> {
    with_source_guard(home, |g| locked_metadata(g, f))
}
/// Called under the existing source/config/client/token locks at each
/// publication boundary; no new source is selected or returned.
pub(crate) fn revalidate_binding(g: &SourceGuard, expected: &SourceBinding) -> Result<()> {
    if ObservedSource::read(g)?.binding() == expected {
        Ok(())
    } else {
        Err(Error::Stale)
    }
}

/// Only the security-relevant Google/resolver projection participates in
/// source rotation; unrelated full-config updates preserve Google authority.
pub(crate) fn config_projection(raw: &str) -> Result<RawDigest> {
    let table: toml::Table = toml::from_str(raw).map_err(|_| Error::Invalid)?;
    let empty = toml::Table::new();
    let integrations = match table.get("integrations") {
        Some(v) => v.as_table().ok_or(Error::Invalid)?,
        None => &empty,
    };
    let master = match integrations.get("google_workspace") {
        None => false,
        Some(toml::Value::Boolean(v)) => *v,
        _ => return Err(Error::Invalid),
    };
    let bytes=zeroize::Zeroizing::new(serde_json::to_vec(&json!({"path":integrations.get("google_credential_path"),"master":master,"sa":integrations.get("google_service_account"),"bridge":integrations.get("google_apps_script"),"resolver":table.get("secret_manager")})).map_err(|_|Error::Invalid)?);
    Ok(RawDigest::of(&bytes))
}
/// Caller owns source then config; acquire ONLY client then token here.
/// Never calls the higher config_commit layer or reacquires source/config.
pub(crate) fn commit_google_config_under_guard(
    g: &SourceGuard,
    current: &str,
    next: &str,
) -> Result<()> {
    home_store::ensure_publication_unblocked(g)?;
    if current == next {
        return Ok(());
    }
    let before = home_store::read_config_snapshot(g)?;
    if before
        .as_ref()
        .map_or("", |b| std::str::from_utf8(b).unwrap_or("\0"))
        != current
    {
        return Err(Error::Stale);
    }
    let write = home_store::prepare_config_write(
        before.as_ref().map(|b| b.as_slice()),
        next.as_bytes().to_vec(),
    )?;
    if config_projection(current)? == config_projection(next)? {
        return home_store::publish_unrelated_source(g, write);
    }
    locked(
        &g.home.join(super::super::mcp_oauth::CLIENT_CONFIG_FILE),
        || {
            let source = ObservedSource::read(g)?;
            let target = ObservedSource::read_config(g, Some(next))?;
            let change = home_store::publication_transaction(g, |r| {
                let made = r.fence_source_change(&source, target.binding().clone(), Some(write))?;
                #[cfg(test)]
                home_store::record_source_fence(g);
                Ok(made)
            })?;
            let after = ObservedSource::read(g)?;
            home_store::publication_transaction(g, |r| {
                r.finish_change(&change, &after, home_store::host_time_now(g))
            })
        },
    )
}

fn bounded(path: &Path, required: bool) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Io),
    };
    let metadata = file.metadata().map_err(|_| Error::Io)?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(Error::Invalid);
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    std::io::Read::by_ref(&mut file)
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(Error::Invalid);
    }
    Ok(Some(bytes))
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceBinding {
    #[serde(rename = "canonical_home")]
    home: RawDigest,
    #[serde(rename = "client_stamp")]
    client: Option<RawDigest>,
    #[serde(rename = "source_stamp")]
    source: RawDigest,
}
impl SourceBinding {
    pub(crate) fn home_stamp(&self) -> &RawDigest {
        &self.home
    }
    pub(crate) fn client_stamp(&self) -> Option<&RawDigest> {
        self.client.as_ref()
    }
    pub(crate) fn source_stamp(&self) -> &RawDigest {
        &self.source
    }
}
#[derive(Clone, PartialEq, Eq, Serialize)]
struct SourceStamp {
    configuration: RawDigest,
    resolver: RawDigest,
    sa_key: Option<(RawDigest, RawDigest)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HomeBackend {
    OAuth,
    ServiceAccount,
    Bridge,
}
/// An actual locked metadata observation. No public/Deserialize constructor.
/// The raw client is confined to a non-serializable claimed exchange capsule.
pub(crate) struct RawClientSnapshot {
    _raw: Value,
}

pub(crate) struct ObservedSource {
    binding: SourceBinding,
    enabled_binding: SourceBinding,
    oauth_binding: SourceBinding,
    disabled_binding: SourceBinding,
    stamp: SourceStamp,
    master: bool,
    sa_present: bool,
    bridge_present: bool,
    disabled_path: bool,
    raw_client: Option<Value>,
}
/// Static raw-client validation only: no decrypt/protected key/provider.
pub(crate) fn validate_google_client_raw(c: &Value) -> Result<()> {
    if c["provider_id"] != "google" || !c.is_object() {
        return Err(Error::Invalid);
    }
    for field in [
        "client_id",
        "client_secret",
        "auth_url",
        "token_url",
        "redirect_uri",
    ] {
        let value = c[field].as_str().ok_or(Error::Invalid)?;
        if value.len() > 64 * 1024
            || value.chars().any(char::is_control)
            || (field != "client_secret" && value.trim().is_empty())
        {
            return Err(Error::Invalid);
        }
    }
    for field in ["auth_url", "token_url", "redirect_uri"] {
        url::Url::parse(c[field].as_str().ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)?;
    }
    let _: Vec<String> = serde_json::from_value(c["scopes"].clone()).map_err(|_| Error::Invalid)?;
    Ok(())
}
impl ObservedSource {
    fn read(g: &SourceGuard) -> Result<Self> {
        Self::read_config(g, None)
    }
    fn read_config(g: &SourceGuard, override_text: Option<&str>) -> Result<Self> {
        let bytes = bounded(&g.home.join("config.toml"), false)?;
        let raw = match override_text {
            Some(raw) => raw,
            None => match &bytes {
                Some(bytes) => std::str::from_utf8(bytes).map_err(|_| Error::Invalid)?,
                None => "",
            },
        };
        let table: toml::Table = toml::from_str(raw).map_err(|_| Error::Invalid)?;
        let empty = toml::Table::new();
        let integrations = match table.get("integrations") {
            Some(v) => v.as_table().ok_or(Error::Invalid)?,
            None => &empty,
        };
        let master = match integrations.get("google_workspace") {
            None => false,
            Some(toml::Value::Boolean(b)) => *b,
            _ => return Err(Error::Invalid),
        };
        let disabled_path = match integrations.get("google_credential_path") {
            None => false,
            Some(toml::Value::String(v)) if matches!(v.as_str(), "oauth" | "service_account" | "apps_script" | "none") => v == "none",
            _ => return Err(Error::Invalid),
        };
        let sa_present = integrations.contains_key("google_service_account");
        let bridge_present = integrations.contains_key("google_apps_script");
        if let Some(mode) = integrations.get("google_credential_path").and_then(toml::Value::as_str) {
            if (mode == "service_account" && (!sa_present || bridge_present))
                || (mode == "apps_script" && (!bridge_present || sa_present))
                || (matches!(mode, "oauth" | "none") && (sa_present || bridge_present)) {
                return Err(Error::Invalid);
            }
        }
        if sa_present && !integrations["google_service_account"].is_table() {
            return Err(Error::Invalid);
        }
        if bridge_present && !integrations["google_apps_script"].is_table() {
            return Err(Error::Invalid);
        }
        if table.get("secret_manager").is_some_and(|v| !v.is_table()) {
            return Err(Error::Invalid);
        }
        let sa_key = if sa_present {
            let cfg = crate::google_service_account::parse_config(raw, &g.home)
                .map_err(|_| Error::Invalid)?
                .ok_or(Error::Invalid)?;
            let path = cfg.key_file.canonicalize().map_err(|_| Error::Invalid)?;
            let bytes = bounded(&path, true)?.ok_or(Error::Invalid)?;
            Some((
                RawDigest::of(path.as_os_str().as_encoded_bytes()),
                RawDigest::of(&bytes),
            ))
        } else {
            None
        };
        if bridge_present {
            crate::google_apps_script::parse_bridge_section(raw)
                .map_err(|_| Error::Invalid)?
                .ok_or(Error::Invalid)?;
        }
        let projection = json!({"google_credential_path":integrations.get("google_credential_path"),"google_workspace":master,"google_service_account":integrations.get("google_service_account"),"google_apps_script":integrations.get("google_apps_script")});
        let resolver = RawDigest::of(
            &serde_json::to_vec(&table.get("secret_manager")).map_err(|_| Error::Invalid)?,
        );
        let stamp = SourceStamp {
            configuration: RawDigest::of(
                &serde_json::to_vec(&projection).map_err(|_| Error::Invalid)?,
            ),
            resolver,
            sa_key,
        };
        let mut enabled_projection = projection.clone();
        enabled_projection["google_workspace"] = json!(true);
        let enabled_stamp = SourceStamp {
            configuration: RawDigest::of(
                &serde_json::to_vec(&enabled_projection).map_err(|_| Error::Invalid)?,
            ),
            ..stamp.clone()
        };
        // Only callback activation selects OAuth and retires the alternative.
        enabled_projection["google_credential_path"] = json!("oauth");
        enabled_projection["google_service_account"] = Value::Null;
        enabled_projection["google_apps_script"] = Value::Null;
        let oauth_stamp = SourceStamp {
            configuration: RawDigest::of(
                &serde_json::to_vec(&enabled_projection).map_err(|_| Error::Invalid)?,
            ),
            resolver: stamp.resolver.clone(),
            sa_key: None,
        };
        let mut disabled_projection = projection;
        disabled_projection["google_workspace"] = json!(false);
        let disabled_stamp = SourceStamp {
            configuration: RawDigest::of(
                &serde_json::to_vec(&disabled_projection).map_err(|_| Error::Invalid)?,
            ),
            ..stamp.clone()
        };
        let raw_client = if let Some(bytes) = bounded(
            &g.home.join(super::super::mcp_oauth::CLIENT_CONFIG_FILE),
            false,
        )? {
            let clients: Vec<Value> = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
            if clients.len() > 512 {
                return Err(Error::Invalid);
            }
            let google: Vec<_> = clients
                .into_iter()
                .filter(|r| r["provider_id"] == "google")
                .collect();
            if google.len() > 1 {
                return Err(Error::Invalid);
            }
            let client = google.into_iter().next();
            if let Some(c) = &client {
                validate_google_client_raw(c)?;
            }
            client
        } else {
            None
        };
        let client = raw_client
            .as_ref()
            .map(|r| serde_json::to_vec(r).map(|b| RawDigest::of(&b)))
            .transpose()
            .map_err(|_| Error::Invalid)?;
        let binding = SourceBinding {
            home: RawDigest::of(g.home.as_os_str().as_encoded_bytes()),
            client,
            source: RawDigest::of(&serde_json::to_vec(&stamp).map_err(|_| Error::Invalid)?),
        };
        let oauth_binding = SourceBinding {
            source: RawDigest::of(&serde_json::to_vec(&oauth_stamp).map_err(|_| Error::Invalid)?),
            ..binding.clone()
        };
        let enabled_binding = SourceBinding {
            source: RawDigest::of(&serde_json::to_vec(&enabled_stamp).map_err(|_| Error::Invalid)?),
            ..binding.clone()
        };
        let disabled_binding = SourceBinding {
            source: RawDigest::of(
                &serde_json::to_vec(&disabled_stamp).map_err(|_| Error::Invalid)?,
            ),
            ..binding.clone()
        };
        Ok(Self {
            binding,
            enabled_binding,
            oauth_binding,
            disabled_binding,
            stamp,
            master,
            sa_present,
            bridge_present,
            disabled_path,
            raw_client,
        })
    }
    /// H1 desired-client planning. This is a target projection, not an
    /// actual observation or receipt; publication must re-read after fsync.
    pub(crate) fn preview_google_client(&self, raw: &Value) -> Result<SourceBinding> {
        validate_google_client_raw(raw)?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(raw).map_err(|_| Error::Invalid)?);
        Ok(SourceBinding {
            client: Some(RawDigest::of(&bytes)),
            ..self.binding.clone()
        })
    }
    /// Target projection for intentional client removal, not a receipt.
    pub(crate) fn preview_without_google_client(&self) -> SourceBinding {
        SourceBinding {
            client: None,
            ..self.binding.clone()
        }
    }
    pub(crate) fn binding(&self) -> &SourceBinding {
        &self.binding
    }
    pub(crate) fn oauth_binding(&self) -> &SourceBinding {
        &self.oauth_binding
    }
    pub(crate) fn binding_with_master(&self, enabled: bool) -> &SourceBinding {
        if enabled {
            &self.enabled_binding
        } else {
            &self.disabled_binding
        }
    }
    pub(crate) fn require_client(&self) -> Result<()> {
        self.raw_client.as_ref().map(|_| ()).ok_or(Error::Invalid)
    }
    pub(crate) fn take_raw_client(&mut self) -> Result<RawClientSnapshot> {
        self.raw_client
            .take()
            .map(|raw| RawClientSnapshot { _raw: raw })
            .ok_or(Error::Invalid)
    }
}
/// Sealed, nonsecret logical connector receipt. It is not a verified Google
/// subject and it contains neither a bearer nor raw encrypted secret fields.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HomeSourceReceipt {
    canonical_home: RawDigest,
    backend: HomeBackend,
    credential_id: String,
    logical_epoch: u64,
    source_generation: String,
    client_stamp: Option<RawDigest>,
    source_stamp: SourceStamp,
    scopes: Vec<String>,
}
impl std::fmt::Debug for HomeSourceReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeSourceReceipt")
            .field("backend", &self.backend)
            .field("logical_epoch", &self.logical_epoch)
            .finish_non_exhaustive()
    }
}
impl HomeSourceReceipt {
    pub(crate) fn backend(&self) -> HomeBackend {
        self.backend
    }
    pub(crate) fn credential_id(&self) -> &str {
        &self.credential_id
    }
    pub(crate) fn logical_epoch(&self) -> u64 {
        self.logical_epoch
    }
    pub(crate) fn source_generation(&self) -> &str {
        &self.source_generation
    }
    pub(crate) fn scopes(&self) -> &[String] {
        &self.scopes
    }
}
fn selection(
    g: &SourceGuard,
    s: &ObservedSource,
    scopes: &[&str],
    migrate: bool,
) -> Result<HomeSourceReceipt> {
    if !s.master || s.disabled_path {
        return Err(Error::NotReady);
    }
    // SA takes precedence; no invalid/pending SA configuration may silently
    // become OAuth. SA/Bridge receipt issuance is a later writer phase.
    if s.sa_present || s.bridge_present {
        return Err(Error::PendingWiring);
    }
    let scopes = home_store::normalized_scopes(scopes.iter().map(|s| s.to_string()).collect())?;
    let m = if migrate {
        home_store::publication_transaction(g, |r| {
            r.require_oauth_scopes(&scopes, s, home_store::host_time_now(g))?;
            r.prepare(s)
        })?
    } else {
        home_store::transaction(g, |r| {
            r.require_oauth_scopes(&scopes, s, home_store::host_time_now(g))?;
            r.metadata()
        })?
    };
    let credential = m.credential.ok_or(if s.bridge_present {
        Error::PendingWiring
    } else {
        Error::NotReady
    })?;
    Ok(HomeSourceReceipt {
        canonical_home: s.binding.home.clone(),
        backend: HomeBackend::OAuth,
        credential_id: credential.credential_id,
        logical_epoch: credential.logical_epoch,
        source_generation: m.source_generation,
        client_stamp: s.binding.client.clone(),
        source_stamp: s.stamp.clone(),
        scopes,
    })
}
/// Explicit metadata migration. Not called by ordinary public token loads.
pub(crate) fn prepare_home_metadata(home: &Path, scopes: &[&str]) -> Result<HomeSourceReceipt> {
    with_metadata(home, |g, s| selection(g, s, scopes, true))
}
/// Explicit gateway preparation, including the documented metadata migration;
/// this is not an ordinary credential load or a hidden decrypt operation.
pub(crate) fn prepare_home_selection(home: &Path, scopes: &[&str]) -> Result<HomeSourceReceipt> {
    prepare_home_metadata(home, scopes)
}
/// Read-only: validation never migrates/reissues an authority or changes a row.
pub(crate) fn validate_home_source(home: &Path, receipt: &HomeSourceReceipt) -> Result<()> {
    let scopes: Vec<_> = receipt.scopes.iter().map(String::as_str).collect();
    with_metadata(home, |g, s| {
        if selection(g, s, &scopes, false)? == *receipt {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    })
}

#[cfg(test)]
pub(crate) use oauth_material::mcp_budget_testing;

#[path = "home_source/bridge_material.rs"]
pub(crate) mod bridge_material;
#[path = "home_source/sa_material.rs"]
pub(crate) mod sa_material;
#[path = "home_source/w3_observation.rs"]
pub(crate) mod w3_observation;

impl HomeSourceReceipt {
    pub(crate) fn storage_weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.credential_id.capacity()
            + self.source_generation.capacity()
            + self.scopes.capacity() * std::mem::size_of::<String>()
            + self.scopes.iter().map(String::capacity).sum::<usize>()
    }
}
pub(crate) async fn resolve_home_native_preflight<'a>(
    home: &Path,
    selection: &NativeHomeSelection,
    tool: &str,
    deadline: tokio::time::Instant,
    permit: &'a crate::google_identity::effect_authority::GooglePreflightPermit<'a>,
) -> Result<HomeOAuthResolved> {
    native_selection::resolve_preflight(home, selection, tool, deadline, permit).await
}

#[cfg(test)]
pub(crate) use oauth_material::W3RefreshEndpointGuard;

pub(crate) use oauth_material::resolve_home_oauth_preflight;
