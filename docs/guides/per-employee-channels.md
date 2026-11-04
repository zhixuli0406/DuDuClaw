# Give an AI employee its own Telegram bot and Google Chat app

Google Chat channel credentials authorize the Chat application (`chat.bot`). They do not connect Gmail, Drive or Calendar. Workspace data tools have their own [Google setup](google-setup.md). Saving a channel in the dashboard applies live; restart after editing channel configuration by hand.

By default every AI employee is reached through the company's shared channels
(one Telegram bot, one Google Chat app). This guide sets up a channel identity
for one employee: people then talk to *that* employee through its own bot or
app, and it answers under that name.

Where: **AI employees → (employee) → Integrations → Own channels**.

| | Telegram bot | Google Chat app |
|---|---|---|
| Who can set it | admin only (the employee's owner sees the status) | admin only (the owner sees the status) |
| What you need | a token from @BotFather | a Google Cloud project, a Chat app, a service-account JSON key, and public HTTPS |
| Restart needed | no | no |
| Without it | the shared Telegram bot | the shared Google Chat app |

One bot or one Chat app belongs to one employee. Saving a bot or app another
employee already uses is refused.

## Telegram

1. In Telegram, open **@BotFather**, send `/newbot` and follow the prompts.
2. BotFather replies with a token such as `123456789:AAE…`. Copy all of it.
3. Paste it into **Own channels → Telegram** and press **Connect**.

DuDuClaw asks Telegram who the bot is before saving. A token Telegram refuses
is not saved; if Telegram cannot be reached at that moment the token is saved
and the bot keeps retrying. The card then shows **Connected as @your_bot**, or
the reason it is not connected (for example "Bot Token 無效" after the token
was revoked in BotFather — paste the new one).

How it behaves:

- Private chats and groups both work. In a group the bot answers only when it
  is mentioned (`@your_bot`), replied to, or given a command; change that per
  group with the channel's behaviour settings.
- Every message to this bot is answered by this employee. `@` calls to other
  employees are a feature of the shared bot only.
- Each employee keeps its own conversation history with a person, even when
  the same person also talks to other employees' bots.
- Who may talk to the bot follows the Telegram channel's access rules
  (allowed / blocked users, pairing) exactly as for the shared bot.
- The "thinking…" status message works the same way as on the shared bot.
- If the shared bot uses the same token, the employee's bot takes over that
  bot; removing the employee's bot gives it back to the shared configuration.

## Google Chat

Each employee needs its **own Google Cloud project** and Chat app (one Chat app per project). Follow the [shared setup prerequisites, key-policy guidance and Cloud Shell flow](channels-googlechat-teams.md#google-chat), adding `--agent <employee-id>`:

The automatic import requires a running DuDuClaw gateway and an administrator login (including MFA when required).

```bash
duduclaw channels googlechat bootstrap --agent alice --endpoint-url https://chat.example.com/webhook/googlechat/agents/alice
```

`--agent` affects naming in the provisioning script and selects the employee when the CLI imports. It does not create the employee; create it first. Each employee still uses its own project. Complete the printed Google Chat Configuration page: app name/avatar/description, interactive features, 1:1 and spaces, connection values, and visibility to your own email. Test a direct message and a space reply. For Cloud Shell, download the key securely and run the printed import command on the DuDuClaw host, retaining `--agent alice`.

Use a dedicated new project for each app. **Uncheck “Build this Chat app as a Google Workspace add-on”** when creating the app.

### Appendix: manual setup

In **Own channels → Google Chat → Manual setup**, enter the project number and full JSON key. Paste `https://<public-host>/webhook/googlechat/agents/<employee-id>` in Google configuration and select **Project number** as Authentication Audience. Save and test a message. Only administrators can configure or remove credentials. Keys are encrypted; an empty key field retains the saved key. The app must be a member of the space it replies to.

## Where later messages go

Everything that answers a conversation later goes out through the bot or app
that conversation came in through:

- answers from another AI employee this employee handed the work to;
- reminders and messages the employee sends back into that chat;
- approval requests, `/goal` progress and other notices for that chat.

On Telegram this is the employee's own bot; if that bot was removed in the
meantime the message fails instead of going out through another bot. On
Google Chat the app that most recently received a message from that space is
used; if Google refuses the send because that app has left the space, it is
forgotten and the next send uses another app seen there.

## Security notes

- Tokens and keys are stored encrypted in the employee's `agent.toml`
  (`bot_token_enc`, `service_account_json_enc`). If the machine's encryption
  key cannot be used, the save is refused instead of storing plaintext.
- The status shows the bot username and the service-account email, never the
  token or key.
- Every bind and removal is written to the security audit log
  (`agent_channel_bound`, `agent_channel_removed`).

## Not covered

- Reminders created before this release keep sending through the shared bot.
- Only the "Project number" authentication audience is supported for Google
  Chat HTTP apps (not "HTTP endpoint URL").
- Discord and Slack per-employee bots are set from the same page through
  **Add channel** (admin) as before.
