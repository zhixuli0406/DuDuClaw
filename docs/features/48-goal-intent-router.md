# Goal intent router

> When you tell an agent to do a real job in a chat channel, it now notices
> and offers to turn that into a tracked goal — but it never creates one
> behind your back. You always press the button.

## What it is

Until now, only the dashboard could start an autonomous goal (the loop where
an agent works toward acceptance criteria and escalates to a human when it
gets stuck). In every chat channel, anything that wasn't an explicit `/goal`
command was treated as ordinary conversation. So "help me pull this quarter's
invoices into a report and email it out" — obviously a job, not a question —
got a chat reply and nothing was tracked.

The goal intent router closes that gap. It reads each incoming message,
decides whether it looks like task delegation, and if so replies with a small
menu: turn this into a goal, think first (generate a plan for your approval),
or just keep chatting. Creating a goal spends tokens and starts a loop that
runs on its own, so the router never does it automatically — a hit only ever
produces a suggestion you confirm. That confirmation step is what keeps a
misread cheap: the worst case is one extra message you dismiss.

## How it decides

Three layers, cheapest first — the same shape as the knowledge router:

1. **Hard exclusions (zero cost).** Commands, very short messages, questions,
   injection-scan hits, and messages that are mostly a quoted reply are passed
   straight through to normal chat.
2. **Signal score (no LLM).** A deterministic table adds points for
   delegation verbs, deliverable nouns, multi-step phrasing, and deadlines;
   it subtracts for question marks, small-talk markers, and short follow-ups.
   A high score suggests a goal outright; a clearly-low score stays chat.
3. **Grey band (the small model).** Scores in between ask a language model one
   yes/no question: is this delegating a job that takes several steps? With a
   local inference backend configured this runs on-device at zero cloud cost;
   without one it folds the question into the reply the agent was already
   going to generate, so it still costs nothing extra. Either way, if the
   model is unavailable or its answer can't be parsed, the router fails open
   to chat and never blocks your reply.

## Confirming a suggestion

On Telegram, Discord, Slack, and LINE the suggestion comes with three buttons
(建立目標 / 想一想 / 只是聊聊). On the other channels it's a text menu — reply
`1`, `2`, or `3`. Choosing "建立目標" runs the exact same path as typing
`/goal` yourself, so every access check and approval gate still applies.
Choosing "想一想" generates a plan and parks the goal for your approval before
any work starts. Anything else — a different reply, or ten minutes of silence
— quietly drops the suggestion.

Pressing a button, typing `/goal`, and using the dashboard all reach one
creation path, so the acceptance criteria are frozen at creation the same way
in every case.

## Configuration

Per-agent in `agent.toml` (or global in `config.toml`) under `[goal_intent]`:

| Key | Default | Meaning |
|-----|---------|---------|
| `enabled` | `true` | Master switch. Safe to leave on because every hit is confirmation-gated. |
| `mode` | `"auto"` | `auto` (local model if available, else fold into the reply), `local`, `reply_tag`, or `off`. |
| `t_goal` | `65` | Score at or above which a message suggests a goal outright. |
| `t_gray` | `30` | Score below which a message stays chat; between the two goes to the grey band. |
| `cooldown_minutes` | `30` | Minimum gap between suggestions in one conversation. |
| `daily_cap` | `20` | Per-agent suggestions per UTC day. |
| `suggest_ttl_minutes` | `10` | How long a pending suggestion waits for your reply. |

The router runs after the access gate and takeover interception, so it never
widens what a message is allowed to do — it only offers a shortcut to a path
you could already take by hand.
