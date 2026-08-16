# Human takeover

> An admin who types directly in a channel silences the AI for that one conversation until they hand it back — no button to press, no mode to switch, no global settings touched.

---

## Why speaking, not a button

Support tools model human handoff in three ways. Intercom treats the AI as a replaceable assignee: a human sending one message transfers control. ManyChat treats automation as a track you can mute: a human starting to type pauses it for 30 minutes. LINE Official Account opens a per-conversation "temporary manual chat" escape hatch — one hour by default, extendable, endable early, and explicitly guaranteed not to touch global settings.

All three reach the same conclusion: **a support agent should not have to press a button before taking over.** A person jumps into a conversation because something went wrong, and at that moment one extra step is one extra thing to forget. DuDuClaw takes the same path — the moment an admin speaks, the takeover holds.

---

## How it works

### Trigger conditions (strict on purpose)

Only a message from someone meeting **both** of these conditions triggers a takeover:

1. The channel account has completed **identity binding** in the dashboard, and the binding is **verified**;
2. The bound dashboard account is an **active admin or supervisor**.

Nobody else triggers it: regular employee accounts, bindings whose channel account was filled in but never verified, unbound stranger accounts, or the same id on a different messenger.

Deployments where nobody has bound a channel account yet (say, a one-person company right after install) get **no automatic takeover**. That is deliberate: in that situation the only available identity proof is "the message came from the configured destination", which inside a channel means "you" — applying it would mean the owner's first message silences the AI forever. To pause the AI in such a deployment, use the `!STOP` safe word, or complete one channel binding in the dashboard first.

Typing `/takeover` in the channel tells you which of these situations you are in.

### What happens during a takeover

**The AI goes fully silent for this conversation.** It does not answer, and it does not post "I am paused" notices — a human is handling things, and a bot popping up to report its own status is interference.

**Messages are still recorded.** Every message during the takeover (the admin's, the customer's) is written to the conversation log, so when the AI resumes it sees what happened in between and does not continue on a broken thread of context.

**Scheduled actions are held too.** This is the easiest part to get wrong: blocking only *new replies* is not enough, because work already in flight finishes later and pushes its result in — the human wraps up, then the bot's stale messages trickle out (a trap ManyChat users have actually hit). DuDuClaw checks every dispatch path aimed at this conversation:

| Path | Behavior during takeover |
|---|---|
| Inbound channel messages | No reply (the message is still written to the conversation log) |
| Autonomous task dispatch | Frozen — no dispatch, no escalation to a human (the person to find is already on scene) |
| Autonomous task progress pushes | Held, merged and delivered after handback |
| Autonomous task "your decision needed" cards | Held, delivered after handback (buttons fully preserved) |
| Autonomous task completion notices (fully automatic mode) | Held, merged and delivered after handback |
| Regular proactive pushes (FYI / needs-confirmation tiers) | Held, merged and delivered after handback |
| Autopilot rule circuit-breaker notices | Held, delivered after handback (buttons fully preserved) |
| Task-board wake-ups | Skipped; wake-ups resume normally after handback |
| Proactive care messages | Dropped (an hour-late "you have been working for two hours straight" is a wrong message, not a late one) |
| Scheduled (cron) job results | Dropped (the next run replaces it; the run log is kept) |
| Delegation reports (a sub-employee finishing and reporting back) | Dropped (the result stays in the queue, readable from the dashboard) |

Every skip is written to the gateway log, so "why didn't the AI say anything" always has an answer — never a silent black hole.

**Three notification types pass through, deliberately**: high-risk action approvals, install-request sign-offs, and channel failure alerts. All three are action-required tier (urgent, important, and genuinely needing your hands), and none of them concern the conversation you are handling. Notification governance already exempts this tier from quiet hours, for the same reason: suppressing "this action may be irreversible — approve?" to save one interruption buys real risk.

**A takeover is conversation-level, not account-level.** The same AI employee keeps replying in other groups and other DMs; threads within the same group are independent of each other. One deliberate widening: taking over a thread also blocks pushes to its parent channel — one extra quiet message beats interrupting while the human is talking.

### Lifecycle

| Action | How |
|---|---|
| Start | An admin speaks in the conversation (default 60 minutes) |
| Extend | `/takeover +30m` (also accepts `+30`, `30m`, `45min`) |
| Status | `/takeover` — who holds the takeover, minutes remaining |
| End early | `/takeover end` (also accepts `結束`) |
| Auto handback | Timer expires; the AI resumes on its own |

Every message the admin sends restarts the 60-minute clock from that moment — a conversation being handled is never snatched back mid-sentence. Conversely, 60 minutes after the admin leaves the conversation, handback is automatic, so nobody has to remember to switch the AI back on.

**Slack exception**: Slack intercepts unregistered slash commands client-side, so `/takeover` never reaches us at all. On Slack use `/duduclaw takeover`, `/duduclaw takeover +30m`, `/duduclaw takeover end`. Every other channel accepts both spellings.

Extensions have a ceiling: `max_duration_minutes` (default 12 hours, hard cap also 12 hours). "Pause forever" is not this feature's job — that is disabling the AI employee.

### Three things happen at once

The moment a takeover holds is one action, not three:

1. **Pause** AI replies for this conversation;
2. **Claim the work**: unfinished autonomous tasks that came from this conversation are marked as handled by you (the board stops showing them as "AI working on it");
3. **Log to the activity feed**: who took over which conversation, and how many pieces of work came along.

Drop any one of the three and a black hole remains — most commonly pausing without marking, so the board still shows the AI running the task and the next person has no idea someone is already on it.

The order is deliberate: pause first (the only safety-relevant step — if it fails, the whole takeover fails and nothing is announced), then mark the work, last log the activity. Failures in the latter two only write warnings and never undo a pause already in effect.

---

## What the conversation shows

When a takeover starts, one message appears in the conversation:

```
👤 王小明 已接手對話，接下來由真人回覆（約 60 分鐘）。
```

("王小明 has taken over this conversation; a human will reply from here — about 60 minutes.")

On handback:

```
🤖 AI 已恢復回應。
```

("The AI has resumed responding.")

**This layer has to be built in-house.** Intercom is the only platform with complete built-in identity disclosure (compliance-driven); ManyChat's own FAQ states it does not notify contacts that automation is paused; LINE's architecture has no such layer at all. So DuDuClaw sends these two messages itself.

The displayed name comes from the dashboard display name. When none is set, it shows 「管理員」 ("administrator") — the channel account id is **never** exposed.

---

## Configuration

`~/.duduclaw/config.toml`:

```toml
[takeover]
enabled = true            # on by default; set false to turn the feature off entirely
duration_minutes = 60     # length of one takeover
max_duration_minutes = 720  # extension ceiling (hard cap 12 hours)
```

Bad values (0, negative, astronomical) are clamped into a sane range rather than breaking the feature — a typo in a config file should not cost you "the AI never speaks again" or "the pause ends after one second".

State lives in `~/.duduclaw/takeover_state.json`, the only file ever written. When nobody is taking over, the file is empty (or absent). **No global settings, no `agent.toml`, no channel configuration are touched** — the most important line in LINE's four-part design, and the reason support agents dare to use this feature at all.

---

## Dashboard

The `takeover.list` RPC (supervisor and above) returns the conversations currently taken over: who holds each one, on which channel, minutes remaining, and which pieces of work were claimed along the way.

**Read-only, no writes.** A takeover *is* the fact of a person being in that conversation; turning it into a dashboard button would create a second authorization model and let someone "take over" a conversation they are not even present in. To take over, go speak in the conversation.

---

## Known boundaries

- **Slash commands do not count as speaking.** An admin typing `/status` does not trigger a takeover — a command is an operation, not conversation.
- **One holder at a time.** A second admin speaking in the same conversation transfers the takeover to them (whoever spoke last is the person the customer is actually talking to).
- **The handback notice needs a bot token.** A channel with no resolvable token does not receive the "AI has resumed responding" message; the activity feed records the handback regardless.
- **Expiry does not depend on a background job.** Every checkpoint compares against the current time, so the AI resumes on schedule even if the sweep task never runs; the background sweep only sends the handback notice.

---

## Related documents

- [34-goal-loop.md](34-goal-loop.md) — the autonomous task loop (a takeover freezes it)
- [40-notification-governance.md](40-notification-governance.md) — notification tiers and the deferred-delivery queue (takeover reuses the same queue)
- [37-delegation-isolation.md](37-delegation-isolation.md) — who may command whom
