# RFC-28: DuDuClaw control room — a terminal client for autonomous employees (`duduclaw tui`)

**Status:** Accepted (2026-10-28). Implementation in progress, milestone by milestone (§9). Nothing in this RFC is shipped until the CHANGELOG says so.
**Author:** DuDuClaw team
**Date:** 2026-10-29
**Supersedes decision:** none.

---

## 1. Motivation

DuDuClaw employees work on their own. The goal loop starts rounds, the dispatcher claims tasks, cron jobs and heartbeats fire, responsibilities wake employees on events, and autopilot rules react to the event bus. A person is needed only when something stops: a goal pauses at `needs_human`, an approval waits, a breaker opens.

Today the only place to watch and steer that work is the browser dashboard, and the dashboard mostly learns about autonomous work by polling (the sidebar re-reads approvals every 60 seconds). The terminal tools people already use beside DuDuClaw — agent-aware terminals and multiplexers — are built around a different assumption: an agent is something a person launched in a tab, and when the person leaves, nobody is watching.

This RFC adds a terminal client built around DuDuClaw's model instead: **employees, tasks and runs are the units; panes are just views onto them.** Closing the client never stops an employee. Opening it answers three questions in one screen:

1. What are my employees doing right now, and what did they finish while I was away?
2. Who is waiting for me, and why?
3. How do I make one employee, or all of them, stop acting on their own — reliably?

### 1.1 Prior art (concepts only)

The interaction design borrows ideas from agent-aware terminals and multiplexers (cmux, Warp's agent CLI, Wave Terminal, herdr, ADE, agent-terminal, Clinch): priority badges that roll up from a pane to its parent, a "done but not yet seen" state separate from "idle", recording *who raised the hand* for every attention item, snapshot-then-subscribe reconciliation with an explicit "events lost" signal, and an inbox where only "needs you" items notify. **No code is taken from any of these projects**; everything here is designed and written for DuDuClaw.

### 1.2 Non-goals

- Not a coding agent. Writing code interactively is out of scope.
- Not a replacement for the browser dashboard. Settings, long forms and rich content (screenshots, large diffs) stay there.
- Not a new authority. The client calls the same gateway RPCs as the dashboard and is authorized exactly like it (§7).

---

## 2. Shape

`duduclaw tui` is a subcommand of the single `duduclaw` binary, implemented in a new crate `crates/duduclaw-tui` with ratatui + crossterm. It runs in any terminal and over SSH.

| Command | Purpose |
|---|---|
| `duduclaw tui` | Full control room |
| `duduclaw tui inbox [--follow] [--json]` | Inbox only, sized for a narrow split in another multiplexer; `--json` prints one JSON object per line for scripts |
| `duduclaw tui watch <agent\|run>` | Live tail of one employee or one run |

The client is a gateway client. It talks to the gateway over the two WebSockets the dashboard already uses:

- `GET /ws` — `WsFrame` RPC, push events (status, lists, decisions, controls).
- `GET /ws/chat` — WebChat, for conversations with employees.

It uses `duduclaw_gateway::protocol::WsFrame` directly (the CLI crate already depends on the gateway). If the gateway is unreachable the client shows that and refuses to act; it never falls back to opening databases itself.

The main area tiles one to four **blocks**. A block is one of `run | chat | task | log`, and from milestone M4 also `pty` (§9).

---

## 3. Screen

```
┌ DuDuClaw control room ─ ~/.duduclaw ─ gateway 1.76.3 [ok] ─ today $1.84 ─ autonomy: running ┐
│ ! Needs you (3)            │ [1 run] adu · goal "LINE weekly" round 2/5                     │
│ > approval shell_exec adu  │   09:41 tasks_claim       ok                                   │
│   goal paused  xiaolan     │   09:42 wiki_search "line error rate" ...                      │
│     blocked_needs_decision ├────────────────────────────────────────────────────────────────┤
│   budget breaker OPEN      │ [2 chat] researcher                                            │
│ Employees                  │                                                                │
│ * adu        working 2     │                                                                │
│ ? xiaolan    awaiting      │                                                                │
│ + researcher done (unseen) │                                                                │
│ - accountant paused        │                                                                │
│ Next autonomous runs       │                                                                │
│   11:00 cron researcher    │                                                                │
├────────────────────────────┴────────────────────────────────────────────────────────────────┤
│ [list] researcher > _                                  F1 help  Ctrl-P commands  q quit     │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.1 Employee states

| State | Meaning | Glyph (ASCII default) |
|---|---|---|
| `working` | a turn or goal round is running | `*` |
| `awaiting` | `needs_human` or a pending approval | `?` |
| `done` | finished since this client last looked | `+` |
| `idle` | nothing running, nothing unseen | `.` |
| `paused` | status paused or autonomy paused | `-` |
| `ended` | terminated / archived | `x` |

Badge priority: awaiting > error > done > working. Only awaiting and errors notify. "Seen" state is per client, stored locally in `~/.duduclaw/tui-state.json`, never written back to the gateway.

ASCII glyphs are the default because most box-drawing and geometric symbols have East Asian *Ambiguous* width and misalign in terminals configured to treat Ambiguous as wide, which is common for zh-TW and ja-JP users. `glyphs = "unicode"` in `~/.duduclaw/tui.toml` opts in.

### 3.2 Focus

Two focus modes, always shown in the status line:

- **List focus**: arrows / `j` `k` move, `Enter` opens, single-letter actions (`p` pause autonomy, `s` snooze, `r` mark seen, `1`–`4` switch block).
- **Input focus** (`i` or `/` to enter, `Esc` to leave): text goes to the selected employee. A leading `@name ` sets the WebChat `agent` field. Gateway chat commands such as `/goal` and `/status` are sent as-is. `/pause`, `/resume` and `/stop` are Computer Use commands and are labelled so in completion; pausing an employee always uses the RPC in §5.

Command palette: `Ctrl-P`.

### 3.3 Inbox and decision cards

- Every inbox row names **who raised the hand**: goal loop (with its `PauseReason`), approval (with its kind), breaker, or an employee's question. The source decides which actions are offered.
- New items only enter the inbox and update badges. **They never take focus.** A decision card opens only when the user selects a row and presses `Enter`; keys pressed during the first second after a card opens are ignored.
- Approving an approval requires typing a two-character confirmation code shown on that card, then `Enter`. Rejecting is a single key. High-risk effect classes show the full payload, which must be scrolled to the end before the code field accepts input.
- Every decision carries the digest of the payload the user saw; the gateway rejects it if the approval changed (§8.2).
- Goal decisions: `retry` and `continue` are single keys; `abort`, `done` and `takeover` are terminal and need a second press of the same key.
- Snooze hides a row locally until a time. An approval cannot be snoozed past 80 % of its remaining TTL; it resurfaces earlier with a reason, because an expired approval is a rejection.
- The client shows only actions the current identity's RPCs will accept.

### 3.4 Blocks

| Block | Source | Notes |
|---|---|---|
| run | `runs.subscribe` (opt-in, §8.1) plus `runs.get` for history | Runtimes that do not stream steps show "this runtime reports start and finish only" |
| chat | `/ws/chat` (`progress`, `step`, `thinking`, `assistant_done`) | Several conversations can run at once after M0b |
| task | `tasks.list_page`, `tasks.iterations`, `tasks.timeline`, `tasks.changes` | `goal_decide`, `steer`, `stop` |
| log | `logs.subscribe` | Manager and above |
| pty (M4) | local PTY owned by the user | Not routed through the gateway |

Cost and cron views are shown only to roles whose RPCs allow them; other roles see "requires administrator" instead of an empty panel.

### 3.5 Notifications

- Foreground: badges and inbox counts.
- Background tab: an OSC 9 / OSC 777 notification with fixed text only ("DuDuClaw: N items need you"), never employee-controlled strings. Off by default; on via `tui.toml`; off by default when `SSH_CONNECTION` is set.
- Client closed: the existing channel notifications from the goal loop keep working.

---

## 4. Autonomy as a first-class concern

The control room shows, per autonomous subsystem, what will happen next and what happened last:

| Subsystem | Shown | Controls (gateway authorizes as usual) |
|---|---|---|
| Dispatch + goal loop | rounds, criteria ledger, verifier verdicts, `PauseReason` | `goal_create`, `goal_decide`, plan approval |
| Responsibilities | list, next trigger, recent fires | pause, disable, steer, stop |
| Autopilot + `[tick]` | rules, breaker state | read-only |
| Cron, heartbeat, digest | next run, last result | cron pause / resume / run-now (admin), heartbeat trigger |
| Workflows | runs | cancel |
| Evolution, prediction | status, stagnation, playbook | read-only |
| Team-as-Agent | role turns | read-only |
| Budget | breakers | read-only |

The control room only performs reversible "stop" or "one step forward" controls. Changes that grant standing authority or alter global behaviour stay in the browser dashboard because they need full forms and a lot of context to judge. This is a UX split, not a security boundary (§7).

---

## 5. Autonomy pause (gateway primitive)

New gateway RPCs:

- `autonomy.pause { agent_id | all, reason? }`
- `autonomy.resume { agent_id | all }`
- `autonomy.status {}`

While an employee (or everyone) is paused, every autonomous path is blocked and each has a test proving it:

| Path | While paused |
|---|---|
| goal loop admission | no new round starts |
| goal round in progress | the current turn finishes, then the task stops at `needs_human` with `PauseReason::operator_paused` |
| dispatcher | does not claim |
| cron | due job is skipped and the skip recorded |
| heartbeat | skipped |
| responsibility fire | skipped and recorded |
| autopilot rule actions | not executed |
| assign schedule | not fired |

Rules:

- The pause state is independent of the employee's `status`, persisted, and survives restart.
- **Fail closed**: if the pause state cannot be read, every employee is treated as paused for autonomous work, and `autonomy.status` reports the error.
- An employee whose `status` is not operational is also blocked from every autonomous path. (Before this RFC, heartbeat ignored `Paused`.)
- A person's direct conversation with an employee still works while it is paused.
- Authorization: per-employee pause needs the Manager role plus a live Operator-or-higher binding to that employee; `all` needs Admin. There is no MCP tool; employees cannot pause or resume themselves through DuDuClaw.
- `autonomy.changed` is pushed to users who can view that employee.

In the control room `p` sends the pause immediately; the status line shows `autonomy: paused (adu)`; `u` sends the resume. There is no delayed send: an emergency stop must not wait, and a delayed send that dies with the connection would look sent when it was not.

---

## 6. Live events

### 6.1 Which process emits what

Approvals are mostly created by the employee-side `duduclaw mcp-server` process, which cannot reach the gateway's in-process broadcast channel. Every event therefore names its emitting process:

| Event | Emitted by | Path |
|---|---|---|
| `agent.turn_started` / `agent.turn_finished` (payload includes `source`: channel / goal / cron / heartbeat / responsibility) | gateway, at a runtime-independent choke point | in-process broadcast |
| `task.needs_human` and goal state transitions | gateway goal loop | in-process broadcast, under new names so existing autopilot rules on `task.updated` do not start firing on them |
| `task.created` / `task.updated` from employees | mcp-server | already written to `events.db`; added to the forwarding allow-list |
| `approval.created` / `approval.decided` / `approval.expired` | any process that opens the approval broker | written inside the broker to `events.db` |
| `agent.status_changed`, `autonomy.changed` | gateway or mcp-server | in-process or `events.db` |
| `budget.breaker_open`, `autopilot.breaker_open` | gateway | in-process broadcast |
| `run.step` | gateway | **not** broadcast; opt-in per connection via `runs.subscribe`, rate-limited |

Events from `events.db` arrive within the existing ~2 s forwarding interval, which is acceptable for "needs you".

### 6.2 Reconciliation

- Each `/ws` connection numbers events **after** push filtering (`seq`), and each gateway start has a `boot_id`.
- The broadcast buffer grows from 64; when a connection lags, the gateway sends `events_lost`.
- On `events_lost`, a changed `boot_id`, or reconnect, the client re-reads the existing list RPCs. There is no replay buffer and no new snapshot RPC. Events are invalidation signals; RPC results are the truth. Events may be duplicated; refetching is idempotent.
- Until the refetch succeeds, the inbox and employee states are greyed out and labelled "data may be stale", and counts show `?`, never `0`.
- The browser dashboard switches to these events and drops its 60-second approval polling, so the events have a consumer before the terminal client ships.

### 6.3 WebChat concurrency (M0b)

- Turns in different sessions on one socket run concurrently; turns in the same session are serialized.
- A per-user in-flight turn cap is added before the connection cap is re-keyed from IP to user; the global cap stays.
- `cancel` kills the CLI, writes a `cancelled` entry to the session, and withdraws approvals that turn opened and nobody decided.

---

## 7. Security

The control room is a dashboard client and has exactly the dashboard's authority; the gateway authorizes every call. Its own rules:

1. **Refuses to start inside an AI session** (the existing AI-session markers) and is on the Bash-lane protected list. Like every Bash-lane rule, this stops accidental use; it is not containment. Containment against an employee with shell access is not granting Bash, using the task sandbox, or running employees as a different OS user.
2. **Sanitizes every gateway-supplied string before drawing it**: C0 (except newline, handled by layout), DEL, C1, bidi controls (U+202A–U+202E, U+2066–U+2069) and zero-width characters are removed. In the full view of an approval, removed characters are shown visibly (for example `␛`, `⟨U+202E⟩`) instead of silently dropped. Notifications carry fixed text only.
3. **Token hygiene**: JWT only; the legacy admin-token / `system` identity is refused with a pointer to account login. Tokens live in memory, never in argv, environment, logs, panic messages or files. No `--token` flag. Core dumps are disabled. A failed refresh returns to the login screen.
4. **Transport**: plain `ws://` only to `127.0.0.1`, `[::1]` or `localhost`; anything else must be `wss://` with certificate verification. For remote use, SSH into the gateway host and run the client there; do not port-forward the gateway, because anything on the forwarding machine can then reach the loopback-only login path.
5. **Connections**: one `/ws` and one `/ws/chat`, so the browser dashboard keeps its WebChat connection slots.
6. The gateway records `client: "tui"` for audit only, never for authorization. The client never holds employee identity tokens, never calls `/api/internal/*`, and never shells out to operator CLI commands.

### 7.1 Login

| Edition / state | Login | Client behaviour |
|---|---|---|
| Personal, local auto-login on | local session | automatic, only when connecting to a local gateway |
| Personal, auto-login off, password set | `/api/login` | password prompt |
| Personal, auto-login off, no password ever set | — | clear error: set a password or enable auto-login in the dashboard |
| Team / Enterprise | `/api/login` + OTP | password and OTP |
| `must_change_password` | — | clear error: change it in the dashboard first |
| long-running session | refresh | background refresh; both sockets reconnect with the new token |
| non-admin role | — | admin-only panels hidden with an explanation |

---

## 8. Smaller gateway changes

### 8.1 `runs.subscribe`
`runs.subscribe { run_id | agent_id }` / `runs.unsubscribe`, per connection, rate-limited server-side, same authorization as `runs.get`.

### 8.2 `approvals.decide` digest
`approvals.decide` accepts an optional `expected_digest` (hash of the payload shown). A mismatch is rejected. The control room always sends it.

---

## 9. Milestones

| Milestone | Scope |
|---|---|
| M0a | §5 autonomy pause; §6.1–6.2 events and reconciliation; §8; dashboard consumes the events |
| M0b | §6.3 WebChat concurrency and cancel |
| M1 | `duduclaw-tui` crate, `tui` / `tui inbox` / `tui watch`; §7 rules and login matrix; inbox, employees, decision cards, chat block, notifications; snapshot tests with a recorded-frame fake gateway and a live-gateway test |
| M2 | tiled blocks; run / task / log blocks; next-autonomous-runs panel; responsibility and goal controls |
| M3 | command palette, resume hint on exit, `tui.toml`, polish |
| M4 | `pty` block: a local shell or a hand-run agent CLI inside the control room. The PTY belongs to the user and does not pass through the gateway; "waiting for you" signals from it are display-only and authorize nothing. The terminal emulation is written for DuDuClaw or uses a general-purpose crate; none is taken from the prior-art projects |

Each milestone ships with: the five required CI checks green, a local Windows cross-compile, a live run against an isolated home, and CHANGELOG plus three-language docs in the same pull request.

Separately, the legacy interactive mode of `duduclaw agent` (no subcommand), which bypasses the gateway, will be deprecated under the normal two-minor-version policy once the control room ships.

---

## 10. Open questions

1. Whether to store refresh tokens in the OS keychain (per-application ACL) so Team-edition users do not log in on every start. Deferred to after M1.
2. Bringing dashboard-only approval kinds into the control room needs a "person present" proof bound to the approval id (passkey user verification, or a platform biometric prompt). Until then those kinds open in the browser.
