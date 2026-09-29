use super::*;

// ── Streaming progress types ───────────────────────────────

/// Progress events emitted during Claude CLI streaming.
///
/// Sent to the channel via callback so users see real-time progress
/// instead of silence during long-running agentic tasks.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// Periodic keepalive — no new stream-json events for `keepalive_interval`.
    Keepalive,
    /// Claude is using a tool (parsed from stream-json `tool_use` content block).
    ToolUse {
        tool: String,
        /// Optional file path or search pattern extracted from tool input.
        detail: Option<String>,
    },
    /// Claude updated its task list (parsed from a `TodoWrite` tool_use block).
    /// Carries the full list so channels can render/edit a progress board.
    TodoUpdate { todos: Vec<TodoItem> },
    /// A tool-step boundary (start/end) for the dashboard's agentic task tree
    /// (openhuman-parity project C-P1). Emitted per `tool_use` block (start) and
    /// matching `tool_result` (end), with a nesting `depth`. **Dashboard-only**:
    /// text channels (Telegram/Slack/…) ignore this variant — it renders as an
    /// empty string via [`ProgressEvent::to_display`] and each channel callback
    /// early-returns on it. Only the WebChat socket forwards it (as a `step`
    /// frame). See [`StepEvent`] / [`StepTracker`].
    Step(StepEvent),
    /// The model id the backend ACTUALLY answered with, parsed from the
    /// stream-json `assistant` event's `message.model` (which reflects any
    /// CLI-side substitution — account tier, alias resolution, fallback).
    /// **Dashboard-only** like [`ProgressEvent::Step`]: text channels ignore
    /// it; the WebChat socket records it and stamps the `assistant_done`
    /// frame's `model` field so the UI shows the real model, not the
    /// configured intent.
    ModelInfo { model: String },
}

/// Phase of a tool step in the agentic task tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPhase {
    /// A `tool_use` block was emitted — the tool started.
    Start,
    /// The matching `tool_result` arrived — the tool finished.
    End,
}

impl StepPhase {
    /// Stable wire token used in the WebChat `step` frame.
    pub fn as_str(self) -> &'static str {
        match self {
            StepPhase::Start => "start",
            StepPhase::End => "end",
        }
    }
}

/// One boundary of a tool invocation, forming the dashboard's collapsible
/// agentic task tree (openhuman-parity project C-P1).
///
/// A `Start` carries a CJK-safe args `summary`; an `End` carries `summary =
/// None`. `depth` is the nesting level — the number of still-open tool calls
/// at the moment this one started, so a `Task` sub-agent whose inner tools
/// resolve before it does surfaces its children at `depth ≥ 1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEvent {
    pub phase: StepPhase,
    pub tool: String,
    /// CJK-safe args summary (≤120 chars). `None` for `End` phase.
    pub summary: Option<String>,
    /// Nesting depth (outstanding tool calls when this step started).
    pub depth: usize,
    /// Wall-clock timestamp, unix epoch milliseconds.
    pub ts_ms: u64,
}

/// Max chars for a step's args summary (CJK-safe, per project convention 1).
pub(super) const STEP_SUMMARY_CHAR_CAP: usize = 120;

/// Current wall-clock time in unix epoch milliseconds (saturating on error).
pub(super) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Custom-skill usage counting (L5 §14) ────────────────────

/// TTL for the approved custom-skill slug cache — bounds the DB hit to once per
/// minute per home even under a fast tool_use stream.
pub(super) const CUSTOM_SKILL_SLUG_TTL: std::time::Duration = std::time::Duration::from_secs(60);

pub(super) struct SlugCacheEntry {
    loaded_at: Instant,
    slugs: Arc<HashSet<String>>,
}

pub(super) fn custom_skill_slug_cache()
-> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, SlugCacheEntry>> {
    static CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, SlugCacheEntry>>> =
        OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Approved custom-skill slugs for `home_dir`, cached [`CUSTOM_SKILL_SLUG_TTL`].
/// The registry is opened only on a cold/stale entry, and never while the cache
/// lock is held (the lock never spans an `.await`). Any open/read failure yields
/// an empty set — usage counting degrades silently, never blocks the reply.
pub(super) async fn approved_custom_skill_slugs(home_dir: &Path) -> Arc<HashSet<String>> {
    {
        let cache = custom_skill_slug_cache()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = cache.get(home_dir) {
            if entry.loaded_at.elapsed() < CUSTOM_SKILL_SLUG_TTL {
                return entry.slugs.clone();
            }
        }
    }
    let slugs: HashSet<String> = match crate::custom_skills::CustomSkillStore::open(home_dir) {
        Ok(store) => store
            .list_approved()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.slug)
            .collect(),
        Err(_) => HashSet::new(),
    };
    let arc = Arc::new(slugs);
    let mut cache = custom_skill_slug_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    cache.insert(
        home_dir.to_path_buf(),
        SlugCacheEntry {
            loaded_at: Instant::now(),
            slugs: arc.clone(),
        },
    );
    arc
}

/// Skill names invoked via the Claude CLI `Skill` tool in one stream-json
/// `assistant` event. The Skill tool carries its target under `input.skill` (its
/// documented parameter); `command`/`name` are accepted as resilient fallbacks
/// across CLI versions. Only `tool_use` blocks whose tool name is exactly
/// "Skill" are considered.
pub(super) fn extract_skill_tool_names(event: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if event.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return out;
    }
    let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array()) else {
        return out;
    };
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        if block.get("name").and_then(|n| n.as_str()) != Some("Skill") {
            continue;
        }
        let name = block
            .get("input")
            .and_then(|i| {
                i.get("skill")
                    .or_else(|| i.get("command"))
                    .or_else(|| i.get("name"))
            })
            .and_then(|s| s.as_str())
            .map(str::trim)
            .unwrap_or("");
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// Token-equality match of an invoked skill name against approved custom-skill
/// slugs. **Exact** string equality — never substring (project convention 2: a
/// substring test would let the slug "report" be counted for a "report-daily"
/// invocation and inflate saved-hours). CJK slugs match unchanged.
pub(super) fn matched_custom_slug<'a>(invoked: &str, approved: &'a HashSet<String>) -> Option<&'a str> {
    approved.get(invoked).map(String::as_str)
}

/// Build a CJK-safe (≤120 char) one-line summary of a `tool_use` block's args
/// for the dashboard step tree.
///
/// Prefers the most informative field (path / command / query / prompt …);
/// falls back to a compact comma-joined key list. Uses
/// [`duduclaw_core::truncate_chars`] — never raw byte slicing (project
/// convention 1: `&s[..n]` panics mid-char on CJK/emoji input).
pub(super) fn summarize_tool_input(block: &serde_json::Value) -> Option<String> {
    let input = block.get("input")?;
    for key in &[
        "file_path",
        "path",
        "command",
        "pattern",
        "query",
        "url",
        "prompt",
        "description",
    ] {
        if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
            let val = val.trim();
            if !val.is_empty() {
                return Some(duduclaw_core::truncate_chars(val, STEP_SUMMARY_CHAR_CAP));
            }
        }
    }
    // Fallback: compact list of argument keys (still informative for the tree).
    let obj = input.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let joined = obj
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Some(duduclaw_core::truncate_chars(
        &joined,
        STEP_SUMMARY_CHAR_CAP,
    ))
}

/// Stateful converter from parsed stream-json events to ordered [`StepEvent`]s
/// (openhuman-parity project C-P1).
///
/// Feed it every parsed stream-json event via [`StepTracker::ingest`]:
/// `assistant` messages carry `tool_use` blocks (a step **start**); `user`
/// messages carry `tool_result` blocks (a step **end**). It keeps a stack of
/// outstanding `(tool_use_id, tool_name)` pairs so nested / parallel calls get
/// a correct `depth`, and matches each `tool_result` to its `tool_use_id`
/// (falling back to the most recent open call when the id is absent).
///
/// Pure and deterministic apart from the wall-clock timestamp — unit-tested
/// against synthetic start / end / nested / non-tool events.
#[derive(Debug, Default)]
pub struct StepTracker {
    /// Outstanding (unresolved) tool calls, innermost last: (tool_use_id, tool).
    open: Vec<(String, String)>,
}

impl StepTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one parsed stream-json event, returning any step boundaries it
    /// produced (usually 0 or 1; more when a single assistant message batches
    /// several parallel `tool_use` blocks).
    pub fn ingest(&mut self, event: &serde_json::Value) -> Vec<StepEvent> {
        let ts_ms = now_unix_ms();
        let mut out = Vec::new();
        match event.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array())
                else {
                    return out;
                };
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                        continue;
                    }
                    let tool = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let id = block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let summary = summarize_tool_input(block);
                    // depth = outstanding calls *before* this one is pushed.
                    let depth = self.open.len();
                    out.push(StepEvent {
                        phase: StepPhase::Start,
                        tool: tool.clone(),
                        summary,
                        depth,
                        ts_ms,
                    });
                    self.open.push((id, tool));
                }
            }
            Some("user") => {
                let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array())
                else {
                    return out;
                };
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let id = block
                        .get("tool_use_id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default();
                    // Match the result to its open call by id; fall back to the
                    // most recent open call when the id is missing/unknown.
                    let popped = if !id.is_empty() {
                        self.open
                            .iter()
                            .rposition(|(oid, _)| oid == id)
                            .map(|pos| self.open.remove(pos))
                    } else {
                        self.open.pop()
                    }
                    .or_else(|| self.open.pop());
                    if let Some((_, tool)) = popped {
                        out.push(StepEvent {
                            phase: StepPhase::End,
                            tool,
                            summary: None,
                            // depth after removal = the level this step returns to.
                            depth: self.open.len(),
                            ts_ms,
                        });
                    }
                }
            }
            _ => {}
        }
        out
    }
}

/// One entry of the agent's live task list (mirrors the Claude CLI
/// `TodoWrite` input shape: `content` / `status` / `activeForm`).
#[derive(Debug, Clone, PartialEq)]
pub struct TodoItem {
    pub content: String,
    /// "pending" | "in_progress" | "completed" (unknown values render as pending).
    pub status: String,
    /// Present-tense label shown while the item is in progress.
    pub active_form: Option<String>,
}

/// Parse the `todos` array out of a `TodoWrite` tool_use block's `input`.
/// Returns `None` when the shape is unrecognised (fail-soft: caller falls
/// back to a generic ToolUse event).
pub(crate) fn parse_todo_write_input(input: &serde_json::Value) -> Option<Vec<TodoItem>> {
    let items = input.get("todos")?.as_array()?;
    let todos: Vec<TodoItem> = items
        .iter()
        .filter_map(|it| {
            let content = it.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            Some(TodoItem {
                content: content.to_string(),
                status: it
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("pending")
                    .to_string(),
                active_form: it
                    .get("activeForm")
                    .and_then(|s| s.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            })
        })
        .collect();
    if todos.is_empty() { None } else { Some(todos) }
}

impl ProgressEvent {
    /// Format as a user-facing progress message.
    pub fn to_display(&self) -> String {
        match self {
            Self::Keepalive => "⏳ 仍在處理中…".to_string(),
            Self::ToolUse { tool, detail } => {
                let action = match tool.as_str() {
                    "Read" | "read" => "正在讀取",
                    "Write" | "write" => "正在撰寫",
                    "Edit" | "edit" => "正在編輯",
                    "Grep" | "grep" | "search" => "正在搜尋",
                    "Glob" | "glob" => "正在搜尋檔案",
                    "Bash" | "bash" => "正在執行指令",
                    _ => "正在使用工具",
                };
                match detail {
                    Some(d) => format!("⏳ {action} {d}…"),
                    None => format!("⏳ {action}…"),
                }
            }
            Self::TodoUpdate { todos } => render_todo_list(todos),
            // Dashboard-only structured step — never rendered as channel text.
            // Text channels early-return on this variant; WebChat forwards it
            // as a `step` frame instead.
            Self::Step(_) => String::new(),
            // Dashboard-only metadata — same contract as `Step`.
            Self::ModelInfo { .. } => String::new(),
        }
    }
}

/// Max todo items rendered in a channel progress message (rest summarised).
pub(super) const TODO_RENDER_CAP: usize = 12;
/// Max chars per rendered todo line (CJK-safe truncation).
pub(super) const TODO_ITEM_CHAR_CAP: usize = 60;

/// Render a todo list as a compact, channel-friendly progress board.
///
/// Plain-text/emoji only — every channel renders this correctly without
/// platform-specific markup (bold etc. is added by the per-channel
/// formatting layer downstream where supported).
pub(crate) fn render_todo_list(todos: &[TodoItem]) -> String {
    let done = todos.iter().filter(|t| t.status == "completed").count();
    let total = todos.len();
    let mut out = format!("📋 任務進度({done}/{total} 完成)");
    for item in todos.iter().take(TODO_RENDER_CAP) {
        let (icon, label) = match item.status.as_str() {
            "completed" => ("✅", item.content.as_str()),
            "in_progress" => (
                "🔄",
                item.active_form.as_deref().unwrap_or(item.content.as_str()),
            ),
            _ => ("⬜", item.content.as_str()),
        };
        let label = crate::channel_format::truncate_chars(label, TODO_ITEM_CHAR_CAP);
        out.push('\n');
        out.push_str(icon);
        out.push(' ');
        out.push_str(&label);
    }
    if total > TODO_RENDER_CAP {
        out.push_str(&format!("\n… 及其他 {} 項", total - TODO_RENDER_CAP));
    }
    out
}

/// Callback type for sending progress events to the channel.
///
/// The callback is `Send + Sync` so it can be invoked from the streaming loop.
/// Implementations should be lightweight (just enqueue a message send).
pub type ProgressCallback = Box<dyn Fn(ProgressEvent) + Send + Sync>;

