//! Credentials doctrine (WP-H1, P0) — the single secret-reading contract.
//!
//! Before this module the codebase had **seven** independent hand-rolled
//! "try `<field>_enc`, else `<field>` plaintext" implementations, each with a
//! slightly different empty-value rule and only two of which recognised the
//! `secret://<backend>/<name>` reference scheme that
//! [`crate::secret_manager`] has supported since ADR-004. On every other path a
//! `secret://…` string was handed to the caller **verbatim** and sent to the
//! vendor API as if it were the credential.
//!
//! Three types replace all of that:
//!
//! - [`SecretRef`] — what the *config file* holds. It is a reference, not a
//!   secret: safe to log, diff, and render. Built by exactly one classifier,
//!   [`SecretRef::classify`], which owns the empty-value rule.
//! - [`Secret`] — a resolved value. `Debug`/`Display` print `<redacted>`, it is
//!   zeroized on drop, it does not implement `Serialize`, and
//!   [`Secret::new`] **refuses the empty string**, so "set to empty" cannot be
//!   represented at all. That turns the doctrine's "empty means unset" from a
//!   review rule into a type-system fact.
//! - [`SecretStatus`] — what `describe()` answers: *is it set, where does it
//!   come from, can the dashboard write it, is there plaintext residue*. It
//!   never holds the value and never calls a backend.
//!
//! ## Resolution is one function in two stages
//!
//! [`SecretRef::resolve_local`] is stage 1: it handles every source that is
//! free to read (inline ciphertext via the keyfile, legacy plaintext, and
//! `secret://env/…`). When the reference points at a network backend
//! (Vault / 1Password / Infisical) it returns
//! [`LocalOutcome::NeedsBackend`] rather than guessing.
//!
//! [`SecretRef::resolve`] is the whole function: stage 1, then the network
//! fetch for `NeedsBackend`. Callers already inside `async` use it.
//!
//! [`SecretRef::resolve_sync`] is stage 1 with `NeedsBackend` treated as
//! **unset plus a warning** — fail-closed. It exists for the handful of
//! synchronous call sites that cannot `await` today; the important property is
//! that it can never return the literal `secret://…` string, which is the bug
//! this module was written to kill.

use std::fmt;
use std::path::Path;

use serde::Serialize;
use zeroize::Zeroize;

use crate::secret_manager::{SecretBackend, SecretManagerConfig, SecretUri};

// ─── Secret ────────────────────────────────────────────────────────────────

/// A resolved credential value.
///
/// Deliberately awkward to leak:
/// - `Debug` and `Display` render `<redacted>`;
/// - the buffer is zeroized on drop;
/// - there is no `Serialize` impl, so it cannot ride out through a JSON RPC
///   response by accident;
/// - it can never hold the empty string ([`Secret::new`] returns `None`).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a resolved value.
    ///
    /// Returns `None` for the empty string — "configured but empty" is not a
    /// state this type can represent, which is what makes the doctrine's
    /// empty-is-unset rule structural rather than advisory. Note that a value
    /// consisting only of whitespace is **kept**: some vendors issue tokens
    /// with surrounding whitespace significance, and silently trimming a
    /// credential would be a behaviour change, not a safety win.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.is_empty() { None } else { Some(Self(value)) }
    }

    /// Borrow the underlying value. Named to make review grep-able.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and hand back the raw `String`.
    ///
    /// The returned `String` is **not** zeroized on drop — use it only where
    /// the value must cross an API boundary that demands ownership.
    pub fn expose_owned(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

// ─── SourceKind / SecretStatus ─────────────────────────────────────────────

/// Where a [`SecretRef`]'s value comes from. Non-secret by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Nothing configured.
    Unset,
    /// `<field>_enc`: AES-256-GCM ciphertext decrypted with the machine keyfile.
    Inline,
    /// A bare plaintext value sitting in the config file.
    Legacy,
    /// `secret://env/NAME`.
    Env,
    /// `secret://local/NAME`.
    Local,
    /// `secret://vault/NAME`.
    Vault,
    /// `secret://onepassword/NAME`.
    OnePassword,
    /// `secret://infisical/NAME`.
    Infisical,
    /// `secret://keychain/SERVICE/ACCOUNT` — OS-native credential store.
    Keychain,
    /// `secret://file//absolute/path` — Docker / Kubernetes mounted secret.
    File,
    /// A lone field holding either ciphertext or plaintext, indistinguishable
    /// without a decrypt attempt. Reported honestly rather than guessed at —
    /// `describe()` never decrypts.
    Ambiguous,
}

impl SourceKind {
    fn from_backend(backend: &SecretBackend) -> Self {
        match backend {
            SecretBackend::Local => SourceKind::Local,
            SecretBackend::Vault => SourceKind::Vault,
            SecretBackend::Env => SourceKind::Env,
            SecretBackend::OnePassword => SourceKind::OnePassword,
            SecretBackend::Infisical => SourceKind::Infisical,
            SecretBackend::Keychain => SourceKind::Keychain,
            SecretBackend::File => SourceKind::File,
        }
    }

    /// Whether the dashboard can overwrite this source from the UI. External
    /// references are rotated in their own backend, not here.
    fn writable(self) -> bool {
        matches!(
            self,
            SourceKind::Unset | SourceKind::Inline | SourceKind::Legacy | SourceKind::Ambiguous
        )
    }
}

/// The answer to "is this credential set, and where does it live" — the single
/// replacement for the five masking dialects that used to coexist
/// (`********`, `***set***`, `••••9f3c`, `ddc_prod_a1b2…`, bare `bool`).
///
/// Never holds the value; never calls a backend, so rendering forty fields
/// costs zero Vault round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretStatus {
    /// A source is configured. For a reference this means "a reference is
    /// present", not "the backend currently answers" — that is a resolve-time
    /// fact, deliberately not asserted here.
    pub configured: bool,
    /// Which source wins.
    pub source: SourceKind,
    /// Non-secret human description: `"encrypted(keyfile)"`,
    /// `"env:TELEGRAM_BOT_TOKEN"`, `"vault:kv/telegram"`, `"plaintext(legacy)"`,
    /// `"unset"`. Never contains the value or the ciphertext.
    pub source_label: String,
    /// Whether the dashboard may write this field.
    pub writable: bool,
    /// A plaintext twin coexists with a `_enc` twin for the same field
    /// (design §1.5). The plaintext is inert for reads — the ciphertext wins —
    /// but it is still a credential sitting in the file, and nothing else in
    /// the system could see it. Detection only; removal is a P1 operator
    /// action because deleting user data is irreversible.
    pub residue: bool,
}

impl SecretStatus {
    fn unset() -> Self {
        Self {
            configured: false,
            source: SourceKind::Unset,
            source_label: "unset".to_string(),
            writable: true,
            residue: false,
        }
    }
}

// ─── SecretRef ─────────────────────────────────────────────────────────────

/// One link in a reference's resolution chain.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefSource {
    /// Base64 AES-256-GCM ciphertext from `<field>_enc`.
    Inline(String),
    /// A parsed `secret://<backend>/<name>` reference.
    Reference(SecretUri),
    /// A bare plaintext credential.
    Legacy(String),
    /// A single field that is *either* ciphertext or plaintext and cannot be
    /// told apart without attempting a decrypt (see [`SecretRef::from_single`]).
    Ambiguous(String),
}

/// What the config file holds for one credential field.
///
/// Safe to log, diff and render: it carries a ciphertext or a reference URI,
/// never a resolved value. Construct it with [`SecretRef::classify`] (raw TOML,
/// where a present-but-empty key is meaningful) or [`SecretRef::from_typed`]
/// (deserialized structs, where it is not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    /// Ordered candidates; first success wins. Empty ⇒ unset.
    chain: Vec<RefSource>,
    /// Plaintext and ciphertext twins both present and non-empty.
    residue: bool,
}

impl SecretRef {
    /// Classify a `<field>_enc` / `<field>` pair read straight out of a TOML
    /// table.
    ///
    /// `plain` distinguishes three states and they are **not** interchangeable:
    ///
    /// | `plain`        | meaning                                             |
    /// |----------------|-----------------------------------------------------|
    /// | `None`         | key absent — fall through to `_enc`                 |
    /// | `Some("")`     | key present but blank — an explicit *removal marker*; suppresses a stale `_enc` twin |
    /// | `Some(v)`      | a value: a `secret://` reference, or legacy plaintext |
    ///
    /// The `Some("")` rule is load-bearing: the write path deletes the
    /// plaintext key rather than blanking it, so a blank key means an operator
    /// removed the channel and a leftover ciphertext must not resurrect it.
    pub fn classify(enc: Option<&str>, plain: Option<&str>) -> Self {
        let enc = enc.filter(|s| !s.is_empty());
        let residue = enc.is_some() && plain.is_some_and(|p| !p.is_empty());

        // Explicit removal marker wins over everything.
        if plain == Some("") {
            return Self {
                chain: Vec::new(),
                residue,
            };
        }

        let mut chain = Vec::new();
        if let Some(enc) = enc {
            chain.push(RefSource::Inline(enc.to_string()));
        }
        if let Some(plain) = plain.filter(|s| !s.is_empty()) {
            chain.push(Self::plain_source(plain));
        }
        Self { chain, residue }
    }

    /// Classify a pair coming from a deserialized struct (`agent.toml`'s typed
    /// `[channels.*]` sections), where an absent key and a blank key both
    /// arrive as `""` and therefore cannot carry the removal marker.
    pub fn from_typed(enc: Option<&str>, plain: &str) -> Self {
        Self::classify(enc, Some(plain).filter(|s| !s.is_empty()))
    }

    /// Classify a **single** field that may hold either ciphertext or a
    /// plaintext value — the older `[integrations.*] secret = "…"` shape, which
    /// never grew a `_enc` twin.
    ///
    /// Semantics match the hand-rolled readers this replaces ("try to decrypt;
    /// if that fails the string *is* the value") with one correction: a value
    /// that fails to decrypt and turns out to be a `secret://…` reference is
    /// now resolved as a reference instead of being handed to the remote
    /// service as the credential.
    ///
    /// There is no residue concept here — one field cannot be its own twin —
    /// so [`Self::has_residue`] is always `false`.
    pub fn from_single(value: &str) -> Self {
        let value = value.trim();
        if value.is_empty() {
            return Self {
                chain: Vec::new(),
                residue: false,
            };
        }
        // A reference is unambiguous, so skip the decrypt attempt entirely
        // (base64-shaped ciphertext and a `secret://` URI cannot collide).
        if value.starts_with("secret://") {
            return Self {
                chain: vec![Self::plain_source(value)],
                residue: false,
            };
        }
        Self {
            chain: vec![RefSource::Ambiguous(value.to_string())],
            residue: false,
        }
    }

    /// A single plaintext value: reference if it uses the scheme, else legacy.
    ///
    /// A malformed `secret://` (unknown backend, missing name) is **not**
    /// downgraded to legacy plaintext — that would send the URI itself to the
    /// vendor, which is precisely the bug being fixed. It becomes an empty
    /// chain, i.e. unset.
    fn plain_source(plain: &str) -> RefSource {
        if plain.starts_with("secret://") {
            match SecretUri::parse(plain) {
                Ok(uri) => RefSource::Reference(uri),
                Err(e) => {
                    tracing::warn!(
                        "credential field holds a malformed secret:// reference \
                         ({e}) — treating as unset rather than sending the URI \
                         as the credential"
                    );
                    // An unusable reference: represent it as an empty legacy
                    // value, which `Secret::new` rejects.
                    RefSource::Legacy(String::new())
                }
            }
        } else {
            RefSource::Legacy(plain.to_string())
        }
    }

    /// Non-secret status for dashboards, `doctor`, and logs. Never resolves.
    pub fn describe(&self) -> SecretStatus {
        let Some(first) = self.chain.first() else {
            return SecretStatus {
                residue: self.residue,
                ..SecretStatus::unset()
            };
        };
        let (source, source_label) = match first {
            RefSource::Inline(_) => (SourceKind::Inline, "encrypted(keyfile)".to_string()),
            RefSource::Legacy(v) if v.is_empty() => {
                // Only produced by an unparseable `secret://` reference.
                (SourceKind::Unset, "unset(invalid-reference)".to_string())
            }
            RefSource::Legacy(_) => (SourceKind::Legacy, "plaintext(legacy)".to_string()),
            RefSource::Ambiguous(_) => (
                SourceKind::Ambiguous,
                "encrypted-or-plaintext(single field)".to_string(),
            ),
            RefSource::Reference(uri) => (
                SourceKind::from_backend(&uri.backend),
                format!("{}:{}", uri.backend, uri.name),
            ),
        };
        SecretStatus {
            configured: source != SourceKind::Unset,
            source,
            source_label,
            writable: source.writable(),
            residue: self.residue,
        }
    }

    /// True when a plaintext twin sits next to a ciphertext twin (design §1.5).
    pub fn has_residue(&self) -> bool {
        self.residue
    }

    /// Stage 1 of resolution: everything readable without network I/O.
    ///
    /// Walks the chain in order. An inline ciphertext that fails to decrypt
    /// (missing / rotated keyfile) falls through to the next candidate, which
    /// preserves the pre-existing enc-then-plaintext fallback behaviour.
    pub fn resolve_local(&self, home_dir: &Path) -> LocalOutcome {
        for source in &self.chain {
            match source {
                RefSource::Inline(ciphertext) => {
                    if let Some(plain) = crate::keyfile::decrypt_keyfile_value(ciphertext, home_dir)
                    {
                        // A decrypted value may itself be a reference (an
                        // "encrypted pointer" — supported by the pre-existing
                        // async path, preserved here).
                        match Self::deref_once(&plain, home_dir) {
                            LocalOutcome::Unset => continue,
                            other => return other,
                        }
                    }
                    // Undecryptable: fall through to the plaintext twin.
                }
                RefSource::Legacy(value) => {
                    if let Some(secret) = Secret::new(value.clone()) {
                        return LocalOutcome::Resolved(secret);
                    }
                }
                RefSource::Ambiguous(value) => {
                    // Try it as ciphertext; whatever comes out may itself be a
                    // reference, exactly as for a real `_enc` field. A decrypt
                    // failure means the field was plaintext all along.
                    if let Some(plain) = crate::keyfile::decrypt_keyfile_value(value, home_dir) {
                        match Self::deref_once(&plain, home_dir) {
                            LocalOutcome::Unset => continue,
                            other => return other,
                        }
                    }
                    if let Some(secret) = Secret::new(value.clone()) {
                        return LocalOutcome::Resolved(secret);
                    }
                }
                RefSource::Reference(uri) => return Self::resolve_uri_local(uri, home_dir),
            }
        }
        LocalOutcome::Unset
    }

    /// One level of indirection for a value that came out of the keyfile: if it
    /// is itself a `secret://` reference, resolve that; otherwise it is the
    /// value. Bounded to a single hop — no reference chasing loops.
    fn deref_once(value: &str, home_dir: &Path) -> LocalOutcome {
        if !value.starts_with("secret://") {
            return match Secret::new(value.to_string()) {
                Some(s) => LocalOutcome::Resolved(s),
                None => LocalOutcome::Unset,
            };
        }
        match SecretUri::parse(value) {
            Ok(uri) => Self::resolve_uri_local(&uri, home_dir),
            Err(e) => {
                tracing::warn!("encrypted credential decrypted to a malformed secret:// reference ({e}) — treating as unset");
                LocalOutcome::Unset
            }
        }
    }

    /// Resolve a URI that needs no network — `env`, `keychain` and `file`
    /// (P1). Everything else is handed back as [`LocalOutcome::NeedsBackend`].
    ///
    /// The split is [`SecretBackend::is_local`], not an ad-hoc match arm, so
    /// adding a backend forces the author to declare which side it is on.
    fn resolve_uri_local(uri: &SecretUri, home_dir: &Path) -> LocalOutcome {
        match uri.backend {
            SecretBackend::Env => match std::env::var(&uri.name) {
                Ok(v) => match Secret::new(v) {
                    Some(s) => LocalOutcome::Resolved(s),
                    None => {
                        tracing::warn!(
                            var = %uri.name,
                            "secret://env reference points at an environment \
                             variable that is set but empty — treating as unset"
                        );
                        LocalOutcome::Unset
                    }
                },
                Err(_) => {
                    tracing::warn!(
                        var = %uri.name,
                        "secret://env reference points at an unset environment variable"
                    );
                    LocalOutcome::Unset
                }
            },
            SecretBackend::Keychain => {
                match crate::secret_manager::KeychainSecretAdapter::new().get_blocking(&uri.name) {
                    Ok(v) => Self::wrap_or_warn_empty(v, "secret://keychain", &uri.name),
                    Err(e) => {
                        // The name is a service/account pair, never a value.
                        tracing::warn!(entry = %uri.name, "secret://keychain reference could not be resolved: {e}");
                        LocalOutcome::Unset
                    }
                }
            }
            SecretBackend::File => {
                match crate::secret_manager::FileSecretAdapter::new(home_dir)
                    .read_blocking(&uri.name)
                {
                    Ok(v) => Self::wrap_or_warn_empty(v, "secret://file", &uri.name),
                    Err(e) => {
                        tracing::warn!(path = %uri.name, "secret://file reference could not be resolved: {e}");
                        LocalOutcome::Unset
                    }
                }
            }
            SecretBackend::Local
            | SecretBackend::Vault
            | SecretBackend::OnePassword
            | SecretBackend::Infisical => LocalOutcome::NeedsBackend(uri.clone()),
        }
    }

    /// A backend answered — turn its value into a `Secret`, or say out loud
    /// that the entry exists but is blank. Silence here is what produced the
    /// 2026-08-13 "token was moved and everything went quiet" incident.
    fn wrap_or_warn_empty(value: String, scheme: &str, name: &str) -> LocalOutcome {
        match Secret::new(value) {
            Some(s) => LocalOutcome::Resolved(s),
            None => {
                tracing::warn!(
                    entry = %name,
                    "{scheme} reference resolved to an empty value — treating as unset"
                );
                LocalOutcome::Unset
            }
        }
    }

    /// Full resolution, including network-backed references.
    pub async fn resolve(
        &self,
        sm_cfg: &SecretManagerConfig,
        home_dir: &Path,
    ) -> Option<Secret> {
        match self.resolve_local(home_dir) {
            LocalOutcome::Resolved(secret) => Some(secret),
            LocalOutcome::Unset => None,
            LocalOutcome::NeedsBackend(uri) => {
                crate::secret_manager::resolve_secret_reference(
                    &uri.to_string(),
                    sm_cfg,
                    home_dir,
                )
                .await
                .and_then(Secret::new)
            }
        }
    }

    /// Resolution for call sites that cannot `await`.
    ///
    /// Identical to [`Self::resolve`] for every source
    /// [`SecretBackend::is_local`] accepts — inline ciphertext, legacy
    /// plaintext, `secret://env`, `secret://keychain` and `secret://file`. A
    /// network-backed reference (vault / 1Password / Infisical) is **refused**
    /// with a warning instead of resolved: fail-closed, and above all never
    /// returning the literal `secret://…` string.
    ///
    /// P1 status (closed out by WP-6C, 2026-08): every production call site
    /// that used to be stuck on this fail-closed path now runs on an async
    /// context and calls [`Self::resolve`] instead — the Apps Script bridge
    /// config and the reminder scheduler's channel tokens (both async-ified
    /// earlier, under WP-4A), and, as of WP-6C, the per-agent channel token
    /// bot-start paths (`telegram.rs` / `discord.rs` / `slack.rs`
    /// `start_*_bots()`, `resolve_agent_channel_token_via_reports_to` and its
    /// `reports_to` cascade callers), `duduclaw-gateway`'s
    /// `dispatcher::forward_to_channel` delegation-forward path (via the new
    /// `config_crypto::decrypt_config_field_async`), and
    /// `duduclaw-inference`'s `inference.toml` `[openai_compat] api_key`
    /// (`OpenAiCompatConfig::resolved_api_key`).
    ///
    /// This method itself is not removed — `duduclaw-gateway`'s
    /// `config_crypto::decrypt_config_field` keeps a synchronous twin for any
    /// future genuinely-synchronous caller (none exists in production today)
    /// and this module's own tests exercise the fail-closed contract
    /// directly. A *real* synchronous dead end (no tokio runtime reachable at
    /// all, e.g. very early process bootstrap before the gateway's runtime
    /// exists) would still need this method; none of the call sites converted
    /// here turned out to be one.
    pub fn resolve_sync(&self, home_dir: &Path) -> Option<Secret> {
        match self.resolve_local(home_dir) {
            LocalOutcome::Resolved(secret) => Some(secret),
            LocalOutcome::Unset => None,
            LocalOutcome::NeedsBackend(uri) => {
                tracing::warn!(
                    backend = %uri.backend,
                    "credential is a secret:// reference to a network backend but \
                     was read from a synchronous path that cannot fetch it — \
                     treating as unset (fail-closed)"
                );
                None
            }
        }
    }
}

/// Outcome of [`SecretRef::resolve_local`].
#[derive(Debug)]
pub enum LocalOutcome {
    /// A value was obtained without network I/O.
    Resolved(Secret),
    /// Nothing configured, or a reference that resolved to nothing.
    Unset,
    /// A reference to a network backend; only `async` callers can go further.
    NeedsBackend(SecretUri),
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::CryptoEngine;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "duduclaw-secretref-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn encrypt(&self, plain: &str) -> String {
            let keyfile = self.0.join(".keyfile");
            let key = if keyfile.exists() {
                let bytes = std::fs::read(&keyfile).unwrap();
                let mut k = [0u8; 32];
                k.copy_from_slice(&bytes);
                k
            } else {
                let k = CryptoEngine::generate_key().unwrap();
                std::fs::write(&keyfile, k).unwrap();
                k
            };
            CryptoEngine::new(&key).unwrap().encrypt_string(plain).unwrap()
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── Secret: the empty-value rule is structural ──────────────

    #[test]
    fn secret_rejects_empty_string() {
        assert!(Secret::new("").is_none());
        assert_eq!(Secret::new("x").unwrap().expose(), "x");
    }

    #[test]
    fn secret_never_prints_its_value() {
        let s = Secret::new("super-secret-token").unwrap();
        assert_eq!(format!("{s}"), "<redacted>");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert!(!format!("{s:?} {s}").contains("super-secret"));
    }

    #[test]
    fn expose_owned_hands_back_the_value() {
        let s = Secret::new("tok").unwrap();
        assert_eq!(s.expose_owned(), "tok");
    }

    // ── classify: the three plaintext states ────────────────────

    #[test]
    fn absent_plaintext_falls_through_to_ciphertext() {
        let home = TempHome::new();
        let enc = home.encrypt("from-enc");
        let r = SecretRef::classify(Some(&enc), None);
        assert_eq!(
            r.resolve_sync(home.path()).unwrap().expose(),
            "from-enc"
        );
        assert!(!r.has_residue());
    }

    #[test]
    fn blank_plaintext_is_a_removal_marker_that_suppresses_stale_ciphertext() {
        let home = TempHome::new();
        let enc = home.encrypt("stale");
        let r = SecretRef::classify(Some(&enc), Some(""));
        assert!(r.resolve_sync(home.path()).is_none());
        assert!(!r.describe().configured);
    }

    #[test]
    fn typed_form_treats_blank_plaintext_as_absent_not_removed() {
        // agent.toml's typed structs cannot express "present but blank", so an
        // enc-only channel must still resolve.
        let home = TempHome::new();
        let enc = home.encrypt("agent-token");
        let r = SecretRef::from_typed(Some(&enc), "");
        assert_eq!(
            r.resolve_sync(home.path()).unwrap().expose(),
            "agent-token"
        );
    }

    #[test]
    fn ciphertext_wins_over_plaintext_and_flags_residue() {
        let home = TempHome::new();
        let enc = home.encrypt("encrypted-wins");
        let r = SecretRef::classify(Some(&enc), Some("plaintext-loses"));
        assert_eq!(
            r.resolve_sync(home.path()).unwrap().expose(),
            "encrypted-wins"
        );
        assert!(r.has_residue(), "plaintext twin next to _enc is residue");
        assert!(r.describe().residue);
    }

    #[test]
    fn undecryptable_ciphertext_falls_back_to_plaintext() {
        // No keyfile at all → the ciphertext is unusable; the pre-existing
        // behaviour is to fall through rather than fail the channel.
        let home = TempHome::new();
        let r = SecretRef::classify(Some("not-real-ciphertext"), Some("fallback-token"));
        assert_eq!(
            r.resolve_sync(home.path()).unwrap().expose(),
            "fallback-token"
        );
    }

    #[test]
    fn nothing_configured_is_unset() {
        let home = TempHome::new();
        let r = SecretRef::classify(None, None);
        assert!(r.resolve_sync(home.path()).is_none());
        let st = r.describe();
        assert!(!st.configured);
        assert_eq!(st.source, SourceKind::Unset);
        assert!(st.writable);
    }

    // ── secret:// references ────────────────────────────────────

    #[test]
    fn env_reference_resolves_synchronously() {
        let home = TempHome::new();
        let var = format!("DUDUCLAW_SECRETREF_TEST_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "from-env") };
        let r = SecretRef::classify(None, Some(&format!("secret://env/{var}")));
        let got = r.resolve_sync(home.path());
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got.unwrap().expose(), "from-env");
    }

    #[test]
    fn env_reference_to_unset_variable_is_unset_not_literal() {
        let home = TempHome::new();
        let r = SecretRef::classify(None, Some("secret://env/DUDUCLAW_DEFINITELY_UNSET_XYZ"));
        assert!(r.resolve_sync(home.path()).is_none());
    }

    /// The bug this module exists to kill: a `secret://` string must never be
    /// handed back as if it were the credential.
    #[test]
    fn network_reference_is_never_returned_as_a_literal_token() {
        let home = TempHome::new();
        for reference in [
            "secret://vault/telegram_bot_token",
            "secret://onepassword/tg",
            "secret://infisical/tg",
            "secret://local/tg",
        ] {
            let r = SecretRef::classify(None, Some(reference));
            let got = r.resolve_sync(home.path());
            assert!(
                got.is_none(),
                "{reference} must fail closed on a sync path, got {got:?}"
            );
            assert!(matches!(
                r.resolve_local(home.path()),
                LocalOutcome::NeedsBackend(_)
            ));
        }
    }

    #[test]
    fn malformed_reference_is_unset_not_literal() {
        let home = TempHome::new();
        for bad in ["secret://bogus/name", "secret://oops", "secret://vault/"] {
            let r = SecretRef::classify(None, Some(bad));
            assert!(
                r.resolve_sync(home.path()).is_none(),
                "{bad} must not resolve"
            );
            assert!(!r.describe().configured, "{bad} must not read as configured");
        }
    }

    #[test]
    fn encrypted_pointer_to_env_reference_resolves() {
        // `<field>_enc` whose plaintext is itself a reference — supported by
        // the pre-existing async path, preserved here.
        let home = TempHome::new();
        let var = format!("DUDUCLAW_SECRETREF_PTR_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "via-pointer") };
        let enc = home.encrypt(&format!("secret://env/{var}"));
        let r = SecretRef::classify(Some(&enc), None);
        let got = r.resolve_sync(home.path());
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got.unwrap().expose(), "via-pointer");
    }

    // ── P1: keychain / file references ──────────────────────────

    #[test]
    fn file_reference_resolves_synchronously_from_the_home_secrets_root() {
        let home = TempHome::new();
        std::fs::create_dir_all(home.path().join("secrets")).unwrap();
        let p = home.path().join("secrets").join("tg");
        std::fs::write(&p, "file-token\n").unwrap();
        let r = SecretRef::classify(None, Some(&format!("secret://file/{}", p.display())));
        assert_eq!(r.resolve_sync(home.path()).unwrap().expose(), "file-token");
        let st = r.describe();
        assert_eq!(st.source, SourceKind::File);
        assert!(st.configured);
        assert!(!st.writable, "a mounted file is rotated by its mounter");
    }

    #[test]
    fn file_reference_outside_the_roots_is_unset_not_literal() {
        let home = TempHome::new();
        let p = home.path().join("loose-secret");
        std::fs::write(&p, "should-not-be-read").unwrap();
        let r = SecretRef::classify(None, Some(&format!("secret://file/{}", p.display())));
        assert!(
            r.resolve_sync(home.path()).is_none(),
            "containment failure must be unset, never the path or the contents"
        );
        // Still reported as configured — the reference exists; it is the
        // resolve that failed, and `describe()` deliberately does not resolve.
        assert!(r.describe().configured);
    }

    #[test]
    fn file_and_keychain_uris_round_trip_through_display() {
        use crate::secret_manager::SecretUri;
        for s in [
            "secret://file//run/secrets/tg_token",
            "secret://keychain/duduclaw/telegram",
        ] {
            assert_eq!(SecretUri::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn keychain_reference_describes_without_touching_the_os() {
        let home = TempHome::new();
        let r = SecretRef::classify(None, Some("secret://keychain/duduclaw/telegram"));
        let st = r.describe();
        assert_eq!(st.source, SourceKind::Keychain);
        assert_eq!(st.source_label, "keychain:duduclaw/telegram");
        assert!(st.configured && !st.writable);
        // In a build without the `keychain` feature the lookup fails loudly and
        // yields unset — never the URI itself.
        #[cfg(not(feature = "keychain"))]
        assert!(r.resolve_sync(home.path()).is_none());
        let _ = home;
    }

    // ── P1: the ambiguous single-field dialect ──────────────────

    #[test]
    fn from_single_reads_ciphertext_then_falls_back_to_plaintext() {
        let home = TempHome::new();
        let enc = home.encrypt("decrypted-value");
        assert_eq!(
            SecretRef::from_single(&enc)
                .resolve_sync(home.path())
                .unwrap()
                .expose(),
            "decrypted-value"
        );
        // Not ciphertext at all → the field *is* the value (legacy behaviour).
        assert_eq!(
            SecretRef::from_single("literal-shared-secret")
                .resolve_sync(home.path())
                .unwrap()
                .expose(),
            "literal-shared-secret"
        );
        assert!(SecretRef::from_single("   ").resolve_sync(home.path()).is_none());
        assert!(!SecretRef::from_single("").describe().configured);
    }

    /// The dialect-6 bug: a single-field credential holding a network
    /// reference used to be sent to the remote service verbatim.
    #[test]
    fn from_single_never_returns_a_reference_as_the_value() {
        let home = TempHome::new();
        let r = SecretRef::from_single("secret://vault/apps_script_shared_secret");
        assert!(r.resolve_sync(home.path()).is_none());
        assert_eq!(r.describe().source, SourceKind::Vault);

        let var = format!("DUDUCLAW_SINGLE_FIELD_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "from-env-single") };
        let got = SecretRef::from_single(&format!("secret://env/{var}")).resolve_sync(home.path());
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got.unwrap().expose(), "from-env-single");
    }

    #[test]
    fn from_single_never_reports_residue() {
        let home = TempHome::new();
        let enc = home.encrypt("v");
        assert!(!SecretRef::from_single(&enc).has_residue());
        assert_eq!(
            SecretRef::from_single(&enc).describe().source,
            SourceKind::Ambiguous,
            "describe must not pretend to know which of the two it is"
        );
    }

    // ── describe ────────────────────────────────────────────────

    #[test]
    fn describe_reports_source_without_the_value() {
        let home = TempHome::new();
        let enc = home.encrypt("v");
        let inline = SecretRef::classify(Some(&enc), None).describe();
        assert_eq!(inline.source, SourceKind::Inline);
        assert_eq!(inline.source_label, "encrypted(keyfile)");
        assert!(inline.configured && inline.writable);
        assert!(!inline.source_label.contains(&enc));

        let legacy = SecretRef::classify(None, Some("bare-token")).describe();
        assert_eq!(legacy.source, SourceKind::Legacy);
        assert_eq!(legacy.source_label, "plaintext(legacy)");
        assert!(!legacy.source_label.contains("bare-token"));

        let vault = SecretRef::classify(None, Some("secret://vault/kv/tg")).describe();
        assert_eq!(vault.source, SourceKind::Vault);
        assert_eq!(vault.source_label, "vault:kv/tg");
        assert!(vault.configured);
        assert!(!vault.writable, "external references are not UI-writable");

        let env = SecretRef::classify(None, Some("secret://env/TG_TOKEN")).describe();
        assert_eq!(env.source, SourceKind::Env);
        assert_eq!(env.source_label, "env:TG_TOKEN");
        assert!(!env.writable);
    }

    #[test]
    fn describe_serializes_without_the_value() {
        let home = TempHome::new();
        let enc = home.encrypt("top-secret-value");
        let st = SecretRef::classify(Some(&enc), Some("plain-secret-value")).describe();
        let json = serde_json::to_string(&st).unwrap();
        assert!(!json.contains("top-secret-value"));
        assert!(!json.contains("plain-secret-value"));
        assert!(!json.contains(&enc));
        assert!(json.contains("\"residue\":true"));
    }

    /// §1.5: residue is detected even when the plaintext twin is inert.
    #[test]
    fn residue_detection_matches_the_field_1_5_incident_shape() {
        let home = TempHome::new();
        let enc = home.encrypt("real-oauth-token");
        let r = SecretRef::classify(Some(&enc), Some("leftover-plaintext-oauth-token"));
        assert!(r.has_residue());
        // Blank plaintext next to a ciphertext is a removal marker, not residue
        // worth reporting as a leak — but we still surface it so `doctor` can
        // point at the stale ciphertext.
        let removed = SecretRef::classify(Some(&enc), Some(""));
        assert!(!removed.has_residue());
        assert!(!SecretRef::classify(Some(&enc), None).has_residue());
        assert!(!SecretRef::classify(None, Some("plain")).has_residue());
    }

    // ── async resolve parity ────────────────────────────────────

    #[tokio::test]
    async fn async_resolve_matches_sync_for_local_sources() {
        let home = TempHome::new();
        let cfg = SecretManagerConfig::default();
        let enc = home.encrypt("same-value");
        let r = SecretRef::classify(Some(&enc), Some("other"));
        assert_eq!(
            r.resolve(&cfg, home.path()).await.unwrap().expose(),
            r.resolve_sync(home.path()).unwrap().expose()
        );

        let unset = SecretRef::classify(None, None);
        assert!(unset.resolve(&cfg, home.path()).await.is_none());
    }

    #[tokio::test]
    async fn async_resolve_of_unbuildable_backend_is_none_not_literal() {
        // vault backend with no token configured → build fails → None.
        let home = TempHome::new();
        let cfg = SecretManagerConfig::default();
        let r = SecretRef::classify(None, Some("secret://vault/whatever"));
        assert!(r.resolve(&cfg, home.path()).await.is_none());
    }
}
