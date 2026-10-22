//! Protected key in the OS credential store (B2c).
//!
//! Credentials that let an AI employee act as one specific person (a staff
//! member's Google refresh token, a domain-wide-delegation service-account
//! key) are sealed with a separate 32-byte AES-256 key — the *protected key* —
//! that lives only in the OS credential store, never in a file. Home-wide
//! credentials (`.keyfile` and the `*_enc` values it decrypts) are untouched.
//!
//! What this protects against, honestly (design `DESIGN-credential-keychain-
//! 2026-10` §2; AI employees run as the same OS user as the gateway):
//!
//! - **macOS**: the keychain item's decrypt ACL trusts only the `duduclaw`
//!   executable (by code hash). A file read (Claude's `Read` tool, `cat`, a
//!   backup) gets nothing; `security` / Python / any other program reading the
//!   item fails without a person approving a system dialog. Code injected
//!   into a `duduclaw` process is trusted too, so the level is `AppBound` only
//!   when this executable runs with hardened runtime, else `UserStore`
//!   (design §2.3 E9/E10). It does NOT stop a same-user process from
//!   overwriting or deleting the item (E6/E7). A changed key is refused when
//!   it does not match the marker's key id and canary, when it differs from
//!   the key this process loaded first, or when a sealed value carries another
//!   key id. Replacing key *and* marker and then restarting the gateway is not
//!   caught here; the old sealed values then fail with `key_changed`, which
//!   callers must surface (design §6).
//! - **Windows** (`UserStore`): Credential Manager is per user; any same-user
//!   program can read it. Stops file reads and backups only.
//! - **Everything else** (Linux, Docker): no supported store; `File` level,
//!   person identities are refused.
//!
//! Only the process holding `<home>/locks/gateway.lock` may unseal
//! ([`CredentialProtection::load_key_for_gateway`]); the MCP server and CLI
//! subcommands never need the key.

pub mod marker;
mod messages;
mod store;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests;
#[cfg(all(test, target_os = "macos"))]
mod macos_tests;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::CryptoEngine;
use marker::{Marker, MarkerRead};

#[cfg(target_os = "macos")]
pub use macos::MacKeychainStore;
pub use store::{KeyStore, MemoryKeyStore, StoreError, UnsupportedStore};
#[cfg(windows)]
pub use windows::WindowsCredentialStore;

const KEY_LEN: usize = 32;
/// Prefix of a value sealed with the protected key.
pub const SEALED_PREFIX: &str = "pk:v1:";

/// How well the stored key resists a process running as the same OS user.
/// Ordered weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionLevel {
    /// Key material in ordinary files (today's `.keyfile`), or nothing enabled.
    File,
    /// OS per-user store without per-program access control (Windows).
    UserStore,
    /// OS store whose item only the `duduclaw` executable may read (macOS).
    AppBound,
}

/// The lowest level at which an employee may act as a specific person.
/// Owner rule (2026-10-23): credentials must be in the OS keychain first.
/// Windows counts per that rule (design §5.1, Q1); raising this to
/// `AppBound` limits person identities to macOS.
pub const PERSON_IDENTITY_MIN_LEVEL: ProtectionLevel = ProtectionLevel::UserStore;

/// Which credential store backs the protected key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    MacosKeychain,
    WindowsCredentialManager,
    None,
}

/// Closed set of states (design §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionState {
    NotEnabled,
    Ready,
    NeedsAuthorization,
    Missing,
    KeyChanged,
    Invalid,
    Unsupported,
    Unavailable,
}

impl ProtectionState {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotEnabled => "not_enabled",
            Self::Ready => "ready",
            Self::NeedsAuthorization => "needs_authorization",
            Self::Missing => "missing",
            Self::KeyChanged => "key_changed",
            Self::Invalid => "invalid",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
        }
    }
}

/// What `status()` reports. Never carries key material.
#[derive(Debug, Clone, Serialize)]
pub struct ProtectionStatus {
    /// Effective level now: the backend's level when `Ready`, else `File`.
    pub level: ProtectionLevel,
    /// Level this platform's store would give once enabled.
    pub platform_level: ProtectionLevel,
    pub backend: BackendKind,
    pub state: ProtectionState,
    pub key_id: Option<String>,
    pub enabled_at: Option<String>,
    pub person_identity_allowed: bool,
    /// One line, e.g. 「憑證保護：系統金鑰圈（只有 DuDuClaw 程式讀得到）」.
    pub summary_zh: String,
    /// What it means / what to do.
    pub detail_zh: String,
}

/// A refused operation. `code` is a closed set; `message_zh` is user-facing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionError {
    pub code: &'static str,
    pub message_zh: String,
}

impl ProtectionError {
    fn new(code: &'static str, message_zh: impl Into<String>) -> Self {
        Self { code, message_zh: message_zh.into() }
    }

    fn from_state(state: ProtectionState, extra: &str) -> Self {
        let (summary, detail) = messages::state_text(state, ProtectionLevel::File, BackendKind::None, extra);
        Self::new(state.code(), format!("{summary}。{detail}"))
    }
}

impl fmt::Display for ProtectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message_zh)
    }
}

impl std::error::Error for ProtectionError {}

/// Result of [`CredentialProtection::enable_detailed`].
#[derive(Debug, Clone)]
pub struct Enabled {
    pub status: ProtectionStatus,
    /// State before the call.
    pub previous: ProtectionState,
    /// False when it was already `Ready` and nothing was done.
    pub changed: bool,
}

/// The unwrapped protected key. Only obtainable inside the gateway process.
pub struct ProtectedKey {
    key: Zeroizing<[u8; KEY_LEN]>,
    key_id: String,
}

impl fmt::Debug for ProtectedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProtectedKey(<redacted>, key_id={})", self.key_id)
    }
}

impl ProtectedKey {
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Seal `plaintext` as `pk:v1:<key_id>:<base64>`.
    pub fn seal(&self, plaintext: &[u8]) -> Result<String, ProtectionError> {
        let engine = engine_for(&self.key)?;
        let ct = engine
            .encrypt(plaintext)
            .map_err(|e| ProtectionError::new("seal_failed", format!("加密失敗：{e}")))?;
        Ok(format!("{SEALED_PREFIX}{}:{}", self.key_id, BASE64.encode(ct)))
    }

    /// Open a value produced by [`ProtectedKey::seal`]. A value sealed under a
    /// different key is reported as `key_changed` (design §6 item 3).
    pub fn unseal(&self, sealed: &str) -> Result<Zeroizing<Vec<u8>>, ProtectionError> {
        let (kid, body) = parse_sealed(sealed)?;
        if kid != self.key_id {
            return Err(ProtectionError::from_state(ProtectionState::KeyChanged, ""));
        }
        let bytes = BASE64
            .decode(body)
            .map_err(|_| ProtectionError::new("sealed_invalid", "封存的憑證格式錯誤"))?;
        engine_for(&self.key)?
            .decrypt(&bytes)
            .map(Zeroizing::new)
            .map_err(|_| ProtectionError::new("sealed_invalid", "封存的憑證無法解開（可能已損壞或被改動）"))
    }
}

/// Split `pk:v1:<key_id>:<base64>` into its key id and body.
pub fn parse_sealed(sealed: &str) -> Result<(&str, &str), ProtectionError> {
    let rest = sealed
        .strip_prefix(SEALED_PREFIX)
        .ok_or_else(|| ProtectionError::new("sealed_invalid", "不是用保護金鑰封存的值"))?;
    let (kid, body) = rest
        .split_once(':')
        .ok_or_else(|| ProtectionError::new("sealed_invalid", "封存的憑證格式錯誤"))?;
    if kid.is_empty() || body.is_empty() {
        return Err(ProtectionError::new("sealed_invalid", "封存的憑證格式錯誤"));
    }
    Ok((kid, body))
}

fn engine_for(key: &[u8; KEY_LEN]) -> Result<CryptoEngine, ProtectionError> {
    CryptoEngine::new(key).map_err(|e| ProtectionError::new("crypto_failed", format!("加密元件初始化失敗：{e}")))
}

/// In-process pin: first key id loaded per account (design §6 item 2).
fn pins() -> &'static Mutex<HashMap<String, String>> {
    static PINS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    PINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pin_check(account: &str, kid: &str) -> bool {
    let mut map = pins().lock().unwrap_or_else(|p| p.into_inner());
    match map.get(account) {
        Some(pinned) => pinned == kid,
        None => {
            map.insert(account.to_string(), kid.to_string());
            true
        }
    }
}

fn pin_reset(account: &str) {
    pins().lock().unwrap_or_else(|p| p.into_inner()).remove(account);
}

/// The store this platform uses. Chosen by `cfg` only — never by config or
/// environment, so nothing an employee writes can redirect it.
pub fn platform_store() -> Box<dyn KeyStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(MacKeychainStore::default_keychain())
    }
    #[cfg(windows)]
    {
        Box::new(WindowsCredentialStore)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Box::new(UnsupportedStore)
    }
}

/// Protection for one DuDuClaw home, over one store.
pub struct CredentialProtection {
    home: PathBuf,
    store: Box<dyn KeyStore>,
}

impl CredentialProtection {
    pub fn platform(home: &Path) -> Self {
        Self::with_store(home, platform_store())
    }

    pub fn with_store(home: &Path, store: Box<dyn KeyStore>) -> Self {
        Self { home: home.to_path_buf(), store }
    }

    fn account(&self) -> String {
        marker::account_for_home(&self.home)
    }

    /// Current state. Reads the key once to verify it, then drops it.
    pub fn status(&self) -> ProtectionStatus {
        let platform_level = self.store.level();
        let backend = self.store.backend();
        let marker = read_marker_for(&self.home, self.store.as_ref(), &self.account());
        let (state, key_id, enabled_at, extra) = match marker {
            Err((state, extra)) => (state, None, None, extra),
            Ok(m) => match self.load_verified(&m) {
                Ok(key) => (ProtectionState::Ready, Some(key.key_id), Some(m.enabled_at), String::new()),
                Err((state, extra)) => (state, Some(m.key_id), Some(m.enabled_at), extra),
            },
        };
        let level = if state == ProtectionState::Ready { platform_level } else { ProtectionLevel::File };
        let (summary_zh, detail_zh) = messages::state_text(state, level, backend, &extra);
        ProtectionStatus {
            level,
            platform_level,
            backend,
            state,
            key_id,
            enabled_at,
            person_identity_allowed: state == ProtectionState::Ready
                && level >= PERSON_IDENTITY_MIN_LEVEL,
            summary_zh,
            detail_zh,
        }
    }

    /// Create the protected key. Idempotent when already `Ready` and `reset`
    /// is false. `reset: true` always makes a new key (also from `Ready`: the
    /// recovery when key *and* marker may both have been replaced, design §6)
    /// and voids everything sealed with the old one; it is required whenever
    /// a marker exists but the key is unusable.
    pub fn enable(&self, reset: bool) -> Result<ProtectionStatus, ProtectionError> {
        self.enable_detailed(reset).map(|e| e.status)
    }

    /// [`CredentialProtection::enable`] plus what it replaced, for the audit.
    pub fn enable_detailed(&self, reset: bool) -> Result<Enabled, ProtectionError> {
        if self.store.backend() == BackendKind::None {
            return Err(ProtectionError::from_state(ProtectionState::Unsupported, ""));
        }
        self.with_op_lock(|| {
            let current = self.status();
            let previous = current.state;
            if previous == ProtectionState::Ready && !reset {
                return Ok(Enabled { status: current, previous, changed: false });
            }
            if previous != ProtectionState::Ready && previous != ProtectionState::NotEnabled && !reset {
                return Err(ProtectionError::new(
                    "reset_required",
                    format!("{}。重新啟用會換一把新的保護金鑰，已封存的個人帳號憑證會作廢，要確認後再做。", current.summary_zh),
                ));
            }
            let status = self.create_key()?;
            Ok(Enabled { status, previous, changed: true })
        })
    }

    fn create_key(&self) -> Result<ProtectionStatus, ProtectionError> {
        let account = self.account();
        let key = Zeroizing::new(
            CryptoEngine::generate_key()
                .map_err(|e| ProtectionError::new("crypto_failed", format!("產生金鑰失敗：{e}")))?,
        );
        let engine = engine_for(&key)?;
        let canary = marker::make_canary(&engine).map_err(|e| ProtectionError::new("crypto_failed", e))?;
        self.store
            .write_new(&account, &key[..])
            .map_err(|e| store_error(&e))?;
        let m = marker::new_marker(self.store.backend(), account.clone(), marker::key_id(&key[..]), canary);
        if let Err(e) = marker::write_marker(&self.home, &m) {
            let _ = self.store.delete(&account);
            return Err(ProtectionError::new("marker_write_failed", format!("無法寫入記錄檔：{e}")));
        }
        pin_reset(&account);
        let status = self.status();
        if status.state != ProtectionState::Ready {
            let _ = self.store.delete(&account);
            let _ = marker::remove_marker(&self.home);
            pin_reset(&account);
            return Err(ProtectionError::new(
                "verify_failed",
                format!("寫入系統金鑰圈後讀不回來，已還原：{}", status.detail_zh),
            ));
        }
        Ok(status)
    }

    /// Remove the protected key and the marker. Everything sealed with it
    /// becomes unreadable.
    pub fn disable(&self) -> Result<(), ProtectionError> {
        self.with_op_lock(|| {
            let account = self.account();
            self.store.delete(&account).map_err(|e| store_error(&e))?;
            marker::remove_marker(&self.home).map_err(|e| ProtectionError::new("marker_write_failed", e))?;
            pin_reset(&account);
            Ok(())
        })
    }

    /// macOS after an upgrade: read once with the system dialog allowed, then
    /// re-create the item from this executable so its ACL trusts only us.
    ///
    /// It accepts the key the marker vouches for. If key and marker were both
    /// replaced, this re-blesses the replacement; B2's stores must check that
    /// their existing sealed values still open (design §6) — a `key_changed`
    /// there needs `enable(reset: true)`, not `authorize`. Run it with the same
    /// `duduclaw` executable the gateway uses, or the new ACL trusts the wrong
    /// one.
    pub fn authorize(&self) -> Result<ProtectionStatus, ProtectionError> {
        self.with_op_lock(|| {
            let account = self.account();
            let m = read_marker_for(&self.home, self.store.as_ref(), &account)
                .map_err(|(state, extra)| ProtectionError::from_state(state, &extra))?;
            let bytes = self.store.read_interactive(&account).map_err(|e| store_error(&e))?;
            let key = verify_bytes(&bytes, &m).map_err(|(state, extra)| ProtectionError::from_state(state, &extra))?;
            self.store.write_new(&account, key.as_ref()).map_err(|e| store_error(&e))?;
            let status = self.status();
            if status.state == ProtectionState::Ready {
                Ok(status)
            } else {
                Err(ProtectionError::new(status.state.code(), format!("{}。{}", status.summary_zh, status.detail_zh)))
            }
        })
    }

    /// Serialize enable / disable / authorize across processes (the RPC and
    /// a terminal `authorize` may race) with `<home>/locks/credential_protection.lock`.
    fn with_op_lock<T>(&self, f: impl FnOnce() -> Result<T, ProtectionError>) -> Result<T, ProtectionError> {
        let lock = self.home.join("locks").join("credential_protection");
        duduclaw_core::with_file_lock(&lock, || Ok(f()))
            .map_err(|e| ProtectionError::new("lock_failed", format!("無法取得憑證保護的操作鎖：{e}")))?
    }

    /// The key itself — only for the process holding the gateway lock.
    pub fn load_key_for_gateway(&self) -> Result<ProtectedKey, ProtectionError> {
        if !duduclaw_core::gateway_instance::held(&self.home) {
            return Err(ProtectionError::new(
                "not_gateway",
                "只有 DuDuClaw 主程式能解開保護金鑰",
            ));
        }
        let m = read_marker_for(&self.home, self.store.as_ref(), &self.account())
            .map_err(|(state, extra)| ProtectionError::from_state(state, &extra))?;
        let key = self
            .load_verified(&m)
            .map_err(|(state, extra)| ProtectionError::from_state(state, &extra))?;
        Ok(ProtectedKey { key: key.key, key_id: key.key_id })
    }

    /// The single check B2a/B2b call before using a person's credentials.
    pub fn person_identity_gate(&self) -> Result<ProtectedKey, ProtectionError> {
        let key = self.load_key_for_gateway()?;
        if self.store.level() < PERSON_IDENTITY_MIN_LEVEL {
            return Err(ProtectionError::new("level_too_low", messages::LEVEL_TOO_LOW));
        }
        Ok(key)
    }

    fn load_verified(&self, m: &Marker) -> Result<VerifiedKey, (ProtectionState, String)> {
        let bytes = self.store.read(&m.account).map_err(|e| state_of(&e))?;
        let key = verify_bytes(&bytes, m)?;
        if !pin_check(&m.account, &key.key_id) {
            return Err((ProtectionState::KeyChanged, String::new()));
        }
        Ok(key)
    }
}

struct VerifiedKey {
    key: Zeroizing<[u8; KEY_LEN]>,
    key_id: String,
}

impl AsRef<[u8]> for VerifiedKey {
    fn as_ref(&self) -> &[u8] {
        &self.key[..]
    }
}

fn verify_bytes(bytes: &[u8], m: &Marker) -> Result<VerifiedKey, (ProtectionState, String)> {
    let arr: [u8; KEY_LEN] = bytes
        .try_into()
        .map_err(|_| (ProtectionState::KeyChanged, String::new()))?;
    let key = Zeroizing::new(arr);
    let kid = marker::key_id(&key[..]);
    if kid != m.key_id {
        return Err((ProtectionState::KeyChanged, String::new()));
    }
    let engine = CryptoEngine::new(&key).map_err(|e| (ProtectionState::Unavailable, e.to_string()))?;
    if !marker::canary_matches(&engine, &m.canary) {
        return Err((ProtectionState::KeyChanged, String::new()));
    }
    Ok(VerifiedKey { key, key_id: kid })
}

/// Read the marker and check it belongs to this home and this platform.
fn read_marker_for(home: &Path, store: &dyn KeyStore, account: &str) -> Result<Marker, (ProtectionState, String)> {
    if store.backend() == BackendKind::None {
        return Err((ProtectionState::Unsupported, String::new()));
    }
    match marker::read_marker(home) {
        MarkerRead::Absent => Err((ProtectionState::NotEnabled, String::new())),
        MarkerRead::Invalid(why) => Err((ProtectionState::Invalid, why)),
        MarkerRead::Present(m) if m.account != account || m.backend != store.backend() => Err((
            ProtectionState::Invalid,
            "記錄檔和這台機器或這個資料夾不符".to_string(),
        )),
        MarkerRead::Present(m) => Ok(m),
    }
}

fn state_of(e: &StoreError) -> (ProtectionState, String) {
    match e {
        StoreError::NotFound => (ProtectionState::Missing, String::new()),
        StoreError::InteractionRequired => (ProtectionState::NeedsAuthorization, String::new()),
        StoreError::Unavailable(why) => (ProtectionState::Unavailable, why.clone()),
    }
}

fn store_error(e: &StoreError) -> ProtectionError {
    let (state, extra) = state_of(e);
    ProtectionError::from_state(state, &extra)
}

/// Current protection for `home` on this platform's store.
pub fn credential_protection_level(home: &Path) -> ProtectionStatus {
    CredentialProtection::platform(home).status()
}

/// The single gate for person identities (B2a/B2b), on this platform's store.
pub fn person_identity_gate(home: &Path) -> Result<ProtectedKey, ProtectionError> {
    CredentialProtection::platform(home).person_identity_gate()
}
