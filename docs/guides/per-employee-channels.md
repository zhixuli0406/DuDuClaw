# Give an AI employee its own Telegram bot and Google Chat app

By default every AI employee is reached through the company's shared channels
(one Telegram bot, one Google Chat app). This guide sets up a channel identity
for one employee: people then talk to *that* employee through its own bot or
app, and it answers under that name.

Where: **AI employees → (employee) → Integrations → Own channels**.

| | Telegram bot | Google Chat app |
|---|---|---|
| Who can set it | admin only (the employee's owner sees the status) | admin only (the owner sees the status) |
| What you need | a token from @BotFather | a Google Cloud project, a Chat app, a service-account JSON key, and a public HTTPS address for the gateway |
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

Each employee needs its **own Google Cloud project** with its own Chat app
(Google allows one Chat app per project).

1. Create a Google Cloud project for the employee and enable the **Google
   Chat API**. Note the **project number** (digits, shown on the project
   dashboard; not the project ID).
2. In the same project create a **service account** and download a **JSON
   key**.
3. On the Chat API **Configuration** page create the app: enable interactive
   features, allow 1:1 messages and spaces, choose **HTTP endpoint URL** and
   enter the address shown on the employee's card:
   `https://<your-public-host>/webhook/googlechat/agents/<employee-id>`.
   Set **Authentication Audience** to **Project number**.
4. As an admin, paste the project number and the whole JSON key into **Own
   channels → Google Chat** and press **Connect**. DuDuClaw checks the key by
   requesting an access token; the card shows the service account it uses.
5. In Google Chat, add the app to a space or start a chat with it.

How it behaves:

- Every request Google sends to that address is verified against **that**
  employee's project number before anything is read. A request signed for
  another app, a wrong audience, a missing or forged token are refused (HTTP
  401). An address for an employee without an app, or an unknown id, answers
  the same 401, so the address does not reveal which employees have an app.
  One source address may make at most 120 requests a minute.
- Replies (and messages this employee forwards to a space later) are sent with
  this app's credentials, so the app must be a member of the space.
- To change only the project number, leave the key field empty; the stored
  key is kept.
- Removing the app closes the address at once.

## Where later messages go

Everything that answers a conversation later goes out through the bot or app
that conversation came in through:

- answers from another AI employee this employee handed the work to;
- reminders and messages the employee sends back into that chat;
- approval requests, `/goal` progress and other notices for that chat.

On Telegram this is the employee's own bot; if that bot was removed in the
meantime the message fails instead of going out through another bot. On
Google Chat the app that is a member of the space is used (the shared app
when it is there, otherwise the employee's own app).

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
  Chat apps (not "HTTP endpoint URL").
- Discord and Slack per-employee bots are set from the same page through
  **Add channel** (admin) as before.
