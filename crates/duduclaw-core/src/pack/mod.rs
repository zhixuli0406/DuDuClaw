//! Unified pack format — one `pack.toml` schema and one in-memory [`Pack`]
//! type covering what used to be three (really four) separate declarations of
//! "a pre-configured set of AI employees".
//!
//! Design: `commercial/docs/DESIGN-pack-format-unification-2026-09.md`
//! (feature audit 2026-09-29, item **T5 / O2**).
//!
//! # The four legacy formats this reads
//!
//! | On disk | What it declares | → [`PackKind`] |
//! |---|---|---|
//! | `presets/<id>/preset.toml` | one de-identified `agent.toml` fragment | [`PackKind::Preset`] |
//! | `<slug>/expert.toml` | a roster (agents + `reports_to`) + skills + wiki | [`PackKind::Team`] |
//! | `teams/<industry>-team/team.toml` | front desk + worker kits + compliance overlays | [`PackKind::Team`] |
//! | `<industry>-pro/` (`SOUL.md` [+ `template.toml`]) | a single industry persona | [`PackKind::Template`] |
//!
//! **No disk migration.** The loaders accept the legacy files verbatim; the
//! premium content tree (whose compliance overlays were reviewed line by line
//! by a human) is never rewritten by a machine. The canonical `pack.toml`
//! form exists so new packs have one schema to target and so
//! `duduclaw pack inspect` can show an author what their legacy file looks
//! like under it.
//!
//! # What this module is NOT
//!
//! It does **not** install anything. `duduclaw-core` cannot scaffold agents
//! (that needs `duduclaw-cli` / `duduclaw-gateway`), so installation stays
//! where it is — `duduclaw-cli::pack_cmd::install_pack` is the single front
//! door and it routes a parsed `Pack` into the existing, test-backed
//! pipelines. This module owns the *types and the reading*, nothing else.
//!
//! It also does not invent a second validation dialect: slug / department /
//! rank shapes are checked with the same `crate::is_valid_agent_id`,
//! `crate::is_valid_department`, `crate::org::OrgRank::parse` every other
//! caller uses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::org;
use crate::preset::sanitize_preset_table;

/// Canonical manifest basename.
pub const PACK_FILE: &str = "pack.toml";
/// Legacy expert-pack manifest basename.
pub const LEGACY_EXPERT_FILE: &str = "expert.toml";
/// Legacy team-playbook manifest basename.
pub const LEGACY_TEAM_FILE: &str = "team.toml";
/// Legacy preset content basename (see [`crate::preset::PRESET_FILE`]).
pub const LEGACY_PRESET_FILE: &str = "preset.toml";
/// Optional display-metadata file of a legacy `<industry>-pro/` pack.
pub const LEGACY_INDUSTRY_FILE: &str = "template.toml";

/// Schema version written into a canonical `pack.toml`. A file declaring a
/// *higher* version was written by a newer DuDuClaw; this build refuses it
/// rather than guessing (same posture as `preset_bindings.toml` /
/// `org.toml`).
pub const PACK_SCHEMA: i64 = 1;

/// UI locale used as the first display-name fallback.
const DEFAULT_LOCALE: &str = "zh-TW";

/// What a pack declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PackKind {
    /// A job-configuration bundle with no identity — the `[agent]` table is
    /// stripped entirely (batch-privilege-escalation fence, see
    /// [`crate::preset`]).
    Preset,
    /// A roster: one or more agents in a `reports_to` hierarchy, optionally
    /// with skills, wiki SOP pages, prompts and channel hints.
    Team,
    /// A single industry persona (`SOUL.md` + optional settings), with no
    /// roster of its own.
    Template,
}

impl PackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PackKind::Preset => "preset",
            PackKind::Team => "team",
            PackKind::Template => "template",
        }
    }

    /// Parse the canonical `kind` token. Unknown values are `None` (the
    /// caller refuses the file — never a silent default, which would mean
    /// running a preset's config through the roster installer).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "preset" => Some(PackKind::Preset),
            "team" => Some(PackKind::Team),
            "template" => Some(PackKind::Template),
            _ => None,
        }
    }

    /// zh-TW label for CLI/report output.
    pub fn label(self) -> &'static str {
        match self {
            PackKind::Preset => "職務組合",
            PackKind::Team => "團隊",
            PackKind::Template => "產業板模",
        }
    }
}

/// Licensing tier a pack belongs to. **The single place the premium decision
/// is expressed** — callers gate on `tier == Premium`, they no longer each
/// re-derive "is this a paid pack" from a directory path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum PackTier {
    #[default]
    Free,
    Premium,
}

impl PackTier {
    pub fn as_str(self) -> &'static str {
        match self {
            PackTier::Free => "free",
            PackTier::Premium => "premium",
        }
    }

    /// Parse the canonical `tier` token. Anything unrecognised is
    /// **Premium** — fail-closed: an unreadable tier must not accidentally
    /// unlock paid content.
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "free" => PackTier::Free,
            _ => PackTier::Premium,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PackTier::Free => "免費",
            PackTier::Premium => "付費方案",
        }
    }
}

/// Which on-disk dialect a [`Pack`] was read from. Kept so
/// `duduclaw pack inspect` can tell an author which generation their file
/// belongs to (legacy formats are still read; no removal date).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSource {
    Canonical,
    LegacyPreset,
    LegacyExpert,
    LegacyTeam,
    LegacyIndustry,
}

impl PackSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PackSource::Canonical => "pack.toml",
            PackSource::LegacyPreset => "preset.toml",
            PackSource::LegacyExpert => "expert.toml",
            PackSource::LegacyTeam => "team.toml",
            PackSource::LegacyIndustry => "industry-pack",
        }
    }

    /// `true` for every format other than canonical `pack.toml`.
    pub fn is_legacy(self) -> bool {
        !matches!(self, PackSource::Canonical)
    }
}

/// Doctor-style prerequisites. Missing items warn, never block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackRequires {
    pub env: Vec<String>,
    pub bins: Vec<String>,
}

impl PackRequires {
    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.bins.is_empty()
    }
}

/// One roster member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackAgent {
    /// In-pack agent name (also the `agents/<name>/` directory).
    pub name: String,
    /// Canonical DuDuClaw role string (`front_desk`, `worker`, …). Empty ⇒
    /// the installer's default (`worker`).
    pub role: String,
    pub display_name: String,
    /// In-pack supervisor by `name`. Empty ⇒ pack root.
    pub reports_to: String,
    pub department: String,
    /// Display rank. Empty ⇒ derived from `role` by
    /// [`crate::org::rank_for_role`].
    pub rank: String,
    pub trigger: String,
    pub skills: Vec<String>,
    /// One-line role summary (team playbooks carry these; expert packs do
    /// not — empty there, which is lossless in both directions).
    pub summary: String,
    /// Compliance overlay lines appended to this member's CONTRACT.toml
    /// `must_not` and SOUL.md overlay section at conversion time.
    pub overlay: Vec<String>,
    /// Optional preset id to bind this member to after install.
    pub preset: String,
    /// Shared worker kit this member is built from (team playbooks only).
    pub kit: String,
}

impl PackAgent {
    /// Effective rank: explicit value, else derived from `role`.
    pub fn effective_rank(&self) -> String {
        let raw = self.rank.trim();
        if !raw.is_empty() {
            return raw.to_string();
        }
        org::rank_for_role(&self.role).as_str().to_string()
    }
}

/// A human-held post the team deliberately does not automate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackHumanRole {
    pub title: String,
    pub summary: String,
}

/// A shared worker kit deliberately excluded from this team, with the reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackExcludedKit {
    pub kit: String,
    pub reason: String,
}

/// Why a pack could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    /// No recognised manifest in the directory.
    NotFound(String),
    /// Present but unreadable / unparseable / semantically refused.
    Invalid(String),
    /// The canonical file declares a schema this build does not understand.
    SchemaTooNew { found: i64, supported: i64 },
    /// A preset pack carried org-authority fields (`agent.name` /
    /// `reports_to` / `department`) — the whole pack is refused, fail-closed.
    OrgFieldsRejected(Vec<String>),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::NotFound(what) => write!(
                f,
                "找不到可辨識的包 manifest（{what}）—— 需要 pack.toml / expert.toml / team.toml / preset.toml"
            ),
            PackError::Invalid(reason) => write!(f, "包內容無效: {reason}"),
            PackError::SchemaTooNew { found, supported } => write!(
                f,
                "pack.toml schema {found} 比這個版本能讀的 {supported} 新；請升級 DuDuClaw"
            ),
            PackError::OrgFieldsRejected(fields) => write!(
                f,
                "職務組合帶有組織權威欄位，整包拒絕: {}",
                fields.join(", ")
            ),
        }
    }
}

impl std::error::Error for PackError {}

/// A pack, normalized. Every loader in this module produces this exact shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    /// On-disk slug. **Always the directory name** when loaded from a
    /// directory — install records and removal key on it (see
    /// [`Pack::id_mismatch`]).
    pub id: String,
    pub kind: PackKind,
    pub tier: PackTier,
    pub version: String,
    /// locale → display name. A legacy single `label` becomes
    /// `{"zh-TW": label}` (single→map is lossless; map→single is not, which
    /// is why this is stored as a map).
    pub display_name: BTreeMap<String, String>,
    pub description: String,
    pub author: String,
    pub license: String,
    pub category: String,
    pub tags: Vec<String>,
    /// Concrete task examples (dashboard summon card / inspiration gallery).
    pub examples: Vec<String>,
    pub requires: PackRequires,
    pub prompts: Vec<String>,
    pub channels: Vec<String>,
    /// `PackKind::Preset` only: the sanitized `agent.toml`-shaped table.
    pub config: toml::value::Table,
    pub agents: Vec<PackAgent>,
    pub humans: Vec<PackHumanRole>,
    pub excluded: Vec<PackExcludedKit>,
    /// in-pack agent name → eval suite source (relative to the pack root).
    pub eval_suites: BTreeMap<String, String>,
    /// Opaque autopilot rule tables, passed through to `autopilot.create`.
    pub autopilot_rules: Vec<toml::value::Table>,
    pub source: PackSource,
    /// Set when the manifest's own `name`/`id` differs from the directory
    /// name. The directory wins (it is the install key); callers warn.
    /// `experts/pharmacy-pro/` (manifest name `pharmacy-assistant`) is the
    /// real-world case this exists for.
    pub id_mismatch: Option<String>,
}

impl Default for Pack {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: PackKind::Team,
            tier: PackTier::Free,
            version: String::new(),
            display_name: BTreeMap::new(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            category: String::new(),
            tags: Vec::new(),
            examples: Vec::new(),
            requires: PackRequires::default(),
            prompts: Vec::new(),
            channels: Vec::new(),
            config: toml::value::Table::new(),
            agents: Vec::new(),
            humans: Vec::new(),
            excluded: Vec::new(),
            eval_suites: BTreeMap::new(),
            autopilot_rules: Vec::new(),
            source: PackSource::Canonical,
            id_mismatch: None,
        }
    }
}

impl Pack {
    /// Display name for `locale`, falling back `locale → zh-TW → en → id`.
    /// Same resolution order as the legacy `ExpertSection::display`, so the
    /// dashboard label of an existing pack does not change.
    pub fn display(&self, locale: &str) -> &str {
        let pick = |k: &str| {
            self.display_name
                .get(k)
                .map(|s| s.as_str())
                .filter(|s| !s.trim().is_empty())
        };
        pick(locale)
            .or_else(|| pick(DEFAULT_LOCALE))
            .or_else(|| pick("en"))
            .unwrap_or(&self.id)
    }

    /// Version, never empty (`0.0.0` stands in) — matching what
    /// `expert pack` / `expert publish` already do when authoring.
    pub fn version_or_zero(&self) -> &str {
        if self.version.trim().is_empty() {
            "0.0.0"
        } else {
            &self.version
        }
    }

    /// Roster root members (`reports_to` empty).
    pub fn roots(&self) -> impl Iterator<Item = &PackAgent> {
        self.agents
            .iter()
            .filter(|a| a.reports_to.trim().is_empty())
    }

    /// Render a `PackKind::Preset` pack back as a legacy `preset.toml`.
    ///
    /// The preset **store** is not part of this unification (see the design
    /// doc's non-goals): `preset_bindings.toml`, `resolve_for_agent` and the
    /// `agent_resolved/` materialization all read `presets/<id>/preset.toml`.
    /// So installing a canonical `pack.toml` preset means writing that file —
    /// one emitter, here, rather than a hand-rolled writer at each call site.
    ///
    /// `None` for a non-preset pack (nothing to render).
    pub fn to_legacy_preset_toml(&self) -> Option<String> {
        if self.kind != PackKind::Preset {
            return None;
        }
        let mut meta = toml::value::Table::new();
        meta.insert(
            "version".into(),
            toml::Value::String(self.version_or_zero().to_string()),
        );
        meta.insert(
            "label".into(),
            toml::Value::String(self.display(DEFAULT_LOCALE).to_string()),
        );
        meta.insert(
            "description".into(),
            toml::Value::String(self.description.clone()),
        );
        let mut root = self.config.clone();
        root.insert("preset".into(), toml::Value::Table(meta));
        toml::to_string_pretty(&toml::Value::Table(root)).ok()
    }

    /// Shape problems that are safe to report but must not block reading.
    /// Strict authoring-time validation stays in `expert pack` (which also
    /// checks on-disk assets this module never looks at).
    pub fn lint(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.id.trim().is_empty() {
            out.push("缺少 id（包的目錄名）".to_string());
        } else if !crate::is_valid_agent_id(&self.id) {
            out.push(format!("id '{}' 非合法 slug", self.id.escape_debug()));
        }
        if let Some(name) = &self.id_mismatch {
            out.push(format!(
                "manifest 內的名稱 '{}' 與目錄名 '{}' 不符（以目錄名為準）",
                name.escape_debug(),
                self.id.escape_debug()
            ));
        }
        if self.version.trim().is_empty() {
            out.push("缺少 version".to_string());
        }
        match self.kind {
            PackKind::Preset => {
                if self.config.is_empty() {
                    out.push("職務組合沒有任何設定內容".to_string());
                }
                if !self.agents.is_empty() {
                    out.push("職務組合不應帶 roster（[[pack.agents]]）".to_string());
                }
            }
            PackKind::Team => {
                if self.agents.is_empty() {
                    out.push("團隊至少要有一個成員".to_string());
                }
                let names: std::collections::BTreeSet<&str> =
                    self.agents.iter().map(|a| a.name.as_str()).collect();
                if names.len() != self.agents.len() {
                    out.push("成員 name 有重複".to_string());
                }
                for a in &self.agents {
                    let rt = a.reports_to.trim();
                    if !rt.is_empty() && !names.contains(rt) {
                        out.push(format!("{} 的 reports_to '{rt}' 不在 roster 內", a.name));
                    }
                    if !a.department.trim().is_empty()
                        && !crate::is_valid_department(a.department.trim())
                    {
                        out.push(format!(
                            "{} 的 department '{}' 非合法部門名",
                            a.name,
                            a.department.escape_debug()
                        ));
                    }
                    if !a.rank.trim().is_empty() && org::OrgRank::parse(&a.rank).is_none() {
                        out.push(format!(
                            "{} 的 rank '{}' 非法（executive / manager / staff）",
                            a.name,
                            a.rank.escape_debug()
                        ));
                    }
                }
                if self.roots().count() == 0 {
                    out.push("roster 沒有根成員（每個 reports_to 都指向別人）".to_string());
                }
            }
            PackKind::Template => {}
        }
        out
    }
}

// ────────────────────────── small TOML helpers ──────────────────────────

fn str_field(t: Option<&toml::value::Table>, key: &str) -> String {
    t.and_then(|t| t.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn string_vec(t: Option<&toml::value::Table>, key: &str) -> Vec<String> {
    t.and_then(|t| t.get(key))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn sub_table<'a>(t: Option<&'a toml::value::Table>, key: &str) -> Option<&'a toml::value::Table> {
    t.and_then(|t| t.get(key)).and_then(|v| v.as_table())
}

fn table_array<'a>(t: Option<&'a toml::value::Table>, key: &str) -> Vec<&'a toml::value::Table> {
    t.and_then(|t| t.get(key))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_table()).collect())
        .unwrap_or_default()
}

fn parse_toml(text: &str) -> Result<toml::value::Table, PackError> {
    text.parse::<toml::Table>()
        .map_err(|e| PackError::Invalid(e.to_string()))
}

/// `label` (single string) → the display-name map, so a legacy single label
/// and a multi-locale map end up in one representation.
fn label_to_map(label: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if !label.trim().is_empty() {
        m.insert(DEFAULT_LOCALE.to_string(), label.trim().to_string());
    }
    m
}

// ─────────────────────────── canonical loader ───────────────────────────

/// Parse a canonical `pack.toml`. `id` is the directory name and always wins
/// over a manifest-declared id (see [`Pack::id_mismatch`]).
pub fn parse_pack(id: &str, text: &str) -> Result<Pack, PackError> {
    let root = parse_toml(text)?;
    let p = root.get("pack").and_then(|v| v.as_table());
    if p.is_none() {
        return Err(PackError::Invalid("缺少 [pack] 區段".into()));
    }

    let schema = p
        .and_then(|t| t.get("schema"))
        .and_then(|v| v.as_integer())
        .unwrap_or(PACK_SCHEMA);
    if schema > PACK_SCHEMA {
        return Err(PackError::SchemaTooNew {
            found: schema,
            supported: PACK_SCHEMA,
        });
    }

    let kind_raw = str_field(p, "kind");
    let Some(kind) = PackKind::parse(&kind_raw) else {
        return Err(PackError::Invalid(format!(
            "kind '{}' 非法（preset / team / template）",
            kind_raw.escape_debug()
        )));
    };

    let declared_id = str_field(p, "id");
    let id_mismatch = (!declared_id.trim().is_empty() && declared_id.trim() != id.trim())
        .then(|| declared_id.trim().to_string());

    let mut display_name: BTreeMap<String, String> = BTreeMap::new();
    if let Some(dn) = sub_table(p, "display_name") {
        for (k, v) in dn {
            if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
                display_name.insert(k.clone(), s.trim().to_string());
            }
        }
    }
    if display_name.is_empty() {
        display_name = label_to_map(&str_field(p, "label"));
    }

    // Preset config: everything under `[pack.config]`, sanitized exactly as a
    // legacy preset would be — refusing the whole pack on org fields (R4).
    let mut config = toml::value::Table::new();
    if kind == PackKind::Preset {
        let raw = sub_table(p, "config").cloned().unwrap_or_default();
        let report = sanitize_preset_table(&raw);
        if !report.stripped_org.is_empty() {
            return Err(PackError::OrgFieldsRejected(report.stripped_org));
        }
        config = report.table;
    }

    let agents = table_array(p, "agents")
        .into_iter()
        .map(|a| PackAgent {
            name: str_field(Some(a), "name"),
            role: str_field(Some(a), "role"),
            display_name: str_field(Some(a), "display_name"),
            reports_to: str_field(Some(a), "reports_to"),
            department: str_field(Some(a), "department"),
            rank: str_field(Some(a), "rank"),
            trigger: str_field(Some(a), "trigger"),
            skills: string_vec(Some(a), "skills"),
            summary: str_field(Some(a), "summary"),
            overlay: string_vec(Some(a), "overlay"),
            preset: str_field(Some(a), "preset"),
            kit: str_field(Some(a), "kit"),
        })
        .collect();

    let humans = table_array(p, "humans")
        .into_iter()
        .map(|h| PackHumanRole {
            title: str_field(Some(h), "title"),
            summary: str_field(Some(h), "summary"),
        })
        .collect();

    let excluded = table_array(p, "excluded")
        .into_iter()
        .map(|x| PackExcludedKit {
            kit: str_field(Some(x), "kit"),
            reason: str_field(Some(x), "reason"),
        })
        .collect();

    let mut eval_suites = BTreeMap::new();
    if let Some(es) = sub_table(p, "eval_suites") {
        for (k, v) in es {
            if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
                eval_suites.insert(k.clone(), s.trim().to_string());
            }
        }
    }

    let autopilot_rules = table_array(p, "autopilot_rules")
        .into_iter()
        .cloned()
        .collect();

    Ok(Pack {
        id: id.trim().to_string(),
        kind,
        tier: PackTier::parse(&str_field(p, "tier")),
        version: str_field(p, "version"),
        display_name,
        description: str_field(p, "description"),
        author: str_field(p, "author"),
        license: str_field(p, "license"),
        category: str_field(p, "category"),
        tags: string_vec(p, "tags"),
        examples: string_vec(p, "examples"),
        requires: PackRequires {
            env: string_vec(sub_table(p, "requires"), "env"),
            bins: string_vec(sub_table(p, "requires"), "bins"),
        },
        prompts: string_vec(sub_table(p, "prompts"), "recommended"),
        channels: string_vec(sub_table(p, "channels"), "suggested"),
        config,
        agents,
        humans,
        excluded,
        eval_suites,
        autopilot_rules,
        source: PackSource::Canonical,
        id_mismatch,
    })
}

// ─────────────────────────── legacy loaders ────────────────────────────

/// The four pre-`pack.toml` manifest readers (`preset.toml`, `expert.toml`,
/// `team.toml`, `<industry>-pro/`). Split into its own file for size only —
/// every reader is re-exported here, so `pack::parse_legacy_*` paths are
/// unchanged.
mod legacy;

pub use legacy::{
    parse_legacy_expert, parse_legacy_industry, parse_legacy_preset, parse_legacy_team,
};

// ─────────────────────────── directory dispatch ─────────────────────────

/// Which manifest a directory carries, in the order [`load_dir`] prefers.
pub fn detect_dir(dir: &Path) -> Option<PackSource> {
    if dir.join(PACK_FILE).is_file() {
        return Some(PackSource::Canonical);
    }
    if dir.join(LEGACY_EXPERT_FILE).is_file() {
        return Some(PackSource::LegacyExpert);
    }
    if dir.join(LEGACY_TEAM_FILE).is_file() {
        return Some(PackSource::LegacyTeam);
    }
    if dir.join(LEGACY_PRESET_FILE).is_file() {
        return Some(PackSource::LegacyPreset);
    }
    if dir.join("SOUL.md").is_file() {
        return Some(PackSource::LegacyIndustry);
    }
    None
}

/// Read whatever pack manifest `dir` carries into a [`Pack`].
///
/// `id` is the directory's file name. `fallback_label` is only consulted for
/// a `<industry>-pro/` directory (pass `""` when you have no table).
pub fn load_dir(dir: &Path, fallback_label: &str) -> Result<Pack, PackError> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let Some(source) = detect_dir(dir) else {
        return Err(PackError::NotFound(dir.display().to_string()));
    };
    let read = |name: &str| -> Result<String, PackError> {
        std::fs::read_to_string(dir.join(name))
            .map_err(|e| PackError::Invalid(format!("讀取 {name} 失敗: {e}")))
    };
    match source {
        PackSource::Canonical => parse_pack(&id, &read(PACK_FILE)?),
        PackSource::LegacyExpert => parse_legacy_expert(&id, &read(LEGACY_EXPERT_FILE)?),
        PackSource::LegacyTeam => parse_legacy_team(&id, &read(LEGACY_TEAM_FILE)?),
        PackSource::LegacyPreset => parse_legacy_preset(&id, &read(LEGACY_PRESET_FILE)?),
        PackSource::LegacyIndustry => {
            let template = std::fs::read_to_string(dir.join(LEGACY_INDUSTRY_FILE)).ok();
            parse_legacy_industry(&id, template.as_deref(), fallback_label)
        }
    }
}

/// Every pack directly under `root` that carries a recognised manifest,
/// sorted by id. A missing directory yields an empty list, not an error.
pub fn list_dir(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && detect_dir(p).is_some())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests;
