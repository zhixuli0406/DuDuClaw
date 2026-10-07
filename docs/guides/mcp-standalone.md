# Standalone MCP server

DuDuClaw's MCP server can run on its own, without the DuDuClaw gateway, and
give Claude Code, Codex or Cursor a persistent memory store and a Markdown wiki
that survive between sessions. This page covers what you get, the setup, and
how it relates to the full platform.

## Setup

You need Node.js (for `npx`). Nothing is installed globally.

### Claude Code

```bash
npx duduclaw mcp init --client claude-code
```

The command creates `~/.duduclaw` if it is missing, issues a key and asks
before running `claude mcp add` for you (user scope). Add `--yes` to skip the
question. Start a new Claude Code session and run `/mcp`: `duduclaw` should be
listed as connected.

If Claude Code already has a `duduclaw` server, the command looks at it
first. An entry written by an earlier `mcp init` is replaced. Any other entry
(another key, another server under the same name, a configuration file it
cannot read) stops the command before a key is issued and shows that entry
with its secrets masked; add `--replace` to replace it. Before replacing, the
old entry is saved to `~/.duduclaw/mcp_init/claude-duduclaw-<time>.json`
(readable by you only), and if the new entry cannot be added the old one is
put back.

If the Claude Code CLI is not installed, or you answer no, the command prints
the exact `claude mcp add …` line to run yourself.

### Codex

```bash
npx duduclaw mcp init --client codex
```

It prints a block to paste into `~/.codex/config.toml`:

```toml
[mcp_servers.duduclaw]
command = "npx"
args = ["-y", "duduclaw@<version>", "mcp-server"]
env = { DUDUCLAW_MCP_API_KEY = "ddc_refresh_prod_…" }
```

`<version>` is the version that ran `mcp init` (what `duduclaw --version`
prints): the snippet pins it, so the server the client starts is
the one that issued the key. Run `mcp init` again after upgrading.

### Cursor

```bash
npx duduclaw mcp init --client cursor
```

It prints the `~/.cursor/mcp.json` entry. If the file already lists other
servers, copy only the `duduclaw` object into its `mcpServers`.

### Notes on the command

- With no `--client` it prints all three snippets and registers nothing with
  any client. Like every run, it still issues a new key (valid 90 days).
- Each `--client` value is a separate key with its own memory namespace and
  wiki: a key from `--client print` writes to `external/standalone-print`,
  not to `external/standalone-claude-code`. Use the value of the client you
  will connect.
- When `duduclaw` is installed globally (`npm install -g duduclaw`), the
  snippets point at the installed binary instead of `npx`. The path is the
  one the binary was started from, with links left as they are; when it
  belongs to one Node version (nvm, volta), run `mcp init` again after
  switching versions.
- The printed `claude mcp add` line is quoted for bash and zsh (single
  quotes). The Windows build quotes it for PowerShell and cmd (double
  quotes).
- If you run it with `DUDUCLAW_HOME` set, the snippets carry the same
  `DUDUCLAW_HOME`, so the server opens the same data directory.
- The key is printed once. DuDuClaw stores only its hash, in
  `~/.duduclaw/mcp_tokens.db`. The client keeps the key in plain text in its
  own configuration (`~/.claude.json`, `~/.codex/config.toml`,
  `~/.cursor/mcp.json`); anyone who can read that file can use the key until
  it expires or is revoked. It is valid for 90 days; run `mcp init` again
  for a new one, then revoke the old one with
  `duduclaw mcp revoke-token <jti>` (the command lists earlier active keys
  for the same client). `duduclaw mcp list-tokens` shows all keys.
- The command refuses to run inside a DuDuClaw AI employee's session, and
  names the variable that gave it away. A `DUDUCLAW_MCP_API_KEY` exported in
  your own shell counts too: unset it and run the command again.

## What you get

The key carries four scopes: `memory:read`, `memory:write`, `wiki:read`,
`wiki:write`. `tools/list` shows exactly the 24 tools those scopes can call:

| Area | Tools |
|---|---|
| Memory | `memory_store`, `memory_search`, `memory_read`, `memory_fetch_batch`, `memory_get_history`, `memory_get_at`, `memory_alias_add`, `memory_alias_list`, `memory_improve` |
| User profile | `user_profile_record`, `user_profile_get`, `user_code_profile` |
| Code map | `code_map` (symbol map of the directory given in `root`, or the server's working directory) |
| Wiki | `wiki_write`, `wiki_read`, `wiki_ls`, `wiki_search`, `wiki_stats`, `wiki_lint`, `wiki_graph`, `wiki_export`, `wiki_dedup`, `wiki_rebuild_fts`, `wiki_share` |

Where the data lives:

- Memory: `~/.duduclaw/memory.db`, namespace `external/standalone-<client>`
  (for example `external/standalone-claude-code`). Each client set up with
  its own `mcp init` has its own namespace.
- Wiki: `~/.duduclaw/agents/standalone-<client>/wiki/`.
- Shared wiki (`~/.duduclaw/shared/wiki/`): `wiki_share` copies a summary of
  one of your pages there as `sources/standalone-<client>--<page>.md`, with
  your client id as the author. `wiki_write` with `scope="shared"` is refused
  (`-32003`): shared-wiki writes are made as an AI employee, and this key is
  not one. The shared reads (`wiki_ls`, `wiki_read`, `wiki_search`,
  `wiki_stats`, `wiki_lint` with `scope="shared"`) show only the pages every
  caller may see: no `departments/<dept>/` page and no namespace limited
  with `visible_to_departments`.

The key is an external key. That keeps the wiki in the client's own directory
and stops a client from passing another namespace or agent id; it also means
the key can only ever hold the scopes an external client may have
(`memory:*`, `wiki:*`, `messaging:send`). `mcp init --scopes` refuses anything
else.

Tools outside these scopes are not listed, and neither are the tools that
act for an AI employee (next section). Calling one by name is refused by the
server (`-32003`); the listing follows the permission check, it does not
replace it.

### What is not included, and why

Measured on 2026-10-07 on a fresh data directory with the 1.70.1 binary, a key
holding every non-admin scope, each tool called once. The first three rows
describe what those tools did; the server now refuses them to every key that
is not an AI employee:

| Scope or tools | Result without the gateway |
|---|---|
| `working_state_get` / `_set` / `_clear` / `_handoff` | Refused (`-32003`). They act for the AI employee that runs the server (`unknown agent: dudu` when there is none) |
| `memory_search_by_layer`, `memory_successful_conversations`, `memory_episodic_pressure`, `memory_consolidation_status` | Refused (`-32003`). They read the default employee's memories, not the client's |
| `shared_wiki_delete`, `wiki_namespace_status`, `canvas_push`, `canvas_clear` | Refused (`-32003`). They judge or act as the default employee |
| `messaging:send` (`send_message`, `send_photo`, `send_sticker`, `synthesize_speech`, `transcribe_audio`) | Need channels configured in `config.toml` (`Unknown channel`) or an external speech service |
| `mail:read` / `mail:send` | Cannot be granted to an external key; `mail_*` are refused to it like the rows above. The mail worker runs in the gateway |
| `team:handoff` | Cannot be granted to an external key; needs a task from the gateway's goal loop |
| `odoo:*`, `notion:*`, `google:*`, `github:*` | Need an integration configured through the dashboard |
| `discovery:execute` | `discovery requires an explicit signed caller identity` |
| `skill:execute` (`office_script`) | Cannot be granted to an external key; writes into an AI employee's directory |
| `identity:read`, `files:read` | Work, but cannot be granted to an external key; their data (person registry, `~/.duduclaw/attachments`) is set up through the platform |
| `fork:execute`, `os:native`, `recording`, `db:read` | Need a per-employee capability switch |
| Everything else (`tasks_*`, `web_fetch_cached`, `agent_*`, …) | `Insufficient scope: Admin required` |

## Moving to the full platform

`duduclaw run` starts the gateway and its dashboard on the same data
directory, and the standalone key keeps working. What carries over and what
does not:

- Your AI employees do not see the standalone memory: an employee reads its
  own namespace (its id), the standalone client reads
  `external/standalone-<client>`.
- The standalone wiki stays in `agents/standalone-<client>/wiki/`. That
  directory has no `agent.toml`, so the gateway treats it as storage, not as
  an employee: it writes no MCP configuration there, and no employee can be
  created under a name starting with `standalone-`. Pages shared with
  `wiki_share` are in the shared wiki, which employees can read.
- Gateway-only tools appear for employees, not for the standalone key. To give
  a client more, issue a different key with
  `duduclaw mcp issue-refresh-token`.

## Troubleshooting

- `MCP authentication failed: DUDUCLAW_MCP_API_KEY environment variable not set. Run: duduclaw mcp init --client claude-code`:
  the client started the server without the key. Re-run `mcp init` or add
  the `env` entry.
- `API key not found in registry`: the key was revoked, or the server is
  reading a different data directory than the one `mcp init` wrote to (check
  `DUDUCLAW_HOME`).
- `API key expired`: keys last 90 days. Run `mcp init` again.

## Publishing to the MCP Registry

The registry metadata is `distribution/registries/mcp/server.json`; the
publish steps (owner only, after each release) are in
[`distribution/registries/README.md`](../../distribution/registries/README.md).
