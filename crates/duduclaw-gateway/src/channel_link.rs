//! Reverse handoff (W2-3 / 04 doc §E8): "在 <通道> 中開啟" — a link on a
//! dashboard object (task, approval) that jumps back to the channel
//! conversation it came from or is currently parked in.
//!
//! Mirror of [`crate::deep_link`] (which builds dashboard-ward links for
//! channel pushes) in the opposite direction. Every URL builder below is a
//! **pure function** — no I/O, no async — so each platform's capability
//! ceiling can be pinned with a plain unit test. The 03b capability survey
//! (`commercial/docs/ux-redesign-2026-08/03b-channel-embedded-gui-capabilities.md`)
//! is the source of truth for what is constructible at all; a platform not
//! covered there by a documented "open this chat/message" mechanism gets no
//! builder and [`conversation_link`] returns `None` for it — never a guess,
//! never a partial/broken link (CLAUDE.md security convention 4: fail
//! closed, and here that means fail quiet rather than fail ugly).
//!
//! ## Coordinate persistence (W2-7)
//!
//! [`ConversationCoords`] carries fields for data the URL builders need that
//! is not visible at the point a "在通道中開啟" link gets rendered — each is
//! now captured somewhere durable and fed in by [`resolve_conversation_link`]
//! (or, for Discord, by the caller — see below):
//! - `discord_guild_id` — Discord's channel-open/message-open URL requires a
//!   guild id, which only arrives transiently on inbound Gateway events.
//!   `discord.rs` caches `channel_id -> guild_id` from every inbound message
//!   (see [`crate::discord::guild_id_for_channel`]); `TaskRow` and
//!   `decision_message_store`'s per-card entry each snapshot it onto their
//!   own record at write time (`/goal` task creation, decision-card push)
//!   rather than this function re-reading the cache live — a task/card keeps
//!   resolving even after the bot leaves the guild or the cache is pruned.
//!   Callers of [`resolve_conversation_link`] pass their already-resolved
//!   value through the `discord_guild_id` parameter.
//! - `slack_workspace_domain` — Slack's `archives` permalink requires the
//!   workspace's `<team>.slack.com` subdomain. `slack.rs`'s
//!   `run_socket_mode` extracts it from `auth.test`'s `url` field on every
//!   (re)connect and persists it via [`record_slack_workspace_domain`]; this
//!   function reads it back with [`read_slack_workspace_domain`] — one
//!   global value, no per-conversation key needed.
//! - `teams_group_id` / `teams_channel_name` / `teams_tenant_id` — Teams'
//!   `/l/channel/...` deep link needs the Team's Office 365 group id and the
//!   channel's display name. `msteams.rs`'s `ConversationRef` store now
//!   carries these as best-effort fields, populated from whatever a given
//!   inbound activity's `channelData` happens to include (Teams does not
//!   reliably send the channel display name on every activity — a miss
//!   stays `None`, never fabricated); this function looks the conversation
//!   reference up by `chat_id`.
//!
//! A coordinate that was never captured (no Discord message from that
//! channel yet, Slack bot never connected, Teams activity missing
//! `channelData`) degrades to `None` exactly like before — [`discord_link`]
//! / [`slack_link`] / [`teams_link`] never guess. Telegram and WhatsApp need
//! no such capture step: their inputs are already reachable from
//! stored/resolvable data.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::sync::Mutex as TokioMutex;

/// Everything [`conversation_link`] might need to build a link, gathered by
/// the caller. Only `chat_id` is required; every other field is optional
/// because most of it is not persisted anywhere yet (see module docs) — the
/// dispatcher degrades per platform rather than failing the whole call.
#[derive(Debug, Clone, Default)]
pub struct ConversationCoords<'a> {
    /// The channel-native chat/conversation identifier (Telegram chat id,
    /// Discord channel id, Slack channel id, Teams conversation id, WhatsApp
    /// phone number). Never rendered to the user — only ever consumed here
    /// to build a URL (CLAUDE.md convention: internal ids don't leak to the
    /// frontend; the caller must not pass this back out unless it's inside
    /// the returned link).
    pub chat_id: &'a str,
    /// A specific message to jump to, when known.
    pub message_id: Option<&'a str>,
    pub telegram_bot_username: Option<&'a str>,
    pub discord_guild_id: Option<&'a str>,
    pub slack_workspace_domain: Option<&'a str>,
    pub teams_group_id: Option<&'a str>,
    pub teams_channel_name: Option<&'a str>,
    pub teams_tenant_id: Option<&'a str>,
}

/// Dispatch to the per-platform builder by channel name. `None` for any
/// channel with no documented "open this conversation" mechanism (LINE —
/// confirmed no message-edit/deep-link-to-conversation capability, 03b
/// appendix; Feishu — AppLink only opens an embedded *webview*, not a jump
/// to an existing chat; Google Chat — 03b found no custom URL scheme at
/// all) or an unrecognised channel string.
pub fn conversation_link(channel: &str, coords: &ConversationCoords<'_>) -> Option<String> {
    match channel {
        "telegram" => telegram_link(coords.telegram_bot_username, coords.chat_id, coords.message_id),
        "discord" => discord_link(coords.discord_guild_id, coords.chat_id, coords.message_id),
        "slack" => slack_link(coords.slack_workspace_domain, coords.chat_id, coords.message_id),
        "teams" => teams_link(
            coords.teams_group_id,
            Some(coords.chat_id),
            coords.teams_channel_name,
            coords.teams_tenant_id,
        ),
        "whatsapp" => whatsapp_link(coords.chat_id),
        _ => None,
    }
}

/// Telegram: `chat_id` is the Bot API chat id (positive = private chat with
/// the bot; negative = group; `-100`-prefixed = supergroup/channel, per the
/// well-known Bot API id-space convention).
///
/// - **Private chat**: Telegram has no permalink concept for private-chat
///   messages at all (only public channels/supergroups do — `t.me/c/...`),
///   so the best reachable target is the conversation itself:
///   `https://t.me/<bot_username>`. `message_id` is accepted but unused —
///   there is nothing more precise to link to.
/// - **Supergroup/channel** (`-100`-prefixed): `https://t.me/c/<internal_id>/<message_id>`
///   — requires a message id; there is no official "just open this group"
///   link without one.
/// - **Plain basic group** (negative, not `-100`-prefixed): no permalink
///   mechanism exists at all (only supergroups/channels have the internal
///   numeric id space `t.me/c/` addresses) → `None`.
pub fn telegram_link(bot_username: Option<&str>, chat_id: &str, message_id: Option<&str>) -> Option<String> {
    let id: i64 = chat_id.trim().parse().ok()?;
    if id > 0 {
        let username = bot_username.map(str::trim).filter(|s| !s.is_empty())?;
        return Some(format!("https://t.me/{username}"));
    }
    let msg = message_id.map(str::trim).filter(|s| !s.is_empty())?;
    let internal = id.to_string();
    let internal = internal.strip_prefix("-100")?;
    if internal.is_empty() {
        return None;
    }
    Some(format!("https://t.me/c/{internal}/{msg}"))
}

/// Discord: `https://discord.com/channels/<guild>/<channel>[/<message>]` —
/// the same URL "Copy Message Link" (or "Copy Channel Link") produces in the
/// client, works in both the desktop app and a browser. Requires the guild
/// id (see module docs — not persisted anywhere today, so this resolves to
/// `None` in production until that's wired up). Falls back to a
/// channel-level link (still lands the operator in the right conversation)
/// when no message id is available.
pub fn discord_link(guild_id: Option<&str>, channel_id: &str, message_id: Option<&str>) -> Option<String> {
    let guild = guild_id.map(str::trim).filter(|s| !s.is_empty())?;
    let channel = channel_id.trim();
    if channel.is_empty() {
        return None;
    }
    match message_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(msg) => Some(format!("https://discord.com/channels/{guild}/{channel}/{msg}")),
        None => Some(format!("https://discord.com/channels/{guild}/{channel}")),
    }
}

/// Slack: `https://<workspace>.slack.com/archives/<channel>[/p<ts>]` — the
/// same shape `chat.getPermalink` returns. Requires the workspace's
/// `<team>.slack.com` subdomain (see module docs — not persisted today, so
/// this resolves to `None` in production until that's wired up). Falls back
/// to a channel-level link when no message timestamp is available; a
/// malformed (non-numeric-with-one-dot) `message_ts` also degrades to the
/// channel-level link rather than emitting a broken permalink.
pub fn slack_link(workspace_domain: Option<&str>, channel_id: &str, message_ts: Option<&str>) -> Option<String> {
    let domain = workspace_domain.map(str::trim).filter(|s| !s.is_empty())?;
    let channel = channel_id.trim();
    if channel.is_empty() {
        return None;
    }
    let base = format!("https://{domain}.slack.com/archives/{channel}");
    let Some(ts) = message_ts.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(base);
    };
    // "1234567890.123456" -> "p1234567890123456" (Slack's own permalink shape).
    let compact: String = ts.chars().filter(|c| *c != '.').collect();
    if compact.is_empty() || !compact.chars().all(|c| c.is_ascii_digit()) {
        return Some(base);
    }
    Some(format!("{base}/p{compact}"))
}

/// Microsoft Teams: the official channel deep link
/// `https://teams.microsoft.com/l/channel/<channelId>/<channelName>?groupId=<groupId>&tenantId=<tenantId>`
/// (03b §2(d)). Requires the Team's Office 365 group id and the channel's
/// display name (see module docs — not persisted today, so this resolves to
/// `None` in production until that's wired up). `tenant_id` is optional but
/// recommended by Microsoft's own docs — appended when present.
pub fn teams_link(
    group_id: Option<&str>,
    channel_id: Option<&str>,
    channel_name: Option<&str>,
    tenant_id: Option<&str>,
) -> Option<String> {
    let group = group_id.map(str::trim).filter(|s| !s.is_empty())?;
    let channel = channel_id.map(str::trim).filter(|s| !s.is_empty())?;
    let name = channel_name.map(str::trim).filter(|s| !s.is_empty())?;
    let mut url = format!(
        "https://teams.microsoft.com/l/channel/{}/{}?groupId={}",
        percent_encode_path_segment(channel),
        percent_encode_path_segment(name),
        percent_encode_path_segment(group),
    );
    if let Some(tenant) = tenant_id.map(str::trim).filter(|s| !s.is_empty()) {
        url.push_str("&tenantId=");
        url.push_str(&percent_encode_path_segment(tenant));
    }
    Some(url)
}

/// WhatsApp Cloud API: `https://wa.me/<phone>` (03b §3(d)) — pre-fills a
/// chat with that number in the operator's own WhatsApp app; the platform
/// has no editable/structured deep link beyond this (requires the human to
/// hit send). `chat_id` must already look like an E.164 phone number
/// (digits only after stripping one optional leading `+`, 8-15 digits) —
/// anything else is refused rather than emitting a link that opens the
/// wrong (or no) chat.
pub fn whatsapp_link(chat_id: &str) -> Option<String> {
    let trimmed = chat_id.trim().strip_prefix('+').unwrap_or(chat_id.trim());
    if trimmed.len() < 8 || trimmed.len() > 15 {
        return None;
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("https://wa.me/{trimmed}"))
}

/// Percent-encode a single URL path/query segment. Encodes every byte
/// outside the unreserved set (RFC 3986 `ALPHA / DIGIT / "-" / "." / "_" /
/// "~"`) — including UTF-8 continuation bytes individually, which is safe
/// because a compliant percent-decoder reassembles the original UTF-8 bytes
/// regardless of how they were split into `%XX` triplets. No panics: this
/// walks bytes, never slices by char boundary (project convention 1).
fn percent_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── Slack workspace domain: persisted at Socket Mode connect (W2-7) ─────
//
// `slack_link` needs the `<team>.slack.com` subdomain. `slack.rs`'s
// `run_socket_mode` extracts it once from `auth.test`'s `url` field on every
// (re)connect and persists it here via [`record_slack_workspace_domain`] (the
// coordinate capture point lives in slack.rs; this module owns the shared
// store + the read side, matching this file's role for every other
// platform). A single global value — one gateway process serves one Slack
// workspace — so unlike Discord (per-channel) or Teams (per-conversation),
// no key beyond the filename is needed.

pub(crate) const SLACK_WORKSPACE_STORE_FILE: &str = "slack_workspace.json";

fn slack_workspace_store_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join(SLACK_WORKSPACE_STORE_FILE)
}

/// Persist the workspace subdomain. Best-effort and non-fatal: a write
/// failure only means a later "在 Slack 中開啟" link degrades to `None`,
/// never blocks the Socket Mode connection that called this.
pub(crate) fn record_slack_workspace_domain(home_dir: &Path, domain: &str) {
    if domain.is_empty() {
        return;
    }
    let path = slack_workspace_store_path(home_dir);
    let domain = domain.to_string();
    let result = duduclaw_core::with_file_lock(&path, || {
        let bytes = serde_json::to_vec(&serde_json::json!({ "domain": domain }))
            .map_err(std::io::Error::other)?;
        // Atomic replace (temp + rename), matching decision_message_store's
        // pattern for the same class of small durable JSON state.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)
    });
    if let Err(e) = result {
        tracing::debug!(error = %e, "slack: failed to persist workspace domain (non-fatal)");
    }
}

/// Read the persisted workspace subdomain. `None` when never connected yet
/// (or the file is missing/corrupt) — an honest gap, never guessed.
fn read_slack_workspace_domain(home_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(slack_workspace_store_path(home_dir)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("domain").and_then(|v| v.as_str()).map(str::to_string).filter(|s| !s.is_empty())
}

// ── Telegram bot username: cached resolution ────────────────────────────
//
// `telegram_link` needs the bot's `@username`, which Telegram only exposes
// live via `getMe`. A list RPC (`tasks.list` / `approvals.list`) can be
// called frequently by the dashboard, so this caches the result
// process-wide with a short TTL rather than hitting the Telegram API on
// every call — mirroring the `PendingUpdate` 5-minute-TTL pattern already
// used elsewhere in `handlers.rs`. One shared bot per gateway (the same
// assumption `handle_telegram_bind_token` makes), so the cache is keyed by
// nothing — there is only ever one value to remember.

const TELEGRAM_USERNAME_CACHE_TTL: Duration = Duration::from_secs(300);

fn telegram_username_cache() -> &'static TokioMutex<Option<(Option<String>, Instant)>> {
    static CACHE: OnceLock<TokioMutex<Option<(Option<String>, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| TokioMutex::new(None))
}

/// Resolve (and cache) the deployment Telegram bot's `@username` — the global
/// `channels.telegram_bot_token` first, falling back to the first per-agent
/// token (a deployment whose bot is agent-scoped has an empty global field
/// but a perfectly linkable bot). `None` when unconfigured or unreachable —
/// cached too (for the same TTL), so a missing/offline bot costs at most one
/// `getMe` attempt per cache window instead of one per list call.
pub async fn cached_telegram_bot_username(home_dir: &Path) -> Option<String> {
    let cache = telegram_username_cache();
    {
        let guard = cache.lock().await;
        if let Some((value, at)) = guard.as_ref() {
            if at.elapsed() < TELEGRAM_USERNAME_CACHE_TTL {
                return value.clone();
            }
        }
    }
    let token = crate::config_crypto::channel_dm_token_candidates(home_dir, "telegram")
        .await
        .into_iter()
        .next();
    let username = match token {
        Some(t) => crate::handlers::fetch_telegram_bot_username(&t).await,
        None => None,
    };
    let mut guard = cache.lock().await;
    *guard = Some((username.clone(), Instant::now()));
    username
}

/// Resolve the "open in `channel`" link for a `(channel, chat_id,
/// message_id)` triple, fetching whatever supporting data (Telegram bot
/// username, Slack workspace domain, Teams conversation reference) is
/// available. The one entry point every RPC handler call site uses — keeps
/// the plumbing (cache lookups, side-store reads) out of `handlers.rs`.
/// `None` when `chat_id` is blank or the platform/data combination can't
/// produce a link (see [`conversation_link`]).
///
/// `discord_guild_id` is the one coordinate this function does NOT resolve
/// itself: unlike Slack (one workspace, read from a global file) and Teams
/// (per-conversation, read from `teams_conversations.json` keyed by
/// `chat_id`), Discord's guild id is snapshotted onto the caller's own
/// record at write time (`TaskRow::source_discord_guild_id`,
/// `decision_message_store`'s per-card entry) rather than looked up live
/// here — see `discord::guild_id_for_channel`'s module docs for why. Pass
/// through whatever the caller already has; `None` when not yet known.
pub async fn resolve_conversation_link(
    home_dir: &Path,
    channel: &str,
    chat_id: &str,
    message_id: Option<&str>,
    discord_guild_id: Option<&str>,
) -> Option<String> {
    if channel.trim().is_empty() || chat_id.trim().is_empty() {
        return None;
    }
    let telegram_bot_username = if channel == "telegram" {
        cached_telegram_bot_username(home_dir).await
    } else {
        None
    };
    let slack_workspace_domain =
        if channel == "slack" { read_slack_workspace_domain(home_dir) } else { None };
    let teams_ref = if channel == "teams" { crate::msteams::lookup_conversation_ref(home_dir, chat_id) } else { None };
    let coords = ConversationCoords {
        chat_id,
        message_id,
        telegram_bot_username: telegram_bot_username.as_deref(),
        discord_guild_id,
        slack_workspace_domain: slack_workspace_domain.as_deref(),
        teams_group_id: teams_ref.as_ref().and_then(|r| r.teams_group_id.as_deref()),
        teams_channel_name: teams_ref.as_ref().and_then(|r| r.teams_channel_name.as_deref()),
        teams_tenant_id: teams_ref.as_ref().and_then(|r| r.teams_tenant_id.as_deref()),
    };
    conversation_link(channel, &coords)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Telegram ──────────────────────────────────────────────
    #[test]
    fn telegram_private_chat_with_username() {
        assert_eq!(
            telegram_link(Some("duduclaw_bot"), "123456789", None),
            Some("https://t.me/duduclaw_bot".to_string())
        );
    }

    #[test]
    fn telegram_private_chat_without_username_is_none() {
        assert_eq!(telegram_link(None, "123456789", None), None);
        assert_eq!(telegram_link(Some("   "), "123456789", None), None);
    }

    #[test]
    fn telegram_supergroup_with_message_id() {
        assert_eq!(
            telegram_link(None, "-1001234567890", Some("42")),
            Some("https://t.me/c/1234567890/42".to_string())
        );
    }

    #[test]
    fn telegram_supergroup_without_message_id_is_none() {
        assert_eq!(telegram_link(None, "-1001234567890", None), None);
    }

    #[test]
    fn telegram_basic_group_is_never_linkable() {
        // Negative but NOT -100-prefixed: a plain (non-super) group. No
        // permalink mechanism exists even with a message id.
        assert_eq!(telegram_link(None, "-987654321", Some("1")), None);
    }

    #[test]
    fn telegram_unparseable_chat_id_is_none() {
        assert_eq!(telegram_link(Some("bot"), "not-a-number", None), None);
    }

    // ── Discord ───────────────────────────────────────────────
    #[test]
    fn discord_full_message_link() {
        assert_eq!(
            discord_link(Some("111"), "222", Some("333")),
            Some("https://discord.com/channels/111/222/333".to_string())
        );
    }

    #[test]
    fn discord_channel_level_fallback_without_message_id() {
        assert_eq!(
            discord_link(Some("111"), "222", None),
            Some("https://discord.com/channels/111/222".to_string())
        );
    }

    #[test]
    fn discord_without_guild_id_is_none() {
        // A caller with no guild id (never captured for this channel yet)
        // must degrade to no link, never a broken guess.
        assert_eq!(discord_link(None, "222", Some("333")), None);
    }

    // ── Slack ─────────────────────────────────────────────────
    #[test]
    fn slack_permalink_with_ts() {
        assert_eq!(
            slack_link(Some("acme"), "C123", Some("1234567890.123456")),
            Some("https://acme.slack.com/archives/C123/p1234567890123456".to_string())
        );
    }

    #[test]
    fn slack_channel_level_fallback_without_ts() {
        assert_eq!(
            slack_link(Some("acme"), "C123", None),
            Some("https://acme.slack.com/archives/C123".to_string())
        );
    }

    #[test]
    fn slack_malformed_ts_degrades_to_channel_level() {
        assert_eq!(
            slack_link(Some("acme"), "C123", Some("not-a-timestamp")),
            Some("https://acme.slack.com/archives/C123".to_string())
        );
    }

    #[test]
    fn slack_without_workspace_domain_is_none() {
        // No workspace domain (bot never connected yet) must degrade to no
        // link, never a broken guess.
        assert_eq!(slack_link(None, "C123", Some("1.2")), None);
    }

    // ── Teams ─────────────────────────────────────────────────
    #[test]
    fn teams_full_deep_link_with_tenant() {
        let link = teams_link(Some("grp-1"), Some("19:abc@thread.tacv2"), Some("General"), Some("tenant-1"));
        assert_eq!(
            link,
            Some(
                "https://teams.microsoft.com/l/channel/19%3Aabc%40thread.tacv2/General?groupId=grp-1&tenantId=tenant-1"
                    .to_string()
            )
        );
    }

    #[test]
    fn teams_without_tenant_omits_tenant_param() {
        let link = teams_link(Some("grp-1"), Some("chan-1"), Some("General"), None);
        assert_eq!(link, Some("https://teams.microsoft.com/l/channel/chan-1/General?groupId=grp-1".to_string()));
    }

    #[test]
    fn teams_missing_group_id_is_none() {
        // No group id (channelData didn't carry one) must degrade to no
        // link, never a broken guess.
        assert_eq!(teams_link(None, Some("chan-1"), Some("General"), None), None);
    }

    #[test]
    fn teams_missing_channel_name_is_none() {
        assert_eq!(teams_link(Some("grp-1"), Some("chan-1"), None, None), None);
    }

    // ── WhatsApp ──────────────────────────────────────────────
    #[test]
    fn whatsapp_valid_e164_digits() {
        assert_eq!(whatsapp_link("+886912345678"), Some("https://wa.me/886912345678".to_string()));
        assert_eq!(whatsapp_link("886912345678"), Some("https://wa.me/886912345678".to_string()));
    }

    #[test]
    fn whatsapp_rejects_non_phone_shapes() {
        assert_eq!(whatsapp_link("not-a-phone"), None);
        assert_eq!(whatsapp_link("123"), None); // too short
        assert_eq!(whatsapp_link("1234567890123456"), None); // too long (16 digits)
    }

    // ── Dispatcher ────────────────────────────────────────────
    #[test]
    fn conversation_link_dispatches_by_channel() {
        let coords = ConversationCoords {
            chat_id: "123456789",
            telegram_bot_username: Some("duduclaw_bot"),
            ..Default::default()
        };
        assert_eq!(conversation_link("telegram", &coords), Some("https://t.me/duduclaw_bot".to_string()));
    }

    #[test]
    fn conversation_link_line_is_always_none() {
        // Confirmed capability gap (03b appendix): LINE has no edit/deep-link
        // mechanism at all — never show a button, never guess a link.
        let coords = ConversationCoords { chat_id: "u1", ..Default::default() };
        assert_eq!(conversation_link("line", &coords), None);
    }

    #[test]
    fn conversation_link_feishu_and_googlechat_are_none() {
        let coords = ConversationCoords { chat_id: "c1", ..Default::default() };
        assert_eq!(conversation_link("feishu", &coords), None);
        assert_eq!(conversation_link("googlechat", &coords), None);
    }

    #[test]
    fn conversation_link_unknown_channel_is_none() {
        let coords = ConversationCoords { chat_id: "c1", ..Default::default() };
        assert_eq!(conversation_link("carrier_pigeon", &coords), None);
    }

    // ── resolve_conversation_link (async integration smoke) ───
    #[tokio::test]
    async fn resolve_conversation_link_blank_chat_id_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_conversation_link(dir.path(), "telegram", "", None, None).await, None);
        assert_eq!(resolve_conversation_link(dir.path(), "", "123", None, None).await, None);
    }

    #[tokio::test]
    async fn resolve_conversation_link_unconfigured_telegram_degrades_to_none() {
        // No config.toml at all in this tempdir ⇒ no token ⇒ no username ⇒
        // no link. Must not panic, must not hang.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_conversation_link(dir.path(), "telegram", "123456789", None, None).await, None);
    }

    // ── W2-7: coordinate persistence → real URL, per platform ──

    /// Discord: `discord.rs` records `channel_id -> guild_id` from an
    /// inbound message; a caller (e.g. `chat_commands::handle_goal_create`)
    /// looks it up and passes it through — this is that full round trip,
    /// not just the pure builder in isolation.
    #[tokio::test]
    async fn resolve_conversation_link_discord_round_trips_through_recorded_guild() {
        let dir = tempfile::tempdir().unwrap();
        crate::discord::record_channel_guild(dir.path(), "222", "111");
        let guild = crate::discord::guild_id_for_channel(dir.path(), "222");
        assert_eq!(guild.as_deref(), Some("111"));
        let link = resolve_conversation_link(dir.path(), "discord", "222", Some("333"), guild.as_deref()).await;
        assert_eq!(link, Some("https://discord.com/channels/111/222/333".to_string()));
    }

    /// Discord: a channel this gateway has never seen a message from has no
    /// recorded guild id — the caller passes `None` through and the link
    /// degrades, it does not panic or guess.
    #[tokio::test]
    async fn resolve_conversation_link_discord_unseen_channel_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let guild = crate::discord::guild_id_for_channel(dir.path(), "999");
        assert_eq!(guild, None);
        let link = resolve_conversation_link(dir.path(), "discord", "999", None, guild.as_deref()).await;
        assert_eq!(link, None);
    }

    /// Slack: `slack.rs` persists the workspace domain at Socket Mode
    /// connect; this function reads it straight back from the same file, no
    /// parameter needed from the caller (one global value per gateway).
    #[tokio::test]
    async fn resolve_conversation_link_slack_reads_persisted_workspace_domain() {
        let dir = tempfile::tempdir().unwrap();
        record_slack_workspace_domain(dir.path(), "acme");
        let link = resolve_conversation_link(dir.path(), "slack", "C123", None, None).await;
        assert_eq!(link, Some("https://acme.slack.com/archives/C123".to_string()));
    }

    /// Slack: never connected in this home_dir ⇒ no persisted domain ⇒ `None`.
    #[tokio::test]
    async fn resolve_conversation_link_slack_never_connected_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let link = resolve_conversation_link(dir.path(), "slack", "C123", None, None).await;
        assert_eq!(link, None);
    }

    /// Teams: `msteams.rs` extends `ConversationRef` (`teams_conversations.json`)
    /// with best-effort `channelData`-derived coordinates; this function
    /// reads the reference back by `chat_id` (the conversation id).
    #[tokio::test]
    async fn resolve_conversation_link_teams_reads_conversation_ref_coords() {
        let dir = tempfile::tempdir().unwrap();
        let mut store: std::collections::HashMap<String, crate::msteams::ConversationRef> =
            std::collections::HashMap::new();
        store.insert(
            "conv-1".to_string(),
            crate::msteams::ConversationRef {
                service_url: "https://smba.trafficmanager.net/amer".into(),
                bot_account: serde_json::json!({}),
                user_account: serde_json::json!({}),
                updated_at: 1,
                teams_group_id: Some("grp-1".into()),
                teams_channel_name: Some("General".into()),
                teams_tenant_id: Some("tenant-1".into()),
            },
        );
        std::fs::write(dir.path().join("teams_conversations.json"), serde_json::to_vec(&store).unwrap()).unwrap();

        let link = resolve_conversation_link(dir.path(), "teams", "conv-1", None, None).await;
        assert_eq!(
            link,
            Some(
                "https://teams.microsoft.com/l/channel/conv-1/General?groupId=grp-1&tenantId=tenant-1"
                    .to_string()
            )
        );
    }

    /// Teams: a conversation reference recorded before `channelData` carried
    /// team/channel info (or an activity that never included it — e.g. a
    /// 1:1 chat) has no group id — degrade to `None`, not a broken link.
    #[tokio::test]
    async fn resolve_conversation_link_teams_missing_coords_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut store: std::collections::HashMap<String, crate::msteams::ConversationRef> =
            std::collections::HashMap::new();
        store.insert(
            "conv-2".to_string(),
            crate::msteams::ConversationRef {
                service_url: "https://smba.trafficmanager.net/amer".into(),
                bot_account: serde_json::json!({}),
                user_account: serde_json::json!({}),
                updated_at: 1,
                teams_group_id: None,
                teams_channel_name: None,
                teams_tenant_id: None,
            },
        );
        std::fs::write(dir.path().join("teams_conversations.json"), serde_json::to_vec(&store).unwrap()).unwrap();

        let link = resolve_conversation_link(dir.path(), "teams", "conv-2", None, None).await;
        assert_eq!(link, None);
    }
}
