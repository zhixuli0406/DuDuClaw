# Group context for AI employees

Group context is off by default. An Admin can enable it for an individual group under **Channels → channel → Group settings**. The buffer starts with messages received after enablement; it does not fetch platform history. Tell group members before enabling retention.

## Retention and replies

Choose a recently seen conversation or enter its group ID. Discord also requires its server ID. The defaults are 50 messages and 24 hours; the maximums are 200 messages and 168 hours. The message cap applies to the whole group, including its threads. Disabling a group deletes its buffered rows. Existing conversation transcripts, sent messages, backups and platform history are separate and are not deleted by this switch.

Enabled groups retain incoming text even when **Only reply when mentioned** prevents a reply. Existing channel whitelists still apply. Passwords and keys are always masked before storage. General personal data (names, phone numbers and email addresses) is replaced only when RFC-23 redaction and its channel input source are enabled, following the existing session-history policy. With redaction off, credential-masked text is retained. A redaction error or a missing manager while redaction is configured on skips capture. The enable control shows both retention limits and this policy; when channel input redaction is off, it links to **Settings → Redaction** (`/app/system/settings?tab=redaction`). Each message has at most 2,000 characters; attachments contribute only a placeholder, without downloading their contents for this buffer. Timestamps record gateway receipt time in UTC. This buffer is plaintext local storage, not an encrypted archive. Writer connections enable SQLite secure deletion. Disabling retention and each retention batch request a truncating WAL checkpoint; busy readers or checkpoint failures are logged and a later retention batch retries. On Unix, every read/write open refuses symlinks, non-regular files and foreign-owned files, and tightens file permissions to 0600. External backups and filesystem snapshots are not covered by this deletion.

When answering, the employee receives a bounded reference block of messages since the preceding bot reply in the same conversation/thread, excluding the triggering message and later arrivals. The block is labelled as group members' content, not instructions, and is limited to 6,000 characters. The block is placed before the triggering message in the current user-side content, never in the system prompt. Input-guard matches are omitted line by line; the assembled block, chronological name/text combinations and text-only forms are scanned again across message/sender boundaries, with whitespace-joined and whitespace-stripped variants. A match drops the whole reference block for that turn and logs a warning without stopping the reply. Forged delimiter characters are escaped. Group conversations continue to exclude personal context. Bot answers are recorded after the provider accepts delivery; failed delivery does not advance the reply boundary. Progress indicators and operator command acknowledgments are not an archive of all bot activity.


Each thread is read separately; omit `thread_id` for the base conversation. Discord uses the verified parent channel as `group_id` and the thread channel as `thread_id`. Bot rows identify the answering employee and use the provider receipt ID when available. DingTalk webhooks and successful responses without a message receipt use a clearly marked `local:` ID. Each accepted text chunk is retained even if a later chunk fails. Progress messages and generated-document upload notices are outside this buffer; Telegram audio contributes a voice placeholder.

## Employee history access

`group_history_read` accepts `channel`, `group_id`, optional `thread_id`, `limit` (1–100), and optional `before` (Unix seconds). It reads only this buffer, newest first. The employee must be operational (`Active`) and have a verified host-issued identity and must be the explicit channel/thread or `allowed_channels` resolver selection. Default-agent and main-agent fallback grant no access; the main agent needs an explicit binding too. Disabled groups, other employees and unavailable data produce the same `not available` response. A read requires `messaging:read`; an existing external send-only profile is not upgraded. The tool is listed only when the employee has an enabled readable group, and reads are audited with metadata rather than message content.

Before returning history, the tool uses the same guard as the automatic reference block to scan every row, exact output and chronological cross-message combinations, including names, text and other returned string fields. Any match refuses the whole call with the uniform `not available` result without identifying a row. Successful newest-first JSON messages remain inside the same `<group_recent_context>` untrusted-data envelope; forged delimiter characters are escaped.

## Platform delivery limits

| Platform | What to configure or expect |
|---|---|
| Discord | Enable **MESSAGE CONTENT INTENT** in the Developer Portal. The gateway already requests it. |
| Slack | Subscribe to `message.channels` and `message.groups`; grant `channels:history` and `groups:history`, reinstall the app after changing scopes, and invite it to the channels. Plain `message` events feed the buffer. |
| Telegram | Use BotFather `/setprivacy` to disable privacy mode if the bot must receive ordinary group messages. Bot admins also receive broader traffic. |
| LINE | Group messages are delivered to the bot while it belongs to the group. This buffer is separate from the short-lived ingress queue. |
| Teams | May only include messages that mention the app. Receiving all team channel messages requires the relevant app installation and RSC permission, including `ChannelMessage.Read.Group`. This feature does not request extra permissions for you. |
| Google Chat | May only include messages that mention the app. The interaction webhook does not become a general history subscription by enabling this buffer. |
| Feishu / DingTalk | Includes only events the configured bot actually receives. No history backfill is performed. |

WhatsApp and WeCom are outside this feature. Platform permission grants, real installations and cross-platform delivery have not been verified by this change. Check an ordinary, unmentioned group message in an isolated test group before relying on complete context.

Official references: [Slack message events](https://api.slack.com/events/message), [Telegram privacy](https://core.telegram.org/bots/faq), [Teams RSC delivery](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/channel-messages-for-bots-and-agents), [Google Chat interactions](https://developers.google.com/workspace/chat/receive-respond-interactions).

## Discord employee choices

In the same Admin settings area, select the employees that a server's `/agent` command and employee menu may choose. An empty list disables switching. Discord server-management permission is still required, and the main agent cannot be selected through these controls. Removing an employee from the list or changing their status to Paused, Terminated, Archived or Deleted makes the stored selection ineffective on the next resolution. The settings refuse saving non-operational employees, and the choices menu filters them out. Dedicated employee bots also require an operational employee. `/config mention_only` is unchanged.
