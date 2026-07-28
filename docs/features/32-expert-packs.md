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

Two foreign formats install through the same command: a Claude Code plugin (`.claude-plugin/plugin.json`) and a single Agent Skill (`SKILL.md`). Format detection is fail-closed — an unrecognised layout is rejected with a listing of what was found, never half-imported.

## The Install Pipeline

Every install, including one-click dashboard installs and LLM-generated drafts, runs the same sequence:

1. **Fence the archive.** Zip extraction is guarded against zip-slip with a 50 MB cap; URL downloads share the same cap.
2. **Validate placement first.** `--attach-under <agent>` (hang the pack's root agents under an existing supervisor, e.g. your CEO) is checked before anything is written — a typo aborts with nothing installed.
3. **Scan every foreign body.** Each imported SOUL/SKILL text is demoted to DATA and passed through both the prompt-injection input guard and the skill security scanner. Any block-level finding stops that asset from landing; the report says why.
4. **Scaffold parents-first.** Agents install in topological order so `reports_to` always points at something real. Name clashes are reported as conflicts unless `--rename` opts into an `-imported` suffix. An unknown role string falls back to `worker` (a bad role would brick the agent at registry load); non-Claude model ids are kept verbatim but flagged for review — the platform never silently coerces to one model.
5. **Merge, don't replace.** `agent.partial.toml` deep-merges onto the scaffolded `agent.toml`; pack-declared MCP servers merge into the agent's `.mcp.json`, but the wired `duduclaw` server entry is never overwritten — a hostile pack cannot hijack the tool surface.
6. **Quarantine hooks.** See below.

`--dry-run` renders the full plan without writing a byte. Every item lands in an honest report (imported / skipped / conflict / warning) — nothing is silently dropped. `expert remove <slug>` deletes only what the install record says the pack created; pre-existing assets stay.

## Org Placement: Department × Rank

A roster member with a `department` gets it written to `[agent] department` in `agent.toml`, and the installer creates the matching shared-wiki department space — so the new hire shows up on the org chart and departments page immediately. Rank (`executive` / `manager` / `staff`) is display metadata: when the manifest omits it, it is derived from the role (`ceo` → executive; `main` / `front_desk` / `team_leader` / `product_manager` → manager; everything else staff). Rank is derived, never authoritative — the `reports_to` tree remains the single source of hierarchy.

## Built-in Catalog

The dashboard catalog surfaces the 22 premium industry team playbooks (clinic, pharmacy, accounting, law firm, e-commerce, …) as one-click installable packs, converted on demand by the idempotent `expert convert-teams` pipeline into a versioned cache. Standalone expert packs list alongside the teams. Entries are grouped into six category sections — health, professional, retail, lifestyle, education, other — and each card shows which departments its roster lands in, so 22+ packs read as an org menu rather than a flat grid.

## LLM-Guided Authoring

No pack for your business? Describe one. The guided flow takes an industry hint, a free-text description (2,000 chars max), a team size (1–8), and suggested channels. The model emits a **strict JSON design** — it never writes files; the gateway materializes the design into a draft pack, validates it with a mirror of the manifest validator, and shows a preview. Up to 5 generate/revise rounds per draft; drafts expire after 24 hours.

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
| Draft lifetime | 24 h |
| Hooks in generated packs | none (fail-closed) |
| Export formats | `claude-plugin` (P0) |
