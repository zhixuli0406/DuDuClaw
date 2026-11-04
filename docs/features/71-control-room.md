# Terminal control room

[繁體中文](zh-TW/71-control-room.md) · [日本語](ja-JP/71-control-room.md)

`duduclaw tui` shows what your AI employees are doing while you are not watching, and lets you stop or steer them from a terminal. It is a client of the gateway: every action goes through the same authorization as the dashboard, and the room holds no authority of its own.

Keystrokes and commands are in the [guide](../guides/tui.md). This page explains what the room is for and where its limits are.

## What it is for

Employees work on goals, schedules and responsibilities without anyone asking. Three questions follow from that: what is running now, what needs me, and how do I stop it. The room answers those three in one screen, over SSH if needed.

| Question | Where |
|---|---|
| What needs me? | The inbox: approvals and goals waiting for a reply, each saying who raised it and why. Breaker notices appear there as information; they do not count as needing you or trigger a desktop notification. They can raise an in-app error toast. |
| What is running? | Employee list, run and task blocks, next scheduled runs, `duduclaw tui watch`. |
| How do I stop it? | `p` pauses autonomous work for the selected employee, `u` resumes. Admin can pause everyone from the palette. |

Conversation messages now use right-aligned user blocks and plain employee replies with a name/time header. Tool steps and round results are grouped beneath their own turn. Pending approvals and paused goals appear above the persistent input as an inline card; numbered actions are labels only, and `Enter` opens the existing guarded decision card. Permission checks, confirmation codes, version binding and no automatic retry are unchanged. Centered overlays and bottom-right toasts use raised surfaces over a muted background, with dark/light/ANSI/NO_COLOR variants. `motion = "on"` animates only active tool steps at about 80 ms; `"off"` uses static glyphs. The guide describes input focus, the preserved draft and overlay keys.

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

Text from employees is untrusted. Names, task titles, tool output and chat replies are cleaned of terminal control sequences before they are drawn, and links are not made clickable. Desktop notifications carry fixed text and a number, never a name or content, and are off by default. In-app toasts and the notification mailbox use fixed wording plus a sanitized employee display name only; they never contain message, task or tool content.

The embedded terminal block belongs to you and does not pass through the gateway. A "waiting for you" marker from it is display only: it authorizes nothing and answers nothing. It is not available on Windows.

Run the room on the gateway host, over SSH if remote. Do not forward the gateway port: that would expose its local login route to the forwarding host.

## Appearance

The layout follows the dashboard: a neutral app-shell background, a borderless sidebar about 34 cells wide, and a page-canvas content panel inset by one cell with a single faint rounded ring. The sidebar starts with the brand and a search row (`Ctrl-P` opens the existing palette), followed by needs-you, working and idle groups. Paused employees belong to idle and retain the pause symbol and word. Each employee has a two-line row with a right-aligned status pill or metadata and muted context. Selection fills the row and adds `›`; unfocused selection is quieter. Unselected employees with unread inbox items carry `✦` (`*` in ASCII). Below 100 columns, or at short heights, employee rows collapse to one line. Overflow shows how many employees are hidden; arrow navigation keeps the selected row visible.

Chat, task and record tabs still use `Tab` / `Shift-Tab`, and `[` / `]` keep record navigation. `I` opens the inbox; Manager/Admin can open schedules with `S`. Splits, PTYs, decision cards and autonomous-work blocks remain available. Connection and permitted cost sit at the bottom of the sidebar, beneath inbox and schedule counts. Cost is hidden when unavailable or refused; dollars use two decimals, or four for nonzero amounts below $0.01. The content header shows the employee status/name, configured runtime/preferred model from the existing employee snapshot when available (not the model actually used after a runtime fallback), and right-aligned tabs with a hover-tone active pill. Key hints are at the bottom of the content panel, with the gateway version right-aligned. There is no global header or filter row. `?` includes the full home path. The minimum remains 24×12; snapshots cover 80×24, 100×30, 140×40 and 200×50 in dark, light, ANSI and NO_COLOR, with additional ASCII and ambiguous-width checks. These are rendering checks, not native-terminal acceptance of this redesign.

In-app toasts show `完成`, `需要你` or `錯誤` in success, warning or danger colors at the main area's bottom right. At most two are visible, newest on top. Completion and error notices expire after about six seconds; needs-you notices remain until seen or dismissed. They never take focus or cover a decision card. `F8` explicitly focuses toasts; arrows select and `x` dismisses.

`M` opens a mailbox of the process's last 100 notifications. `N` remains goal creation. `Tab` filters all, unread or errors; arrows select, `Enter` jumps to the employee or inbox item and `Esc` closes. Opening the mailbox marks current notices read and dismisses their toasts; new arrivals remain unread until their notification is opened with `Enter`, their related inbox item is opened or marked read, or the mailbox is closed and reopened. History and unread state stay in memory and do not change gateway requests.

`z` enlarges the focused block, hides the sidebar and content header, and keeps a restore hint in the footer. Press it again to restore the layout. Decision cards and the palette remain usable. Chat and record tabs show compact activity timelines: each available tool step or round result occupies one line. Use `t`, arrows and `Enter` to focus, select and expand/collapse existing details. All text remains sanitized and width-aware, and run content still requires the existing `c` consent. These views reuse already received data without new RPCs; `tui watch` is unchanged.

`theme = "auto"` uses `COLORFGBG`, else dark; set `theme = "light"` explicitly for light terminals. Explicit `light` and `dark` select their respective palettes. `theme = "ansi"` uses only the 16 ANSI colors and leaves canvas/surface backgrounds to the terminal; it determines light/dark from `COLORFGBG`, else dark. `background = "terminal"` leaves the app-shell and page-canvas backgrounds to the terminal in any theme, while preserving card/selection surfaces. Color support otherwise follows `COLORTERM=truecolor` or `24bit`, then a `TERM` containing `256color`, then 16 ANSI colors. Any `NO_COLOR` value, including empty, disables colors while retaining selection and emphasis cues.

The four surfaces use the dashboard’s neutral zinc palette; amber is for needs-you/paused states and keys, green for working, and brand blue for the logo. Text/surface pairs below 4.5:1 receive the smallest supported foreground adjustment; the decorative ring deliberately stays below text contrast. Tests cover reference truecolor/256/16-color pairs. Actual ANSI contrast depends on the terminal’s customized palette; terminal-background contrast uses COLORFGBG or a fallback, so an unknown background cannot be guaranteed.

The 16-color contrast calculations use the reference ANSI palette; terminal-customized ANSI colors determine the actual displayed contrast.

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
