//! The closed-enumeration ActionGuard findings and the deterministic
//! analyzer that produces them. Moved verbatim out of `approval.rs`.

use super::*;

/// Closed-enumeration findings the deterministic H21 analyzer can produce for
/// one tool call. Every judge prompt is built exclusively from these — see
/// the module section header above for the threat this closes.
///
/// Three dimensions (`ToolCategory*`, `TargetScope*`, `Magnitude*`) are
/// mutually exclusive within themselves — [`analyze_action_guard_findings`]
/// emits at most one variant per dimension, and omits the "nothing notable"
/// member of each (`ToolCategoryUnknown` / `TargetScopeNone` /
/// `MagnitudeSingleTarget`) from its output so an uninteresting call can
/// legitimately produce an empty finding set. The remaining two
/// (`ProtectedPathHit`, `DestructiveSemanticsDetected`) are presence-only
/// flags, emitted only when true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionGuardFinding {
    // ── Tool category ──────────────────────────────────────────────────
    /// Tool name matches filesystem-delete vocabulary (delete/remove/purge/…).
    ToolCategoryFilesystemDelete,
    /// Tool name matches filesystem-write vocabulary (write/save/create/…).
    ToolCategoryFilesystemWrite,
    /// Tool name matches local process-execution vocabulary (bash/shell/exec/…).
    ToolCategoryProcessExec,
    /// Tool name matches outbound-email vocabulary (email/mail).
    ToolCategoryEmailSend,
    /// Tool name matches channel-messaging vocabulary (send/message/notify/…).
    ToolCategoryMessagingSend,
    /// Tool name matches outbound-HTTP vocabulary (http/webhook/fetch/…).
    ToolCategoryNetworkEgress,
    /// Tool name matches browser/computer-use automation vocabulary.
    ToolCategoryBrowserOrDesktopAutomation,
    /// Tool name matches OS-native action vocabulary (`os_open` and siblings).
    ToolCategoryOsNativeAction,
    /// Tool name matches wiki/memory persistent-store vocabulary.
    ToolCategoryKnowledgeStore,
    /// Tool name matches ERP / financial / business-record vocabulary.
    ToolCategoryFinancialOrBusiness,
    /// Tool name matches skill/capability install vocabulary.
    ToolCategorySkillOrCapabilityInstall,
    /// None of the known category keyword groups matched — analyzer has no
    /// opinion on tool category. Never emitted into a finding set.
    ToolCategoryUnknown,

    // ── Target scope (the most exposed scope across all args wins) ──────
    /// An argument value resolves under the agent's own workspace directory.
    TargetScopeWorkspaceInternal,
    /// An argument value resolves under the OS user's home directory, outside
    /// the agent's workspace.
    TargetScopeHomeDir,
    /// An argument value is an absolute filesystem path outside the home
    /// directory (e.g. `/etc`, `/usr`, `/System`).
    TargetScopeSystemPath,
    /// An argument value looks like an outbound network URL
    /// (`http`/`https`/`ws`/`wss`/`ftp`).
    TargetScopeExternalNetwork,
    /// No path- or URL-shaped argument value was found. Never emitted into a
    /// finding set.
    TargetScopeNone,

    // ── Magnitude ─────────────────────────────────────────────────────
    /// Arguments indicate a single, specific target — the assumed default.
    /// Never emitted into a finding set (absence of `BatchOrBulk` IS this).
    MagnitudeSingleTarget,
    /// Arguments indicate a bulk / recursive / wildcard / multi-item target
    /// (array with >1 item, glob wildcard, recursive/force flag, chained
    /// shell commands).
    MagnitudeBatchOrBulk,

    // ── Presence-only findings ────────────────────────────────────────
    /// An argument value matches a curated list of sensitive-path substrings
    /// (SSH keys, credential files, DuDuClaw's own config, `/etc/passwd`, …).
    ProtectedPathHit,
    /// An argument value or the tool name matches destructive-verb
    /// vocabulary (delete/remove/drop/truncate/overwrite/format/wipe/purge/
    /// force, or a recursive-force shell flag).
    DestructiveSemanticsDetected,
}

impl ActionGuardFinding {
    /// Fixed, closed-enumeration token. Never derived from tool-call content.
    pub fn token(&self) -> &'static str {
        self.token_and_description().0
    }

    /// Fixed, closed-enumeration zh-TW description handed to the judge
    /// alongside [`Self::token`]. Never derived from tool-call content.
    pub fn description(&self) -> &'static str {
        self.token_and_description().1
    }

    /// Single source of truth for token+description — an exhaustive `match`
    /// (no wildcard arm), so adding a new [`ActionGuardFinding`] variant
    /// without giving it a token/description here is a **compile error**,
    /// not a silent gap. [`ALL_ACTION_GUARD_FINDINGS`] below is the
    /// enumerable "findings 全集" and carries its own compile-enforced
    /// completeness check (`all_action_guard_findings_is_exhaustive` test).
    fn token_and_description(&self) -> (&'static str, &'static str) {
        use ActionGuardFinding::*;
        match self {
            ToolCategoryFilesystemDelete => (
                "tool_category:filesystem_delete",
                "工具名稱屬於「檔案刪除／移除」類別",
            ),
            ToolCategoryFilesystemWrite => (
                "tool_category:filesystem_write",
                "工具名稱屬於「檔案寫入／建立」類別",
            ),
            ToolCategoryProcessExec => (
                "tool_category:process_exec",
                "工具名稱屬於「本機程序執行（shell／指令）」類別",
            ),
            ToolCategoryEmailSend => (
                "tool_category:email_send",
                "工具名稱屬於「寄送電子郵件」類別",
            ),
            ToolCategoryMessagingSend => (
                "tool_category:messaging_send",
                "工具名稱屬於「發送通道訊息／通知」類別",
            ),
            ToolCategoryNetworkEgress => (
                "tool_category:network_egress",
                "工具名稱屬於「對外網路請求（HTTP／webhook）」類別",
            ),
            ToolCategoryBrowserOrDesktopAutomation => (
                "tool_category:browser_or_desktop_automation",
                "工具名稱屬於「瀏覽器／桌面自動化操作」類別",
            ),
            ToolCategoryOsNativeAction => (
                "tool_category:os_native_action",
                "工具名稱屬於「作業系統原生動作」類別",
            ),
            ToolCategoryKnowledgeStore => (
                "tool_category:knowledge_store",
                "工具名稱屬於「wiki／記憶系統」相關類別",
            ),
            ToolCategoryFinancialOrBusiness => (
                "tool_category:financial_or_business",
                "工具名稱屬於「財務／ERP／商業紀錄」類別",
            ),
            ToolCategorySkillOrCapabilityInstall => (
                "tool_category:skill_or_capability_install",
                "工具名稱屬於「技能／能力安裝」類別",
            ),
            ToolCategoryUnknown => (
                "tool_category:unknown",
                "分析器無法將工具名稱歸類到已知類別",
            ),
            TargetScopeWorkspaceInternal => (
                "target_scope:workspace_internal",
                "參數中偵測到的路徑落在此 agent 自己的工作區內",
            ),
            TargetScopeHomeDir => (
                "target_scope:home_dir",
                "參數中偵測到的路徑落在使用者家目錄內、但在此 agent 工作區之外",
            ),
            TargetScopeSystemPath => (
                "target_scope:system_path",
                "參數中偵測到的路徑落在家目錄之外的系統路徑（如 /etc、/usr）",
            ),
            TargetScopeExternalNetwork => (
                "target_scope:external_network",
                "參數中偵測到對外網路目標（http／https／ws／wss URL）",
            ),
            TargetScopeNone => (
                "target_scope:none",
                "參數中未偵測到任何路徑或網址形狀的目標",
            ),
            MagnitudeSingleTarget => (
                "magnitude:single_target",
                "未偵測到批量／遞迴訊號，視為單一目標",
            ),
            MagnitudeBatchOrBulk => (
                "magnitude:batch_or_bulk",
                "偵測到批量／遞迴／萬用字元／多目標訊號",
            ),
            ProtectedPathHit => (
                "protected_path_hit",
                "參數中偵測到受保護路徑清單中的字樣（如金鑰、憑證、設定檔）",
            ),
            DestructiveSemanticsDetected => (
                "destructive_semantics_detected",
                "工具名稱或參數中偵測到刪除／覆寫／強制等破壞性語意詞彙",
            ),
        }
    }
}

/// The full closed enumeration of [`ActionGuardFinding`] — the "findings 全
/// 集" constant table the judge prompt and every finding a call can ever
/// produce are drawn from. Paired with [`ActionGuardFinding::token`] /
/// [`ActionGuardFinding::description`] (via [`ActionGuardFinding::token_and_description`])
/// this IS the constant table: variant → fixed token → fixed description.
/// Kept in sync with the enum by `all_action_guard_findings_is_exhaustive`
/// (a compiler-enforced exhaustive `match` with no wildcard arm — adding a
/// variant without listing it here fails the build).
pub const ALL_ACTION_GUARD_FINDINGS: &[ActionGuardFinding] = &[
    ActionGuardFinding::ToolCategoryFilesystemDelete,
    ActionGuardFinding::ToolCategoryFilesystemWrite,
    ActionGuardFinding::ToolCategoryProcessExec,
    ActionGuardFinding::ToolCategoryEmailSend,
    ActionGuardFinding::ToolCategoryMessagingSend,
    ActionGuardFinding::ToolCategoryNetworkEgress,
    ActionGuardFinding::ToolCategoryBrowserOrDesktopAutomation,
    ActionGuardFinding::ToolCategoryOsNativeAction,
    ActionGuardFinding::ToolCategoryKnowledgeStore,
    ActionGuardFinding::ToolCategoryFinancialOrBusiness,
    ActionGuardFinding::ToolCategorySkillOrCapabilityInstall,
    ActionGuardFinding::ToolCategoryUnknown,
    ActionGuardFinding::TargetScopeWorkspaceInternal,
    ActionGuardFinding::TargetScopeHomeDir,
    ActionGuardFinding::TargetScopeSystemPath,
    ActionGuardFinding::TargetScopeExternalNetwork,
    ActionGuardFinding::TargetScopeNone,
    ActionGuardFinding::MagnitudeSingleTarget,
    ActionGuardFinding::MagnitudeBatchOrBulk,
    ActionGuardFinding::ProtectedPathHit,
    ActionGuardFinding::DestructiveSemanticsDetected,
];

/// Max argument string values scanned per call (bounds analyzer cost against
/// a pathologically large payload; extra values are simply not inspected —
/// never a panic, never unbounded work).
const ACTION_GUARD_SCAN_MAX_STRINGS: usize = 64;
/// Max recursion depth walking `arguments` (arrays/objects only — bounds cost
/// against a deeply nested payload).
const ACTION_GUARD_SCAN_MAX_DEPTH: usize = 4;

/// Ordered `(category, ascii keyword list)` table for [`classify_tool_category`].
/// Order matters: earlier entries win on a tie (a tool literally named
/// `delete_draft` classifies as `filesystem_delete`, not `filesystem_write`).
/// Keywords are matched as whole ASCII "words" within the tool name via
/// [`duduclaw_core::word_contains_ci`] (an MCP tool name is `snake_case`, so
/// `_`/`-` act as natural word boundaries — `os_open` matches the bare
/// keyword `os`, `cost_summary` does not).
const TOOL_CATEGORY_KEYWORDS: &[(ActionGuardFinding, &[&str])] = &[
    (
        ActionGuardFinding::ToolCategoryFilesystemDelete,
        &[
            "delete", "remove", "purge", "wipe", "truncate", "unlink", "rm",
        ],
    ),
    (
        ActionGuardFinding::ToolCategoryProcessExec,
        &["bash", "shell", "exec", "command", "process"],
    ),
    (
        ActionGuardFinding::ToolCategoryEmailSend,
        &["email", "mail"],
    ),
    (
        ActionGuardFinding::ToolCategoryMessagingSend,
        &["send", "message", "notify", "broadcast", "publish"],
    ),
    (
        ActionGuardFinding::ToolCategoryNetworkEgress,
        &["http", "webhook", "fetch", "request", "url"],
    ),
    (
        ActionGuardFinding::ToolCategoryBrowserOrDesktopAutomation,
        &["browser", "computer", "screen", "click", "desktop"],
    ),
    (ActionGuardFinding::ToolCategoryOsNativeAction, &["os"]),
    (
        ActionGuardFinding::ToolCategoryKnowledgeStore,
        &["wiki", "memory"],
    ),
    (
        ActionGuardFinding::ToolCategoryFinancialOrBusiness,
        &[
            "odoo", "invoice", "payment", "charge", "order", "sale", "account", "finance",
        ],
    ),
    (
        ActionGuardFinding::ToolCategorySkillOrCapabilityInstall,
        &["install", "hub"],
    ),
    (
        ActionGuardFinding::ToolCategoryFilesystemWrite,
        &[
            "write", "save", "create", "edit", "upload", "append", "draft", "drawing",
        ],
    ),
];

/// Curated substrings of well-known sensitive paths. Plain (non-word-bounded)
/// substring matching is deliberate here — unlike the routing/security
/// decisions [`duduclaw_core::origin_host_matches`] / `word_contains_ci`
/// exist for, this only produces an *advisory* finding fed to a judge that
/// still makes the actual call; a false positive costs nothing but an extra
/// line of judge context, so favoring recall over precision is the right
/// trade-off.
const PROTECTED_PATH_SUBSTRINGS: &[&str] = &[
    ".ssh",
    "id_rsa",
    "id_ed25519",
    ".aws",
    ".gnupg",
    ".env",
    "credentials",
    ".git/config",
    "known_hosts",
    ".duduclaw",
    "agent.toml",
    "config.toml",
    "/etc/passwd",
    "/etc/shadow",
    ".netrc",
    "private_key",
    "secret",
];

/// Destructive-verb vocabulary, ASCII word-bounded via `word_contains_ci`.
const DESTRUCTIVE_KEYWORDS: &[&str] = &[
    "delete",
    "remove",
    "drop",
    "truncate",
    "overwrite",
    "format",
    "wipe",
    "purge",
    "force",
];

/// Batch/bulk-signal keyword vocabulary, ASCII word-bounded.
const BATCH_KEYWORDS: &[&str] = &["recursive", "all"];

/// URL scheme prefixes recognized as an external-network target.
const NETWORK_URL_SCHEMES: &[&str] = &["http://", "https://", "ws://", "wss://", "ftp://"];

/// Deterministic, zero-LLM analyzer: inspects a tool call's real name and
/// argument values and produces the closed-enumeration [`ActionGuardFinding`]
/// set that becomes the maybe-irreversible judge's entire evidentiary input
/// (see the H21 section header above). `agent_dir` anchors the
/// workspace-internal boundary; the OS user's home directory (via
/// [`dirs::home_dir`]) anchors the home-dir boundary. Never panics — a
/// missing/malformed `payload["arguments"]` degrades to "no findings for
/// that dimension", never an error.
pub fn analyze_action_guard_findings(
    tool_name: &str,
    payload: &Value,
    agent_dir: &Path,
) -> Vec<ActionGuardFinding> {
    let strings = collect_argument_strings(payload);

    let mut out = Vec::new();
    let category = classify_tool_category(tool_name);
    if category != ActionGuardFinding::ToolCategoryUnknown {
        out.push(category);
    }
    let scope = classify_target_scope(&strings, agent_dir);
    if scope != ActionGuardFinding::TargetScopeNone {
        out.push(scope);
    }
    if classify_magnitude(payload, &strings) == ActionGuardFinding::MagnitudeBatchOrBulk {
        out.push(ActionGuardFinding::MagnitudeBatchOrBulk);
    }
    if has_protected_path_hit(&strings) {
        out.push(ActionGuardFinding::ProtectedPathHit);
    }
    if has_destructive_semantics(tool_name, &strings) {
        out.push(ActionGuardFinding::DestructiveSemanticsDetected);
    }
    out
}

/// Collect every string leaf under `payload["arguments"]` (falling back to
/// `payload` itself when there is no `arguments` key, so callers that pass a
/// bare arguments object directly still work), bounded by
/// [`ACTION_GUARD_SCAN_MAX_STRINGS`] / [`ACTION_GUARD_SCAN_MAX_DEPTH`]. These
/// strings are read ONLY to decide which closed-enum findings apply — never
/// returned to any caller outside this module, never placed in a prompt.
fn collect_argument_strings(payload: &Value) -> Vec<String> {
    let root = payload.get("arguments").unwrap_or(payload);
    let mut out = Vec::new();
    collect_strings_rec(root, 0, &mut out);
    out
}

fn collect_strings_rec(value: &Value, depth: usize, out: &mut Vec<String>) {
    if out.len() >= ACTION_GUARD_SCAN_MAX_STRINGS || depth > ACTION_GUARD_SCAN_MAX_DEPTH {
        return;
    }
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(arr) => {
            for v in arr {
                if out.len() >= ACTION_GUARD_SCAN_MAX_STRINGS {
                    break;
                }
                collect_strings_rec(v, depth + 1, out);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                if out.len() >= ACTION_GUARD_SCAN_MAX_STRINGS {
                    break;
                }
                collect_strings_rec(v, depth + 1, out);
            }
        }
        _ => {}
    }
}

fn classify_tool_category(tool_name: &str) -> ActionGuardFinding {
    for (finding, keywords) in TOOL_CATEGORY_KEYWORDS {
        if keywords
            .iter()
            .any(|k| duduclaw_core::word_contains_ci(tool_name, k))
        {
            return *finding;
        }
    }
    ActionGuardFinding::ToolCategoryUnknown
}

fn looks_like_network_url(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    NETWORK_URL_SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// Best-effort "does this look like a filesystem path" heuristic — advisory
/// only (see [`PROTECTED_PATH_SUBSTRINGS`] doc comment for the precision/
/// recall trade-off rationale). False positives (e.g. a MIME type
/// `"image/png"`) just add a harmless extra `TargetScope*` finding.
fn looks_like_filesystem_path(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return false;
    }
    if t.starts_with('/') || t.starts_with('~') || t.starts_with("./") || t.starts_with("../") {
        return true;
    }
    // Windows drive-letter path, e.g. `C:\Users\...` or `C:/Users/...`.
    let bytes = t.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    t.contains('/')
}

/// Expand a leading `~` against the OS home directory. Falls back to the
/// original string unchanged when the home directory is unresolvable or the
/// string has no `~` prefix.
fn expand_tilde(s: &str, os_home: Option<&Path>) -> String {
    if let Some(rest) = s.strip_prefix('~') {
        if let Some(home) = os_home {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    s.to_string()
}

/// Severity rank used to pick the single "most exposed" `TargetScope*`
/// finding across every string argument (higher = worse).
fn target_scope_rank(f: &ActionGuardFinding) -> u8 {
    use ActionGuardFinding::*;
    match f {
        TargetScopeExternalNetwork => 4,
        TargetScopeSystemPath => 3,
        TargetScopeHomeDir => 2,
        TargetScopeWorkspaceInternal => 1,
        _ => 0,
    }
}

fn classify_target_scope(strings: &[String], agent_dir: &Path) -> ActionGuardFinding {
    let agent_dir_norm = duduclaw_core::agent_guard::lexical_normalize(agent_dir);
    let os_home_norm = dirs::home_dir().map(|h| duduclaw_core::agent_guard::lexical_normalize(&h));

    let mut worst: Option<ActionGuardFinding> = None;
    for s in strings {
        if let Some(candidate) =
            classify_one_string_scope(s, &agent_dir_norm, os_home_norm.as_deref())
        {
            let is_worse = worst
                .map(|w| target_scope_rank(&candidate) > target_scope_rank(&w))
                .unwrap_or(true);
            if is_worse {
                worst = Some(candidate);
            }
        }
    }
    worst.unwrap_or(ActionGuardFinding::TargetScopeNone)
}

fn classify_one_string_scope(
    s: &str,
    agent_dir_norm: &Path,
    os_home_norm: Option<&Path>,
) -> Option<ActionGuardFinding> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if looks_like_network_url(trimmed) {
        return Some(ActionGuardFinding::TargetScopeExternalNetwork);
    }
    if !looks_like_filesystem_path(trimmed) {
        return None;
    }
    let expanded = expand_tilde(trimmed, os_home_norm);
    let path_norm = duduclaw_core::agent_guard::lexical_normalize(Path::new(&expanded));
    if path_norm.starts_with(agent_dir_norm) {
        return Some(ActionGuardFinding::TargetScopeWorkspaceInternal);
    }
    if let Some(home) = os_home_norm {
        if path_norm.starts_with(home) {
            return Some(ActionGuardFinding::TargetScopeHomeDir);
        }
    }
    if path_norm.is_absolute() {
        return Some(ActionGuardFinding::TargetScopeSystemPath);
    }
    // Relative path outside the checks above: treat as workspace-scoped,
    // matching the sandbox convention that a tool's cwd is the agent's own
    // workspace directory (`SandboxLevel::WorkspaceWrite`).
    Some(ActionGuardFinding::TargetScopeWorkspaceInternal)
}

fn classify_magnitude(payload: &Value, strings: &[String]) -> ActionGuardFinding {
    if let Some(args) = payload.get("arguments") {
        if value_has_multi_item_array(args) {
            return ActionGuardFinding::MagnitudeBatchOrBulk;
        }
    }
    for s in strings {
        if s.contains('*')
            || s.contains(';')
            || s.contains("&&")
            || s.contains('|')
            || s.contains("-rf")
            || s.contains("--recursive")
        {
            return ActionGuardFinding::MagnitudeBatchOrBulk;
        }
        if BATCH_KEYWORDS
            .iter()
            .any(|k| duduclaw_core::word_contains_ci(s, k))
        {
            return ActionGuardFinding::MagnitudeBatchOrBulk;
        }
    }
    ActionGuardFinding::MagnitudeSingleTarget
}

fn value_has_multi_item_array(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.len() > 1,
        Value::Object(map) => map.values().any(value_has_multi_item_array),
        _ => false,
    }
}

fn has_protected_path_hit(strings: &[String]) -> bool {
    strings.iter().any(|s| {
        let lower = s.to_ascii_lowercase();
        PROTECTED_PATH_SUBSTRINGS.iter().any(|p| lower.contains(p))
    })
}

fn has_destructive_semantics(tool_name: &str, strings: &[String]) -> bool {
    if DESTRUCTIVE_KEYWORDS
        .iter()
        .any(|k| duduclaw_core::word_contains_ci(tool_name, k))
    {
        return true;
    }
    strings.iter().any(|s| {
        DESTRUCTIVE_KEYWORDS
            .iter()
            .any(|k| duduclaw_core::word_contains_ci(s, k))
            || s.contains("-rf")
            || s.contains("--recursive")
    })
}

// ── D1: simulation narrative (WebDreamer arXiv:2411.06559) ──────
//
// The ActionGuard maybe-irreversible judge (`action_guard_judge` in
// `duduclaw-cli::mcp`) used to return a bare Safe/Risky verdict — a human
// escalation carried no information about *why*. D1 upgrades the judge's
// output to a structured simulation of "what will the world look like after
// this call runs" (2-4 sentences) plus explicit risk points, alongside the
// unchanged fail-closed verdict. The narrative rides on [`ApprovalRecord`]
// (`simulation` field) all the way to the channel push (D2,
// `approval_notify::approval_body` / `goal_notify::needs_human_body`).

/// Max chars kept for [`SimulationNarrative::world_state_change`]. Applied on
/// every construction path ([`SimulationNarrative::from_json`]) — both the
/// raw ActionGuard judge reply and a DB round-trip — so an oversized LLM
/// reply can never bloat `approvals.db` or blow a channel message's length
/// cap. CJK-safe via `truncate_chars`.
pub(super) const SIMULATION_NARRATIVE_MAX_CHARS: usize = 400;
/// Max chars kept per risk-point bullet.
pub(super) const SIMULATION_RISK_POINT_MAX_CHARS: usize = 100;
/// Max risk-point bullets kept.
pub(super) const SIMULATION_MAX_RISK_POINTS: usize = 3;
