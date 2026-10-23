# Importing AI staff from other platforms

DuDuClaw can take an agent you already defined somewhere else — a Doubao partner package, an OpenClaw or Hermes folder, an `AGENTS.md`, a Claude Code subagent, a GitHub Copilot custom agent, a Dify app export — and turn it into an AI staff member. Persona, working rules, notes about the people it serves, memory and skills each land in the DuDuClaw slot that actually reads them.

Also in [繁體中文](zh-TW/agent-config-import.md) and [日本語](ja-JP/agent-config-import.md).

## Two ways in

**Dashboard.** AI team page → **Upload**. Pick a `.zip`, or a single `.md` / `.yml` / `.yaml` file (50 MB at most). The dashboard installs only what was uploaded through this button; importing a folder on the server is a terminal command. DuDuClaw scans it and shows a plan: which staff will be created, where every file goes, which skills stay off and why. Nothing is written until you press **Import**. The result screen lists what really happened. Only administrators can upload and import; other roles see a note saying so, with the upload button disabled.

**Terminal.**

```bash
duduclaw pack install ./報告顧問.zip --dry-run   # plan only, writes nothing
duduclaw pack install ./報告顧問.zip             # import
duduclaw pack install ./reviewer.agent.md            # a single file works too
duduclaw pack install ~/.openclaw                     # a folder works too
```

`--rename` imports under an `-imported` suffix when a staff member with the same id already exists (otherwise the clash is reported and that staff member is skipped). When the same content was imported before, add `--replace <slug>` to replace the earlier version (plus `--accept-edits` when its staff were edited since), or `--as-copy` to keep both; see "Importing the same thing again" below. `duduclaw pack install` is the same command that installs expert packs; there is no separate import command. `duduclaw migrate from openclaw|hermes` still exists for moving a whole platform, including channel tokens and schedules (see [migrate-from.md](migrate-from.md)), and it now uses the same file mapping described below.

## Supported formats

| Format | How it is recognised | One staff member per |
|---|---|---|
| Doubao partner package (豆包「導出夥伴安裝包」) | `manifest.json` with `units`, or `agents/<ref>/agent.yml` + `persona/` | `agents/<ref>/` |
| OpenClaw folder | `openclaw.json` (also `moltbot.json`, `clawdbot.json`) | entry in `agents.list[]` or `agents.entries` |
| OpenClaw / SoulSpec persona workspace | `SOUL.md` at the top | folder |
| Hermes folder (`~/.hermes` or one profile) | `config.yaml` + `SOUL.md` or `memories/` | folder |
| Dify app export | one `.yml` with `app:` and `kind: app` (or `version`) | file |
| Claude Code subagents, Copilot custom agents | `.claude/agents/*.md`, `.github/agents/*.md`, `*.agent.md`, `*.chatmode.md`, or one `.md` with `name` + `description` frontmatter | file |
| Instruction files | `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `AGENT.md`, `.github/copilot-instructions.md` | folder |

Expert packs (`expert.toml`), Claude Code plugins and single Agent Skills keep working as before; see [Expert packs](../features/32-expert-packs.md). Anything else is refused with a listing of what the upload contained. Nothing is half-imported.

## Where each file goes

| Source | Lands in | Notes |
|---|---|---|
| `SOUL.md`, Doubao `persona/SOUL.md` | `SOUL.md` | Read-only to the staff member. |
| `IDENTITY.md` | `IDENTITY.md`, plus name and emoji in `agent.toml` | OpenClaw `- Name:` / `- Emoji:` lines and the Doubao `## 簽名` section are both read. |
| `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, subagent body, Dify `pre_prompt` | `INSTRUCTIONS.md` (new) | Loaded into every prompt after SOUL and IDENTITY. Read-only to the staff member. |
| `USER.md` (Hermes: `memories/USER.md`) | knowledge page "服務對象" | Core layer, so it is in every prompt. |
| `TOOLS.md` | knowledge page "工具備註" | Notes only. It grants no tool. |
| `HEARTBEAT.md` | knowledge page "定期檢查清單" | Not scheduled automatically; create a schedule in the dashboard if you want one. |
| `BOOT.md`, Dify opening statement | knowledge page | Searched on demand. |
| `MEMORY.md`, `memory/*.md` (Hermes: `memories/MEMORY.md`) | memory, one fact per bullet (or per paragraph / `§` entry) | Low trust (`import` origin) with the source file recorded, so the whole file can be withdrawn later. |
| `skills/<name>/SKILL.md` | the staff member's skills | See "Skills" below. |
| MCP servers in a subagent | not imported | A server is something DuDuClaw would run or connect to, so an import never adds one. Each one is listed; add the ones you trust in the dashboard. |
| `model` | `[model] preferred` | Claude ids only. Any other model (`doubao-*`, `gpt-*`, …) is reported and the default model is used. |
| `tools` / `disallowedTools` | `allowed_tools` / `denied_tools` | Narrowing only. Names DuDuClaw does not know are dropped and listed. An allowlist always keeps DuDuClaw's own tools (`mcp__duduclaw__*`: knowledge, memory, tasks, delegation). If a source restricted tools but none of the names map, running commands, writing files and fetching web pages are switched off rather than left open. |
| `BOOTSTRAP.md`, `CLAUDE.local.md` | skipped | One-time setup and personal files. |
| anything else | kept under `<home>/imported/<id>/source/` | Listed in the report. Only the files the import read or listed are kept; credential files, vendor config files (`openclaw.json`, Hermes `config.yaml`), session logs and hidden folders are not. |

When several instruction files are present, the first one in this order wins and the rest are listed as skipped: `AGENTS.md` → `CLAUDE.md` → `GEMINI.md` → `AGENT.md` → `.github/copilot-instructions.md`. A file whose only content is `@AGENTS.md` is read through to that file.

The staff member's id is the ASCII form of the source name. When the name has no ASCII form (a Chinese partner name), the id is `<format>-<ref>`, for example `doubao-a1`; the display name stays Chinese. Channels call staff by `@display name` and `@id`, so a new staff member never takes a name, trigger or id someone on the team already answers to: the id gets `-imported` and the display name gets `（<id>）`, and the report says so.

## Skills

Every skill is rewritten into a valid Agent Skill: the name becomes lowercase letters, digits and hyphens (equal to the folder name), and the original label, often Chinese, is kept under `metadata.label`. A skill whose only name is Chinese gets `skill-<8 hex>`. Broken frontmatter (common in Doubao exports) is replaced; the body is kept as is.

A skill stays **off** until an administrator approves it when it

- ships runnable scripts or programs (`.py`, `.sh`, `.js`, …, a `#!` file, an executable file, or binary content other than images and PDFs; `.svg`, `.html`, `.vbs`, `.wsf` and `.command` count as runnable too), or
- depends on tools DuDuClaw does not have: Feishu / Lark documents and APIs, Doubao built-ins such as 「構建網頁應用」, Coze plugins, Microsoft 365 Graph.

Those skills wait outside the staff member's folder. The import files one approval request in the dashboard inbox, tied to a fingerprint of each waiting skill: a skill whose files change before it is applied is not enabled. After approving it, press **Apply** on the AI team card (or run `duduclaw expert skills <id>`); the skills are copied in, never overwriting a skill of the same name. Denied or expired requests keep them off. A skill that only mentions such a tool a few times is installed and the report says so.

A skill is not installed at all when its SKILL.md fails the content scanners or any of its text files fails the prompt-injection check.

## Safety

- The import is the only review point for SOUL and INSTRUCTIONS, because staff members cannot edit them afterwards. Both, and every other imported text, go through the prompt-injection guard; SOUL and INSTRUCTIONS also go through the skill security scanner. A refused SOUL skips that staff member; a refused instruction file is left out and reported.
- Credentials are never imported: `.env`, `auth.json`, `auth-profiles.json` and similar files are not read or kept, channel tokens in `openclaw.json` are ignored here, and Dify secret variables are dropped.
- An OpenClaw workspace path that points outside the uploaded folder is not followed.
- Zip files: `__MACOSX/`, `._*`, `.DS_Store`, `Thumbs.db` and `desktop.ini` are skipped. File names are read as UTF-8 (flagged, or unflagged as macOS writes them), then GBK or Big5 for archives made on Chinese Windows. If no encoding fits, or GBK and Big5 are equally likely, the upload is refused with the raw bytes in the message rather than written with garbled names. Symbolic links are not extracted. Names must mean the same thing on every system, so a `:` in a name, a trailing dot or space, Windows device names (`CON`, `NUL`, `COM1`, …) and two entries that differ only in upper/lower case are refused. The size, entry-count and compression-ratio limits of expert packs apply.
- Credential and hidden files inside a skill (`.env`, `token.json`, `*.pem`, `credentials/`, dot files) are left out of the copy and listed.
- An AI staff member cannot run the import: `duduclaw pack install` and `duduclaw expert skills` refuse to run inside an employee's session.
- Staff members cannot write their own `INSTRUCTIONS.md` through Write, Edit, MultiEdit, Bash or `office_script`, the same protection SOUL.md and CONTRACT.toml have.

## Undo

Remove the import from the AI team page (or `duduclaw expert remove <id>`). That deletes the staff members it created, the waiting skills, and the kept original under `<home>/imported/<id>/`. Imported memory facts live in the shared memory store; withdraw them with the command shown under "Details" in the report (the source id never contains a path on your machine):

```bash
duduclaw memory forget-source --agent <id> --session "import:agent-import:<import id>/MEMORY.md"
```

## Importing the same thing again

When the same content (the same import name) was imported before, the dashboard does not just fail; it asks:

- **Replace the earlier version**: the earlier version moves to a backup first, then the new one is imported under the same ids and names. Staff folders move to `<home>/agents/_trash/<id>_<time>` (the place removed staff go), and skills, knowledge pages, the original files and the import record move to `<home>/experts/_replaced/<import name>_<time>/`. Every check that can fail (format, whether the upload is the same pack, the staff limit) runs before anything moves. If the new version fails part way, what it already wrote goes to `failed-new/` inside the backup, the earlier version moves back and its org records (supervisor, department) are restored; approvals the new version filed are withdrawn. When a replace succeeds, skill and hook approvals of the earlier version still waiting in To-do are withdrawn. Replaced staff that the new version does not bring back also lose their org record.
- **Import as a copy**: the earlier version stays; this one is imported under a new name such as `<import name>-2`, and staff whose ids are taken get an `-imported` suffix.

DuDuClaw records a content fingerprint of every file of every imported staff member. When you choose **Replace** and someone edited a staff member after the import (personality, working rules, knowledge pages, memory files, added files and so on), the dialog first lists what changed and asks you to press **I understand, replace anyway**; without that confirmation the server refuses. Files DuDuClaw maintains itself (`.mcp.json`, `.claude/`, lock files `*.lock`, the knowledge-base index and log) do not count as edits. Staff imported before this version have no fingerprint, so file modification time is used instead and the dialog says so.

## When an import fails

The dialog explains the reason in plain language, suggests what to do, and shows an error code (for example `import.unrecognized`). If you need help, send us that code. The common ones:

| Error code | Meaning | What to do |
|---|---|---|
| `import.unrecognized` | The format was not recognised | The dialog lists the top level of the archive and any importable folder found deeper. The importer steps through at most two wrapper folders that hold nothing but one folder (`__MACOSX/` and `.DS_Store` do not count); for anything deeper, zip that folder directly. |
| `import.already_imported` | Imported before | Choose **Replace** or **Import as a copy**. |
| `import.replace_edited` | Staff to be replaced were edited after the import | Review the listed edits and confirm again. |
| `import.nothing_imported` | No staff member was imported | The dialog gives each staff member's reason; for an id clash, use **Import as a copy**. |
| `import.quota_exceeded` | Over the license's AI staff limit | Refused before anything is written. The limit is the same one used when creating AI staff. |
| `import.too_large`, `import.zip_bomb`, `import.too_many_entries` | Too big, grows abnormally when unpacked, or too many files | The limit is 50 MB, unpacked size included. |
| `import.name_encoding_ambiguous`, `import.name_encoding_unknown`, `import.case_duplicate`, `import.unsafe_path` | A file name problem | Re-zip with a tool that writes UTF-8 names, and rename files that differ only in case or are unsafe. |
| `import.timeout` | Not finished within 300 seconds | Stopped with nothing changed. Try later or split it into smaller packages. |
| `import.unknown` | Something unexpected | **Details** holds the importer's own message; send it with the code. |

When the upload itself fails (over 50 MB, not a zip, not an administrator), the dialog explains that too.

## Known limits

- Doubao packages have no public specification. The importer follows the one real sample it was built from and reports unknown manifest fields and unit types instead of guessing.
- Dify workflow graphs are not converted. Only the first LLM node's system prompt is taken; rebuild the flow in DuDuClaw. Dify knowledge bases are not part of the export, so their contents must be uploaded again.
- Rules folders (`.cursor/rules`, `.claude/rules`, `.agents/rules`, Windsurf, Cline, Kiro) are listed but not applied yet.
- Report lines are shown in the dashboard's language. The importer's own technical notes under "Details" (file names, reasons quoted from the source) stay in Traditional Chinese.
