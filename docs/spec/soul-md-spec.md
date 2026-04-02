# SOUL.md Format Specification v1.0

> DuDuClaw Agent Identity Document Format
> Status: Draft | Date: 2026-03-31

---

## Overview

`SOUL.md` is the authoritative identity document for a DuDuClaw agent. It defines personality, responsibilities, and behavioral guidelines in freeform Markdown. The Evolution Engine reads and rewrites this file during GVU self-play cycles.

## File Location

```
~/.duduclaw/agents/<agent-name>/SOUL.md
```

## Format

SOUL.md is **pure Markdown** with no YAML frontmatter. The Evolution Engine parses section headings to locate and update specific areas during GVU cycles.

## Required Sections

### `# <Role Title>`

Top-level heading. One sentence describing the agent's primary function.

```markdown
# Restaurant Customer Service Assistant
```

### `## Identity`

1-2 sentences defining what the agent represents and its core purpose.

```markdown
## Identity

I am the AI customer service assistant for [Restaurant Name], helping customers
with menu inquiries, reservations, business hours, and promotions.
```

### `## Personality`

Bulleted list of personality traits. Each trait has a short label followed by a description.

```markdown
## Personality

- **Warm and welcoming** — greet every customer with enthusiasm
- **Precise** — provide accurate information, never guess
- **Patient** — handle repeated questions gracefully
```

### `## Language`

Language configuration for the agent's responses.

```markdown
## Language

- Primary: 繁體中文 (zh-TW)
- Secondary: English
- Tone: Professional yet approachable; avoid jargon
```

### `## Core Responsibilities`

Numbered list of 4-8 main operational duties.

```markdown
## Core Responsibilities

1. Answer menu and pricing inquiries
2. Handle reservation requests (date, time, party size)
3. Provide business hours and location information
4. Process customer complaints with empathy
5. Promote current specials and seasonal menus
```

### `## Response Style`

Guidelines for formatting and length of responses.

```markdown
## Response Style

- Keep responses under 200 characters for LINE/Telegram
- Use bullet points for lists of 3+ items
- Include relevant emoji sparingly (1-2 per message)
- Always end with a follow-up question or call-to-action
```

### `## Escalation Rules`

Conditions that trigger handoff to a human operator.

```markdown
## Escalation Rules

- Customer expresses anger after 2 unresolved exchanges
- Request involves refunds, legal matters, or safety concerns
- Customer explicitly asks to speak with a human
- Agent confidence is below threshold for 3 consecutive turns
```

## Optional Sections

| Section | Purpose |
|---------|---------|
| `## Domain Knowledge` | Industry-specific facts the agent should know |
| `## Greeting Templates` | Pre-defined greetings for different contexts (DM, group, returning user) |
| `## Prohibited Topics` | Topics the agent must decline to discuss (supplements CONTRACT.toml) |
| `## Evolution Notes` | Auto-generated section — GVU Updater appends observations here |

## Evolution Engine Integration

- **SHA-256 fingerprint**: SOUL Guard computes a hash after each write; drift triggers an alert
- **Version history**: `VersionStore` maintains full history in `evolution.db`
- **Observation period**: After any GVU-driven change, a 24-hour observation window begins
- **Auto-rollback**: If prediction error increases during observation, the change reverts automatically
- **Atomic write**: All updates use temp-file + rename to prevent corruption

## Constraints

- **Encoding**: UTF-8
- **Max size**: No hard limit, but keep under 4,000 tokens for optimal prompt injection
- **No frontmatter**: Do not use YAML/TOML frontmatter — the file must be pure Markdown
- **Heading levels**: Use `##` for all sections; `#` is reserved for the role title
- **No executable code**: Fenced code blocks are for illustration only; the engine does not execute them

## Validation

There is no strict schema validation for SOUL.md — it is treated as freeform content. However, the following soft checks apply:

1. File must contain at least one `#` heading
2. File must be valid UTF-8
3. File should contain `## Identity` section (warning if missing)
4. SHA-256 hash must match stored fingerprint (security check)

## Example

See complete examples in:
- `templates/restaurant/SOUL.md` — Food service agent
- `templates/manufacturing/SOUL.md` — Factory operations agent
- `templates/trading/SOUL.md` — B2B sales agent
