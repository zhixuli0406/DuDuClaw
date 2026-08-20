//! In-channel chat commands (e.g. `/status`, `/new`, `/usage`).
//!
//! These are intercepted before the message reaches the AI pipeline,
//! so they respond instantly with zero LLM cost.
//!
//! Safety words (`!STOP`, `!RESUME`, etc.) are also handled here as
//! a special class of commands that interact with the failsafe system.

use crate::channel_reply::ReplyContext;
use duduclaw_security::safety_word::{SafetyWordAction, SafetyWordScope};
use tracing::warn;

/// Parsed chat command from user input.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatCommand {
    /// `/status` — show agent name, status, token usage, last active time
    Status,
    /// `/new` — reset current session (clear history)
    New,
    /// `/usage` — show token usage, estimated cost, cache hit rate
    Usage,
    /// `/help` — list all available commands
    Help,
    /// `/compact` — force session compression
    Compact,
    /// `/model [name]` — show or switch current model
    Model(Option<String>),
    /// `/pair <code>` — verify a pairing code for access control
    Pair(String),
    /// `/voice` — toggle voice reply mode (next response as TTS audio)
    Voice,
    /// `/screenshot` — capture and send current screen
    Screenshot,
    /// `/computer on` — temporarily enable computer use for this session
    ComputerOn,
    /// `/computer off` — disable computer use
    ComputerOff,
    /// `/pause` — pause the active computer use session
    ComputerPause,
    /// `/resume` — resume a paused computer use session
    ComputerResume,
    /// `/stop` — stop the active computer use session
    ComputerStop,
    /// `/replay [n]` — show the last N screenshots from audit log
    Replay(u32),
    /// `/handoff <channel>` — move the current conversation to another channel (G4).
    /// `None` = missing argument → usage message.
    Handoff(Option<String>),
    /// `/undo [n]` — tombstone the last N user+assistant turn pairs (G4, default 1)
    Undo(u32),
    /// `/rollback` — undo back to the last checkpoint watermark (G4)
    Rollback,
    /// `!STOP` / `!停止` — stop the current agent/scope via failsafe
    SafetyStop,
    /// `!STOP ALL` / `!全部停止` — emergency stop all agents
    SafetyStopAll,
    /// `!RESUME` / `!恢復` — resume from failsafe halt
    SafetyResume,
    /// `!STATUS` / `!狀態` — query failsafe status
    SafetyStatus,
    /// `/goal ...` — autonomous goal loop entry point (P5).
    Goal(GoalCommand),
    /// `/rules` — 經驗法則 (W3-2). `all=false, off=None` = the top 3
    /// currently-in-effect rules; `all=true` (`/rules all`) = the full list.
    /// `off=Some(n)` (`/rules off <n>`, W3-5) = disable the rule at position
    /// `n` in the same numbering `/rules all` shows — admin/manager-gated,
    /// see [`handle_rules_off`] for the authorization predicate. Zero LLM
    /// cost either way.
    Rules { all: bool, off: Option<u32> },
}

/// The three shapes a `/goal` command can take.
#[derive(Debug, Clone, PartialEq)]
pub enum GoalCommand {
    /// `/goal` with no args → print usage.
    Usage,
    /// `/goal status` → list this agent's in-progress goal tasks.
    Status,
    /// `/goal <description>` (optionally `<description> || <acceptance criteria>
    /// || outcome:<spec>`). When no `||` is given the description doubles as the
    /// acceptance criteria. `outcome` is the raw `outcome:` payload (WP2.4) — the
    /// text after `outcome:`, validated into an `OutcomeSpec` at creation time;
    /// `None` when the third segment is absent.
    ///
    /// Goal contract v2 (design §6 G1): any `||` segment may also be
    /// `時限:<N>[h|小時|d|天]` / `duration:<N>…` (per-goal deadline) or
    /// `邊界:<text>` / `risk:<text>` (per-goal risk boundary; absent ⇒ the
    /// baseline boundary is applied at injection time, same as the dashboard
    /// form). Segments are position-independent, mirroring `outcome:`.
    ///
    /// P1 (DESIGN-goal-intent-router-2026-08.md §4): a `||`-delimited segment
    /// that is EXACTLY `想一想` or `plan` (no value, position-independent —
    /// same parsing shape as `時限:`/`邊界:`) sets `plan_first = true`,
    /// bringing the dashboard's I-1c "想一想" execution mode to the chat
    /// `/goal` command — see `handle_goal_create`.
    Create {
        description: String,
        acceptance_criteria: Option<String>,
        outcome: Option<String>,
        duration_hours: Option<f64>,
        risk_boundary: Option<String>,
        plan_first: bool,
    },
}

/// Parse the argument tail of a `/goal` command into a [`GoalCommand`].
///
/// - empty / whitespace → [`GoalCommand::Usage`]
/// - `status` (case-insensitive) → [`GoalCommand::Status`]
/// - `<desc> || <criteria>` → split on `||`; an empty right side is treated as
///   "no explicit criteria"
/// - `<desc> || <criteria> || outcome:<spec>` (or `<desc> || outcome:<spec>`) →
///   WP2.4 structured outcome. The `outcome:` segment is captured raw (the whole
///   remainder from `outcome:` onward, so any `||` inside a JSON schema survives)
///   and validated later in [`handle_goal_create`].
/// - anything else → the whole tail is the goal description
fn parse_goal_args(args: Option<&str>) -> GoalCommand {
    let raw = args.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return GoalCommand::Usage;
    }
    if raw.eq_ignore_ascii_case("status") {
        return GoalCommand::Status;
    }
    // Split off an optional trailing `|| outcome:<spec>` section first (WP2.4).
    let (head, outcome) = split_outcome_section(raw);
    // Goal contract v2: pull prefixed `時限:`/`duration:` and `邊界:`/`risk:`
    // segments out wherever they appear; whatever remains keeps the original
    // `<desc> [|| <criteria>]` semantics byte-identically.
    let mut duration_hours: Option<f64> = None;
    let mut risk_boundary: Option<String> = None;
    // P1: a standalone (no-value) `||` segment that is exactly "想一想" or
    // "plan" (position-independent, same shape as the prefixed segments
    // above) opts into I-1c plan-first mode. Unlike `時限:`/`邊界:` this has
    // no `key:value` prefix — it IS the whole segment — so it is matched by
    // exact equality, not `strip_any_prefix`.
    let mut plan_first = false;
    let mut plain: Vec<&str> = Vec::new();
    for seg in head.split("||") {
        let seg = seg.trim();
        if let Some(v) = strip_any_prefix(seg, &["時限:", "時限：", "duration:"]) {
            duration_hours = parse_duration_hours(v);
            if duration_hours.is_none() {
                // Fail closed like a malformed outcome spec: a mistyped
                // deadline must not silently become part of the description.
                return GoalCommand::Usage;
            }
        } else if let Some(v) = strip_any_prefix(seg, &["邊界:", "邊界：", "risk:"]) {
            let v = v.trim();
            if !v.is_empty() {
                risk_boundary = Some(v.to_string());
            }
        } else if seg == "想一想" || seg.eq_ignore_ascii_case("plan") {
            plan_first = true;
        } else if !seg.is_empty() {
            plain.push(seg);
        }
    }
    let Some((desc, rest)) = plain.split_first() else {
        return GoalCommand::Usage;
    };
    GoalCommand::Create {
        description: (*desc).to_string(),
        acceptance_criteria: rest.first().map(|s| (*s).to_string()),
        outcome,
        duration_hours,
        risk_boundary,
        plan_first,
    }
}

/// Case-insensitive (ASCII) prefix strip across several accepted spellings.
/// Uses `get(..)` so a prefix length landing mid-char in `seg` is a clean
/// no-match, never a byte-boundary panic (coding convention 1).
fn strip_any_prefix<'a>(seg: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for p in prefixes {
        if let Some(head) = seg.get(..p.len()) {
            if head.eq_ignore_ascii_case(p) {
                return Some(&seg[p.len()..]);
            }
        }
    }
    None
}

/// Parse `時限:`/`duration:` payloads: `36`, `36h`, `36小時`, `3d`, `3天`
/// (days ×24). Range-checked 1–720 hours like the dashboard form; anything
/// else ⇒ `None` (caller fails closed to usage).
fn parse_duration_hours(v: &str) -> Option<f64> {
    let v = v.trim();
    let (num, mult) = if let Some(n) = v.strip_suffix("小時").or_else(|| v.strip_suffix('h')).or_else(|| v.strip_suffix('H')) {
        (n, 1.0)
    } else if let Some(n) = v.strip_suffix('天').or_else(|| v.strip_suffix('d')).or_else(|| v.strip_suffix('D')) {
        (n, 24.0)
    } else {
        (v, 1.0)
    };
    let hours: f64 = num.trim().parse().ok()?;
    let hours = hours * mult;
    (hours.is_finite() && (1.0..=720.0).contains(&hours)).then_some(hours)
}

/// Split a `/goal` tail into `(head, outcome_spec)` where `head` is everything
/// before the `|| outcome:` segment and `outcome_spec` is the raw payload after
/// `outcome:`. The whole remainder (including any `||` inside a JSON schema) is
/// kept so `outcome:{"a":"1||2"}` is not truncated. Returns `(raw, None)` when no
/// top-level `outcome:` segment is present. `split`/`join` on `||` are exact
/// inverses, so the reconstructed head equals the original substring.
fn split_outcome_section(raw: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = raw.split("||").collect();
    // Segment 0 is always the description; scan later segments for `outcome:`.
    for i in 1..parts.len() {
        if let Some(spec) = parts[i].trim_start().strip_prefix("outcome:") {
            let head = parts[..i].join("||");
            // The outcome payload is this segment's remainder plus any following
            // `||`-joined segments (JSON schemas may contain `||` inside strings).
            let mut spec = spec.to_string();
            if i + 1 < parts.len() {
                spec.push_str("||");
                spec.push_str(&parts[i + 1..].join("||"));
            }
            return (head, Some(spec.trim().to_string()));
        }
    }
    (raw.to_string(), None)
}

impl ChatCommand {
    /// Whether this command requires admin/owner privileges.
    ///
    /// Safety words that affect agent operation (`!STOP`, `!STOP ALL`, `!RESUME`)
    /// should only be usable by admins to prevent denial-of-service by
    /// arbitrary group members. `!STATUS` is read-only and allowed for all.
    ///
    /// `/model <name>` writes `agent.toml` and changes what every subsequent
    /// turn costs, so it is admin-only too — but only in its *setter* form:
    /// bare `/model` just reports the current model and stays open to everyone.
    pub fn requires_admin(&self) -> bool {
        matches!(
            self,
            ChatCommand::SafetyStop
                | ChatCommand::SafetyStopAll
                | ChatCommand::SafetyResume
                | ChatCommand::Model(Some(_))
        )
    }
}

/// Check if a message starts with a `/` command or `!` safety word.
pub fn is_command(text: &str) -> bool {
    let trimmed = text.trim();
    (trimmed.starts_with('/') && trimmed.len() > 1 && trimmed.as_bytes()[1].is_ascii_alphabetic())
        || is_likely_safety_word(trimmed)
}

/// Fast heuristic: does this look like a safety word?
///
/// This uses only prefix checking (`!` followed by non-space) so it
/// doesn't need the full config. Actual validation happens in
/// `parse_safety_word_with_config`.
fn is_likely_safety_word(text: &str) -> bool {
    text.starts_with('!') && text.len() > 1 && text.as_bytes()[1] != b' '
}

/// Try to parse a safety word using the actual loaded config.
///
/// Call this instead of the hardcoded-default version.
pub fn parse_safety_word_with_config(
    text: &str,
    config: &duduclaw_security::killswitch::SafetyWordsConfig,
) -> Option<ChatCommand> {
    match duduclaw_security::safety_word::check(text, config) {
        SafetyWordAction::Stop(SafetyWordScope::CurrentScope) => Some(ChatCommand::SafetyStop),
        SafetyWordAction::Stop(SafetyWordScope::Global) => Some(ChatCommand::SafetyStopAll),
        SafetyWordAction::Resume => Some(ChatCommand::SafetyResume),
        SafetyWordAction::Status => Some(ChatCommand::SafetyStatus),
        SafetyWordAction::None => None,
    }
}

/// Parse a chat command from user text.  Returns `None` if not a recognized command.
///
/// Also checks for safety words (e.g., `!STOP`, `!RESUME`).
/// Pass the actual `SafetyWordsConfig` loaded from KILLSWITCH.toml.
pub fn parse_command(
    text: &str,
    safety_config: Option<&duduclaw_security::killswitch::SafetyWordsConfig>,
) -> Option<ChatCommand> {
    let trimmed = text.trim();

    // Check safety words first (they use `!` prefix)
    if let Some(config) = safety_config {
        if let Some(cmd) = parse_safety_word_with_config(trimmed, config) {
            return Some(cmd);
        }
    } else {
        // Fallback: use defaults if no config provided
        let default_config = duduclaw_security::killswitch::SafetyWordsConfig::default();
        if let Some(cmd) = parse_safety_word_with_config(trimmed, &default_config) {
            return Some(cmd);
        }
    }

    if !trimmed.starts_with('/') {
        return None;
    }

    // Split into command and args: "/model claude-sonnet" → ("model", Some("claude-sonnet"))
    let without_slash = &trimmed[1..];
    let (cmd, args) = match without_slash.find(char::is_whitespace) {
        Some(pos) => (&without_slash[..pos], Some(without_slash[pos..].trim())),
        None => (without_slash, None),
    };

    match cmd.to_ascii_lowercase().as_str() {
        "status" | "s" => Some(ChatCommand::Status),
        "new" | "reset" => Some(ChatCommand::New),
        "usage" | "cost" => Some(ChatCommand::Usage),
        "help" | "h" => Some(ChatCommand::Help),
        "compact" => Some(ChatCommand::Compact),
        "model" | "m" => Some(ChatCommand::Model(
            args.filter(|a| !a.is_empty()).map(|a| a.to_string()),
        )),
        "pair" => {
            let code = args.filter(|a| !a.is_empty()).map(|a| a.to_string())?;
            Some(ChatCommand::Pair(code))
        }
        "voice" | "v" => Some(ChatCommand::Voice),
        "screenshot" | "ss" => Some(ChatCommand::Screenshot),
        "computer" => match args.map(|a| a.to_ascii_lowercase()).as_deref() {
            Some("on") => Some(ChatCommand::ComputerOn),
            Some("off") => Some(ChatCommand::ComputerOff),
            _ => Some(ChatCommand::Screenshot), // bare /computer → show status
        },
        "pause" => Some(ChatCommand::ComputerPause),
        "resume" => Some(ChatCommand::ComputerResume),
        "stop" => Some(ChatCommand::ComputerStop),
        "replay" => {
            let n = args.and_then(|a| a.parse::<u32>().ok()).unwrap_or(5);
            Some(ChatCommand::Replay(n))
        }
        "handoff" => Some(ChatCommand::Handoff(
            args.filter(|a| !a.is_empty())
                .map(|a| a.to_ascii_lowercase()),
        )),
        "undo" => {
            // Same laxity as /replay: non-numeric arg falls back to default.
            let n = args.and_then(|a| a.parse::<u32>().ok()).unwrap_or(1);
            Some(ChatCommand::Undo(n))
        }
        "rollback" => Some(ChatCommand::Rollback),
        "goal" => Some(ChatCommand::Goal(parse_goal_args(args))),
        // W3-2 經驗法則 (+ W3-5 `off <n>`). Anything other than `all`/`off <n>`
        // in the tail is ignored rather than rejected (same laxity as
        // `/replay`) — a mistyped argument should still show the summary,
        // not an error. `off` with a missing/non-numeric/zero index still
        // produces `Rules { off: Some(0) }` rather than falling back to the
        // summary — [`handle_rules_off`] reports "no such rule number"
        // instead of silently showing the listing, since a person who typed
        // `/rules off` deserves to know the command was recognized but
        // needs a number, not a bare summary that looks like `off` was
        // ignored.
        "rules" => {
            let tail = args.map(str::trim).unwrap_or("");
            let mut words = tail.split_whitespace();
            let off = match words.next() {
                Some(w) if w.eq_ignore_ascii_case("off") => {
                    Some(words.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0))
                }
                _ => None,
            };
            Some(ChatCommand::Rules {
                all: off.is_none() && tail.eq_ignore_ascii_case("all"),
                off,
            })
        }
        _ => None,
    }
}

/// Execute a chat command and return the response text.
///
/// `is_admin` indicates whether the user has admin/owner privileges (the
/// per-channel `admin_users` allowlist — `channel_reply::is_channel_admin`).
/// Safety words that modify agent state (`!STOP`, `!RESUME`) require admin.
///
/// `channel_user_id` is the sender's raw channel account id (the identifier
/// a dashboard "bind this channel account" flow would key on — e.g. a
/// Telegram numeric id, a Slack `U…` id). It is unused by every command
/// except `/rules off <n>` (W3-5), which needs the STRONGER
/// identity-verified [`crate::takeover::manager_display_name`] predicate
/// takeover.rs already established, not the looser `admin_users` list —
/// disabling a rule is a persistent write to the agent's playbook, not a
/// read. Pass `""` for a channel with no addressable per-sender identity
/// (e.g. an anonymous WebChat connection); [`handle_rules_off`] then
/// correctly refuses with "no verified identity" rather than guessing.
pub async fn handle_command(
    cmd: &ChatCommand,
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    is_admin: bool,
    channel_user_id: &str,
) -> String {
    // Enforce admin requirement for destructive safety commands.
    // Fail-closed: callers must compute real admin status per channel
    // (channel_reply::is_channel_admin) — never hardcode `true`.
    if cmd.requires_admin() && !is_admin {
        return "⚠️ 此指令僅限管理員使用。請管理員在該頻道設定 admin_users 後再試。".to_string();
    }

    match cmd {
        ChatCommand::Status => handle_status(ctx, session_id, agent_id).await,
        ChatCommand::New => handle_new(ctx, session_id).await,
        ChatCommand::Usage => handle_usage(ctx, agent_id).await,
        ChatCommand::Help => handle_help(),
        ChatCommand::Compact => handle_compact(ctx, session_id).await,
        ChatCommand::Model(name) => handle_model(ctx, agent_id, name.as_deref()).await,
        ChatCommand::Pair(code) => handle_pair(ctx, session_id, code).await,
        ChatCommand::Voice => {
            let mut voice_set = ctx.voice_sessions.lock().await;
            let key = session_id.to_string();
            if voice_set.contains(&key) {
                voice_set.remove(&key);
                "🔇 Voice mode OFF — replies will be sent as text.".to_string()
            } else {
                voice_set.insert(key);
                "🔊 Voice mode ON — next replies will be sent as audio.".to_string()
            }
        }
        ChatCommand::Screenshot => {
            // Handled by the caller — needs access to the orchestrator
            "📸 截圖指令已接收（需要 active computer use session）".to_string()
        }
        ChatCommand::ComputerOn => "🖥️ Computer Use 已啟用（本次 session）".to_string(),
        ChatCommand::ComputerOff => "🖥️ Computer Use 已關閉".to_string(),
        ChatCommand::ComputerPause => {
            "⏸ Computer Use session 已暫停。發送 /resume 繼續".to_string()
        }
        ChatCommand::ComputerResume => "▶️ Computer Use session 已恢復".to_string(),
        ChatCommand::ComputerStop => "🛑 Computer Use session 已終止".to_string(),
        ChatCommand::Replay(n) => handle_replay(ctx, agent_id, *n).await,
        ChatCommand::Handoff(target) => {
            handle_handoff(ctx, session_id, agent_id, target.as_deref()).await
        }
        ChatCommand::Undo(n) => handle_undo(ctx, session_id, *n).await,
        ChatCommand::Rollback => handle_rollback(ctx, session_id).await,
        ChatCommand::SafetyStop => handle_safety_stop(ctx, session_id, agent_id).await,
        ChatCommand::SafetyStopAll => handle_safety_stop_all(ctx).await,
        ChatCommand::SafetyResume => handle_safety_resume(ctx, session_id).await,
        ChatCommand::SafetyStatus => handle_safety_status(ctx, session_id).await,
        ChatCommand::Goal(goal) => handle_goal(ctx, session_id, agent_id, goal).await,
        ChatCommand::Rules { all, off: None } => handle_rules(ctx, agent_id, *all).await,
        ChatCommand::Rules { off: Some(n), .. } => {
            handle_rules_off(ctx, session_id, channel_user_id, agent_id, *n).await
        }
    }
}

// ── Individual handlers ─────────────────────────────────────────

async fn handle_status(ctx: &ReplyContext, session_id: &str, agent_id: &str) -> String {
    let reg = ctx.registry.read().await;
    let agent = reg.get(agent_id).or_else(|| reg.main_agent());

    let (display_name, role, status, model) = match agent {
        Some(a) => (
            a.config.agent.display_name.clone(),
            format!("{:?}", a.config.agent.role),
            format!("{:?}", a.config.agent.status),
            a.config.model.preferred.clone(),
        ),
        None => (
            "Unknown".to_string(),
            "N/A".to_string(),
            "N/A".to_string(),
            "N/A".to_string(),
        ),
    };
    drop(reg);

    // Session info
    let session_info = match ctx
        .session_manager
        .get_or_create(session_id, agent_id)
        .await
    {
        Ok(session) => format!(
            "Session: #{}\nTokens: {}\nLast active: {}",
            session.lineage, session.total_tokens, session.last_active
        ),
        Err(_) => "Session: N/A".to_string(),
    };

    // Channel status
    let channel_map = ctx.channel_status.read().await;
    let channels: Vec<String> = channel_map
        .iter()
        .map(|(name, state)| {
            let icon = if state.connected { "🟢" } else { "🔴" };
            format!("{icon} {name}")
        })
        .collect();
    drop(channel_map);

    format!(
        "📊 *Status*\n\
         Agent: {display_name}\n\
         Role: {role}\n\
         Status: {status}\n\
         Model: `{model}`\n\
         {session_info}\n\
         Channels: {}\n\
         Version: v{}",
        if channels.is_empty() {
            "none".to_string()
        } else {
            channels.join(", ")
        },
        crate::updater::current_version(),
    )
}

async fn handle_new(ctx: &ReplyContext, session_id: &str) -> String {
    match ctx.session_manager.delete_session(session_id).await {
        Ok(()) => "✅ Session cleared. Starting fresh!".to_string(),
        Err(e) => {
            warn!(session_id, error = %e, "Failed to clear session");
            format!("⚠️ Failed to clear session: {e}")
        }
    }
}

async fn handle_usage(_ctx: &ReplyContext, _agent_id: &str) -> String {
    // Cost telemetry summary — reads from SQLite via CostTelemetry module.
    // TODO: wire to crate::cost_telemetry::get_summary when available.
    "💰 Usage tracking is available in the Dashboard → Reports page.".to_string()
}

fn handle_help() -> String {
    "📖 *Available Commands*\n\n\
     `/status` — Agent status and session info\n\
     `/new` — Clear session, start fresh\n\
     `/usage` — Token usage and cost summary\n\
     `/help` — Show this help message\n\
     `/compact` — Force session compression\n\
     `/model [name]` — Show or switch model\n\
     `/pair <code>` — Verify pairing code\n\
     `/voice` — Toggle voice reply\n\
     `/handoff <頻道>` — 將對話轉移到其他頻道\n\
     `/undo [次數]` — 撤銷最近 N 輪對話（預設 1，最多 20）\n\
     `/rollback` — 回退到最近一次檢查點（僅回退對話，不還原檔案）\n\
     `/goal <目標>` — 交付一個自主目標，AI 自主做到完成、卡住問你（`/goal status` 看進度）\n\
     `/rules` — 看目前生效中的經驗法則（`/rules all` 看全部、`/rules off <編號>` 停用一條，限管理員／主管）\n\
     `/takeover` — 看目前是誰在回覆（管理員直接發言即接手；`+30m` 延長、`end` 交還）\n\n\
     *Safety Words*\n\
     `!STOP` / `!停止` — Stop current agent\n\
     `!STOP ALL` / `!全部停止` — Emergency stop all\n\
     `!RESUME` / `!恢復` — Resume agent\n\
     `!STATUS` / `!狀態` — Check safety status"
        .to_string()
}

async fn handle_compact(ctx: &ReplyContext, session_id: &str) -> String {
    match ctx.session_manager.force_compress(session_id).await {
        Ok(saved) => format!("🗜️ Session compressed. Saved ~{saved} tokens."),
        Err(e) => {
            warn!("Failed to compress session: {e}");
            format!("⚠️ Compression failed: {e}")
        }
    }
}

async fn handle_pair(ctx: &ReplyContext, session_id: &str, code: &str) -> String {
    // Channels that intercept commands here don't carry a user id, so the
    // pairing subject is the session id. The central access gate in
    // channel_reply checks BOTH user id and session id, so either form of
    // approval unlocks the conversation. Codes come from the operator via
    // the `pairing_generate` MCP tool.
    if ctx
        .access_control
        .verify_pairing_code(session_id, code)
        .await
    {
        "✅ 配對成功，現在可以開始對話了。".to_string()
    } else {
        "❌ 配對碼錯誤或已過期，請向管理員索取新的配對碼。".to_string()
    }
}

/// Guard the value that is about to be written into `agent.toml [model]
/// preferred`. This string is later handed to CLI runtimes as an argument and
/// stored in a config file, so it is validated as a strict identifier rather
/// than trusted: model ids are vendor slugs (`claude-opus-5`,
/// `deepseek-chat`, `gpt-5.1`), never paths, flags or shell text.
///
/// Deliberately a *shape* check, not an allow-list: new models appear between
/// releases, and rejecting a valid one the operator wants would be worse than
/// letting a typo through — a wrong-but-well-formed id surfaces immediately as
/// a provider error on the next turn.
fn validate_model_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("模型名稱不可為空。".to_string());
    }
    if name.len() > 64 {
        return Err("模型名稱過長（上限 64 字元）。".to_string());
    }
    if name.starts_with('-') {
        return Err("模型名稱不可以 `-` 開頭。".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
    {
        return Err("模型名稱只能包含英數字與 - _ . : / 。".to_string());
    }
    Ok(())
}

async fn handle_model(ctx: &ReplyContext, agent_id: &str, new_model: Option<&str>) -> String {
    let reg = ctx.registry.read().await;
    let agent = reg.get(agent_id).or_else(|| reg.main_agent());

    match agent {
        Some(a) => {
            let current = a.config.model.preferred.clone();
            let resolved_id = a.config.agent.name.clone();
            // Drop the read guard before the write path re-acquires it.
            drop(reg);
            match new_model {
                Some(name) => {
                    let name = name.trim();
                    if name == current {
                        return format!("🤖 已經在用 `{current}` 了，沒有變更。");
                    }
                    if let Err(e) = validate_model_name(name) {
                        return format!("⚠️ {e}");
                    }
                    let target = name.to_string();
                    match crate::channel_reply::update_agent_toml_with(
                        &ctx.registry,
                        &resolved_id,
                        move |table| {
                            let model = table
                                .entry("model")
                                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                                .as_table_mut()
                                .ok_or("agent.toml [model] is not a table")?;
                            model.insert("preferred".into(), toml::Value::String(target));
                            Ok(())
                        },
                    )
                    .await
                    {
                        // The rescan is what makes the change take effect now;
                        // say so plainly when it did not land rather than
                        // reporting a success the next turn will contradict.
                        Ok(true) => format!("✅ 已切換模型：`{current}` → `{name}`，下一則訊息就會生效。"),
                        Ok(false) => format!(
                            "⚠️ 已寫入設定（`{current}` → `{name}`），但熱重載失敗，要重啟 gateway 才會生效。"
                        ),
                        Err(e) => format!("⚠️ 切換失敗：{e}"),
                    }
                }
                None => format!("🤖 目前模型：`{current}`\n輸入 `/model <名稱>` 可切換（僅限管理員）。"),
            }
        }
        None => "⚠️ No agent found.".to_string(),
    }
}

// ── G4 session portability handlers (/handoff, /undo, /rollback) ──

/// `/handoff <channel>` — copy the conversation state onto the same agent's
/// session on the target channel. Fail-closed at every step: unknown
/// channel, unconfigured/disconnected channel, no existing session on the
/// target (we cannot invent a `<channel>:<chat_id>` key), or an AMBIGUOUS
/// target (several unrelated sessions — see `resolve_handoff_target`;
/// "most recent wins" would land the transcript in another user's chat).
async fn handle_handoff(
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    target: Option<&str>,
) -> String {
    let channel_list = duduclaw_core::SUPPORTED_CHANNEL_TYPES.join("、");
    let target = match target {
        Some(t) => t.trim().to_ascii_lowercase(),
        None => {
            return format!("用法：/handoff <頻道>\n可用頻道：{channel_list}");
        }
    };

    if !duduclaw_core::SUPPORTED_CHANNEL_TYPES.contains(&target.as_str()) {
        return format!("❌ 不支援的頻道「{target}」。\n可用頻道：{channel_list}");
    }

    let source_channel = session_id.split(':').next().unwrap_or("");
    if source_channel == target {
        return format!("目前對話已經在 {target} 頻道，不需要轉移。");
    }

    // The session row's agent is authoritative; fall back to the caller's.
    let owner = match ctx.session_manager.session_agent(session_id).await {
        Ok(Some(a)) => a,
        Ok(None) => agent_id.to_string(),
        Err(e) => {
            warn!(session_id, error = %e, "handoff: session_agent lookup failed");
            return format!("⚠️ 轉移失敗：{e}");
        }
    };

    // Fail closed: the target channel must be configured AND connected on
    // this gateway before we move a conversation onto it. Per-agent bots
    // register under `<channel>:<agent>` keys — check those too before
    // declaring the channel unconnected (exact keys, never substrings).
    let connected = {
        let status = ctx.channel_status.read().await;
        [
            target.clone(),
            format!("{target}:{owner}"),
            format!("{target}:{agent_id}"),
        ]
        .iter()
        .any(|key| status.get(key).map(|s| s.connected).unwrap_or(false))
    };
    if !connected {
        return format!(
            "❌ {target} 頻道尚未設定或目前未連線，無法轉移。請先在儀表板完成該頻道設定。"
        );
    }

    let candidates = match ctx
        .session_manager
        .active_sessions_for_channel(&owner, &target, 10)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(session_id, target = %target, error = %e, "handoff: target lookup failed");
            return format!("⚠️ 轉移失敗：{e}");
        }
    };
    let target_session = match crate::session_portability::resolve_handoff_target(
        &candidates,
        session_id,
    ) {
        crate::session_portability::HandoffTarget::Resolved(id) => id,
        crate::session_portability::HandoffTarget::NoSession => {
            // Design choice (documented in session_portability.rs): sessions
            // are keyed by <channel>:<chat_id>, so without an inbound message
            // we cannot know where to deliver. Ask the user to ping first.
            return format!(
                "❌ {target} 頻道還沒有任何對話。請先在 {target} 頻道傳一則訊息給代理建立會話，再回來執行 /handoff {target}。"
            );
        }
        crate::session_portability::HandoffTarget::Ambiguous(n) => {
            // Fail-closed: never guess among several users' conversations.
            return format!(
                "❌ 目標頻道有多個對話（{n} 個），無法確定要轉移到哪一個。\
                 請先在 {target} 頻道傳送一則訊息以指定對話，再回來執行 /handoff {target}。"
            );
        }
    };

    match ctx
        .session_manager
        .handoff_session(session_id, &target_session)
        .await
    {
        Ok(crate::session_portability::HandoffDecision::Done(report)) => format!(
            "✅ 已將對話轉移到 {target} 頻道（共 {} 則訊息）。\n到 {target} 頻道傳送訊息即可接續目前的對話；本頻道的紀錄仍會保留。",
            report.copied_messages
        ),
        Ok(crate::session_portability::HandoffDecision::NothingToHandoff) => {
            "目前對話沒有可轉移的內容。".to_string()
        }
        Err(e) => {
            warn!(session_id, target = %target, error = %e, "handoff failed");
            format!("⚠️ 轉移失敗：{e}")
        }
    }
}

/// `/undo [N]` — tombstone the last N turn pairs (default 1, max 20).
async fn handle_undo(ctx: &ReplyContext, session_id: &str, n: u32) -> String {
    use crate::session_portability::{UndoDecision, UNDO_MAX_PAIRS};
    if n == 0 || n > UNDO_MAX_PAIRS {
        return format!("❌ 次數必須介於 1 到 {UNDO_MAX_PAIRS} 之間。用法：/undo [次數]");
    }
    match ctx.session_manager.undo_last_turns(session_id, n).await {
        Ok(UndoDecision::Undone { pairs, messages, .. }) => {
            format!("↩️ 已撤銷最近 {pairs} 輪對話（共 {messages} 則訊息）。")
        }
        Ok(UndoDecision::NothingToUndo) => "目前沒有可撤銷的對話。".to_string(),
        Ok(UndoDecision::BoundaryBlocked { available }) => format!(
            "⚠️ 無法撤銷 {n} 輪：更早的對話已經過壓縮整理，無法再撤銷。目前最多可撤銷 {available} 輪（/undo {available}）。"
        ),
        Err(e) => {
            warn!(session_id, error = %e, "undo failed");
            format!("⚠️ 撤銷失敗：{e}")
        }
    }
}

/// `/rollback` — conversation-state rollback to the last checkpoint
/// watermark. File changes made by the agent are NOT reverted (the CLI
/// runtimes own file edits) and the reply says so honestly.
async fn handle_rollback(ctx: &ReplyContext, session_id: &str) -> String {
    use crate::session_portability::RollbackDecision;
    match ctx.session_manager.rollback_to_checkpoint(session_id).await {
        Ok(RollbackDecision::RolledBack { messages, .. }) => format!(
            "⏪ 已回退到最近一次檢查點（移除 {messages} 則訊息）。\n注意：這只會回退對話內容；代理先前對檔案所做的變更不會被還原。"
        ),
        Ok(RollbackDecision::NothingToRollback) => {
            "目前沒有可回退的檢查點內容。若要撤銷最近的對話，可改用 /undo。".to_string()
        }
        Err(e) => {
            warn!(session_id, error = %e, "rollback failed");
            format!("⚠️ 回退失敗：{e}")
        }
    }
}

// ── /goal autonomous goal loop entry (P5) ───────────────────────

/// Derive `(channel, chat_id)` from a session id
/// (`<channel>:<chat_id>[:<thread>]`), matching
/// `channel_reply::parse_session_id_parts`. Stamped onto a goal task's
/// `source_channel` / `source_chat_id` so goal-loop progress and needs_human
/// notices push back to the conversation that launched the goal (falling back to
/// the agent's `[proactive]` channel when a task has no source).
pub(crate) fn source_from_session(session_id: &str) -> (String, String) {
    let parts: Vec<&str> = session_id.splitn(3, ':').collect();
    match parts.as_slice() {
        [] | [_] => (String::new(), session_id.to_string()),
        [ch, cid, ..] => (ch.to_string(), cid.to_string()),
    }
}

/// zh-TW label for a goal task status (user-facing; internal status strings must
/// never leak to the channel).
fn goal_status_label(status: &str) -> &'static str {
    match status {
        "todo" => "待派工",
        "pending" => "待重試",
        "in_progress" => "執行中",
        "review" => "驗收中",
        "needs_human" => "待你決定",
        "blocked" => "受阻",
        "done" => "已完成",
        "cancelled" => "已放棄",
        "failed" => "已失敗",
        _ => "進行中",
    }
}

fn goal_usage_text() -> String {
    "🎯 *自主目標 /goal*\n\
     `/goal <目標描述>` — 交付一個目標，AI 員工會自主規劃、執行、自我驗收，完成或卡住時通知你\n\
     `/goal <目標> || <驗收標準>` — 用 `||` 另外指定驗收標準（沒給就用目標本身當驗收基準）\n\
     `/goal <目標> || <驗收標準> || outcome:<spec>` — 再加結構化產出驗收（交付前零成本自動校驗）\n\
     • `outcome:text`（預設，行為不變）\n\
     • `outcome:json:<JSON Schema>` — 校驗最終回覆裡的 ```json 區塊（object/array/string/number/boolean）\n\
     • `outcome:files:<glob,glob>` — 斷言工作目錄下有對應產出檔（例 `outcome:files:report.docx`）\n\
     `/goal <目標> || 時限:36h || 邊界:<紅線>` — 任一段可加時限（`36h`／`3天`，逾期回到你手上）與風險邊界（沒給就套基本款：法規／資安／風控／人審紅線）\n\
     `/goal <目標> || 想一想` — 先請 AI 員工產出一份執行計畫給你核准，核准後才開始執行（不會自動開工；也可寫 `plan`，任一段皆可）\n\
     `/goal status` — 查看進行中的目標任務\n\n\
     範例：`/goal 整理這批客戶資料成月報並寄出 || 報表含每月營收圖表 || 時限:2天 || outcome:files:report.docx`"
        .to_string()
}

/// H9-W four-element goal guidance (harness-borrowings 2026-08 WP-D,
/// borrowed from WorkBuddy's task-description framework). Appended to the
/// `/goal` creation reply only when no explicit `||` acceptance criteria was
/// given — the goal text became the acceptance basis by default, and this
/// nudges the user toward a sharper contract next time. zh-TW, terminal-user
/// friendly: no internal vocabulary (no "judge"/"MAV"/"baseline"/"column").
fn four_element_guidance() -> String {
    "\n\n💡 這次沒有另外指定驗收標準，之後想清楚這四件事會更好抓：\n\
     • 目標：要達成什麼\n\
     • 輸入：需要用到哪些資料或素材\n\
     • 輸出格式：成果長什麼樣子（例如 Word／Excel／一段文字／一張圖）\n\
     • 約束：風格、時限等限制，以及「怎樣才算完成」\n\
     建議補 3-5 條具體、看得出結果的驗收標準（例如「報表含每月營收圖表」而非「做得好」），\
     用 `/goal 描述 || 標準` 格式重新交付一次即可。"
        .to_string()
}

async fn handle_goal(
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    goal: &GoalCommand,
) -> String {
    match goal {
        GoalCommand::Usage => goal_usage_text(),
        GoalCommand::Status => handle_goal_status(ctx, agent_id).await,
        GoalCommand::Create {
            description,
            acceptance_criteria,
            outcome,
            duration_hours,
            risk_boundary,
            plan_first,
        } => {
            handle_goal_create(
                ctx,
                session_id,
                agent_id,
                description,
                acceptance_criteria.as_deref(),
                outcome.as_deref(),
                *duration_hours,
                risk_boundary.as_deref(),
                *plan_first,
            )
            .await
        }
    }
}

/// Create a `goal_mode` task assigned to the current conversation's agent and
/// stamp its source conversation for progress write-back.
///
/// `pub(crate)`: the channel-side goal intent router (`goal_intent.rs`,
/// DESIGN-goal-intent-router-2026-08.md) reuses this exact path for its "1"
/// (立為目標任務) confirmation — same acceptance-criteria defaulting, same
/// baseline freeze, same four-element guidance, same optional decomposition
/// — so confirming a suggestion is byte-identical in effect to typing
/// `/goal <description>` by hand (always with `plan_first = false`; "2" 想一想
/// goes through the separate `goal_intent::accept_as_plan_first` path).
///
/// `plan_first` (P1) brings the dashboard's I-1c "想一想" mode to this path:
/// same `goal_plan::generate_plan_first` + `apply_plan_first_result` pair as
/// `handlers::handle_tasks_goal_create`'s `plan_first` branch, so a chat
/// `/goal <desc> || 想一想` parks `needs_human` with a narrative plan awaiting
/// approval instead of opening straight to `todo` — see the branch below.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_goal_create(
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    description: &str,
    acceptance_criteria: Option<&str>,
    outcome: Option<&str>,
    duration_hours: Option<f64>,
    risk_boundary: Option<&str>,
    plan_first: bool,
) -> String {
    let store = match crate::task_store::TaskStore::open(&ctx.home_dir) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "goal: open task store failed");
            return format!("⚠️ 無法建立目標任務：{e}");
        }
    };
    let (channel, chat_id) = source_from_session(session_id);
    // No explicit criteria → the goal text itself is the acceptance basis.
    let criteria = acceptance_criteria
        .map(str::to_string)
        .unwrap_or_else(|| description.to_string());

    // ── WP2.4: parse + validate the structured outcome spec up front ──
    // A malformed spec is rejected fail-closed (the task is NOT created) so the
    // user fixes it rather than getting a silently text-only acceptance.
    let outcome_tag = match outcome.map(crate::outcome_spec::OutcomeSpec::parse) {
        Some(Ok(spec)) => spec.to_tag(), // None for `text` (nothing to persist)
        Some(Err(e)) => return format!("⚠️ outcome 產出驗收格式錯誤：{e}"),
        None => None,
    };

    // ── D4 item 1: optional LLMCompiler-style decomposition ──
    // Off by default; only attempted when `[goal_loop] planner_enabled = true`.
    // A valid multi-task DAG is inserted as-is; anything else (declined split,
    // parse failure, cycle, trivial plan) falls through to the single-task path
    // below so behavior is byte-identical when the planner is off or unhelpful.
    // Skipped entirely when an outcome spec is set: the structured contract
    // applies to one final deliverable, not to each decomposed subtask.
    // Also skipped when a per-goal deadline or risk boundary is set: the
    // contract applies to one task, and decomposed subtasks would not
    // inherit it (same reasoning as the outcome-spec skip above). P1: also
    // skipped when `plan_first` is set — 想一想 is a single-task narrative
    // plan for human approval, not a machine-parsed DAG (same independence
    // the dashboard's I-1c doc comment on `handle_tasks_goal_create` notes).
    if outcome_tag.is_none()
        && duration_hours.is_none()
        && risk_boundary.is_none()
        && !plan_first
        && crate::goal_plan::planner_enabled(&ctx.home_dir)
    {
        if let Some(reply) = try_decompose_goal(
            &store,
            &ctx.home_dir,
            agent_id,
            description,
            &criteria,
            &channel,
            &chat_id,
        )
        .await
        {
            return reply;
        }
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let short = duduclaw_core::truncate_chars(&task_id, 8);
    let title = duduclaw_core::truncate_chars(description, 60);

    let mut task = crate::task_store::TaskRow::new(
        task_id,
        title,
        description.to_string(),
        "medium".to_string(),
        agent_id.to_string(),
        format!("goal:{channel}"),
    );
    task.status = "todo".to_string();
    task.goal_mode = true;
    task.acceptance_criteria = Some(criteria.clone());
    // H9-G goal contract freeze (harness-borrowings 2026-08 WP-D): snapshot the
    // acceptance criteria into an immutable baseline the SAME moment they're
    // set. The judge reads this column (see `dispatch_engine.rs`'s
    // `review_goal_tasks`), never the mutable `acceptance_criteria` field, so
    // a later operator edit to the latter cannot retroactively change what the
    // task is judged on.
    task.acceptance_criteria_baseline = Some(criteria);
    // Goal contract v2 (design §6 G1/G3): same semantics as the dashboard
    // form — deadline computed at creation; boundary stored only when
    // explicitly given (baseline is applied at injection time instead).
    if let Some(h) = duration_hours {
        task.deadline_at = Some(
            (chrono::Utc::now() + chrono::Duration::seconds((h * 3600.0) as i64)).to_rfc3339(),
        );
    }
    task.risk_boundary =
        risk_boundary.map(|b| duduclaw_core::truncate_chars(b, 2000));
    // WP2.4: ride the structured outcome spec on the existing comma-separated
    // tags field (base64url, comma-free) — no schema change. `text` specs and
    // the no-spec case leave tags untouched (behaviour unchanged).
    if let Some(tag) = &outcome_tag {
        task.tags = tag.clone();
    }
    // W2-7: snapshot the Discord guild id (if known) onto the task at
    // creation time — see `discord::guild_id_for_channel` module docs for
    // why this is captured now rather than looked up live at list time.
    // Borrow before the two moves below consume `channel`/`chat_id`.
    let source_discord_guild_id = (channel == "discord")
        .then(|| crate::discord::guild_id_for_channel(&ctx.home_dir, &chat_id))
        .flatten();
    if !channel.is_empty() {
        task.source_channel = Some(channel);
    }
    task.source_chat_id = Some(chat_id);
    task.source_discord_guild_id = source_discord_guild_id;

    // P1 "想一想": generate a narrative plan and park the task `needs_human`
    // instead of `todo` — same precedent as the decomposer call above (runs
    // synchronously here, before `insert_task`, so no dispatch-engine round
    // — not even a restricted one — ever starts before a human approves).
    // Fail-closed: a planner failure never falls through to the normal
    // `todo` path — `apply_plan_first_result`'s `Err` branch still parks
    // `needs_human` under the `infra` pause class for a human to retry or
    // cancel. Mirrors `handlers::handle_tasks_goal_create`'s `plan_first`
    // branch exactly (same two functions, same field semantics).
    if plan_first {
        let criteria_for_plan = task.acceptance_criteria.as_deref().unwrap_or("");
        let plan_result =
            crate::goal_plan::generate_plan_first(&ctx.home_dir, &task.description, criteria_for_plan)
                .await;
        if let Err(e) = &plan_result {
            warn!(
                agent = %agent_id,
                error = %e,
                "goal: plan-first generation failed — parking needs_human (infra class) instead of auto-starting"
            );
        }
        crate::goal_plan::apply_plan_first_result(&mut task, plan_result);

        if let Err(e) = store.insert_task(&task).await {
            warn!(error = %e, "goal: insert plan-first task failed");
            return format!("⚠️ 無法建立目標任務：{e}");
        }
        return match &task.plan_pending {
            Some(plan) => format!(
                "🧭 已建立目標任務 #{short}，並產生執行計畫，等你核准後才會開始執行：\n\n{plan}\n\n請到 /goals 儀表板核准或修改。"
            ),
            None => format!(
                "⚠️ 已建立目標任務 #{short}，但計畫生成失敗，已標記為待你決定，請到 /goals 儀表板處理。"
            ),
        };
    }

    if let Err(e) = store.insert_task(&task).await {
        warn!(error = %e, "goal: insert task failed");
        return format!("⚠️ 無法建立目標任務：{e}");
    }

    let cap = crate::goal_loop::GoalLoopConfig::from_home(&ctx.home_dir).iteration_cap;
    let enabled = crate::dispatch_engine::dispatch_engine_enabled(&ctx.home_dir);
    let mut msg = format!(
        "🎯 已建立自主目標任務 #{short}\n\
         目標：{}\n\
         我會自主嘗試最多 {cap} 輪；完成或卡住時會在這裡通知你。\n\
         輸入 `/goal status` 可隨時查看進度。",
        duduclaw_core::truncate_chars(description, 200),
    );
    if outcome_tag.is_some() {
        msg.push_str(
            "\n📐 已設定結構化產出驗收：交付前會先做零成本自動校驗，未達標會直接退回修正（不浪費驗收判官）。",
        );
    }
    if let Some(h) = duration_hours {
        msg.push_str(&format!(
            "\n⏱ 時限：{h:.0} 小時內須通過驗收，逾期會回到你手上裁決。"
        ));
    }
    if task.risk_boundary.is_some() {
        msg.push_str("\n🚧 已設定本目標風險邊界：每輪執行與驗收判官都會以它檢核。");
    } else {
        msg.push_str("\n🚧 未指定風險邊界，將套用基本款（法規／資安／風控／人審紅線）。");
    }
    // H9-W four-element guidance (harness-borrowings 2026-08 WP-D): the task
    // is created either way (goal text doubles as the acceptance basis when
    // no `||` criteria was given) — this only nudges toward a clearer
    // contract on the NEXT `/goal`, it never blocks or delays this one.
    if acceptance_criteria.is_none() {
        msg.push_str(&four_element_guidance());
    }
    if !enabled {
        msg.push_str(
            "\n\n⚠️ 目前尚未啟用自主派工引擎，任務已建立但不會自動開始執行。\
             請在 config.toml 設定 `[dispatch] enabled = true` 後重啟。",
        );
    }
    msg
}

/// Attempt LLMCompiler-style decomposition (D4 item 1). Returns `Some(reply)`
/// when a valid multi-task DAG was inserted; `None` to fall back to the
/// single-task path. Fail-safe at every step: a decomposer error, a trivial or
/// cyclic plan, or an insert failure ⇒ `None` (never partially inserts a broken
/// DAG — inserts are attempted only after `plan_is_dag` passes; on a mid-insert
/// error the fallback single task still gets created by the caller).
async fn try_decompose_goal(
    store: &crate::task_store::TaskStore,
    home_dir: &std::path::Path,
    agent_id: &str,
    description: &str,
    criteria: &str,
    channel: &str,
    chat_id: &str,
) -> Option<String> {
    use crate::goal_plan::{plan_is_dag, plan_to_tasks, GoalDecomposer};

    let decomposer =
        crate::goal_plan::LlmGoalDecomposer::new(crate::goal_plan::UtilityDecomposeCaller {
            home_dir: home_dir.to_path_buf(),
        });
    let plan = match decomposer.decompose(description, criteria).await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "goal planner: decompose failed — falling back to single task");
            return None;
        }
    };
    // Topological / range validation. A cycle (or a trivial 0–1 task plan) ⇒
    // reject the whole DAG and fall back to single-task mode (fail-safe).
    if !plan_is_dag(&plan) {
        if plan.len() >= 2 {
            warn!(
                subtasks = plan.len(),
                "goal planner: plan rejected (cycle or invalid deps) — single-task fallback"
            );
        }
        return None;
    }

    let source_channel = (!channel.is_empty()).then_some(channel);
    let mut rows = plan_to_tasks(
        &plan,
        agent_id,
        &format!("goal:{channel}"),
        criteria,
        source_channel,
        Some(chat_id),
    );
    // W2-7: same snapshot-at-creation-time coordinate as the single-task
    // path (`handle_goal_create`) — every sub-task of a decomposed goal
    // shares the same source Discord channel/guild.
    if channel == "discord" {
        if let Some(guild) = crate::discord::guild_id_for_channel(home_dir, chat_id) {
            for row in &mut rows {
                row.source_discord_guild_id = Some(guild.clone());
            }
        }
    }
    for row in &rows {
        if let Err(e) = store.insert_task(row).await {
            warn!(error = %e, task = %row.id, "goal planner: subtask insert failed — single-task fallback");
            return None;
        }
    }

    let enabled = crate::dispatch_engine::dispatch_engine_enabled(home_dir);
    let cap = crate::goal_loop::GoalLoopConfig::from_home(home_dir).iteration_cap;
    let steps: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let dep_note = if plan[i].deps.is_empty() {
                String::new()
            } else {
                format!("(依賴前 {} 步)", plan[i].deps.len())
            };
            format!(
                "  {}. {} {dep_note}",
                i + 1,
                duduclaw_core::truncate_chars(&r.title, 40)
            )
        })
        .collect();
    let mut msg = format!(
        "🎯 已把目標拆成 {} 個可平行推進的子任務:\n{}\n\
         依賴滿足的子任務會並行開跑,每個各自驗收;完成或卡住時會在這裡通知你。\n\
         我會對每個子任務自主嘗試最多 {cap} 輪。輸入 `/goal status` 可查看進度。",
        rows.len(),
        steps.join("\n"),
    );
    if !enabled {
        msg.push_str(
            "\n\n⚠️ 目前尚未啟用自主派工引擎,子任務已建立但不會自動開始執行。\
             請在 config.toml 設定 `[dispatch] enabled = true` 後重啟。",
        );
    }
    Some(msg)
}

/// List the current agent's in-progress goal tasks.
async fn handle_goal_status(ctx: &ReplyContext, agent_id: &str) -> String {
    let store = match crate::task_store::TaskStore::open(&ctx.home_dir) {
        Ok(s) => s,
        Err(e) => return format!("⚠️ 無法讀取目標任務：{e}"),
    };
    let tasks = match store.list_tasks(None, Some(agent_id), None).await {
        Ok(t) => t,
        Err(e) => return format!("⚠️ 無法讀取目標任務：{e}"),
    };
    const ACTIVE: &[&str] = &[
        "todo",
        "pending",
        "in_progress",
        "review",
        "needs_human",
        "blocked",
    ];
    let lines: Vec<String> = tasks
        .iter()
        .filter(|t| t.goal_mode && ACTIVE.contains(&t.status.as_str()))
        .map(|t| {
            let short = duduclaw_core::truncate_chars(&t.id, 8);
            format!(
                "• #{short} [{}] 第 {} 輪 — {}",
                goal_status_label(&t.status),
                t.retry_count + 1,
                duduclaw_core::truncate_chars(&t.title, 40),
            )
        })
        .collect();
    if lines.is_empty() {
        "📭 目前沒有進行中的自主目標任務。用 `/goal <目標描述>` 交付一個。".to_string()
    } else {
        format!("🎯 進行中的自主目標任務：\n{}", lines.join("\n"))
    }
}

// ── Safety word handlers ───────────────────────────────────────

async fn handle_safety_stop(ctx: &ReplyContext, session_id: &str, _agent_id: &str) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        failsafe.force_halt(session_id, "safety word: !STOP").await;
        // Also reset the circuit breaker for this scope
        if let Some(ref cb) = ctx.circuit_breakers {
            cb.reset(session_id).await;
        }
        "🛑 Agent stopped. Use `!RESUME` to restart.".to_string()
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_safety_stop_all(ctx: &ReplyContext) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        // Halt all active scopes + a global scope marker
        failsafe
            .force_halt("__global__", "safety word: !STOP ALL")
            .await;
        for (scope, _) in failsafe.active_states().await {
            failsafe.force_halt(&scope, "safety word: !STOP ALL").await;
        }
        "🛑 EMERGENCY STOP — all agents halted. Use `!RESUME` to restart.".to_string()
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_safety_resume(ctx: &ReplyContext, session_id: &str) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        // Check if there's a global halt — need to resume that too
        let global_level = failsafe.get_level("__global__").await;
        failsafe.resume(session_id).await;

        if global_level != duduclaw_security::failsafe::FailsafeLevel::L0Normal {
            // Global halt is active — also resume it.
            // NOTE: requires_admin() should be checked by the channel handler
            // before calling this. We resume global here because chat_commands
            // is the only path where admin checks can happen.
            failsafe.resume("__global__").await;
            "✅ Agent resumed. Global halt also cleared.".to_string()
        } else {
            "✅ Agent resumed. Normal operation restored.".to_string()
        }
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_safety_status(ctx: &ReplyContext, session_id: &str) -> String {
    if let Some(ref failsafe) = ctx.failsafe {
        let state = failsafe.get_state(session_id).await;
        let scope_status = duduclaw_security::failsafe::format_status(session_id, state.as_ref());

        // Also check global halt
        let global_state = failsafe.get_state("__global__").await;
        let global_status = if global_state.is_some() {
            "\n\n⚠️ GLOBAL HALT is active.".to_string()
        } else {
            String::new()
        };

        // Circuit breaker status
        let cb_status = if let Some(ref cb) = ctx.circuit_breakers {
            let state = cb.state(session_id).await;
            let reason = cb.trip_reason(session_id).await;
            let reason_str = reason.map(|r| format!(" ({r})")).unwrap_or_default();
            format!("\n\nCircuit Breaker: {:?}{reason_str}", state)
        } else {
            String::new()
        };

        format!("🔒 *Safety Status*\n\n{scope_status}{global_status}{cb_status}")
    } else {
        "⚠️ Failsafe system not initialized.".to_string()
    }
}

async fn handle_replay(ctx: &ReplyContext, agent_id: &str, limit: u32) -> String {
    let audit = crate::screenshot_audit::BrowserAuditLog::new(&ctx.home_dir, 7);
    match audit.entries_for_agent(agent_id, limit as usize) {
        Ok(entries) if entries.is_empty() => "📭 沒有找到截圖記錄".to_string(),
        Ok(entries) => {
            let mut lines = vec![format!("📸 最近 {} 筆操作記錄：", entries.len())];
            for entry in &entries {
                let ts = entry.timestamp.format("%H:%M:%S");
                let ss = entry
                    .screenshot_path
                    .as_ref()
                    .map(|p| format!(" [截圖: {}]", p.display()))
                    .unwrap_or_default();
                lines.push(format!(
                    "• {ts} [{tier}] {action}{ss}",
                    tier = entry.tier,
                    action = entry.action,
                ));
            }
            lines.join("\n")
        }
        Err(e) => {
            warn!(error = %e, "Failed to read audit log");
            format!("⚠️ 讀取審計記錄失敗：{e}")
        }
    }
}

// ── /takeover — human takeover lifecycle (W3-1, pattern D3) ─────
//
// Deliberately parsed and dispatched separately from [`ChatCommand`]: every
// other chat command is intercepted per-channel *before* the reply pipeline,
// where the sender's channel account id is not carried (see `handle_pair`'s
// note). `/takeover` is an identity-bearing command — only a verified manager
// may extend or end a takeover — so it is handled at the one place that has
// both the conversation and the sender:
// `channel_reply::build_reply_with_session_inner`. That is also the only
// choke point that is guaranteed to be reached on every channel, including
// the ones whose per-channel interception drops unknown slash commands.

/// The three shapes `/takeover` takes (LINE OA's four-piece contract minus
/// "start", which needs no command — speaking is the start; see
/// [`crate::takeover`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeoverCommand {
    /// `/takeover` — who holds this conversation and until when.
    Status,
    /// `/takeover +30m` — push the deadline out by N minutes.
    Extend(i64),
    /// `/takeover end` — hand back to the AI now.
    End,
}

/// Parse `/takeover [...]`. Returns `None` for anything that is not this
/// command, so the caller falls through to its normal handling.
///
/// Accepted tails (case-insensitive, `＋`/`ｍ` full-width forms included since
/// the target users type on zh-TW IMEs):
/// - empty ⇒ [`TakeoverCommand::Status`]
/// - `end` / `結束` / `stop` ⇒ [`TakeoverCommand::End`]
/// - `+30m`, `+30`, `30m`, `30` ⇒ [`TakeoverCommand::Extend`]
/// - anything else ⇒ `Status` (which prints usage), never a silent no-op.
pub fn parse_takeover(text: &str) -> Option<TakeoverCommand> {
    let trimmed = text.trim();
    let rest = trimmed
        .strip_prefix('/')
        .or_else(|| trimmed.strip_prefix('／'))?;
    let (cmd, args) = match rest.find(char::is_whitespace) {
        Some(pos) => (&rest[..pos], rest[pos..].trim()),
        None => (rest, ""),
    };
    // Strip a Telegram-style `@BotName` suffix so `/takeover@bot end` works.
    let cmd = cmd.split('@').next().unwrap_or(cmd);
    if !cmd.eq_ignore_ascii_case("takeover") {
        return None;
    }
    if args.is_empty() {
        return Some(TakeoverCommand::Status);
    }
    let lowered = args.to_ascii_lowercase();
    if matches!(lowered.as_str(), "end" | "stop" | "off") || args == "結束" || args == "交還" {
        return Some(TakeoverCommand::End);
    }
    // `+30m` / `30m` / `30`, tolerating full-width plus and `m`.
    let digits: String = lowered
        .replace('＋', "+")
        .replace('ｍ', "m")
        .trim_start_matches('+')
        .trim_end_matches("min")
        .trim_end_matches('m')
        .trim()
        .to_string();
    match digits.parse::<i64>() {
        Ok(n) if n > 0 => Some(TakeoverCommand::Extend(n)),
        _ => Some(TakeoverCommand::Status),
    }
}

/// Execute a `/takeover` command. Every reply is end-user zh-TW; internal
/// vocabulary (conversation keys, agent ids, task ids) never appears.
///
/// Authorization is the same predicate typing-takeover uses
/// ([`crate::takeover::manager_display_name`]): an Active dashboard
/// Admin/Manager reached through a verified channel binding. A non-manager
/// gets a refusal, not a silent no-op — a person who typed a command deserves
/// to know why nothing happened.
pub async fn handle_takeover(
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    cmd: &TakeoverCommand,
) -> String {
    use duduclaw_core::takeover_state::{self, TakeoverConfig};

    let home = ctx.home_dir.as_path();
    let cfg = TakeoverConfig::from_home(home);
    if !cfg.enabled {
        return "此系統未啟用真人接手功能。".to_string();
    }
    let Some((channel, _)) = takeover_state::conversation_target(session_id) else {
        return "這個對話無法使用接手功能。".to_string();
    };
    let now = chrono::Utc::now();
    let holding = takeover_state::active_at(home, session_id, now);

    // Status is readable by anyone in the conversation: "is a human handling
    // this right now" is not privileged information, and hiding it would
    // recreate the black box this feature exists to open.
    if matches!(cmd, TakeoverCommand::Status) {
        return match holding {
            Some(rec) => format!(
                "👤 目前由 {} 接手中，還有約 {} 分鐘。\n輸入 `/takeover +30m` 延長、`/takeover end` 交還 AI。",
                rec.holder_display,
                rec.minutes_left(now).max(1)
            ),
            None => {
                let how = if crate::takeover::identity_available(home) {
                    "管理員只要在這個對話裡直接發言，就會自動接手。"
                } else {
                    "尚未有任何管理員在儀表板綁定此通道帳號，因此無法自動辨識接手者。"
                };
                format!("🤖 目前由 AI 回覆中，沒有人接手。\n{how}")
            }
        };
    }

    // Extend / end are writes — verified manager only.
    if crate::takeover::manager_display_name(home, &channel, user_id).is_none() {
        return "您沒有變更接手狀態的權限。請先於儀表板以此通道帳號完成綁定。".to_string();
    }
    let Some(rec) = holding else {
        return "目前沒有人接手這個對話，無需變更。".to_string();
    };

    match cmd {
        TakeoverCommand::Status => unreachable!("handled above"),
        TakeoverCommand::Extend(minutes) => {
            match takeover_state::extend(home, session_id, *minutes, &cfg, now) {
                Ok(Some(updated)) => format!(
                    "👤 已延長接手時間，還有約 {} 分鐘。",
                    updated.minutes_left(now).max(1)
                ),
                Ok(None) => "目前沒有人接手這個對話，無需變更。".to_string(),
                Err(e) => {
                    warn!(session_id, error = %e, "takeover: extend failed");
                    "⚠️ 延長失敗，請稍後再試。".to_string()
                }
            }
        }
        TakeoverCommand::End => match crate::takeover::end_takeover(home, session_id).await {
            Ok(Some(_)) => format!(
                "{}\n（{} 已結束接手）",
                crate::takeover::announce_resumed(),
                rec.holder_display
            ),
            Ok(None) => "目前沒有人接手這個對話，無需變更。".to_string(),
            Err(e) => {
                warn!(session_id, error = %e, "takeover: end failed");
                "⚠️ 結束接手失敗，請稍後再試。".to_string()
            }
        },
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_command() {
        assert!(is_command("/status"));
        assert!(is_command("/help"));
        assert!(is_command("  /status  "));
        assert!(is_command("/model claude-sonnet"));
        assert!(!is_command("hello"));
        assert!(!is_command("/ not a command"));
        assert!(!is_command("/123"));
        assert!(!is_command(""));
        assert!(!is_command("/"));
    }

    #[test]
    fn test_parse_status() {
        assert_eq!(parse_command("/status", None), Some(ChatCommand::Status));
        assert_eq!(parse_command("/STATUS", None), Some(ChatCommand::Status));
        assert_eq!(parse_command("/s", None), Some(ChatCommand::Status));
    }

    #[test]
    fn test_parse_new() {
        assert_eq!(parse_command("/new", None), Some(ChatCommand::New));
        assert_eq!(parse_command("/reset", None), Some(ChatCommand::New));
    }

    #[test]
    fn test_parse_usage() {
        assert_eq!(parse_command("/usage", None), Some(ChatCommand::Usage));
        assert_eq!(parse_command("/cost", None), Some(ChatCommand::Usage));
    }

    #[test]
    fn test_parse_model_with_arg() {
        assert_eq!(
            parse_command("/model claude-sonnet-4-6", None),
            Some(ChatCommand::Model(Some("claude-sonnet-4-6".to_string())))
        );
    }

    #[test]
    fn test_parse_model_no_arg() {
        assert_eq!(
            parse_command("/model", None),
            Some(ChatCommand::Model(None))
        );
    }

    #[test]
    fn test_parse_unknown() {
        assert_eq!(parse_command("/unknown", None), None);
        assert_eq!(parse_command("not a command", None), None);
    }

    #[test]
    fn test_parse_help() {
        assert_eq!(parse_command("/help", None), Some(ChatCommand::Help));
        assert_eq!(parse_command("/h", None), Some(ChatCommand::Help));
    }

    #[test]
    fn test_parse_compact() {
        assert_eq!(parse_command("/compact", None), Some(ChatCommand::Compact));
    }

    #[test]
    fn test_parse_computer_commands() {
        assert_eq!(
            parse_command("/screenshot", None),
            Some(ChatCommand::Screenshot)
        );
        assert_eq!(parse_command("/ss", None), Some(ChatCommand::Screenshot));
        assert_eq!(
            parse_command("/computer on", None),
            Some(ChatCommand::ComputerOn)
        );
        assert_eq!(
            parse_command("/computer off", None),
            Some(ChatCommand::ComputerOff)
        );
        assert_eq!(
            parse_command("/pause", None),
            Some(ChatCommand::ComputerPause)
        );
        assert_eq!(
            parse_command("/resume", None),
            Some(ChatCommand::ComputerResume)
        );
        assert_eq!(
            parse_command("/stop", None),
            Some(ChatCommand::ComputerStop)
        );
        assert_eq!(parse_command("/replay", None), Some(ChatCommand::Replay(5)));
        assert_eq!(
            parse_command("/replay 10", None),
            Some(ChatCommand::Replay(10))
        );
    }

    #[test]
    fn test_parse_session_portability_commands() {
        // /handoff — arg is lowercased; missing arg → None payload (usage reply)
        assert_eq!(
            parse_command("/handoff telegram", None),
            Some(ChatCommand::Handoff(Some("telegram".to_string())))
        );
        assert_eq!(
            parse_command("/handoff Slack", None),
            Some(ChatCommand::Handoff(Some("slack".to_string())))
        );
        assert_eq!(
            parse_command("/handoff", None),
            Some(ChatCommand::Handoff(None))
        );

        // /undo — default 1, explicit N passes through (validated in handler)
        assert_eq!(parse_command("/undo", None), Some(ChatCommand::Undo(1)));
        assert_eq!(parse_command("/undo 3", None), Some(ChatCommand::Undo(3)));
        assert_eq!(parse_command("/undo abc", None), Some(ChatCommand::Undo(1)));

        assert_eq!(
            parse_command("/rollback", None),
            Some(ChatCommand::Rollback)
        );

        // Non-admin commands, same gating as peers (/new, /compact).
        assert!(!ChatCommand::Handoff(Some("telegram".into())).requires_admin());
        assert!(!ChatCommand::Undo(1).requires_admin());
        assert!(!ChatCommand::Rollback.requires_admin());
    }

    #[test]
    fn test_parse_goal() {
        // Basic: whole tail is the description, no explicit criteria.
        assert_eq!(
            parse_command("/goal 整理客戶資料成報表", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "整理客戶資料成報表".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // Two-part: `||` separates description and acceptance criteria.
        assert_eq!(
            parse_command("/goal 做月報 || 含營收圖表並寄出", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: Some("含營收圖表並寄出".to_string()),
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // Empty right side of `||` → treated as no explicit criteria.
        assert_eq!(
            parse_command("/goal 做月報 ||", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // WP2.4 three-part: description || criteria || outcome:<spec>.
        assert_eq!(
            parse_command(
                "/goal 做月報 || 含營收圖表 || outcome:files:report.docx",
                None
            ),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: Some("含營收圖表".to_string()),
                outcome: Some("files:report.docx".to_string()),
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // Outcome may follow the description directly (criteria omitted).
        assert_eq!(
            parse_command("/goal 做月報 || outcome:text", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: None,
                outcome: Some("text".to_string()),
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // A `||` inside a JSON schema string survives (payload kept whole).
        assert_eq!(
            parse_command(
                r#"/goal 產出設定 || outcome:json:{"type":"object","required":["a||b"]}"#,
                None
            ),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "產出設定".to_string(),
                acceptance_criteria: None,
                outcome: Some(r#"json:{"type":"object","required":["a||b"]}"#.to_string()),
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // Goal contract v2: duration + boundary segments, position-independent.
        assert_eq!(
            parse_command("/goal 做月報 || 時限:36h || 邊界:不得寄給客戶草稿 || 含營收圖表", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: Some("含營收圖表".to_string()),
                outcome: None,
                duration_hours: Some(36.0),
                risk_boundary: Some("不得寄給客戶草稿".to_string()),
                plan_first: false,
            }))
        );
        // Days multiply ×24; ASCII `duration:`/`risk:` spellings accepted.
        assert_eq!(
            parse_command("/goal 做月報 || duration:2天 || risk:no external email", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: Some(48.0),
                risk_boundary: Some("no external email".to_string()),
                plan_first: false,
            }))
        );
        // P1: standalone `想一想` segment opts into plan-first mode.
        assert_eq!(
            parse_command("/goal 做月報 || 想一想", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: true,
            }))
        );
        // Co-exists with an explicit acceptance-criteria segment.
        assert_eq!(
            parse_command("/goal 做月報 || 含營收圖表並寄出 || 想一想", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: Some("含營收圖表並寄出".to_string()),
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: true,
            }))
        );
        // ASCII `plan` spelling, case-insensitive.
        assert_eq!(
            parse_command("/goal 做月報 || PLAN", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: true,
            }))
        );
        // Position-independent: the plan-first token may appear BEFORE the
        // description segment — the first remaining plain segment still
        // becomes the description, matching `時限:`/`邊界:`'s own
        // position-independence.
        assert_eq!(
            parse_command("/goal 想一想 || 做月報 || 含營收圖表", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: Some("含營收圖表".to_string()),
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: true,
            }))
        );
        // Co-exists with duration/boundary segments too.
        assert_eq!(
            parse_command("/goal 做月報 || 時限:36h || 想一想 || 邊界:不得寄給客戶草稿", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "做月報".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: Some(36.0),
                risk_boundary: Some("不得寄給客戶草稿".to_string()),
                plan_first: true,
            }))
        );
        // A description that merely CONTAINS "想一想" (not a standalone
        // segment) is plain text, not the plan-first token.
        assert_eq!(
            parse_command("/goal 想一想這個活動怎麼辦比較好", None),
            Some(ChatCommand::Goal(GoalCommand::Create {
                description: "想一想這個活動怎麼辦比較好".to_string(),
                acceptance_criteria: None,
                outcome: None,
                duration_hours: None,
                risk_boundary: None,
                plan_first: false,
            }))
        );

        // Malformed / out-of-range duration fails closed to usage, never
        // silently becoming part of the description.
        assert_eq!(
            parse_command("/goal 做月報 || 時限:abc", None),
            Some(ChatCommand::Goal(GoalCommand::Usage))
        );
        assert_eq!(
            parse_command("/goal 做月報 || 時限:9999h", None),
            Some(ChatCommand::Goal(GoalCommand::Usage))
        );

        // status subcommand (case-insensitive).
        assert_eq!(
            parse_command("/goal status", None),
            Some(ChatCommand::Goal(GoalCommand::Status))
        );
        assert_eq!(
            parse_command("/goal STATUS", None),
            Some(ChatCommand::Goal(GoalCommand::Status))
        );

        // Empty args → usage.
        assert_eq!(
            parse_command("/goal", None),
            Some(ChatCommand::Goal(GoalCommand::Usage))
        );
        assert_eq!(
            parse_command("/goal   ", None),
            Some(ChatCommand::Goal(GoalCommand::Usage))
        );

        // Not an admin-gated command.
        assert!(!ChatCommand::Goal(GoalCommand::Usage).requires_admin());
    }

    #[test]
    fn test_source_from_session() {
        assert_eq!(
            source_from_session("telegram:12345"),
            ("telegram".to_string(), "12345".to_string())
        );
        // Extra segments (thread id) are dropped — chat_id is the 2nd segment.
        assert_eq!(
            source_from_session("telegram:12345:678"),
            ("telegram".to_string(), "12345".to_string())
        );
        // No colon → empty channel, whole string as chat_id.
        assert_eq!(
            source_from_session("solo"),
            (String::new(), "solo".to_string())
        );
    }

    #[test]
    fn test_parse_safety_words() {
        assert_eq!(parse_command("!STOP", None), Some(ChatCommand::SafetyStop));
        assert_eq!(
            parse_command("!STOP ALL", None),
            Some(ChatCommand::SafetyStopAll)
        );
        assert_eq!(
            parse_command("!RESUME", None),
            Some(ChatCommand::SafetyResume)
        );
        assert_eq!(
            parse_command("!STATUS", None),
            Some(ChatCommand::SafetyStatus)
        );
        assert_eq!(parse_command("!停止", None), Some(ChatCommand::SafetyStop));
    }
}

#[cfg(test)]
mod model_switch_tests {
    use super::*;

    /// `/model <name>` writes agent.toml and changes what every turn costs, so
    /// it must not be usable by an arbitrary group member. Bare `/model` only
    /// reports, and stays open.
    #[test]
    fn setting_a_model_requires_admin_but_reading_does_not() {
        assert!(ChatCommand::Model(Some("claude-opus-5".into())).requires_admin());
        assert!(!ChatCommand::Model(None).requires_admin());
    }

    #[test]
    fn validate_model_name_accepts_real_vendor_slugs() {
        for ok in [
            "claude-opus-5",
            "claude-sonnet-4-6",
            "gpt-5.1",
            "deepseek-chat",
            "qwen/qwen3-32b",
            "claude-opus-5[1m]".trim_end_matches(['[', '1', 'm', ']']),
        ] {
            assert!(validate_model_name(ok).is_ok(), "rejected {ok}");
        }
    }

    #[test]
    fn validate_model_name_rejects_argument_and_shell_shapes() {
        // The value ends up as a CLI argument and inside a config file.
        for bad in [
            "",
            "--dangerously-skip-permissions",
            "model name",
            "model;rm -rf /",
            "$(whoami)",
            "model\nprefer = \"x\"",
            "../../etc/passwd\0",
        ] {
            assert!(validate_model_name(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn validate_model_name_rejects_an_over_long_value() {
        assert!(validate_model_name(&"a".repeat(65)).is_err());
        assert!(validate_model_name(&"a".repeat(64)).is_ok());
    }
}

// ── /rules — 經驗法則 (W3-2, D-O3 = A) ───────────────────────────────
//
// Self-evolution is this product's headline claim, and until now the rules an
// AI employee had learned were visible only in the dashboard. `/rules` puts
// them where the user already is, at zero LLM cost (the whole command is a
// SQLite read plus `playbook::humanize` template assembly).
//
// Everything below is deliberately at the file tail to keep the merge surface
// with other in-flight command work down to the three one-line hooks above.

/// Default channel summary size (D.14: 通道前 3 條摘要).
const RULES_SUMMARY_LIMIT: usize = 3;
/// `/rules all` hard cap — a channel message is not a dashboard; past this we
/// say how many were withheld rather than flooding the chat.
const RULES_FULL_LIMIT: usize = 15;

/// Status keys shown by the bare `/rules` summary: the rules that are actually
/// steering behaviour right now.
const RULES_IN_EFFECT: &[&str] = &["active", "trial"];
/// Additional status keys `/rules all` surfaces. `retired` (已淘汰) is
/// deliberately absent from both — a dead rule is dashboard history, not news.
const RULES_ALL_EXTRA: &[&str] = &["observing", "dormant"];

/// One rule prepared for channel rendering.
struct RuleLine {
    status: String,
    sentence: String,
    streak: u32,
    helpful: u32,
    harmful: u32,
    applications: usize,
}

/// Deterministic, zero-LLM channel rendering of an agent's experience rules.
///
/// Pure over its input so the wording is unit-testable without a database;
/// [`handle_rules`] is only the I/O around it.
fn render_rules_message(lines: &[RuleLine], total_in_effect: usize, all: bool) -> String {
    if lines.is_empty() {
        return if all {
            "📘 這位 AI 員工還沒有累積任何經驗法則。等它做過幾次任務、從結果裡歸納出原則後，這裡就會有內容。"
                .to_string()
        } else {
            "📘 目前沒有生效中的經驗法則。輸入 `/rules all` 可以看還在觀察中的項目。".to_string()
        };
    }

    let header = if all {
        format!("📘 *經驗法則*（共 {} 條）", lines.len())
    } else {
        format!(
            "📘 *經驗法則*（生效中 {total_in_effect} 條，以下列出前 {}）",
            lines.len()
        )
    };

    let mut out = vec![header, String::new()];
    for (i, l) in lines.iter().enumerate() {
        out.push(format!("{}. 〔{}〕{}", i + 1, l.status, l.sentence));
        let used = l.applications.max((l.helpful + l.harmful) as usize);
        let mut tail = if used == 0 {
            "還沒有實際使用紀錄".to_string()
        } else {
            format!("用過 {used} 次，{} 次有幫助、{} 次幫倒忙", l.helpful, l.harmful)
        };
        if l.streak > 0 {
            tail.push_str(&format!("；連續 {} 次沒出問題", l.streak));
        }
        out.push(format!("   ↳ {tail}"));
    }

    out.push(String::new());
    if all {
        out.push(
            "要停用某一條，輸入 `/rules off <上面的編號>`（限管理員／主管）；或到 儀表板 → 記憶 → 自主進化。"
                .to_string(),
        );
    } else {
        out.push(
            "輸入 `/rules all` 看全部＋編號；要停用某一條，先看 `/rules all` 的編號再輸入 `/rules off <編號>`（限管理員／主管）。"
                .to_string(),
        );
    }
    out.join("\n")
}

/// Result of [`ranked_rule_lines`]: entries already filtered, sorted, and
/// truncated the way `/rules`/`/rules all` render them, plus the counts
/// needed for the header/footer text.
struct RankedRules {
    /// `(playbook entry id, rendered line)`, in display order — position `i`
    /// here is exactly "line `i + 1`" in the rendered message, and exactly
    /// what `/rules off <n>` (W3-5) resolves `n` against (see
    /// [`handle_rules_off`]). Carrying the id alongside the line means the
    /// two commands share one query/sort instead of risking two independent
    /// ones silently drifting apart.
    entries: Vec<(String, RuleLine)>,
    in_effect: usize,
    /// Count before the display limit was applied — `entries.len()` alone
    /// can't tell the caller whether anything was truncated.
    total: usize,
}

/// Rank + filter an agent's playbook entries exactly the way `/rules`
/// renders them: `all=false` → only the top [`RULES_SUMMARY_LIMIT`]
/// currently-in-effect entries; `all=true` → up to [`RULES_FULL_LIMIT`]
/// entries across the wider `RULES_IN_EFFECT ∪ RULES_ALL_EXTRA` visibility
/// set. Sorted by (net score, then streak) — the same order the injection
/// path ranks by, so what the user reads first is what the model actually
/// sees first.
async fn ranked_rule_lines(
    engine: &duduclaw_memory::SqliteMemoryEngine,
    agent_id: &str,
    all: bool,
) -> RankedRules {
    let mut prepared: Vec<(i64, u32, String, RuleLine)> = Vec::new();
    let mut in_effect = 0usize;
    for (mem, meta, stats) in crate::playbook::list_active(engine, agent_id).await {
        let h = crate::playbook::humanize(&mem.content, &meta, &stats, &mem.tags);
        let visible = RULES_IN_EFFECT.contains(&h.status_key)
            || (all && RULES_ALL_EXTRA.contains(&h.status_key));
        if RULES_IN_EFFECT.contains(&h.status_key) {
            in_effect += 1;
        }
        if !visible {
            continue;
        }
        prepared.push((
            stats.net(),
            meta.success_streak,
            mem.id.clone(),
            RuleLine {
                status: h.status.clone(),
                sentence: h.sentence.clone(),
                streak: h.evidence.success_streak,
                helpful: h.evidence.helpful,
                harmful: h.evidence.harmful,
                applications: h.evidence.applications,
            },
        ));
    }

    prepared.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let limit = if all { RULES_FULL_LIMIT } else { RULES_SUMMARY_LIMIT };
    let total = prepared.len();
    let entries = prepared.into_iter().take(limit).map(|(_, _, id, l)| (id, l)).collect();
    RankedRules { entries, in_effect, total }
}

async fn handle_rules(ctx: &ReplyContext, agent_id: &str, all: bool) -> String {
    let Some(db_path) = ctx.memory_db_path.clone() else {
        return "📘 這位 AI 員工還沒有開啟記憶功能，所以還不會累積經驗法則。".to_string();
    };
    if !db_path.exists() {
        return "📘 這位 AI 員工還沒有累積任何經驗法則。".to_string();
    }
    let engine = match duduclaw_memory::SqliteMemoryEngine::new(&db_path) {
        Ok(e) => e,
        Err(e) => {
            warn!(agent = agent_id, error = %e, "/rules: failed to open memory db");
            return "⚠️ 暫時讀不到經驗法則，請稍後再試。".to_string();
        }
    };

    let ranked = ranked_rule_lines(&engine, agent_id, all).await;
    let shown = ranked.entries.len();
    let lines: Vec<RuleLine> = ranked.entries.into_iter().map(|(_, l)| l).collect();

    let mut msg = render_rules_message(&lines, ranked.in_effect, all);
    if all && ranked.total > shown {
        msg.push_str(&format!("\n（另有 {} 條未列出，完整清單請看儀表板。）", ranked.total - shown));
    }
    msg
}

/// `/rules off <n>` (W3-5): disable the rule at position `n` in the SAME
/// numbering `/rules all` shows ([`ranked_rule_lines`] with `all=true` — one
/// shared query/sort, so the two commands can never disagree about what
/// "#n" means). Retires through the existing manual-retire path
/// (`playbook::PlaybookDelta::Retire` + `apply_deltas`, the same one
/// `handlers.rs`'s `playbook.retire` dashboard RPC uses), so an in-channel
/// disable is auditable exactly like a dashboard one, and records one
/// Activity Feed row (`playbook_rule_retired`, matching the dashboard path's
/// event shape).
///
/// ## Authorization
///
/// Reuses [`crate::takeover::manager_display_name`] — the SAME
/// identity-verified Admin/Manager predicate typing-takeover (W3-1) and
/// `/takeover` use — NOT the looser per-channel `admin_users` list every
/// other command's `is_admin` gates on. Disabling a rule is a persistent
/// write to the agent's playbook (it stops steering every future turn), not
/// a read; a deployment/channel with no dashboard identity bound to the
/// sender's account refuses outright and points at the dashboard, rather
/// than silently falling back to the weaker gate.
async fn handle_rules_off(
    ctx: &ReplyContext,
    session_id: &str,
    channel_user_id: &str,
    agent_id: &str,
    n: u32,
) -> String {
    let home = ctx.home_dir.as_path();

    if !crate::takeover::identity_available(home) {
        return "⚠️ 這個部署還沒有設定任何管理員身分綁定，無法在通道內停用經驗法則。請改用 儀表板 → 記憶 → 自主進化。"
            .to_string();
    }
    let Some((channel, _)) = duduclaw_core::takeover_state::conversation_target(session_id) else {
        return "⚠️ 這個對話無法使用此指令。".to_string();
    };
    if crate::takeover::manager_display_name(home, &channel, channel_user_id).is_none() {
        return "⚠️ 您沒有停用經驗法則的權限，請先於儀表板以此通道帳號完成管理員／主管綁定。".to_string();
    }

    let Some(db_path) = ctx.memory_db_path.clone() else {
        return "📘 這位 AI 員工還沒有開啟記憶功能，沒有經驗法則可以停用。".to_string();
    };
    if !db_path.exists() {
        return "📘 這位 AI 員工還沒有累積任何經驗法則。".to_string();
    }
    let engine = match duduclaw_memory::SqliteMemoryEngine::new(&db_path) {
        Ok(e) => e,
        Err(e) => {
            warn!(agent = agent_id, error = %e, "/rules off: failed to open memory db");
            return "⚠️ 暫時讀不到經驗法則，請稍後再試。".to_string();
        }
    };

    let ranked = ranked_rule_lines(&engine, agent_id, true).await;
    let Some((entry_id, line)) = resolve_rule_ordinal(&ranked.entries, n) else {
        return format!(
            "⚠️ 找不到編號 {n} 的規則。請先輸入 `/rules all` 確認目前的編號（目前共 {} 條）。",
            ranked.entries.len()
        );
    };

    let eval_cases_root = home.join("agents").join(agent_id).join("evals");
    let delta = crate::playbook::PlaybookDelta::Retire {
        id: entry_id.clone(),
        reason: format!("channel /rules off (by {channel_user_id})"),
    };
    let outcome = crate::playbook::apply_deltas(
        &engine,
        agent_id,
        vec![delta],
        &[],
        &eval_cases_root,
        chrono::Utc::now(),
    )
    .await;

    if let Some((_, reason)) = outcome.rejected.first() {
        warn!(agent_id, n, reason, "/rules off: retire rejected");
        return "⚠️ 停用失敗，請稍後再試或改用儀表板。".to_string();
    }
    let retired = outcome
        .applied
        .iter()
        .any(|op| matches!(op, crate::playbook::AppliedOp::Retired { .. }));
    if !retired {
        return "這條規則似乎已經停用了，無需重複操作。".to_string();
    }

    // Activity Feed — same event shape `playbook.retire`'s dashboard RPC
    // uses, so an in-channel disable shows up next to a dashboard one rather
    // than being invisible outside this one chat.
    if let Ok(store) = crate::task_store::TaskStore::open(home) {
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "playbook_rule_retired".to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: format!("管理者透過通道停用了 AI 員工「{agent_id}」的一條經驗法則"),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: None,
        };
        if let Err(e) = store.append_activity(&row).await {
            tracing::debug!(error = %e, "/rules off: activity append failed (non-fatal)");
        }
    }

    format!("✅ 已停用第 {n} 條規則：{}", line.sentence)
}

/// Resolve a 1-based `/rules off <n>` ordinal against the SAME slice
/// `/rules all` rendered, or `None` for `0` / out of range. Pure so the
/// off-by-one/bounds edge cases are unit-testable without a database.
fn resolve_rule_ordinal(entries: &[(String, RuleLine)], n: u32) -> Option<&(String, RuleLine)> {
    let idx = usize::try_from(n).ok()?.checked_sub(1)?;
    entries.get(idx)
}

#[cfg(test)]
mod rules_command_tests {
    use super::*;

    fn line(status: &str, sentence: &str, helpful: u32, harmful: u32, streak: u32) -> RuleLine {
        RuleLine {
            status: status.to_string(),
            sentence: sentence.to_string(),
            streak,
            helpful,
            harmful,
            applications: (helpful + harmful) as usize,
        }
    }

    #[test]
    fn parses_bare_and_all_forms() {
        assert_eq!(parse_command("/rules", None), Some(ChatCommand::Rules { all: false, off: None }));
        assert_eq!(parse_command("/rules all", None), Some(ChatCommand::Rules { all: true, off: None }));
        assert_eq!(parse_command("/RULES ALL", None), Some(ChatCommand::Rules { all: true, off: None }));
        // A mistyped tail degrades to the summary rather than erroring out.
        assert_eq!(
            parse_command("/rules everything", None),
            Some(ChatCommand::Rules { all: false, off: None })
        );
    }

    #[test]
    fn parses_off_forms() {
        assert_eq!(
            parse_command("/rules off 3", None),
            Some(ChatCommand::Rules { all: false, off: Some(3) })
        );
        assert_eq!(
            parse_command("/rules OFF 12", None),
            Some(ChatCommand::Rules { all: false, off: Some(12) })
        );
        // Extra whitespace tolerated.
        assert_eq!(
            parse_command("/rules   off   3  ", None),
            Some(ChatCommand::Rules { all: false, off: Some(3) })
        );
    }

    #[test]
    fn off_with_a_missing_or_non_numeric_index_is_recognized_not_silently_ignored() {
        // `off` alone or with garbage still parses as an `off` command (with
        // a deliberately out-of-range 0) rather than falling back to the
        // bare/all summary — a person who typed `/rules off` deserves
        // "which number?", not a listing that looks like `off` vanished.
        assert_eq!(parse_command("/rules off", None), Some(ChatCommand::Rules { all: false, off: Some(0) }));
        assert_eq!(
            parse_command("/rules off abc", None),
            Some(ChatCommand::Rules { all: false, off: Some(0) })
        );
    }

    #[test]
    fn rules_is_readonly_so_it_never_demands_the_weak_admin_gate() {
        // `requires_admin()` gates the looser per-channel `admin_users`
        // check; `/rules off` uses the STRONGER identity-verified
        // `takeover::manager_display_name` predicate instead (enforced
        // inside `handle_rules_off`), so it must stay `false` here too —
        // otherwise a channel with no admin_users configured at all would
        // refuse the command before ever reaching the real gate.
        assert!(!ChatCommand::Rules { all: false, off: None }.requires_admin());
        assert!(!ChatCommand::Rules { all: true, off: None }.requires_admin());
        assert!(!ChatCommand::Rules { all: false, off: Some(1) }.requires_admin());
    }

    #[test]
    fn empty_state_differs_between_summary_and_full_list() {
        let summary = render_rules_message(&[], 0, false);
        assert!(summary.contains("/rules all"), "{summary}");
        let full = render_rules_message(&[], 0, true);
        assert!(!full.contains("/rules all"), "{full}");
        assert!(full.contains("還沒有累積"), "{full}");
    }

    #[test]
    fn summary_numbers_rules_and_reports_the_in_effect_total() {
        let lines = vec![
            line("生效中", "當提到「訂單」時，我會先查資料庫再回覆。", 4, 0, 4),
            line("試用中", "當明顯出錯時，我會先說明原因。", 1, 0, 1),
        ];
        let msg = render_rules_message(&lines, 5, false);
        assert!(msg.contains("生效中 5 條"), "{msg}");
        assert!(msg.contains("1. 〔生效中〕"), "{msg}");
        assert!(msg.contains("2. 〔試用中〕"), "{msg}");
        assert!(msg.contains("用過 4 次，4 次有幫助、0 次幫倒忙"), "{msg}");
        assert!(msg.contains("連續 4 次沒出問題"), "{msg}");
    }

    #[test]
    fn an_unused_rule_says_so_instead_of_printing_a_fake_zero_record() {
        let msg = render_rules_message(&[line("試用中", "我會先問清楚。", 0, 0, 0)], 1, false);
        assert!(msg.contains("還沒有實際使用紀錄"), "{msg}");
        assert!(!msg.contains("用過 0 次"), "{msg}");
    }

    #[test]
    fn channel_copy_never_leaks_internal_vocabulary() {
        let msg = render_rules_message(
            &[line("觀察中（尚未生效）", "當踩到安全界線時，我會先徵求同意。", 0, 0, 0)],
            0,
            true,
        );
        for internal in ["playbook", "Playbook", "shadow", "probation", "GVU", "AEE", "SOUL"] {
            assert!(!msg.contains(internal), "leaked `{internal}`: {msg}");
        }
        // And the empty states too.
        for msg in [render_rules_message(&[], 0, false), render_rules_message(&[], 0, true)] {
            for internal in ["playbook", "shadow", "probation", "GVU"] {
                assert!(!msg.contains(internal), "leaked `{internal}`: {msg}");
            }
        }
    }

    #[test]
    fn help_advertises_the_command() {
        let help = handle_help();
        assert!(help.contains("/rules"), "{help}");
        assert!(help.contains("經驗法則"), "{help}");
    }

    // ── W3-5 `/rules off <n>` — id/ordinal pairing correctness ─────────
    // The property that actually matters for "不會停錯條": whatever
    // `ranked_rule_lines` renders as line `n` is the exact entry
    // `resolve_rule_ordinal(_, n)` returns, and retiring that id touches
    // only that one row.

    fn temp_rules_eval_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let suite = dir.path().join("s");
        std::fs::create_dir(&suite).unwrap();
        std::fs::write(
            suite.join("c.toml"),
            "[case]\nname = \"c\"\nagent = \"a\"\nprompt = \"hi\"\n[judge]\nrubric = \"r\"\n",
        )
        .unwrap();
        dir
    }

    fn rule_add_delta(content: &str) -> crate::playbook::PlaybookDelta {
        crate::playbook::PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions {
                output_contains: vec!["ok".to_string()],
                ..Default::default()
            },
            content: content.to_string(),
            category: crate::playbook::PlaybookCategory::Repair,
            signals_match: vec!["*".to_string()],
            eval_cases: vec![crate::playbook::EvalCaseRef("s/c".to_string())],
            strategy: Vec::new(),
            rationale: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn ranked_rule_lines_gives_every_entry_a_distinct_id() {
        let engine = duduclaw_memory::SqliteMemoryEngine::in_memory().unwrap();
        let evals = temp_rules_eval_root();
        let agent = "agent-rules-off";
        let now = chrono::Utc::now();

        let deltas = vec![rule_add_delta("rule about refunds"), rule_add_delta("rule about discord")];
        let outcome =
            crate::playbook::apply_deltas(&engine, agent, deltas, &[], evals.path(), now).await;
        assert_eq!(outcome.applied.len(), 2, "{outcome:?}");

        let ranked = ranked_rule_lines(&engine, agent, true).await;
        assert_eq!(ranked.entries.len(), 2);
        assert_eq!(ranked.total, 2);
        let ids: std::collections::HashSet<&str> =
            ranked.entries.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.len(), 2, "ids must not collide");
    }

    #[tokio::test]
    async fn retiring_the_ordinal_resolved_id_touches_only_that_entry() {
        let engine = duduclaw_memory::SqliteMemoryEngine::in_memory().unwrap();
        let evals = temp_rules_eval_root();
        let agent = "agent-rules-off-2";
        let now = chrono::Utc::now();

        let deltas = vec![rule_add_delta("keep me"), rule_add_delta("retire me")];
        crate::playbook::apply_deltas(&engine, agent, deltas, &[], evals.path(), now).await;

        let before = ranked_rule_lines(&engine, agent, true).await;
        assert_eq!(before.entries.len(), 2);
        let (target_id, _) = resolve_rule_ordinal(&before.entries, 2).expect("ordinal 2 must resolve");

        let retire = crate::playbook::PlaybookDelta::Retire { id: target_id.clone(), reason: "test".into() };
        let outcome =
            crate::playbook::apply_deltas(&engine, agent, vec![retire], &[], evals.path(), now).await;
        assert!(outcome.rejected.is_empty(), "{outcome:?}");

        let after = ranked_rule_lines(&engine, agent, true).await;
        assert_eq!(after.entries.len(), 1, "only the resolved id should be gone");
        assert!(
            after.entries.iter().all(|(id, _)| id != target_id),
            "the retired id must not resurface"
        );
    }

    #[test]
    fn resolve_rule_ordinal_rejects_zero_and_out_of_range() {
        let entries = vec![
            ("id-1".to_string(), line("生效中", "first", 0, 0, 0)),
            ("id-2".to_string(), line("生效中", "second", 0, 0, 0)),
        ];
        assert!(resolve_rule_ordinal(&entries, 0).is_none());
        assert_eq!(resolve_rule_ordinal(&entries, 1).unwrap().0, "id-1");
        assert_eq!(resolve_rule_ordinal(&entries, 2).unwrap().0, "id-2");
        assert!(resolve_rule_ordinal(&entries, 3).is_none());
        assert!(resolve_rule_ordinal(&entries, u32::MAX).is_none());
    }
}

#[cfg(test)]
mod takeover_command_tests {
    use super::*;

    // ── W3-1 `/takeover` (D3) ───────────────────────────────────

    #[test]
    fn takeover_parses_its_three_shapes() {
        use TakeoverCommand::*;
        assert_eq!(parse_takeover("/takeover"), Some(Status));
        assert_eq!(parse_takeover("  /takeover  "), Some(Status));
        assert_eq!(parse_takeover("/TAKEOVER"), Some(Status));
        assert_eq!(parse_takeover("/takeover@DuDuBot"), Some(Status));

        assert_eq!(parse_takeover("/takeover end"), Some(End));
        assert_eq!(parse_takeover("/takeover END"), Some(End));
        assert_eq!(parse_takeover("/takeover 結束"), Some(End));
        assert_eq!(parse_takeover("/takeover@DuDuBot end"), Some(End));

        assert_eq!(parse_takeover("/takeover +30m"), Some(Extend(30)));
        assert_eq!(parse_takeover("/takeover +30"), Some(Extend(30)));
        assert_eq!(parse_takeover("/takeover 30m"), Some(Extend(30)));
        assert_eq!(parse_takeover("/takeover +45min"), Some(Extend(45)));
        // Full-width plus / m from a zh-TW IME.
        assert_eq!(parse_takeover("/takeover ＋30ｍ"), Some(Extend(30)));
    }

    #[test]
    fn takeover_never_silently_ignores_a_malformed_tail() {
        // Anything unparseable falls back to Status, which prints usage —
        // a person who typed a command must never get silence.
        assert_eq!(parse_takeover("/takeover wat"), Some(TakeoverCommand::Status));
        assert_eq!(parse_takeover("/takeover -5"), Some(TakeoverCommand::Status));
        assert_eq!(parse_takeover("/takeover +0m"), Some(TakeoverCommand::Status));
    }

    #[test]
    fn takeover_does_not_capture_other_input() {
        assert_eq!(parse_takeover("/status"), None);
        assert_eq!(parse_takeover("/take over"), None);
        assert_eq!(parse_takeover("takeover"), None);
        assert_eq!(parse_takeover("我要 /takeover"), None);
        assert_eq!(parse_takeover(""), None);
        // `/takeovers` must not match a prefix.
        assert_eq!(parse_takeover("/takeovers"), None);
    }

    #[test]
    fn takeover_is_not_a_legacy_chat_command() {
        // It is dispatched from the reply pipeline, not the per-channel
        // interceptor — `parse_command` must leave it alone so channels that
        // fall through on an unknown slash command reach that pipeline.
        assert_eq!(parse_command("/takeover", None), None);
        assert_eq!(parse_command("/takeover end", None), None);
    }

    #[test]
    fn help_advertises_takeover() {
        let help = handle_help();
        assert!(help.contains("/takeover"), "{help}");
        // No internal vocabulary in a user-facing help screen.
        for internal in ["takeover_state", "claimed_by", "session_id", "needs_human"] {
            assert!(!help.contains(internal), "leaked `{internal}`: {help}");
        }
    }
}

/// H9-W / H9-G (harness-borrowings 2026-08 WP-D) — `/goal` four-element
/// guidance and immutable acceptance-criteria baseline freeze.
#[cfg(test)]
mod goal_contract_tests {
    use super::*;

    fn test_ctx(home: &std::path::Path) -> ReplyContext {
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::AgentRegistry::new(home.join("agents")),
        ));
        let sessions = std::sync::Arc::new(
            crate::session::SessionManager::new(&home.join("sessions.db")).unwrap(),
        );
        let status: crate::channel_reply::ChannelStatusMap =
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
    }

    #[tokio::test]
    async fn four_element_guidance_appears_when_no_explicit_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let msg = handle_goal_create(
            &ctx,
            "telegram:123:abc",
            "agent-a",
            "整理客戶資料成報表",
            None,
            None,
            None,
            None,
            false,
        )
        .await;
        assert!(msg.contains("目標："), "{msg}");
        assert!(msg.contains("輸入："), "{msg}");
        assert!(msg.contains("輸出格式："), "{msg}");
        assert!(msg.contains("約束："), "{msg}");
        assert!(msg.contains("3-5"), "{msg}");
        // No internal vocabulary leaks into the user-facing reply.
        for internal in ["acceptance_criteria", "baseline", "MAV", "judge", "column"] {
            assert!(!msg.contains(internal), "leaked `{internal}`: {msg}");
        }
    }

    #[tokio::test]
    async fn no_guidance_when_explicit_criteria_given() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let msg = handle_goal_create(
            &ctx,
            "telegram:123:abc",
            "agent-a",
            "整理客戶資料成報表",
            Some("含營收圖表並寄出"),
            None,
            None,
            None,
            false,
        )
        .await;
        assert!(
            !msg.contains("這次沒有另外指定驗收標準"),
            "guidance must not appear when criteria was explicitly given: {msg}"
        );
    }

    #[tokio::test]
    async fn goal_create_freezes_an_immutable_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let _msg = handle_goal_create(
            &ctx,
            "telegram:123:abc",
            "agent-a",
            "整理客戶資料成報表",
            Some("含營收圖表並寄出"),
            None,
            None,
            None,
            false,
        )
        .await;

        let store = crate::task_store::TaskStore::open(dir.path()).unwrap();
        let tasks = store
            .list_tasks_filtered(None, Some("agent-a"), None, Some(true))
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        let task = &tasks[0];
        assert_eq!(task.acceptance_criteria.as_deref(), Some("含營收圖表並寄出"));
        assert_eq!(
            task.acceptance_criteria_baseline.as_deref(),
            Some("含營收圖表並寄出"),
            "baseline must be frozen to the same value at creation"
        );
    }

    #[tokio::test]
    async fn goal_create_without_explicit_criteria_still_freezes_the_goal_text_as_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let _msg = handle_goal_create(
            &ctx,
            "telegram:123:abc",
            "agent-b",
            "整理客戶資料成報表",
            None,
            None,
            None,
            None,
            false,
        )
        .await;

        let store = crate::task_store::TaskStore::open(dir.path()).unwrap();
        let tasks = store
            .list_tasks_filtered(None, Some("agent-b"), None, Some(true))
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        let task = &tasks[0];
        assert_eq!(
            task.acceptance_criteria.as_deref(),
            Some("整理客戶資料成報表")
        );
        assert_eq!(
            task.acceptance_criteria_baseline.as_deref(),
            Some("整理客戶資料成報表")
        );
    }
}
