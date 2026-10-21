# Calling AI staff with `@`

People talk to one AI employee by default and can hand a single message to another one by typing `@` and a name, the way you mention someone in a messaging app. Since v1.76.0 who you can call depends on who you are, and every message is routed on its own.

## How a message is routed

1. A message that calls nobody goes to the conversation's default employee: the one bound to the channel or thread, else the one whose `allowed_channels` lists the channel, else the main employee. In the dashboard WebChat, the selected employee is the default.
2. A message that calls an employee goes to that employee, **for this message only**. The next message without a name goes back to the default employee. Nothing sticks.
3. An employee can be called by `@trigger` (the `[agent] trigger` value), `@display name` or `@id`. Matching ignores letter case and works with CJK text glued to the name (`@財務小幫手幫我算帳`). `＠` works too. An e-mail address (`ann@example.com`) is never a mention.
4. When several names could match, the longest wins (`@財務小幫手` over `@財務`). When a message mentions several employees, the first one in the text that you may call wins.
5. A partial name (`@財務`) or a name shared by two employees never routes silently: you get the list of employees it could mean.
6. Without `@`, nothing switches. Writing `財務小幫手，幫我算帳` goes to the default employee.

## Who may call whom

| Sender | May call |
|---|---|
| Dashboard Admin | everyone |
| Dashboard user in WebChat | the employees bound to the account (any access level) |
| Messaging-app sender linked to a dashboard account (a verified channel identity) | the account's bound employees, plus the conversation's default employee and employees opened to outside contacts |
| Anyone else: a LINE friend, a Telegram user, a website widget visitor | the conversation's default employee and employees opened to outside contacts |

An employee is opened to outside contacts on its edit page (**Callable by outside contacts**, `[channels] public_trigger = true` in `agent.toml`). It is off by default, only an Admin can change it, and employees cannot change it themselves.

Calling an employee you may not call is refused before any model call: the reply says you have no permission to call that employee. It does not name the employee, but it does tell you the name belongs to one (a name that matches nobody is just text for the default employee). The refusal is written to the security audit log (`trigger_route_refused`) with the dashboard account, or a short digest for an unlinked sender, never the raw platform id. Allowed switches are logged at info level.

A WebChat account that is not bound to the default employee is refused when it sends a message or chat command (`/goal`, `/status` …) that calls nobody, and is asked to pick an employee or type `@`. In WebChat, an answer from a called employee is kept in that employee's own conversation history, not in the selected employee's.

Repeated refusals from the same sender for the same employee are audited once a minute.

## The `@` list

- **WebChat**: typing `@` in the message box opens a list of the employees you may call (avatar, name, department). It filters as you type; arrow keys move, Enter or Tab inserts `@name `, Escape closes it. Each reply from an employee other than the one you are talking to is labelled with who answered.
- **LINE**: send `@` alone (or `@` with part of a name) and the bot answers with the list as quick-reply buttons, at most 13 at a time; a longer list ends with a **更多** button for the next page. Tapping a name sends `@name`.
- **Telegram** (shared bot): the same list comes as inline buttons.
- Other channels answer with a text list.

The list, the refusal and the "say the task" prompt go out only after the same access gate (blocklist, pairing) and human-takeover checks as an AI reply.

## Calling an employee and then saying the task

On messaging channels (LINE, Telegram, Discord, Slack and the others), a message that is only `@name` (or tapping a name in the LINE or Telegram list) gets the reply "請說要交給「name」的事". **Only your next message** in that conversation goes to that employee, within 10 minutes; after that, routing is normal again. Your access is checked again when that message arrives. Another member's message in the same group is not affected.

The waiting state is kept in memory. A gateway restart forgets it, so the next message goes to the default employee. A LINE message that was already accepted under it is quarantined (`account_route_authorization_changed`) instead of being answered by the wrong employee.

## Group "mention only"

- **LINE**: with **mention only** on, the bot answers only messages that call an employee by `@` (or ask for the `@` list), and the one message a called employee is waiting for. Before v1.76.0 this setting silenced the whole group. LINE does not tell a bot which of its own messages was quoted, so quoting the bot does not count.
- **Telegram**: the shared bot also accepts employee mentions in mention-only groups, besides its own `@username` and replies to it. Per-employee bots keep their own `@username` rule.

## Who answered

- WebChat reply frames (`assistant_done`) carry `agent` (id) and `agent_name`.
- On LINE and Telegram, a reply from an employee other than the conversation's default starts with `【name】`. Replies from the default employee have no prefix. The LINE bubble header now names the employee that answered (it used to always name the main employee).
- Contract checks (`CONTRACT.toml`) and secret restoration run as the employee that answered.

## Not covered

- Discord, Slack and the other channels use the same rules, the text list and the one-shot call, but they have no buttons. Only WebChat and LINE were tested end to end; Telegram is covered by unit tests.
- A sender with a linked dashboard account on other channels is mapped only when the identity was verified through the channel login flow.
