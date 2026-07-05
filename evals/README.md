# Agent Behavior Evals

Harness-level eval / regression suite for DuDuClaw agents, run with:

```bash
duduclaw eval [PATH] [--filter NAME] [--replay | --record] [--no-judge] [--report out.json]
```

Each case sends one prompt to an agent through the **same CLI harness
invocation the gateway uses** (stream-json output, capability
allow/deny tool wiring, per-agent `.mcp.json`, `--max-turns` budget), parses
the transcript, and checks:

1. **Deterministic `[expect]` assertions** — which tools were used, what the
   final answer says. Zero LLM cost; replayable offline.
2. **Optional `[judge]` LLM rubric** — a 0..1 score from an LLM judge (routed
   through the operator's configured utility runtime). Unparseable judge
   output fails the case (fail closed).

The process exits non-zero when any case fails, so CI can gate on it
(Braintrust eval-action pattern).

## Layout

```
evals/
├── README.md
├── examples/                      # runnable samples
│   ├── refund-flow.toml           # live case (needs an agent + credentials)
│   ├── greeting-replay.toml       # offline replay case
│   └── greeting-replay.transcript.jsonl
└── <suite>/                       # your suites: one .toml per case
    ├── <case>.toml
    └── <case>.transcript.jsonl    # recorded baseline (via --record)
```

`PATH` may be a suite directory (recursive, sorted) or a single case file.
Default is `./evals`.

## Case format

```toml
[case]
name = "refund-flow"          # [a-zA-Z0-9_-], shown in reports
agent = "support-bot"         # agent id under ~/.duduclaw/agents/<agent>
prompt = "A customer asks for a refund on order #1234."
# system_prompt = "..."       # optional --system-prompt-file override
# model = "claude-haiku-4-5"  # default: claude-sonnet-4-6
# timeout_secs = 180          # live-run wall clock (1..=3600)
# max_turns = 25              # CLI --max-turns (1..=100)
# transcript = "custom.jsonl" # replay file, relative to this case file;
                              # default: <case-file-stem>.transcript.jsonl

[expect]                      # all fields optional; each configured field
                              # becomes one assertion in the report
must_use_tools = ["tasks_create"]     # matched exactly OR by final `__`
must_not_use_tools = ["Bash"]         # segment (mcp__duduclaw__tasks_create)
output_contains = ["1234"]            # case-sensitive substring
output_not_contains = ["sk-ant-"]
output_regex = "(?i)refund"           # Rust regex over the final answer
min_text_blocks = 1
max_tool_calls = 10

[judge]                       # optional LLM rubric
enabled = true                # default true when the section exists
rubric = "Politely acknowledges the refund and cites the order number."
min_score = 0.7               # pass when score >= min_score (default 0.7)
```

A case must define at least one `[expect]` assertion or an enabled `[judge]`.
Unknown fields are rejected (typos fail fast instead of silently passing).

## Live vs. replay

| Mode | Command | Needs | Use for |
|------|---------|-------|---------|
| Live | `duduclaw eval evals/support` | provisioned agent + ambient `claude` credentials | authoring cases, pre-release behavior checks |
| Live + record | `duduclaw eval evals/support --record` | same | refreshing regression baselines (`*.transcript.jsonl`) |
| Replay | `duduclaw eval evals/support --replay` | nothing (offline) | CI regression gate on the deterministic assertions |

Notes:

- Live runs execute inside the agent directory with the agent's
  `[capabilities]` tool allow/deny lists applied, using whoever runs the
  command's `claude` login (no multi-account rotation — evals are an
  operator/CI tool).
- The `[judge]` rubric also runs in replay mode (it scores the recorded final
  answer); pass `--no-judge` for a fully deterministic, zero-cost run.
- `--report out.json` writes a machine-readable report (per-case assertions,
  judge score/rationale, transcript diagnostics, durations).

## Try it now

```bash
# Offline, no credentials needed:
duduclaw eval evals/examples/greeting-replay.toml --replay
```
