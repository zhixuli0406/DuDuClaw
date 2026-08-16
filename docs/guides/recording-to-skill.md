# Recording to skill

Record a single human demonstration (browser or desktop) and distill it into a
replayable SKILL.md draft, which becomes a real skill once an admin approves it. Good
fit for "show the AI employee how to pull a report" or "walk through a form-filling
SOP" once, then let the agent replay it.

## Prerequisites

1. **Enable the capability (off by default)**: add this to the target agent's
   `agent.toml`:

   ```toml
   [capabilities]
   recording = true
   ```

   Without this switch, all five recording tools are refused at the MCP dispatch entry
   point (fail-closed).
2. **MCP scope**: external keys need the `recording` scope (built-in agents' default
   principal already includes Admin).
3. **Browser recording** needs Node.js and the playwright module:

   ```bash
   npm install -g playwright
   npx playwright install chromium
   ```

   If the module can't be found, point to it with environment variables:
   `DUDUCLAW_PLAYWRIGHT_NODE_PATH=/path/to/node_modules`,
   `DUDUCLAW_NODE=/path/to/node`.
4. **Desktop recording** currently supports macOS only, and requires granting Screen
   Recording permission (System Settings → Privacy & Security → Screen Recording).

## Tool overview

| Tool | Purpose |
|------|------|
| `browser_record_start(url, name?, headless?, max_seconds?)` | Opens a real browser with tracing + HAR enabled for a human to demonstrate a workflow |
| `browser_record_stop(id)` | Stops recording and writes `trace.zip` / `session.har` (auto-redacted) / `actions.json` |
| `desktop_record_start(name?, max_seconds?)` | Desktop recording: one screenshot per second + the foreground window title (no keystroke logging) |
| `desktop_record_stop(id)` | Stops desktop recording |
| `skill_from_recording(id, name?)` | Distills a recording into a SKILL.md draft and submits it for approval |

Recordings land in `~/.duduclaw/recordings/<id>/` (directory permissions 700), with a
30-minute (configurable, capped at 2 hours) automatic-stop safety limit.

## Typical flow

1. Tell the agent "I'll show you how to pull the report — record it," and the agent
   calls `browser_record_start(url="https://erp.example.com", name="monthly report SOP")`.
2. The human completes the whole workflow in the opened browser window, then either
   closes the window or asks the agent to call `browser_record_stop(id)`.
3. The agent calls `skill_from_recording(id)`:
   - It parses the redacted HAR (method / URL / body skeleton for non-static-asset API
     calls) plus the sequence of UI actions, and hands both to an LLM to distill into a
     SKILL.md (frontmatter includes `name` / `trigger` / `skill_type` /
     `requires_env`).
   - It first runs a deterministic security scan (including prompt-injection rules);
     any High/Critical finding blocks it outright.
   - Once it passes, the file is written to the isolated draft area
     `~/.duduclaw/skills-drafts/<id>/SKILL.md`, and an approval request is created.
4. An admin approves it in the dashboard's approval center, and the skill installs and
   activates automatically; a rejection leaves it in the draft area.

## Security design

- **No silent background recording**: both start and stop produce an explicit reply
  and log signal.
- **HAR redaction**: header values like `Authorization` / `Cookie` / `Set-Cookie`, every
  cookie value, and any token-like query parameter or JSON body field are all replaced
  with an `<env:VAR>` placeholder; the distilled SKILL.md lists the needed environment
  variables under `requires_env` and never contains real credentials.
- **Desktop recording never touches keystrokes**: it only records which window is in
  the foreground; an input-event stream (rdev) is not yet implemented.
- **Never goes straight into the skill library**: every distilled artifact goes through
  the same self-built skill approval pipeline (isolated draft area → manual approval →
  install), with another security scan run at install time.

## Known limitations

- Desktop recording is currently a degraded "screenshot + foreground window" mode: no
  input-event stream, and distillation relies solely on the window-switch sequence.
- Desktop replay is fundamentally a computer-use task (`skill_type: desktop-sop`) —
  replay executes step by step, verifies each step with a screenshot, and stops on any
  failure.
- Browser recording needs a locally available Playwright; `headless=true` is only
  suitable for verification (use the default headed mode for human demonstrations).
