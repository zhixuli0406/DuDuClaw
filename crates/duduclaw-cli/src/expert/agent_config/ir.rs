//! The intermediate representation every adapter produces and the one
//! writer consumes. Adapters only *read* (no disk writes, no scanning); the
//! writer owns scanning, conflict handling and every effect.

use std::path::PathBuf;

/// One text file lifted from the source, with where it came from (shown in
/// the import report and used as the memory source lineage).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Doc {
    /// Path relative to the import root, `/`-separated (report + lineage).
    pub source: String,
    pub text: String,
    pub masked_values: usize,
}

impl Doc {
    pub fn new(source: impl Into<String>, text: impl Into<String>) -> Self {
        Doc {
            source: source.into(),
            text: text.into(),
            masked_values: 0,
        }
    }
}

/// A skill directory found in the source (normalised later by the writer).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillDraft {
    /// The skill directory on disk (inside the staging copy).
    #[serde(skip_serializing)]
    pub dir: PathBuf,
    /// Name the source gave it (`name:` / `dir`), possibly non-ASCII.
    pub name_hint: String,
    /// Human label (豆包 `label`, often Chinese) kept in metadata.
    pub label: Option<String>,
    pub version: Option<String>,
    /// Report path relative to the import root.
    pub source: String,
}

/// A file the importer could not map to a DuDuClaw slot. Archived verbatim
/// under `<home>/imported/<slug>/` and listed in the report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Unmapped {
    pub rel: String,
    pub reason: String,
}

/// One imported AI employee.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentDraft {
    /// The agent's key inside the source (`a1`, a workspace id, a file stem).
    pub ref_id: String,
    /// Preferred DuDuClaw id (ASCII); may be empty or invalid — the writer
    /// falls back to `<format>-<ref>`.
    pub id_hint: String,
    pub display_name: String,
    pub emoji: Option<String>,
    pub description: String,
    pub soul: Option<Doc>,
    pub identity: Option<Doc>,
    pub instructions: Option<Doc>,
    pub user_profile: Option<Doc>,
    pub contract: Option<Doc>,
    /// Product-native settings, validated against the editable allowlist.
    pub settings: toml::Table,
    pub tools_notes: Option<Doc>,
    pub heartbeat: Option<Doc>,
    pub memory: Vec<Doc>,
    pub knowledge: Vec<Doc>,
    pub skills: Vec<SkillDraft>,
    /// MCP servers are only named in the report; never installed.
    pub mcp_servers: serde_json::Map<String, serde_json::Value>,
    pub model: Option<String>,
    /// Source tool allowlist (`None` = the source did not restrict tools).
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    /// `ref_id` of the supervisor inside the same bundle.
    pub reports_to: Option<String>,
    /// Source fields that have no DuDuClaw equivalent (reported, not applied),
    /// e.g. `permissionMode: bypassPermissions`.
    pub ignored_fields: Vec<(String, String)>,
    /// Files lower in the instructions precedence table, or otherwise read
    /// but not used: `(source, reason)`.
    pub skipped: Vec<(String, String)>,
    pub unmapped: Vec<Unmapped>,
}

/// Everything one source yields.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportBundle {
    /// Source label for the report (`豆包夥伴安裝包`, `OpenClaw 工作區`, …).
    pub format_label: String,
    /// Short ASCII prefix for fallback ids and paths (`doubao`, `openclaw`).
    pub format_code: &'static str,
    /// Pack slug hint (ASCII); empty → derived from the first agent.
    pub slug_hint: String,
    pub display_name: String,
    pub description: String,
    pub version: String,
    pub agents: Vec<AgentDraft>,
    /// Bundle-level files that map nowhere.
    pub unmapped: Vec<Unmapped>,
    /// Plain warnings for the report.
    pub warnings: Vec<String>,
}
