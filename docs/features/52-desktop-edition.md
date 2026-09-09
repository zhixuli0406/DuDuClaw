# DuDuClaw OS desktop edition — one machine, shared by a person and the AI

> The desktop edition is a computer you use like any other, with AI
> employees living on the same machine. They do their work without taking
> your screen, your cursor, or your focus: the moment you touch the keyboard
> or mouse, whatever the AI was driving on your desktop stops.

---

## What it is

The whole-disk DuDuClaw OS image (`duduclaw-image-appliance`) boots into
DuDuClaw's own desktop: its own Wayland compositor (`duduclaw-comp`) and
shell (`duduclaw-shell`), not a stock Linux desktop with an agent bolted on.
A person sits at it and uses it as their machine — browser, office suite,
games, Windows and Android apps through the compatibility layer — while the
DuDuClaw gateway and its AI employees run on the same box: answering chat
channels, running goals, sensing the environment, and, when asked, operating
applications on the desktop itself.

The design rule that makes sharing workable is stated once and enforced in
the compositor: **your desktop is yours; human input always wins.** Nothing
below depends on an agent behaving well. The compositor owns the input seats,
the freeze, and the emergency stop, and an agent process cannot reach any of
them.

## The desktop

- **Home screen, window switcher, lock screen, control center, notification
  center** — a lean desktop that is fully operable from the keyboard. The
  complete shortcut list is in [51-os-keyboard-shortcuts.md](51-os-keyboard-shortcuts.md).
- **Cmd+K delegation bar** — from any app, summon the bar and hand a task to
  an AI employee in plain language. The bar is the desktop's entry point to
  the same goal loop the chat channels use ([34-goal-loop.md](34-goal-loop.md)).
- **First-run wizard** — language, network, administrator account, and a few
  preferences on first boot; the live installer ISO is a graphical wizard too.
- **AI runtime authorization** — one step of that wizard lists every AI
  provider the image ships a runtime for (Claude Code, Codex, Gemini CLI,
  Grok, Qwen, Kimi, Copilot, Cursor, Mistral Vibe, OpenCode, plus the
  plain API-key providers — Kiro is a supported runtime but is deliberately
  not bundled in the image), each row showing whether it is unset, has a key
  stored, or is signed in. A row takes an API key — stored through the same
  encrypted `[[accounts]]` path the dashboard uses — or, for the CLIs that
  have one, an interactive sign-in: the machine drives that CLI's own login
  and shows you the device code and verification URL, with a button to open
  it in the appliance's browser. Signing in with a consumer subscription
  shows the risk up front and will not start until you accept it: since
  March 2026 Anthropic and Google block subscription tokens from
  third-party products server-side and accounts have been suspended over it,
  so an API key is the recommended path. The whole step is skippable in one
  click; the summary at the end says how many providers ended up authorized.
- **Input and audio** — fcitx5 Chinese IME, PipeWire / WirePlumber audio,
  XWayland for X11 applications.
- **Apps** — Chromium, LibreOffice and Steam preloaded offline as Flatpaks;
  Windows desktop apps through Bottles, full Windows through a KVM virtual
  machine + RDP, Android apps through Waydroid. What is and is not promised
  is spelled out in [the app compatibility guide](../guides/app-compat.md).

## How sharing works without getting in your way

The compositor knows three driving modes for a co-drive session:

| Mode | Who is driving your desktop | When |
|---|---|---|
| **Human** (default) | You. The agent has no input rights on the shared desktop at all. | Always, unless you delegated a task that needs the GUI. |
| **Shadow** | Nobody on *your* desktop: the agent works on a headless second output (`duduclaw-shadow-0`) with, if you want it, a small picture-in-picture preview in the corner. | The default place for any GUI task. Your windows, focus and cursor are untouched; you keep watching a video or playing a game. |
| **Co-drive** (watch / handover) | The agent drives on your visible desktop and you watch every step. | Only when the task targets the window you are already using, when you pulled the preview to the foreground, or when a sensitive step requires you present. |

The rules, all enforced in `duduclaw-comp` rather than in the agent:

- **Its own seat.** The agent injects input through a dedicated seat
  (`duduclaw-agent`), so every event is attributed to it in the audit trail,
  and its cursor is drawn in a different shape and colour from yours.
- **You touch anything, it freezes.** Any keyboard or pointer event from the
  physical seat freezes the agent seat before the next agent command is
  processed — measured at 3–4 ms in QEMU and container runs against a 50 ms
  design target. Frozen commands are dropped, not queued, so nothing "catches
  up" on you later. Reconnecting does not clear a freeze.
- **Handing back is explicit.** Super+Enter, or the shell's own hand-back
  button. There is deliberately no "resume after N seconds of idle": an
  implicit resume is an accident waiting to happen.
- **Super+Esc is the emergency stop.** It ends the session outright. The
  agent cannot intercept or disable it.
- **Watch mode.** In sensitive contexts the rest of the trajectory requires
  you present; if you walk away, the session pauses and resumes when you
  come back — the one exception to the explicit hand-back rule.
- **A border you cannot miss.** While an agent is co-driving the screen has
  an amber border; in handover it turns dark red; with no session nothing is
  drawn at all.
- **Everyday use is unaffected.** Chat replies, the goal loop, scheduled
  jobs and typed MCP tools never touch the desktop. Shadow work keeps running
  while you use the machine. The only thing a freeze stops is the one
  foreground co-drive session on your desktop.

## What the AI may do on your desktop

- **Typed first, GUI last.** DuDuClaw's own apps are driven through the
  gateway's tools, never through their pixels. Third-party apps go through a
  registry of native APIs, CLIs and D-Bus interfaces where one exists, then
  through the accessibility tree (AT-SPI2) for GTK / Qt / Chromium apps.
  Screenshot-based control is not part of this release.
- **Off by default.** Co-driving is a per-agent capability (`[capabilities]
  codrive`), fail-closed; the `codrive_run` tool is Admin scope.
- **Consequential actions ask first.** Sending, purchasing, deleting,
  granting: an approval card is raised through the same broker every other
  DuDuClaw action uses, and nothing is injected until it is approved. Denied,
  expired, or unreachable broker all abort the step.
- **Credentials are never the agent's.** Login, password and payment steps
  hand the desktop over to you (`take_over`); the agent's perception is frozen
  for the duration. Online banking and CAPTCHA are on a refuse list that does
  not even raise an approval card.
- **What it sees is data, not instructions.** Text read from the screen or
  the accessibility tree is fenced as data before it reaches a model, password
  fields are never read, and injected-looking text is neutralised and logged.
  Injection risk is not zero; human review is the last line of defence.
- **A private, authenticated channel.** The injection socket is
  token-authenticated and never public. On the appliance the gateway runs as
  one user and the compositor/shell as another, so an agent process
  structurally cannot reach the human-side shell control socket. Every event
  lands in a JSONL audit trail.

## Current Status

- As of v0.2.0 (the current release; shipped this way since v0.1.0), the
  whole-disk image ships this desktop. Co-driving is compiled in and
  **off by default**.
- Verified so far, with real input events rather than simulators: freeze /
  hand-back / emergency stop, target highlight, audit, socket-token rotation,
  the shadow workspace with picture-in-picture, agent-initiated handover,
  watch mode, and the mode border — in containers (Xvfb + a real Chromium)
  and in QEMU virtual machines.
- Not yet verified: a real DRM hardware backplane (including two-monitor
  border geometry), the hand-back button on a real screen, the
  gateway-to-compositor status round trip, and accessibility-tree click-through
  on a real machine (paused until that test can be re-run). There is no
  session recording yet.
- The appliance's own security posture (read-only root, firewall, privilege
  separation) and the trust-chain build options are covered in
  [50-duduclaw-os-appliance.md](50-duduclaw-os-appliance.md).

## Further reading

- [50-duduclaw-os-appliance.md](50-duduclaw-os-appliance.md) — the appliance: editions, install, device page, security design.
- [51-os-keyboard-shortcuts.md](51-os-keyboard-shortcuts.md) — every shortcut, including Super+Enter / Super+Esc.
- [34-goal-loop.md](34-goal-loop.md) — what happens after you hand a task to an AI employee.
- [42-human-takeover.md](42-human-takeover.md) — the same "a human speaking wins" rule, applied to chat channels.
- [33-os-native-perception.md](33-os-native-perception.md) — what the AI senses on the machine without touching your desktop.
