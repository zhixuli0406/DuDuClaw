# CONTRACT.toml Format Specification v1.0

> DuDuClaw Agent Behavioral Boundary Definition
> Status: Draft | Date: 2026-03-31

---

## Overview

`CONTRACT.toml` defines hard behavioral boundaries for a DuDuClaw agent. Unlike SOUL.md (which guides personality), CONTRACT.toml enforces **non-negotiable rules** that the agent must never violate. The contract is validated at runtime against every agent output, and violations are logged to the security audit trail.

## File Location

```
~/.duduclaw/agents/<agent-name>/CONTRACT.toml
```

## Schema

### `[boundaries]` (Required)

Core behavioral constraints.

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

A match triggers a `ContractViolation` and the output is blocked.

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

- Injected into the system prompt as guidelines
- Used by `duduclaw test` for red-team validation
- **Not enforced at runtime** (informational contract, not output filter)

**Examples**:
```toml
must_always = [
    "include allergen warnings when discussing menu items",
    "confirm reservation details before finalizing",
    "escalate angry customers after 2 unresolved exchanges",
]
```

#### `max_tool_calls_per_turn`

Integer. Maximum number of MCP tool calls the agent can make in a single response turn. Set to `0` for unlimited. Default: `5`.

### `[browser]` (Optional)

Browser automation capability configuration. Omit this section entirely to use system defaults (deny-by-default).

```toml
[browser]
enabled = true
max_tier = "headless_browser"
trusted_domains = ["example.com", "*.gov.tw"]
blocked_domains = ["*.onion", "localhost"]
```

#### `max_tier` — Maximum Browser Escalation Level

Controls how far up the 5-layer browser router the agent can escalate.

| Value | Layer | Description |
|-------|-------|-------------|
| `"api_fetch"` | L1 | HTTP requests only (reqwest / WebFetch) |
| `"static_scrape"` | L2 | CSS/XPath selector extraction |
| `"headless_browser"` | L3 | Playwright MCP (headless) |
| `"sandbox_browser"` | L4 | Container-isolated Playwright |
| `"computer_use"` | L5 | Virtual display + Claude Vision API |

#### `trusted_domains` / `blocked_domains`

Arrays of domain patterns. Supports glob syntax (`*.example.com`).
- `trusted_domains`: Allowlist — agent can only access these domains
- `blocked_domains`: Blocklist — these domains are always rejected
- If both are set, `trusted_domains` takes precedence (allowlist mode)

### `[browser.restrictions]` (Optional)

Fine-grained browser action restrictions.

```toml
[browser.restrictions]
allow_form_submit = false       # Can the agent submit HTML forms?
allow_file_download = false     # Can the agent download files?
max_pages_per_session = 20      # Max pages visited per browser session
max_session_minutes = 10        # Max duration of a browser session
screenshot_audit = true         # Log screenshots to browser_audit.jsonl
require_human_approval_for = [  # Actions requiring human sign-off
    "form_submit",
    "login",
    "payment_*",
]
```

### `[browser.computer_use]` (Optional)

Layer 5 (Computer Use) specific configuration. Only applies when `max_tier = "computer_use"`.

```toml
[browser.computer_use]
enabled = false                 # Must be explicitly enabled
max_actions = 50                # Max mouse/keyboard actions per session
container_required = true       # Require container sandbox for L5
display_size = "1280x800"       # Virtual display resolution
blur_patterns = [               # CSS selectors to blur in screenshots
    "input[type=password]",
    ".credit-card",
    "[data-sensitive]",
]
```

## Validation Logic

The contract validator runs against every agent output:

1. Each `must_not` rule is tested against the output text
2. Matching uses case-insensitive substring first, then glob if wildcards present
3. On violation: a ~60-character context window is extracted around the match
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

Browser and computer_use sections are also injected when configured.

## Red-Team Testing

```bash
# Test agent against its CONTRACT.toml boundaries
duduclaw test <agent-name>

# Include browser automation tests (L1-L5)
duduclaw test <agent-name> --browser
```

The test runner:
1. Loads the agent's CONTRACT.toml
2. Generates adversarial prompts targeting each `must_not` rule
3. Verifies each `must_always` behavior is exhibited
4. Reports pass/fail per rule with evidence

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
