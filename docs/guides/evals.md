# Agent Behavior Evals (`duduclaw eval`)

Golden-task **behavioral regression** for agents. Each case sends one prompt to
an agent through the **same CLI harness invocation the gateway uses** (stream‑json
output, `[capabilities]` tool allow/deny wiring, per‑agent `.mcp.json`,
`--max-turns` budget), parses the resulting transcript, and checks it against
deterministic assertions plus an optional LLM‑judge rubric.

This is the ADK‑evalset / Braintrust eval‑action pattern adapted to DuDuClaw:
one TOML file per case, an exit code CI can gate on, and an offline replay mode
so regressions are catchable without spending tokens.

> **Why this matters for a self‑evolving platform.** DuDuClaw's GVU loop rewrites
> `SOUL.md` and validates its own changes with its own Verifier. That Verifier is
> *inside* the loop — it can drift together with the thing it is grading. Evals are
> the **external yardstick**: a fixed, human‑authored set of expected behaviors that
> a prompt change, a runtime/provider swap, a `claude` CLI upgrade, or a GVU
> `SOUL.md` rewrite **cannot silently regress**. See
> [GVU yardstick](#gvu-integration-the-external-yardstick) below.

---

## Quick start

```bash
# Offline — no agent, no credentials needed (deterministic regression):
duduclaw eval evals/examples/greeting-replay.toml --replay
duduclaw eval evals/examples/grounded-replay.toml --replay

# Live — run a real agent and record a baseline transcript for later replay:
duduclaw eval evals/examples/refund-flow.toml --record

# Run a whole suite (recursive, sorted), write a machine-readable report:
duduclaw eval evals/support --report eval-report.json
```

`PATH` may be a single `*.toml` case file **or** a suite directory (searched
recursively, run in sorted order). It defaults to `./evals`.

### Flags

| Flag | Meaning |
|------|---------|
| `--filter <substr>` | Only run cases whose `[case] name` contains `<substr>`. |
| `--replay` | Parse recorded `*.transcript.jsonl` files instead of running the agent live (offline, zero credentials). Mutually exclusive with `--record`. |
| `--record` | Live‑run, then write the raw stream‑json next to each case as a `*.transcript.jsonl` baseline for future `--replay`. |
| `--no-judge` | Skip the `[judge]` rubric even when a case enables it (fully deterministic, zero‑cost). |
| `--report <path>` | Write a JSON report (per‑case assertions, judge score/rationale, transcript diagnostics, durations). |

**Exit code:** the process exits **non‑zero when any case fails**, so it drops
straight into a CI gate. A human‑readable table is printed to the console; the
`--report` file is the machine‑readable counterpart.

---

## Case format

One TOML file per case:

```toml
[case]
name   = "refund-flow"          # [a-zA-Z0-9_-], ≤64 chars; shown in reports
agent  = "support-bot"          # agent id under ~/.duduclaw/agents/<agent>
prompt = "A customer asks for a refund on order #1234. Handle it."
# system_prompt = "..."         # optional: passed via --system-prompt-file
# model         = "claude-haiku-4-5"   # default: claude-sonnet-4-6
# timeout_secs  = 180           # live-run wall clock (1..=3600)
# max_turns     = 25            # CLI --max-turns (1..=100)
# transcript    = "custom.jsonl" # replay file, relative to this case file;
                                #   default: <case-file-stem>.transcript.jsonl

[expect]                        # all fields optional; each *configured* field
                                # produces exactly one assertion in the report
must_use_tools     = ["tasks_create"]  # must be invoked ≥ once
must_not_use_tools = ["Bash"]          # must never be invoked
output_contains     = ["1234"]         # case-sensitive substring of final answer
output_not_contains = ["sk-ant-"]      # must be absent from final answer
output_regex        = "(?i)refund"     # Rust regex the final answer must match
min_text_blocks     = 1                # ≥ N assistant text blocks
max_tool_calls      = 10               # ≤ N tool_use blocks (budget guard)

# Zero or more trace-grounding assertions — see "Trace grounding" below.
[[expect.grounded]]
tool               = "memory_search"   # must be called ≥1 time without erroring
min_overlap_chars  = 12                # default 12; CJK-safe char count
# output_regex     = "30 days"         # optional, see below

[judge]                         # optional LLM rubric (Braintrust scorer style)
enabled   = true                # default true when the [judge] section exists
rubric    = "Politely acknowledges the refund and cites the order number."
min_score = 0.7                 # pass when score >= min_score (0.0..=1.0)
```

Rules enforced at load time (fail‑fast, so a typo never half‑runs a suite):

- A case **must** define at least one `[expect]` assertion **or** an enabled
  `[judge]`. A case with no checks is rejected.
- **Unknown fields are rejected** — `tool_calls_includ` (typo) fails loudly
  instead of silently passing.
- `output_regex` must compile; `min_score` must be `0.0..=1.0`; `timeout_secs`
  and `max_turns` are range‑checked; a `transcript` path may not be absolute or
  contain `..` (a case file can't be tricked into reading arbitrary files).
- A malformed case is reported as a **FAILED case with a reason** — never
  skipped. A corrupt suite can't sneak a green CI run.

### Tool‑name matching

`must_use_tools` / `must_not_use_tools` match the tool name **exactly** or by its
final `__`‑delimited segment — token‑anchored, never a raw substring. So
`tasks_create` matches `mcp__duduclaw__tasks_create`, but `create` does **not**
match `tasks_create`. (This mirrors the project's "no unanchored `contains` for
routing decisions" convention.)

### What "output" means

Assertions run against the **final answer text** parsed from the stream‑json
transcript (a non‑empty `result` event wins; otherwise the last assistant text
block) — the same precedence the gateway's own stream parser uses. Tool
assertions run against the ordered list of `tool_use` blocks. Regex and
substring checks are UTF‑8/CJK‑safe (Rust `regex`, no byte slicing).

---

## Trace grounding (`[[expect.grounded]]`, GroundEval)

A worker can produce a fluent, on-topic final answer that simply **fabricates**
the underlying fact — "checked the refund policy: 30 days" without ever
calling `memory_search`, or calling it and then citing a number the tool never
returned. `must_use_tools` only checks that a tool was *invoked*; it says
nothing about whether the final answer actually reflects what the tool
returned. `[[expect.grounded]]` closes that gap (GroundEval, arXiv:2606.22737):

```toml
[[expect.grounded]]
tool              = "memory_search"  # matched like must_use_tools (exact or
                                      # final `__`-segment)
min_overlap_chars = 12               # default 12
output_regex      = "30 days"        # optional
```

A grounded assertion passes only when **all** of the following hold:

1. `tool` was called at least once **without** `is_error` on its `tool_result`.
2. The final answer shares a **contiguous run of ≥ `min_overlap_chars` chars**
   with at least one of that tool's result texts (CJK-safe: counted in
   `char`s, not bytes — a 12-char Chinese passage is 12, not 36).
3. If `output_regex` is set, the substring it matches in the final answer must
   itself appear verbatim in one of the tool's result texts — a regex match
   on the *answer* alone is not enough if the cited fact was never in the
   evidence.

This needs a transcript with `tool_result` capture (added alongside this
feature). A transcript recorded before `tool_result` capture existed — or
loaded via a case whose `tool_calls.jsonl`-equivalent result stream got
dropped — fails the assertion **closed**, with a detail telling you to
`--record` a fresh transcript, rather than silently passing on missing
evidence.

### Where this evidence also shows up: goal-mode acceptance

The same tool-call evidence feeds the **goal-mode acceptance judge**
(`DispatchEngine::review_goal_tasks`, WP4): before scoring a `review` task,
the judge reads `tool_calls.jsonl` for that task's claim→review window and
attaches a compact `<tool_activity>` block (`tool: N ok, M err`, per tool,
capped at 20 lines) to the acceptance prompt. The `correctness` aspect is
instructed to treat any action the worker *claims* but that never shows up in
`<tool_activity>` as unverified. This is best-effort: a missing/unreadable
audit file simply omits the block — the review is never blocked on an
observability gap.

---

## Live vs. replay

| Mode | Command | Needs | Use for |
|------|---------|-------|---------|
| **Live** | `duduclaw eval evals/support` | provisioned agent + ambient `claude` credentials | authoring cases, pre‑release behavior checks |
| **Live + record** | `duduclaw eval evals/support --record` | same | (re)creating regression baselines (`*.transcript.jsonl`) |
| **Replay** | `duduclaw eval evals/support --replay` | nothing (offline) | the CI regression gate on the deterministic assertions |

- Live runs execute **inside the agent directory** with the agent's
  `[capabilities]` allow/deny tool lists applied and, if present, its per‑agent
  `.mcp.json` (`--strict-mcp-config`). They use whoever runs the command's
  `claude` login — no multi‑account rotation; evals are an operator/CI tool, not
  a channel path.
- Cases are intentionally **single‑shot and session‑free** (no `--resume`) for
  reproducibility.
- The `[judge]` rubric also runs in **replay** (it scores the recorded final
  answer). Add `--no-judge` for a fully deterministic, zero‑cost run.

Typical workflow: author a case, run `--record` once to capture a known‑good
transcript, commit the `*.transcript.jsonl`, then let CI run `--replay` on every
PR. Refresh the baseline with `--record` when you *intend* the behavior to change.

---

## CI example (GitHub Actions)

Replay mode needs no credentials, so it fits a standard PR gate. The non‑zero
exit code fails the job automatically.

```yaml
name: agent-evals
on: [pull_request]

jobs:
  evals:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build duduclaw
        run: cargo build -p duduclaw-cli --release
      - name: Run behavioral evals (offline replay)
        run: |
          ./target/release/duduclaw eval evals \
            --replay --no-judge \
            --report eval-report.json
      - name: Upload eval report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: eval-report
          path: eval-report.json
```

Drop `--no-judge` (and provide `CLAUDE_CODE_OAUTH_TOKEN` / an API key) if you
want the rubric judge to run in CI too. For a nightly **live** behavior check,
run the same command without `--replay` on a self‑hosted runner that has a
provisioned agent + `claude` login.

---

## GVU integration: the external yardstick

The evolution engine's GVU loop (Generator → Verifier → Updater) proposes
`SOUL.md` rewrites and gates them with an *internal* 4‑layer verifier, then holds
a **24‑hour observation window** before confirming or auto‑rolling‑back a version
(`ObservationFinalizer` / `duduclaw evolution finalize`, which computes
post‑metrics from `prediction.db` + `feedback.jsonl`).

Evals are the **independent** counterpart to that internal verifier:

- The internal Verifier grades a proposal against the model's *own* judgment. It
  can co‑drift with the behavior it grades.
- An eval suite grades the *running agent* against **human‑authored expected
  behaviors** that don't move when `SOUL.md` does. If a GVU rewrite quietly drops
  the "always cite the refund policy page" behavior, a `must_use_tools` /
  `output_regex` case turns red — even though GVU's Verifier approved the change.

### Design hook (not wired this pass)

> **TODO (design‑only).** Feed the eval suite's pass‑rate into the `SOUL.md`
> 24‑hour observation‑window **post‑metrics** so a version that regresses a golden
> behavior is auto‑rolled‑back, not just confirmed on prediction‑error metrics.
> Concretely: after a GVU version is applied, run
> `duduclaw eval evals/<agent> --replay --report <tmp>.json`; treat the
> `failed`/`total` ratio as an additional negative signal alongside the
> `prediction.db` post‑metrics the `ObservationFinalizer` already reads. A drop in
> eval pass‑rate across the observation window becomes a rollback trigger. This
> wave ships the yardstick and the CLI; the finalizer wiring is intentionally left
> for a follow‑up so the eval runner can stabilize as a standalone gate first.

---

## Where things live

```
evals/                              # your eval suites (repo-relative)
├── examples/
│   ├── greeting-replay.toml        #   offline replay sample
│   ├── greeting-replay.transcript.jsonl
│   ├── grounded-replay.toml        #   offline replay sample ([[expect.grounded]])
│   ├── grounded-replay.transcript.jsonl
│   └── refund-flow.toml            #   live sample (needs an agent)
└── <suite>/
    ├── <case>.toml
    └── <case>.transcript.jsonl     #   recorded baseline (via --record)
```

The implementation lives in `crates/duduclaw-cli/src/eval/`:
`case.rs` (format + validation), `transcript.rs` (stream‑json parsing),
`assertions.rs` (deterministic checks), `judge.rs` (LLM rubric, reusing the
RFC‑26 fork‑judge `LlmCaller` plumbing), `runner.rs` (live spawn + replay), and
`mod.rs` (orchestration + reporting).
