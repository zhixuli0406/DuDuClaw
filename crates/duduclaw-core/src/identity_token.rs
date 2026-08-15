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
//!   `.mcp.json`, and the codex/gemini/antigravity/grok equivalents under
//!   `<agent_dir>/.codex/config.toml`, `.gemini/settings.json`,
//!   `.gemini/antigravity-cli/settings.json`, `.grok/config.toml` — now sets
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
//! Deliberately inert on existing installs:
//!
//! | `identity.key` | `DUDUCLAW_AGENT_TOKEN` | `require_identity_token` | Outcome |
//! |---|---|---|---|
//! | absent | (any) | (any) | [`IdentityVerdict::Disabled`] — byte-identical to pre-WP21 behaviour |
//! | present | valid | (any) | [`IdentityVerdict::Verified`] |
//! | present | missing/invalid | `false` (default) | [`IdentityVerdict::Unverified`] — one `warn!` per process, then proceed |
//! | present | missing/invalid | `true` | [`IdentityVerdict::Rejected`] — fail closed |

use std::path::{Path, PathBuf};

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

/// Key length in bytes. 32 = the HMAC-SHA256 block-optimal size (no hashing of
/// the key on use) and 256 bits of entropy.
pub const IDENTITY_KEY_LEN: usize = 32;

/// `<home>/identity.key`.
pub fn identity_key_path(home_dir: &Path) -> PathBuf {
    home_dir.join(IDENTITY_KEY_FILE)
}

/// Read the identity key, or `None` when the feature is not enabled.
///
/// `None` on *any* problem — missing file, wrong length, unreadable — because
/// every caller's fallback is "behave exactly as before", never "invent a key".
/// A key of the wrong length is a corrupted file, and silently accepting it
/// would make tokens minted by one process unverifiable by another.
pub fn load_key(home_dir: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(identity_key_path(home_dir)).ok()?;
    (raw.len() == IDENTITY_KEY_LEN).then_some(raw)
}

/// Generate `identity.key` if absent; return the key either way.
///
/// Called once at gateway startup. Never overwrites an existing key (that would
/// invalidate every token already handed to a running CLI subprocess). Written
/// via a temp file + rename so a crash mid-write cannot leave a truncated key,
/// and chmod'ed 0600 *before* the rename so it is never briefly world-readable.
pub fn ensure_key(home_dir: &Path) -> std::io::Result<Vec<u8>> {
    if let Some(existing) = load_key(home_dir) {
        return Ok(existing);
    }
    let path = identity_key_path(home_dir);
    // A wrong-length (corrupt) file reaches here too; regenerating is correct —
    // it was unusable, and nothing could have been verified against it.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut key = vec![0u8; IDENTITY_KEY_LEN];
    getrandom::fill(&mut key)
        .map_err(|e| std::io::Error::other(format!("CSPRNG unavailable: {e}")))?;

    let tmp = path.with_extension("key.tmp");
    std::fs::write(&tmp, &key)?;
    crate::platform::set_owner_only(&tmp).ok();
    std::fs::rename(&tmp, &path)?;
    crate::platform::set_owner_only(&path).ok();
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
/// Codex `config.toml`, Gemini/Antigravity `settings.json`, Grok). Sibling of
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
    /// No `identity.key` — the feature is not enabled on this install.
    Disabled,
    /// The claimed id carries a valid tag.
    Verified,
    /// Key present, tag missing or wrong, soft mode: proceed but warn.
    Unverified,
    /// Key present, tag missing or wrong, `require_identity_token = true`.
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

/// Testable core of [`verify_env_identity`] with the env reads hoisted out.
pub fn verify_claim(
    home_dir: &Path,
    claimed_id: &str,
    token: &str,
    require: bool,
) -> IdentityVerdict {
    let Some(key) = load_key(home_dir) else {
        return IdentityVerdict::Disabled;
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

    fn key() -> Vec<u8> {
        (0u8..32).collect()
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
    fn corrupt_key_is_ignored_then_regenerated() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(identity_key_path(home), b"short").unwrap();
        assert!(load_key(home).is_none(), "wrong-length key must not be used");
        let k = ensure_key(home).unwrap();
        assert_eq!(k.len(), IDENTITY_KEY_LEN);
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

        // 1-2. No key ⇒ Disabled regardless of token or strictness.
        assert_eq!(
            verify_claim(home, "sales-rep", "", false),
            IdentityVerdict::Disabled
        );
        assert_eq!(
            verify_claim(home, "sales-rep", "deadbeef", true),
            IdentityVerdict::Disabled
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
