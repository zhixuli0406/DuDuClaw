# Build your own expert pack (hands-on tutorial)

> Audience: creators (SIs, consultants, community contributors) who want to package "one AI employee" or "a whole team" into a shareable install bundle.
> This is a tutorial; for the full field reference see [features/32-expert-packs.md](../features/32-expert-packs.md).

An expert pack is DuDuClaw's unified packaging unit: a directory (or a zip / URL) holding an employee's persona, skills, SOP wiki pages, and recommended prompts, so anyone can install it into their own DuDuClaw with a single command.

## 1. Minimum viable pack (10 minutes)

Create a directory:

```
my-first-pack/
├── expert.toml
├── agents/
│   └── helper/
│       ├── soul.md              # employee persona (identity/responsibilities/boundaries)
│       └── agent.partial.toml   # optional: fragment deep-merged into agent.toml
└── skills/
    └── greeting/
        └── SKILL.md             # optional: a skill bundled with the pack
```

Minimum `expert.toml` content (fields follow [features/32](../features/32-expert-packs.md) as the source of truth):

```toml
[expert]
name = "my-first-pack"
description = "Demo: a friendly little helper"
version = "0.1.0"
author = "your name"
license = "MIT"
tags = ["demo"]
category = "general"

[[expert.agents]]
name = "helper"
role = "main"
display_name = "Helper"
```

Write the persona in `agents/helper/soul.md`. Identity / responsibilities / boundaries is a good starting structure — the clearer the boundaries, the more confidently people will install it.

### About `pack.toml`

Use `expert.toml` for team packs and industry packs. The newer `pack.toml` schema can describe teams, industry packs and job presets, and `duduclaw pack inspect` reads all of them. But `duduclaw pack install` can only install a `pack.toml` whose `kind` is `"preset"` (a job preset). A `pack.toml` with `kind = "team"` or `"template"` can be read and inspected and cannot be installed yet, because the installer behind it recognises a team or industry pack only by its `expert.toml`. For that reason `expert.toml` is deprecated but still read, and it will be removed together with the rewritten premium templates once a `pack.toml` team pack can be installed. No version number is set.

Have a pack and want to see how it looks under the canonical schema? `duduclaw pack inspect <dir> --emit-canonical` prints the equivalent `pack.toml` for you to review. It never writes over your files.

## 2. Local test loop

```bash
# Read it back the way DuDuClaw sees it, before installing anything
duduclaw pack inspect ./my-first-pack

# Validate + install (installs directly from a directory)
duduclaw pack install ./my-first-pack

# See what got installed
duduclaw pack list

# Package it into a shareable zip (authoring verbs stay under `expert`)
duduclaw expert pack ./my-first-pack

# Others install it (local zip or URL both work)
duduclaw pack install ./my-first-pack-0.1.0.zip
duduclaw pack install https://example.com/my-first-pack-0.1.0.zip

# Clean up (removes the pack's employees, bundled skills, and wiki pages)
duduclaw expert remove my-first-pack
```

`duduclaw expert install` was removed in v1.69.0: it prints `duduclaw pack install` and exits with code 2. `expert list` still works and lists the installed records.

Install-side protections are built in: a zip-slip fence, a 50MB cap, content scanning. **Hooks always install disabled** into a quarantine directory (`hooks-disabled/`) and need an explicit operator trust decision before they run. Don't assume hooks will just work when you write a pack.

## 3. Advanced: teams, wiki pages, requirement declarations

- **Multi-agent teams**: multiple `[[expert.agents]]` entries, hierarchy via `reports_to` (created in topological order automatically at install time), departments via `department`.
- **SOPs / knowledge**: `wiki/<namespace>/*.md` files install into the shared knowledge base. Put regulations, scripts, and price lists here — not in the SOUL.
- **Requirement declarations**: `[expert.requires]`'s `env` (required environment variables) and `bins` (required external commands) let installers know the prerequisites before they install, instead of hitting a wall afterward.
- **Recommended prompts**: `[expert.prompts] recommended` lists 3-5 "try these first" lines — this is the installer's first win.

## 4. Converting existing assets

- Already have DuDuClaw teams? `duduclaw expert convert-teams` batch-converts team playbooks into packs.
- Publishing to the Claude Code ecosystem? `duduclaw expert export <slug> --format claude-plugin` converts to plugin format.

## 5. Publishing and quality

Today's publishing path: put the zip at any downloadable URL (a GitHub Release is the easiest), and others run `pack install <url>`. You're also welcome to add a page to the template gallery (`distribution/gallery/`). A centralized registry (PR submission + automated validation + signing) is under construction.

Quality checklist (a future tiered scorecard will look at these):
- [ ] SOUL has a clear "boundaries" section
- [ ] `requires` honestly lists prerequisites
- [ ] Includes an eval case (`duduclaw eval-scaffold` can draft one from the SOUL) — packs with an eval rank higher
- [ ] Changelog-style version notes (what changed in which version)
