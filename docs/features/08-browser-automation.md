# Browser Automation and Computer Use

> Three MCP tool groups, from a plain HTTP fetch to a full virtual desktop. The agent picks; there is no automatic router.

---

## A note on history

Earlier versions of this page described a **5-layer automatic router** that escalated L1 → L2 → L3 → L4 → L5 on its own, including an "L4 Sandbox Browser" tier.

That router (`browser_router.rs`) was written in 2026-04, never acquired a call site, and never appeared in the CHANGELOG. It was deleted in 2026-09. The L4 tier it described never existed as a separate capability at all.

What ships is simpler: three groups of MCP tools that the agent chooses between, plus an optional external MCP server for headless browsing. Cost discipline comes from the agent's own judgment and the tool descriptions, not from a routing engine.

---

## What actually ships

### L1 — `web_fetch_cached`

A plain HTTP GET with SSRF protection, disk caching and rate limiting. Returns status, content type and body, truncated at 60k characters. `ttl_seconds` controls the cache (default 86400).

The SSRF gate (`web_fetch::validate_url` + `resolve_public_addrs`) rejects internal hosts and cloud metadata endpoints, resolves DNS at request time, requires every resolved address to be public, and then pins the answer for the request. The same gate protects the resident-sensing `http_poll` and `websocket` source kinds.

Use it for: documented APIs, JSON endpoints, server-rendered pages you only need the raw bytes of.

### L2 — `web_extract`

Fetches a URL through the same cached, SSRF-validated path, then extracts elements with a CSS selector. Output format is `text` (default), `html`, or `json` (structured, with attributes and children).

Use it for: traditional server-rendered sites where the content is in the initial HTML.

Neither L1 nor L2 executes JavaScript. A single-page app returns its empty shell.

### L3 — Playwright or Browserbase, as an external MCP server

For JavaScript-rendered pages, the agent gets a headless browser by having a browser MCP server registered in its own `<agent_dir>/.mcp.json` (`mcp_template.rs` generates a Playwright or a Browserbase config). This is a per-agent MCP server, separate from the globally registered DuDuClaw MCP server.

It is not part of the DuDuClaw binary, there is no fallback from L2 into it, and nothing installs it for you — the operator adds it, and the agent's `allowed_tools` / `denied_tools` decide whether the agent may call it.

### L5 — Computer Use

Seven MCP tools drive a virtual display inside a container sandbox: `computer_screenshot`, `computer_click`, `computer_type`, `computer_key`, `computer_scroll`, `computer_session_start`, `computer_session_stop`.

`computer_use_orchestrator` owns the loop — container lifecycle → screenshot → Claude vision analysis → action → repeat — and reports progress back to the originating channel. The container image (`duduclaw-computer-use:latest` by default), display size and network mode are configurable, and cleanup is guaranteed even on panic or task cancellation.

Use it for: anything a person sitting at a computer could do — logins, drag-and-drop, visual pattern recognition. It is the slowest and most expensive option by a wide margin.

---

## Security: deny by default

Every tier past L2 needs explicit authorization in `agent.toml`:

```toml
[capabilities]
computer_use = false        # the seven computer_* tools
browser_via_bash = false    # launching a browser from a Bash tool call
allowed_tools = [...]       # allowlist
denied_tools = [...]        # denylist
```

- `computer_use = false` (the default) makes every `computer_*` MCP tool return a refusal, checked fail-closed: an absent file, malformed TOML, or a wrong-typed key all deny.
- `denied_tools` is passed to the CLI as `--disallowedTools` and is *also* enforced at the MCP dispatcher front door, so the restriction holds even on the PTY-pooled path.
- `browser_via_bash` no longer sets an environment flag. The `bash-gate.sh` allowlist that used to read `DUDUCLAW_BROWSER_VIA_BASH` was removed with the rest of the shell hooks in `ba015a48`. The capability still takes effect: it feeds `disallowed_tools()` and `CapabilitiesConfig::sandbox_level()`, which is how the codex and gemini runtimes decide between `ReadOnly` and `WorkspaceWrite` sandboxes.
- The three additional restriction knobs that only ever existed as fields on the dead router (trusted/blocked domains, per-session page caps, screenshot audit, per-action human approval) are **not** implemented anywhere. Approval gating for irreversible actions goes through `ApprovalBroker` and `agent.toml [capabilities] approval_required_tools` / `irreversible_tools` instead.

---

## Rough cost comparison

| Tier | Startup | Memory | Executes JS | Needs a container |
|---|---|---|---|---|
| L1 `web_fetch_cached` | ~0 ms | ~1 MB | no | no |
| L2 `web_extract` | ~0 ms | ~5 MB | no | no |
| L3 Playwright / Browserbase MCP | seconds | hundreds of MB | yes | no (external process or cloud) |
| L5 `computer_*` | ~10 s | 500 MB+ | yes | yes |

Reaching for L5 when L1 would answer the question is the expensive mistake, and it is the agent's to avoid: nothing in the platform stops it.

---

## Interaction with other systems

- **Container sandbox** — L5 runs on the same container infrastructure that isolates agent task execution (`--network=none`, tmpfs, read-only rootfs).
- **Security defense** — capability enforcement and the audit trail are described in [05-security-defense.md](05-security-defense.md).
- **Resident sensing** — `http_poll` / `websocket` tick sources share L1's SSRF gate. See [41-resident-sensing.md](41-resident-sensing.md).
- **Audit log** — every MCP tool call, including refusals, lands in `tool_calls.jsonl` with masked arguments and results.

---

## The takeaway

The honest version is less impressive than the router story and easier to operate: four ways to touch the web, each with its own cost and its own switch, and an agent that has to choose. When the routing engine was deleted, this page had to stop describing one.
