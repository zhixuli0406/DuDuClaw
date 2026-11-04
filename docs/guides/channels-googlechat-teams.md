# Google Chat & Microsoft Teams channels

Google Chat channel credentials authorize the Chat application (`chat.bot`). They do not connect Gmail, Drive or Calendar. Workspace data tools have their own [Google setup](google-setup.md). Saving a channel in the dashboard applies live; restart after editing channel configuration by hand.

DuDuClaw supports Google Chat and Microsoft Teams as first-class channels
(alongside Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat).
Teams and Google Chat need a public HTTPS gateway.

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

### Before you start

Use a Business or Enterprise Google Workspace account with Chat enabled and an administrator who allows Chat apps. You need permission to create or directly own a Cloud project, enable Chat API, and create a service account and key. Local setup needs Bash, Python 3, `gcloud`, your own `gcloud auth login`, and a public HTTPS gateway URL; Cloud Shell supplies the command-line tools. A Pub/Sub mode that works without a public URL is planned for a later release.

### One-step setup

Use a dedicated new Google Cloud project (the default). Run this command on the DuDuClaw host:

The automatic import requires a running DuDuClaw gateway and an administrator login (including MFA when required).

```bash
# HTTP: replace the host with your public gateway address.
duduclaw channels googlechat bootstrap --endpoint-url https://chat.example.com/webhook/googlechat
```

The command creates a project, enables Chat API, and prepares a service account before creating its private JSON key as the last cloud step. It then imports the settings. `--project-id your-project-id` reuses a project you directly own; group or inherited ownership is conservatively refused. Optional flags: `--app-name`, `--agent`, `--out`, and `--dry-run` (no cloud or local changes). The default project ID is `duduclaw-chat-<random6>`. The private output directory stores `service-account.json` with mode 0600. A rerun reuses a matching key. Imported credentials are encrypted at rest. If a later script step fails after key creation, it attempts to revoke the new cloud key and remove the local file. Revocation failures or an unreadable key ID are reported for manual follow-up. If the script succeeds but CLI import fails, the CLI keeps the key file and exits non-zero. The new service-account key remains valid; the error includes the file path and exact commands to retry the import or revoke the key manually. The CLI never revokes keys or deletes key files automatically.

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

### Key policy

If key creation reports `FAILED_PRECONDITION`, ask an Organization Policy Administrator to review the project exception. Organizations created on or after 2024-05-03 commonly enforce key-creation restrictions. The script prints the commands and stops; it does not change policy:

```bash
gcloud resource-manager org-policies disable-enforce iam.disableServiceAccountKeyCreation --project=your-project-id
gcloud resource-manager org-policies disable-enforce iam.managed.disableServiceAccountKeyCreation --project=your-project-id
```

For the managed constraint, an administrator should verify support in their installed gcloud and may need the Organization Policies console. See [Google's key policy guidance](https://cloud.google.com/iam/docs/troubleshoot-org-policies).

### Appendix: manual setup

Create a project, enable Chat API, create a service account and JSON key, and configure the Chat page as above. In **Channels → Add Google Chat → Manual setup**, enter the project number and full JSON key. Save and test a message. HTTP verifies the inbound JWT against `chat@system.gserviceaccount.com` and your project number. Unsupported Workspace add-on payloads produce a diagnostic asking you to uncheck the add-on option.

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

## Group context setup

To retain ordinary group discussion, an Admin must enable each group in Channels → Group settings. Telegram bots need BotFather `/setprivacy` disabled (or appropriate bot-admin delivery). Slack apps need `message.channels` / `message.groups` events and `channels:history` / `groups:history` scopes, followed by reinstalling the app and inviting it to the channel. Discord needs MESSAGE CONTENT INTENT. Google Chat and Teams may only include messages that mention the app; Teams full channel delivery requires RSC such as `ChannelMessage.Read.Group`. See [group context](../features/72-group-context.md) for retention, employee authorization and platform limitations. Real platform delivery remains unverified for this change.
