//! [`TaskPacket`] — the **only** type that crosses a role boundary inside an
//! employee's team (Team-as-Agent P1/WP-1).
//!
//! # Why a packet instead of a transcript
//!
//! Handing role B role A's conversation does not work across model families
//! and quietly does not work *within* one:
//!
//! * The wire shape of a tool call differs per vendor (`tool_use` /
//!   `function_call` / `functionCall`), so a transcript is not portable.
//! * OpenAI documents that reasoning items are silently dropped when the
//!   model family changes — a "lossless" replay that loses exactly the part
//!   that explains the decision.
//! * Measured: structured notes beat both a raw trace and a prose summary
//!   (arXiv:2606.02875 — 20–59% fewer events, 42–63% fewer prompt tokens).
//!
//! So the packet carries **decisions and references**, never content that a
//! provider owns.
//!
//! # Deliberately absent fields
//!
//! There is no `transcript`, no `tool_use` / `function_call` / `functionCall`
//! block, no `thinking` / `reasoning` / `encrypted_content`, and **no
//! `serde_json::Value` escape hatch** anywhere in this type or its children.
//! That is a design constraint, not an oversight, and it is enforced rather
//! than documented: every struct here is `deny_unknown_fields`, so a packet
//! that smuggles one of those keys in fails to deserialize instead of being
//! silently accepted and ignored (see
//! `rejects_deliberately_absent_provider_fields`).
//!
//! (The one type whose `Deserialize` is hand-written, [`OutputFormat`], keeps
//! that guarantee: it delegates the object form to a private
//! `deny_unknown_fields` twin, so `{"kind":"json","thinking":"…"}` is refused
//! by name exactly as before.)
//!
//! # Strict about meaning, tolerant about spelling
//!
//! The caller is usually a model, and a refusal it cannot act on costs a whole
//! turn. Two deliberate tolerances, neither of which widens what a packet may
//! *mean*: every field but the seven in [`REQUIRED_PACKET_KEYS`] has a serde
//! default (a packet with only those seven is complete, see
//! [`MINIMAL_PACKET_EXAMPLE`]), and `output_format` accepts the bare token
//! (`"markdown"`) as well as the tagged object, normalizing to the tagged form
//! on the way out. Every cap, every contradiction check and every
//! provider-field rejection is unchanged.
//!
//! # Caps: reject, never truncate
//!
//! `constraints` and `audience` are the *incompressible* section
//! (arXiv:2608.29028: compression taxes boundaries unilaterally — ambiguity
//! drove violation rates from <15% to 50–73%, and an allowlist "nearly
//! eliminates" leakage). Truncating them silently converts an explicit
//! boundary into an implicit one, which is precisely the failure the fields
//! exist to prevent. So every cap here — including the whole-packet
//! [`TASK_PACKET_MAX_BYTES`] — **rejects the packet**, exactly as
//! `working_state_handoff` rejects an oversized note rather than trimming it.
//!
//! The same fields must also be on `prompt_compression`'s never-trim list on
//! the gateway side; that wiring belongs to a later WP.
//!
//! # Relationship to gateway types
//!
//! Two gateway types are mirrored here rather than imported, because this
//! crate must stay dependency-free:
//!
//! * [`Assertion`] mirrors `duduclaw_gateway::playbook::EntryAssertions` —
//!   same four kinds, same caps ([`ASSERTION_LIST_MAX`],
//!   [`ASSERTION_TOKEN_MAX_CHARS`]) and the same contradiction rules, so the
//!   conversion in both directions is lossless.
//! * [`Fidelity`] mirrors
//!   `duduclaw_gateway::prediction::task_forward::ObservationFidelity`, with
//!   the same three values and the same `as_str()` spellings (`full` /
//!   `mcp_only` / `none` — note the underscore, which a `Debug`-derived
//!   lowercase would lose). The gateway maps its enum onto this one when it
//!   builds a packet; the verifier must know the executor's evidence grade,
//!   and `None` must never be silently read as `McpOnly`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::Role;

// ── Caps ────────────────────────────────────────────────────────────────

/// Whole-packet serialized ceiling, in bytes of compact JSON. A packet over
/// this is **rejected entirely** — see the module docs on why truncation is
/// not an option here.
pub const TASK_PACKET_MAX_BYTES: usize = 16_384;

/// Maximum enumerated constraints (design §3.4).
pub const CONSTRAINTS_MAX: usize = 12;
/// Maximum characters (Unicode scalar values, so CJK counts 1 per glyph) in
/// one constraint's text.
pub const CONSTRAINT_TEXT_MAX_CHARS: usize = 200;
/// Maximum characters in a constraint id.
pub const CONSTRAINT_ID_MAX_CHARS: usize = 64;

/// Maximum audience entries. An allowlist longer than this is not an
/// allowlist.
pub const AUDIENCE_MAX: usize = 16;
/// Maximum characters in one audience id.
pub const AUDIENCE_ID_MAX_CHARS: usize = 64;

/// Maximum assertions **per kind** — mirrors
/// `playbook::entry::ASSERTION_LIST_MAX`.
pub const ASSERTION_LIST_MAX: usize = 6;
/// Maximum characters in one assertion value — mirrors
/// `playbook::entry::ASSERTION_TOKEN_MAX_CHARS`.
pub const ASSERTION_TOKEN_MAX_CHARS: usize = 80;

/// Maximum characters in `objective`. One sentence, not a briefing.
pub const OBJECTIVE_MAX_CHARS: usize = 1_000;

/// Maximum entries in `evidence_index` — it is an index, and an unbounded
/// one would re-import the content the packet exists to keep out.
pub const EVIDENCE_INDEX_MAX: usize = 32;

// ── What a caller has to be told ────────────────────────────────────────

/// The seven keys a caller must supply. Everything else on [`TaskPacket`] has
/// a serde default, so a packet with exactly these keys is complete, not a
/// draft.
pub const REQUIRED_PACKET_KEYS: &[&str] = &[
    "packet_id",
    "goal_id",
    "round",
    "from_role",
    "to_role",
    "objective",
    "output_format",
];

/// Every key a caller *may* add, in declaration order. Kept next to
/// [`REQUIRED_PACKET_KEYS`] because the two together are what the
/// `team_handoff` tool description shows a model — "required keys: …" with no
/// list of the optional ones reads as "these seven and nothing else".
pub const OPTIONAL_PACKET_KEYS: &[&str] = &[
    "parent_packet",
    "tool_scope",
    "boundaries",
    "constraints",
    "audience",
    "acceptance",
    "acceptance_baseline_ref",
    "artifacts",
    "wiki_refs",
    "memory_refs",
    "state_keys",
    "evidence_index",
    "findings",
    "open_questions",
    "blockers",
    "next_steps",
    "fidelity",
    "budget",
    "irreversible",
];

/// A complete, valid packet with placeholders — the copy the `team_handoff`
/// tool description and `docs/spec/task-packet.md` show verbatim.
///
/// Valid JSON on purpose (`round` is a real number, the placeholders are
/// string values), so the drift test can parse it. A live round-2 planner
/// burned twelve consecutive `invalid_packet` refusals with only a field list
/// and a doc path it could not open; an example it can copy is the fix.
pub const MINIMAL_PACKET_EXAMPLE: &str = r#"{"packet_id":"pk-1","goal_id":"<your task_id>","round":1,"from_role":"planner","to_role":"executor","objective":"one sentence: what the next role must do","output_format":"markdown"}"#;

/// The same minimal packet with this member's real identity filled in, for
/// the composer's role header (where the caller already knows its task, round
/// and both roles).
pub fn minimal_packet_example(goal_id: &str, round: u32, from: Role, to: Role) -> String {
    format!(
        r#"{{"packet_id":"pk-1","goal_id":"{goal_id}","round":{round},"from_role":"{}","to_role":"{}","objective":"一句話:這一段要做什麼","output_format":"markdown"}}"#,
        from.as_str(),
        to.as_str(),
    )
}

// ── On-disk layout ──────────────────────────────────────────────────────

/// Directory under the DuDuClaw home that holds every team packet.
pub const TEAM_PACKETS_DIR: &str = "team_packets";

/// Highest `round` that can appear in a packet path.
///
/// The goal loop's own iteration cap is 5, so this is four orders of
/// magnitude of headroom — it exists only so a corrupt or hostile `round`
/// cannot mint an unbounded number of directories.
pub const PACKET_ROUND_MAX: u32 = 10_000;

/// Where the packet for one `(task, round, from → to)` edge lives:
/// `<home>/team_packets/<task_id>/r<round>/<from>-to-<to>.json`.
///
/// This is the **only** path the handoff tool writes and the composer reads.
/// It is derived, never taken from a caller, so a packet cannot be planted
/// anywhere else in the home.
///
/// # Fail-closed
///
/// Returns [`PacketError::InvalidPathComponent`] rather than a path when
/// either component is outside contract:
///
/// * `task_id` must satisfy [`crate::is_valid_agent_id`] — the project's
///   broad "safe to use as a path/log component" predicate (ASCII
///   alphanumerics plus `-`/`_`, non-empty, ≤64 chars). Goal task ids are
///   UUIDv4 strings, which pass. Path separators, `..`, `.`, NUL, drive
///   letters and every other traversal shape do not, so directory escape is
///   rejected at the type boundary instead of being sanitized into something
///   that merely *looks* safe.
/// * `round` must be ≤ [`PACKET_ROUND_MAX`].
///
/// A fallible return is deliberate: a "best effort" `PathBuf` for an invalid
/// id would have to either panic or silently substitute a different location,
/// and both are worse than a refusal the caller must handle (coding
/// convention 4 — security gates fail closed).
pub fn packet_path(
    home: &Path,
    task_id: &str,
    round: u32,
    from: Role,
    to: Role,
) -> Result<PathBuf, PacketError> {
    if !crate::is_valid_agent_id(task_id) {
        return Err(PacketError::InvalidPathComponent { field: "task_id" });
    }
    if round > PACKET_ROUND_MAX {
        return Err(PacketError::InvalidPathComponent { field: "round" });
    }
    Ok(home
        .join(TEAM_PACKETS_DIR)
        .join(task_id)
        .join(format!("r{round}"))
        .join(format!("{}-to-{}.json", from.as_str(), to.as_str())))
}

// ── Leaf types ──────────────────────────────────────────────────────────

/// Expected shape of the downstream role's product (Anthropic's second
/// subagent element).
///
/// # Two accepted input spellings, one output spelling
///
/// Serialization is always the internally-tagged object
/// (`{"kind":"markdown"}`), so what lands on disk and what the composer reads
/// never changes. **Deserialization** additionally accepts the bare token
/// (`"markdown"`), because that is what a model writes when it has only been
/// told the four legal values — a live round-2 planner spent twelve
/// consecutive `invalid_packet` refusals on schema shape rather than on the
/// work. Accepting the short form costs nothing and removes a whole class of
/// refusal; `{"kind":"json","schema":…}` stays the only way to carry a schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputFormat {
    /// Prose / report.
    Markdown,
    /// Structured data. `schema` names or inlines the expected shape; `None`
    /// means "JSON, shape agreed out of band".
    Json { schema: Option<String> },
    /// A unified diff against the workspace.
    Diff,
    /// Files written to disk, reported via [`TaskPacket::artifacts`].
    Files,
}

/// The tagged wire form, kept as its own derived type so the hand-written
/// [`Deserialize`] below can delegate to it — and so `deny_unknown_fields`
/// still produces serde's own "unknown field `x`, expected one of …"
/// message, which is a far better teacher than a custom one.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OutputFormatTagged {
    Markdown,
    Json {
        #[serde(default)]
        schema: Option<String>,
    },
    Diff,
    Files,
}

impl From<OutputFormatTagged> for OutputFormat {
    fn from(t: OutputFormatTagged) -> Self {
        match t {
            OutputFormatTagged::Markdown => OutputFormat::Markdown,
            OutputFormatTagged::Json { schema } => OutputFormat::Json { schema },
            OutputFormatTagged::Diff => OutputFormat::Diff,
            OutputFormatTagged::Files => OutputFormat::Files,
        }
    }
}

impl OutputFormat {
    /// The four bare tokens the short input form accepts, in the order the
    /// tool description and the spec list them.
    pub const TOKENS: &'static [&'static str] = &["markdown", "json", "diff", "files"];

    /// Parse the short form. Trimmed and ASCII-case-insensitive (a model that
    /// writes `"Markdown"` means `markdown`), but always an exact token match
    /// — never a substring test (coding convention 2).
    pub fn from_token(token: &str) -> Option<Self> {
        let t = token.trim();
        if t.eq_ignore_ascii_case("markdown") {
            Some(OutputFormat::Markdown)
        } else if t.eq_ignore_ascii_case("json") {
            Some(OutputFormat::Json { schema: None })
        } else if t.eq_ignore_ascii_case("diff") {
            Some(OutputFormat::Diff)
        } else if t.eq_ignore_ascii_case("files") {
            Some(OutputFormat::Files)
        } else {
            None
        }
    }

    /// Stable token for audit rows.
    pub fn code(&self) -> &'static str {
        match self {
            OutputFormat::Markdown => "markdown",
            OutputFormat::Json { .. } => "json",
            OutputFormat::Diff => "diff",
            OutputFormat::Files => "files",
        }
    }
}

impl<'de> Deserialize<'de> for OutputFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FormatVisitor;

        impl<'de> serde::de::Visitor<'de> for FormatVisitor {
            type Value = OutputFormat;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "\"markdown\" | \"json\" | \"diff\" | \"files\", \
                     or {\"kind\":\"json\",\"schema\":null}",
                )
            }

            fn visit_str<E>(self, v: &str) -> Result<OutputFormat, E>
            where
                E: serde::de::Error,
            {
                OutputFormat::from_token(v).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "output_format must be one of \"markdown\" / \"json\" / \"diff\" / \
                         \"files\" (or the tagged form {{\"kind\":\"json\",\"schema\":null}}), \
                         got `{v}`"
                    ))
                })
            }

            fn visit_map<A>(self, map: A) -> Result<OutputFormat, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                OutputFormatTagged::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(OutputFormat::from)
            }
        }

        deserializer.deserialize_any(FormatVisitor)
    }
}

/// Tool guidance (Anthropic's third element).
///
/// **NOT WIRED — human-readable only (as of the 2026-09 review).** Both lists
/// are carried, validated and stored, and nothing reads them: they reach no
/// spawn, no `--allowedTools` / `--disallowedTools`, and
/// `render_packet_for_prompt` does not even render them, so a planner writing
/// `tool_scope.denied = ["mail_send"]` gets **no effect at all**. A role
/// member's real tool envelope comes from the employee's `[capabilities]` via
/// `ephemeral::check_tool_subset`, which this field does not participate in.
///
/// This doc previously claimed "the composer turns them into the runtime's
/// `--allowedTools` / `--disallowedTools`, so a denial here is a real denial,
/// not a request". That was false in every build that has shipped, and an
/// operator reading it would have believed a boundary existed where none did —
/// the one documentation failure mode worse than silence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolScope {
    pub allowed: Vec<String>,
    pub denied: Vec<String>,
}

impl ToolScope {
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty() && self.denied.is_empty()
    }
}

/// One enumerated constraint. Numbered rather than prose because the
/// downstream role — and the verifier — must be able to cite *which* one was
/// violated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    /// Short stable handle (`c1`, `no-external-email`, …).
    pub id: String,
    /// The constraint itself, ≤ [`CONSTRAINT_TEXT_MAX_CHARS`] characters.
    pub text: String,
}

impl Constraint {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// The four machine-checkable assertion kinds, mirroring
/// `playbook::EntryAssertions`'s four lists one-for-one.
/// The wire value of each variant is deliberately the gateway's
/// `EntryAssertions` **field name** (plural for the tool lists), so
/// [`AssertionKind::as_str`] and the serde encoding can never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssertionKind {
    /// A tool the turn MUST call at least once.
    #[serde(rename = "must_use_tools")]
    MustUseTool,
    /// A tool the turn must NOT call.
    #[serde(rename = "must_not_use_tools")]
    MustNotUseTool,
    /// A substring the final answer MUST contain.
    #[serde(rename = "output_contains")]
    OutputContains,
    /// A substring the final answer must NOT contain.
    #[serde(rename = "output_not_contains")]
    OutputNotContains,
}

impl AssertionKind {
    pub const ALL: &'static [AssertionKind] = &[
        AssertionKind::MustUseTool,
        AssertionKind::MustNotUseTool,
        AssertionKind::OutputContains,
        AssertionKind::OutputNotContains,
    ];

    /// Stable token; also the `EntryAssertions` field name on the gateway
    /// side, which is what makes the conversion mechanical.
    pub fn as_str(&self) -> &'static str {
        match self {
            AssertionKind::MustUseTool => "must_use_tools",
            AssertionKind::MustNotUseTool => "must_not_use_tools",
            AssertionKind::OutputContains => "output_contains",
            AssertionKind::OutputNotContains => "output_not_contains",
        }
    }

    /// The kind that would contradict this one.
    fn opposite(&self) -> AssertionKind {
        match self {
            AssertionKind::MustUseTool => AssertionKind::MustNotUseTool,
            AssertionKind::MustNotUseTool => AssertionKind::MustUseTool,
            AssertionKind::OutputContains => AssertionKind::OutputNotContains,
            AssertionKind::OutputNotContains => AssertionKind::OutputContains,
        }
    }

    /// Tool-name kinds compare case-insensitively (as `EntryAssertions` does);
    /// output substrings are compared verbatim, because case can be the point.
    fn case_insensitive(&self) -> bool {
        matches!(
            self,
            AssertionKind::MustUseTool | AssertionKind::MustNotUseTool
        )
    }
}

/// One acceptance assertion — a zero-LLM pre-filter the verifier can replay
/// against the recorded turn before spending a judge call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub kind: AssertionKind,
    pub value: String,
}

impl Assertion {
    pub fn new(kind: AssertionKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

/// A reference to something in `artifacts.jsonl` — never the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// `artifacts.jsonl` id.
    pub id: String,
    /// Optional workspace-relative path, for a human reading the packet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional content hash, so a downstream role can detect a swap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// One confirmed fact, with the evidence it rests on. Not a prose summary:
/// a finding with an empty `evidence` list is a claim, and the packet says so
/// by keeping the two fields separate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Finding {
    pub text: String,
    /// Ids drawn from `artifacts` / `evidence_index` / `memory_refs`.
    pub evidence: Vec<String>,
}

/// Evidence grade of the upstream role's observations.
///
/// Core-local mirror of `duduclaw_gateway::prediction::task_forward::
/// ObservationFidelity`; the gateway maps its value onto this one when
/// building a packet. The three grades are **never** conflated — a verifier
/// that cannot tell `None` from `McpOnly` will read "no tool calls recorded"
/// as "no tool calls made".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// Native tool events plus MCP audit — both visible, with outcomes.
    Full,
    /// MCP audit log only (`tool_calls.jsonl`). The main branch today.
    McpOnly,
    /// No tool evidence at all.
    #[default]
    None,
}

impl Fidelity {
    /// Stable snake_case token, identical to the gateway's spellings.
    /// Hand-written, not `Debug`-derived: `McpOnly` lowercases to
    /// `"mcponly"`, which has already silently broken one column contract in
    /// this workspace.
    pub fn as_str(&self) -> &'static str {
        match self {
            Fidelity::Full => "full",
            Fidelity::McpOnly => "mcp_only",
            Fidelity::None => "none",
        }
    }
}

/// Execution budget for the downstream role. Every field is optional: unset
/// means "the dispatcher's own limit applies", which is not the same as
/// "unlimited".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

impl Budget {
    pub fn is_empty(&self) -> bool {
        *self == Budget::default()
    }
}

// ── The packet ──────────────────────────────────────────────────────────

/// The single cross-role handoff type.
///
/// Field groups, in declaration order: identity and lineage; Anthropic's four
/// subagent elements; the incompressible section; the acceptance contract;
/// references (never content); structured upstream notes; observation grade;
/// execution budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPacket {
    // ── Identity and lineage (maps onto A2A Task.id / contextId /
    //    referenceTaskIds, so an ACP bridge is a rename, not a redesign).
    pub packet_id: String,
    /// The goal task this packet serves.
    pub goal_id: String,
    /// The packet this one answers, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_packet: Option<String>,
    /// Goal-loop round, shared with `task_iterations.round`.
    pub round: u32,
    pub from_role: Role,
    pub to_role: Role,

    // ── Anthropic's four elements ───────────────────────────────────────
    /// One sentence: what to do. Anthropic's own write-up records a vague
    /// objective ("research the semiconductor shortage") producing duplicated
    /// work across subagents.
    pub objective: String,
    pub output_format: OutputFormat,
    #[serde(default, skip_serializing_if = "ToolScope::is_empty")]
    pub tool_scope: ToolScope,
    /// Explicitly what NOT to do.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundaries: Vec<String>,

    // ── Incompressible section ──────────────────────────────────────────
    /// Enumerated, ≤ [`CONSTRAINTS_MAX`] × [`CONSTRAINT_TEXT_MAX_CHARS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
    /// Allowlist of role / channel ids this content may reach. Empty means
    /// "no declared audience", which the composer treats as the enclosing
    /// task's own audience — it is NOT "everyone".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audience: Vec<String>,

    // ── Acceptance contract ─────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance: Vec<Assertion>,
    /// Points back at the goal's frozen `acceptance_criteria_baseline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_baseline_ref: Option<String>,

    // ── References, never content ───────────────────────────────────────
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wiki_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_refs: Vec<String>,
    /// `working_state` authoritative keys this role may rely on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_keys: Vec<String>,
    /// "Title + id" of the key material upstream actually looked at, so the
    /// downstream role knows *what to ask for* — the known weak point of a
    /// pull-based handoff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_index: Vec<String>,

    // ── Structured upstream notes ───────────────────────────────────────
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,

    // ── Honesty and limits ──────────────────────────────────────────────
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Budget::is_empty")]
    pub budget: Budget,
    /// **NOT WIRED — human-readable only (as of the 2026-09 review).** A
    /// declaration the packet carries and nothing consumes: it triggers no
    /// ActionGuard check and no artifact-receipt comparison, and the composer
    /// does not render it into any prompt. ActionGuard still guards
    /// irreversible calls at the point of the call, entirely independently of
    /// this field. (The previous doc claimed it "triggers ActionGuard plus a
    /// full artifact-receipt comparison downstream" — it never has.)
    #[serde(default)]
    pub irreversible: bool,
}

impl TaskPacket {
    /// A minimal packet: identity, lineage and Anthropic's first two
    /// elements. Everything else starts empty, which is a valid packet.
    pub fn new(
        packet_id: impl Into<String>,
        goal_id: impl Into<String>,
        round: u32,
        from_role: Role,
        to_role: Role,
        objective: impl Into<String>,
        output_format: OutputFormat,
    ) -> Self {
        Self {
            packet_id: packet_id.into(),
            goal_id: goal_id.into(),
            parent_packet: None,
            round,
            from_role,
            to_role,
            objective: objective.into(),
            output_format,
            tool_scope: ToolScope::default(),
            boundaries: Vec::new(),
            constraints: Vec::new(),
            audience: Vec::new(),
            acceptance: Vec::new(),
            acceptance_baseline_ref: None,
            artifacts: Vec::new(),
            wiki_refs: Vec::new(),
            memory_refs: Vec::new(),
            state_keys: Vec::new(),
            evidence_index: Vec::new(),
            findings: Vec::new(),
            open_questions: Vec::new(),
            blockers: Vec::new(),
            next_steps: Vec::new(),
            fidelity: Fidelity::None,
            budget: Budget::default(),
            irreversible: false,
        }
    }

    /// Compact-JSON size, the unit [`TASK_PACKET_MAX_BYTES`] is measured in.
    /// A packet that cannot serialize at all reports `usize::MAX` so the cap
    /// check fails closed rather than passing a packet nobody can send.
    pub fn serialized_len(&self) -> usize {
        serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
    }

    /// Structural validation. Every failure **rejects the whole packet** —
    /// nothing here truncates, clamps or drops an element.
    ///
    /// Checks, in order (so the first error for a given packet is stable):
    /// identity non-blank → objective → constraints → audience → acceptance
    /// → evidence index → total size.
    pub fn validate(&self) -> Result<(), PacketError> {
        for (field, value) in [("packet_id", &self.packet_id), ("goal_id", &self.goal_id)] {
            if value.trim().is_empty() {
                return Err(PacketError::BlankField {
                    field: field.into(),
                });
            }
        }
        if self.from_role == self.to_role {
            return Err(PacketError::SelfHandoff {
                role: self.from_role,
            });
        }

        if self.objective.trim().is_empty() {
            return Err(PacketError::BlankField {
                field: "objective".into(),
            });
        }
        if self.objective.chars().count() > OBJECTIVE_MAX_CHARS {
            return Err(PacketError::FieldTooLong {
                field: "objective".into(),
                chars: self.objective.chars().count(),
                max: OBJECTIVE_MAX_CHARS,
            });
        }

        // ── constraints: the incompressible section ─────────────────────
        if self.constraints.len() > CONSTRAINTS_MAX {
            return Err(PacketError::TooManyItems {
                field: "constraints".into(),
                count: self.constraints.len(),
                max: CONSTRAINTS_MAX,
            });
        }
        let mut seen_ids: Vec<&str> = Vec::with_capacity(self.constraints.len());
        // Indexed labels (`constraints[3].text`): with twelve constraints
        // allowed, "which one" is the actionable half of the refusal, and the
        // caller is a model that has to fix the packet without reading this
        // file.
        for (i, c) in self.constraints.iter().enumerate() {
            if c.id.trim().is_empty() {
                return Err(PacketError::BlankField {
                    field: format!("constraints[{i}].id").into(),
                });
            }
            if c.id.chars().count() > CONSTRAINT_ID_MAX_CHARS {
                return Err(PacketError::FieldTooLong {
                    field: format!("constraints[{i}].id").into(),
                    chars: c.id.chars().count(),
                    max: CONSTRAINT_ID_MAX_CHARS,
                });
            }
            if c.text.trim().is_empty() {
                return Err(PacketError::BlankField {
                    field: format!("constraints[{i}].text").into(),
                });
            }
            // Unicode scalar values, so a 200-character CJK constraint is
            // 200 — not 600 as a byte count would have it.
            if c.text.chars().count() > CONSTRAINT_TEXT_MAX_CHARS {
                return Err(PacketError::FieldTooLong {
                    field: format!("constraints[{i}].text").into(),
                    chars: c.text.chars().count(),
                    max: CONSTRAINT_TEXT_MAX_CHARS,
                });
            }
            // Duplicate ids make "constraint c3 was violated" ambiguous,
            // which defeats the enumeration.
            if seen_ids.iter().any(|s| s.eq_ignore_ascii_case(c.id.trim())) {
                return Err(PacketError::DuplicateConstraintId {
                    id: c.id.trim().to_string(),
                });
            }
            seen_ids.push(c.id.trim());
        }

        // ── audience allowlist ──────────────────────────────────────────
        if self.audience.len() > AUDIENCE_MAX {
            return Err(PacketError::TooManyItems {
                field: "audience".into(),
                count: self.audience.len(),
                max: AUDIENCE_MAX,
            });
        }
        for (i, a) in self.audience.iter().enumerate() {
            if a.trim().is_empty() {
                return Err(PacketError::BlankField {
                    field: format!("audience[{i}]").into(),
                });
            }
            if a.chars().count() > AUDIENCE_ID_MAX_CHARS {
                return Err(PacketError::FieldTooLong {
                    field: format!("audience[{i}]").into(),
                    chars: a.chars().count(),
                    max: AUDIENCE_ID_MAX_CHARS,
                });
            }
        }

        // ── acceptance: same shape and caps as EntryAssertions, so the
        //    gateway conversion is lossless in both directions.
        for kind in AssertionKind::ALL {
            let count = self.acceptance.iter().filter(|a| a.kind == *kind).count();
            if count > ASSERTION_LIST_MAX {
                return Err(PacketError::TooManyItems {
                    field: kind.as_str().into(),
                    count,
                    max: ASSERTION_LIST_MAX,
                });
            }
        }
        for (i, a) in self.acceptance.iter().enumerate() {
            if a.value.trim().is_empty() {
                return Err(PacketError::BlankField {
                    field: format!("acceptance[{i}].value").into(),
                });
            }
            if a.value.chars().count() > ASSERTION_TOKEN_MAX_CHARS {
                return Err(PacketError::FieldTooLong {
                    field: format!("acceptance[{i}].value").into(),
                    chars: a.value.chars().count(),
                    max: ASSERTION_TOKEN_MAX_CHARS,
                });
            }
            let opposite = a.kind.opposite();
            let contradicted = self.acceptance.iter().any(|b| {
                b.kind == opposite
                    && if a.kind.case_insensitive() {
                        b.value.eq_ignore_ascii_case(&a.value)
                    } else {
                        b.value == a.value
                    }
            });
            if contradicted {
                return Err(PacketError::ContradictoryAssertion {
                    value: a.value.clone(),
                });
            }
        }

        if self.evidence_index.len() > EVIDENCE_INDEX_MAX {
            return Err(PacketError::TooManyItems {
                field: "evidence_index".into(),
                count: self.evidence_index.len(),
                max: EVIDENCE_INDEX_MAX,
            });
        }

        // ── whole-packet ceiling, last: it is the cheapest check to state
        //    and the most expensive to run.
        let bytes = self.serialized_len();
        if bytes > TASK_PACKET_MAX_BYTES {
            return Err(PacketError::TooLarge {
                bytes,
                max: TASK_PACKET_MAX_BYTES,
            });
        }

        Ok(())
    }
}

/// Why a [`TaskPacket`] is not sendable. Closed enum with a stable
/// [`PacketError::code`] for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    /// A required field is empty or whitespace-only.
    ///
    /// `field` is `Cow` rather than `&'static str` so an item inside a list
    /// can name its own index (`constraints[3].text`) — the caller fixing the
    /// packet is a model that cannot read this file, so "which one" has to be
    /// in the message.
    BlankField { field: Cow<'static, str> },
    /// A field exceeded its character cap (counted in Unicode scalar values).
    FieldTooLong {
        field: Cow<'static, str>,
        chars: usize,
        max: usize,
    },
    /// A list exceeded its item cap.
    TooManyItems {
        field: Cow<'static, str>,
        count: usize,
        max: usize,
    },
    /// Two constraints share an id, so "constraint X was violated" would be
    /// ambiguous.
    DuplicateConstraintId { id: String },
    /// The same value appears in an assertion kind and its opposite — it can
    /// never be satisfied.
    ContradictoryAssertion { value: String },
    /// `from_role == to_role`; a packet is a handoff, not a note to self.
    SelfHandoff { role: Role },
    /// The serialized packet exceeds [`TASK_PACKET_MAX_BYTES`]. Rejected
    /// whole — see the module docs.
    TooLarge { bytes: usize, max: usize },
    /// A value that would become a filesystem path component is outside
    /// contract (see [`packet_path`]). Never produced by
    /// [`TaskPacket::validate`].
    InvalidPathComponent { field: &'static str },
}

impl PacketError {
    /// Stable snake_case token. Fixed strings, never `Debug`-derived.
    pub fn code(&self) -> &'static str {
        match self {
            PacketError::BlankField { .. } => "blank_field",
            PacketError::FieldTooLong { .. } => "field_too_long",
            PacketError::TooManyItems { .. } => "too_many_items",
            PacketError::DuplicateConstraintId { .. } => "duplicate_constraint_id",
            PacketError::ContradictoryAssertion { .. } => "contradictory_assertion",
            PacketError::SelfHandoff { .. } => "self_handoff",
            PacketError::TooLarge { .. } => "packet_too_large",
            PacketError::InvalidPathComponent { .. } => "invalid_path_component",
        }
    }
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::BlankField { field } => write!(f, "`{field}` must not be blank"),
            PacketError::FieldTooLong { field, chars, max } => {
                write!(f, "`{field}` is {chars} chars (max {max})")
            }
            PacketError::TooManyItems { field, count, max } => {
                write!(f, "`{field}` has {count} items (max {max})")
            }
            PacketError::DuplicateConstraintId { id } => {
                write!(f, "duplicate constraint id `{id}`")
            }
            PacketError::ContradictoryAssertion { value } => write!(
                f,
                "`{value}` appears in an assertion and its opposite; it can never be satisfied"
            ),
            PacketError::SelfHandoff { role } => {
                write!(f, "from_role and to_role are both `{role}`")
            }
            PacketError::TooLarge { bytes, max } => write!(
                f,
                "packet is {bytes} bytes (max {max}); rejected whole — never truncated"
            ),
            PacketError::InvalidPathComponent { field } => {
                write!(f, "`{field}` cannot be used as a packet path component")
            }
        }
    }
}

impl std::error::Error for PacketError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> TaskPacket {
        TaskPacket::new(
            "pk-1",
            "goal-7",
            2,
            Role::Planner,
            Role::Executor,
            "整理上季合約到期清單",
            OutputFormat::Json { schema: None },
        )
    }

    // ── round-trip ──────────────────────────────────────────────────────

    #[test]
    fn json_round_trip_preserves_every_field() {
        let mut p = packet();
        p.parent_packet = Some("pk-0".into());
        p.tool_scope = ToolScope {
            allowed: vec!["db_select".into()],
            denied: vec!["mail_send".into()],
        };
        p.boundaries = vec!["不要寄信給客戶".into()];
        p.constraints = vec![Constraint::new("c1", "只讀 2026 Q2 的資料")];
        p.audience = vec!["verifier".into(), "channel:telegram".into()];
        p.acceptance = vec![
            Assertion::new(AssertionKind::MustUseTool, "db_select"),
            Assertion::new(AssertionKind::OutputContains, "到期"),
        ];
        p.acceptance_baseline_ref = Some("goal-7#baseline".into());
        p.artifacts = vec![ArtifactRef {
            id: "a1".into(),
            path: Some("out/list.csv".into()),
            sha256: Some("deadbeef".into()),
        }];
        p.wiki_refs = vec!["auto/sop/contract-renewal".into()];
        p.memory_refs = vec!["m-42".into()];
        p.state_keys = vec!["quarter".into()];
        p.evidence_index = vec!["合約母檔 · a1".into()];
        p.findings = vec![Finding {
            text: "共 14 筆到期".into(),
            evidence: vec!["a1".into()],
        }];
        p.open_questions = vec!["要含自動續約嗎？".into()];
        p.blockers = vec![];
        p.next_steps = vec!["交給 verifier".into()];
        p.fidelity = Fidelity::McpOnly;
        p.budget = Budget {
            max_turns: Some(5),
            max_tokens: Some(120_000),
            wall_clock_secs: Some(900),
            max_cost_usd: Some(1.25),
        };
        p.irreversible = true;

        let json = serde_json::to_string(&p).unwrap();
        let back: TaskPacket = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        p.validate().expect("the fully-populated packet is valid");
    }

    #[test]
    fn minimal_packet_round_trips_and_validates() {
        let p = packet();
        let json = serde_json::to_string(&p).unwrap();
        let back: TaskPacket = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        p.validate().unwrap();
    }

    /// The exact 7-key example the `team_handoff` tool description and the
    /// composer's role header hand to a model. If this stops deserializing,
    /// every role member in every team is filing packets against a promise the
    /// validator no longer keeps.
    const MINIMAL_EXAMPLE: &str = MINIMAL_PACKET_EXAMPLE;

    fn sorted_keys(v: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    fn sorted(keys: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        out.sort();
        out
    }

    #[test]
    fn the_documented_minimal_packet_deserializes_and_validates() {
        let p: TaskPacket = serde_json::from_str(MINIMAL_EXAMPLE).expect("7 keys is enough");
        p.validate().expect("the minimal packet is valid");
        assert_eq!(p.packet_id, "pk-1");
        assert_eq!(p.round, 1);
        assert_eq!(p.from_role, Role::Planner);
        assert_eq!(p.to_role, Role::Executor);
        assert_eq!(p.output_format, OutputFormat::Markdown);
        // Every other field defaulted to empty — which is a valid packet, not
        // a half-filled one.
        assert!(p.constraints.is_empty());
        assert!(p.audience.is_empty());
        assert!(p.acceptance.is_empty());
        assert!(p.findings.is_empty());
        assert!(p.next_steps.is_empty());
        assert_eq!(p.fidelity, Fidelity::None);
        assert!(p.budget.is_empty());
        assert!(!p.irreversible);
        assert!(p.parent_packet.is_none());
        assert!(p.tool_scope.is_empty());
    }

    #[test]
    fn every_optional_field_may_be_omitted() {
        // A fully-populated packet serializes every key there is, so the two
        // documented lists together must cover it exactly — a field added
        // without a serde default, or added and never documented, fails here
        // rather than in a live handoff.
        let mut all = packet();
        all.parent_packet = Some("pk-0".into());
        all.tool_scope = ToolScope {
            allowed: vec!["db_select".into()],
            denied: vec![],
        };
        all.boundaries = vec!["b".into()];
        all.constraints = vec![Constraint::new("c1", "t")];
        all.audience = vec!["verifier".into()];
        all.acceptance = vec![Assertion::new(AssertionKind::MustUseTool, "db_select")];
        all.acceptance_baseline_ref = Some("r".into());
        all.artifacts = vec![ArtifactRef {
            id: "a1".into(),
            path: None,
            sha256: None,
        }];
        all.wiki_refs = vec!["w".into()];
        all.memory_refs = vec!["m".into()];
        all.state_keys = vec!["k".into()];
        all.evidence_index = vec!["e".into()];
        all.findings = vec![Finding::default()];
        all.open_questions = vec!["q".into()];
        all.blockers = vec!["b".into()];
        all.next_steps = vec!["n".into()];
        all.budget = Budget {
            max_turns: Some(1),
            ..Budget::default()
        };
        let full = serde_json::to_value(&all).unwrap();
        for key in full.as_object().unwrap().keys() {
            assert!(
                REQUIRED_PACKET_KEYS.contains(&key.as_str())
                    || OPTIONAL_PACKET_KEYS.contains(&key.as_str()),
                "`{key}` is documented neither as required nor as optional"
            );
        }

        let minimal: serde_json::Value = serde_json::from_str(MINIMAL_EXAMPLE).unwrap();
        // Sorted, not declaration order: `serde_json::Map` is a `BTreeMap`
        // here (no `preserve_order` feature), so the *set* is the contract.
        assert_eq!(
            sorted_keys(&minimal),
            sorted(REQUIRED_PACKET_KEYS),
            "the example must carry exactly the required keys"
        );
        // Absent already — asserting removal is still a no-op keeps the
        // documented list and the serde defaults from drifting apart.
        for key in OPTIONAL_PACKET_KEYS {
            let mut v = minimal.clone();
            v.as_object_mut().unwrap().remove(*key);
            serde_json::from_value::<TaskPacket>(v)
                .unwrap_or_else(|e| panic!("`{key}` must be optional: {e}"));
        }
    }

    #[test]
    fn the_composer_example_matches_the_tool_description_example() {
        // Same seven keys in the same order; only the identity values and the
        // language of the objective placeholder differ.
        let rendered = minimal_packet_example("task-1183", 2, Role::Executor, Role::Verifier);
        let v: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(sorted_keys(&v), sorted(REQUIRED_PACKET_KEYS));
        let p: TaskPacket = serde_json::from_str(&rendered).unwrap();
        p.validate()
            .expect("the rendered example is a valid packet");
        assert_eq!(p.goal_id, "task-1183");
        assert_eq!(p.round, 2);
        assert_eq!(p.from_role, Role::Executor);
        assert_eq!(p.to_role, Role::Verifier);
    }

    #[test]
    fn output_format_accepts_the_bare_token_as_well_as_the_tagged_object() {
        for (token, expect) in [
            ("markdown", OutputFormat::Markdown),
            ("json", OutputFormat::Json { schema: None }),
            ("diff", OutputFormat::Diff),
            ("files", OutputFormat::Files),
            // Trimmed and ASCII-case-insensitive: a model writing `Markdown`
            // means markdown, and refusing it teaches nothing.
            ("  Markdown ", OutputFormat::Markdown),
            ("FILES", OutputFormat::Files),
        ] {
            let got: OutputFormat =
                serde_json::from_str(&serde_json::to_string(token).unwrap()).unwrap();
            assert_eq!(got, expect, "{token:?}");
        }
        // The tagged form is unchanged, including a missing `schema`.
        assert_eq!(
            serde_json::from_str::<OutputFormat>(r#"{"kind":"json"}"#).unwrap(),
            OutputFormat::Json { schema: None }
        );
        // Serialization is ALWAYS the tagged object — the short form is an
        // input tolerance, never a second on-disk spelling.
        assert_eq!(
            serde_json::to_string(&OutputFormat::Files).unwrap(),
            r#"{"kind":"files"}"#
        );
        // An unknown token names the four legal values instead of saying
        // "invalid value".
        let err = serde_json::from_str::<OutputFormat>("\"csv\"").unwrap_err();
        let msg = err.to_string();
        for token in OutputFormat::TOKENS {
            assert!(msg.contains(token), "`{token}` must appear in: {msg}");
        }
        assert!(msg.contains("csv"), "the rejected token must appear: {msg}");
        // The tagged form still refuses a smuggled key by name.
        let err = serde_json::from_str::<OutputFormat>(r#"{"kind":"json","thinking":"x"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("thinking"), "{err}");
    }

    #[test]
    fn a_missing_required_field_names_itself() {
        // This is the whole point of the round-2 fix: the refusal a model gets
        // has to say which key is missing.
        for key in REQUIRED_PACKET_KEYS {
            let mut v: serde_json::Value = serde_json::from_str(MINIMAL_EXAMPLE).unwrap();
            v.as_object_mut().unwrap().remove(*key);
            let err = serde_json::from_value::<TaskPacket>(v)
                .expect_err("required")
                .to_string();
            assert!(
                err.contains(key),
                "the serde message must name `{key}`: {err}"
            );
        }
    }

    #[test]
    fn validation_messages_name_the_offending_index() {
        let mut p = packet();
        p.constraints = vec![
            Constraint::new("c1", "ok"),
            Constraint::new("c2", "ok"),
            Constraint::new("c3", "ok"),
            Constraint::new("c4", "越".repeat(CONSTRAINT_TEXT_MAX_CHARS + 1)),
        ];
        let err = p.validate().unwrap_err();
        assert_eq!(err.code(), "field_too_long");
        assert!(
            err.to_string().contains("constraints[3].text"),
            "must name the item: {err}"
        );

        let mut p = packet();
        p.audience = vec!["verifier".into(), "  ".into()];
        assert!(
            p.validate()
                .unwrap_err()
                .to_string()
                .contains("audience[1]")
        );

        let mut p = packet();
        p.acceptance = vec![
            Assertion::new(AssertionKind::MustUseTool, "db_select"),
            Assertion::new(AssertionKind::OutputContains, " "),
        ];
        assert!(
            p.validate()
                .unwrap_err()
                .to_string()
                .contains("acceptance[1].value")
        );
    }

    #[test]
    fn output_format_variants_round_trip() {
        for fmt in [
            OutputFormat::Markdown,
            OutputFormat::Json { schema: None },
            OutputFormat::Json {
                schema: Some("{\"type\":\"array\"}".into()),
            },
            OutputFormat::Diff,
            OutputFormat::Files,
        ] {
            let json = serde_json::to_string(&fmt).unwrap();
            let back: OutputFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(back, fmt, "{json}");
        }
        assert_eq!(
            serde_json::to_string(&OutputFormat::Markdown).unwrap(),
            r#"{"kind":"markdown"}"#
        );
    }

    #[test]
    fn fidelity_spellings_match_the_gateway_contract() {
        assert_eq!(Fidelity::Full.as_str(), "full");
        // The underscore is the whole point — a Debug-derived lowercase
        // would produce "mcponly".
        assert_eq!(Fidelity::McpOnly.as_str(), "mcp_only");
        assert_eq!(Fidelity::None.as_str(), "none");
        assert_eq!(
            serde_json::to_string(&Fidelity::McpOnly).unwrap(),
            "\"mcp_only\""
        );
        assert_eq!(Fidelity::default(), Fidelity::None);
    }

    // ── deliberately absent fields ──────────────────────────────────────

    #[test]
    fn rejects_deliberately_absent_provider_fields() {
        let base = serde_json::to_value(packet()).unwrap();
        for key in [
            "transcript",
            "tool_use",
            "function_call",
            "functionCall",
            "thinking",
            "reasoning",
            "encrypted_content",
            "messages",
        ] {
            let mut v = base.clone();
            v.as_object_mut()
                .unwrap()
                .insert(key.to_string(), serde_json::json!("anything"));
            let got = serde_json::from_value::<TaskPacket>(v);
            assert!(
                got.is_err(),
                "`{key}` must be rejected by deny_unknown_fields, not silently ignored"
            );
        }
    }

    #[test]
    fn nested_types_also_deny_unknown_fields() {
        // The escape hatch would otherwise just move one level down.
        assert!(
            serde_json::from_str::<Constraint>(r#"{"id":"c1","text":"t","thinking":"x"}"#).is_err()
        );
        assert!(serde_json::from_str::<ArtifactRef>(r#"{"id":"a","tool_use":{}}"#).is_err());
        assert!(serde_json::from_str::<Finding>(r#"{"text":"t","transcript":"x"}"#).is_err());
        assert!(serde_json::from_str::<ToolScope>(r#"{"allowed":[],"raw":"x"}"#).is_err());
        assert!(serde_json::from_str::<Budget>(r#"{"max_turns":1,"reasoning":"x"}"#).is_err());
        assert!(
            serde_json::from_str::<Assertion>(r#"{"kind":"must_use_tools","value":"v","extra":1}"#)
                .is_err()
        );
    }

    // ── caps ────────────────────────────────────────────────────────────

    #[test]
    fn constraints_cap_is_twelve() {
        let mut p = packet();
        p.constraints = (0..CONSTRAINTS_MAX)
            .map(|i| Constraint::new(format!("c{i}"), "ok"))
            .collect();
        p.validate().expect("exactly at the cap is fine");

        p.constraints.push(Constraint::new("c99", "one too many"));
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::TooManyItems {
                field: "constraints".into(),
                count: CONSTRAINTS_MAX + 1,
                max: CONSTRAINTS_MAX,
            }
        );
    }

    #[test]
    fn constraint_text_cap_counts_characters_not_bytes() {
        // 200 CJK characters = 600 UTF-8 bytes. A byte-based cap would
        // reject this valid constraint (and, worse, a byte-index truncation
        // would panic mid-character).
        let cjk = "約".repeat(CONSTRAINT_TEXT_MAX_CHARS);
        assert_eq!(cjk.len(), CONSTRAINT_TEXT_MAX_CHARS * 3);
        let mut p = packet();
        p.constraints = vec![Constraint::new("c1", &cjk)];
        p.validate().expect("200 CJK chars is exactly at the cap");

        p.constraints = vec![Constraint::new("c1", format!("{cjk}約"))];
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::FieldTooLong {
                field: "constraints[0].text".into(),
                chars: CONSTRAINT_TEXT_MAX_CHARS + 1,
                max: CONSTRAINT_TEXT_MAX_CHARS,
            }
        );
    }

    #[test]
    fn oversized_packet_is_rejected_whole_never_truncated() {
        let mut p = packet();
        // Stay under every per-field cap; only the total is over.
        p.constraints = (0..CONSTRAINTS_MAX)
            .map(|i| Constraint::new(format!("c{i}"), "字".repeat(CONSTRAINT_TEXT_MAX_CHARS)))
            .collect();
        p.findings = (0..40)
            .map(|i| Finding {
                text: format!("{} {}", "詳細發現".repeat(40), i),
                evidence: vec!["a1".into()],
            })
            .collect();
        let before = p.clone();
        let err = p.validate().unwrap_err();
        assert_eq!(err.code(), "packet_too_large");
        assert!(matches!(err, PacketError::TooLarge { bytes, max }
            if bytes > TASK_PACKET_MAX_BYTES && max == TASK_PACKET_MAX_BYTES));
        // Rejection must not mutate anything.
        assert_eq!(p, before);
    }

    #[test]
    fn packet_just_under_the_ceiling_is_accepted() {
        let mut p = packet();
        // ~15 KB of ASCII in next_steps: comfortably valid, close to the cap.
        p.next_steps = (0..60)
            .map(|i| format!("{} {i}", "x".repeat(240)))
            .collect();
        assert!(p.serialized_len() < TASK_PACKET_MAX_BYTES);
        p.validate().unwrap();
    }

    #[test]
    fn audience_cap_and_blank_entries() {
        let mut p = packet();
        p.audience = (0..AUDIENCE_MAX).map(|i| format!("r{i}")).collect();
        p.validate().unwrap();
        p.audience.push("r99".into());
        assert_eq!(p.validate().unwrap_err().code(), "too_many_items");

        let mut p = packet();
        p.audience = vec!["  ".into()];
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::BlankField {
                field: "audience[0]".into()
            }
        );
    }

    #[test]
    fn acceptance_mirrors_entry_assertions_caps_and_contradictions() {
        let mut p = packet();
        p.acceptance = (0..ASSERTION_LIST_MAX)
            .map(|i| Assertion::new(AssertionKind::MustUseTool, format!("tool{i}")))
            .collect();
        p.validate().unwrap();

        p.acceptance
            .push(Assertion::new(AssertionKind::MustUseTool, "tool99"));
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::TooManyItems {
                field: "must_use_tools".into(),
                count: ASSERTION_LIST_MAX + 1,
                max: ASSERTION_LIST_MAX,
            }
        );

        // Tool names contradict case-insensitively (EntryAssertions rule)…
        let mut p = packet();
        p.acceptance = vec![
            Assertion::new(AssertionKind::MustUseTool, "db_select"),
            Assertion::new(AssertionKind::MustNotUseTool, "DB_Select"),
        ];
        assert_eq!(p.validate().unwrap_err().code(), "contradictory_assertion");

        // …output substrings do not: case can be the assertion.
        let mut p = packet();
        p.acceptance = vec![
            Assertion::new(AssertionKind::OutputContains, "PASS"),
            Assertion::new(AssertionKind::OutputNotContains, "pass"),
        ];
        p.validate().unwrap();

        // Verbatim output contradiction is still caught.
        let mut p = packet();
        p.acceptance = vec![
            Assertion::new(AssertionKind::OutputContains, "到期"),
            Assertion::new(AssertionKind::OutputNotContains, "到期"),
        ];
        assert_eq!(p.validate().unwrap_err().code(), "contradictory_assertion");
    }

    #[test]
    fn assertion_value_cap_is_cjk_safe() {
        let mut p = packet();
        p.acceptance = vec![Assertion::new(
            AssertionKind::OutputContains,
            "條".repeat(ASSERTION_TOKEN_MAX_CHARS),
        )];
        p.validate().unwrap();
        p.acceptance = vec![Assertion::new(
            AssertionKind::OutputContains,
            "條".repeat(ASSERTION_TOKEN_MAX_CHARS + 1),
        )];
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::FieldTooLong {
                field: "acceptance[0].value".into(),
                chars: ASSERTION_TOKEN_MAX_CHARS + 1,
                max: ASSERTION_TOKEN_MAX_CHARS,
            }
        );
    }

    // ── identity / sanity ───────────────────────────────────────────────

    #[test]
    fn blank_identity_and_objective_are_rejected() {
        let mut p = packet();
        p.packet_id = "  ".into();
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::BlankField {
                field: "packet_id".into()
            }
        );

        let mut p = packet();
        p.goal_id = String::new();
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::BlankField {
                field: "goal_id".into()
            }
        );

        let mut p = packet();
        p.objective = "\u{3000}".into(); // ideographic space
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::BlankField {
                field: "objective".into()
            }
        );
    }

    #[test]
    fn a_packet_is_a_handoff_not_a_note_to_self() {
        let mut p = packet();
        p.to_role = Role::Planner;
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::SelfHandoff {
                role: Role::Planner
            }
        );
    }

    #[test]
    fn duplicate_constraint_ids_are_rejected() {
        let mut p = packet();
        p.constraints = vec![Constraint::new("c1", "a"), Constraint::new("C1", "b")];
        // Ids collide case-insensitively, and the error names the *offending*
        // (second) spelling, which is the one the operator has to go fix.
        assert_eq!(
            p.validate().unwrap_err(),
            PacketError::DuplicateConstraintId { id: "C1".into() }
        );
    }

    #[test]
    fn evidence_index_is_bounded() {
        let mut p = packet();
        p.evidence_index = (0..EVIDENCE_INDEX_MAX).map(|i| format!("e{i}")).collect();
        p.validate().unwrap();
        p.evidence_index.push("e99".into());
        assert_eq!(p.validate().unwrap_err().code(), "too_many_items");
    }

    #[test]
    fn error_codes_are_unique_and_stable() {
        let all = [
            PacketError::BlankField { field: "x".into() },
            PacketError::FieldTooLong {
                field: "x".into(),
                chars: 1,
                max: 0,
            },
            PacketError::TooManyItems {
                field: "x".into(),
                count: 1,
                max: 0,
            },
            PacketError::DuplicateConstraintId { id: "x".into() },
            PacketError::ContradictoryAssertion { value: "x".into() },
            PacketError::SelfHandoff {
                role: Role::Utility,
            },
            PacketError::TooLarge { bytes: 1, max: 0 },
        ];
        let mut codes: Vec<&str> = all.iter().map(|e| e.code()).collect();
        codes.sort_unstable();
        let unique = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), unique, "error codes must be unique");
        for e in &all {
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn assertion_kind_tokens_match_entry_assertions_field_names() {
        // Lossless conversion on the gateway side depends on these exact
        // spellings.
        assert_eq!(AssertionKind::MustUseTool.as_str(), "must_use_tools");
        assert_eq!(AssertionKind::MustNotUseTool.as_str(), "must_not_use_tools");
        assert_eq!(AssertionKind::OutputContains.as_str(), "output_contains");
        assert_eq!(
            AssertionKind::OutputNotContains.as_str(),
            "output_not_contains"
        );
        for k in AssertionKind::ALL {
            assert_eq!(k.opposite().opposite(), *k);
        }
    }

    // ── packet_path (P1/WP-5) ────────────────────────────────────────────

    #[test]
    fn packet_path_shape_is_exactly_the_documented_layout() {
        let p = packet_path(
            Path::new("/home/.duduclaw"),
            "6f1c2a8e-1111-4222-8333-444455556666",
            2,
            Role::Planner,
            Role::Executor,
        )
        .expect("uuid task id is valid");
        assert_eq!(
            p,
            Path::new("/home/.duduclaw")
                .join("team_packets")
                .join("6f1c2a8e-1111-4222-8333-444455556666")
                .join("r2")
                .join("planner-to-executor.json")
        );
    }

    #[test]
    fn packet_path_round_zero_and_cap_are_accepted() {
        for round in [0, PACKET_ROUND_MAX] {
            let p = packet_path(
                Path::new("/h"),
                "task-1",
                round,
                Role::Executor,
                Role::Verifier,
            )
            .expect("in-range round");
            assert!(
                p.to_string_lossy().contains(&format!("r{round}")),
                "round {round} must appear verbatim"
            );
        }
    }

    #[test]
    fn packet_path_refuses_traversal_and_separators() {
        for bad in [
            "..",
            ".",
            "../../etc",
            "a/b",
            "a\\b",
            "",
            "   ",
            "task.1",
            "C:",
            "task\0id",
            "任務",
        ] {
            let err = packet_path(Path::new("/h"), bad, 1, Role::Planner, Role::Executor)
                .expect_err("must be refused");
            assert_eq!(err.code(), "invalid_path_component", "{bad:?}");
        }
    }

    #[test]
    fn packet_path_refuses_round_over_cap() {
        let err = packet_path(
            Path::new("/h"),
            "task-1",
            PACKET_ROUND_MAX + 1,
            Role::Planner,
            Role::Executor,
        )
        .expect_err("over cap");
        assert_eq!(err.code(), "invalid_path_component");
        assert!(err.to_string().contains("round"));
    }

    #[test]
    fn packet_path_never_escapes_the_home_directory() {
        let home = Path::new("/home/.duduclaw");
        let p = packet_path(home, "task-1", 3, Role::Verifier, Role::Executor).unwrap();
        assert!(p.starts_with(home));
        assert!(!p.components().any(|c| c.as_os_str() == ".."));
    }
}

/// Audience narrows a separately established task ACL. Empty constraints inherit
/// that ACL; they never grant access without it. Keys must be host derived.
pub fn audience_allows(task_acl_allowed:bool,audience:&[String],trusted_keys:&[String])->bool {
    task_acl_allowed && (audience.is_empty() || audience.iter().any(|key|trusted_keys.contains(key)))
}
#[cfg(test)]
mod workflow_audience_tests {
    use super::audience_allows;
    #[test]
    fn audience_requires_task_acl_and_exact_host_keys() {
        let keys=vec!["user:alice".into(),"role:manager".into(),"channel:dashboard".into()];
        assert!(!audience_allows(false,&[],&keys));
        assert!(audience_allows(true,&[],&keys));
        assert!(audience_allows(true,&["user:alice".into()],&keys));
        assert!(!audience_allows(true,&["alice".into()],&keys));
        assert!(!audience_allows(true,&["user:alic".into()],&keys));
        assert!(!audience_allows(false,&["role:manager".into()],&keys));
    }
}
