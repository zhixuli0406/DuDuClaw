# SKILL.md format specification

A skill is a Markdown file with a YAML frontmatter block. DuDuClaw parses the
frontmatter into `SkillMeta` and treats the body as the skill content. Both the
Anthropic `<skill-name>/SKILL.md` layout and the legacy flat `<skill>.md` layout
are supported.

## Frontmatter fields

```yaml
---
name: compress-context          # required — machine-stable id / registry key
description: Compress the conversation history when it grows too long
trigger: /compress              # optional invocation trigger
tools: [search, code]           # optional tool allowlist
tags: [utility, memory]         # optional
author: agnes                   # optional
version: 1.0.0                  # optional
requires: [tokenizer]           # optional — other skills this depends on
skill_type: atomic              # atomic | functional | planning
compose_mode: parallel          # parallel | sequential | conditional

# WP8 — localisation. Employees see the display strings for their locale.
display:
  zh-TW:
    name: 壓縮對話
    description: 對話太長時自動壓縮歷史
  ja-JP:
    name: 会話を圧縮
    description: 会話履歴が長くなったら圧縮する

# WP8 — time saved per use, set by the skill-activation approval flow.
estimated_minutes_saved: 15
---

# Skill body (Markdown)
...
```

### `name` and `description`

`name` is the machine-stable identity used as the registry key, for override
dedup, and in `requires`. It never changes with locale. `description` is the
default (usually English) summary.

### `display` — localised names (WP8)

A map keyed by locale (`zh-TW` / `en` / `ja-JP` / …). Each entry has an optional
`name` and `description`. Presentation surfaces (`skill_list`, `skill_search`,
the dashboard SkillMarketPage, channel messages) resolve display text with this
fallback chain:

```
requested locale → zh-TW → original `name` / `description`
```

Blank localised strings are skipped, so a half-filled entry never shadows the
original. Skills predating WP8 have an empty `display` map and render from
`name`/`description` unchanged — fully backward compatible.

### `estimated_minutes_saved` (WP8)

Minutes this skill saves per use, gathered in the skill-creation dialogue ("大概
能省下幾分鐘?") and written on manager approval (`action_kind =
skill_activation`). The WP10 leaderboard multiplies it by usage count to rank
skills by cumulative time saved. Absent ⇒ treated as 0 by the leaderboard.

## Backward compatibility

Every field except `name` is optional with a sensible default. A SKILL.md with
only `name` and a body parses. A file with no frontmatter at all is treated as a
body-only skill whose `name` comes from the filename.
