# OS-Native Perception & Proactive Care

> Your AI employee notices how you work on this machine — watching only what you told it to, remembering only aggregates, and speaking up only when it's worth interrupting you.

---

## What It Is

An opt-in sensing layer (v1.42–1.46) that lets a designated agent perceive local OS activity — file changes in chosen folders and which app is in the foreground — and act on it three ways:

1. **Automation** — OS events feed the existing autopilot rule engine (`os_file` / `os_frontmost` events), with one-click templates on the OS page.
2. **Memory** — a daily distillation turns the raw event stream into a handful of temporal-memory facts about how the user works.
3. **Care** — an hourly proactive check reads the day's app-usage summary and may send one short, useful message to your most recent conversation.

Everything is off by default. Each layer is a separate switch in `agent.toml`.

## The Sensing Layer

**Filesystem watch** (`os_events.rs`): agents with `[capabilities] os_native = true` and a non-empty `[os_watch] paths` list get a debounced, rate-limited watcher per agent (`debounce_ms`, `max_events_per_min`). Events go onto the same broadcast bus the autopilot engine already consumes. Watchers hot-reload: editing `[os_watch]` via `agents.update` stops/starts that agent's watcher in place, no gateway restart. Per-agent counters persist to `<home>/os_watch_stats.json` for the `os_watch_status` MCP tool.

**Frontmost polling** (`os_frontmost.rs`): a positive `[os_watch] frontmost_poll_secs` spawns a low-frequency poll loop. An event is emitted only when the foreground app or window actually changed — an idle desktop produces nothing. Each app switch appends one `{"ts","app"}` line to a daily JSONL log at `<home>/os/<agent>/frontmost-<date>.jsonl`; only today's and yesterday's files are kept. The poller is a pure sensing source: it never computes idle state (that stays with the heartbeat path).

## Data Minimization

The privacy rules are enforced at collection time, not just at write time:

- **Window titles are never written to disk.** The daily log stores app name + timestamp only — the minimum the care summary needs.
- **File names never leave the event.** Footprint aggregation reduces every path to its containing directory before doing anything else, so the highest-risk substring (`quarterly-layoffs-draft.docx`) is dropped at the source.
- **Every perceived string passes the perception sanitizer** (`sanitize_perception_text`) before it enters a prompt, an aggregation key, or memory — the same boundary all OS-sensing paths share.
- **Non-opted-in agents are never tracked.** An agent without the footprint flag is not added to the in-memory tracking map at all, not merely skipped at persistence.

## Footprint → Temporal Memory

`footprint_distill.rs` subscribes to the same event bus and aggregates per agent, per UTC day: foreground-app seconds, active-directory counts, and an hour-of-day activity histogram. Once a day (when the tracked day rolls over), it distills the finished day into **up to three** deterministic `(subject="user", predicate, object)` triples via `store_temporal` — write rate is O(agents × days), never one row per event. The existing supersession chain closes out yesterday's `daily_active_app` fact automatically, and Ebbinghaus retrievability ranks what keeps being recalled.

Details that matter:

- Opt-in via `[os_watch] footprint = true`, layered on top of `os_native` — filesystem watching alone does not grant footprint memory.
- Buckets snapshot to `<home>/os/<agent>/footprint-aggregate.json` (atomic tmp+rename) on every distill tick; a restart loses at most one check interval, not the whole day.
- Every write is stamped `origin = "agent_derived"` (trust ceiling 0.6, per the v1.41 write-time origin binding) and carries a sensitivity label: `daily_active_app` / `active_hours` → Personal, `active_directory` → Internal.

## Proactive Care

The heartbeat scheduler runs a proactive check per agent on its schedule:

```text
check due? → quiet hours? skip → rate limit? skip
  → call Claude with the check prompt + MCP tools
  → response contains PROACTIVE_OK → discard silently
  → otherwise → send to the notify target
```

- **Built-in default check**: an `os_native` agent with no hand-written `PROACTIVE.md` uses `DEFAULT_OS_CARE_CHECKS`, a conservative built-in ("worked 2h straight → suggest a break; still going after 23:00 → one short check-in; meetings dominating → offer to organize todos"). Default answer is `PROACTIVE_OK` — silence over noise. Previously, agents collected a day of data and never acted on it because the check silently skipped without a `PROACTIVE.md`.
- **OS context**: `frontmost_daily_summary` reads the daily JSONL log and produces a ranked app-time summary (gaps over 30 min are treated as "walked away", apps under 1 min dropped, top 6 kept, app names sanitized). App names and durations only, never window content.
- **Quiet hours + rate limit**: timezone-aware quiet hours (may span midnight) and a sliding-window per-hour message cap.
- **Notify-target fallback**: with no explicit target, the message goes to the agent's most recent *pushable* channel conversation from `sessions.db` (Discord/Telegram/LINE/Slack/WhatsApp/Feishu/Google Chat/Teams — WebChat is pull-only and excluded).

## The Proactive Gate

For rule-driven interruptions there is a separate LLM-scored gate (`proactive_gate.rs`), landed as a new autopilot action `proactive_notify` — deterministic `notify` rules are untouched and fire exactly as before. The flow (ContextAgent-style proactive scoring):

1. Sanitize all event text, build a scoring prompt with persona context and the current interruptibility score.
2. One utility LLM call returns `proactive_score ∈ 1..=5`.
3. Dynamic threshold `base + round(interruptibility × 2)` — the busier the user, the higher the bar.
4. Score ≥ threshold → allow; anything else — including LLM error, parse failure, or timeout — → **suppress** (fail-closed: never interrupt on uncertainty).

Default base threshold 3, default cap 4 proactive notifications per agent per rolling hour. Every decision writes one audit line to `<home>/proactive_gate.jsonl`.

## One-Click Templates

Authoring an OS rule used to require knowing event names and condition JSON by hand — so nobody did. The OS page now ships template cards (`OsAutomationTemplates.tsx`): a **file template** ("when something lands in this folder, act") that also appends the watch path via `os.settings.update`, and an **app template** ("when this app comes to the foreground, remind me on this channel"). Each fills a small form and creates a real autopilot rule through the normal `autopilot.create` RPC — server-validated and circuit-breaker protected like any hand-written rule.

## Opt-In Switches & Edition Quota

| Layer | Switch | Default |
|---|---|---|
| OS-native seat | `[capabilities] os_native = true` | off |
| Filesystem watch | `[os_watch] paths = [...]` | empty (nothing watched) |
| Frontmost polling | `[os_watch] frontmost_poll_secs` | 0 / absent (no polling) |
| Footprint memory | `[os_watch] footprint = true` | off |
| Proactive gate | `[proactive] enabled = true` | off |

Edition gating is a **quota lock, not a capability lock**: Personal edition allows exactly one OS-native agent; paid tiers are uncapped. No feature is removed or degraded on any edition — only the number of seats that may sense the OS at once.
