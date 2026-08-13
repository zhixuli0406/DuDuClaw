# Expert Pack Format v1.0 (Draft)

> Status: Draft, seeking feedback. The reference implementation is `duduclaw expert` (install/pack/publish/export); this document is the normative description so OTHER tools can produce and consume packs.

## 1. Purpose & positioning

An **expert pack** is a portable "complete AI employee (or team)" — persona, hierarchy, skills, knowledge pages, prerequisites and recommended prompts, in one directory/zip.

The 2026 interop landscape solves adjacent problems, not this one:

| Standard | What it captures | What it doesn't |
|---|---|---|
| SKILL.md (Agent Skills) | one reusable capability | who the agent *is*, its team, its knowledge |
| AGENTS.md | per-project context for coding agents | portability of the agent itself |
| A2A agent card | identity + wire capabilities for interop | the agent's inner definition |
| Letta `.af` | one stateful agent's serialized runtime | teams, knowledge bases, install semantics |

The expert pack occupies the remaining slot: **the whole employee, installable**. It deliberately *composes with* the standards above rather than replacing them (skills ship as SKILL.md verbatim; installed agents expose A2A cards; export to Claude Code plugin exists today).

## 2. Layout

```
<pack>/
├── expert.toml                     # manifest (normative, §3)
├── agents/<name>/soul.md           # persona (identity / duties / boundaries)
├── agents/<name>/agent.partial.toml# optional config fragment, deep-merged
├── skills/<name>/SKILL.md          # optional bundled skills (Agent Skills format)
├── wiki/<namespace>/*.md           # optional shared-knowledge pages
├── hooks/                          # optional lifecycle hooks (see §4 security)
└── evals/                          # optional eval suite (quality-tier signal)
```

Distribution forms: directory, `.zip` (≤50MB unpacked), or an `https://…zip` URL. Registry distribution adds sha256 + (code lane) minisign signature — see the registry spec in `distribution/registry/`.

## 3. `expert.toml` (normative field reference)

```toml
[expert]
name = "clinic-team"          # slug: [a-z0-9-], unique install id
description = "…"             # required, human one-liner
version = "1.0.0"             # semver
author = "…"
license = "MIT"               # SPDX-ish string
tags = ["clinic", "front-desk"]
category = "clinic"           # one primary category

[expert.display_name]          # locale → display string
"zh-TW" = "診所前台團隊"

[expert.prompts]
recommended = ["…", "…"]      # "try these first" (installer First-Win)

[expert.channels]
suggested = ["line"]          # channel hints, non-binding

[[expert.agents]]              # one block per employee
name = "front-desk"           # slug; must have agents/front-desk/soul.md
role = "main"                 # main | worker (implementation-defined extensible)
display_name = "前台"
reports_to = ""               # parent agent name; "" = root. Install is
                               # topologically ordered (parents first).
department = ""               # org grouping (delegation policy input)
rank = ""                     # advisory label, NOT an authorization input
trigger = "@front"            # mention trigger hint
skills = ["greeting"]         # names under skills/ this agent gets

[expert.requires]
env = ["SOME_API_KEY"]        # env vars the pack needs — declared, not fetched
bins = ["ffmpeg"]             # external binaries required
```

Rules: unknown keys are ignored (forward compatibility); missing required keys fail validation; `requires` is **declarative honesty** — installers surface it before install, nothing auto-installs.

## 4. Install semantics (what a conforming installer MUST do)

1. **Fence the archive**: reject path traversal (zip-slip), cap unpacked size.
2. **Topological order**: create agents parents-before-children per `reports_to`.
3. **Deep-merge, never clobber**: `agent.partial.toml` merges into generated config; existing same-name assets are a conflict (rename is an explicit flag, not a default).
4. **Hooks are quarantined**: imported hooks land DISABLED and require an explicit operator trust action. A pack must function without its hooks enabled.
5. **Scan foreign content**: personas/skills pass the host's content scanning before activation.
6. **Removal is scoped**: uninstall removes pack-owned assets only; pre-existing assets are untouched.

## 5. Interop mappings

### 5.1 → Claude Code plugin（shipped: `duduclaw expert export --format claude-plugin`）

| Pack | Plugin |
|---|---|
| `expert.toml` name/description/version | `.claude-plugin/plugin.json` |
| `skills/<n>/SKILL.md` | `skills/<n>/SKILL.md`（verbatim） |
| agents' souls | commands/context material（plugin has no agent runtime）|

### 5.2 → Letta `.af`（mapping sketch; no shipped converter — honest gaps marked）

| Pack | `.af` | Notes |
|---|---|---|
| `soul.md` | system prompt | direct |
| `agent.partial.toml` model prefs | `llm_config` | partial |
| memory | ✗ | packs ship *empty* employees by design（.af serializes a live agent's memory; a pack is a template, not a snapshot）|
| team (`reports_to`) | ✗ | `.af` is single-agent |

### 5.3 → A2A agent card

Installed agents expose `/.well-known/agent-card.json`; pack fields map to card identity (name/description) and `x-duduclaw` capability extension. The pack is the *source*, the card is the *runtime advertisement* — cards are generated, never hand-shipped in packs.

## 6. Versioning & stability

- This spec: semver; v1.x additions are backwards-compatible (new optional keys only).
- Breaking changes ⇒ v2 with a new top-level marker; installers refuse majors they don't know (fail closed).

## 7. Reference material

- Tutorial: [guides/build-your-own-pack.md](../guides/build-your-own-pack.md)
- Feature doc: [features/32-expert-packs.md](../features/32-expert-packs.md)
- Registry (distribution + trust lanes): `distribution/registry/README.md`
- Reference implementation: `crates/duduclaw-cli/src/expert/`
