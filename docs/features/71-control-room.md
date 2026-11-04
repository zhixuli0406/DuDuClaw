# Terminal control room

[繁體中文](zh-TW/71-control-room.md) · [日本語](ja-JP/71-control-room.md)

`duduclaw tui` shows what your AI employees are doing while you are not watching, and lets you stop or steer them from a terminal. It is a client of the gateway: every action goes through the same authorization as the dashboard, and the room holds no authority of its own.

Keystrokes and commands are in the [guide](../guides/tui.md). This page explains what the room is for and where its limits are.

## What it is for

Employees work on goals, schedules and responsibilities without anyone asking. Three questions follow from that: what is running now, what needs me, and how do I stop it. The room answers those three in one screen, over SSH if needed.

| Question | Where |
|---|---|
| What needs me? | The inbox: approvals and goals waiting for a reply, each saying who raised it and why. Breaker notices appear there as information; they do not count as needing you and do not trigger a notification. |
| What is running? | Employee list, run and task blocks, next scheduled runs, `duduclaw tui watch`. |
| How do I stop it? | `p` pauses autonomous work for the selected employee, `u` resumes. Admin can pause everyone from the palette. |

## Stopping work

Three controls look alike and do different things.

| Control | What stops | What keeps working |
|---|---|---|
| Autonomy pause (`p`) | Goal rounds, cron, heartbeat, responsibilities, autopilot actions and assigned schedules for that employee. It is sent immediately, with no confirmation, because an emergency stop must not wait. | The current turn finishes. A person can still talk to the employee. |
| Employee status pause (palette) | Changes the employee's own status, with a confirmation card. | Nothing autonomous runs while the status is not operational. |
| Cancel a chat turn | That turn. If delegated runs are still going, the room asks whether to stop them too. | Runs you leave alone finish. They cannot delegate further under that turn. |

The status line shows who is paused. If the pause state cannot be read, the gateway treats autonomous work as paused and the room says so. While a change has not been confirmed, the room shows 待確認 rather than guessing.

## What the room will not do

- It never grants standing permission or changes global behaviour. Those stay in the dashboard, where the whole form is in view.
- Approvals that need a person present still open in the browser.
- An external action that already ran cannot be recalled by stopping the run.
- Task-board tasks and goals a turn created are not followed when that turn is cancelled.

## Trust boundaries

Text from employees is untrusted. Names, task titles, tool output and chat replies are cleaned of terminal control sequences before they are drawn, and links are not made clickable. Notifications carry fixed text and a number, never a name or content, and they are off by default.

The embedded terminal block belongs to you and does not pass through the gateway. A "waiting for you" marker from it is display only: it authorizes nothing and answers nothing. It is not available on Windows.

Run the room on the gateway host, over SSH if remote. Do not forward the gateway port: that would expose its local login route to the forwarding host.

## Appearance

The room uses a sidebar workspace. Employee cards sort by needs-you, error, working, idle, then paused, and show the most useful available context: a waiting item, a goal round, the next heartbeat or a pause reason. Unread inbox items get an accent rail; the selected card gets a filled background. Below 100 columns, cards become a symbol and short name. `I` opens the sidebar inbox in the main area; Manager/Admin can open the full schedule list with `S`. Inbox counts include informational rows, which remain separate from actionable counts.

The main area has chat, task and record tabs (`Tab` / `Shift-Tab`); `[` / `]` keep their record-navigation meaning. Existing splits, PTYs, decision cards and autonomous-work blocks remain available. The one-line header shows connection, version and cost (two decimals, four only for nonzero amounts below $0.01); help includes the full home path. Schedules that have never run say `尚未執行`. Light/dark palettes, status colors, focused borders and readable local times remain in use.

`theme = 'auto'` is the default in `tui.toml`: startup tries OSC 11 with an approximately 150 ms timeout, then `COLORFGBG`, then dark. Choose `light` or `dark` explicitly to skip detection. Colors adapt to truecolor, 256-color or 16-color terminals. Any `NO_COLOR` value, including an empty value, disables color while keeping bold and reverse-video cues.

Native Windows consoles, non-TTY input, `TERM=linux` or `dumb`, and `NO_COLOR` skip OSC 11 and use the fallback order. The [guide](../guides/tui.md#local-files) describes the bounded input recovery and reference-palette contrast.

## Terminals

The list below records what has actually been run. A terminal missing from it may work; it has not been checked.

| Terminal | Status |
|---|---|
| tmux on macOS | Full screen tested on a real gateway, including a reply from a real local model, a mid-reply cancel, and mouse input reaching programs in the terminal block (X10, SGR, urxvt and SGR-Pixels encodings, and `vim`). Notifications use tmux passthrough where tmux allows it. |
| Terminal.app on macOS | Full screen tested on a real gateway: drawing, CJK width, `p` and `u`, CJK and emoji input, the terminal block, clean exit. Keys came from a test program, not a physical keyboard. |
| macOS and Linux terminals over a pseudo-terminal in automated tests | Rendering covered by snapshots at 60 columns and wider, down to 12 rows. |
| Windows, `tui inbox` over SSH | Tested on a native Windows machine. |
| Windows 11 full screen under ConPTY | Tested on a real gateway on a native Windows machine, with a test program driving a real Pseudo Console: drawing, resize, too-small message, `p` and `u` confirmed in the gateway, CJK input, clean exit. Windows Terminal itself has not been used. |
| Ubuntu 22.04 on real hardware, full screen under a PTY | Tested on a real gateway: drawing, resize, too-small message, `p` and `u` confirmed in the gateway, clean exit and terminal restore. A physical terminal emulator has not been used. |

The room needs at least 24 columns and 12 rows, and says so when the window is smaller. Status glyphs default to Unicode in a UTF-8 locale unless `TERM` is `linux` or `dumb`; otherwise they use ASCII. An explicit `glyphs = 'ascii'` or `'unicode'` in `tui.toml` overrides detection. Unicode mode uses rounded borders. The room handles CJK width and whole committed characters, but an input method has not been tested by hand.
