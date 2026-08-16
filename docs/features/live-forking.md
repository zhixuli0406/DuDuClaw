# Live forking: when to use it

> Usage scenarios for live forking: when to fork, when not to, and how it differs from `duduclaw eval`. For the mechanism and the maze metaphor, read the technical deep-dive [28-live-forking.md](./28-live-forking.md); the full design is in [RFC-26](../rfc/RFC-26-deep-agents-alignment.md).

Live forking lets a running task split on the spot into N competing branches. Each branch runs with a different account, an isolated workspace copy, and its own budget; when they finish, an AI judge (or you) picks the best one and merges it back. It is off by default; enable it per agent with `agent.toml [fork] enabled = true`.

The one-line difference: normally a task walks one path and starts over when it goes wrong; with fork on, one task walks several paths at once and the results compete directly.

---

## What a branch actually is

A DuDuClaw agent runs as a `claude` CLI subprocess, so a "branch" is one isolated subprocess execution carrying:

- its own **workspace overlay** (copy-on-write: it reads the parent workspace, but writes land only in its own copy, so branches never contaminate each other)
- its own **account** (the AccountRotator assigns distinct accounts, so branches never collide on the same rate limit)
- its own **budget cap** and an optional **steering message** (telling the branch which direction to try)
- a **read-only shared view** of the parent workspace

After finishing, each branch can optionally run a `test_command` (for example `pytest -q`), and its exit code feeds the judge's scoring.

---

## Three scenarios where forking pays off

### Scenario 1: several candidate approaches, pick the winner

A problem has several reasonable solutions and you are not sure which is best. Instead of trying them one at a time and backtracking on failure, open three branches at once, give each a different strategy, and compare the results.

Typical case: "this slow query — one branch tries adding an index, one rewrites the JOIN, one adds a cache; whichever measures fastest wins." Each branch gets a different steering message, the judge scores by test pass rate and quality, and `merge_mode = "auto_with_fallback"` auto-picks the winner while keeping a confirmation gate.

### Scenario 2: high-risk change, rehearse it in a branch first

The thing you want to touch would be painful to get wrong (a large refactor, core config changes, a destructive migration), and you want to see exactly what it would become before deciding whether to land it.

The branch's copy-on-write overlay is a natural fit: every write a branch makes lands in its own copy, and the parent workspace does not change by a single character before merge. If the attempt blows up, discard that overlay and the parent stays intact; only when it works do the winner's writes get merged back. This is cleaner than "commit first, revert later", because the failed attempt never touched the mainline.

### Scenario 3: AI-judged review

You want "pick the best of several results, and be able to say why", where "producing a result" alone is not enough.

The judge emits a `JudgeVerdict`, with confidence computed by the RFC-26 formula: `quality_spread·0.4 + test_pass_ratio·0.4 + internal_consistency·0.2`. This is exactly the scenario meeting §15 asked about: one agent produces several results, then another agent is dispatched as the reviewer. Fail-closed is deliberate: if the judge is missing, the verdict cannot be parsed, or the sandbox fails to start, everything falls back to `manual` (all branches are laid out for you to see); it never silently picks one at random.

---

## When not to use it

Fork is not something to leave on by default. Skip it when:

- **The task is deterministic**: there is only one answer and no trade-off space (format conversion, a plain lookup). Opening branches just burns N times the cost for nothing.
- **Cost is tight**: N branches = N accounts, N budgets. When subscription quota or API spend is under pressure, fork amplifies consumption. `[fork] max_branches` and `aggregate_budget_usd` are hard caps, but the underlying question is whether this task deserves parallelism at all.
- **There is nothing to score against**: with an empty `test_command`, the `test_pass_ratio` in the judge formula is neutralized, scoring falls back to quality and consistency alone, and objective selection loses ground. For tasks with no runnable tests, fork's value is mostly scenario 2 (isolated trial and error), not scenario 1 (objective selection).
- **You need to force-kill a branch across processes**: `terminate_branch` can only stop subprocesses in its own process. Killing a branch subprocess across a different process is a by-design exclusion listed in RFC-26.

---

## How it relates to `duduclaw eval`

Both use an "AI judge", but for opposite purposes; do not confuse them:

| | Live forking | `duduclaw eval` |
|---|---|---|
| When | At run time, while the task is in flight | After the fact / CI, offline regression |
| Goal | Pick the best of several paths on the spot and continue with it | Verify against fixed golden tasks that the agent has not regressed |
| Input | One live task + N strategies | Pre-recorded cases in `evals/<suite>/<case>.toml` |
| Output | Winner branch merged into the workspace | JSON report + non-zero exit to block a PR |

In implementation, `duduclaw eval`'s LLM judge reuses the `duduclaw-fork` judge primitives, so the review logic is the same set. One way to remember it: **fork scouts ahead, eval checks the work afterwards**. For eval usage, see the [evals guide](../guides/evals.md).

---

## Configuration and tools

`agent.toml`:

```toml
[fork]
enabled = false              # off by default; enable per agent
max_branches = 4             # hard cap on branch count (avoid account/quota blowout)
default_budget_usd = 0.50    # per-branch cap
aggregate_budget_usd = 1.50  # cap across all branches
merge_mode = "auto_with_fallback"
test_command = ""            # optional; empty ⇒ judge's test_pass_ratio neutralized
test_timeout_s = 120
```

MCP tools (registered only when the agent sets `[fork] enabled = true`, uniformly gated behind `Scope::ForkExecute`, fail-closed):

| Tool | What it does |
|---|---|
| `fork_run` | Split the current task into N branches |
| `inspect_branches` | List live branches + state + spend |
| `diff_branches` | File/output diff between two branches |
| `merge_or_select` | Resolve a fork (judge verdict or your pick) |
| `terminate_branch` | Kill a runaway branch |
| `fork_cost` | Aggregate and per-branch spend |

Every fork resolution is written to `~/.duduclaw/fork_history.jsonl` and the Activity Feed, and the dashboard has a ForkPage for visualization.

---

## One-line summary

Live forking suits tasks with trade-offs, risk, and something to compare. It does not suit tasks that are deterministic, cost-sensitive, or unscoreable. Before turning it on, ask one question: for this task, is the quality gained from walking several paths worth several times the spend.
