# Terminal control room

[繁體中文](zh-TW/tui.md) · [日本語](ja-JP/tui.md)

`duduclaw tui` opens a control room for pending approvals, goals needing a reply, and employee status. Select a row to inspect its details. Where the gateway provides it, administrators also see the next scheduled autonomous run. Closing the control room does not stop employees.

Approval and goal decision cards are available in the full control room. Chat, notifications, employee pause controls, and `tui watch` are deferred. The terminal interface currently uses Traditional Chinese.

## Start

Start the gateway, then run one of these commands on the same host:

```sh
duduclaw tui
duduclaw tui inbox
duduclaw tui inbox --follow
duduclaw tui inbox --follow --json
```

The full screen needs a terminal. `inbox` prints one snapshot and exits; `--follow` prints a new complete snapshot when it changes. `--json` produces one JSON object per line on stdout, with `type`, `stale`, `count`, `error`, and `items`. Output itself needs no TTY; if account login is needed, an interactive terminal is required. There is no token flag or credential file for unattended login.

The CLI connects to `127.0.0.1`, using `DUDUCLAW_PORT`, then `<home>/config.toml [gateway] port`, then the default gateway port. Home is `DUDUCLAW_HOME` or `~/.duduclaw`. This version has no remote URL flag.

**For remote use, SSH into the gateway host and run it there; do not port-forward the gateway.** Port forwarding would expose its loopback-only local login route to programs on the forwarding host.

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

New items never take selection. Read marks do not dismiss pending requests at the gateway. Completion badges use recent run history, not a complete history of everything you have missed.

On disconnect, lost events, or an incomplete refetch, lists show `資料可能過期` (“data may be stale”) and counts become `?`; JSON uses `stale: true` and `count: null`. The last complete list remains visible. A refetch has a shared limit of 2,000 rows and 4 MiB of encoded response content across employees, inbox, recent runs, and schedules. Exceeding either limit stops that refetch and keeps it stale, never publishes a partial list as complete. Responses and events are limited to 256 KiB per WebSocket message/frame, with a 16-event queue. Large installations may need the dashboard.

## Decision cards

Select an inbox row and press `Enter` to open its card. New arrivals only update the list and badges; they never open a card or move focus. All key presses during the first second are ignored. `Esc` closes a card; arrows and `PgUp` / `PgDn` scroll it.

For an approval, type the two-digit confirmation code and press `Enter`; `n` denies immediately. Each opening generates a random code different from the previous opening. High-risk (`admin`, shell-type, or unclassified) operations show the full payload with control characters made visible; the code input stays locked until the bottom is visible. Resizing can lock it again. This only guards against mistyped or blind input: **a process running under the same UID that can read the screen can bypass it**. It is not a person-presence or authentication check.

Goal cards use `r` for retry. `a` aborts, `d` marks done, and `t` takes over: press the same key twice, with no countdown; another key cancels confirmation. `c` continues a finished goal only after an explicit follow-up message (`i` to edit, `Esc` to finish editing). The current inbox lists only `needs_human` tasks, so it does not offer `continue`; use the dashboard for finished goals. The current `tasks.goal_decide` handler accepts `task_id`, `action`, optional `note`, and requires `message` for `continue`. It does **not** accept/check a revision, so this client does not claim revision protection or send an invented field.

Cards with `may_decide=false` are read-only and explain why. Goal actions require a known Operator/Owner binding or Admin access from `users.me`. Dashboard-only approvals, questions requiring answers, and bound operations stay in the dashboard. The TUI displays a fixed instruction and never opens a link supplied by the server. Standing-authority and global configuration changes remain in the dashboard.

The client sends `expected_digest` only when a card supplies a string `digest`, omitting it for null/missing values. A structured digest-mismatch rejection closes the card, refetches, and displays a fixed “card changed” message. **This checkout's gateway does not yet return or check that digest**, so payload-change protection is not available against this gateway. Every decision, including RPC errors and uncertain timeouts, triggers a refetch; the client never invents a successful result or automatically resends a decision. Server error text is displayed through `TerminalText`. Stale or expired approvals are never actionable; an absent expiry also disables decisions.

`s` in the list or card snoozes the row locally for up to ten minutes. Approvals are capped at 80% of remaining TTL and resurface no later than when 20% of TTL or ten minutes (whichever is less) remains, with a reason; an earlier requested wake-up wins. A shortened deadline also resurfaces the row. Expiry means denial, and snooze cannot extend it. Only identifiers and timestamps are saved in `tui-state.json`; the line-oriented `inbox` command continues to show the authoritative pending list.

## Local files

| File under home | Use |
|---|---|
| `tui-state.json` | Written when marking items read or snoozing. Contains read identifiers and snooze timestamps only; atomically replaced through a temporary file in the same directory. |
| `tui.toml` | Optional, operator-created settings; the TUI reads it and does not create or rewrite it. |

```toml
# ~/.duduclaw/tui.toml
glyphs = "ascii"                 # or "unicode"
ambiguous_width = "narrow"        # or "wide", matching your terminal
poll_interval_seconds = 15        # 5..3600
```

Invalid settings fall back with a notice. Borders use ASCII. Both files are protected from employee writes by the agent-file-guard's existing home-directory policy.

## Security limits

Every request is authorized by the gateway. The client uses account JWTs held in memory and refuses legacy admin tokens or `system` identities. It never reads gateway databases as a fallback. It installs no logging subscriber, log file, or telemetry exporter, even with `RUST_LOG=trace`. On Unix it disables core dumps before reading credentials. Application-owned secret buffers are wiped where practical; console, HTTP, TLS, and WebSocket libraries can retain copies that this client cannot guarantee to wipe. Windows crash-dump suppression is not implemented, so the “no secret on disk” guarantee holds only on Unix.

AI-session refusal and the Bash command guard are speed bumps. An employee with unrestricted shell access under the same OS user can evade them and modify files directly. Use OS isolation, the task sandbox, or withhold Bash for actual containment.

Gateway text is sanitized for terminal display, and details make removed control characters visible. Standard variation selectors U+FE00–U+FE0F remain for CJK/emoji presentation; tag-plane characters U+E0000–U+E0FFF, including supplementary variation selectors, are removed. JSON preserves source text; terminal stdout additionally escapes every character forbidden by the sanitizer (including bidi and default-ignorable characters), while non-terminal output is unchanged. After decoding, consumers must sanitize decoded fields before displaying them in a terminal.

The transport library accepts plaintext only for exact loopback addresses. `localhost` is pinned to `127.0.0.1` for REST and WebSocket connections, ignoring hosts/DNS changes. TLS verification remains enabled; a TLS endpoint named `localhost` must have a certificate valid for the pinned IP. IPv6 loopback is available to library callers as `[::1]`.
