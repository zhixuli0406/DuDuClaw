# Approvals and answers in chat channels

When an AI employee wants to take a high-risk action (today: a high-risk computer-use step), it can ask for a decision in the chat it is answering. The request has a full ID. Reply in the same bot account and the same conversation or thread with that ID:

```text
確認 <full request UUID>     approve <full request UUID>
取消 <full request UUID>     deny <full request UUID>
回答 <full request UUID> <answer>     answer <full request UUID> <answer>
```

A message is treated as a decision **only** when its first word is one of these six verbs and its second word is a complete request UUID. Everything else is ordinary conversation and goes to the employee as before:

- 「確認」 on its own (for example after the employee asked "shall I send it?"), "approve the Q3 budget", "deny it": these reach the model unchanged and the bot does not answer them itself.
- Replying to an older decision card with a bare verb (「同意」, 「拒絕」, 「取消」, `approve`, `deny` — the watch-friendly text reply) keeps working exactly as it did, through that card's own handling.
- A bare 「是」, "yes", A/B, or an incomplete ID never decides any request.

Small slips in a command that does carry a complete ID are still treated as the decision rather than passed to the employee: several spaces or a full-width space between the words, and closing punctuation right after the ID or at the end (`確認 <id>。`, `approve <id>.`, `確認 <id>！`). The ID may be written in capitals or without hyphens. The verb itself must still be the first word, spelled exactly as above (`Approve`, `確認：<id>` or simplified 「确认」 are not decisions).

There are two kinds of request. A **question** collects an answer (`回答` / `answer`); the answer is stored as data and never authorizes an action. An **approval** (`確認` / `取消`, `approve` / `deny`) authorizes or refuses one action. `確認 <question-id>` and `回答 <approval-id>` are both refused.

## Who can decide in a channel

All of these must hold:

1. **The person who triggered the turn.** The request is bound to the platform account of the bot, the conversation or thread, and the person whose message started the turn. Another member of the same group, the same person in another chat, another bot of the same deployment, or a display name cannot decide it. In Telegram, anonymous group administrators, members posting "as the channel" and automatic forwards from a linked channel share placeholder sender IDs; their messages never create a request and never decide one. A forwarded message is not a decision either: forwarding someone's 「確認 <id>」 is not the forwarder saying it.
2. **A verified Admin or Manager, when the deployment has dashboard users.** As soon as `users.db` contains any user, the person must have a **verified** channel identity linked to an Active Admin or Manager account. An Employee, an unlinked account or an unverified link is refused. A Manager who is not the person who triggered the turn cannot decide from the channel; use the dashboard.
3. **Solo deployments.** Only when there are no dashboard users at all may the person who triggered the turn decide their own request.
4. **Channel access still applies.** The channel's current allowlist, blocklist, pairing, channel and server (guild) settings are read again at decision time; a sender those settings refuse cannot decide. With an `allowed_users` list and pairing off, both the sender and the conversation must be on the list; with pairing on, either one being admitted is enough. This applies to ordinary messages too.

Answers to questions follow rules 1 and 4: only the person who triggered the turn can answer, the channel access settings apply, and no Admin or Manager role is needed.

When a decision is refused before the bot knows the sender is the bound person (unknown ID, a request that belongs to another account, conversation or person, a sender the channel settings refuse), the reply is always the same sentence, so it cannot be used to find out which request IDs exist. The access check runs before the request is looked up, for typed commands and for buttons alike.

When several DuDuClaw bots share a group, each one that receives a decision command answers it once (the bots it does not belong to with the same refusal). This is deliberate: staying silent only for requests that belong to another bot would itself reveal which IDs exist.

## What each request is bound to

An approval binds the requesting employee, the bot account, the person, the conversation or thread, a digest of the exact operation, the policy and an expiry. With a task it also binds the task contract and its revision; editing the contract, pausing or restarting requires a new approval, and recreating a task with the same ID and content does not revive an old approval. Expired, withdrawn, unknown-format or corrupt bindings are refused. Older requests keep their own domain flow and gain no new resume rights.

## Computer use

The high-risk confirmation is created by the trusted channel entry the employee is currently answering and delivered with that entry's real account and thread. Telegram uses the bot ID verified by `getMe`; Slack uses the team, user and app IDs from `auth.test` and `bots.info` and checks them against each event; Discord uses the connected application ID; LINE uses the signature-verified destination and source. Credentials stay in memory, are never written to the approvals database and never fall back to a global bot.

After approval and before the action, the gateway checks again the employee's capabilities, execution grants, task cancellation, expiry, stop and pause, and observes the screen and focused window again; if any of these changed the request is invalidated and has to be observed and approved again. Removing a channel from the allowlist does not withdraw an action that was already approved; cancel the request, stop the session or revoke the grant for that. Typed text is stored only as a digest and a character count. After a gateway restart, old screen approvals are marked `restart_requires_reobserve` and old coordinates are never replayed.

`~/.duduclaw/threat_level` is the operator kill switch: a missing file means GREEN; `GREEN`, `YELLOW` and `RED` are read in any case, with surrounding whitespace and a leading UTF-8 BOM ignored. A file that exists but cannot be read, is empty or holds anything else is read again twice, 50 ms apart (an `echo GREEN > threat_level` empties the file for a moment), and is treated as RED if it is still unclear. Write it atomically (a temporary file, then rename) to avoid that window.

### Where decisions are handled

Telegram polling and Slack Socket Mode handle a full-ID decision on the receiver, before the conversation queue, so a computer-use step that waits for its own confirmation never blocks it (this includes `@bot 確認 <id>` in Telegram groups; only a mention of this bot at the very start is removed, in any letter case). LINE moves decision events to a separate durable worker. Discord gives decision messages and decision buttons their own small pool of handler slots, separate from the ten used by ordinary replies. A message or button only gets one of those slots after the sender has passed the channel access check, and a decision message never downloads its attachments, so a stream of fake decisions cannot keep the slots busy. Attachment downloads for ordinary messages stop after 30 seconds. Ordinary work is bounded per conversation; when the queue is full the bot says the message was not accepted.

Discord buttons use the platform-verified channel ID, type and guild. For every guild message the bot first looks up the current channel's ID, guild and type; if the lookup fails or disagrees, the message is not processed. Direct messages use the platform's DM information.

## Channels that cannot ask or decide

These entries cannot create these confirmations and cannot decide bound requests; a high-risk computer-use step started from them is refused:

- WhatsApp, Feishu, Microsoft Teams, Google Chat, WeCom, DingTalk, WebChat
- the Telegram Mini App
- Slack and Discord slash commands
- scheduled (cron), reminder and delegated work

Their existing card buttons and text replies keep deciding the older request types they always handled. Typing 「確認 <id>」 in one of them does nothing to a bound request; the message goes to the employee like any other.

Bound requests do not get the older reminder cards with buttons; the request is only decided in its own conversation by full ID, or in the dashboard.

## Decided only in the dashboard

The gateway's dashboard-only list is the authority; today it contains:

- knowledge reviews (held memory claims)
- LINE inbox operator actions requested with `duduclaw ops channel-ingress`
- workflow activation
- reconciling an operation whose result is unknown (`approvals.resolve_uncertain`, Admin)

A channel reply to one of these never takes effect.

## Slack: the `users:read` scope

Slack decisions are bound to the bot's own verified account, which needs `bots.info`, which needs the **`users:read`** scope on the bot token. Without it the account cannot be verified and every Slack decision and computer-use confirmation on that bot is refused (fail closed). The gateway then:

- answers a decision sent to that bot with a sentence saying it cannot decide in Slack and to use the dashboard;
- writes one Activity Feed row (`slack_decision_identity_unavailable`) the first time per bot and gateway process;
- records the bot in `<home>/state/slack_decision_identity.json`, shown by `duduclaw doctor` in the "Slack 決定身分" row.

Add `users:read`, reinstall the app to the workspace and let the gateway reconnect; the doctor row clears after a successful connection.

## Execution results

Approval is not execution. The operation ledger records `prepared`, `executing` and `succeeded` / `failed` / `uncertain`. Execution holds a lease with an increasing fence; an operation whose process ended without a receipt becomes `uncertain` and is never re-sent automatically.

An Admin can page through the ledger with `approvals.operations` (`before_rowid`, `limit` 1–500) or pass an `operation_id`. `approvals.resolve_uncertain` takes the operation ID, the current fence, success or failure, the real receipt and a reason; it only settles the state, it never runs anything again. A dashboard decision is a separate management permission and cannot answer a question on behalf of the channel user. `approvals.list` returns binding details and answers only to people allowed to decide the request, using their current role.

On Unix `approvals.db` is created 0600 and existing files and WAL/SHM are tightened; symlinked database or sidecar files are refused. There is no automatic retention job for the ledger in this version.

The operation `run_id` is created by the server: durable ingress uses the accepted event's run ID (`run_origin_kind=ingress`), other trusted entries use the computer-use session ID (`computer_session`).

## Known limits

- A person or the operating system can change the desktop between the last screen check and the input; the gateway re-observes after approval, audit and ledger writes, and refuses known stale screens and revoked permissions, but cannot freeze an external desktop atomically.
- Not verified on a real Telegram supergroup: whether a reply inside a non-forum supergroup carries `message_thread_id`. If it does, a confirmation sent as a reply may not match a request triggered outside that reply thread; it is refused (never approved by mistake). Send the confirmation as a normal message.
- Slack role lookup uses the channel and user ID without the workspace. The request itself is still bound to the bot's team and app, so this affects only which dashboard role is looked up in a deployment with several Slack workspaces.

Also in [繁體中文](zh-TW/durable-channel-decisions.md) and [日本語](ja-JP/durable-channel-decisions.md).
