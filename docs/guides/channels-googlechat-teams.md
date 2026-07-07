# Google Chat & Microsoft Teams Channels

DuDuClaw supports Google Chat and Microsoft Teams as first-class channels
(alongside Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat).
Both are webhook-based: your gateway must be reachable over public HTTPS.

Both channels include:

- **Markdown-aware replies** — LLM markdown is converted to each platform's
  native markup (Google Chat `*bold*` / `<url|text>` links; Teams markdown
  with tables downgraded to monospace blocks).
- **Typing feedback** — Teams shows a real typing indicator (refreshed every
  3 s); Google Chat has no typing API, so DuDuClaw posts a placeholder
  message ("🤔 思考中…") and edits it in place.
- **Live progress** — during long agent tasks, tool activity and the agent's
  TODO task board (from `TodoWrite`) are shown via message edits and removed
  when the final reply arrives.

## Google Chat

### Setup

1. Create (or reuse) a Google Cloud project and **enable the Google Chat
   API**. Note the **project number** (IAM & Admin → Settings).
2. Create a **service account** in the same project and download its JSON
   key. No domain-wide delegation is needed — the Chat app itself is the
   principal (scope `chat.bot`).
3. Open the Chat API **Configuration** page: set the app name/avatar/
   description, enable *Interactive features*, check *Receive 1:1 messages*
   and *Join spaces and group conversations*, and set **HTTP endpoint URL**
   to `https://<your-host>/webhook/googlechat`.
4. Under *Authentication Audience* choose **Project number**.
5. Configure DuDuClaw (`config.toml`, or dashboard → Channels → 新增
   `googlechat`):

```toml
[channels]
googlechat_project_number = "123456789012"
# Paste the full service-account JSON (stored encrypted as *_enc)
googlechat_service_account_json = '{ "type": "service_account", ... }'
```

6. Restart the gateway. The log prints
   `✅ Google Chat webhook ready at /webhook/googlechat`.

### Notes

- Inbound requests are verified fail-closed: the `Authorization: Bearer`
  JWT must be signed by `chat@system.gserviceaccount.com` with audience =
  your project number.
- Replies are sent asynchronously via `spaces.messages.create` (the
  synchronous window is only 30 s — too short for agent tasks) and thread
  correctly via `REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD`.
- Unpublished Chat apps are visible only inside your Workspace org; wider
  distribution requires Google Workspace Marketplace publishing.

## Microsoft Teams

> Office 365 Connectors / incoming webhooks were **retired in May 2026** —
> a real Azure Bot is the only supported transport.

### Setup

1. **Entra app registration** (Azure portal → App registrations →
   New registration, *single tenant*): note the **Application (client) ID**
   and **Directory (tenant) ID**; create a **client secret**.
2. **Azure Bot resource** (Create a resource → Azure Bot, free **F0** tier —
   Teams messages are always free): use the existing App ID, set
   *Configuration → Messaging endpoint* to
   `https://<your-host>/webhook/teams`, then enable
   *Channels → Microsoft Teams*.
3. Configure DuDuClaw:

```toml
[channels]
teams_app_id = "00000000-0000-0000-0000-000000000000"
teams_app_password = "<client secret>"   # stored encrypted as *_enc
teams_tenant_id = "<tenant id>"          # empty = legacy multi-tenant bot
```

4. **Teams app package**: create a zip with `manifest.json` (schema ≥1.19,
   `bots[].botId` = your App ID, scopes `personal`/`team`/`groupChat`) plus
   `color.png` (192×192) and `outline.png` (32×32). Upload via Teams →
   Apps → *Manage your apps* → *Upload a custom app* (requires the org's
   custom-app policy) or the org catalog.
5. Restart the gateway. The log prints
   `✅ Microsoft Teams webhook ready at /webhook/teams`.

### Notes

- Inbound activities are verified fail-closed against the Bot Framework
  JWKS (`login.botframework.com`), audience = your App ID, and the token's
  `serviceUrl` claim must match the activity — with an Entra tenant-scoped
  fallback for single-tenant registrations.
- Every inbound message persists a **conversation reference**
  (`~/.duduclaw/teams_conversations.json`, capped at 500 entries) so
  proactive sends — delegation callback forwarding and Computer Use
  screenshots/confirmations — can reach the conversation later. A
  conversation must have messaged the bot at least once before proactive
  sends can target it.
- Replies use `textFormat: markdown`. Teams renders no tables/headings in
  plain messages, so DuDuClaw downgrades tables to monospace code blocks
  and headings to bold.
- In channels the bot only receives messages that @mention it (Teams
  platform behavior); the mention is stripped before reaching the agent.

## Formatting matrix (all channels)

| Channel | Native format used | Tables | Typing |
|---------|--------------------|--------|--------|
| Telegram | HTML parse mode (`<b>`, `<pre><code>`, `<blockquote>`) | monospace `<pre>` | `sendChatAction` every 4 s |
| Discord | markdown + embeds | monospace fence | `POST /typing` every 8 s |
| Slack | native `markdown` block (falls back to mrkdwn) | native | `assistant.threads.setStatus` |
| LINE | plain text + Flex bubble | key-value records | loading animation (1:1, ≤60 s) |
| WhatsApp | `*bold*` / `~strike~` / ``` blocks | monospace fence | `typing_indicator` (≤25 s, on inbound) |
| Feishu | interactive Card 2.0 markdown | native | — (progress via messages) |
| Google Chat | Chat markup (`*bold*`, `<url\|text>`) | monospace fence | placeholder + edit-in-place |
| MS Teams | markdown activity | monospace fence | `typing` activity every 3 s |
| WebChat | raw markdown (dashboard renders) | native | `progress` WS events |
