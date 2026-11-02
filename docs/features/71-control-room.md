# Terminal control room

[繁體中文](zh-TW/71-control-room.md) · [日本語](ja-JP/71-control-room.md)

`duduclaw tui` shows what your AI employees are doing while you are not watching, and lets you stop or steer them from a terminal. It is a client of the gateway: every action goes through the same authorization as the dashboard, and the room holds no authority of its own.

Keystrokes and commands are in the [guide](../guides/tui.md). This page explains what the room is for and where its limits are.

## What it is for

Employees work on goals, schedules and responsibilities without anyone asking. Three questions follow from that: what is running now, what needs me, and how do I stop it. The room answers those three in one screen, over SSH if needed.

| Question | Where |
|---|---|
| What needs me? | The inbox: approvals, goals waiting for a reply, breaker notices. Each row says who raised it and why. |
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

## Terminals

The list below records what has actually been run. A terminal missing from it may work; it has not been checked.

| Terminal | Status |
|---|---|
| tmux on macOS | Full screen tested on a real gateway. Notifications use tmux passthrough where tmux allows it. |
| macOS and Linux terminals over a pseudo-terminal in automated tests | Rendering covered by snapshots at 60 columns and wider, down to 12 rows. |
| Windows, `tui inbox` over SSH | Tested on a native Windows machine. |
| Windows full screen | Not verified. SSH cannot exercise it. |
| Linux native on real hardware | Not verified yet. |

The room needs at least 24 columns and 12 rows, and says so when the window is smaller. Status glyphs are ASCII by default; set `glyphs = 'unicode'` in `tui.toml` if your font has them. The room handles CJK width and whole committed characters, but an input method has not been tested by hand.
