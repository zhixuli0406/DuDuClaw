# Painless migration from OpenClaw / Hermes / paperclip

`duduclaw migrate-from` moves an existing OpenClaw, Hermes, or paperclip setup into
DuDuClaw with one command. It defaults to **preview mode**: it only prints what would
be imported, what would be skipped, and why. Once the plan looks right, add `--apply`
to actually write anything.

```bash
# Preview (writes nothing)
duduclaw migrate-from openclaw

# Apply after reviewing the plan
duduclaw migrate-from openclaw --apply
```

## Command

```
duduclaw migrate-from <openclaw|hermes|paperclip> [--source <path>] [--apply] [--rename]
```

| Flag | Effect |
|---|---|
| (none) | Preview the migration plan; writes nothing. |
| `--source <path>` | Source directory. openclaw/hermes have defaults; **paperclip requires it**. |
| `--apply` | Actually perform the write. |
| `--rename` | On an agent-id collision, import under an `-imported` suffix instead of skipping. |

Every item is tagged with a status:

- `IMPORTED` — imported (or will be).
- `PARTIAL` — partially imported, or needs manual confirmation (e.g. a non-Claude model).
- `SKIPPED(reason)` — skipped, with a reason (source file missing, parse failure, security block, etc.).
- `CONFLICT(reason)` — the target already has a value; the existing setting is left untouched.

The overall result rolls up to `COMPLETE` / `DEGRADED` / `PARTIAL`. After an apply run,
a full report is written to `~/.duduclaw/imported/<platform>/migration-report.md`. Every
token value is shown masked as "first 4, last 4" — never in plaintext on screen or in
the report.

## Per platform

### OpenClaw (`~/.openclaw`)

```bash
duduclaw migrate-from openclaw            # defaults to ~/.openclaw
duduclaw migrate-from openclaw --source /path/to/.openclaw --apply
```

Reads `openclaw.json` (JSON5) and imports:

- **Agents**: `agents.list[]` (or a single default `main`), each with its workspace
  persona (`SOUL.md`) and memory (`MEMORY.md` / `USER.md` / bullet entries from
  `memory/*.md`).
- **Channel tokens**: `channels.telegram.botToken`, `channels.discord.token`,
  `channels.slack.botToken` + `appToken` (written encrypted into config.toml). WhatsApp
  is a linked device and technically cannot be transferred, so it is `SKIPPED`.
- **Model**: `agents.defaults.model.primary` (with the `anthropic/` prefix stripped).
- **Anthropic API key**: read from the `env` section and `~/.openclaw/.env`. Keys for
  other providers are `SKIPPED`.
- **Cron**: the legacy `cron/jobs.json` (parsed defensively). The newer SQLite cron
  schema is unvalidated and `SKIPPED`.
- **Skills**: `SKILL.md` folders located by OpenClaw's own precedence order (scanned
  before install).

The legacy directory names `~/.moltbot` and `~/.clawdbot` are also supported.

### Hermes (`~/.hermes`)

```bash
duduclaw migrate-from hermes --apply
# migrate a non-active profile:
duduclaw migrate-from hermes --source ~/.hermes/profiles/<name> --apply
```

Hermes is a single-agent platform, so this produces one DuDuClaw agent (id `hermes`).
Imports:

- **Model**: `model.default` from `config.yaml`.
- **Channel tokens** (from `.env`): `TELEGRAM_BOT_TOKEN`, `DISCORD_BOT_TOKEN`,
  `SLACK_BOT_TOKEN` + `SLACK_APP_TOKEN`. `EMAIL_*` channels are not yet supported in v1
  and are `SKIPPED`.
- **Persona / memory**: `SOUL.md`, `memories/MEMORY.md`, `memories/USER.md`.
- **Cron**: `cron/jobs.json` (parsed defensively).
- Only the **active profile** is migrated; other profiles are listed as `SKIPPED`, with
  a prompt to migrate each one individually via `--source`.

### paperclip: via the official export

paperclip's data lives in an embedded PostgreSQL instance, and DuDuClaw does not connect
to that database directly. Export from the paperclip side first:

```bash
paperclipai company export <company-id> --out ./export \
  --include company,agents,projects,issues,tasks,skills

duduclaw migrate-from paperclip --source ./export --apply
```

`--source` is **required** (omitting it prints the instructions above). Imports:

- **Agents**: frontmatter (`name/title/reportsTo/skills`) from `agents/<slug>/AGENTS.md`
  becomes a DuDuClaw agent, and the body becomes `SOUL.md`. `reportsTo` maps directly to
  `reports_to` (agents are created in topological order by hierarchy; a detected cycle
  falls back to no superior for every agent involved and is marked `PARTIAL`).
- **Tasks**: `tasks/<slug>/TASK.md` becomes a Task Board entry; `recurring` becomes cron.
- **Skills**: `skills/<slug>/SKILL.md` goes into the agent's SKILLS/ (scanned first).
- **COMPANY.md** becomes a shared wiki page at `shared/wiki/imported/paperclip-company.md`.
- The official paperclip export format **contains no secrets** (channel tokens, API
  keys, DB ids), so channels and keys are always `SKIPPED`.

## Security and data preservation

- **Skills are scanned before install**: every `SKILL.md` first passes through
  duduclaw-security's prompt-injection scanner (6 rule categories). A hit fails closed:
  nothing is installed, and it is marked `SKIPPED(security)`. Imported skills keep
  `skill_auto_activate` at `false` (the safe default).
- **Never overwritten**: an existing agent with the same id is `SKIPPED` (or use
  `--rename`); a channel token / API key already present in config.toml is a
  `CONFLICT`, and the original value is left untouched.
- **Tokens land encrypted**: channel tokens and API keys are encrypted with AES-256-GCM
  before being written into config.toml — never in plaintext.
- **No data loss**: v1 does not parse conversation history into `sessions.db`, but
  `--apply` archives the original session / conversation files verbatim to
  `~/.duduclaw/imported/<platform>/raw/` for later reference.

## v1 non-goals (honest boundaries)

1. Ingesting conversation history into the database (verbatim archiving only).
2. OpenClaw's newer SQLite cron / auth-profiles (schema unvalidated).
3. WhatsApp linked-device credentials (bound to the device, cannot be transferred).
4. Hermes profiles other than the active one (migrate each individually via `--source`).
5. Reading paperclip's Postgres directly (use the official export instead).
6. External memory backends (Honcho / Mem0 / QMD / LanceDB).

## FAQ

**Q: Does preview mode change anything?**
No. Without `--apply` nothing is written and no channel is started.

**Q: I hit a CONFLICT partway through — what do I do?**
CONFLICT means the target already has a value, and it was left alone to protect your
existing configuration. To replace it with the imported value, manually remove the old
value from config.toml first and rerun, or import as a separate agent with `--rename`.

**Q: What happens with a non-Claude model?**
It is kept as-is in `[model] preferred` and marked `PARTIAL`, prompting you to manually
confirm which runtime it maps to (codex / gemini / openai_compat). DuDuClaw will not
guess on your behalf.
