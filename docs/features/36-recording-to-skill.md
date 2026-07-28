# Recording to Skill

> Show the agent once — a browser or desktop demonstration becomes a replayable SKILL.md draft, and a human approves it before it ever runs.

---

## What It Is

Some SOPs are easier to demonstrate than to describe: look up a monthly report in the ERP, file a form, walk a vendor portal. Recording to Skill closes that loop. You perform the task once while the agent records; the recording is distilled into a SKILL.md draft; an admin reviews and approves it in the dashboard; only then does it become an installed skill.

Five MCP tools drive the loop:

| Tool | Purpose |
|---|---|
| `browser_record_start(url, name?, headless?, max_seconds?)` | Open a real browser with tracing + HAR and record a manual demonstration |
| `browser_record_stop(id)` | Stop; land `trace.zip` / `session.har` (redacted) / `actions.json` |
| `desktop_record_start(name?, max_seconds?)` | Desktop recording: 1 fps screenshots + foreground window titles |
| `desktop_record_stop(id)` | Stop the desktop recording |
| `skill_from_recording(id, name?)` | Distill the recording into a SKILL.md draft and submit it for approval |

## Two Recorders

**Browser** (needs local Node.js + Playwright): a real Playwright context runs with tracing, HAR capture, and an injected action recorder, driven by a small Node script materialized into the recording directory. You demonstrate in a headed window; `headless=true` exists for verification only. Module discovery can be overridden with `DUDUCLAW_PLAYWRIGHT_NODE_PATH` and `DUDUCLAW_NODE`.

**Desktop** (macOS only for now, needs the Screen Recording permission): a detached worker subprocess captures one screenshot per second plus the foreground window title. It deliberately records **no keystrokes** — an input-event stream is not implemented, so distillation works from the window-switch sequence and screenshots alone.

## Secrets Never Leave the Machine

The HAR is redacted **in place** at stop time, before anything reads it downstream:

- Values of credential headers (`Authorization`, `Cookie`, `Set-Cookie`, `x-api-key`, and friends) are replaced.
- All cookie values are replaced.
- Query parameters and JSON body fields whose names look credential-like (`token`, `secret`, `password`, `api_key`, `session`, `signature`, …) are replaced.

Every replacement becomes an `<env:VAR>` placeholder, and the distilled SKILL.md lists those variables under `requires_env` in its frontmatter — the skill documents *which* credentials it needs without ever containing one.

## Distillation and the Approval Gate

`skill_from_recording` parses the redacted HAR (non-static API calls: method, URL, body skeleton) together with the UI action sequence, and has the LLM distill a SKILL.md draft (frontmatter: `name` / `trigger` / `skill_type` / `requires_env`).

The draft **never lands directly in a loadable skill library**:

1. A deterministic security scan (including the prompt-injection ruleset) runs before submission — High/Critical risk is refused outright, fail-closed.
2. What passes is staged in an isolated drafts area (`~/.duduclaw/skills-drafts/<id>/SKILL.md`) with a pending-approval record routed to a human through the shared ApprovalBroker.
3. Only the dashboard approval action installs the skill — and that install runs its own re-scan. A rejection leaves the draft in quarantine.

Desktop-derived skills carry `skill_type: desktop-sop` and replay as computer-use tasks: step by step, with a screenshot verification after each step, stopping on the first failure.

## The Capability Gate

All five tools are deny-by-default. The MCP dispatch gate requires **both** `[capabilities] recording = true` in the calling agent's `agent.toml` **and** the `recording` MCP scope on the caller's key — either one missing means refusal, fail-closed. Recording is something an operator turns on per agent, not something any agent can reach for.

## Runaway and Filesystem Hygiene

- Recording is never silent: start and stop each produce an explicit reply and a log signal.
- A hard auto-stop caps every session at 30 minutes by default (configurable up to 2 hours) — a forgotten recording cannot run unbounded.
- Artifacts live under `~/.duduclaw/recordings/<id>/` with owner-only permissions (700 directories, 600 files; Windows inherits profile ACLs).
- Recording ids follow a strict `rec-<timestamp>-<hex>` format validated before any filesystem use — an id is a path component, so anything else is rejected.
- HAR files above 50 MB are not parsed in-process (OOM protection); `*_record_stop` waits up to 30 seconds for the worker to flush artifacts.

## Limits

| Aspect | Limit |
|---|---|
| Session length | 30 min default, 2 h hard max |
| HAR parsed in-process | ≤ 50 MB |
| Desktop input capture | window titles only, no keystrokes |
| Desktop platform | macOS (Screen Recording permission required) |
| Install path | approval pipeline only — no direct install |
