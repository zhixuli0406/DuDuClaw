# Channel Enhancement Plan — Plan C Full Implementation

> **Created**: 2026-04-03
> **Status**: Implementation complete, 2 rounds of deep review done
> **Goal**: Discord Plan C (Slash Commands + multi-guild + threads + embeds + buttons) + cross-channel feature alignment
> **Review**: Round 1 found 6C/12H → all CRITICAL fixed. Round 2 found 0C/4H → all fixed.

---

## Overview

Implement full-featured Discord experience (Plan C) and align other channels (Telegram, LINE, Slack) with equivalent capabilities where the platform supports them.

### Cross-Channel Feature Matrix

| Feature | Discord | Telegram | LINE | Slack |
|---------|---------|----------|------|-------|
| Thread/Topic support | **TODO** | **TODO** (supergroup topics) | N/A (no threads) | **DONE** (thread_ts) |
| Channel/chat whitelist | **TODO** | **TODO** | **TODO** | **TODO** |
| Mention-only mode | **TODO** | **TODO** | N/A (webhook 1:1) | Partial (strips mention) |
| Slash commands | **TODO** (Interactions API) | **TODO** (BotCommands) | N/A | **TODO** (Bolt) |
| Rich replies (embed/card) | **TODO** (Embeds) | **TODO** (inline keyboard) | **TODO** (Flex Message) | **TODO** (Block Kit) |
| Interactive components | **TODO** (Buttons/Selects) | **TODO** (InlineKeyboard) | **TODO** (Quick Reply) | **TODO** (Block Kit actions) |
| Multi-guild/group settings | **TODO** | **TODO** | **TODO** | **TODO** |
| Session per thread | **TODO** | **TODO** | N/A | **DONE** |
| Message splitting | **TODO** (2000 char limit) | Partial | N/A (5000 char) | **DONE** (4000 char) |
| Typing indicator | **TODO** | **TODO** | N/A | N/A |
| Per-channel MCP config | **TODO** | **TODO** | **TODO** | **TODO** |

---

## Phase 1: Shared Infrastructure

### 1.1 Channel Settings Store (SQLite)
- [ ] Create `channel_settings` table in session DB
  ```sql
  CREATE TABLE IF NOT EXISTS channel_settings (
      channel_type TEXT NOT NULL,     -- 'discord', 'telegram', 'slack', 'line'
      scope_id TEXT NOT NULL,         -- guild_id, chat_id, or 'global'
      key TEXT NOT NULL,
      value TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      PRIMARY KEY (channel_type, scope_id, key)
  );
  ```
- [ ] `ChannelSettingsManager` struct with CRUD methods
- [ ] Settings keys: `mention_only`, `allowed_channels`, `auto_thread`, `response_mode`, `agent_override`
- [ ] Wire into `ReplyContext` as shared state

### 1.2 Unified Message Formatting
- [ ] `ChannelMessage` enum for rich content (text, embed, buttons, etc.)
- [ ] `MessageFormatter` trait with per-channel implementations
- [ ] Markdown → platform-native conversion (Discord markdown, Slack mrkdwn, Telegram MarkdownV2, LINE Flex)
- [ ] Message splitting with platform-aware limits (Discord 2000, Slack 4000, Telegram 4096, LINE 5000)

### 1.3 Channel Config in config.toml
- [ ] Add `[channels.discord]`, `[channels.telegram]`, `[channels.slack]`, `[channels.line]` sections
  ```toml
  [channels.discord]
  mention_only = true
  auto_thread = true
  allowed_guilds = []         # empty = all
  allowed_channels = []       # empty = all

  [channels.telegram]
  mention_only = false
  allowed_chats = []
  ```

---

## Phase 2: Discord Plan C

### 2.1 Thread Support + Auto-Thread Replies
- [ ] Extract `guild_id` from MESSAGE_CREATE payload
- [ ] Detect if message is in a thread (Discord threads have their own `channel_id`)
- [ ] Auto-create thread for main channel messages: `POST /channels/{id}/messages/{msg_id}/threads`
- [ ] Map thread_id → DuDuClaw session for conversation continuity
- [ ] Handle `THREAD_CREATE`, `THREAD_UPDATE`, `THREAD_DELETE` events
- [ ] Auto-archive handling: close session when thread is archived
- [ ] Add `GUILD_MESSAGE_TYPING` intent for typing indicator

### 2.2 Channel Whitelist + Mention-Only Mode
- [ ] Read `allowed_channels` and `allowed_guilds` from settings
- [ ] Check `mentions` array in MESSAGE_CREATE for bot mention
- [ ] Skip messages not mentioning bot (when `mention_only = true`)
- [ ] Strip bot mention from message content before processing
- [ ] Per-guild override support

### 2.3 Slash Commands (Application Commands)
- [ ] Register global commands on READY event:
  - `/ask <prompt>` — Direct AI query
  - `/status` — Show bot status (uptime, model, session count)
  - `/config <key> <value>` — Configure guild settings (admin only)
  - `/session` — Show/manage current session
  - `/agent <name>` — Switch active agent
- [ ] Handle `INTERACTION_CREATE` events (op 0, t: INTERACTION_CREATE)
- [ ] Respond with interaction response type 4 (CHANNEL_MESSAGE_WITH_SOURCE)
- [ ] Deferred response (type 5) for long AI queries + follow-up edit
- [ ] Permission check: `/config` requires MANAGE_GUILD permission

### 2.4 Embed Replies
- [ ] Convert AI reply to Discord Embed format
  ```json
  {
    "embeds": [{
      "description": "reply text",
      "color": 16750848,
      "footer": { "text": "DuDuClaw • agent_name" },
      "timestamp": "2026-04-03T00:00:00Z"
    }]
  }
  ```
- [ ] Short replies (<200 chars) → plain text; long replies → embed
- [ ] Code blocks preserved in embed description
- [ ] Message splitting: embed description max 4096 chars, send multiple embeds if needed
- [ ] Error replies use red embed (color: 0xFF0000)

### 2.5 Button / Select Menu Interactions
- [ ] Add action row with buttons to replies:
  - "Continue" — Continue conversation in thread
  - "New Session" — Start fresh session
  - "Switch Agent" — Select menu with available agents
  - "Stop" — End current session
- [ ] Handle `INTERACTION_CREATE` with `component_type` = 2 (button) / 3 (select)
- [ ] Button custom_id format: `duduclaw:{action}:{session_id}`
- [ ] Select menu for agent switching with agent list from registry

### 2.6 Multi-Guild Settings
- [ ] Extract `guild_id` from all events
- [ ] Per-guild settings stored in `channel_settings` table
- [ ] Guild-specific agent override
- [ ] Guild-specific allowed channels list
- [ ] Guild join/leave events: auto-register default settings on GUILD_CREATE

### 2.7 Typing Indicator
- [ ] Send typing indicator while processing: `POST /channels/{id}/typing`
- [ ] Refresh every 8 seconds (typing indicator expires after 10s)
- [ ] Stop on reply sent

### 2.8 Message Splitting (Discord 2000 char limit)
- [ ] Split replies at 1900 chars (buffer for embed overhead)
- [ ] Respect code block boundaries
- [ ] Send continuation messages in same thread/channel

---

## Phase 3: Telegram Enhancement

### 3.1 Topic/Forum Support (Supergroups)
- [ ] Detect `message_thread_id` field in incoming messages
- [ ] Include `message_thread_id` in send_message for topic-aware replies
- [ ] Session ID: `telegram:{chat_id}:{thread_id}` for topic-scoped sessions
- [ ] Handle forum topic creation/close events

### 3.2 Mention-Only Mode
- [ ] Check for `@bot_username` in message text (group chats)
- [ ] Read `mention_only` from channel settings
- [ ] Always respond in private chats (DM)
- [ ] Strip bot mention from text before processing

### 3.3 Bot Commands Registration
- [ ] Register commands via `setMyCommands`:
  - `/ask` — Ask AI
  - `/status` — Bot status
  - `/voice` — Toggle voice mode (already exists)
  - `/session` — Session info
  - `/reset` — Clear session
- [ ] Handle commands in poll_loop

### 3.4 Inline Keyboard Buttons
- [ ] Add `reply_markup` to send_message with `InlineKeyboardMarkup`
- [ ] Buttons: "Continue", "New Session", "Voice Mode"
- [ ] Handle `callback_query` in poll_loop
- [ ] Answer callback query to dismiss loading state

---

## Phase 4: LINE Enhancement

### 4.1 Flex Message Replies
- [ ] Convert long replies to Flex Message format (card-style)
- [ ] Short replies (<200 chars) → plain text message
- [ ] Code blocks → Flex Message with monospace font
- [ ] Error messages → Flex Message with red accent

### 4.2 Quick Reply Buttons
- [ ] Add quickReply items to message responses
- [ ] Options: "Continue", "New Session", "Switch Agent"
- [ ] Handle postback events from Quick Reply

### 4.3 Rich Menu (Optional)
- [ ] Create Rich Menu with common actions
- [ ] Link to default users

---

## Phase 5: Slack Alignment

### 5.1 Slash Commands
- [ ] Register `/duduclaw` slash command (or `/ask`)
- [ ] Handle slash command events in Socket Mode
- [ ] Respond with ephemeral messages for config, visible for queries

### 5.2 Block Kit Messages
- [ ] Convert AI replies to Block Kit format
  ```json
  {
    "blocks": [
      { "type": "section", "text": { "type": "mrkdwn", "text": "reply" } },
      { "type": "actions", "elements": [...buttons] }
    ]
  }
  ```
- [ ] Action buttons: "Continue", "New Session"
- [ ] Handle `block_actions` events

### 5.3 Channel Whitelist
- [ ] Read `allowed_channels` from settings
- [ ] Filter messages in handle_event

---

## Phase 6: MCP Tools

### 6.1 channel_config Tool
- [ ] `channel_config` — Get/set channel settings
  - Params: `channel`, `scope_id`, `key`, `value` (optional for get)
  - Supports: mention_only, auto_thread, allowed_channels, agent_override

### 6.2 channel_status Enhanced
- [ ] Add per-guild connection status
- [ ] Add active session count per channel
- [ ] Add thread/topic count

---

## Implementation Order

1. **Phase 1** (shared infra) — Foundation for everything else
2. **Phase 2.1-2.2** (Discord threads + filtering) — Most requested features
3. **Phase 2.3** (Slash commands) — Proper Discord UX
4. **Phase 2.4-2.5** (Embeds + buttons) — Rich experience
5. **Phase 3** (Telegram) — Second most popular channel
6. **Phase 4** (LINE) — Webhook-based, lower complexity
7. **Phase 5** (Slack) — Already most mature, alignment only
8. **Phase 6** (MCP) — Cross-channel config management
9. **Phase 2.6-2.8** (Multi-guild + polish) — Final refinements

---

## Notes

- Discord API v10 used throughout
- All new config fields have `_enc` encrypted counterparts
- Backwards compatible: missing settings → sensible defaults (respond to all, no thread, plain text)
- Gateway intents will be updated: add `GUILD_MESSAGE_TYPING` (1<<11) for typing indicator
- Slash commands require `applications.commands` OAuth2 scope
