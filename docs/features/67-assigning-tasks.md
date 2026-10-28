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

## In the dashboard: the 交辦任務 page

The **New task** button (sidebar, and the centre ＋ on a phone) and every other "give this to an AI employee" entry open the `/assign` page in both editions. It replaced the earlier pop-up form and has two tabs.

**Standard** hands over a skill. The list shows the skills at least one AI employee you can see has switched on: **Recently run** first (newest first, with the time and who ran it), then **Other skills** by name. Pick one, optionally add one line (up to 500 characters) and files, and press **Assign task**. The gateway decides who runs it, the same way the footer line predicts: among the employees holding the skill with it switched on (a copy that failed the install scan is skipped), the one who ran it most recently; if nobody has run it yet, the first by employee id. You need the same right on that employee as for any assignment (Admin, an Owner or Operator binding, or a team grant that derived Operator); otherwise the request is refused rather than sent to someone else. The goal's description starts with the skill, your line and the file paths, followed by the skill's full text (when the text does not fit, the opening part is included and the full text is saved as a file next to the attachments). Its acceptance criteria are the skill's own description plus your line. RPC: `tasks.skill_assign` (`skill_name`, optional `note`, optional `attachments`).

**Complex task** is the full form: 問一問 / 交辦 / 想一想, the employee and data-scope chips, **When**, what done looks like, the output format and more settings. Entries that already know the employee or the goal (an employee card, a gallery card) open this tab filled in. With 問一問, the draft and any files go to the chat page as an unsent message.

**Files.** Both tabs take up to 10 files, each up to 20 MB and 50 MB together. They are saved into the running employee's own file area (`agents/<id>/attachments/`) and their full paths are added to the task description under 附件 so the employee can open them. A file that is not valid or is over a limit refuses the whole request before anything is created. A scheduled assignment saves its files once, when you create the schedule; every run points at those copies. Because the dashboard's live connection refuses messages over 1 MB, a request with files goes to `POST /api/tasks/rpc` (signed-in session only), which runs the same checks as the live connection.

## Scheduling an assignment

Work can be handed over for later or on a repeat. Nothing is created when you set it up; at each due time the gateway creates the task (or goal) on your behalf, exactly as if you had assigned it then.

**From a chat**, add a `時間:` or `排程:` segment (also `schedule:`, `at:`, `when:`):

| You send | Schedule |
|---|---|
| `/交辦 @業務專員 寫週報 \|\| 時間:每週一 9:00` | every Monday 09:00 |
| `… \|\| 排程:平日 早上9點` | Monday to Friday 09:00 |
| `… \|\| 排程:每週一、三、五 下午2點半` | Mon, Wed, Fri 14:30 |
| `… \|\| 排程:每天 18:00` / `daily 18:00` | every day |
| `… \|\| 排程:每小時` / `hourly` | every hour on the hour |
| `… \|\| 時間:明天 15:00` / `10/25 14:00` / `2026-10-25 14:00` / `tomorrow 15:00` | once |
| `… \|\| 排程:cron 0 9 * * 1` | a cron expression (Admins only) |

A repeat without a time means 09:00, and a date without a year that has already passed this year means next year. Chat times are read in the gateway host's time zone; the confirmation shows what was understood and the next run. The other `||` segments (criteria, `時限:`, `想一想`) apply to every run.

- `/交辦 排程` (`/assign schedules`) lists your schedules with their number, employee, timing and state.
- `/交辦 排程 暫停 <number>`, `恢復 <number>`, `刪除 <number>` (`pause`, `resume`, `remove`) control one of yours. The number is the 8-character `#…` from the list.

**In the dashboard**, the 交辦任務 page's **Complex task** tab has **When**: now (the default), once at a date and time, every day, weekdays, weekly on chosen days, every hour, or (Admins) a custom cron expression. Times are read in your browser's time zone. **Goals → Assigned by me** shows a **Scheduled** section with the next and last run and **Pause / Resume / Remove**; an Admin can switch it to everyone's schedules. Over RPC, `tasks.goal_create` and `tasks.create` accept `schedule` (`{kind:"once", at}`, `{kind:"daily"|"weekdays", time}`, `{kind:"weekly", days:[1..7], time}`, `{kind:"hourly", minute}`, `{kind:"cron", expr}`, or a phrase such as `"每週一 9:00"`) and `schedule_timezone` (IANA); `assign_schedules.list` (`scope: "mine" | "all"`), `.pause`, `.resume` and `.remove` (`id`) manage them.

**Your right is checked again at every run.** Creating a schedule needs the same right as assigning now. Before each run the gateway re-reads your account: if it was suspended or offboarded, the binding was removed, or the report you reached the employee through moved out of your team (or an Admin who set a cron schedule or a risk boundary is no longer Admin), the run is skipped, the schedule is paused with the reason, and you get a message on your linked channels. An employee that is disabled or deleted pauses the schedule the same way. Resume re-checks the creator's right (an Admin resuming someone else's schedule does not lend it their own rights). Only the creator and Admins can see or control a schedule; other people's numbers read as not found.

Each run records `on_behalf_of = user:<you>`, `created_via` (`dashboard` or the chat channel) and, for chat schedules, the conversation, so progress goes back there. Goals have `created_by = goal:schedule`; board tasks keep your account id in `created_by`.

Lists show at most 200 entries, with active/paused schedules first and recently completed history after them, so history cannot hide ongoing schedules' controls.

Limits:

- At most 20 active or paused schedules per person.
- Without Admin rights a schedule may run at most once an hour; Admins at most every 5 minutes.
- Raising the minimum interval also applies to existing schedules on their next run or resume. A restricted schedule pauses; create a new schedule with a longer interval.
- A pause or removal committed before the run's task is stored prevents that task. Stop an already stored task separately on its task page.
- If the previous run of the same schedule is still unfinished, that slot is skipped (`previous_open`), so runs never pile up.
- A run missed by more than 24 hours (the gateway was down) is skipped, not replayed; a shorter outage runs the missed slot once.
- A one-time schedule must be at least a minute ahead and within a year.
- Scheduled runs do not count toward the chat caps above; they appear under **Assigned by me** and in `/交辦 狀態` like any other task you assigned.

```toml
[goal_defaults]
assign_schedule_max_active = 20            # per person, 0..200
assign_schedule_min_interval_minutes = 60  # non-Admins; can only be raised (60..10080)
```

An existing unreadable or malformed configuration, or a wrongly typed scheduling limit, never falls back to looser defaults: create and resume are refused, and due schedules wait for the settings to be repaired. Listing, pausing and removing remain available; the dashboard asks for an administrator's attention.

Schedules are evaluated on the cron scheduler's 30-second tick, only in the gateway that holds the data directory's lock. Each slot's task id is derived from the schedule and the slot, so a restart or a retry never creates the same run twice.

Audit events: `assign_schedule_created`, `assign_schedule_paused` / `_resumed` / `_removed` (by a person), `assign_schedule_fired` (with the task id), `assign_schedule_skipped` (with the reason: `access_lost`, `agent_unavailable`, `schedule_restricted`, `previous_open`, `missed`, `create_failed`) and `assign_schedule_auto_paused`.

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
