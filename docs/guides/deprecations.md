# Deprecations

Names that still work but will go away. Nothing on this page has been removed
yet: every old name listed here is still accepted, still does exactly what it
did before, and — for MCP tools — is still listed in `tools/list` so a model
that learned the old name can keep calling it.

**Policy.** A deprecated name survives **two minor versions**. Everything on
this page was deprecated in **v1.66.0** and is scheduled for removal in
**v1.68.0**.

How each surface signals deprecation:

| Surface | Signal | Still works? |
|---|---|---|
| MCP tool | `description` opens with `[deprecated → <new tool> <param>]`; `tool_catalog` marks it `deprecated: true` | Yes — still in `tools/list`, still callable. Hiding it would make it *uncallable*, which is the opposite of what a deprecation window is for |
| CLI subcommand | clap `hide = true` — gone from `--help`, still parsed | Yes |
| `config.toml` value | `warn!` once per process on read, plus an audit event when written | Yes — the configured behaviour is never silently substituted |
| Dashboard | Only the new name is offered. A saved deprecated value stays visible, labelled 已棄用 | Yes |

---

## MCP tools

### Wiki: one `wiki_*` set, with a `scope` parameter

`wiki_*` and `shared_wiki_*` were two near-mirror APIs differing only in which
wiki they addressed. They are now one set with `scope: "agent" | "shared"`,
defaulting to `agent` — so every existing `wiki_*` call is unchanged.

| Old | New |
|---|---|
| `shared_wiki_ls` | `wiki_ls` with `scope="shared"` |
| `shared_wiki_read` | `wiki_read` with `scope="shared"` |
| `shared_wiki_write` | `wiki_write` with `scope="shared"` |
| `shared_wiki_search` | `wiki_search` with `scope="shared"` |
| `shared_wiki_stats` | `wiki_stats` with `scope="shared"` |
| `shared_wiki_lint` | `wiki_lint` with `scope="shared"` |

Unchanged: the `.scope.toml` namespace SoT policy, `wiki_visible_to`
visibility, and the author-or-main-agent rule on delete. Only the entry point
moved.

**`shared_wiki_delete` is deliberately NOT merged.** It has no agent-local
twin, so a `wiki_delete` with `scope="agent"` would have *added* a destructive
capability instead of removing a duplicate one. It keeps its own name and is
not deprecated.

### Creating work: one `tasks_create`

| Old | New |
|---|---|
| `schedule_task` (recurring cron) | `tasks_create` with `schedule="<cron expression>"` |

`tasks_create` gained two optional parameters:

- **`kind`** — `"task"` (default, a Kanban board row) or `"goal"` (an
  autonomous goal: acceptance criteria frozen at creation, an AI judge panel
  decides when it is done, optional `plan_first` parks it for human approval).
  `kind="goal"` runs the same code as the dashboard's 交辦 sheet.
- **`schedule`** — a cron expression (5 or 6 fields) registers recurring work;
  an RFC3339 instant (`2026-10-01T09:00:00+08:00`) registers a one-shot
  wake-up. Both need `notify_channel` + `notify_chat_id` to deliver a result;
  a one-shot without them is refused rather than created undeliverable.

`kind="goal"` and `schedule` cannot be combined — a goal runs to completion
once. The request is refused, never half-applied.

**`goals_create` and `create_task` are NOT deprecated.** `goals_create`
creates a node in the goal *hierarchy* (Initiative → Project → Issue) — the
why-chain an assignee sees. `create_task` submits an explicit multi-step
`steps` plan to the TaskSpec dispatcher, which `tasks_create` has no parameter
for; deprecating it would promise a replacement that does not exist. Both
descriptions now say what they are and point at `tasks_create` for the case
they are *not* for.

The delegation policy check (department × hierarchy) is enforced **once, at
the merged entry**, before any branch runs. That is the security point of the
merge: a caller can no longer route a cross-department assignment through
whichever of the old tools checked least.

### Skill search: one `skill_search`, with a `source` parameter

| Old | New |
|---|---|
| `skill_bank_search` | `skill_search` with `source="bank"` |

`skill_search` gained `source`:

- `"all"` (default) — the configured skill hubs **and** this deployment's
  learned skill bank, de-duplicated by skill name;
- `"github"` — the GitHub hub only;
- `"hub"` — the curated registries;
- `"bank"` — the learned skill bank only.

Rule of thumb for a model: leave `source` alone unless you already know where
the skill lives.

The learned skill bank is still an empty in-memory stub, so `source="bank"`
honestly reports an empty store rather than quietly returning hub results.

---

## CLI subcommands

The old spellings are hidden from `--help` but still parse.

### `migrate`

Three unrelated commands whose help text had to disclaim each other:

| Old | New |
|---|---|
| `duduclaw migrate` | `duduclaw migrate schema` (bare `duduclaw migrate` still means this) |
| `duduclaw migrate-from <platform>` | `duduclaw migrate from <platform>` |
| `duduclaw data-migrate` | `duduclaw migrate data` |

### `export`

Four unrelated exports told apart only by which group they sat in:

| Old | New |
|---|---|
| `duduclaw export --out …` | `duduclaw export data --out …` (the bare form still means this) |
| `duduclaw audit …` | `duduclaw export audit …` |
| `duduclaw gdpr export <contact>` | `duduclaw export gdpr <contact>` |
| `duduclaw playbook export --agent …` | `duduclaw export playbook --agent …` |

`duduclaw gdpr erase` and `duduclaw playbook migrate-soul` are unaffected.

### `acp`

Two different protocols distinguished only by a doc-comment disclaimer:

| Old | New |
|---|---|
| `duduclaw acp` (editor-facing Agent Client Protocol) | `duduclaw acp client` (bare `duduclaw acp` still means this) |
| `duduclaw acp-server` (A2A agent-to-agent) | `duduclaw acp server` |

---

### `pack`

Three install verbs for what was always the same thing — a pre-configured set of AI employees. `duduclaw pack` is the single front door (T5/O2); `duduclaw expert install` / `expert list` are aliases over the same code, and every legacy manifest dialect is still read verbatim (no disk migration).

| Old | New |
|---|---|
| `duduclaw expert install <src>` | `duduclaw pack install <src>` |
| `duduclaw expert list` | `duduclaw pack list` |
| `expert.toml` (expert-pack manifest) | `pack.toml` (`kind = "team"`) |
| `team.toml` (premium team playbook) | `pack.toml` (`kind = "team"`, `tier = "premium"`) |
| `preset.toml` (job-preset content file) | `pack.toml` (`kind = "preset"`) |

`preset_bindings.toml` (which employee has which preset applied) is state, not a pack format, and is not deprecated. Authoring verbs (`expert pack` / `publish` / `export` / `convert-teams` / `hooks` / `remove`) stay under `duduclaw expert`.

## Config values

### `[dispatch] judge`

| Old value | Move to | Why |
|---|---|---|
| `evaluator_only` | `mav` | `[dispatch] two_stage_judge` (on by default) already runs the cheap evaluator first and pays for the panel only on a completion candidate, so the cost motive is covered without weakening acceptance |
| `human_only` | `mav`, plus per-agent `[capabilities] autonomy_level` and `approval_required_tools` | Parks work for a person where it matters, instead of disabling machine adjudication platform-wide |

`mav` and `external` are unaffected. All four values still parse: a deployment
already on a deprecated mode keeps behaving exactly as configured, logs one
warning per process, and — if the value is written through the dashboard —
records a `judge_mode_deprecated` audit event. The dashboard offers only `mav`
and `external`, but shows a saved deprecated value (labelled) rather than
silently switching it.

---

## What happens in v1.68.0

Each old name above is removed. Before upgrading past v1.67.x:

1. Grep your agent prompts, skills, and automations for the old MCP tool names.
2. Grep your scripts, cron entries, and systemd units for the old CLI spellings.
3. Check `config.toml [dispatch] judge` for a deprecated value.

The `[deprecated → …]` prefix in every affected tool description is
machine-greppable on purpose.
