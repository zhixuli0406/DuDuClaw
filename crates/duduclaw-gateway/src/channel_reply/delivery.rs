use super::*;

/// Read the default_agent from config.toml [general] section.
pub(super) async fn get_default_agent(home_dir: &Path) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    let general = table.get("general")?.as_table()?;
    let name = general.get("default_agent")?.as_str()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// WP1.3: resolve the agent directory that a channel turn's `📎DELIVER:` paths
/// must live under (the trusted sandbox root for path validation).
///
/// `explicit_agent` is the per-bot / user-bound agent when the channel knows
/// it; otherwise the configured `[general] default_agent` is used, falling back
/// to the registry's main agent. Returns `None` only when no agent can be
/// resolved at all (then delivery is skipped, fail-closed).
pub async fn resolve_agent_dir_for_delivery(
    ctx: &ReplyContext,
    explicit_agent: Option<&str>,
) -> Option<std::path::PathBuf> {
    let id = match explicit_agent {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => match get_default_agent(&ctx.home_dir).await {
            Some(d) => d,
            None => {
                let reg = ctx.registry.read().await;
                reg.main_agent()?.config.agent.name.clone()
            }
        },
    };
    Some(ctx.home_dir.join("agents").join(&id))
}

/// WP1.3: resolve the base directory an inbound attachment should be saved
/// under. Prefers the (per-agent) directory from
/// [`resolve_agent_dir_for_delivery`] so files land in
/// `~/.duduclaw/agents/<id>/attachments/`; falls back to the shared home dir
/// only when no agent can be resolved. `save_attachment_in_base` appends the
/// `attachments/` segment.
pub async fn resolve_attachment_base(
    ctx: &ReplyContext,
    explicit_agent: Option<&str>,
) -> std::path::PathBuf {
    resolve_agent_dir_for_delivery(ctx, explicit_agent)
        .await
        .unwrap_or_else(|| ctx.home_dir.clone())
}

/// WP1.3: post-process a finished reply for `📎DELIVER:` markers — send any
/// referenced files through `sender` and return the user-visible text (marker
/// lines stripped). No marker → the reply is returned untouched with zero I/O.
/// When the sandbox root can't be resolved, markers are stripped without
/// sending (fail-closed — never leak the raw marker, never send unvalidated).
pub async fn deliver_documents_for_reply(
    ctx: &ReplyContext,
    explicit_agent: Option<&str>,
    reply: String,
    sender: &dyn crate::channel_sender::ChannelSender,
) -> String {
    deliver_documents_for_reply_guarded(ctx, explicit_agent, reply, sender, None).await
}

/// Retain and recheck the source lease across file staging and every channel
/// document upload. A staged revocation suppresses remaining uploads and
/// prevents source-derived filenames or paths from reaching the final text.
pub async fn deliver_documents_for_reply_guarded(
    ctx: &ReplyContext,
    explicit_agent: Option<&str>,
    reply: String,
    sender: &dyn crate::channel_sender::ChannelSender,
    guarded: Option<&GuardedReply>,
) -> String {
    // `office_docs::DeliveryCheck` is a sync `dyn Fn() -> bool` shared with
    // non-async staging code, so the in-loop hook uses the blocking form; it
    // latches a lost lease and the awaited rechecks around it (and `Drop`)
    // run the rollback.
    let still_valid = || guarded.is_none_or(GuardedReply::still_valid_blocking);
    if guard_lost(guarded).await {
        return CCR_DELIVERY_REFUSED_TEXT.to_string();
    }
    let check = guarded.map(|_| &still_valid as &crate::office_docs::DeliveryCheck<'_>);
    if !reply.contains(crate::office_docs::DELIVER_MARKER) {
        // Marker-less reply that TALKS about a produced document (live
        // 2026-07-28 incident: real .docx written, marker forgotten, user got
        // prose only) — run the deterministic sweep so recently-produced
        // office files in the agent workdir still reach the user + archive.
        if crate::office_docs::reply_mentions_document(&reply) {
            if let Some(agent_dir) = resolve_agent_dir_for_delivery(ctx, explicit_agent).await {
                if guard_lost(guarded).await {
                    return CCR_DELIVERY_REFUSED_TEXT.to_string();
                }
                crate::office_docs::sweep_undeclared_deliverables_checked(
                    &agent_dir,
                    &ctx.home_dir,
                    sender,
                    check,
                )
                .await;
            }
        }
        return if guard_lost(guarded).await {
            CCR_DELIVERY_REFUSED_TEXT.to_string()
        } else {
            reply
        };
    }
    match resolve_agent_dir_for_delivery(ctx, explicit_agent).await {
        Some(agent_dir) => {
            if guard_lost(guarded).await {
                return CCR_DELIVERY_REFUSED_TEXT.to_string();
            }
            let delivered = crate::office_docs::process_deliverables_checked(
                &reply,
                &agent_dir,
                &ctx.home_dir,
                sender,
                check,
            )
            .await;
            if guard_lost(guarded).await {
                CCR_DELIVERY_REFUSED_TEXT.to_string()
            } else {
                delivered
            }
        }
        None => {
            if guard_lost(guarded).await {
                return CCR_DELIVERY_REFUSED_TEXT.to_string();
            }
            let (cleaned, _) = crate::office_docs::parse_deliverables(&reply);
            cleaned
        }
    }
}

/// Return the name of the agent that binds `global_token`, if any.
///
/// Shared by every token-exclusive channel (Telegram / Slack / Discord) to
/// decide whether the generic global poller must defer to an agent-bound one.
/// When a token is configured both globally and on a specific agent, the global
/// generic poller is skipped: running both fights over the exclusive long-poll /
/// gateway session (409 Conflict) and the global path routes via `default_agent`
/// rather than the bound agent, which surfaces as "identity mixing".
pub(crate) fn find_global_token_owner<'a, I>(global_token: &str, agent_tokens: I) -> Option<&'a str>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    agent_tokens
        .into_iter()
        .find(|(_, token)| *token == global_token)
        .map(|(name, _)| name)
}

/// Validate at startup that `default_agent` (if set) names a real, loaded agent.
///
/// A dangling `default_agent` — left over from a renamed or removed agent — does
/// not error; at routing time it silently falls back to an arbitrary main agent,
/// which surfaces as "identity mixing" (the wrong agent answers a channel). This
/// is loud at boot so operators can fix `config.toml` before users notice.
///
/// Returns `true` when the configuration is sound (default_agent unset, or set
/// and resolvable), `false` when it points at a missing agent.
pub async fn validate_default_agent(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
) -> bool {
    let Some(name) = get_default_agent(home_dir).await else {
        return true; // unset → main_agent() fallback is intentional
    };
    let reg = registry.read().await;
    if reg.get(&name).is_some() {
        info!("default_agent '{name}' resolved successfully");
        return true;
    }
    let available: Vec<&str> = reg
        .list()
        .iter()
        .map(|a| a.config.agent.name.as_str())
        .collect();
    warn!(
        "default_agent '{name}' in config.toml does not match any loaded agent \
         (available: {available:?}) — channel messages without an explicit \
         binding will fall back to the main agent and may be answered by the \
         wrong agent. Fix [general] default_agent or remove it."
    );
    false
}

/// Estimate the token count for a piece of text.
///
/// Uses a CJK-aware heuristic:
/// - CJK characters (U+3000–U+9FFF and supplementary ranges): ~1.5 chars/token
/// - ASCII words: ~4 chars/token
/// - Mixed: weighted average
///
/// This is significantly more accurate than the naive `len / 4` for Chinese,
/// Japanese, and Korean text, which is the primary language of this application.
pub(super) fn estimate_tokens(text: &str) -> u32 {
    let mut cjk_chars: u32 = 0;
    let mut other_chars: u32 = 0;

    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3000..=0x9FFF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x20000..=0x2A6DF).contains(&cp)
            || (0x2A700..=0x2CEAF).contains(&cp)
        {
            cjk_chars += 1;
        } else {
            other_chars += 1;
        }
    }

    // CJK: ~1.5 chars per token; other: ~4 chars per token
    let cjk_tokens = (cjk_chars as f32 / 1.5).ceil() as u32;
    let other_tokens = (other_chars as f32 / 4.0).ceil() as u32;
    cjk_tokens + other_tokens + 1 // +1 minimum
}

/// Parse session_id "telegram:12345" or "telegram:12345:thread" into (channel, chat_id).
/// Human-facing channel label for activity summaries ("telegram" → "Telegram").
pub(super) fn channel_display_name(channel: &str) -> &'static str {
    match channel {
        "telegram" => "Telegram",
        "line" => "LINE",
        "discord" => "Discord",
        "slack" => "Slack",
        "whatsapp" => "WhatsApp",
        "feishu" => "飛書",
        "googlechat" => "Google Chat",
        "teams" => "Teams",
        "webchat" => "WebChat",
        _ => "頻道",
    }
}

pub(super) fn parse_session_id_parts(session_id: &str) -> (&str, &str) {
    let parts: Vec<&str> = session_id.splitn(3, ':').collect();
    match parts.len() {
        0 | 1 => ("", session_id),
        _ => (parts[0], parts[1]),
    }
}

/// The Anthropic API key the direct-API reply fallback uses:
/// `ANTHROPIC_API_KEY` when set and non-empty, else `config.toml [api]
/// anthropic_api_key(_enc)` (resolved through the same secret reader as every
/// other config credential). `None` when neither yields a value.
pub(super) async fn get_api_key(home_dir: &Path) -> Option<String> {
    api_key_with_env(std::env::var("ANTHROPIC_API_KEY").ok(), home_dir).await
}

/// [`get_api_key`] with the environment value passed in, so tests never
/// depend on (or read) the process environment.
pub(super) async fn api_key_with_env(env: Option<String>, home_dir: &Path) -> Option<String> {
    if let Some(key) = env.filter(|k| !k.is_empty()) {
        return Some(key);
    }
    crate::config_crypto::read_encrypted_config_field(home_dir, "api", "anthropic_api_key").await
}

// ─────────────────────────────────────────────────────────────────────
// RFC-21 §1 step 4 — sender block construction
// ─────────────────────────────────────────────────────────────────────

