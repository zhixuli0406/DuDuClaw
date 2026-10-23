# Calling AI staff with `@`

People talk to one AI employee by default and can hand a single message to another one by typing `@` and a name, the way you mention someone in a messaging app. Since v1.76.0 who you can call depends on who you are, and every message is routed on its own.

## How a message is routed

1. A message that calls nobody goes to the conversation's default employee: the one bound to the channel or thread, else the one whose `allowed_channels` lists the channel, else the main employee. In the dashboard WebChat, the selected employee answers; with nobody selected, the default employee does.
2. A message that calls an employee goes to that employee, **for this message only**. The next message without a name goes back to the default employee. Nothing sticks.
3. An employee can be called by `@id`, `@trigger` (the `[agent] trigger` value) or `@display name`. Matching ignores letter case and works with CJK text glued to the name (`@財務小幫手幫我算帳`). `＠` works too. An e-mail address (`ann@example.com`) is never a mention.
4. When several names could match, the longest wins (`@財務小幫手` over `@財務`). At equal length an id beats a trigger and a trigger beats a display name. A name that still belongs to several employees never routes on its own, even when you may call only one of them: you get the choice (or a refusal when you may call none). When a message mentions several employees, the first one in the text that you may call wins.
5. A partial name (`@財務`) never routes silently: you get the list of employees it could mean.
6. Without `@`, nothing switches. Writing `財務小幫手，幫我算帳` goes to the default employee.
7. A platform mention of a person (LINE's mention, Telegram's `@username` of a group member) is not an employee call, even when the person's name starts like an employee's.

## Who may call whom

| Sender | May call |
|---|---|
| Dashboard Admin | everyone |
| Dashboard user in WebChat | the default employee (as before), employees opened to outside contacts, and the employees bound to the account (any access level); a manager also reaches their team's employees (next section) |
| Messaging-app sender linked to a dashboard account (a verified channel identity) | the conversation's default employee, employees opened to outside contacts, and the account's bound employees; a manager also reaches their team's employees |
| Anyone else: a LINE friend, a Telegram user, a website widget visitor | the conversation's default employee and employees opened to outside contacts |

### Managers reach their team's employees

In the Enterprise edition each account can have one direct manager (`manager_user_id` on `users.create` / `users.update`; the dashboard screen for it comes later). Someone whose role is Manager (or Admin) may call the AI employees bound to anyone below them, down to 8 levels. A suspended report still counts (the manager can look after their employees); an offboarded one does not. Someone with the Employee role gets no team, even if people are filed under them.

Rights that come through a report stop at Operator, never Owner:

| Action | Admin | Direct Owner | Direct Operator | Direct Viewer | Manager (through a report) | Default and public employees | Unlinked sender |
|---|---|---|---|---|---|---|---|
| `@` chat, pick the employee as the WebChat partner | ✅ | ✅ | ✅ | ✅ | ✅ | `@` only, not as a picked partner (the default employee is who answers when nobody is picked) | default and public employees only |
| Hand over work (dashboard task create, goal create, task reassign) | ✅ | ✅ | ✅ | ❌ | ✅ when the report's binding is Owner or Operator | ❌ | ❌ |
| Task board listing for the employee | ✅ | ✅ | ✅ | ✅ | ✅ when work may be handed over; cards only (title, status), no content | — | — |
| Memory, conversation history, employee settings and persona files | ✅ | by binding level | by binding level | by binding level | ❌ | — | — |

When the report's binding is Viewer, the manager may only chat with that employee. The **AI staff** page and the employee settings pages still list direct bindings only.

A manager may update, reassign, cancel or remove a task they handed to a team employee from the dashboard (they created it) while they may still assign to that employee; tasks created by others still need direct Operator. A task shown as a card only carries no link to its source conversation (`channel` and `channel_link` are null).

A binding, role, status or manager change applies to the next message and the next dashboard action, with no new login and no cache window to wait out (every write invalidates the permission caches). If the user records cannot be read, access narrows to no bindings. The Personal edition has no users or managers and behaves as before; after a downgrade from Enterprise, reporting lines left in the data are ignored.

An employee is opened to outside contacts on its edit page (**Callable by outside contacts**, `[channels] public_trigger = true` in `agent.toml`). It is off by default, only an Admin can change it, and employees cannot change it themselves.

A dashboard account (signed in to WebChat, or a messaging-app sender linked to one) who calls an employee they may not call is refused before any model call: the reply says you have no permission to call that employee. It does not name the employee, but it does tell you the name belongs to one. An unlinked sender (a LINE friend, a Telegram user, a website widget or Odoo Live Chat visitor) gets no refusal: a name they may not call is treated like a name that matches nobody, plain text for the default employee, and it does not count as addressing the bot in a mention-only group, so outsiders cannot use it to find out which employees exist. The refusal is written to the security audit log (`trigger_route_refused`) with the dashboard account, or, for an unlinked sender, a 16-character HMAC of the platform id under a key kept in this installation (`trigger_audit.key`, owner-only, created once); the raw platform id is never logged. Repeated refusals from the same sender for the same employee are audited once a minute. Allowed switches are logged at info level with where the right came from (`via`): `binding`, `team:<report account id>:<levels>`, `default`, `public` or `admin`. An Admin account is recorded as `user:<account id>`.

**In a group chat everyone sees the answer.** A linked member who calls an employee bound to their account in a LINE or Telegram group gets the answer in the group, where every member can read it. Bind restricted employees to people with that in mind, or have them use a one-to-one chat.

## The `@` list

- **WebChat**: typing `@` in the message box opens a list of the employees you may call (avatar, name, department). The list comes from `agents.callable`, the same rule every message is checked against; when it has more than one source it is split into **My AI staff**, **My team: <name>'s AI staff** and **Public AI staff**. The staff strip above the conversation includes team employees too. It filters as you type; arrow keys move, Enter or Tab inserts `@name `, Escape closes it. Each reply from an employee other than the one you are talking to is labelled with who answered.
- **LINE**: send `@` alone (or `@` with part of a name) and the bot answers with the list as quick-reply buttons, at most 13 at a time; a longer list ends with a "more" button for the next page. Tapping a name sends `@name`.
- **Telegram** (shared bot): the same list comes as inline buttons, or as text when no button fits.
- Other channels answer with a text list.

The list, the refusal and the "say the task" prompt go out only after the same access gate (blocklist, pairing) and human-takeover checks as an AI reply. A list and a refusal are each sent at most once every 10 seconds per sender in a conversation; repeats inside that window get no answer (this saves the platform's message quota). Messaging-channel wording follows `[general] default_language` (zh-TW, English or Japanese); WebChat shows it in the viewer's dashboard language.

## Calling an employee and then saying the task

On messaging channels (LINE, Telegram, Discord, Slack and the others), a message that is only `@name` (or tapping a name in the LINE or Telegram list) gets the reply "say what to hand to name". **Only your next message** in that conversation goes to that employee, within 10 minutes; after that, routing is normal again. Your access is checked again when that message arrives. Another member's message in the same group is not affected.

The waiting state is kept in memory, so a gateway restart forgets it and the next message goes to the default employee. On LINE, which employee an accepted message goes to is decided once, when the message is accepted, and stored with it; checks before it runs and before each reply reuse that and only confirm you may still call the employee. A retried message never re-arms a waiting call. If the employee can no longer take the message, it is quarantined and the bot asks you to send it again.

## Each employee keeps its own conversation history

A called employee answers in its own session inside the conversation (`<conversation>#agent:<id>`), on every channel and in WebChat. Its history, saved secrets and pending goal confirmations never mix with the default employee's. The default employee does not see what the called employee answered, and the other way round.

## Group "mention only"

- **LINE**: with **mention only** on, the bot answers only a mention of the bot itself, messages that call an employee by `@` (or ask for the `@` list), and the one message a called employee is waiting for. Before v1.76.0 this setting silenced the whole group. LINE does not tell a bot which of its own messages was quoted, so quoting the bot does not count.
- **Telegram**: the shared bot also accepts employee mentions in mention-only groups, besides its own `@username` and replies to it. Per-employee bots keep their own `@username` rule.

## Who answered

- WebChat reply frames (`assistant_done`) carry `agent` (id) and `agent_name`.
- On LINE and Telegram, a reply from an employee other than the conversation's default starts with `【name】`. Replies from the default employee have no prefix. The LINE bubble header now names the employee that answered (it used to always name the main employee).
- Contract checks (`CONTRACT.toml`) and secret restoration run as the employee that answered, in its session.

## Not covered

- Discord, Slack and the other channels use the same rules, the text list and the one-shot call, but they have no buttons and no native-mention filtering. Only WebChat and LINE were tested end to end; Telegram is covered by unit tests.
- A sender with a linked dashboard account on other channels is mapped only when the identity was verified through the channel login flow.
- A manager hands work to team employees from the dashboard for now; a chat command for it is not built yet. On the task board a manager sees team employees' tasks as cards only.
