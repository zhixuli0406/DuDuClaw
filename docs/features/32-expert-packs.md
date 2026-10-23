# Expert Packs

> A whole AI team in one zip — agents, skills, SOPs, and org placement, installed through a security pipeline that trusts nothing it imports.

---

## What It Is

An expert pack is a portable bundle of a team: one or more agents in a `reports_to` hierarchy, plus the skills, shared-wiki SOP pages, recommended prompts, and channel hints that make that team useful from day one. The native format is a directory (or `.zip`, or `https://…zip` URL) with an `expert.toml` manifest:

```text
<slug>/
├── expert.toml                       # manifest: roster, category, prompts, requires
├── agents/<name>/soul.md             # persona (becomes SOUL.md)
├── agents/<name>/agent.partial.toml  # settings fragment, deep-merged onto the scaffold
├── skills/<name>/SKILL.md            # Agent Skills spec, verbatim
└── wiki/<ns>/*.md                    # shared-wiki SOP / policy pages
```

Each roster entry carries `name`, `role`, `reports_to` (in-pack supervisor), `department`, `rank`, a trigger keyword, and a skill list. `duduclaw expert pack` validates strictly before zipping (slug shape, duplicate names, `reports_to` cycles via topological sort, missing `soul.md`, SKILL.md frontmatter name must equal its directory name); `install` is lenient and reports problems instead of guessing fixes. `[expert.requires]` lists env vars and binaries doctor-style — missing ones warn, never block.

Foreign formats install through the same command: a Claude Code plugin (`.claude-plugin/plugin.json`), a single Agent Skill (`SKILL.md`), and agent definitions from other platforms (Doubao partner packages, OpenClaw, Hermes, `AGENTS.md` / `CLAUDE.md` / `GEMINI.md`, Claude Code / Copilot subagents, Dify app exports; see [Importing AI staff from other platforms](../guides/agent-config-import.md)). Format detection is fail-closed — an unrecognised layout is rejected with a listing of what was found, never half-imported.

## One Format, One Front Door

An expert pack, a premium team playbook and a job preset used to be three manifest dialects with three install verbs. They are now one schema (`pack.toml`) behind one command:

```bash
duduclaw pack list                 # installed packs, local presets, what the catalog offers
duduclaw pack inspect <src>        # normalized view + which dialect it was read from
duduclaw pack install <src>        # routes by `kind`; premium content checks the licence once
```

`[pack] kind` chooses the route — `preset` (one job configuration, no identity), `team` (a roster, what an expert pack has always been), or `template` (a single industry persona). `[pack] tier` is the single premium decision: instead of four code paths each guessing "is this paid content" from a directory path, a pack carries its tier and one predicate reads it. A tier that cannot be read counts as premium — an unreadable field must never unlock paid content.

`duduclaw expert install` was removed in v1.69.0: it prints `duduclaw pack install` and exits with code 2. The authoring verbs (`pack`, `publish`, `export`, `convert-teams`, `hooks`, `remove`) and `expert list` (which lists the installed records) stay under `duduclaw expert`. The dashboard's one-click install and `experts.install` reach the same code, because both already drive the CLI.

**`expert.toml`, `team.toml` and `preset.toml` are still read.** Nothing on disk is rewritten. `expert.toml`, `team.toml` and the industry pack directory layout are deprecated and will be removed together with the rewritten premium templates in a later version (no version number is set). The reason is that a `pack.toml` team or industry pack cannot be installed yet: the installer accepts only a `pack.toml` job preset. `preset.toml` is the storage format of job presets and is not deprecated. `duduclaw pack inspect <dir> --emit-canonical` prints the `pack.toml` a legacy pack corresponds to so you can review and migrate it deliberately, rather than having a machine rewrite content you are responsible for.

## The Install Pipeline

Every install, including one-click dashboard installs and LLM-generated drafts, runs the same sequence:

1. **Fence the archive.** Zip extraction is guarded against zip-slip with a 50 MB cap; URL downloads share the same cap.
2. **Validate placement first.** `--attach-under <agent>` (hang the pack's root agents under an existing supervisor, e.g. your CEO) is checked before anything is written — a typo aborts with nothing installed.
3. **Scan every foreign body.** Each imported SOUL/SKILL text is demoted to DATA and passed through both the prompt-injection input guard and the skill security scanner. Any block-level finding stops that asset from landing; the report says why.
4. **Scaffold parents-first.** Agents install in topological order so `reports_to` always points at something real. Name clashes are reported as conflicts unless `--rename` opts into an `-imported` suffix. An unknown role string falls back to `worker` (a bad role would brick the agent at registry load); non-Claude model ids are kept verbatim but flagged for review — the platform never silently coerces to one model.
5. **Merge, don't replace.** `agent.partial.toml` deep-merges onto the scaffolded `agent.toml`; pack-declared MCP servers merge into the agent's `.mcp.json`, but the wired `duduclaw` server entry is never overwritten — a hostile pack cannot hijack the tool surface.
6. **Quarantine hooks.** See below.

`--dry-run` renders the full plan without writing a byte. Every item lands in an honest report (imported / skipped / conflict / warning) — nothing is silently dropped. `expert remove <slug>` deletes only what the install record says the pack created; pre-existing assets stay.

Installing the same pack again takes `--replace <slug>` (the earlier version moves to a backup first and back on failure) or `--as-copy` to keep both; the dashboard offers the same choice. Before anything is written, the installer counts the AI staff the pack would add and refuses the whole pack when that exceeds the license limit. Error codes and what to do about them: [Import AI staff from another platform](../guides/agent-config-import.md).

## Org Placement: Department × Rank

A roster member with a `department` gets it written to `[agent] department` in `agent.toml`, and the installer creates the matching shared-wiki department space — so the new hire shows up on the org chart and departments page immediately. Rank (`executive` / `manager` / `staff`) is display metadata: when the manifest omits it, it is derived from the role (`ceo` → executive; `main` / `front_desk` / `team_leader` / `product_manager` → manager; everything else staff). Rank is derived, never authoritative — the `reports_to` tree remains the single source of hierarchy.

## Built-in Catalog

The dashboard catalog surfaces the 22 premium industry team playbooks (clinic, pharmacy, accounting, law firm, e-commerce, …) as one-click installable packs, converted on demand by the idempotent `expert convert-teams` pipeline into a versioned cache. Standalone expert packs list alongside the teams. Entries are grouped into six category sections — health, professional, retail, lifestyle, education, other — and each card shows which departments its roster lands in, so 22+ packs read as an org menu rather than a flat grid.

## LLM-Guided Authoring

No pack for your business? Describe one. The guided flow takes an industry hint, a free-text description (2,000 chars max), a team size (1–8), and suggested channels. The model emits **strict JSON designs** — it never writes files; the gateway materializes them into a draft pack, validates it with a mirror of the manifest validator, and shows a preview. Up to 5 generate/revise rounds per draft; drafts expire after 24 hours.

Generation runs in stages, so a team is no longer squeezed into one model reply:

1. **Roster** — members, roles, responsibilities, reporting lines.
2. **One call per member** — the member's role description (SOUL) with required sections (who I am, scope, workflow steps, at least two example answers, red lines each with a reason, when to hand over to a human, tone), the member's own skill(s) (when to use, steps, an output template, a worked example), and CONTRACT rules restated from the SOUL's red lines. Every worker gets at least one skill of its own; the front desk gets the routing skill.
3. **Knowledge base** — at least four shared wiki pages: process SOP, glossary, compliance notes and FAQ. The compliance page always opens with a notice that law names, article numbers and figures need a human check; the prompt forbids inventing them.

Stages 2 and 3 run two model calls at a time. Each stage checks its JSON and gets one retry with the problems fed back. A member that still fails keeps a stand-in role description built from the roster and no skills, and the preview says which member fell back (a *partial* draft); only a roster failure, or every member failing, fails the run. The dashboard starts the run in the background and polls its progress per stage; cancelling stops the model calls and keeps the previous round of the draft. The prompts carry a built-in worked example (one SOUL, one skill, one wiki page), plus an installed industry pack's manifest when one is cached, so a fresh install gets the same quality bar. Newly generated members no longer carry model-written `agent.partial.toml` settings. Member ids always start with the team slug (`flowershop-team` → `flowershop-…`), and the routing trigger is derived from the display name (letters and digits only), never taken from the model.

An optional switch lets the knowledge-base stage use web search (Claude CLI's `WebSearch`, which runs on the vendor side). Every other tool stays off, including `WebFetch`; the Direct API fallback has no tools. Pages written with web search are marked, the preview shows their full text, and install is refused until the admin confirms they reviewed them. When the CLI failed and the tool-less fallback wrote the knowledge base, the preview says no web search happened. A run stops after 25 minutes by default (`[experts] generate_timeout_minutes` in `config.toml`, 5–120); a timed-out, failed or cancelled round keeps the previous one. At most 3 generations run at once; installs do not count.

**Red lines in CONTRACT.toml are also an output filter.** `must_not` rules are matched word for word against every channel reply, and a match blocks the reply. Rules are therefore sanitized, both when a team is generated and when any pack is installed: wildcard characters are removed, rules shorter than 10 characters are dropped, and so are rules that repeat what the employee is meant to say (its handoff wording and example answers), so a single keyword such as "refund" can never block every reply that mentions refunds. Dropped rules are listed in the install result. Review the red lines in the preview before installing.

The preview lists each member's skills and red lines, the knowledge-base pages, any partial-generation notes, and what the installer is predicted to skip (an AI staff member or wiki page with the same name already exists, or content that fails the safety scan). After install, the dashboard reports what actually landed per member (skills, CONTRACT.toml) and per wiki page; an existing same-name page is never overwritten and is reported instead.

When any member id of a pack is already taken, the installer renames the whole team with one suffix (`-2`, `-3`, …; `-imported` first with `--rename`) so reporting lines stay inside the team. A member whose display name or trigger another employee already answers to is renamed 「name（id）」 and its trigger follows; the preview warns ahead and the install result lists every rename. A pack's skill count includes per-employee skills.

Two hard rules keep self-authoring safe: generated packs may **never contain hooks** (blocked in the prompt and post-validated, fail-closed), and installing a draft goes through the full CLI security pipeline above — LLM output is treated as external content, same as a stranger's zip.

## Hooks: Quarantined Until a Human Says Yes

Imported hooks are arbitrary commands wired into an agent runtime — a supply-chain risk. So they are copied **disabled** into a quarantine directory and never wired implicitly. Enablement needs an explicit grant: `--trust-hooks` at install time, or an ApprovalBroker request decided later in the dashboard approval center and applied with `duduclaw expert hooks <slug>`. No grant, a denial, or a TTL-expired approval all leave hooks disabled (fail-closed). The state machine (`disabled → pending_approval → enabled | disabled`) is persisted per pack and shared between CLI and dashboard.

## Export

`duduclaw expert export <slug> --format claude-plugin` turns an installed pack back into a Claude Code plugin: `.claude-plugin/plugin.json` (DuDuClaw-specific fields under an `x-duduclaw` key, which Claude Code ignores), one `agents/<id>.md` per agent with frontmatter plus the SOUL body, and the agents' non-duduclaw MCP servers aggregated at plugin level. Teams built here are not locked in here.

## Limits

| Aspect | Limit |
|---|---|
| Archive size (zip / download) | 50 MB |
| Generated team size | 1–8 agents |
| Generation rounds per draft | 5 |
| Model calls in flight during generation | 2 |
| One generation run | 25 min by default (`[experts] generate_timeout_minutes`, 5–120) |
| Generation jobs at once | 3 (installs not counted) |
| Knowledge-base pages per generated team | 4–10 |
| Skills per generated member | 1–3 |
| Draft lifetime | 24 h |
| Hooks in generated packs | none (fail-closed) |
| Export formats | `claude-plugin` (P0) |
