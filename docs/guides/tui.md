# Terminal control room

[繁體中文](zh-TW/tui.md) · [日本語](ja-JP/tui.md)

`duduclaw tui` opens a control room for pending approvals, goals needing a reply, and employee status. Select a row to inspect its details. Where the gateway provides it, administrators also see the next scheduled autonomous run. Closing the control room does not stop employees.

Approval and goal decision cards are available in the full control room. Chat blocks, input focus, `tui watch`, and optional notifications are available. Employee status pause/resume uses the same permission checks and confirmation cards. The terminal interface currently uses Traditional Chinese.

## Start

Start the gateway, then run one of these commands on the same host:

```sh
duduclaw tui
duduclaw tui inbox
duduclaw tui inbox --follow
duduclaw tui inbox --follow --json
duduclaw tui watch adu
```

The full screen needs a terminal. `inbox` prints one snapshot and exits; `--follow` prints a new complete snapshot when it changes. `--json` produces one JSON object per line on stdout, with `type`, `stale`, `count`, `error`, and `items`. Output itself needs no TTY; if account login is needed, an interactive terminal is required. There is no token flag or credential file for unattended login.

The CLI connects to `127.0.0.1`, using `DUDUCLAW_PORT`, then `<home>/config.toml [gateway] port`, then the default gateway port. Home is `DUDUCLAW_HOME` or `~/.duduclaw`. This version has no remote URL flag.

**For remote use, SSH into the gateway host and run it there; do not port-forward the gateway.** Port forwarding would expose its loopback-only local login route to programs on the forwarding host.

## Embedded local terminal

Press `P` in list focus, or choose `pty` in the command palette, to open an owner-local PTY in the focused block. It runs `$SHELL` (fallback `/bin/sh`) and inherits the owner's environment after removing every `DUDUCLAW_*` variable. Gateway JWTs remain in memory and are never added to the child's environment. The existing AI-session startup refusal still applies. No gateway response can supply a local command.

While terminal input has focus, **`Ctrl-\` or `F12` returns to the control room** (`Ctrl-4` is the legacy encoding of `Ctrl-\`). All other ordinary terminal keys, including `Esc`, `Ctrl-C`, `Ctrl-P`, arrows, function keys and page keys, go to the child. `Enter` / `i` returns to terminal input. In control-room focus, `PgUp` / `PgDn` scroll the terminal's bounded 5,000-line history. Paste forwards text, preserving newlines and tabs but removing control characters that could escape a bracketed-paste wrapper; the wrapper is used when the child requests it. Input from each event batch is coalesced and written in order by a dedicated writer. Host event reads continue even when the child stops reading: escape keys, control-room quit and resize remain available. Use `F12` or `Ctrl-\`, then `q`, to quit from terminal focus. The buffer counts queued and in-flight bytes, with a 1 MiB hard limit; a chunk that would exceed it is refused in full with a visible message. Mouse events are forwarded only during PTY input focus, inside its viewport, when the child enables reporting. Legacy, UTF-8 (1005), urxvt (1015) and SGR (1006) encodings use pane-relative cell coordinates, including the displayed character widths. SGR-Pixels (1016) estimates pane-relative, one-based pixels from the host cell dimensions, falling back to 8×16 pixels per cell; subcell pointer positions are unavailable. Encoding precedence is 1016 > 1006 > 1015 > 1005 > legacy; an unfocused pane receives nothing. The status line identifies the focus and escape key.

A child OSC 9 notification, OSC 777 `notify` notification, or an OSC 0/2 title exactly `waiting for you`, `waiting for input` or `needs input` (case insensitive) raises a fixed waiting marker. It clears on local input. This is display-only: it never authorizes, answers, calls the gateway or emits a host notification. Oversized or malformed strings are ignored.

The block starts only the shell from the environment of the person who launched the control room: `$SHELL` on Unix (`/bin/sh` if unset), or `%ComSpec%` on Windows (`cmd.exe` if unset; Windows PTYs remain disabled). Run other programs from that shell. `tui.toml` is not a trusted execution boundary. The former `[pty] command` setting has been removed; an old key is ignored with a settings notice, while other valid settings still apply.

A dedicated parser thread updates the VT model and publishes bounded visible-cell snapshots. Rendering and decision cards never wait for parsing. A fixed-storage prefilter discards 7-bit OSC, DCS, APC, PM and SOS string controls; at most 256 bytes of an OSC are inspected for a display-only waiting signal and never forwarded; CAN/SUB cancel a sequence and ESC restarts escape parsing. Raw 8-bit C1 bytes outside valid UTF-8 are dropped; UTF-8 scalars pass only after complete validation. ESC is emitted only as part of a complete whitelisted short escape, charset designator or CSI sequence. CSI sequences are limited to 128 bytes after the introducer and 32 parameters (including subparameters); longer sequences are discarded. CSI count and position parameters are clamped to `max(rows, columns, 1)`; scroll regions are bounded by the row count. The filter buffers each bounded CSI until its final byte, including across read boundaries. SGR colors, private sequences, modes, reports, window operations and saved cursors pass unchanged. The renderer emits sanitized cell text, colors and bold; it never replays the child's OSC clipboard/title/hyperlink controls, DCS, sixel, or terminal mode sequences to the host. Layout changes resize the PTY (bounded to 200 rows × 500 columns). CJK wide cells and combining marks are drawn in both configured width modes. The parser uses narrow ambiguous-character coordinates; wide-mode rendering expands ambiguous glyphs and clips at the block boundary. Full-screen child applications that require wide ambiguous cursor coordinates are not yet verified.

Closing or replacing a block with a pending/running child asks for `y`; `Esc` cancels. Quitting stops local PTY children; gateway employees continue. Unix cleanup uses the existing process-group helper and inventories other job-control groups in the same session before reaping the shell. Deliberately detached processes (for example, using `setsid`) keep running, as they do in any terminal. Even successful session cleanup cannot prove that every descendant stopped: cleanup remains reported as unconfirmed, including when inventory or signalling fails. Windows PTYs are disabled with an explanation: ConPTY tree cleanup has not been verified, despite the existing Job Object helper for other process launch paths. No waiting-for-you badge is inferred from terminal text, and terminal output never authorizes an operation.

RPC `/ws` and WebChat `/ws/chat` send Close and allow up to one second for the peer's closing handshake on normal shutdown. The control-room, inbox and watch entry points await shutdown.


## Command palette and exit summary

Press `Ctrl-P` in list, help or input focus. Type a fuzzy query (Traditional Chinese labels or English aliases such as `stop`, `steer`, `log`); arrows select, `Enter` runs the displayed shortcut, and `Esc` returns to the previous focus with the chat draft intact. `Ctrl-K` remains reserved. The list is recalculated from the focused block, current role, employee binding and fresh data. Autonomy pause/resume sends immediately. Other controls open the same confirmation card as their shortcut; the palette never approves a card for you. Cards and content warnings own their keyboard, so the palette cannot open over them.

`F1` / `?` shows help only in the focused block; other blocks stay visible. Long home paths retain their right end with a leading ellipsis. At the minimum supported height (12 rows), inbox and employee panels each retain one content row; the schedule panel may be hidden. Borders remain ASCII in either glyph mode. All navigation works without a mouse. Windows key-release events never execute commands.

The control room offers controls that stop work or move it one step forward. It never grants standing permission; Admin can pause or resume autonomy globally. `cron run now`, `heartbeat trigger`, and task stop are one-way actions and always go through a confirmation card; controls are not necessarily reversible.

After quitting and restoring the terminal, stdout shows the authoritative pending inbox count (including locally snoozed items), running employee names, and a command to return to the same home. Stale counts or employee data show `?`. Names are sanitized and bounded; no inbox, task, chat or tool content is printed. Unix resume commands use Bash/Zsh ANSI-C quoting; Windows commands use PowerShell. Closing the client leaves employees running. Windows crash-dump suppression remains unimplemented; compilation is not native terminal acceptance.

## Login

| Edition or state | Behaviour |
|---|---|
| Personal, local auto-login enabled | The gateway may issue a local account session without prompting. |
| Personal, auto-login disabled, password set | Choose account password and enter the account email and password. |
| Personal, auto-login disabled, initial setup incomplete | Open the dashboard to set a password or enable local auto-login first. |
| Team / Enterprise | Choose password **or** account email code. Accounts enrolled in MFA then require an authenticator code or recovery code. The gateway never grants these editions local auto-login. |
| Password change required | Change the password in the dashboard before starting the control room. |
| Session expires | Refresh in memory and reconnect. If refresh fails, request explicit account login again; never fall back to automatic local login. |
| Non-admin account | Only permitted employees and inbox items are available. Admin panels explain the required permission. |

Password and channel code are alternative primary login methods. For an account enrolled in MFA, either method is followed by a second step: a six-digit authenticator code or a recovery code. Wrong codes can be retried within the gateway's attempt limit (the terminal also stops after five attempts); an expired or exhausted challenge requires a new login. If enrollment is required, complete it in the dashboard; the terminal does not use the enrollment token.

Channel codes are requested for the entered account email and delivered through the gateway's configured, bound channel; delivery must already work in the dashboard. Passwords, codes and recovery codes are not echoed. Login prompts and fixed errors go to stderr. Without an interactive terminal, a required code produces an error instead of waiting for input, including `tui inbox --json`. Gateways without MFA keep the existing login flow.

## Keys and freshness

| Key | Action |
|---|---|
| Arrows / `j` / `k` | Move selection |
| `Enter` | Open details or a decision card |
| `r` | Mark this item or identified completed run as read on this client |
| `PgUp` / `PgDn` | Scroll details |
| `?` / `F1` | Toggle help |
| `q` / `Ctrl-C` | Exit |

New items never take selection. Read marks do not dismiss pending requests at the gateway. Completion badges use recent run history, not a complete history of everything you have missed. If the selected row disappears, selection moves to the same position, or the last row; an existing selected ID stays selected through new arrivals and reordering.

On disconnect, lost events, or an incomplete refetch, lists show `資料可能過期` (“data may be stale”) and counts become `?`; JSON uses `stale: true` and `count: null`. The last complete list remains visible. A refetch has a shared limit of 2,000 rows and 4 MiB of encoded response content across employees, inbox, recent runs, and schedules. Exceeding either limit stops that refetch and keeps it stale, never publishes a partial list as complete. Responses and events are limited to 256 KiB per WebSocket message/frame, with a 16-event queue. Large installations may need the dashboard.

## Chat and input focus

The status line always identifies the focus. In list focus, `1`–`4` selects a block and `C` opens chat. Select an employee and press `i` to type; `/` starts input with a slash. `Esc` returns to list focus and keeps the draft. While typing, single-letter shortcuts are text. CJK characters, committed IME text, and bracketed paste are supported. `Enter` sends; `Ctrl-C` exits.

A leading `@name ` selects the WebChat `agent` field and is removed from the message. Without it, the employee selected when input opened is the recipient. `/goal`, `/status`, and `/cost` pass unchanged to the gateway. `/pause`, `/stop`, and `/resume` show a Computer Use note: these commands do **not** pause an employee.

The right area tiles up to four blocks as described below. Different chat blocks can run turns concurrently over one socket, with one in-flight turn per conversation. Each block keeps its employee, conversation, draft, history, scroll position and cancellation prompt; the gateway turn cap is reported in the affected block. Unknown or conflicting conversation frames are ignored. Thinking and tool steps collapse to one status line; an ASCII spinner also works with `glyphs = "ascii"`. `PgUp` / `PgDn` in list focus scroll chat history, capped at 2,000 lines of at most 512 characters each. New output never changes focus. Employee output passes through `TerminalText`; the client emits no OSC 8 hyperlinks.

One `/ws/chat` connection shares the `/ws` client's in-memory token refresh and reconnects with backoff. Interrupted messages are not automatically resent; their outcome is uncertain. Completed conversations can resume on reconnect; changing employee starts a new conversation. In list focus, `X` cancels a running reply only after the gateway has sent a frame carrying `msg_id`. Otherwise the block says cancellation is unavailable. `cancel_accepted` only means the request was accepted; the cancellation completes on an `error` frame whose `code` is `cancelled`. A final response can also end the turn if delivery has already begun.

If delegated work from that turn is still running, the gateway includes a count and a private, single-use reference. The control room shows: 「還有 N 位員工的工作還在跑，要一起停嗎？ y 停止／n 讓它們跑完」. In **list focus**, `y` sends one stop request; `n` or `Esc` dismisses the question. In **input focus**, `y` and `n` remain ordinary text; the status line explains how to return to list focus to answer. New output never moves focus. The reference is never displayed. The question expires after 10 minutes, disappears on disconnection, and is lost if the gateway restarts. Ignoring or dismissing it sends no request and leaves the delegated work running.

The result displays 「已停止 N 項」 and, when needed, 「M 項還在收尾」. Work still stopping is not counted as stopped. If the reference is no longer usable, 「這個詢問已失效，下游工作不受影響」 means this request stopped nothing. Public widget visitors cannot stop employee work and receive no question. Automatic stop on credential revocation applies while the turn is running, or when a stop request arrives after revocation. Work from an already finished turn, or an unanswered offer, is not automatically stopped.

Stopping follows only delegated runs carrying the exact originating conversation and turn, and excludes work attributed to another person. It cannot recall an external action already executed. Task-board tasks and goals created by the turn are not followed; use their own controls. A run that loses its origin information cannot be found. A run left alive can finish. A row carrying that cancelled turn is fenced against further delegation and new approvals; a run that deliberately drops or forges its lineage is not followed. This does not pause the employee or affect its other conversations.

Stopped means the managed process group and any task-sandbox container are gone. A process that deliberately leaves its process group (for example, with `setsid`) is outside that check unless the employee runs in the task sandbox. For local child processes on Unix, a stop waits for the managed process group to disappear; ending only its parent is not enough. A task-sandbox run also waits for confirmation that its container was cleaned up. If shutdown cannot be confirmed within the wait, the run remains under “still stopping.” On Windows, the current process-management interface cannot confirm that all processes have exited, so a cancelled child-process run is conservatively reported as still stopping even after termination has been requested. A request already sent to an external service cannot be recalled by stopping the local run. Gemini CLI and Antigravity runtimes have been removed and have no dispatch path here; this does not remove the Gemini API provider. These rules describe the implementation's reporting boundary, not completed native-platform or provider acceptance.

## Tiled blocks and task controls

The right side holds one to four blocks. `v` splits left/right, `x` splits top/bottom, `1`–`4` focuses a block, and `Ctrl-W` closes it locally. Closing a block does not cancel a task or chat turn. Layout survives resizing during the session; when the right area is narrower than 64 columns or shorter than 16 rows, only the focused block is shown with the hidden count. It returns to the same layout when enlarged. Layout is not saved across application restarts.

Use `D`, `C`, `R`, `T`, `L` for detail, chat, run, task and log. Select an employee before `R` or `T`; the run/task block keeps that employee while the left list moves. `C` opens a separate chat (reusing an empty detail pane or adding a pane, up to four total). `i` uses the focused chat; from another kind of pane it opens an existing chat or creates one. Only the focused chat receives input or `X` cancellation, and its downstream-stop offer stays there. `[` / `]` selects a run or task, and `PgUp` / `PgDn` scrolls. Run browsing covers the most recent 50 runs; task pages use 50 rows and the gateway total.

Run blocks read `runs.get` history. They try `runs.subscribe` and show an explicit five-second polling notice only on the gateway's unknown-method response; other failures remain errors. Subscribed `run.step` frames must match both employee and run, and only 256 steps are retained. Persisted history is reconciled periodically. A known non-Claude employee runtime with no step records shows `此 runtime 只回報起訖`; the employee's current runtime is not proof of which runtime produced an older run. Run/tool previews stay hidden by default. Press `c`, read the warning, then `c` again to reveal them; other keys cancel. The warning is once per control-room session, and `c` subsequently toggles content. Previews are truncated, not redacted.

Task blocks read `tasks.list_page`, `tasks.iterations`, `tasks.timeline`, `tasks.changes` and `tasks.stop_status`. Default views show round verdicts, event types and changed paths, without feedback, source snippets or tool payloads. `Enter` opens the existing goal decision card, including finished goals with an explicit follow-up message. An Operator/Owner binding enables `e` to steer an unfinished goal and `s` to stop. Steering opens a text editor (up to 4,000 characters); `Enter` moves to confirmation. The gateway still checks whether steering is enabled. Stop carries the observed `expected_authority_revision`; goal decisions carry `expected_updated_at`.

In a task block, `b` switches to responsibilities for that employee. `[` / `]` selects a responsibility and shows `responsibilities.get` next trigger plus recent `responsibilities.fires`. `p` pauses and `d` disables with `expected_control_epoch`. `o` opens the current occurrence's task using the gateway-provided task ID, where steering and stop use the same task controls. Standing authority and contract changes stay in the dashboard.

Logs use `logs.subscribe`, require Manager or Admin and retain at most 256 lines of 2,048 characters. Other roles see a reason. Lines pass through `TerminalText`; the gateway has no log replay, so missed lines cannot be reconstructed.

The schedule area shows enabled and paused cron jobs for Admin, and heartbeat state for Manager/Admin. `p` pauses/resumes a cron and `n` runs it once; on a heartbeat, `n` triggers it. Cron run-now includes a fresh `request_id`. Cron uses the gateway’s RFC3339 `next_run`; null is labelled unscheduled or unknown and is never computed locally. Heartbeat shows the actual `last_run` time, not a fabricated success result. Cron expressions, timezone and available last-run/result fields appear in details.

In list focus, select an employee and press `p` to immediately send `autonomy.pause {agent_id}`; `u` sends `autonomy.resume {agent_id}`. Neither waits for a card or confirmation. These controls require Manager plus a live Operator/Owner binding; the gateway checks permission again. In input focus, `p` and `u` remain text. In a responsibility block, `p` operates on that responsibility; cron `p` and cron/heartbeat `n` work with the selected schedule without requiring the Detail block. Run-now, heartbeat trigger and task stop retain their confirmation cards.

Admin users can choose 「暫停全部自主工作」 or 「恢復全部自主工作」 in `Ctrl-P`; these send `{all:true}` immediately. Global resume has no single-key shortcut. The status line shows paused names (at most two, truncated to twelve characters each, plus a remaining count), or 「自主: 全部暫停」. Connection, reconnection, lost events and every `autonomy.changed` trigger `autonomy.status`. Employee rows, badges and the status line show 「待確認」 while data is stale, a refetch is pending or the state cannot be read; cached data never implies paused or running work. A refusal shows the reason and never changes local state optimistically.

Autonomy pause persists independently of employee status, blocks new autonomous work and leaves direct human conversations available; a running turn can finish before its next round is blocked. Global resume clears the global pause only; individual pauses remain. Employee rows distinguish autonomy pause from an employee's paused or stopped status. The palette also offers 「暫停員工狀態（需確認）」 / 「恢復員工狀態（需確認）」 through `agents.pause` / `agents.resume`, retaining the existing confirmation card. These change the employee's own operational status and do not clear the independent autonomy pause.

Autonomy pause and resume are the immediate exception: they send without a confirmation card. Task, responsibility, schedule and employee status actions retain their separate decision cards. Each card ignores keys for the first second, requires inspecting the full request and entering a fresh two-digit code, and refuses submission while stale or after its displayed subject changes. A request is never automatically retried. Accepted, refused and conflict responses trigger refetches; accepted stop may still be `cancel_pending`. The UI never invents a successful state. Socket fake-gateway tests and live gateway/native-terminal acceptance remain NOT RUN in the restricted sandbox; in-process fake RPC tests cover request shapes and card state transitions.

## Autonomous subsystem views

From list focus or `Ctrl-P`, open `W` workflows, `A` autopilot, `E` evolution/playbook, `B` budget incidents, or `G` the latest digest. These blocks support splitting, focus switching and scrolling. Empty, refused, stale and unavailable responses are distinct; an empty visible scope does not mean everything is healthy. Gateway strings are sanitized, fields are limited to 240 characters and tables to 50 rows, with a dashboard pointer for omitted content.

- **Workflows:** select a source task in the task block, then press `W`. The view reads that task's drafts (first page, up to 20) and up to 20 runs per draft; fixture runs are excluded. `[` / `]` selects a run and reads its steps. `s` opens the one-way cancellation card; Manager plus Operator access is required, and the gateway rechecks both the source task and draft owner. No activation or standing-authority controls are exposed.
- **Autopilot:** Admin only. Shows rules and the actual `closed`, `open` or `half_open` state with `opened_at`. An unavailable engine or a missing breaker row is never shown as closed.
- **Evolution:** Manager/Admin status and stagnation snapshots; the selected employee's playbook is read separately with Viewer access. No playbook mutation is exposed.
- **Tasks:** the detail includes the criteria ledger (mode, criteria, evidence, unresolved items and last updated round) and team role turns. A missing ledger is labelled unprovided. A role-turn read failure is shown separately.
- **Budget and digest:** budget incidents require Manager/Admin and are historical events, not proof that a breaker is still open. The digest shows the latest saved result within the caller's audience, including delivered item names; no feedback writes are added.

`budget.breaker_open` and `autopilot.breaker_open` produce bounded informational inbox rows. They identify the employee or rule; `Enter` opens Budget or Autopilot, never a decision card. They record events observed by this client, not a complete historical inbox. Approval and paused-goal rows also show their employee and approval kind or pause reason.

Choose **建立自主目標** in the palette, or press `N` with an employee selected. Enter the objective, use `Tab` to switch **先擬計畫** (initially on), and press `Enter` to review the confirmation card. Submission uses `tasks.goal_create` with `agent_id`, `description` and `plan_first`. A pending plan has a labelled **核准計畫** action: inspect the card to its bottom and enter its two-digit code. The wire action is `retry`, but the unlabelled `r` shortcut is disabled for pending plans. Oversized task cards are read-only and point to the dashboard.

On a finished goal, `c` opens the message input directly. Type or paste a follow-up, press `Enter` to review the target task and complete message. Editing `Esc` clears the message and closes the card. Review starts a new one-second guard and a fresh two-digit code; scroll to the end, type the code and press `Enter` to send. Oversized reviews require the dashboard. The message is required. Cards retain the one-second key guard, freshness/permission checks, revision binding and no automatic retry.

The header shows the number of live turns observed since connection; a reconnect or event gap loses that observation. Optional `task_id` on a turn links the employee's `T` action to its task; absence is unknown, not an error or a guessed task. Turn events carry neither outcome nor run ID, so completion/error badges still come from run history. Run titles show the gateway's source and known round, without inventing a round cap. Admin cost uses `cost.summary {since:<local midnight in RFC3339>}` and is labelled **今日** only when the gateway acknowledges that exact boundary and returns available totals. Midnight uses the local timezone at that date, including DST changes. Older gateways fall back to `hours:24` and **近 24 小時**; unavailable/refused cost is hidden. `since` takes precedence over `hours`, rejects invalid/future values, and is limited to the shorter of retention (currently 30 days) and 31 days. The list-focus footer names the selection and includes `Ctrl-P` and `q`.

A `dashboard.navigate` event is only a passive hint for an already loaded inbox item at `/inbox?item=`. It never changes the selection, focus, cards, or employee targeted by `p`/`u`. `/agents/...`, all other paths, unknown items, and hostile paths are ignored. While typing, using the PTY, viewing help, or answering a card, it does nothing.

## Watch one employee or run

`duduclaw tui watch --run <run-id>` follows exactly one run via `runs.get` and `runs.subscribe {run_id}`. The positional argument remains an employee name; it cannot be combined with `--run`. The same sanitization, content opt-in and AI-session refusal apply.

`duduclaw tui watch adu` prints sanitized lines suitable for a narrow split, without an alternate screen. Employee mode explicitly announces polling and reads `runs.list` and `runs.get` every five seconds; it does not request a subscription because the gateway requires a unique `run_id`. Recent history is limited to four runs and the last 256 events per run, with bounded duplicate tracking. Use `--run` to subscribe to one run; older gateways that return unknown-method fall back to five-second polling with an explicit notice. `Ctrl-C` exits. The same AI-session refusal and account login rules apply.

By default, output contains metadata only (time, status, agent, step kind or tool name), with no user/assistant text or tool arguments. Use `duduclaw tui watch adu --content` to include content previews; stderr warns that content is not redacted. Unknown event shapes show only their name in either mode, never their JSON payload.

## Terminal notifications

Notifications are off by default, including under `SSH_CONNECTION`. Explicit `notifications = true` in `tui.toml` opts in. An increased fresh inbox count while the terminal is unfocused emits OSC 777 for VTE/rxvt-unicode terminals, otherwise OSC 9, with only `DuDuClaw：N 項需要你處理`; employee names, task titles, and tool content never enter it. Focus events are used when available; otherwise the terminal is treated as unfocused. At most one notification is emitted per minute; suppressed rises are not queued. Inside tmux, passthrough is used only if `tmux show-option -gv allow-passthrough` returns `on` or `all`; otherwise notifications are skipped. Connection errors also use fixed text, `DuDuClaw：連線中斷，請檢查控制室`, sharing the focus checks and one-minute rate limit. Actual display depends on the terminal.

## Decision cards

Select an inbox row and press `Enter` to open its card. New arrivals only update the list and badges; they never open a card or move focus. All key presses during the first second are ignored. `Esc` closes a card; arrows and `PgUp` / `PgDn` scroll it.

For an approval, type the two-digit confirmation code and press `Enter`; `n` denies immediately. Each opening generates a random code different from the previous opening. All approval operations show the full payload with control characters made visible; the code input stays locked until the bottom is visible. Resizing can lock it again. This only guards against mistyped or blind input: **a process running under the same UID that can read the screen can bypass it**. It is not a person-presence or authentication check. All approvals currently count as high risk: `approvals.list` supplies `kind` but no gateway-produced safety classification; neither action kind alone nor fields inside `payload` can reduce risk. Resize clears typed input and redraws before queued keys are processed. Footer hints follow list, card, or help focus.

Goal cards without a pending plan use `r` for retry. `a` aborts, `d` marks done, and `t` takes over: press the same key twice, with no countdown; another key cancels confirmation. On a finished goal, `c` opens follow-up input directly; `Esc` clears and closes the editor; only `Enter` opens review of the target task and complete message. Review resets the one-second guard and code; scroll to the end, enter the fresh two-digit code, then press `Enter` to send. Oversized reviews require the dashboard. The inbox lists only `needs_human` tasks; open a finished goal in a task block to continue it. The `tasks.goal_decide` handler accepts `task_id`, `action`, optional `note`, and requires `message` for `continue`. The optional `expected_updated_at` must exactly match the task’s `updated_at` string; comparison and mutation run in the same SQLite transaction for all five actions. A mismatch returns `goal_decide_revision_mismatch` without changing the task or recording a decision. Omission preserves dashboard behavior. The control room always sends the value shown on the card and refuses goal decisions if it is missing.

Cards with `may_decide=false` are read-only and explain why. Goal actions require a known Operator/Owner binding or Admin access from `users.me`. Dashboard-only approvals, questions requiring answers, and bound operations stay in the dashboard. The TUI displays a fixed instruction and never opens a link supplied by the server. Standing-authority and global configuration changes remain in the dashboard.

Approval requires a nonempty string `digest`, sent as `expected_digest`. If it is absent or null (including a redacted viewer payload), approval sends no RPC and the card stays open with the fixed line: **內容未綁定：這張卡無法確認你看到的就是實際內容，請到儀表板決定**. Denial remains available with `n`. `approvals.list` supplies the digest. A structured digest mismatch closes the card and refetches with a fixed change notice. Every sent decision, including RPC errors and uncertain timeouts, triggers a refetch; no optimistic success or automatic resend. Server errors pass through `TerminalText`; stale, expired, or deadline-free approvals cannot be decided.

`s` in the list or card snoozes the row locally for up to ten minutes. Approvals are capped at 80% of remaining TTL and resurface no later than when 20% of TTL or ten minutes (whichever is less) remains, with a reason; an earlier requested wake-up wins. A shortened deadline also resurfaces the row. Expiry means denial, and snooze cannot extend it. Only identifiers and timestamps are saved in `tui-state.json`; the line-oriented `inbox` command continues to show the authoritative pending list. A wall clock earlier than the snooze start clears that snooze permanently; a monotonic deadline also bounds it during the session. Restart loses clock continuity and shows snoozed cards again.

## Local files

| File under home | Use |
|---|---|
| `tui-state.json` | Written when marking items read or snoozing. Contains read identifiers and snooze timestamps only; atomically replaced through a temporary file in the same directory. |
| `tui.toml` | Optional, operator-created settings; the TUI reads it and does not create or rewrite it. |

```toml
# ~/.duduclaw/tui.toml
glyphs = "ascii"                 # or "unicode"
ambiguous_width = "narrow"        # or "wide", matching your terminal
notifications = false
poll_interval_seconds = 15        # 5..3600
default_layout = "single"         # "vertical" / "horizontal"
```

Every supported top-level key is listed below. Each invalid value keeps only that key's default and displays a notice; valid sibling keys still apply. Unknown keys (including transport/auth/security settings) produce a notice and have no effect. Invalid TOML or an unreadable/oversized file uses all defaults. Settings are read once at startup.

| Key | Accepted values | Default / effect |
|---|---|---|
| `glyphs` | `"ascii"`, `"unicode"` | `"ascii"`; employee status glyphs; borders stay ASCII |
| `ambiguous_width` | `"narrow"`, `"wide"` | `"narrow"`; match the terminal's ambiguous-character width |
| `poll_interval_seconds` | Integer, 5–3600 inclusive | `15`; authoritative list reconciliation; run/task/log blocks retain their five-second reconciliation |
| `notifications` | Boolean | `false`, including SSH; explicit opt-in enables fixed-text background notifications |
| `default_layout` | `"single"`, `"vertical"`, `"horizontal"` | `"single"`; one detail block, two side-by-side detail blocks, or two stacked detail blocks; starts with block 1 focused |

Layout changes during a session are not written back. These settings cannot change gateway transport, authentication, authorization, confirmation or any security rule. Borders use ASCII. Both files are protected from employee writes by the agent-file-guard's existing home-directory policy.

## Security limits

Every request is authorized by the gateway. The client uses account JWTs held in memory and refuses legacy admin tokens or `system` identities. It never reads gateway databases as a fallback. It installs no logging subscriber, log file, or telemetry exporter, even with `RUST_LOG=trace`. On Unix it disables core dumps before reading credentials. Application-owned secret buffers are wiped where practical; console, HTTP, TLS, and WebSocket libraries can retain copies that this client cannot guarantee to wipe. Windows crash-dump suppression is not implemented, so the “no secret on disk” guarantee holds only on Unix.

AI-session refusal and the Bash command guard are speed bumps. An employee with unrestricted shell access under the same OS user can evade them and modify files directly. Use OS isolation, the task sandbox, or withhold Bash for actual containment.

Gateway text is sanitized for terminal display, and details make removed control characters visible. Standard variation selectors U+FE00–U+FE0F remain for CJK/emoji presentation; tag-plane characters U+E0000–U+E0FFF, including supplementary variation selectors, are removed. JSON preserves source text; terminal stdout additionally escapes every character forbidden by the sanitizer (including bidi and default-ignorable characters), while non-terminal output is unchanged. After decoding, consumers must sanitize decoded fields before displaying them in a terminal.

The transport library accepts plaintext only for exact loopback addresses. `localhost` is pinned to `127.0.0.1` for REST and WebSocket connections, ignoring hosts/DNS changes. TLS verification remains enabled; a TLS endpoint named `localhost` must have a certificate valid for the pinned IP. IPv6 loopback is available to library callers as `[::1]`.
