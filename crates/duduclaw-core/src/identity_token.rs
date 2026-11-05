//! WP21 debt ⑧ — cryptographic binding for the MCP caller identity.
//!
//! # The hole this closes
//!
//! Every WP21 authorization decision (`can_delegate`, the org-subtree gate,
//! namespace isolation) is judged against *one string*: the `DUDUCLAW_AGENT_ID`
//! env var that `duduclaw mcp-server` reads at startup. That string is injected
//! by DuDuClaw when it spawns an agent's CLI, but nothing stops the agent from
//! spawning a *second* `duduclaw mcp-server` with `DUDUCLAW_AGENT_ID=ceo` and
//! talking to it directly. The entire department/rank isolation is one
//! `env` assignment away from being bypassed.
//!
//! This module adds a keyed MAC over the claimed id, so a caller must *prove*
//! the id was issued by DuDuClaw rather than merely assert it:
//!
//! ```text
//!   DUDUCLAW_AGENT_ID    = "sales-rep"
//!   DUDUCLAW_AGENT_TOKEN = hex(HMAC-SHA256(identity.key, "sales-rep"))
//! ```
//!
//! # Threat model — read this before trusting it
//!
//! This defends against **env-var impersonation** (an agent re-spawning the MCP
//! server under someone else's id). It is **not** process isolation:
//!
//! * The key lives at `~/.duduclaw/identity.key` (0600). An agent that has an
//!   unrestricted `Bash` tool runs as the same OS user and can simply read that
//!   file, mint its own tokens, and impersonate anyone. `identity.key` should
//!   therefore be on the `agent_guard` sensitive-file list (see the note in the
//!   WP21 report — that list is owned by another work item).
//! * Likewise, the same OS user can read another process's environment on most
//!   platforms, so a token *in flight* is not secret from a co-resident agent.
//! * Real containment for a `Bash`-capable agent is the container sandbox /
//!   `duduclaw-sandbox` native confinement, not this MAC.
//! * **`.mcp.json` read surface** (WP22 T3): every write point that persists a
//!   `DUDUCLAW_AGENT_ID` / `DUDUCLAW_AGENT_TOKEN` pair to disk — per-agent
//!   `.mcp.json`, and the codex/grok equivalents under
//!   `<agent_dir>/.codex/config.toml`, `.grok/config.toml` — now sets
//!   the file to 0600 (`duduclaw_core::platform::set_owner_only`), so a peer
//!   agent's token pair is no longer world/group-readable off disk. That only
//!   narrows the read surface to processes running as the same OS user — a
//!   `Bash`-capable co-resident agent can still open a peer's `.mcp.json`
//!   directly, same as reading `identity.key` above. What actually stops the
//!   *use* of a token lifted that way is a second, independent lock: pasting a
//!   peer's id+token pair into *your own* `.mcp.json` is caught by
//!   `org_field_guard`'s [`crate::org_field_guard::ProtectedSurface::AgentMcpJson`]
//!   identity-face guard, which freezes that file's `DUDUCLAW_AGENT_ID`/
//!   `DUDUCLAW_AGENT_TOKEN` pair against being overwritten with someone
//!   else's. 0600 is defense-in-depth against casual/off-box disclosure
//!   (backups, misconfigured shares, non-agent local users), not a substitute
//!   for that guard.
//!
//! What it does buy: the "just set the env var" bypass — by far the cheapest
//! and most likely one, and the only one available to agents whose capability
//! set has no shell — stops working, and a *failed* attempt is loggable.
//!
//! # Rollout
//!
//! Strict by default; soft mode requires explicit false and a future UTC grace
//! deadline no more than 30 days away. The deadline is rechecked on verification.
//!
//! | `identity.key` | `DUDUCLAW_AGENT_TOKEN` | `require_identity_token` | Outcome |
//! |---|---|---|---|
//! | absent, never initialized | (any) | `false` + valid grace | [`IdentityVerdict::Disabled`] |
//! | absent | (any) | `true` | [`IdentityVerdict::Rejected`] |
//! | missing after initialization | (any) | (any) | [`IdentityVerdict::Rejected`] |
//! | unreadable / malformed / unsafe permissions | (any) | (any) | [`IdentityVerdict::Rejected`] |
//! | present | valid | (any) | [`IdentityVerdict::Verified`] |
//! | present | missing/invalid | `false` + valid grace | [`IdentityVerdict::Unverified`] — one `warn!` per process, then proceed |
//! | present | missing/invalid | `true` | [`IdentityVerdict::Rejected`] — fail closed |

use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;
#[cfg(test)]
mod tests_security;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Proof that the accompanying [`crate::ENV_AGENT_ID`] was issued by DuDuClaw:
/// `hex(HMAC-SHA256(identity.key, agent_id))`.
///
/// Injected next to `DUDUCLAW_AGENT_ID` at every MCP env-assembly point. Absent
/// when no `identity.key` exists (feature not enabled).
pub const ENV_AGENT_TOKEN: &str = "DUDUCLAW_AGENT_TOKEN";

/// File name of the per-install identity secret, under the DuDuClaw home.
pub const IDENTITY_KEY_FILE: &str = "identity.key";
/// Durable evidence that this installation must never silently mint a new key.
pub const IDENTITY_INITIALIZED_FILE: &str = "identity.key.initialized";
pub const IDENTITY_LOCK_FILE: &str = "identity.key.lock";

/// Key length in bytes. 32 = the HMAC-SHA256 block-optimal size (no hashing of
/// the key on use) and 256 bits of entropy.
pub const IDENTITY_KEY_LEN: usize = 32;

/// `<home>/identity.key`.
pub fn identity_key_path(home_dir: &Path) -> PathBuf {
    home_dir.join(IDENTITY_KEY_FILE)
}

/// Read a usable identity key; missing or invalid keys return `None`.
/// Authorization must use [`verify_claim`], which distinguishes absence from
/// a broken key and rejects the latter even in soft mode.
pub fn load_key(home_dir: &Path) -> Option<Vec<u8>> {
    match read_key(home_dir) {
        Ok(key) => key,
        Err(error) => {
            error_key_once(home_dir, &error);
            None
        }
    }
}

/// Only a genuinely absent file is disabled; every other failure is an error.
fn read_key(home_dir: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let path = identity_key_path(home_dir);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::other("identity key must be a regular file"));
    }
    let file = open_private(&path, false, false)?;
    let mut raw = Vec::with_capacity(IDENTITY_KEY_LEN + 1);
    file.take((IDENTITY_KEY_LEN + 1) as u64).read_to_end(&mut raw)?;
    if raw.len() != IDENTITY_KEY_LEN {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
            "identity key must contain exactly 32 bytes"));
    }
    Ok(Some(raw))
}

static KEY_ERROR_LOGGED: std::sync::Once = std::sync::Once::new();

fn error_key_once(home_dir: &Path, error: &std::io::Error) {
    KEY_ERROR_LOGGED.call_once(|| {
        tracing::error!(
            path = %identity_key_path(home_dir).display(),
            error = %error,
            "MCP caller identity rejected: signing key unavailable or invalid. \
             Restore the original 32-byte identity.key with owner-only permissions \
             (0600 on Unix), then restart the gateway. Strict mode also requires \
             a valid DuDuClaw-issued token; disabling strict mode cannot bypass a broken key."
        );
    });
}

fn initialized(home: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(home.join(IDENTITY_INITIALIZED_FILE)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn open_private(path: &Path, write: bool, create_new: bool) -> std::io::Result<std::fs::File> {
    windows::open(path, write, create_new)
}

#[cfg(not(windows))]
fn open_private(path: &Path, write: bool, create_new: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(write).create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("identity state must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        validate_unix_owner_links(metadata.uid(), metadata.nlink(), unsafe { libc::geteuid() })?;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
                "identity state permissions must be 0600"));
        }
    }
    #[cfg(target_os = "macos")]
    reject_macos_acl(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_unix_owner_links(uid: u32, nlink: u64, effective_uid: u32) -> std::io::Result<()> {
    if uid != effective_uid || nlink != 1 {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "identity state must be owned by the effective user and have exactly one hard link"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn reject_macos_acl(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // Darwin's extended ACL API is in libc, but not exposed by the libc crate.
    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(acl: *mut libc::c_void, id: libc::c_int, entry: *mut *mut libc::c_void) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    // Darwin acl_get_fd returns the extended ACL, or ENOENT when none exists.
    let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) { return Ok(()) }
        return Err(error);
    }
    let mut entry = std::ptr::null_mut();
    let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
    let error = std::io::Error::last_os_error();
    unsafe { acl_free(acl); }
    if result == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "identity state must have no extended ACL entries"));
    }
    // Darwin reports EINVAL when the independent, valid ACL has no first entry.
    if result == -1 && error.raw_os_error() == Some(libc::EINVAL) { return Ok(()) }
    Err(error)
}

fn sync_home(home: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(home)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = home;
    Ok(())
}

struct PendingFile(PathBuf);
impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Publish a fully flushed private file without ever replacing the destination.
/// The hard link is atomic and fails if any destination entry already exists.
fn install_new(home: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|e| std::io::Error::other(format!("CSPRNG unavailable: {e}")))?;
    let temp_path = home.join(format!(".identity-{}.tmp", hex_lower(&random)));
    let mut file = open_private(&temp_path, true, true)?;
    let temp = PendingFile(temp_path);
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::hard_link(&temp.0, home.join(name))?;
    sync_home(home)?;
    drop(file);
    drop(temp);
    sync_home(home)
}

fn mark_initialized(home: &Path) -> std::io::Result<()> {
    if !initialized(home)? {
        install_new(home, IDENTITY_INITIALIZED_FILE, b"v1\n")?;
    }
    Ok(())
}

/// Gateway startup only. Serialize first creation across processes, preserve
/// existing keys, and persist a marker before publishing the first key. A crash
/// after the marker is durable requires operator recovery, never silent rotation.
pub fn ensure_key(home_dir: &Path) -> std::io::Result<Vec<u8>> {
    crate::config::create_home_dir(home_dir)?;
    let lock_path = home_dir.join(IDENTITY_LOCK_FILE);
    let lock = match open_private(&lock_path, true, true) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists =>
            open_private(&lock_path, true, false)?,
        Err(error) => return Err(error),
    };
    fs2::FileExt::lock_exclusive(&lock)?;
    if let Some(existing) = read_key(home_dir)? {
        mark_initialized(home_dir)?;
        return Ok(existing);
    }
    if initialized(home_dir)? {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound,
            "initialized identity key is missing; restore the original key"));
    }
    let mut key = vec![0u8; IDENTITY_KEY_LEN];
    getrandom::fill(&mut key)
        .map_err(|e| std::io::Error::other(format!("CSPRNG unavailable: {e}")))?;
    mark_initialized(home_dir)?;
    install_new(home_dir, IDENTITY_KEY_FILE, &key)?;
    Ok(key)
}

/// `hex(HMAC-SHA256(key, agent_id))`.
///
/// The agent id is MAC'd as raw UTF-8 bytes. No length prefix or separator is
/// needed because the message is a single field — there is no concatenation
/// ambiguity to exploit.
pub fn mint_token(key: &[u8], agent_id: &str) -> String {
    // `new_from_slice` only errors for key sizes the algorithm rejects;
    // HMAC accepts any length, so this is infallible in practice.
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(agent_id.as_bytes());
    let tag = mac.finalize().into_bytes();
    let mut out = String::with_capacity(tag.len() * 2);
    for b in tag.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Constant-time check that `token` is the tag for `agent_id` under `key`.
///
/// Comparison runs through `Mac::verify_slice`, whose equality check is the
/// `subtle` crate's constant-time primitive — it does not short-circuit on the
/// first differing byte, so an attacker cannot recover the tag one byte at a
/// time by timing. Hex decoding happens first and is length-independent of the
/// secret. A malformed (non-hex / wrong-length) token is rejected without ever
/// reaching the comparison, which leaks only that the *input was malformed* —
/// not any bits of the real tag.
pub fn verify_token(key: &[u8], agent_id: &str, token: &str) -> bool {
    let Some(bytes) = decode_hex(token.trim()) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return false;
    };
    mac.update(agent_id.as_bytes());
    mac.verify_slice(&bytes).is_ok()
}

// ── Signed internal requests ─────────────────────────────────────────────────

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn internal_request_mac(
    internal_key: &[u8],
    agent_id: &str,
    agent_token: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> HmacSha256 {
    use sha2::Digest;
    let body_hash = hex_lower(&Sha256::digest(body));
    let mut mac = HmacSha256::new_from_slice(internal_key).expect("HMAC accepts any key length");
    for (i, part) in [agent_id, agent_token, timestamp, nonce, body_hash.as_str()].iter().enumerate() {
        if i > 0 {
            mac.update(b"\n");
        }
        mac.update(part.as_bytes());
    }
    mac
}

/// The signature of one gateway-internal HTTP request (the computer-use route):
/// `hex(HMAC-SHA256(key = internal_key, agent_id + "\n" + agent_token + "\n" +
/// timestamp + "\n" + nonce + "\n" + hex(sha256(body))))`.
///
/// Neither the internal key nor the identity token travels on the wire: a
/// local process that binds the gateway port while the gateway is down
/// receives only a signature that is useless for any other request. The
/// fields are newline-separated; the caller validates that `agent_id`,
/// `timestamp` and `nonce` contain no newline (the receiver does).
pub fn internal_request_signature(
    internal_key: &[u8],
    agent_id: &str,
    agent_token: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> String {
    hex_lower(&internal_request_mac(internal_key, agent_id, agent_token, timestamp, nonce, body).finalize().into_bytes())
}

/// Constant-time check of an [`internal_request_signature`] (through
/// `Mac::verify_slice`). A malformed (non-hex / wrong-length) signature is
/// rejected without reaching the comparison.
pub fn verify_internal_request_signature(
    internal_key: &[u8],
    agent_id: &str,
    agent_token: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
    signature: &str,
) -> bool {
    let Some(bytes) = decode_hex(signature.trim()) else {
        return false;
    };
    internal_request_mac(internal_key, agent_id, agent_token, timestamp, nonce, body)
        .verify_slice(&bytes)
        .is_ok()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

// ── Env assembly (injection side) ────────────────────────────────────────────

/// The `(DUDUCLAW_AGENT_ID, id)` pair plus, when `identity.key` exists, the
/// matching `(DUDUCLAW_AGENT_TOKEN, tag)` pair.
///
/// Single source of truth for every MCP env-assembly point (Claude `.mcp.json`,
/// Codex `config.toml`, Grok). Sibling of
/// [`crate::mcp_forward_env_vars`], which carries the *process-wide* forward set;
/// this one is per-call because the identity is.
///
/// No key ⇒ just the id, i.e. exactly the pre-WP21 env block.
pub fn agent_identity_env_vars(home_dir: &Path, agent_id: &str) -> Vec<(String, String)> {
    let mut out = vec![(crate::ENV_AGENT_ID.to_string(), agent_id.to_string())];
    if let Some(key) = load_key(home_dir) {
        out.push((ENV_AGENT_TOKEN.to_string(), mint_token(&key, agent_id)));
    }
    out
}

/// [`agent_identity_env_vars`] against the ambient DuDuClaw home.
///
/// Convenience for the injection points that do not already have a home path in
/// scope; resolves it the same way the rest of the process does
/// ([`crate::duduclaw_home`]).
pub fn agent_identity_env_vars_default(agent_id: &str) -> Vec<(String, String)> {
    agent_identity_env_vars(&crate::duduclaw_home(), agent_id)
}

// ── Verification (MCP server side) ───────────────────────────────────────────

/// Outcome of checking the ambient `DUDUCLAW_AGENT_ID` / `DUDUCLAW_AGENT_TOKEN`
/// pair. See the module-level rollout table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityVerdict {
    /// No key or initialization marker, and soft mode was explicitly selected.
    Disabled,
    /// The claimed id carries a valid tag.
    Verified,
    /// Key present, tag missing or wrong, soft mode: proceed but warn.
    Unverified,
    /// Key invalid/unreadable, or strict mode without a key and valid token.
    Rejected,
}

impl IdentityVerdict {
    /// Whether the caller identity may be trusted for authorization.
    ///
    /// `Unverified` counts as trusted *on purpose*: soft mode's contract is
    /// "warn, change nothing". Only [`IdentityVerdict::Rejected`] withholds
    /// trust.
    pub fn is_trusted(self) -> bool {
        !matches!(self, IdentityVerdict::Rejected)
    }
}

/// The sentinel caller id substituted for a rejected claim.
///
/// WP-5B (2026-08): corrected a stale claim in this comment. `_` alone does
/// NOT make an id invalid — [`crate::is_valid_agent_id`] has always allowed
/// underscores (it accepts ASCII alphanumerics, `-`, and `_`); a value like
/// `"__untrusted__"` would pass that check on its own. What actually keeps
/// this sentinel from ever colliding with a real agent, being treated as
/// anyone's ancestor, sharing a department, or landing in the system-sender
/// allowlist is its **double-underscore prefix**: [`crate::is_reserved_agent_id`]
/// (in `delegation_policy`) reserves any id starting with `__`, and every
/// WP21 gate denies reserved ids without needing to know this constant
/// exists. That is the fail-closed property: a new gate written later
/// inherits it for free.
pub const UNTRUSTED_AGENT_ID: &str = "__untrusted__";

/// Verify the claimed identity in the current process environment.
///
/// `require` is `config.toml [delegation] require_identity_token`. Emits at most
/// one `warn!` per process for the soft-mode path (a per-call warning would
/// drown the log — the MCP server handles thousands of calls per session).
pub fn verify_env_identity(home_dir: &Path, require: bool) -> IdentityVerdict {
    let claimed = std::env::var(crate::ENV_AGENT_ID).unwrap_or_default();
    let token = std::env::var(ENV_AGENT_TOKEN).unwrap_or_default();
    verify_claim(home_dir, claimed.trim(), token.trim(), require)
}

/// Verify the exact ambient identity against a credential-bearing bridge's
/// requested employee. Soft mode never authorizes access to remote credentials.
/// Return the verified token so async bridge work can recheck it before use.
pub fn verified_bridge_env_token(home_dir: &Path, agent_id: &str) -> Result<String, &'static str> {
    let claimed = std::env::var(crate::ENV_AGENT_ID).unwrap_or_default();
    let token = std::env::var(ENV_AGENT_TOKEN).unwrap_or_default();
    verify_bridge_claim(home_dir, agent_id, &claimed, &token)?;
    Ok(token)
}

fn verify_bridge_claim(home_dir: &Path, agent_id: &str, claimed: &str, token: &str) -> Result<(), &'static str> {
    if claimed != agent_id || verify_claim(home_dir, claimed, token, true) != IdentityVerdict::Verified {
        return Err("MCP remote bridge requires a verified identity matching --agent");
    }
    Ok(())
}

/// Testable core of [`verify_env_identity`] with the env reads hoisted out.
pub fn verify_claim(
    home_dir: &Path,
    claimed_id: &str,
    token: &str,
    require: bool,
) -> IdentityVerdict {
    // Recheck even when the caller cached false or passed it directly. Expiry
    // must restore strict verification without waiting for a gateway restart.
    let require = if require { true } else {
        match crate::require_identity_token_from_home(home_dir) {
            Ok(require) => require,
            Err(_) => return IdentityVerdict::Rejected,
        }
    };
    // The doctor probe is reserved only against employee creation; the gateway
    // deliberately signs it for an MCP cold-start check, without system reach.
    if crate::is_system_sender(claimed_id)
        || (crate::is_reserved_agent_id(claimed_id) && claimed_id != "doctor-probe")
        || crate::is_reserved_queue_sender(claimed_id)
        || (!claimed_id.is_empty() && !crate::is_valid_agent_id(claimed_id))
    {
        return IdentityVerdict::Rejected;
    }
    let key = match read_key(home_dir) {
        Ok(Some(key)) => key,
        Ok(None) if !require && matches!(initialized(home_dir), Ok(false)) =>
            return IdentityVerdict::Disabled,
        Ok(None) => {
            error_key_once(home_dir, &std::io::Error::new(std::io::ErrorKind::NotFound,
                "identity key is required but missing"));
            return IdentityVerdict::Rejected;
        }
        Err(error) => {
            error_key_once(home_dir, &error);
            return IdentityVerdict::Rejected;
        }
    };
    // An absent claim is not an impersonation: the caller fell through to
    // `config.toml [general] default_agent`. In strict mode that fallback is
    // itself an escalation route (unset the env var, inherit the default
    // agent's authority), so it is refused there too.
    if !claimed_id.is_empty() && !token.is_empty() && verify_token(&key, claimed_id, token) {
        return IdentityVerdict::Verified;
    }
    if require {
        return IdentityVerdict::Rejected;
    }
    warn_unverified_once(claimed_id);
    IdentityVerdict::Unverified
}

static WARNED: std::sync::Once = std::sync::Once::new();

fn warn_unverified_once(claimed_id: &str) {
    WARNED.call_once(|| {
        let shown = if claimed_id.is_empty() {
            "(none)".to_string()
        } else {
            crate::truncate_chars(claimed_id, 64).to_string()
        };
        tracing::warn!(
            claimed_agent = %shown,
            "MCP caller identity is not cryptographically verified \
             (identity.key exists but DUDUCLAW_AGENT_TOKEN is missing or invalid). \
             Proceeding in soft mode; set [delegation] require_identity_token = true \
             in config.toml to refuse unverified callers."
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_grace_config(home: &Path) {
        std::fs::write(home.join("config.toml"), format!(
            "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = {:?}\n",
            (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())).unwrap();
    }

    fn key() -> Vec<u8> {
        (0u8..32).collect()
    }

    #[test]
    fn remote_bridge_requires_verified_matching_identity_even_in_soft_mode() {
        let home = TempDir::new().unwrap();
        write_grace_config(home.path());
        assert!(verify_bridge_claim(home.path(), "alice", "alice", "").is_err());
        let key = ensure_key(home.path()).unwrap();
        let token = mint_token(&key, "alice");
        assert!(verify_bridge_claim(home.path(), "alice", "alice", &token).is_ok());
        for (expected, claimed, supplied) in [
            ("alice", "alice", ""), // Missing token.
            ("bob", "alice", token.as_str()), // Valid identity, wrong --agent.
            ("bob", "bob", token.as_str()), // Forged pair using another employee's token.
            ("bob", "bob", "forged"),
            ("alice", "", token.as_str()), // Missing environment identity.
            ("alice", " alice ", token.as_str()), // Must match exactly.
        ] {
            assert!(verify_bridge_claim(home.path(), expected, claimed, supplied).is_err());
        }
        std::fs::remove_file(identity_key_path(home.path())).unwrap();
        assert!(verify_bridge_claim(home.path(), "alice", "alice", &token).is_err());
    }

    #[test]
    fn internal_request_signature_binds_every_field() {
        let k = b"ddc_prod_0123456789abcdef0123456789abcdef";
        let tok = mint_token(&key(), "alice");
        let sig = internal_request_signature(k, "alice", &tok, "1700000000", "00ff", b"{\"op\":\"status\"}");
        // Independent construction of the documented message.
        use sha2::Digest;
        let body_hash = hex_lower(&Sha256::digest(b"{\"op\":\"status\"}"));
        let mut mac = HmacSha256::new_from_slice(k).unwrap();
        mac.update(format!("alice\n{tok}\n1700000000\n00ff\n{body_hash}").as_bytes());
        assert_eq!(sig, hex_lower(&mac.finalize().into_bytes()));
        let ok = |id: &str, t: &str, ts: &str, n: &str, b: &[u8], s: &str| {
            verify_internal_request_signature(k, id, t, ts, n, b, s)
        };
        assert!(ok("alice", &tok, "1700000000", "00ff", b"{\"op\":\"status\"}", &sig));
        assert!(!ok("bob", &tok, "1700000000", "00ff", b"{\"op\":\"status\"}", &sig));
        assert!(!ok("alice", "00", "1700000000", "00ff", b"{\"op\":\"status\"}", &sig));
        assert!(!ok("alice", &tok, "1700000001", "00ff", b"{\"op\":\"status\"}", &sig));
        assert!(!ok("alice", &tok, "1700000000", "00fe", b"{\"op\":\"status\"}", &sig));
        assert!(!ok("alice", &tok, "1700000000", "00ff", b"{\"op\":\"stop\"}", &sig));
        assert!(!ok("alice", &tok, "1700000000", "00ff", b"{\"op\":\"status\"}", "zz"));
        assert!(!verify_internal_request_signature(b"other-key", "alice", &tok, "1700000000", "00ff", b"{\"op\":\"status\"}", &sig));
    }

    /// RFC 4231 test case 1 — a fixed vector proves this is real HMAC-SHA256
    /// and not a homegrown construction that happens to be self-consistent.
    #[test]
    fn matches_rfc4231_vector() {
        let k = vec![0x0bu8; 20];
        let mut mac = HmacSha256::new_from_slice(&k).unwrap();
        mac.update(b"Hi There");
        let tag = mac.finalize().into_bytes();
        let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // ...and that `mint_token` is exactly that construction.
        assert_eq!(mint_token(&k, "Hi There"), hex);
    }

    #[test]
    fn mint_verify_round_trip() {
        let k = key();
        let t = mint_token(&k, "sales-rep");
        assert_eq!(t.len(), 64, "hex tag is 32 bytes = 64 chars");
        assert!(verify_token(&k, "sales-rep", &t));
        // Whitespace from a shell-quoted env value is tolerated.
        assert!(verify_token(&k, "sales-rep", &format!("  {t}\n")));
    }

    #[test]
    fn rejects_wrong_id_key_and_garbage() {
        let k = key();
        let t = mint_token(&k, "sales-rep");
        // The whole point: a token minted for one id does not authorize another.
        assert!(!verify_token(&k, "ceo", &t));
        assert!(!verify_token(&[9u8; 32], "sales-rep", &t));
        for bad in ["", "zz", &t[..62], &format!("{t}00"), "not-hex-at-all!!"] {
            assert!(!verify_token(&k, "sales-rep", bad), "accepted {bad:?}");
        }
        // A single flipped hex digit must fail (constant-time compare, but the
        // *answer* still has to be right).
        let mut flipped = t.clone();
        flipped.replace_range(0..1, if t.starts_with('a') { "b" } else { "a" });
        assert!(!verify_token(&k, "sales-rep", &flipped));
    }

    #[test]
    fn ensure_key_is_stable_and_owner_only() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        assert!(load_key(home).is_none(), "no key before ensure_key");

        let k1 = ensure_key(home).unwrap();
        assert_eq!(k1.len(), IDENTITY_KEY_LEN);
        // Never rotates: tokens already handed to live subprocesses stay valid.
        let k2 = ensure_key(home).unwrap();
        assert_eq!(k1, k2);
        assert_eq!(load_key(home).unwrap(), k1);
        assert!(!identity_key_path(home).with_extension("key.tmp").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(identity_key_path(home))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "identity.key must not be readable by group/other");
        }
    }

    #[test]
    fn missing_required_key_is_rejected() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(verify_claim(tmp.path(), "alice", "", true), IdentityVerdict::Rejected);
    }

    #[test]
    fn malformed_key_is_rejected_in_both_modes() {
        let tmp = TempDir::new().unwrap();
        write_grace_config(tmp.path());
        ensure_key(tmp.path()).unwrap();
        for bytes in [vec![], b"short".to_vec(), vec![7; IDENTITY_KEY_LEN + 1]] {
            std::fs::write(identity_key_path(tmp.path()), &bytes).unwrap();
            assert_eq!(read_key(tmp.path()).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            for require in [false, true] {
                assert_eq!(verify_claim(tmp.path(), "alice", "", require), IdentityVerdict::Rejected);
            }
            assert!(ensure_key(tmp.path()).is_err());
            assert_eq!(std::fs::read(identity_key_path(tmp.path())).unwrap(), bytes);
        }
    }

    #[test]
    fn unreadable_key_is_rejected_in_both_modes() {
        let tmp = TempDir::new().unwrap();
        write_grace_config(tmp.path());
        // A directory cannot be read as a key, even by a privileged test user.
        std::fs::create_dir(identity_key_path(tmp.path())).unwrap();
        for require in [false, true] {
            assert_eq!(verify_claim(tmp.path(), "alice", "", require), IdentityVerdict::Rejected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_key_is_rejected_even_when_target_is_valid_or_missing() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        write_grace_config(tmp.path());
        let target_home = TempDir::new().unwrap();
        let k = ensure_key(target_home.path()).unwrap();
        let token = mint_token(&k, "alice");
        symlink(identity_key_path(target_home.path()), identity_key_path(tmp.path())).unwrap();
        for require in [false, true] {
            assert_eq!(verify_claim(tmp.path(), "alice", &token, require), IdentityVerdict::Rejected);
        }
        std::fs::remove_file(identity_key_path(target_home.path())).unwrap();
        for require in [false, true] {
            assert_eq!(verify_claim(tmp.path(), "alice", &token, require), IdentityVerdict::Rejected);
        }
        assert!(ensure_key(tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_key_permissions_are_rejected_in_both_modes() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        write_grace_config(tmp.path());
        let k = ensure_key(tmp.path()).unwrap();
        let token = mint_token(&k, "alice");
        for mode in [0o644, 0o660, 0o000] {
            std::fs::set_permissions(identity_key_path(tmp.path()), std::fs::Permissions::from_mode(mode)).unwrap();
            for require in [false, true] {
                assert_eq!(verify_claim(tmp.path(), "alice", &token, require), IdentityVerdict::Rejected);
            }
        }
    }

    #[test]
    fn corrupt_key_is_rejected_without_regeneration() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        ensure_key(home).unwrap();
        std::fs::write(identity_key_path(home), b"short").unwrap();
        assert!(load_key(home).is_none(), "wrong-length key must not be used");
        assert!(ensure_key(home).is_err());
        assert_eq!(std::fs::read(identity_key_path(home)).unwrap(), b"short");
    }

    #[test]
    fn env_vars_carry_token_only_when_enabled() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        let before = agent_identity_env_vars(home, "sales-rep");
        assert_eq!(before.len(), 1, "no key ⇒ pre-WP21 env block, byte-identical");
        assert_eq!(before[0].0, crate::ENV_AGENT_ID);

        let k = ensure_key(home).unwrap();
        let after = agent_identity_env_vars(home, "sales-rep");
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].0, ENV_AGENT_TOKEN);
        assert!(verify_token(&k, "sales-rep", &after[1].1));
        // Each agent gets its own tag — one cannot be replayed as another.
        let other = agent_identity_env_vars(home, "ceo");
        assert_ne!(after[1].1, other[1].1);
        assert!(!verify_token(&k, "ceo", &after[1].1));
    }

    /// The six-cell behaviour matrix from the module docs.
    #[test]
    fn verdict_matrix() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        std::fs::write(home.join("config.toml"), format!(
            "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = {:?}\n",
            (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())).unwrap();
        // 1-2. No key is disabled only in soft mode.
        assert_eq!(
            verify_claim(home, "sales-rep", "", false),
            IdentityVerdict::Disabled
        );
        assert_eq!(
            verify_claim(home, "sales-rep", "deadbeef", true),
            IdentityVerdict::Rejected
        );

        let k = ensure_key(home).unwrap();
        let good = mint_token(&k, "sales-rep");

        // 3. Valid token ⇒ Verified in both modes.
        assert_eq!(
            verify_claim(home, "sales-rep", &good, false),
            IdentityVerdict::Verified
        );
        assert_eq!(
            verify_claim(home, "sales-rep", &good, true),
            IdentityVerdict::Verified
        );

        // 4. Soft mode tolerates missing / wrong / cross-agent tokens.
        for (id, tok) in [("sales-rep", ""), ("sales-rep", "00"), ("ceo", good.as_str())] {
            assert_eq!(
                verify_claim(home, id, tok, false),
                IdentityVerdict::Unverified,
                "soft mode must not break {id}/{tok}"
            );
        }

        // 5. Strict mode refuses the same cases — including the replay of a
        //    valid `sales-rep` tag under the id `ceo`.
        for (id, tok) in [("sales-rep", ""), ("sales-rep", "00"), ("ceo", good.as_str())] {
            assert_eq!(
                verify_claim(home, id, tok, true),
                IdentityVerdict::Rejected,
                "strict mode must refuse {id}/{tok}"
            );
        }

        // 6. Strict mode also refuses an *absent* claim: falling through to
        //    `default_agent` by unsetting the env var is itself an escalation.
        assert_eq!(verify_claim(home, "", &good, true), IdentityVerdict::Rejected);
        assert_eq!(verify_claim(home, "", "", false), IdentityVerdict::Unverified);

        assert!(IdentityVerdict::Disabled.is_trusted());
        assert!(IdentityVerdict::Verified.is_trusted());
        assert!(IdentityVerdict::Unverified.is_trusted());
        assert!(!IdentityVerdict::Rejected.is_trusted());
    }

    /// The sentinel must live in the reserved id space, so no real agent can
    /// ever be created with that name and inherit whatever it is granted.
    #[test]
    fn untrusted_sentinel_is_reserved_and_not_a_system_sender() {
        assert!(crate::is_reserved_agent_id(UNTRUSTED_AGENT_ID));
        assert!(!crate::is_system_sender(UNTRUSTED_AGENT_ID));
    }
}
