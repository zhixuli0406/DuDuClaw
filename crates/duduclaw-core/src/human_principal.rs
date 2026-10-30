//! The person an employee's work is for (`on_behalf_of`).
//!
//! The gateway knows who a chat turn, a handed-over task or a delegated run
//! is for; the employee's runtime and its MCP server only learn it from the
//! host. The value travels in one of two shapes:
//!
//! | wire form | who |
//! |---|---|
//! | `user:<account id>` | a dashboard account (signed in, or a verified channel identity) |
//! | `unmapped:<channel>:<digest>` | a messaging-app sender with no account; the digest is a keyed HMAC of the channel id (16 hex characters) or `unavailable` |
//!
//! Sources, all written by the gateway: the `DUDUCLAW_ON_BEHALF_OF`
//! environment variable it sets on a spawn ([`ENV_ON_BEHALF_OF`]), the
//! `message_queue.on_behalf_of` / `tasks.on_behalf_of` / `approvals.on_behalf_of`
//! columns. Tool arguments are never a source.
//!
//! What it can do: be recorded (audit rows, task rows, approval cards), and,
//! with `[delegation] human_principal = "enforce"`, narrow a delegation the
//! employee tree already allowed to what the person may do. It never widens
//! anything: a forged value can at most make the record wrong or make a
//! check that already passed for the employee also pass for the person.

use std::fmt;
use std::path::Path;

use crate::delegation_policy::{is_reserved_agent_id, is_system_sender};

/// Set by the gateway on every spawn whose turn or run is for a person.
pub const ENV_ON_BEHALF_OF: &str = "DUDUCLAW_ON_BEHALF_OF";

/// Longest account id a principal may carry.
pub const MAX_USER_ID_LEN: usize = 64;
/// Longest channel name an unmapped principal may carry.
pub const MAX_CHANNEL_LEN: usize = 32;
/// Digest value used when the per-home audit key is unavailable.
pub const DIGEST_UNAVAILABLE: &str = "unavailable";
/// Characters of a rejected value kept for the audit record.
pub const REJECTED_PREVIEW_CHARS: usize = 40;

/// A person, as the host names them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HumanPrincipal {
    /// A dashboard account.
    User(String),
    /// A messaging-app sender with no account.
    Unmapped { channel: String, digest: String },
}

/// Why a value was not accepted as a principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRejection {
    /// Stable code for the audit record.
    pub reason: &'static str,
    /// The start of the rejected value, char-safe, for the audit record.
    pub preview: String,
}

impl HumanPrincipal {
    /// A dashboard account principal, or `None` for an id an account never
    /// has (empty, too long, characters outside `[A-Za-z0-9_-]`, `system`,
    /// a system sender name or a reserved employee id).
    pub fn user(user_id: &str) -> Option<Self> {
        valid_user_id(user_id).then(|| Self::User(user_id.to_string()))
    }

    /// An unmapped-sender principal, or `None` when the channel name or the
    /// digest is malformed.
    pub fn unmapped(channel: &str, digest: &str) -> Option<Self> {
        (valid_channel(channel) && valid_digest(digest)).then(|| Self::Unmapped {
            channel: channel.to_string(),
            digest: digest.to_string(),
        })
    }

    /// Parse the wire form. Never accepts anything [`Self::user`] or
    /// [`Self::unmapped`] would refuse.
    pub fn parse(raw: &str) -> Result<Self, PrincipalRejection> {
        let reject = |reason: &'static str| PrincipalRejection {
            reason,
            preview: crate::truncate_chars(raw, REJECTED_PREVIEW_CHARS).to_string(),
        };
        if let Some(id) = raw.strip_prefix("user:") {
            return Self::user(id).ok_or_else(|| {
                if is_system_sender(id) || is_reserved_agent_id(id) || id == "system" {
                    reject("system_sender")
                } else {
                    reject("invalid_user_id")
                }
            });
        }
        if let Some(rest) = raw.strip_prefix("unmapped:") {
            let (channel, digest) = rest.split_once(':').ok_or_else(|| reject("invalid_unmapped"))?;
            return Self::unmapped(channel, digest).ok_or_else(|| reject("invalid_unmapped"));
        }
        Err(reject("missing_prefix"))
    }

    /// The dashboard account id, for a [`Self::User`].
    pub fn user_id(&self) -> Option<&str> {
        match self {
            Self::User(id) => Some(id),
            Self::Unmapped { .. } => None,
        }
    }
}

impl fmt::Display for HumanPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(id) => write!(f, "user:{id}"),
            Self::Unmapped { channel, digest } => write!(f, "unmapped:{channel}:{digest}"),
        }
    }
}

fn valid_user_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_USER_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && id != "system"
        && !is_system_sender(id)
        && !is_reserved_agent_id(id)
}

fn valid_channel(channel: &str) -> bool {
    !channel.is_empty()
        && channel.len() <= MAX_CHANNEL_LEN
        && channel
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_digest(digest: &str) -> bool {
    digest == DIGEST_UNAVAILABLE
        || (digest.len() == 16 && digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// The principal this process was spawned for: `Ok(None)` when the variable
/// is absent or empty, `Err` when it is present but malformed (the caller
/// records `on_behalf_of_rejected` and preserves the rejected state in
/// provenance carriers, or refuses the operation).
pub fn host_principal_checked() -> Result<Option<HumanPrincipal>, PrincipalRejection> {
    match std::env::var(ENV_ON_BEHALF_OF) {
        Ok(raw) if !raw.trim().is_empty() => HumanPrincipal::parse(raw.trim()).map(Some),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(PrincipalRejection {
            reason: "not_unicode",
            preview: String::new(),
        }),
    }
}

/// [`host_principal_checked`], malformed values dropped.
pub fn host_principal() -> Option<HumanPrincipal> {
    host_principal_checked().ok().flatten()
}

// ── Enforcement mode ────────────────────────────────────────────────────────

/// `[delegation] human_principal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanPrincipalMode {
    /// Record who the work is for; never refuse because of it (default).
    #[default]
    Record,
    /// After the employee tree allows a delegation, also require that the
    /// person it is for may do the same.
    Enforce,
}

impl HumanPrincipalMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Enforce => "enforce",
        }
    }

    /// Absent = record; an unrecognised value = enforce, with a warning
    /// (a typo never weakens the setting the operator meant to turn on).
    pub fn from_config_value(value: Option<&str>) -> (Self, Option<String>) {
        match value.map(str::trim) {
            None => (Self::Record, None),
            Some(v) if v.eq_ignore_ascii_case("record") => (Self::Record, None),
            Some(v) if v.eq_ignore_ascii_case("enforce") => (Self::Enforce, None),
            Some(other) => (
                Self::Enforce,
                Some(format!(
                    "[delegation] human_principal = \"{}\" 無法辨識，已當作 \"enforce\"（可用值：record、enforce）",
                    crate::truncate_chars(other, 40)
                )),
            ),
        }
    }

    /// From an already-parsed `config.toml` table. A non-string value counts
    /// as unrecognised.
    pub fn from_table(table: &toml::Table) -> (Self, Option<String>) {
        let Some(section) = table.get("delegation") else {
            return (Self::Record, None);
        };
        let Some(section) = section.as_table() else {
            return Self::from_config_value(Some("<delegation is not a table>"));
        };
        let Some(raw) = section.get("human_principal") else {
            return (Self::Record, None);
        };
        Self::from_config_value(Some(raw.as_str().unwrap_or("<not a string>")))
    }

    /// From `<home>/config.toml`. A missing file means record; a file that
    /// exists but cannot be read or parsed means enforce (fail closed).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::Record,
            Err(e) => {
                tracing::warn!(error = %e, "config.toml unreadable; human_principal treated as enforce");
                return Self::Enforce;
            }
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            tracing::warn!("config.toml does not parse; human_principal treated as enforce");
            return Self::Enforce;
        };
        let (mode, warning) = Self::from_table(&table);
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        mode
    }
}

/// What the person must be allowed to do for a delegation to pass in
/// enforce mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HumanRight {
    /// Talk to the employee (`send_to_agent`, `spawn_agent`).
    Call,
    /// Hand the employee a task (Operator or above).
    Assign,
}

/// The right a delegation path needs. Paths that hand over work as a task
/// need [`HumanRight::Assign`]; an unknown path needs it too (fail closed).
pub fn human_right_for_path(path_kind: &str) -> HumanRight {
    const CALL_PATHS: &[&str] = &[
        "send_to_agent",
        "spawn_agent",
        "sqlite_queue",
        "bus_dispatch",
    ];
    if CALL_PATHS.contains(&path_kind) {
        HumanRight::Call
    } else {
        HumanRight::Assign
    }
}

#[cfg(test)]
#[path = "human_principal_tests.rs"]
mod tests;
