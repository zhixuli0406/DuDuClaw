//! People editing an AI employee's profile files — one writer for all of them.
//!
//! Every human write to `SOUL.md`, `IDENTITY.md`, `USER.md`, `INSTRUCTIONS.md`,
//! `CONTRACT.toml`, `agent.toml` and the employee's own skill files
//! (`SKILLS/**.md`, [`skills`]) goes through [`writer`]: a per-file
//! in-process lock plus the cross-process advisory lock, an optimistic
//! `base_hash` compare, an owner-only atomic replace, a version history kept
//! outside the employee's directory ([`history`]), an audit row, a registry
//! rescan, and the SOUL / IDENTITY / USER / INSTRUCTIONS baseline update (`soul_guard`).
//!
//! The dashboard's own saves (`agent_files.save`, `agent_files.rollback`,
//! `contract.update`) additionally run the strict check ([`flow`]): fixed
//! rules ([`rules`]), then a cheap-model review of the diff ([`review`]),
//! merged into `pass / warn / block` ([`verdict`]). The AI can only make the
//! result stricter, and a review that does not complete is a `warn` (or a
//! `block` when configured) — never a silent pass. A non-admin who hits a
//! `block` can only send the change to an admin ([`approval`], kind
//! `agent_file_change`, decided in the dashboard only).
//!
//! Who may do what is always read live from `users.db`
//! (`require_live_agent_access`), never from the session token's snapshot, and
//! the writer re-checks it inside the file lock.

pub(crate) mod approval;
pub(crate) mod diff;
pub mod external;
pub(crate) mod flow;
pub(crate) mod history;
pub(crate) mod quota;
pub(crate) mod review;
pub(crate) mod rules;
pub(crate) mod runtime_context;
pub(crate) mod skill_rules;
pub(crate) mod skills;
pub(crate) mod verdict;
pub(crate) mod writer;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Byte limit of SOUL / IDENTITY / INSTRUCTIONS and of one skill file.
pub(crate) const MAX_MARKDOWN_BYTES: usize = duduclaw_core::agent_profile::MAX_MARKDOWN_BYTES;
/// Unicode character limit of the user introduction.
pub(crate) const MAX_USER_CHARS: usize = duduclaw_core::agent_profile::MAX_USER_CHARS;
/// Byte limit of CONTRACT.toml.
pub(crate) const MAX_CONTRACT_BYTES: usize = duduclaw_core::agent_profile::MAX_CONTRACT_BYTES;
/// Byte limit of agent.toml (same as the raw editor).
pub(crate) const MAX_AGENT_TOML_BYTES: usize = 256 * 1024;
/// At most this many `must_not` / `must_always` entries.
pub(crate) const MAX_CONTRACT_ENTRIES: usize = 100;
/// Each entry at most this many characters.
pub(crate) const MAX_CONTRACT_ENTRY_CHARS: usize = 500;

/// Which profile file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum FileKey {
    Soul,
    Identity,
    User,
    Instructions,
    Contract,
    AgentToml,
    /// One of the employee's own skill files (`SKILLS/<rel>`, `.md` only).
    Skill(skills::SkillPath),
}

impl FileKey {
    /// The fixed files (skills are listed per employee, see [`skills::list`]).
    pub(crate) const ALL: [FileKey; 6] =
        [Self::Soul, Self::Identity, Self::User, Self::Instructions, Self::Contract, Self::AgentToml];

    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "soul" => Ok(Self::Soul),
            "identity" => Ok(Self::Identity),
            "user" => Ok(Self::User),
            "instructions" => Ok(Self::Instructions),
            "contract" => Ok(Self::Contract),
            "agent_toml" => Ok(Self::AgentToml),
            _ => match s.strip_prefix(skills::KEY_PREFIX) {
                Some(rel) => skills::SkillPath::parse(rel).map(Self::Skill),
                None => Err("file must be soul, identity, user, instructions, contract, agent_toml or skill:<path>".into()),
            },
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Soul => "soul",
            Self::Identity => "identity",
            Self::User => "user",
            Self::Instructions => "instructions",
            Self::Contract => "contract",
            Self::AgentToml => "agent_toml",
            Self::Skill(p) => p.key(),
        }
    }

    /// The file's name (a skill: its path inside `SKILLS/`).
    pub(crate) fn file_name(&self) -> &str {
        match self {
            Self::Soul => "SOUL.md",
            Self::Identity => "IDENTITY.md",
            Self::User => "USER.md",
            Self::Instructions => "INSTRUCTIONS.md",
            Self::Contract => "CONTRACT.toml",
            Self::AgentToml => "agent.toml",
            Self::Skill(p) => p.rel(),
        }
    }

    pub(crate) fn ext(&self) -> &'static str {
        if self.is_markdown() { "md" } else { "toml" }
    }

    pub(crate) fn label_key(&self) -> String {
        match self {
            Self::Skill(_) => "agentFiles.file.skill".to_string(),
            _ => format!("agentFiles.file.{}", self.as_str()),
        }
    }

    pub(crate) fn is_markdown(&self) -> bool {
        matches!(self, Self::Soul | Self::Identity | Self::User | Self::Instructions | Self::Skill(_))
    }

    pub(crate) fn is_skill(&self) -> bool {
        matches!(self, Self::Skill(_))
    }

    /// The folder of this file's versions under `agent_file_history/<id>/`.
    pub(crate) fn history_dir(&self) -> String {
        match self {
            Self::Skill(p) => p.history_dir(),
            _ => self.as_str().to_string(),
        }
    }

    pub(crate) fn max_bytes(&self) -> usize {
        match self {
            Self::User => MAX_USER_CHARS * 4,
            Self::Contract => MAX_CONTRACT_BYTES,
            Self::AgentToml => MAX_AGENT_TOML_BYTES,
            _ => MAX_MARKDOWN_BYTES,
        }
    }

    /// The baseline-protected files.
    pub(crate) fn protected_doc(&self) -> Option<duduclaw_security::soul_guard::ProtectedDoc> {
        use duduclaw_security::soul_guard::ProtectedDoc;
        match self {
            Self::Soul => Some(ProtectedDoc::Soul),
            Self::Identity => Some(ProtectedDoc::Identity),
            Self::User => Some(ProtectedDoc::User),
            Self::Instructions => Some(ProtectedDoc::Instructions),
            _ => None,
        }
    }

    /// Files a person edits as text through `agent_files.save`. `agent.toml`
    /// is read-only here: its fields go through the edit page
    /// (`agents.update`, authority-checked) and its raw text through the
    /// admin-only raw editor.
    pub(crate) fn text_editable(&self) -> bool {
        *self != Self::AgentToml
    }
}

/// `[agent_files]` in `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentFilesConfig {
    /// Versions kept per file (5–100).
    pub history_keep: usize,
    /// Reviews allowed per deployment per day; past it a review "fails".
    pub review_daily_limit: u32,
    /// Reviews one person may start per day.
    pub review_daily_limit_per_user: u32,
    /// Reviews of one employee's files per day.
    pub review_daily_limit_per_agent: u32,
    /// Checks one person may start per minute.
    pub checks_per_minute: u32,
    /// What a review that did not complete counts as (`warn` or `block`;
    /// there is deliberately no "off").
    pub on_analysis_failure: verdict::Verdict,
    pub review_provider: Option<String>,
    pub review_model: Option<String>,
}

impl Default for AgentFilesConfig {
    fn default() -> Self {
        Self {
            history_keep: 20,
            review_daily_limit: 200,
            review_daily_limit_per_user: 40,
            review_daily_limit_per_agent: 60,
            checks_per_minute: 10,
            on_analysis_failure: verdict::Verdict::Warn,
            review_provider: None,
            review_model: None,
        }
    }
}

impl AgentFilesConfig {
    /// Read `[agent_files]`; anything unreadable keeps the (strict) default.
    pub(crate) fn load(home: &Path) -> Self {
        let table = std::fs::read_to_string(home.join("config.toml"))
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .and_then(|t| t.get("agent_files").and_then(|v| v.as_table()).cloned());
        let Some(t) = table else { return Self::default() };
        Self::from_table(&t)
    }

    pub(crate) fn from_table(t: &toml::Table) -> Self {
        let mut c = Self::default();
        if let Some(n) = t.get("history_keep").and_then(|v| v.as_integer()) {
            c.history_keep = n.clamp(5, 100) as usize;
        }
        let int = |k: &str, max: i64| t.get(k).and_then(|v| v.as_integer()).map(|n| n.clamp(0, max) as u32);
        if let Some(n) = int("review_daily_limit", 100_000) {
            c.review_daily_limit = n;
        }
        if let Some(n) = int("review_daily_limit_per_user", 100_000) {
            c.review_daily_limit_per_user = n;
        }
        if let Some(n) = int("review_daily_limit_per_agent", 100_000) {
            c.review_daily_limit_per_agent = n;
        }
        if let Some(n) = int("checks_per_minute", 1_000) {
            c.checks_per_minute = n.max(1);
        }
        // Only "block" changes anything; every other value keeps "warn".
        if t.get("on_analysis_failure").and_then(|v| v.as_str()) == Some("block") {
            c.on_analysis_failure = verdict::Verdict::Block;
        }
        let s = |k: &str| {
            t.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
        };
        c.review_provider = s("review_provider");
        c.review_model = s("review_model");
        c
    }
}

/// `<home>/agents/<id>` after the same checks as the raw editor: a valid id,
/// not starting with `.` / `_`, a real directory (no symlink) directly under
/// `<home>/agents`.
pub(crate) fn agent_dir(home: &Path, agent_id: &str) -> Result<PathBuf, String> {
    if !duduclaw_core::is_valid_agent_id(agent_id) || agent_id.starts_with(['.', '_']) {
        return Err(format!("invalid agent id `{agent_id}`"));
    }
    let agents = home.join("agents");
    let dir = agents.join(agent_id);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_symlink() => return Err(format!("agent `{agent_id}` cannot be edited here")),
        Ok(m) if m.is_dir() => {}
        _ => return Err(format!("agent `{agent_id}` not found")),
    }
    let canon_agents = std::fs::canonicalize(&agents).map_err(|_| "agents directory not found".to_string())?;
    let canon = std::fs::canonicalize(&dir).map_err(|_| format!("agent `{agent_id}` not found"))?;
    if canon != canon_agents.join(agent_id) {
        return Err(format!("agent `{agent_id}` resolves outside the agents directory"));
    }
    Ok(dir)
}

/// The file's path. The file itself may be absent (a new INSTRUCTIONS.md),
/// but when present it must be a regular file, not a symlink. A skill file
/// must already exist (see [`skills::resolve`]).
pub(crate) fn file_path(home: &Path, agent_id: &str, key: &FileKey) -> Result<PathBuf, String> {
    let dir = agent_dir(home, agent_id)?;
    if let FileKey::Skill(p) = key {
        return skills::resolve(&dir, p);
    }
    let path = dir.join(key.file_name());
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => {
            Err(format!("{} cannot be edited here", key.file_name()))
        }
        Ok(_) => Ok(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if *key == FileKey::AgentToml {
                Err(format!("agent `{agent_id}` not found"))
            } else {
                Ok(path)
            }
        }
        Err(e) => Err(format!("cannot read {}: {e}", key.file_name())),
    }
}

/// In-process serialisation of every write to one (home, agent, file): the
/// AI review of one save can take seconds, and two saves must not interleave
/// their hash compare and replace. Cross-process writers are serialised by
/// `with_file_lock` inside the writer.
pub(crate) fn file_mutex(home: &Path, agent_id: &str, key: &FileKey) -> Arc<tokio::sync::Mutex<()>> {
    type Map = Mutex<HashMap<(PathBuf, String, FileKey), Arc<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Map> = OnceLock::new();
    let map = LOCKS.get_or_init(Default::default);
    let mut guard = match map.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .entry((home.to_path_buf(), agent_id.to_string(), key.clone()))
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Read a file as text, `""` when absent.
pub(crate) fn read_or_empty(path: &Path) -> Result<String, String> {
    crate::handlers::config_commit::read_text_or_empty(path)
}

/// Hex SHA-256 of the text (the `base_hash` / `hash` everywhere here; the
/// empty string hashes to the SHA-256 of nothing, so an absent file has a
/// stable hash too).
pub(crate) fn hash_of(text: &str) -> String {
    crate::handlers::config_commit::content_hash(text)
}
