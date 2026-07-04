# Running multiple DuDuClaw instances on one machine (Plan A)

You can run several independent DuDuClaw instances on a single machine, sharing
one binary, by giving each instance its own **state root**, **port**, and
**instance name**. This is "Plan A" — the lightest isolation model. For stronger
isolation (separate OS users, or containers) see the alternatives at the bottom.

## The three env vars

| Env var | Purpose | Must differ per instance |
| --- | --- | --- |
| `DUDUCLAW_HOME` | State root — config, SQLite DBs, `bus_queue.jsonl`, `events.db`, models, shared wiki, secrets, cron. Default `~/.duduclaw`. | **Yes** |
| `DUDUCLAW_PORT` | Gateway HTTP/WS port. Default `18789`. | **Yes** |
| `DUDUCLAW_INSTANCE` | Short instance name (`[a-z0-9-]`). Namespaces the **global MCP registration** key in `~/.claude/settings.json` (`duduclaw` → `duduclaw-<name>`) so instances don't overwrite each other. | Recommended |

Every subsystem resolves its state root through a single canonical helper
(`duduclaw_core::duduclaw_home()`), so setting `DUDUCLAW_HOME` relocates *all*
per-instance state — no path silently leaks back to `~/.duduclaw`.

## Example — two instances

```bash
# Instance "work"
DUDUCLAW_HOME=~/dd-work  DUDUCLAW_PORT=18789 DUDUCLAW_INSTANCE=work \
  duduclaw run --yes

# Instance "play"
DUDUCLAW_HOME=~/dd-play  DUDUCLAW_PORT=18790 DUDUCLAW_INSTANCE=play \
  duduclaw run --yes
```

When each instance registers its MCP server, it writes a namespaced entry into
the shared `~/.claude/settings.json`, carrying its own env into the launch spec
so the Claude-CLI-spawned `duduclaw mcp-server` connects back to the right
instance:

```jsonc
{
  "mcpServers": {
    "duduclaw-work": {
      "command": "/opt/homebrew/bin/duduclaw",
      "args": ["mcp-server"],
      "env": { "DUDUCLAW_HOME": "/Users/you/dd-work", "DUDUCLAW_PORT": "18789", "DUDUCLAW_INSTANCE": "work" }
    },
    "duduclaw-play": {
      "command": "/opt/homebrew/bin/duduclaw",
      "args": ["mcp-server"],
      "env": { "DUDUCLAW_HOME": "/Users/you/dd-play", "DUDUCLAW_PORT": "18790", "DUDUCLAW_INSTANCE": "play" }
    }
  }
}
```

## Must-differ checklist

- [ ] `DUDUCLAW_HOME` — distinct directory per instance
- [ ] `DUDUCLAW_PORT` — distinct port (and the MCP HTTP port if you run `http-server --bind`)
- [ ] `DUDUCLAW_INSTANCE` — distinct name (namespaces the MCP registration)
- [ ] launchd / systemd **service label** — distinct per instance
- [ ] **models directory** — point both `DUDUCLAW_HOME/models` at one shared,
      read-only location (symlink) to avoid duplicating multi-GB GGUF files

## Shared vs isolated state

- **Isolated** by `DUDUCLAW_HOME`: config, all SQLite DBs, bus queue, events,
  cron, shared wiki, JWT/keyfile, evolution state.
- **Shared** under the same OS user: `~/.claude` (Claude CLI OAuth sessions +
  MCP settings). Instances coexist there via the namespaced MCP key, but they
  still draw on the **same OAuth subscription accounts** — heavy concurrent use
  can cause rotation / rate-limit contention. Give each instance its own
  accounts in its `config.toml`, or use per-account profiles
  (`~/.claude/profiles/<name>`), to avoid interference.

## When to choose a stronger model

- **Separate OS users** — each instance runs under its own account, so `~/.duduclaw`
  *and* `~/.claude` (OAuth) are naturally isolated with filesystem-level
  boundaries. Zero code reliance on env vars; still needs distinct ports.
- **Containers** (Docker/Podman) — full filesystem + network-namespace
  isolation; each container can reuse port `18789` internally and map to
  distinct host ports. Note: on macOS, Linux containers have no Metal, so local
  GGUF inference falls back to CPU (keep inference on the host if you need GPU).
