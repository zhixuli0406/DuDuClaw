# Terminal control room

[繁體中文](zh-TW/tui.md) · [日本語](ja-JP/tui.md)

`duduclaw tui` opens a control room for pending approvals, goals needing a reply, and employee status. Select a row to inspect its details. Where the gateway provides it, administrators also see the next scheduled autonomous run. Closing the control room does not stop employees.

Approval and goal decision cards are available in the full control room. Chat blocks, input focus, `tui watch`, and optional notifications are available. Employee status pause/resume uses the M2 permission checks and confirmation cards. The terminal interface currently uses Traditional Chinese.

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

## Command palette and exit summary (M3)

Press `Ctrl-P` in list, help or input focus. Type a fuzzy query (Traditional Chinese labels or English aliases such as `stop`, `steer`, `log`); arrows select, `Enter` runs the displayed shortcut, and `Esc` returns to the previous focus with the chat draft intact. `Ctrl-K` remains reserved. The list is recalculated from the focused block, current role, employee binding and fresh data. A selected control opens the same confirmation card as its shortcut; it never approves or confirms for you. Cards and content warnings own their keyboard, so the palette cannot open over them.

`F1` / `?` shows help only in the focused block; other blocks stay visible. Long home paths retain their right end with a leading ellipsis. At the minimum supported height (12 rows), inbox and employee panels each retain one content row; the schedule panel may be hidden. Borders remain ASCII in either glyph mode. All navigation works without a mouse. Windows key-release events never execute commands.

After quitting and restoring the terminal, stdout shows the authoritative pending inbox count (including locally snoozed items), running employee names, and a command to return to the same home. Stale counts or employee data show `?`. Names are sanitized and bounded; no inbox, task, chat or tool content is printed. Unix resume commands use Bash/Zsh ANSI-C quoting; Windows commands use PowerShell. Closing the client leaves employees running. Windows crash-dump suppression remains unimplemented; compilation is not native terminal acceptance.

## Login

| Edition or state | Behaviour |
|---|---|
| Personal, local auto-login enabled | The gateway may issue a local account session without prompting. |
| Personal, auto-login disabled, password set | Choose account password and enter the account email and password. |
| Personal, auto-login disabled, initial setup incomplete | Open the dashboard to set a password or enable local auto-login first. |
| Team / Enterprise | Choose password **or** account email code. Each method completes login on its own. The gateway never grants these editions local auto-login. |
| Password change required | Change the password in the dashboard before starting the control room. |
| Session expires | Refresh in memory and reconnect. If refresh fails, request explicit account login again; never fall back to automatic local login. |
| Non-admin account | Only permitted employees and inbox items are available. Admin panels explain the required permission. |

OTP is an alternative login method, **not two-factor authentication**. A correct password alone can issue a full session; the client does not chain password and OTP or claim a second-factor guarantee. The code is requested for the entered account email and delivered through the gateway's configured, bound channel. Code delivery must already work in the dashboard. Password and code input are not echoed. Login prompts and fixed error messages go to stderr.

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

The right area tiles up to four blocks as described below. This version permits one in-flight chat turn. Thinking and tool steps collapse to one status line; an ASCII spinner also works with `glyphs = "ascii"`. `PgUp` / `PgDn` in list focus scroll chat history, capped at 2,000 lines of at most 512 characters each. New output never changes focus. Employee output passes through `TerminalText`; the client emits no OSC 8 hyperlinks.

One `/ws/chat` connection shares the `/ws` client's in-memory token refresh and reconnects with backoff. Interrupted messages are not automatically resent; their outcome is uncertain. Completed conversations can resume on reconnect; changing employee starts a new conversation. In list focus, `X` cancels a running reply only after the gateway has sent a frame carrying `msg_id`. Otherwise the block says cancellation is unavailable. `cancel_accepted` only means the request was accepted; the turn ends on `cancelled` or a final response. This requires the additive protocol from PR #264; ordinary chat works with the current gateway.

## Tiled blocks and task controls (M2)

The right side holds one to four blocks. `v` splits left/right, `x` splits top/bottom, `1`–`4` focuses a block, and `Ctrl-W` closes it locally. Closing a block does not cancel a task or chat turn. Layout survives resizing during the session; when the right area is narrower than 64 columns or shorter than 16 rows, only the focused block is shown with the hidden count. It returns to the same layout when enlarged. Layout is not saved across application restarts.

Use `D`, `C`, `R`, `T`, `L` for detail, chat, run, task and log. Select an employee before `R` or `T`; the run/task block keeps that employee while the left list moves. `C` focuses the existing chat or creates it; all chat views share the single conversation. `[` / `]` selects a run or task, and `PgUp` / `PgDn` scrolls. Run browsing covers the most recent 50 runs; task pages use 50 rows and the gateway total.

Run blocks read `runs.get` history. They try `runs.subscribe` and show an explicit five-second polling notice only on the gateway's unknown-method response; other failures remain errors. Subscribed `run.step` frames must match both employee and run, and only 256 steps are retained. Persisted history is reconciled periodically. A known non-Claude employee runtime with no step records shows `此 runtime 只回報起訖`; the employee's current runtime is not proof of which runtime produced an older run. Run/tool previews stay hidden by default. Press `c`, read the warning, then `c` again to reveal them; other keys cancel. The warning is once per control-room session, and `c` subsequently toggles content. Previews are truncated, not redacted.

Task blocks read `tasks.list_page`, `tasks.iterations`, `tasks.timeline`, `tasks.changes` and `tasks.stop_status`. Default views show round verdicts, event types and changed paths, without feedback, source snippets or tool payloads. `Enter` opens the existing goal decision card, including finished goals with an explicit follow-up message. An Operator/Owner binding enables `e` to steer an unfinished goal and `s` to stop. Steering opens a text editor (up to 4,000 characters); `Enter` moves to confirmation. The gateway still checks whether steering is enabled. Stop carries the observed `expected_authority_revision`; goal decisions carry `expected_updated_at`.

In a task block, `b` switches to responsibilities for that employee. `[` / `]` selects a responsibility and shows `responsibilities.get` next trigger plus recent `responsibilities.fires`. `p` pauses and `d` disables with `expected_control_epoch`. `o` opens the current occurrence's task using the gateway-provided task ID, where steering and stop use the same task controls. Standing authority, contract changes and global switches stay in the dashboard.

Logs use `logs.subscribe`, require Manager or Admin and retain at most 256 lines of 2,048 characters. Other roles see a reason. Lines pass through `TerminalText`; the gateway has no log replay, so missed lines cannot be reconstructed.

The schedule area shows enabled and paused cron jobs for Admin, and heartbeat state for Manager/Admin. `p` pauses/resumes a cron and `n` runs it once; on a heartbeat, `n` triggers it. Cron run-now includes a fresh `request_id`. The current `cron.list` has no next-run field, and `heartbeat.status` has no last-result field: those values are labelled unavailable, never guessed from cron expressions or inferred as success. Cron expressions, timezone and available last-run/result fields appear in details.

Select an employee and use `p` to pause/resume its status through `agents.pause` / `agents.resume`. It needs Manager plus an Operator/Owner binding (Admin follows the gateway's admin access rule). This branch does not implement `autonomy.pause`: the card explicitly says status pause is not a blanket autonomy pause. No global pause is offered.

Every new control uses a separate decision card, ignores keys for the first second, requires inspecting the full request and entering a fresh two-digit code, and is refused while stale or after its displayed subject changes. A request is never automatically retried. Accepted, refused and conflict responses trigger refetches; accepted stop may still be `cancel_pending`. The UI never invents a successful state. Socket fake-gateway tests and live gateway/native-terminal acceptance remain NOT RUN in the restricted sandbox; in-process fake RPC tests cover request shapes and card state transitions.

## Watch one employee

`duduclaw tui watch adu` prints sanitized lines suitable for a narrow split, without an alternate screen. It tries `runs.subscribe { agent_id }`; the current gateway reports an unknown method, so it explicitly announces polling and uses `runs.list` and `runs.get` every five seconds. Recent history is limited to four runs and the last 256 events per run, with bounded duplicate tracking. A supporting gateway also supplies `run.step` events; persisted history still reconciles periodically. `Ctrl-C` exits. The same AI-session refusal and account login rules apply.

By default, output contains metadata only (time, status, agent, step kind or tool name), with no user/assistant text or tool arguments. Use `duduclaw tui watch adu --content` to include content previews; stderr warns that content is not redacted. Unknown event shapes show only their name in either mode, never their JSON payload.

## Terminal notifications

Notifications are off by default, including under `SSH_CONNECTION`. Explicit `notifications = true` in `tui.toml` opts in. An increased fresh inbox count while the terminal is unfocused emits OSC 9 with only `DuDuClaw：N 項需要你處理`; employee names, task titles, and tool content never enter it. Focus events are used when available; otherwise the terminal is treated as unfocused. At most one notification is emitted per minute; suppressed rises are not queued. Inside tmux, passthrough is used only if `tmux show-option -gv allow-passthrough` returns `on` or `all`; otherwise notifications are skipped. Actual display depends on the terminal.

## Decision cards

Select an inbox row and press `Enter` to open its card. New arrivals only update the list and badges; they never open a card or move focus. All key presses during the first second are ignored. `Esc` closes a card; arrows and `PgUp` / `PgDn` scroll it.

For an approval, type the two-digit confirmation code and press `Enter`; `n` denies immediately. Each opening generates a random code different from the previous opening. All approval operations show the full payload with control characters made visible; the code input stays locked until the bottom is visible. Resizing can lock it again. This only guards against mistyped or blind input: **a process running under the same UID that can read the screen can bypass it**. It is not a person-presence or authentication check. All approvals currently count as high risk: `approvals.list` supplies `kind` but no gateway-produced safety classification; neither action kind alone nor fields inside `payload` can reduce risk. Resize clears typed input and redraws before queued keys are processed. Footer hints follow list, card, or help focus.

Goal cards use `r` for retry. `a` aborts, `d` marks done, and `t` takes over: press the same key twice, with no countdown; another key cancels confirmation. `c` continues a finished goal only after an explicit follow-up message (`i` to edit, `Esc` to finish editing). The inbox lists only `needs_human` tasks; open a finished goal in a task block to continue it. The `tasks.goal_decide` handler accepts `task_id`, `action`, optional `note`, and requires `message` for `continue`. The optional `expected_updated_at` must exactly match the task’s `updated_at` string; comparison and mutation run in the same SQLite transaction for all five actions. A mismatch returns `goal_decide_revision_mismatch` without changing the task or recording a decision. Omission preserves dashboard behavior. The control room always sends the value shown on the card and refuses goal decisions if it is missing.

Cards with `may_decide=false` are read-only and explain why. Goal actions require a known Operator/Owner binding or Admin access from `users.me`. Dashboard-only approvals, questions requiring answers, and bound operations stay in the dashboard. The TUI displays a fixed instruction and never opens a link supplied by the server. Standing-authority and global configuration changes remain in the dashboard.

Approval requires a nonempty string `digest`, sent as `expected_digest`. If it is absent or null (including a redacted viewer payload), approval sends no RPC and the card stays open with the fixed line: **內容未綁定：這張卡無法確認你看到的就是實際內容，請到儀表板決定**. Denial remains available with `n`. Approving from the control room therefore requires **PR #261 merged**, which adds digests to `approvals.list`. A structured digest mismatch closes the card and refetches with a fixed change notice. Every sent decision, including RPC errors and uncertain timeouts, triggers a refetch; no optimistic success or automatic resend. Server errors pass through `TerminalText`; stale, expired, or deadline-free approvals cannot be decided.

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
