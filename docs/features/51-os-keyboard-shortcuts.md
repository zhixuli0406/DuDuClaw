# DuDuClaw OS keyboard shortcuts

> The complete keyboard shortcut reference for the DuDuClaw OS (a bootable
> appliance image — see
> [50-duduclaw-os-appliance.md](50-duduclaw-os-appliance.md)) desktop
> environment. If you installed DuDuClaw the regular server way and drive it
> through the web dashboard, this page doesn't apply to you — it covers the
> appliance's own desktop.

## What it is

DuDuClaw OS boots straight into a lean desktop environment — a home screen,
a window switcher, a lock screen, and a setup wizard on first boot — all of
it operable without a mouse. This page groups every shortcut by where it's
active, so you don't have to hunt through menus (there aren't many) or
guess.

Two shortcuts on this page are new this round: **Tab** moves between input
fields during first-time setup, and **Cmd+K** now summons the global
delegation bar no matter which app currently has focus.

## Global — active over any app on the device

This group is handled by the system itself, layered above every app — even
when a third-party app currently holds keyboard focus, these keys still
work. And structurally, an AI agent operating the machine can never trigger
or intercept any of them; they're reserved for the human sitting at the
physical keyboard.

| Key | What it does |
|---|---|
| **Cmd+Esc** | Emergency stop. Immediately halts whatever the AI agent is currently doing on screen and hands control back to you. Works no matter what state the screen is in. |
| **Cmd+Return** | Hands control back to a human after the AI agent finishes acting on screen — the calmer, deliberate counterpart to the emergency stop above. |
| **Cmd+K** | Summons the delegation bar over whatever app you're currently using — DuDuClaw's quick entry point for handing a task to an AI employee. **New this round.** |
| **Alt+Tab** (or **Cmd+Tab**) | Cycles windows in most-recently-used order. Hold the modifier and press Tab repeatedly to step through them; release to switch to whichever window is highlighted. Hold **Shift** at the same time to step in reverse. |
| **Esc** (while cycling) | Cancels the window switch and stays on the current window. |
| **Cmd+Q** | Closes the frontmost window. |

## Shell interface — home screen, delegation bar, control center

This group only works while DuDuClaw's own screen holds keyboard focus (if
focus is on a third-party app, use the global **Cmd+K** above instead).

| Key | What it does | Where it works |
|---|---|---|
| **Cmd+K** | Opens or closes the delegation bar. | Home screen |
| **Esc** | Closes whichever panel is currently open — delegation bar, notification center, or control center (only one is ever open at a time). | Home screen |
| **Cmd+L** | Locks the screen immediately, the same as picking "Lock" from the power menu. | Home screen |

## First-time setup

DuDuClaw OS walks through a short setup flow on first boot — language,
network, creating an administrator account, and a few preference screens.
You can get through the whole thing with just these three keys, no mouse
needed.

| Key | What it does | Where it works |
|---|---|---|
| **Enter** | Continues to the next step. On the two steps that need a server round trip before they can continue — creating the administrator account, joining a Wi-Fi network — Enter first triggers that step's own action button (e.g. "Create account", "Connect", if you haven't pressed it yet), then advances only once it succeeds. It behaves the same way pressing Enter after typing a password does on an ordinary login form. | First-time setup |
| **Esc** | Goes back one step. Does nothing on the first step — there's nothing before it. | First-time setup |
| **Tab** / **Shift+Tab** | Moves the cursor to the next (or, with Shift, the previous) text field in the current step — for example between the name and password fields on the account-creation step, or into the Wi-Fi password field. Pressing Tab on the last field wraps back to the first. **New this round.** | First-time setup |

## Lock screen

| Key | What it does |
|---|---|
| Any key or click | Shows the password field (if it isn't already showing). |
| **Enter** | Shows the password field if it isn't showing yet; if it's already showing and you've typed something, submits the unlock attempt. |

## Further reading

- [50-duduclaw-os-appliance.md](50-duduclaw-os-appliance.md) — the appliance
  this desktop runs on, and where first-time setup fits in the overall
  onboarding flow.
