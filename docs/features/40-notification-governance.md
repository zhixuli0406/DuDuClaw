# Notification governance

> Every push gets a severity level, quiet hours defer what can wait, one digest a day covers the rest, and action-rate metrics expose the notification types nobody responds to.

---

## The problem: one pipe for everything

DuDuClaw's AI employees reach out on their own: a task is stuck, the budget ran out, a high-risk action needs your sign-off, an evolution loop stalled, a channel cannot deliver. Each of these messages is reasonable on its own, but before v1.54 they shared one flaw — **no severity tiers**. A 3 AM "evolution event" and a 3 AM "task stuck, waiting on your decision" went out the same pipe, lit up the same screen, made the same sound.

Notification governance is the layer standing in front of every push outlet. It does four things: labels each notification with a level, defers the non-urgent ones during quiet hours, rolls up one digest per day, and measures whether anyone actually acts on each notification type.

---

## How it works

Every notification travels this path from creation to delivery (or deferral):

```
Notification created (NotifyLevel required, no default)
        |
        v
   Is it L3? --yes--> deliver immediately (quiet hours never block L3)
        |
        no
        |
        v
  Quiet hours active? --no--> deliver now
        |
       yes
        |
        v
  Append to ~/.duduclaw/notify_queue.jsonl (file-locked)
        |
        v
  When the window ends, a background scheduler delivers:
    - L1 items for the same destination merged into one message
    - L2 decision cards delivered individually
    - caps: 500 entries max, older than 36 hours dropped (warn log)
```

### The four-level escalation ladder

Every push point must declare its level (`NotifyLevel`) in code, and there is no default — adding a new push outlet forces you to answer "is this worth waking someone up for?"

| Level | Criterion | During quiet hours | DuDuClaw examples |
|---|---|---|---|
| **L0** | No state change | Never enters this layer | Heartbeats, uneventful patrols |
| **L1 FYI** | It happened, no human needed | Deferred, merged after the window ends | Evolution events, SOUL consolidation, skill-gap summaries, daily digest, budget recovery |
| **L2 Needs confirmation** | A human should know and click once | Deferred, delivered individually after the window ends | Dispatch approval (task not yet started), autopilot rule circuit-breaker pause |
| **L3 Act now** | Urgent, important, actionable, and real | **Delivered regardless** | Autonomous task waiting on your decision, high-risk approvals, install sign-off, budget stop, channel failure |

The criteria borrow Google SRE's three-way alert split (Page / Ticket / Report). The practical dividing line is what waiting until morning costs: a dispatch approval that waits until morning starts eight hours late; a task stuck mid-run waiting on your decision means the whole autonomous loop is stalled.

L3 is never affected by quiet hours, and that is deliberate — an urgent notification that a setting can switch off is not an urgent notification.

---

## Quiet hours

### Configuration

```toml
# ~/.duduclaw/agents/<agent>/agent.toml
[proactive]
quiet_hours = "22:00-08:00"   # optional; unset = disabled
timezone    = "Asia/Taipei"   # optional; host system timezone when unparseable
```

```toml
# ~/.duduclaw/config.toml — global fallback
[notify]
quiet_hours = "22:00-08:00"
```

The employee's own setting wins; the global value applies only when it is absent. Ranges that cross midnight (`22:00-08:00`) work as expected. The range is half-open on the right, so an end of `08:00` means delivery resumes at exactly 08:00.

### Parse failure always means "disabled"

A malformed value, an empty string, and `start == end` (for example `00:00-00:00`) are all treated as "no quiet hours", and a `warn` log records which value was ignored. The reasoning is plain: a parser bug must never leave a deployment unable to receive notifications all night. The cost of a misconfigured value is that notifications arrive as usual — never that everything goes silent without a trace.

### Legacy fields never take over

Under `[proactive]` there is an older pair, `quiet_hours_start` / `quiet_hours_end` (numeric hours), whose default is 23–8 — and **every employee has it**. The governance layer deliberately does not read it; otherwise every existing installation would suddenly be muted all night without anyone asking for it. That pair keeps its original, narrower job: scheduling when `[proactive]` proactive checks run.

### Deferred, never dropped silently

Blocked notifications are written to `~/.duduclaw/notify_queue.jsonl` (under a file lock) and delivered by a background scheduler once the window ends:

- **L1 notifications for the same destination merge into one message** (`🌙 勿擾時段收到 3 則通知：` followed by a numbered list) rather than three separate messages.
- **Decision cards are delivered individually**, because each card carries its own buttons.
- The queue has two hard caps: 500 entries at most, and anything older than 36 hours is dropped ("the task finished" from a day and a half ago is no longer news). **Every drop writes a `warn` log** — nothing disappears silently.

Known limitation: a decision resolved from the dashboard during quiet hours still delivers a stale card in the morning. Pressing it is rejected by that decision's storage layer as "already handled" (fail-closed), so the cost is one redundant card, never a duplicate execution.

### The dashboard must say so

The `proactive` block of the `agent.get` RPC returns two extra fields:

- `quiet_hours` — the active range string (`"22:00-08:00"`), `null` when no quiet hours apply
- `quiet_hours_note` — a ready-to-render zh-TW sentence spelling out what gets deferred and what still comes through

Whatever will be silenced must be laid out where the user can see it. That is a hard requirement, not a bonus.

---

## Daily digest

```toml
# ~/.duduclaw/config.toml
[notify]
daily_digest    = false     # off by default, must be enabled explicitly
daily_digest_at = "09:00"   # local time
```

One message per day, pushed to the `[proactive]` destination of `[general] default_agent`, summarizing the past 24 hours:

- tasks completed
- items waiting on you (approvals + install requests + tasks stuck in `needs_human`)
- learning events (`gvu_*` / `playbook_*` / `soul_*` / `evolution_*` activity-feed events)
- spend
- channel failure alert count
- notification types with a low action rate (see the next section)

### No news, no message

On a day where every number is zero, **nothing is sent** — there is no "nothing happened today" note. A digest you can skip every day trains you to skip it on the one day you shouldn't.

The one-per-day cap is hard: a state file records the last local date a digest went out, so a restart never sends a second one. But if the gateway is down at 09:00 and comes back at 11:00, that day's digest still goes out once.

---

## Action-rate measurement

One record is written every time a notification goes out, and another every time someone actually resolves a decision. Both land in `~/.duduclaw/notify_events.jsonl`.

```
duduclaw dashboard → notify.stats RPC
{
  "days": 30,
  "broken_threshold": 0.5,
  "min_sample": 10,
  "types": [
    { "type": "decision.install", "pushed": 12, "actionable": 12, "acted": 5, "action_rate": 0.42, "broken": true },
    { "type": "decision.goal",    "pushed": 12, "actionable": 12, "acted": 12, "action_rate": 1.0,  "broken": false }
  ]
}
```

`broken` applies Google SRE's rule directly: **an alert with accuracy below 50% is a broken alert**. The conditions: at least 10 actionable pushes, and an action rate below 50%.

Some deliberate choices:

- **Pressing the same card twice counts as one action** (matched by decision id rather than counted), so the action rate can never exceed 100%.
- **A rejected press is not an action** — pressed but blocked by permissions or state is a failed interaction, and no evidence that the notification was useful.
- Pure FYI types (nothing to press) report `actionable: 0` and are **never marked broken**. "The action rate of an FYI is 0%" is a tautology, not a finding.

The dashboard chart is implemented; its location is listed in the dashboard section below.

---

## Channel failure record schema

`channel_failures.jsonl` received two adjustments:

1. **The `channel` field is now widespread**: every write point that can derive the platform from the session id (`channel_reply_silent` / `channel_reply_fallback` / `runtime_fallback_substitution` / `trajectory_anomaly` / `foresight_alarm`) fills in `channel`, so the unified dashboard log can answer "which platform did this failure happen on". Writers that cannot derive a platform (internal sessions such as cron / bus / heartbeat, and the PTY fallback that only knows a working directory) write `null` or omit the field — attribution is never fabricated.

2. **Recovery events**: when a channel returns from an alerting state to normal, one record is written:
   ```json
   {"event":"channel_recovered","channel":"telegram","reason":"recovered","resolved":true,"resolves":"telegram_send_failed","timestamp":"…"}
   ```
   Old failure lines are **never rewritten** (append-only audit file); consumers decide whether a failure is still relevant by checking for a later `channel_recovered` on the same channel.

⚠️ A behavioral fix rides along: the channel-failure alert condition changed from "has a `channel` field" to "`event` is in the send-failure allowlist **and** has a `channel` field". Without that step, point 1 would turn every LLM timeout and every trajectory anomaly into a channel-outage alert. The allowlist currently holds only `telegram_send_failed`; to add an entry, the event must genuinely mean "this channel cannot deliver messages".

---

## Dashboard entry points

All of these settings and readouts have RPCs, but the entry points are spread across pages:

| Setting / readout | Location | Notes |
|---|---|---|
| Quiet hours (the employee's own `quiet_hours`, full context) | AI employees list → pick an employee → edit (`/agents/:id/edit`) → the「自動化」(automation) tab | The advanced block has an `HH:MM-HH:MM` input that flags format errors inline; below it, the sentence currently in effect for this employee (`quiet_hours_note`) renders live. |
| Quiet hours (quick switching across employees) | Settings → the「主動行為」(proactive behavior) tab | The same `[proactive] quiet_hours`, with an employee dropdown at the top of the page. If the new-format field is empty but the legacy numeric `quiet_hours_start`/`quiet_hours_end` were changed away from their defaults (meaning the operator set quiet hours through this page in the past), a conversion prompt appears and pre-fills the new format; it only takes effect after saving — avoiding the silent trap of "I thought I changed quiet hours, but the governance layer never saw it" (W2-9). |
| Daily digest switch (`[notify] daily_digest` / `daily_digest_at`) | Settings → the「系統」(system) tab | Switch + time field; one global digest, never per employee. |
| Notification effectiveness (`notify.stats` action rates) | Reports page → the「通知成效」(notification effectiveness) card | Lists pushes / action rate per notification type against the SRE 50% rule from the action-rate section; low-action-rate types get a red bar plus a text hint (never color alone, for color-blind readers and black-and-white printing); pure FYI types (nothing to press) are never marked broken. |

The same `[proactive] quiet_hours` has two editing entry points: the「自動化」tab of the employee edit form is the full context (next to notification destinations and check-in scheduling); the「主動行為」tab in Settings is the shortcut for operators who want to view or switch several employees' quiet hours without opening each employee's details. Both write the same `agent.toml` field; a save in one place shows up in the other immediately.

---

## Related

- Design basis: `commercial/docs/ux-redesign-2026-08/02-ux-methodology.md` theme 4 (P4-1, P4-4, P4-5, P4-6) and `03-analogous-products.md` C1 / C7 / C8 / C12
- Code: `crates/duduclaw-gateway/src/notify_governance.rs`, `notify_stats.rs`, `notify_digest.rs`
- Frontend: `web/src/pages/agent-form/EditAgentPage.tsx` (automation tab), `web/src/components/settings/sections/ProactiveTab.tsx`, `SystemTab.tsx`, `web/src/pages/ReportPage.tsx`
- Related features: [34-goal-loop.md](34-goal-loop.md) (needs_human decision cards), [23-autopilot-engine.md](23-autopilot-engine.md) (rule circuit-breaker notifications)
