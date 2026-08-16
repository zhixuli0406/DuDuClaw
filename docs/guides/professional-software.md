# Professional software integration: Photoshop and AutoCAD

This guide explains how to let a DuDuClaw AI employee operate Adobe Photoshop and AutoCAD. The approach is to mount community-maintained MCP servers through a per-agent `.mcp.json`, then wrap the high-risk tools with capability governance — DuDuClaw does not write drivers for these applications itself. Adding support for another professional application doesn't require a new DuDuClaw release; swapping in a different MCP server swaps in a different set of tools.

> Both MCP servers are third-party, unofficial projects (both MIT licensed). The arbitrary script execution interfaces they expose — Photoshop's ExtendScript, AutoCAD's AutoLISP — are effectively a remote code execution (RCE) surface. Read the "Risk disclosure" and "Capability governance" sections on this page first; do not install and run either server without the guardrails described there.

## Support matrix

| Software | MCP server | Platform | Connection | Requirements | RCE surface |
|------|-----------|------|----------|------|--------|
| Photoshop | `@alisaitteke/photoshop-mcp` (npm) | macOS + Windows | macOS AppleScript / Windows COM, unified through the ExtendScript API | Photoshop installed (2012–2025+), Node.js (`npx`) | `photoshop_execute_script` (arbitrary ExtendScript) |
| AutoCAD (File IPC) | `puran-water/autocad-mcp` (Python) | **Windows only** | temp files + `PostMessageW` injection + AutoLISP dispatcher | Windows 10/11, AutoCAD LT 2024+, Python (`uv`) | `execute_lisp` inside the `system` tool (arbitrary AutoLISP) |
| AutoCAD (ezdxf) | same repo, `AUTOCAD_MCP_BACKEND=ezdxf` | **Cross-platform** (Win/mac/Linux/WSL) | headless, reads and writes DXF directly, never touches the AutoCAD process | Python (`uv`), **no AutoCAD required** | **None** (never runs AutoLISP) |

Rule of thumb: cross-platform, batch, unattended, or untrusted-source CAD work should always go through the **ezdxf** backend. It never launches AutoCAD and never runs AutoLISP, so its RCE surface is zero. Reserve File IPC for cases where a human is present and the task genuinely needs AutoCAD's own geometry engine.

## Installation steps

### 1. Install the target application and the MCP runtime

- **Photoshop**: install Adobe Photoshop locally and confirm it launches. Core functionality doesn't need the UXP plugin — only neural filters (skin smoothing, colorization) require the optional UXP bridge. Node.js is also required (for `npx`).
- **AutoCAD**: clone `puran-water/autocad-mcp` and run `uv sync` to install dependencies. Verify the commit hash after cloning; `uv.lock` pins dependency versions. The File IPC backend additionally requires Windows plus AutoCAD LT 2024+, and loading its `mcp_dispatch.lsp` as described upstream.

### 2. Attach the server in the agent's `.mcp.json`

MCP servers attach to a **single agent's** `.mcp.json` (`~/.duduclaw/agents/<id>/.mcp.json`), not globally. This file already has a `duduclaw` server entry pre-written when the agent is installed — it's what lets the agent reach DuDuClaw's own MCP tools. When you attach a new server, **merge** it into `mcpServers` and keep the existing `duduclaw` entry; don't overwrite the whole file.

Photoshop (with telemetry disabled — see Risk disclosure):

```json
{
  "mcpServers": {
    "duduclaw": { "command": "…", "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "<id>" } },
    "photoshop": {
      "command": "npx",
      "args": ["-y", "@alisaitteke/photoshop-mcp"],
      "env": { "LOG_LEVEL": "2", "ANALYTICS_DISABLED": "1", "POSTHOG_DISABLED": "1" }
    }
  }
}
```

AutoCAD (`command` must be the absolute path to your local venv):

```json
{
  "mcpServers": {
    "duduclaw": { "command": "…", "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "<id>" } },
    "autocad-mcp": {
      "command": "C:\\path\\to\\autocad-mcp\\.venv\\Scripts\\python.exe",
      "args": ["-m", "autocad_mcp"],
      "env": { "AUTOCAD_MCP_BACKEND": "auto" }
    }
  }
}
```

The `serverKey` (`photoshop` / `autocad-mcp`) becomes the tool-name prefix: on the Claude side, tools are named `mcp__<serverKey>__<tool>`. The capability settings in the next section use this naming.

### 3. Configure capability governance

See the next section. This step is required, not optional.

> Both applications above have a matching paid expert pack (`marketing-designer`, `cad-drafter`) that bundles the soul, `.mcp.json` template, capability settings, and safety SOP into a single install. These single-role packs also appear in the dashboard's Expert Packs page built-in catalog, labeled by function/department; at install time you can use the "reports to" picker (or the CLI `duduclaw expert install <pack> --attach-under <agent-id>`) to attach the expert under an existing manager, folding it directly into the org chart and department.

## Capability governance

DuDuClaw controls tools through an agent's `agent.toml [capabilities]`. The four fields below do most of the work of governing external MCP servers like these; all are existing mechanisms, and they take effect as soon as the configuration is correct.

### `allowed_tools`: the enable switch (silently inert if unset)

External per-agent MCP tools are **not** in Claude CLI's default allow-list. In `-p` subprocess mode, any tool outside `allowed_tools` needs an interactive confirmation the subprocess has no way to give, so the call becomes a **silent no-op**. For the Photoshop/AutoCAD tools to work, `allowed_tools` must explicitly include `mcp__photoshop__*` or `mcp__autocad-mcp__*`, plus whatever other tools the agent still needs:

```toml
[capabilities]
allowed_tools = [
  "mcp__duduclaw__*", "mcp__photoshop__*",
  "WebSearch", "WebFetch", "Read", "Write", "Edit", "Glob", "Grep", "TodoWrite",
]
```

Once `allowed_tools` is set, it becomes the **only** auto-approved set (allowlist mode); anything not listed is refused. That works in your favor for narrowing the attack surface (for example, deliberately leaving `Bash` off the list).

`allowed_tools`/`denied_tools` are now enforced independently at two layers: the Claude CLI `-p` subprocess allow-list described above only governs **calls this agent spawns through the CLI**; the MCP dispatch gate (`McpDispatcher`) separately re-checks the same configuration against **any call that talks to an MCP server directly** (stdio, HTTP, SSE, or the openai-compat tool loop on a non-Claude runtime), matching against the tool's base name (the `mcp__<server>__` prefix is stripped automatically). Both layers read the same configuration with identical logic (`denied_tools` always wins), so there's no need to keep a second configuration for non-CLI call paths.

### `denied_tools`: hard-block RCE tools (always wins)

`denied_tools` is evaluated after `allowed_tools` and always wins. Use it to hard-block Photoshop's arbitrary-script tool:

```toml
denied_tools = ["mcp__photoshop__photoshop_execute_script"]
```

### `scoped_tools`: gate high-risk tools behind a human grant

A tool listed in `scoped_tools` gets folded into Claude CLI's `--disallowedTools` unless it has an active grant. Using it requires a per-task human approval through `capability_request` → ApprovalBroker (a PORTICO task-scoped grant), revoked the moment the task ends. Use it for things like overwrite-on-save, or for RCE that `denied_tools` can't isolate precisely.

AutoCAD's `execute_lisp` (RCE) is bundled upstream inside the `system` tool, alongside undo/redo/screenshot — there's no way to block just the risky sub-operation, so the whole `system` tool goes through the grant gate:

```toml
scoped_tools = ["mcp__autocad-mcp__system"]
```

Photoshop's overwrite-on-save tools go through the same grant gate (verify the actual tool names against the server's first introspection call):

```toml
scoped_tools = ["mcp__photoshop__photoshop_save_document", "mcp__photoshop__photoshop_close_document"]
```

### `maybe_irreversible_tools`: an ActionGuard override hint

Tools flagged as "maybe irreversible" get routed to an LLM judge or a human when they run through the goal-loop / duduclaw-dispatch path. This is a complementary declaration; for external MCP tools inside a plain `-p` CLI turn, containment still relies mainly on `scoped_tools` folding them into `--disallowedTools`.

### Granularity limits (an honest caveat)

Capability governance stops at tool-level granularity. When upstream bundles a dangerous operation with safe ones in the same tool (AutoCAD's `system` holds `execute_lisp`, undo, and screenshot together), you can't block just the dangerous sub-operation; the whole tool has to go through the grant gate, at the cost of requiring approval for the safe sub-operations too. For cleaner isolation, switch to a backend with no RCE at all (ezdxf for AutoCAD).

## Risk disclosure

- **Arbitrary code execution (RCE)**: both ExtendScript and AutoLISP can read and write arbitrary files and launch external programs, effectively acting as a shell with desktop-user privileges. That's how these two automation ecosystems are designed, not a bug. Hard-block them (Photoshop) or route them through the grant gate (AutoCAD) as described above; don't open them wide for an agent.
- **Telemetry (Photoshop)**: `@alisaitteke/photoshop-mcp` ships with third-party telemetry (Mixpanel / PostHog) on by default and sends usage events. Set `ANALYTICS_DISABLED=1` in the `.mcp.json env` block when you attach it (already included in this page's example).
- **Supply chain**: `npx -y @alisaitteke/photoshop-mcp` pulls the latest npm release every time, and `-y` auto-confirms it. For production, verify a specific version's behavior in a clean environment first, then pin `args` to `@alisaitteke/photoshop-mcp@<version>`. On the AutoCAD side, verify the commit after cloning and rely on `uv.lock`. Running `npm audit` / `pip-audit` before rollout is recommended.
- **Unofficial**: neither project is an official Adobe / Autodesk release, and neither is affiliated with the vendor.
- **Not audited line by line**: the above was reviewed against both repos' READMEs (a first-hand source). Anything the README doesn't disclose — for example, the actual transmission behavior behind Photoshop's claim that "the API key never leaves the machine" — is unverified; a source-level review and dependency scan are worth doing before a production rollout.

## Troubleshooting

| Symptom | Likely cause | Fix |
|------|----------|------|
| Photoshop/AutoCAD tools "get called but nothing happens" | `allowed_tools` doesn't include that server's `mcp__<key>__*` | Add it (see "the enable switch"); external MCP tools aren't in the default allow-list |
| Save tools always get blocked | Listed in `scoped_tools` with no active grant | Expected behavior; get ApprovalBroker approval to override |
| AutoCAD tools do nothing at all | `.mcp.json`'s `command` is still the template path | Change it to the absolute path of your local venv's python |
| File IPC won't run cross-platform | File IPC only supports Windows + AutoCAD LT 2024+ | Switch to `AUTOCAD_MCP_BACKEND=ezdxf` |
