# Assigning tasks from a chat

Type `/交辦 @employee what to do` (or `/assign`, or the full-width `／交辦`) in a LINE, Telegram, Discord, Slack or dashboard WebChat conversation and the gateway creates a goal task for that AI employee on your behalf. Progress and the result come back to the same conversation. Since v1.76.0.

Calling an employee with `@name task` is still an ordinary conversation turn ([Calling AI staff with `@`](62-calling-ai-staff.md)). Only the command creates a task.

## The command

| You send | What happens |
|---|---|
| `/交辦 @業務專員 整理報價單` | A goal task for that employee. The reply gives the task number. |
| `/交辦 @業務專員 整理報價單 \|\| 含三家比價 \|\| 時限:24h` | The same `\|\|` segments as `/goal`: acceptance criteria, `時限:` / `duration:` (1 to 720 hours, `24h`, `3天`), `邊界:` / `risk:` (what the employee must not do; only an Admin may set it, anyone else's is dropped with a note and the deployment baseline applies), `想一想` / `plan` (a plan comes first and waits for your approval in the dashboard). Without explicit criteria the task text is the acceptance basis. |
| `/交辦` | The employees you may assign to. On LINE they come as quick-reply buttons; tapping one sends `/交辦 @name`. |
| `/交辦 @業務專員` | The bot asks what the task is. **Only your next message** in that conversation becomes the task, within 10 minutes. The message after it is ordinary conversation again. A message that calls someone with `@` instead cancels the wait. |
| `/交辦 狀態` (`/assign status`) | Your unfinished assigned tasks (at most 20), newest first, with employee, status and title. |
| `/交辦 @部分名字 …` | When the name is partial or ambiguous you get the matching employees you may assign to. |

On Telegram, `/assign@YourBot …` works in groups. In a Slack slash command use `/duduclaw assign …`; the answer there is only visible to you.

The confirmation looks like this:

```
📌 已交辦給【業務專員】，任務 #1a2b3c4d
內容：整理報價單
（依組織關係交辦：王小明的 AI 員工）
進度和結果會回到這個對話。輸入「/交辦 狀態」查看你交辦的任務。
```

The bracketed line appears only when the right to assign comes from your reporting line. Messaging-channel wording follows `[general] default_language` (zh-TW, English or Japanese).

## Who may assign to whom

| Sender | May assign to |
|---|---|
| Dashboard Admin | every active employee |
| A dashboard account (signed in to WebChat, or a messaging-app sender linked to one through a verified channel identity) | employees bound to the account at Owner or Operator, and, for a Manager, team employees whose rights through the report reach Operator |
| The same account, for an employee bound at Viewer, the conversation's default employee or an employee opened to outside contacts | nobody: you may talk to them, and the bot says you have no permission to assign them tasks |
| Anyone else (a LINE friend, a Telegram user, a website widget visitor) | nobody: the bot asks you to link your messaging account in the dashboard first (**Profile → Channel identities**), whatever name you typed, so the answer says nothing about which employees exist |

A dashboard account that names an employee it may not reach at all gets the same refusal as a forbidden `@` call. On LINE and Telegram a refusal goes out at most once every 10 seconds per sender in a conversation; repeats inside that window get no answer. Your rights are read live for every command, and checked again when the message an armed `/交辦 @name` was waiting for arrives: a binding or manager change takes effect immediately.

The gateway writes the task itself, with no AI model in between. The Personal edition has no other accounts: the owner (Admin) can use `/交辦` as usual.

## What the task records

| Field | Value |
|---|---|
| `assigned_to` | the employee |
| `on_behalf_of` | `user:<your dashboard account id>`, taken from your verified identity, never from the message |
| `created_via` | the channel (`line`, `telegram`, `webchat`, …) |
| `created_by` | `assign:<channel>` |
| `source_channel`, `source_chat_id` | the conversation, so goal-loop progress (started, rounds, needs your decision, done) is pushed back there |
| `kind` | `goal` |

Older tasks have no `on_behalf_of`; nothing is backfilled. Tasks created in the dashboard (`tasks.create`, goal create) also record you, with `created_via = dashboard`, and a `/goal` typed by a linked account records the account too (its permissions do not change).

When the employee works the task, its work prompt names the person who assigned it, by display name and department only (for example `交辦人: 王小明（業務部）`), never by account id.

The task id is derived from the incoming message where the platform gives it a stable id: the webhook event id on LINE, the message id on Telegram, and a per-send client id in WebChat. A redelivered webhook or update, a message the LINE durable ingress runs again, or the same WebChat frame sent twice never creates a second task. Discord, Slack and the webhook channels have no such id yet.

In the dashboard, the risk-boundary field of the goal form is shown to Admins only, and `tasks.goal_create` refuses a non-blank `risk_boundary` from anyone else: a task's own boundary replaces `[goal_defaults] baseline_boundary`.

## Following up

- In a messaging app, progress comes back to the conversation and `/交辦 狀態` lists your unfinished tasks.
- In WebChat the reply carries a task card; **View progress** opens the **Goals** page in its **Assigned by me** view. WebChat does not receive progress pushes.
- **Goals → Assigned by me** (`tasks.list_page` with `on_behalf_of: "me"`) lists every task you handed over, across employees. You see each task's content (description, criteria, result) even when you reach the employee only through a report. The server takes "me" from your signed-in identity; another account's id in that field is ignored. The task detail page still needs a direct binding to the employee.

## How many

From chats, each person can assign at most 10 tasks every 10 minutes and keep at most 30 of them unfinished; beyond that the reply says so and nothing is created (a plan-first task counts like any other). Tasks created in the dashboard are not counted. Change the caps in `config.toml`:

```toml
[goal_defaults]
chat_assign_per_window = 10   # per 10 minutes, 0..1000; 0 turns chat assigning off
chat_assign_max_open = 30     # unfinished tasks assigned from chats, 0..1000
```

A missing or out-of-range value uses the default.

## Group chats

Assigning works in groups, but only for linked accounts, and **everyone in the group sees the command, the confirmation (with the task text), every progress notice and the titles `/交辦 狀態` lists** (a task whose audience does not include the channel shows its number only). In a LINE group with **mention only** on, `/交辦` and `/assign` count as addressing the bot (other commands still need a mention), and so does the one message an armed `/交辦 @name` is waiting for.

## Audit

- `human_assign` (Info): each created task, with the channel, the sender (`user:<account id>`), the task id and where the right came from (`via`: `binding`, `team:<report account id>:<levels>`, `admin`).
- `human_assign_refused` (Warning): each refusal with its reason (`needs_identity`, `call_only`, `denied`, `rate_limited`); an unlinked sender is recorded as an HMAC of the platform id. Repeats from the same sender for the same employee and reason are written once a minute.
- The task's `goal_loop.created` activity row carries `on_behalf_of`, `created_via` and `via` in its metadata.

## Limits

- The wait after `/交辦 @name` is kept in memory; a gateway restart forgets it. On LINE, if the gateway restarts between accepting your next message and handling it, that message goes to the default employee as conversation (on any channel, a message handled again after a restart is treated as conversation).
- A failure is answered with a fixed sentence; the cause goes to the gateway log only.
- WebChat assignments get no progress messages in the chat; use the Goals page.
- Discord, Slack and the webhook channels (WhatsApp, Feishu, Teams, WeCom, Google Chat, DingTalk) answer with text only; only LINE and WebChat were tested end to end.
- A manager who reaches an employee only through a report sees the task's content in the **Assigned by me** list and in `/交辦 狀態`, but not on the task detail page.
