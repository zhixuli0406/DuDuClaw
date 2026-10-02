# CONTRACT.toml Format Specification v1.0

> DuDuClaw Agent Behavioral Boundary Definition
> Status: Draft | Date: 2026-03-31

---

## Overview

`CONTRACT.toml` defines hard behavioral boundaries for a DuDuClaw agent. Unlike SOUL.md (which guides personality), CONTRACT.toml states **non-negotiable rules** that the agent must never violate. Its `must_not` rules are checked at runtime against the final text of every channel reply, and a violation blocks the reply and is logged to the security audit trail. Output from other paths (delegated tasks, cron, heartbeat, goal-loop rounds) and tool calls are not checked.

## File Location

```
~/.duduclaw/agents/<agent-name>/CONTRACT.toml
```

Operators edit it through the dashboard (the admin-only `contract.get` / `contract.update` RPCs) or by hand. An agent cannot change it. The `agent-file-guard` hook (a Claude Code PreToolUse hook) blocks an agent from writing another agent's files, from writing agent files outside the agents tree, and from writing its own `CONTRACT.toml` (decision `BlockedOwnContractWrite`, `duduclaw_core::check_own_contract_write`; there is no opt-in flag). The own-contract rule covers Write, Edit and MultiEdit, plus a Bash heuristic that blocks a write-shaped command naming the file, as `agents/<self>/CONTRACT.toml` or as a relative spelling such as `CONTRACT.toml` or `./CONTRACT.toml`. The block message tells the agent to ask the operator. The Bash rule is a speed bump: a command that hides the file name gets past it, and real isolation is not giving the agent Bash. Promoting a live-fork (`fork_run`) branch back into the agent directory never copies `CONTRACT.toml` or the other agent-structure files over the parent's copy.

## Schema

### `[boundaries]` (Optional)

Core behavioral constraints. Every key in this section is optional: a missing `must_not` or `must_always` is an empty list and a missing `max_tool_calls_per_turn` is `0` (unlimited). A missing `CONTRACT.toml`, or one that fails to parse as TOML, is treated as an empty contract (the parse failure is logged as a warning). `duduclaw test` still reports a contract with no rules as a failed check.

```toml
[boundaries]
must_not = [
    "pattern or substring the agent must NEVER output",
    "supports glob wildcards: *text*, ?single, [range]",
]
must_always = [
    "behavior the agent must ALWAYS exhibit",
    "used for red-team testing via `duduclaw test`",
]
max_tool_calls_per_turn = 5  # 0 = unlimited
```

#### `must_not` — Output Blocklist

Array of strings. Each string is matched against the agent's output text using:

1. **Case-insensitive substring match** (default)
2. **Glob pattern match** (if string contains `*`, `?`, or `[`)

A match triggers a `ContractViolation`. On the channel reply path, the outgoing reply is replaced by a fixed block message and a `contract_violation` event is written to the audit log (`channel_reply/entry.rs`, `duduclaw-security/src/audit.rs`).

**Examples**:
```toml
must_not = [
    "recommend competitor restaurants",        # substring match
    "*refund*guarantee*",                       # glob: blocks "refund guarantee" anywhere
    "profit margin*",                           # glob: blocks "profit margin" at start
    "internal pricing ?or? cost",               # glob: single char wildcards
]
```

#### `must_always` — Behavioral Requirements

Array of strings describing behaviors the agent is expected to exhibit. These are:

- Injected into the system prompt as guidelines (`contract_to_prompt()`)
- Counted by `duduclaw test` (the "Behavioral contract" check needs at least one rule)
- Not checked by evolution in practice: the evolution gate (`gate_must_always` in `crates/duduclaw-gateway/src/gvu/verifier_gate.rs`) only checks `must_always` against a simulated final SOUL.md state, and the current playbook-based evolution never produces one (its only caller, `gvu/aee/inner_loop.rs`, passes `simulated_final: None`). Today `must_always` reaches the agent only through the system prompt
- **Not enforced against agent replies at runtime** (informational, not an output filter)

**Examples**:
```toml
must_always = [
    "include allergen warnings when discussing menu items",
    "confirm reservation details before finalizing",
    "escalate angry customers after 2 unresolved exchanges",
]
```

#### `max_tool_calls_per_turn`

Integer. Maximum number of MCP tool calls the agent can make in a single response turn. Set to `0` for unlimited. Default when the key is absent: `0` (the file written by the setup wizard sets `5`). Not enforced at runtime: a value above 0 is only added to the system prompt as "Maximum tool calls per turn: N".

### Keys that are not part of the format

The parser (`crates/duduclaw-agent/src/contract.rs`, `struct Contract`) has exactly one section, `[boundaries]`, with the three keys above. It does not use `deny_unknown_fields`, so any other section or key in the file is silently ignored: it neither fails to load nor changes behavior.

In particular, there is **no `[browser]` section**. Earlier drafts of this spec described `[browser]`, `[browser.restrictions]` and `[browser.computer_use]` (a "5-layer browser router" with `max_tier`, `trusted_domains`, `blocked_domains`, `allow_form_submit`, `allow_file_download`, `max_pages_per_session`, `max_session_minutes`, `screenshot_audit`, `require_human_approval_for`, `enabled`, `max_actions`, `container_required`, `display_size`, `blur_patterns`). None of these keys is read by any code; the router they configured was deleted in 2026-09 (see [08-browser-automation](../features/08-browser-automation.md)). Browser and computer-use permissions live in `agent.toml [capabilities]` (`computer_use`, `browser_via_bash`, `allowed_tools`, `denied_tools`, and `[capabilities.computer_use_config]` for `max_actions`, `max_session_minutes`, `display_width`, `display_height`, `allowed_apps`, `blocked_actions`, `auto_confirm_trusted`), not in CONTRACT.toml. Files that still carry a `[browser]` section stay valid; the section is ignored.

## Validation Logic

The contract validator runs against the final text of each channel reply (`channel_reply/entry.rs`):

1. Each `must_not` rule is tested against the output text
2. Matching uses case-insensitive substring first, then glob if wildcards present
3. On violation: a context window of about 20 characters on each side is extracted around the match
4. Returns `ValidationResult { passed: bool, violations: Vec<ContractViolation> }`
5. Violations are logged to `~/.duduclaw/security_audit.jsonl`

## System Prompt Injection

`contract_to_prompt()` generates a Markdown section from the contract and injects it into the agent's system prompt:

```markdown
## Behavioral Contract

### You must NEVER:
- recommend competitor restaurants
- reveal food cost or profit margins
- ...

### You must ALWAYS:
- include allergen warnings when discussing menu items
- ...
```

Only `must_not`, `must_always` and `max_tool_calls_per_turn` are injected; nothing else in the file reaches the prompt.

## Red-Team Testing

```bash
# Test agent against its CONTRACT.toml boundaries
duduclaw test <agent-name> [--bank <file>]

# Generate attack prompts from each must_not rule and run them through the input scanner
duduclaw redteam [--agent <name>] [--out <path>]
```

`duduclaw redteam` (`cmd_redteam` in `crates/duduclaw-cli/src/lib.rs`, attack templates in `crates/duduclaw-gateway/src/redteam.rs`) builds a set of jailbreak prompts for every `must_not` rule and reports which ones the deterministic input scanner (`input_guard::scan_input`) catches. `--agent` defaults to the default agent; `--out` writes the full attack suite to a file. Neither `duduclaw test` nor `duduclaw redteam` calls a model.

The test runner (`cmd_test_agent` in `crates/duduclaw-cli/src/lib.rs`) runs a fixed set of checks, not per-rule generated prompts: SOUL.md integrity, that a contract with at least one rule exists, injection payloads against the input scanner, and a simulated bad output validated against `must_not`. It reports pass/fail per check. `duduclaw test` takes an agent name and an optional `--bank` file; it has no `--browser` flag.

## Constraints

- **Encoding**: UTF-8
- **Format**: Valid TOML (parsed by `toml` crate)
- **`must_not` array**: No hard limit, but keep under 20 rules for performance
- **`must_always` array**: No hard limit
- **Pattern complexity**: Avoid deeply nested globs; simple substring matching is faster

## Example

See complete examples in:
- `templates/restaurant/CONTRACT.toml` — Food service boundaries
- `templates/manufacturing/CONTRACT.toml` — Factory safety boundaries
- `templates/trading/CONTRACT.toml` — B2B trading boundaries
