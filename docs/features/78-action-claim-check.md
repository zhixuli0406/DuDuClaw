# Action claim check

An AI employee sometimes writes "I've sent the email" or "已建立行程" even though no tool call actually did it. The action claim check compares what a chat reply says was done with the employee's tool-call record and tells company staff when a claim has no success record behind it.

## What is checked

The check looks for first-person completion claims in Traditional Chinese and English:

| Action | Example phrases | What counts as a success record |
|---|---|---|
| Sending an email | 已寄出, 已寄送, 信已寄, 郵件已發送, 已發信, "I've sent the email", "Email sent" | A record of a mail tool that says the mail was actually sent. A draft, a mail waiting for confirmation, or any other tool's record does not count |
| Creating a calendar event | 已建立行程, 已排入行事曆, 已新增活動, "Event created", "Added to your calendar" | A successful calendar event creation |
| Writing to a spreadsheet | 已寫入試算表, 已新增到試算表, 已更新試算表, "I've added a row to the sheet", "Appended to the sheet" | A successful spreadsheet append |
| Writing to a Google Doc | 已寫入文件, 已更新文件, "Appended to the doc" | A successful Google Docs append |
| Creating a task | 已建立任務, 已新增待辦, 已加入任務看板, "Task created" | A successful task creation on the task board or in Google Tasks |

Negations, plans, conditions and questions are not treated as claims. The check looks at the whole sentence around a phrase; a sentence ends at 。, ．, ., !, ！, ?, ？ or a line break, and commas do not end it.

- A sentence that ends with a question mark, or contains 嗎, 是否 or 有沒有 anywhere, is a question.
- A negation, condition or future word anywhere before the phrase in the same sentence means it is not a claim: 尚未, 還沒, 仍未, 並未, 沒有, 不會, 不能, 無法, 是否, 如果, 若, 一旦, 等, 待, 將, 會在, 之後, 才會, and the English words "not", "never", "no", "once", "if", "after", "when", "whether", "until", "unless", "will", "would", "should", "can", "could", "going to", "be sent", "to be".

Examples that are not claims: 尚未寄出, 是否要寄出, 寄出後, 待確認後才會寄出, 已排入待確認, 草稿已建立, 如果主管同意，郵件已發送, "not sent", "once sent", "Was the email sent to Bob and Carol yesterday afternoon?", "Once, the email was sent, nothing else is needed."

Only records of the same AI employee in the same conversation count, from the current turn or from the previous 30 minutes of that conversation, so "I created it a moment ago" is backed by the earlier turn. Failed calls and records without a conversation never count. When a turn has no conversation, these actions are not checked at all.

The older agent-management claims ("created agent …", "updated SOUL.md", "sent a message to …") are checked as before.

## The notice

When at least one of the actions above has no success record, one short paragraph is added at the end of the reply. It is saved with the reply, so the AI employee also sees it in the next turn.

- No success record (Traditional Chinese): `（系統提醒：這則回覆提到已完成「{動作}」，但系統在這段對話中查不到對應的成功紀錄，請再確認是否真的完成。）`, where `{動作}` is one of 寄信, 建立行事曆活動, 寫入試算表, 寫入文件, 建立任務.
- Mail only saved as a draft or waiting for confirmation (Traditional Chinese): `（系統提醒：這封信目前只是草稿或待確認，尚未寄出；需要管理員在儀表板確認後才會寄出。）`

The notice follows the employee's reply language: Traditional Chinese, Simplified Chinese, English or Japanese. A reply gets at most one notice, naming at most three actions. The notice never contains tool names, file paths or ids.

## Who sees the notice

Only company staff:

- a signed-in dashboard account in WebChat;
- a one-to-one chat in a messaging app (LINE, Telegram, Slack and others) whose sender's verified identity belongs to an Admin or to an account bound to this AI employee.

Everyone else gets the reply unchanged: Odoo website live chat visitors, website-widget visitors, group chats (other members may be outside the company), senders without a verified identity, background sessions and conversations whose origin is unknown.

## Settings

| Key | Values | Default |
|---|---|---|
| `config.toml [action_claims] mode` | `"notice"`, `"shadow"` | `"notice"` |

`notice` adds the notice for staff. `shadow` only records. There is no switch on a settings page; an Admin sets it in **Settings → Advanced → Advanced config editor** ([dashboard settings](../guides/dashboard-settings.md#action-claim-check-advanced-config-editor-only)):

```toml
[action_claims]
mode = "shadow"
```

The value is read on every reply; no restart is needed. Any other value is read as `shadow` and a warning is written once to the gateway log.

## What is recorded

Recording is the same in both modes and depends on who is reading the reply (the same rule as [Who sees the notice](#who-sees-the-notice)):

- **Company staff**: a claim without a success record is written to the security audit log (`tool_hallucination`), then to the gateway log and sent as an [Autopilot rule engine](23-autopilot-engine.md) event. The limit is per AI employee and per action: after a record is written, repeats of the same action by the same AI employee in the next 10 minutes are counted instead of recorded, and the count is included in the next record (`suppressed_since_last_row`). The 10 minutes start only once a record is actually written; if writing fails, the next repeat is recorded again.
- **Everyone else** (website visitors, group chats, senders without a verified identity, background sessions, unknown origins): the reply is not checked against the tool-call record at all; such claims are only counted. The gateway log gets at most one summary line per minute with the running count and no reply text. Nothing is written to the security audit log and no Autopilot event is sent, so a visitor cannot fill the audit log, or keep the gateway busy, by getting the AI employee to repeat these phrases.

A staff reply is not checked, so it is sent unchanged and nothing is recorded for it, when:

- the gateway is busy: at most 4 checks run at the same time, and a reply that finds all 4 in use is not checked;
- the tool-call log cannot be read within 2 seconds (for example while it is locked by a busy writer), or the reply waited 2 seconds without an answer.

The notice can still be added, but no record is kept, when:

- the security audit log cannot be written within 2 seconds;
- the in-memory table that keeps the 10-minute limits is full (1,024 AI employee and action pairs that are all inside their 10 minutes or still have an uncounted repeat): a new pair is not recorded. Pairs whose 10 minutes have passed with nothing pending are cleared first. Records written later include the running number of such skips (`limiter_overflow_total`).

A staff reply can therefore carry no reminder even though its claim has no success record. Every skip is counted, and the gateway log gets at most one summary line per minute with the counts (no reply text).

## Known limits

- The check matches phrases with regular expressions. It understands Traditional Chinese and English wording only and can miss other phrasings.
- Quoted text and code blocks in the reply are not excluded. A reply that quotes "已寄出" can trigger a notice.
- Wording that does not name the object can match something else; for example "您的包裹已寄出" reads as a sent mail.
- The check prefers missing a claim to flagging a sentence that is not one. A real claim that shares its sentence with an earlier word such as "no", "can", 等 or 將 is not flagged.
- Only the first 16,000 characters of a reply are checked.
- The notice is ordinary reply text. It cannot be told apart from text the AI employee wrote itself, so a manipulated AI employee could print a line that looks the same. Treat the notice as a hint; the records in the dashboard are authoritative.
- Scheduled tasks and messages between AI employees are only recorded, never given a notice.
- Mail is never sent in the same turn: sending waits for an Admin's confirmation in the dashboard. A "sent" claim is therefore always flagged unless a record shows the mail was actually sent, usually with the draft wording above.
- The check reads the most recent part of the tool-call log (up to 4 MB, plus the rotated file when the current one is smaller). On a very busy installation an older record in the 30-minute window can be missed and the notice shown even though the action happened.
- Audit rows are limited to one per employee and claim family per 10 minutes, with one write in flight at a time. If a write stalls for more than 30 seconds after its row was already written, the next occurrence takes over and can write a second row for the same window that repeats the same suppressed count.

See [Security defense](05-security-defense.md) for the other guards.
