# Terminal control room

[繁體中文](zh-TW/tui.md) · [日本語](ja-JP/tui.md)

`duduclaw tui` opens a read-only view of pending approvals, goals needing a reply, and employee status. Select a row to inspect its details. Where the gateway provides it, administrators also see the next scheduled autonomous run. Closing the control room does not stop employees.

This first version does not approve or reject requests, send chat messages, pause employees, or provide `tui watch`. Decisions and chat will come later; use the dashboard for those actions now. The terminal interface currently uses Traditional Chinese.

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
| `Enter` | Open read-only details |
| `r` | Mark this item or identified completed run as read on this client |
| `PgUp` / `PgDn` | Scroll details |
| `?` / `F1` | Toggle help |
| `q` / `Ctrl-C` | Exit |

New items never take selection. Read marks do not dismiss pending requests at the gateway. Completion badges use recent run history, not a complete history of everything you have missed.

On disconnect, lost events, or an incomplete refetch, lists show `資料可能過期` (“data may be stale”) and counts become `?`; JSON uses `stale: true` and `count: null`. The last complete list remains visible. A refetch has a shared limit of 2,000 rows and 4 MiB of encoded response content across employees, inbox, recent runs, and schedules. Exceeding either limit stops that refetch and keeps it stale, never publishes a partial list as complete. Responses and events are limited to 256 KiB per WebSocket message/frame, with a 16-event queue. Large installations may need the dashboard.

## Local files

| File under home | Use |
|---|---|
| `tui-state.json` | Written when marking items read. Contains identifiers only; atomically replaced through a temporary file in the same directory. |
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
