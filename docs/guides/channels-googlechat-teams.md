# Google Chat & Microsoft Teams channels

Google Chat channel credentials authorize the Chat application (`chat.bot`). They do not connect Gmail, Drive or Calendar. Workspace data tools have their own [Google setup](google-setup.md). Saving a channel in the dashboard applies live; restart after editing channel configuration by hand.

DuDuClaw supports Google Chat and Microsoft Teams as first-class channels
(alongside Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat).
Teams and Google Chat HTTP mode need a public HTTPS gateway; Google Chat Pub/Sub mode does not.

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

### Reply display and delivery

Google Chat tables appear as a list: each row starts with its first cell in bold, followed by `column: value` lines. Cell contents are kept in full. Bullets use `•`; headings and bold text spanning lines are converted to Chat markup. Chinese text next to bold markers keeps its original spacing.

Tool details and task lists are visible only to internal staff (an administrator or a verified account directly bound to the answering employee). Other senders see generic progress. Configuration-change notices wait for the next internal reply. Access denial, mute and human takeover do not produce an empty-reply warning; the temporary placeholder is removed when possible.

Final delivery stops further progress edits. If replacing the placeholder fails, DuDuClaw retries once after a short delay (up to three seconds for a rate-limit response), then sends a new message and tries to replace the old placeholder with a neutral line. Remaining answer parts are still attempted. If the original edit succeeded but its response was lost, the fallback can duplicate the first part.

Google Chat never includes a thinking summary in the answer. Existing thinking settings can allow internal staff a duration line; other senders see none. A resolved `zh-TW` reply uses conservative Traditional Chinese character conversion while preserving code and URLs. Auto language adds a reminder to follow the user's message language.

Rendering has not been verified on a real Google Chat client (尚未在真實 Google Chat 用戶端驗證). Check desktop and mobile table lists, Chinese bold boundaries, multiline bold and placeholder replacement.

## Google Chat

### Before you start

Use a Business or Enterprise Google Workspace account with Chat enabled and an administrator who allows Chat apps. You need permission to create or directly own a Cloud project, enable Chat API, and create a service account and key. Local setup needs Bash, Python 3, `gcloud`, your own `gcloud auth login`, and, for HTTP mode, a public HTTPS gateway URL; Cloud Shell supplies the command-line tools. Pub/Sub mode uses outbound connections without a public URL.

### One-step setup

Use a dedicated new Google Cloud project (the default). Run this command on the DuDuClaw host:

The automatic import requires a running DuDuClaw gateway and an administrator login (including MFA when required).

```bash
# HTTP: replace the host with your public gateway address.
duduclaw channels googlechat bootstrap --endpoint-url https://chat.example.com/webhook/googlechat
```

The command creates a project, enables Chat API, and prepares a service account before creating its private JSON key as the last cloud step. It then imports the settings. `--project-id your-project-id` reuses a project you directly own; group or inherited ownership is conservatively refused. Optional flags: `--app-name`, `--agent`, `--out`, and `--dry-run` (no cloud or local changes). The default project ID is `duduclaw-chat-<random6>`. The private output directory stores `service-account.json` with mode 0600. A rerun reuses a matching key. Imported credentials are encrypted at rest. In HTTP mode, if a later script step fails after key creation, it attempts to revoke the new cloud key and remove the local file. Revocation failures or an unreadable key ID are reported for manual follow-up. If the script succeeds but CLI import fails, the CLI keeps the key file and exits non-zero. The new service-account key remains valid; the error includes the file path and exact commands to retry the import or revoke the key manually. The CLI never revokes keys or deletes key files automatically.

If you do not have local gcloud, [open Cloud Shell](https://shell.cloud.google.com/cloudshell/editor?cloudshell_git_repo=https://github.com/zhixuli0406/DuDuClaw&cloudshell_tutorial=scripts/googlechat/cloudshell-tutorial.md&show=terminal) and run `bash scripts/googlechat/bootstrap.sh --endpoint-url https://chat.example.com/webhook/googlechat`. Download the key securely to your DuDuClaw host, set `chmod 600 /private/path/service-account.json`, and run the printed import command with its local path. On Windows, keep the plain regular key file inside your own user profile directory. Reparse points are refused; owner and ACL permissions are not verified.

```bash
duduclaw channels googlechat import --project-number 123456789012 --key-file /private/path/service-account.json
```

### Finish in Google Chat

Open the printed Chat Configuration link. Google does not provide a configuration API for this page, so this final step is manual:

1. **Uncheck “Build this Chat app as a Google Workspace add-on” when creating the app.** Paste the app name (up to 25 characters), a public HTTPS avatar URL, and description. No hosted DuDuClaw avatar is assumed by the script.
2. Enable **Interactive features**, 1:1 messages, and joining spaces.
3. Paste the HTTP endpoint and set **Authentication Audience → Project number**.
4. Set **Visibility** to your own Workspace email and save. Add the app to a space or send it a direct message, then verify an actual reply.

A successful import or resource creation does not prove that Chat is configured or that a message round trip works. The dashboard offers the bootstrap command, Cloud Shell, copyable configuration values; manual fields remain under **Manual setup**.

### Recognising who is messaging (v1.82)

By default a Google Chat sender is an unknown person, so `[google] requesters = "bound_people"` / `owner_only` refuse them and `@` calls reach only the default and public employees. A sender is recognised as a dashboard account when:

- an Admin bound their Chat user id (`users/<digits>`) under **Users → Verify identity → Google Chat** (the default and only way, unless the next option is on), or
- optional, **off by default**: an Admin turned on **Manage → Integrations → Google → Recognise Google Chat senders by their connected Google account** (`[google] chat_identity_from_account = true`) and the person connected their own Google account under **My connections**. The sender's id is then compared with that Google account's id and a match counts as the person. **Not yet verified against a real Google Workspace** — leave it off unless you have confirmed in your own Workspace that the two ids match.

No e-mail matching. Messages from another Chat app (not a person) are ignored, and a Google Chat identity never receives DuDuClaw's own notifications. Google Chat cannot receive login or two-factor codes. Details and the uniqueness rules: [Dashboard settings → Users → Verify identity](dashboard-settings.md#users--verify-identity-channel-identities-v182).

### Key policy

If key creation reports `FAILED_PRECONDITION`, ask an Organization Policy Administrator to review the project exception. Organizations created on or after 2024-05-03 commonly enforce key-creation restrictions. The script prints the commands and stops; it does not change policy:

```bash
gcloud resource-manager org-policies disable-enforce iam.disableServiceAccountKeyCreation --project=your-project-id
gcloud resource-manager org-policies disable-enforce iam.managed.disableServiceAccountKeyCreation --project=your-project-id
```

For the managed constraint, an administrator should verify support in their installed gcloud and may need the Organization Policies console. See [Google's key policy guidance](https://cloud.google.com/iam/docs/troubleshoot-org-policies).

### Appendix: manual setup

Create a project, enable Chat API, create a service account and JSON key, and configure the Chat page as above. In **Channels → Add Google Chat → Manual setup**, enter the project number and full JSON key. Save and test a message. HTTP verifies the inbound JWT against `chat@system.gserviceaccount.com` and your project number. Unsupported Workspace add-on payloads produce a diagnostic asking you to uncheck the add-on option.

### Pub/Sub: no public gateway URL

On a Unix DuDuClaw host, run:

```bash
duduclaw channels googlechat bootstrap --connection pubsub
# Or import a previously created key and pull subscription:
duduclaw channels googlechat import --connection pubsub --project-number 123456789012 --key-file /private/path/service-account.json --subscription projects/your-project-id/subscriptions/duduclaw-chat-events-sub
```

Cloud Shell uses `bash scripts/googlechat/bootstrap.sh --connection pubsub`; download the key securely and run the printed import command on your host. Add `--agent alice` for an employee-specific app, using a dedicated project for each app. The script enables Chat and Pub/Sub APIs, creates `projects/<project-id>/topics/duduclaw-chat-events` and a pull subscription with one-day delivery retention, grants the Google Chat publisher access to the topic, and grants the app service account subscriber and metadata-read access to that subscription. Resource checks and IAM grants finish before key creation. Existing subscriptions must point at the canonical topic; their settings are not changed automatically. Both import and worker startup re-read the subscription from Google. Pub/Sub setup failures print manual revocation commands and retain the key; destructive gcloud commands are never executed in this mode.

In Chat Configuration, turn off **Build this Chat app as a Google Workspace add-on**, choose **Cloud Pub/Sub**, and paste the topic from the summary. No public HTTP endpoint is required. Keep the other app/visibility settings above and test an actual message. The gateway needs outbound HTTPS access to Google OAuth, Pub/Sub and Chat APIs.

The gateway re-reads every message from Google Chat with [`spaces.messages.get`](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/get), using app authentication and `https://www.googleapis.com/auth/chat.bot`. No administrator-approved Chat scope is requested. The pull token separately uses only `https://www.googleapis.com/auth/pubsub`. Pub/Sub content is an untrusted notification: only a strictly validated Chat message resource name is used for the lookup. Sender, space, text, attachments and thread come from the API response. Sender allowlists, pairing and approval rules apply to that fetched identity. A missing/inaccessible message (including 403/404), invalid resource or mismatched space is dropped with a diagnostic and creates no turn. Check app access and the `chat.bot` scope when the operator hint appears. Events without a fetchable message, including app-added events, do not start employee turns.

A bounded persistent inbox deduplicates by event type and Chat message resource name, so republishing with a new Pub/Sub message ID does not rerun a retained event. Records are retained for seven days; messages whose API creation time is older than seven days are refused even after pruning. Accepted events are stored before acknowledgement. After a restart, not-started work may resume, while interrupted running work is marked uncertain and is not automatically rerun. Full queues are acknowledged and dropped without starting a turn; as in HTTP mode, the space is told to resend (at most once a minute per sender and once every 10 seconds per space). This is not an end-to-end exactly-once delivery guarantee. The Unix-only inbox stores only resource references, never message text, sender details or attachments. Each app stream (employee/shared app and project number) is capped at 1,000 rows and 10 pending/queued/running entries, reserving capacity for other streams; global hard caps remain 10,000 rows and 100 active entries, 2 KiB per reference and a 64 MiB database; live queued/running entries are not evicted. Pub/Sub key-file import is refused on non-Unix platforms until equivalent owner/ACL validation is available. Google-side message retrieval and round trips still require live acceptance. Import and worker startup reject subscriptions with exactly-once delivery enabled; keep that option disabled in Google Cloud. An invalid/expired ACK response (HTTP 400) discards only the old delivery batch and pulls again; durable deduplication records remain intact. Transient ACK failures retain the batch for retry.

### Commands, access and pairing

Chat commands (`/new`, `/reset`, `/compact`, `/pair`, `/status`, …) pass the same access check as ordinary messages, on the same space. A blocked sender gets no answer; an unpaired sender gets the pairing hint.

A space has one conversation shared by all its members and threads (an employee's own Chat app keeps its own conversation in the same space). Who may clear or compress a shared conversation (`/new`, `/reset`, `/compact`):

| Installation | 1:1 DM with the app | Space, group chat (group DM) or unknown type |
|---|---|---|
| No user accounts, or no active **Admin**/**Manager** with a verified channel identity (e.g. a personal install) | anyone who passes the access check | anyone who passes the access check |
| At least one active Admin/Manager with a verified channel identity | anyone who passes the access check | only a sender whose verified channel identity is bound to an active Admin or Manager; others are told 「只有管理員可以清除這個聊天室的對話。」 |
| A user database that exists but cannot be read | anyone who passes the access check | refused |

A blocked or unpaired sender can never run these commands. In the second row, an unverified identity or a failed lookup is refused. The space type comes from the message (the HTTP event, or the message re-read through `spaces.messages.get` in Pub/Sub mode). When the message does not say, the gateway asks Google Chat (`spaces.get`, as the app) and remembers the answer for 24 hours. A 1:1 DM means a direct message between one person and the app. A group chat with several people is a shared conversation. If the type is still unknown, the space is treated as shared. The same rule applies to Telegram groups, Discord server channels, Slack channels, LINE groups and rooms, Teams group chats and channels, Feishu groups and DingTalk groups, and to the “new conversation” buttons.

Pairing (`require_pairing`): generate a code with the `pairing_manage` tool (`action: "generate"`) for one of two subjects:

- `users/<id>` — a member's own Chat account. Only that member can redeem the code; anyone else who types it fails.
- `googlechat:spaces/<id>` — the whole space. After pairing, everyone in that space is let through.

`/pair <code>` checks the sender first, then the space, exactly as an ordinary message does. A subject for an employee's bucket (`googlechat:spaces/<id>#agent:<employee>`) is stored and checked as the space itself, so pairing in an employee's own Chat app takes effect. Pairings saved by earlier versions with that suffix never let anyone through. After the upgrade they still do not, so they do not suddenly open the whole space. To pair such a space, generate a new code. Each code expires after 5 minutes, locks after 5 wrong attempts, and a subject cannot get new codes after 15 failures in total.

### Limitations

- Google Chat still cannot send generated files or images as attachments. DuDuClaw now says so plainly, without exposing host paths or directing members to a Dashboard account they may not have. Ask an administrator for the file, or use a channel that supports file delivery. Scheduled replies also process file-delivery markers before sending text.
- Drive attachments are not downloaded automatically. A validated file ID is passed to the employee so it can try the Drive read tool using its connected Google identity; access and supported file types still apply. Failed direct-attachment downloads are reported instead of saving an HTTP error response as a file. Pasted Drive links are unchanged.
- Proactive sends require a `spaces/<id>` target. A 403/404 response explains that the app may not be in the conversation: ask the recipient to message the app once or ask an administrator to install it. Rate limits and server failures are reported as temporary failures. This does not resolve people to DM spaces or send as a user.

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
| Google Chat | Chat markup (`*bold*`, `<url\|text>`) | Key–value list with bold row titles | placeholder + edit-in-place |
| MS Teams | markdown activity | monospace fence | `typing` activity every 3 s |
| WebChat | raw markdown (dashboard renders) | native | `progress` WS events |

## Group context setup

To retain ordinary group discussion, an Admin must enable each group in Channels → Group settings. Telegram bots need BotFather `/setprivacy` disabled (or appropriate bot-admin delivery). Slack apps need `message.channels` / `message.groups` events and `channels:history` / `groups:history` scopes, followed by reinstalling the app and inviting it to the channel. Discord needs MESSAGE CONTENT INTENT. Google Chat and Teams may only include messages that mention the app; Teams full channel delivery requires RSC such as `ChannelMessage.Read.Group`. See [group context](../features/72-group-context.md) for retention, employee authorization and platform limitations. Real platform delivery remains unverified for this change.

### Google Chat replies and preview links

Direct messages reply in the main view by default. Messages written inside a thread (`threadReply = true`) stay in that thread; space replies keep their existing thread behavior. In **Channels → Google Chat → Behavior → DM reply placement**, choose **Main view** (default) or **Always in thread**. Only dashboard administrators can change this preference. It is stored in the channel settings database; do not add it to `config.toml`. Conversation sessions still cover the whole space.

Google Docs/Sheets smart-chip links are included as untrusted message text, with duplicate links removed (up to 10 HTTPS links, each at most 2048 bytes; control characters are rejected). The gateway does not fetch these links. Chat read tools return a `links` array under the scanned data envelope. Missing names use readable placeholders; resource names and stored attachment paths are machine fields and must not be shown to users. Attachments retain their original filenames. No additional OAuth scopes are required.
