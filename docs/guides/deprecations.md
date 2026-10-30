# Deprecations and removals

This page lists renamed or merged public names, which of them have been
removed, and which are still on their way out.

**Policy.** A deprecated name survives **two minor versions**, then it is
removed. The policy is unchanged. v1.69.0 removed everything that was
deprecated in v1.66.0, with the exceptions listed under
[Still deprecated](#still-deprecated) and
[Names withdrawn from the deprecation list](#names-withdrawn-from-the-deprecation-list).

How each surface signals a deprecation while the window is open:

| Surface | Signal | Still works? |
|---|---|---|
| MCP tool | `description` opens with `[deprecated → <new tool> <param>]`; `tool_catalog` marks it `deprecated: true` | Yes. It stays in `tools/list` and stays callable, because hiding a tool makes it uncallable, which defeats the point of a window |
| CLI subcommand | clap `hide = true`: gone from `--help`, still parsed | Yes |
| `config.toml` value | `warn!` once per process on read, plus an audit event when written | Yes. The configured behaviour is never silently substituted |
| Dashboard | Only the new name is offered. A saved deprecated value stays visible, labelled 已棄用 | Yes |
| Agent runtime | `warn!` once per process on read; a `runtime_provider_deprecated` audit event when written through the dashboard. The dashboard stops offering it and labels a saved value 已棄用 | Yes. Parsed and executed exactly as before |

---

## Changed without a deprecation window (v1.74.0)

These changes break existing callers and get no two-version window,
because they close a gap rather than rename something.

### `accept_subscription_risk` on subscription-login RPCs

Custom WebSocket / RPC clients must send `accept_subscription_risk: true`
(a JSON boolean) on these calls, or the gateway refuses them with error code
`subscription_risk_not_accepted`:

- `auth.cli_login.start`
- `accounts.setup_token_start`
- `accounts.add` with `type = "oauth"`
- `config.raw.set` when the submitted `config.toml` adds an `[[accounts]]`
  entry with `type = "oauth"` that the stored file does not have

Send it only after the person has read the subscription-login risk notice.
API-key accounts and other raw edits are unchanged, and the dashboard already
sends the flag after its checkbox. The operator command
`duduclaw auth device --provider copilot` now needs
`--accept-subscription-risk`; scripts that call it must add the flag.

The DuDuClaw OS first-run setup (OOBE) also calls `auth.cli_login.start`.
OS images up to v0.4.0 embed platform v1.73.4 and are unaffected; the OS shell
sends the flag from [DuDuClaw-OS #21](https://github.com/zhixuli0406/DuDuClaw-OS/pull/21), which must ship before an
OS image embeds platform v1.74.0 or later.

`config.raw.set` asks only when a new account id appears. Editing an existing
`oauth` account in the raw editor (a new token, or a different `provider`) is
not asked again. Details:
[Subscription-Login Risk Disclosure](../features/13-multi-runtime.md#subscription-login-risk-disclosure).

---

## Removed in v1.69.0

### MCP tools

Eight tool names are no longer declared. They do not appear in `tools/list`.
Calling one returns a tool error that names the replacement call. The tool
count went from 249 to 241.

| Removed | Use instead |
|---|---|
| `shared_wiki_ls` | `wiki_ls` with `scope="shared"` |
| `shared_wiki_read` | `wiki_read` with `scope="shared"` |
| `shared_wiki_write` | `wiki_write` with `scope="shared"` |
| `shared_wiki_search` | `wiki_search` with `scope="shared"` |
| `shared_wiki_stats` | `wiki_stats` with `scope="shared"` |
| `shared_wiki_lint` | `wiki_lint` with `scope="shared"` |
| `schedule_task` | `tasks_create` with `schedule="<cron expression>"` |
| `skill_bank_search` | `skill_search` with `source="bank"` |

`shared_wiki_delete` and `wiki_share` were never aliases. They keep their names
and are not affected.

**Employees that still name a removed tool.** An `agent.toml [capabilities]`
tool list, a prompt or a skill may still contain an old name. What an old name
does depends on the list:

| Where the old name is written | Effect after the upgrade |
|---|---|
| `allowed_tools` | The entry matches nothing. An employee whose allowlist names only the old tool loses the capability. Replace it with the new name |
| `denied_tools` | The MCP gate still refuses the equivalent call (for example `wiki_write` with `scope="shared"`). The Claude CLI flag no longer matches, so write the new name to deny it everywhere |
| `approval_required_tools`, `irreversible_tools`, `maybe_irreversible_tools` | The gate still applies to the equivalent call. A `wiki_write` without `scope="shared"` is not affected |
| `scoped_tools` | A task grant is still required for the equivalent call. The grant is requested and recorded under the name written in the list |
| `config.toml [provenance] sensitive_tools` | The gateway still gates the new name in its place |

Everywhere except `allowed_tools` the old name keeps working as a guard, but it
is stale, so replace it with the new name. `duduclaw doctor` has a check for
removed tool names. It reads each employee's `agent.toml [capabilities]` lists,
the prompt files at the root of the employee directory (`SOUL.md`,
`IDENTITY.md`, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, `CONTRACT.toml`), the
Markdown files under `SKILLS/` and `wiki/`, and `config.toml` (`[provenance]`
and `[[ccr.allowed_sources]]`). It does not read prompt text inside scheduled
tasks and automation rules, tool assertions in `evals/` and in playbooks,
`.mcp.json`, or the shared wiki.

Background on the three merged entry points, which behave as before:

- **Wiki.** `wiki_*` takes `scope: "agent" | "shared"`, default `agent`, so
  every existing `wiki_*` call is unchanged. The `.scope.toml` namespace policy,
  `wiki_visible_to` visibility and the author-or-main-agent rule on delete did
  not change.
- **Creating work.** `tasks_create` takes `kind` (`"task"`, the default, or
  `"goal"`) and `schedule` (a cron expression with 5 or 6 fields for recurring
  work, or an RFC3339 instant such as `2026-10-01T09:00:00+08:00` for a
  one-shot wake-up). Both forms need `notify_channel` and `notify_chat_id` to
  deliver a result; a one-shot without them is refused. `kind="goal"` combined
  with `schedule` is refused. The delegation policy check (department and
  hierarchy) runs once at this entry, before any branch.
  `goals_create` (a node in the Initiative, Project, Issue hierarchy) and
  `create_task` (an explicit multi-step `steps` plan for the TaskSpec
  dispatcher) are different tools and were never deprecated.
- **Skill search.** `skill_search` takes `source`: `"all"` (default, hubs and
  the learned skill bank, de-duplicated by skill name), `"github"`, `"hub"` or
  `"bank"`. The learned skill bank is still an empty in-memory stub, so
  `source="bank"` reports an empty store.

### CLI spellings

Each of these spellings still parses, prints one line that names the new
spelling, and exits with code 2. Nothing is executed.

| Removed | Use instead |
|---|---|
| `duduclaw migrate-from <platform>` | `duduclaw migrate from <platform>` |
| `duduclaw audit …` | `duduclaw export audit …` |
| `duduclaw gdpr export <contact>` | `duduclaw export gdpr <contact>` |
| `duduclaw playbook export --agent <agent>` | `duduclaw export playbook --agent <agent>` |
| `duduclaw acp-server` | `duduclaw acp server` |
| `duduclaw expert install <source>` | `duduclaw pack install <source>` |

`duduclaw expert install` and `duduclaw pack install` run the same install
pipeline, so what gets installed does not change. The dashboard's one-click
install, upload install and AI-draft install now call `pack install`.
`duduclaw gdpr erase` and `duduclaw playbook migrate-soul` are unaffected.

### `config.toml [dispatch] judge`

The values `evaluator_only` and `human_only` (and their aliases `evaluator`
and `human`) are removed. The valid values are `mav` (default) and `external`.
The dashboard and `system.update_config` refuse to write the removed values.

A `config.toml` that still contains one of them is handled like this:

| Old value | What the gateway does now | What to do |
|---|---|---|
| `evaluator_only` | Uses `mav` for acceptance. Review is stricter and judge cost goes up. `[dispatch] two_stage_judge` (on by default) still runs the cheap evaluator first and pays for the panel only for a completion candidate | Set `judge = "mav"` |
| `human_only` | Does not fall back to machine acceptance. Every item sent for acceptance stops at `needs_human`, with the pause reason shown as a system problem and instructions for the fix | Set `judge = "mav"`, or `external`. For work that needs a person, use the per-agent `[capabilities] autonomy_level` and `approval_required_tools`. To release stuck work, press 標記完成 (mark done) on the task, or fix the setting and press 重試 (retry); a retry sends the task back to `pending`, clears its stored result summary and claim, and uses an optional note as the instruction for the next round. The round counter keeps counting, and files already written are not removed |

Both values log one warning per process. Once per gateway process and data
directory, when the first piece of work enters acceptance, the gateway writes
one `judge_mode_removed` audit event and one Activity Feed notice. After a
gateway restart it writes them again. The `judge_mode_deprecated` audit event
is no longer produced. `duduclaw doctor` lists the situation: `human_only`
shows as a failure because every item sent for acceptance stops, `evaluator_only`
shows as a warning, and a `config.toml` that cannot be read or parsed shows as a
warning that the check could not be made.

### Before you upgrade

1. Grep your agent prompts, skills and automations for the eight removed MCP
   tool names, and check each `agent.toml [capabilities]` tool list.
2. Grep your scripts, cron entries and systemd units for the six removed CLI
   spellings. A removed spelling now exits with code 2 instead of running.
3. Check `config.toml [dispatch] judge` for `evaluator_only` or `human_only`.
4. Run `duduclaw doctor`. It lists leftover removed tool names and a removed
   judge value.

---

## Removed CLI runtimes

| Runtime | Removed in | Deprecation window | Operator action |
|---|---|---|---|
| Gemini CLI (`gemini`) | v1.75.0 | Deprecated in v1.67.0 | Choose a supported runtime and compatible model explicitly |
| Antigravity (`antigravity`, `agy`) | v1.77.1 | None | Choose a supported runtime and compatible model explicitly |

Saved employee `[runtime] provider` and fallback, judge, `[team.roles]` and
`config.toml [runtime] utility_provider` values naming a removed runtime are
refused with an error asking the operator to choose another runtime and model.
The system never migrates these settings to another vendor automatically.
`gemini-*` model ids no longer select any CLI runtime. The runtime catalog,
execution paths, dashboard options and Docker image exclude the removed CLIs.

The Gemini API provider (`generateContent`), `GEMINI_API_KEY` and
OpenAI-compatible Gemini presets remain supported. Leftover local CLI files
and permission rules previously added to the OS user's CLI settings remain
on disk; the operator may clean them up manually.

## Still deprecated

### Pack legacy formats

The legacy pack manifests `expert.toml`, `team.toml` and the industry pack
directory layout are deprecated. v1.69.0 still reads all of them, and none was
removed. They will be removed together with the rewritten premium templates in
a later version. No version number is set.

**Why they stay.** The newer `pack.toml` format can currently only be
installed as a job preset (`kind = "preset"`). `duduclaw pack install`
accepts a `pack.toml` team or industry pack when it reads the file, but then
hands the directory to the expert installer, which recognises a pack only by
its `expert.toml` (or a Claude Code plugin or a lone Agent Skill) and rejects
anything else. Until the installer can install a `pack.toml` team or industry
pack, the old formats cannot be taken away. See
[Build your own pack](build-your-own-pack.md) for what to author today.

| Today | Status |
|---|---|
| `expert.toml` (team and industry packs) | Keep using it. Deprecated, still read |
| `team.toml`, industry pack directories | Deprecated, still read verbatim, no disk migration |
| `pack.toml` with `kind = "preset"` | Current format for job presets |
| `pack.toml` with `kind = "team"` or `"template"` | Can be read and inspected (`pack inspect`), cannot be installed yet |

## Names withdrawn from the deprecation list

These names were announced as deprecated and are now plain, supported
behaviour.

| Name | Status | Reason |
|---|---|---|
| `duduclaw data-migrate` | Kept as a hidden alias of `duduclaw migrate data` | Shipped DuDuClaw OS images run `duduclaw data-migrate --run` from a boot unit on a read-only root filesystem, so the spelling has to keep working. Use `duduclaw migrate data` in new scripts |
| `duduclaw migrate` | Supported, means `duduclaw migrate schema` | The bare form is documented behaviour |
| `duduclaw export --out …` | Supported, means `duduclaw export data --out …` | The bare form is documented behaviour |
| `duduclaw acp` | Supported, means `duduclaw acp client` | The bare form is documented behaviour |
| `duduclaw expert list` | Kept | It lists the installed records, which is not what `duduclaw pack list` shows (installed packs and what is available to install) |
| `preset.toml` | Kept | It is the storage format of job presets. `pack.toml` with `kind = "preset"` is the other way to author one |

`preset_bindings.toml` (which employee has which preset applied) is state, not
a pack format, and was never deprecated. The authoring commands `expert pack`,
`publish`, `export`, `convert-teams`, `hooks` and `remove` stay under
`duduclaw expert`.

## MFA (Unreleased)

MFA phase 1 adds conditional response shapes to `/api/login` and `/api/otp/verify`. Unenrolled accounts with enforcement off retain the exact `access_token`, `refresh_token`, `user` shape. Enrolled accounts receive only `mfa_required`, `challenge_id`; enforced unenrolled accounts receive only `mfa_enrollment_required`, `enrollment_token`. Clients must complete `/api/mfa/verify` or `/api/mfa/enroll/*` before treating login as successful. No legacy endpoint is removed. Dashboard support follows in phase 2.
