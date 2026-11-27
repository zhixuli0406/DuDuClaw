# Goal intent router

> When you tell an agent to do a real job in a chat channel, it now notices
> and offers to turn that into a tracked goal — but it never creates one
> behind your back. You confirm with explicit words or a button.

## What it is

Until now, only the dashboard could start an autonomous goal (the loop where
an agent works toward acceptance criteria and escalates to a human when it
gets stuck). In every chat channel, anything that wasn't an explicit `/goal`
command was treated as ordinary conversation. So "help me pull this quarter's
invoices into a report and email it out" — obviously a job, not a question —
got a chat reply and nothing was tracked.

The goal intent router closes that gap. It reads each incoming message,
decides whether it looks like task delegation, and if so replies with a small
menu: start a task, see a plan for your approval first, or keep chatting. Creating a goal spends tokens and starts a loop that
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
   it subtracts for question marks, small-talk markers, short follow-ups, and
   one-off writing requests such as drafting or replying to a letter, email
   or message. A clearly-low score stays chat. A high score never produces
   a suggestion on its own.
3. **Model confirmation.** Both high and intermediate scores ask a language model one
   yes/no question: is this delegating a job that takes several steps? With a
   local inference backend configured this runs on-device at zero cloud cost;
   without one it folds the question into the reply the agent was already
   going to generate, so it still costs nothing extra. If the local model is unavailable or its answer can't be parsed, the
   reply's own model makes the judgement. Without a valid suggestion from
   that reply, the conversation stays chat; your reply is never blocked.

## Confirming a suggestion

On Telegram, Discord, Slack, and LINE the suggestion comes with three buttons:
🎯 start task / 🧭 plan first / 💬 keep chatting. All channels also have a text
menu. Reply `start task` to create the task or `plan first` to see a plan
before work starts. Reply `keep chatting`, or simply continue the conversation,
to dismiss it. Menu and button language follows `[general] default_language`:
zh-TW uses 開始任務 / 先給計畫 / 繼續聊; Japanese uses タスク開始 / 先に計画 /
このまま会話. The words in all three languages are accepted regardless of the
configured language, including quoted words and trailing punctuation; English
is case-insensitive. Bare `1`, `2`, `3` remain legacy shortcuts, but are ignored
when the AI reply itself contains a numbered list, so a draft selection stays
an ordinary chat answer.

Choosing “start task” uses the same creation checks as typing `/goal` yourself.
Choosing “plan first” generates a plan and holds the task for your approval
before any work starts. A different reply, or ten minutes of silence, drops
the suggestion.

The creation reply explains which reply or button created the task, summarises
the job, and says the AI employee will work step by step and report here when
done or stuck. To cancel, open the dashboard's “Goal tasks” page and stop it
there. If automatic execution is off, the reply says the task was created but
will not start by itself and asks you to contact the administrator. Plan-first
replies also point to that page to approve, edit or stop the task. These replies
use plain language without task IDs, command syntax or configuration hints;
the typed `/goal` command keeps its existing reply.

Pressing a button, typing `/goal`, and using the dashboard all reach one
creation path, so the acceptance criteria are frozen at creation the same way
in every case.

## Configuration

The per-employee `enabled` switch is also available on the employee’s edit page in the dashboard under Automation → Task suggestions.

Per-agent in `agent.toml` (or global in `config.toml`) under `[goal_intent]`:

| Key | Default | Meaning |
|-----|---------|---------|
| `enabled` | `true` | Master switch. Safe to leave on because every hit is confirmation-gated. |
| `mode` | `"auto"` | `auto` (local model if available, else fold into the reply), `local`, `reply_tag`, or `off`. |
| `t_goal` | `65` | Threshold for the high-score grade; still requires model confirmation. |
| `t_gray` | `30` | Score below which a message stays chat; all higher scores require model confirmation. |
| `cooldown_minutes` | `30` | Minimum gap between suggestions in one conversation. |
| `daily_cap` | `20` | Per-agent suggestions per UTC day. |
| `suggest_ttl_minutes` | `10` | How long a pending suggestion waits for your reply. |

The router runs after the access gate and takeover interception, so it never
widens what a message is allowed to do — it only offers a shortcut to a path
you could already take by hand.
