# Browser Automation & Computer Use — Implementation Roadmap

> DuDuClaw 瀏覽器操作與電腦控制能力的分階段實作計畫。
> 核心原則：**能用 API 就不開瀏覽器，能在宿主跑就不開容器，能無頭就不用 computer_use。**

---

## Architecture: 5-Layer Auto-Routing

```
Agent Request → BrowserRouter (Rust, <1ms)
                 ├── L1: API Fetch        (reqwest / WebFetch — zero cost)
                 ├── L2: Static Scrape    (CSS/XPath selector — zero cost)
                 ├── L3: Headless Browser (Playwright MCP — low cost)
                 ├── L4: Sandbox Browser  (Container + Playwright — medium cost)
                 └── L5: Computer Use     (Virtual display + Claude vision — high cost)
```

Each layer auto-escalates only when the previous layer cannot handle the task.

---

## Phase 0: Foundation — Capability Gates (v0.10.0) ✅

**Goal**: Zero new features, but establish the security framework that all future phases build on.

### Completed

- [x] `CapabilitiesConfig` struct in `duduclaw-core/src/types.rs`
  - `computer_use: bool` (default: false)
  - `browser_via_bash: bool` (default: false)
  - `allowed_tools: Vec<String>` — explicit allowlist
  - `denied_tools: Vec<String>` — explicit denylist
  - `disallowed_tools()` method computes CLI `--disallowedTools` arg

- [x] `AgentConfig.capabilities` field (defaults to all-denied if omitted)

- [x] `claude_runner.rs` — `prepare_claude_cmd()` passes `--disallowedTools` to Claude CLI
  - All call paths updated: `call_with_rotation`, `call_claude_with_env`, `call_claude`
  - Sets `DUDUCLAW_BROWSER_VIA_BASH=1` env var when `browser_via_bash` enabled

- [x] `channel_reply.rs` — `call_claude_cli()` passes `--disallowedTools`
  - Capabilities extracted from agent config and threaded through
  - Sets `DUDUCLAW_BROWSER_VIA_BASH=1` env var when enabled

- [x] `bash-gate.sh` — Layer 1.5: Browser automation allowlist
  - Allows `npx playwright`, `npx puppeteer`, `playwright test/install/codegen`
  - Only when `DUDUCLAW_BROWSER_VIA_BASH=1` env is set
  - Rejects chained commands (`&&`, `||`, `` ` ``, `$(`) to prevent injection

### Agent Configuration Example

```toml
# agent.toml
[capabilities]
computer_use = false         # DANGEROUS: controls host display
browser_via_bash = true      # Allow playwright/puppeteer via Bash tool
denied_tools = []            # Additional tools to block
```

### Files Changed

| File | Change |
|------|--------|
| `crates/duduclaw-core/src/types.rs` | +`CapabilitiesConfig`, +`AgentConfig.capabilities` |
| `crates/duduclaw-gateway/src/claude_runner.rs` | `--disallowedTools` in `prepare_claude_cmd()`, `DUDUCLAW_BROWSER_VIA_BASH` env |
| `crates/duduclaw-gateway/src/channel_reply.rs` | `--disallowedTools` in `call_claude_cli()`, capabilities threading |
| `.claude/hooks/bash-gate.sh` | Layer 1.5 browser automation allowlist |

---

## Phase 1: L1 + L2 — Static Fetch & Extract (v0.10.1)

**Goal**: MCP tools for cached HTTP fetch and CSS selector extraction.

### Tasks

- [ ] MCP tool: `web_fetch_cached(url, ttl_seconds)` — HTTP GET with local cache
  - `reqwest` with configurable TTL (default 24h)
  - Response size limit: 5MB
  - URL validation: block `file://`, `javascript:`, internal IPs
  - Cache location: `~/.duduclaw/cache/web/`

- [ ] MCP tool: `web_extract(url, selector, format)` — CSS/XPath → JSON
  - Depends on `web_fetch_cached` for content retrieval
  - `scraper` crate for CSS selector, `select.rs` for XPath
  - Output formats: `text`, `html`, `json` (structured)
  - Multiple selectors in one call

- [ ] Rate limiter: 10 requests/minute per agent (configurable in `agent.toml`)

- [ ] `input_guard.rs` — scan fetched content before returning to agent
  - Prevents prompt injection via malicious web content

- [ ] Unit tests for URL validation (SSRF prevention)

### Files to Create/Modify

| File | Change |
|------|--------|
| `crates/duduclaw-cli/src/mcp.rs` | Register `web_fetch_cached`, `web_extract` tools |
| `crates/duduclaw-gateway/src/web_fetch.rs` | New: HTTP fetch + cache + rate limit |
| `crates/duduclaw-gateway/src/web_extract.rs` | New: CSS/XPath extraction |
| `Cargo.toml` | Add `scraper` dependency |

---

## Phase 2: L3 — Headless Browser via MCP (v0.11.0)

**Goal**: Playwright MCP server for JS-rendered pages and basic interaction.

### Tasks

- [ ] Agent `.mcp.json` template with Playwright MCP server config
  - `@anthropic-ai/mcp-server-playwright --headless`
  - Auto-generate when `capabilities.browser_via_bash = true`

- [ ] `BrowserRouter` (Rust) — route requests to appropriate layer
  - Heuristic: has API endpoint? → L1. Needs JS? → L3. Untrusted domain? → L4.
  - Configurable via `CONTRACT.toml [browser]` section

- [ ] `CONTRACT.toml` `[browser]` section parser
  - `trusted_domains`, `blocked_domains`, `max_pages_per_session`
  - `require_human_approval_for` actions list

- [ ] System prompt injection: browser security rules from CONTRACT.toml
  - Domain restrictions, form submission rules, download prohibition

- [ ] Dashboard: browser activity audit viewer
  - Real-time WebSocket stream of browser actions
  - Screenshot thumbnails for visual audit

### Dependencies

- `npm install -g @anthropic-ai/mcp-server-playwright`
- Playwright Chromium browser binary

---

## Phase 3: L4 — Sandboxed Browser (v0.12.0)

**Goal**: Container-isolated Playwright for untrusted domains.

### Tasks

- [ ] Docker image: `duduclaw/browser-sandbox`
  - Base: Playwright + Chromium headless
  - `--network=allowlist` (iptables domain-level filtering)
  - `--read-only` rootfs, `tmpfs` workspace
  - 512MB memory limit, 5min hard timeout

- [ ] Extend `duduclaw-container/sandbox.rs` with browser sandbox mode
  - Mount agent dir read-only at `/agent`
  - Inject allowed domains via env var
  - Capture screenshots to audit directory

- [ ] Domain-level network allowlist (iptables/nftables rules in container)
  - DNS resolution only for whitelisted domains
  - Block all internal IP ranges

- [ ] Screenshot audit storage
  - `~/.duduclaw/audit/screenshots/<agent>/<timestamp>.png`
  - Configurable retention (default 7 days)

- [ ] Dashboard: human approval gate for form submissions
  - WebSocket push → Dashboard modal → approve/deny
  - 30s timeout → auto-deny

---

## Phase 4: L5 — Computer Use (v0.13.0, on-demand)

**Goal**: Claude `computer_use` in a virtual display container for visual reasoning tasks.

### Tasks

- [ ] Container image: `duduclaw/computer-use`
  - Xvfb + VNC + Chromium/Firefox
  - Virtual display: 1280×800
  - `xdotool` for mouse/keyboard

- [ ] Claude Messages API integration with `computer_use` tool
  - Screenshot → base64 → Claude vision
  - Claude returns coordinates/actions
  - Container executes via `xdotool`

- [ ] Sensitive area masking
  - Detect password fields → blur before sending to Claude
  - Payment form detection → require human approval

- [ ] Double confirmation gate for high-risk actions
  - Form submission, file download, payment
  - Dashboard confirmation required

- [ ] `CONTRACT.toml [browser.l5_computer_use]` section
  - `enabled: false` (default off)
  - `max_actions: 50`
  - `container_required: true`

---

## Phase 5: Browserbase Cloud (optional, v0.14.0)

**Goal**: Cloud-hosted browser for production use without local resources.

### Tasks

- [ ] Browserbase MCP server integration
  - `@anthropic-ai/mcp-server-browserbase`
  - API key encrypted via `key_vault.rs`

- [ ] Session recording and playback
  - Browserbase provides built-in session replay

- [ ] Cost tracking in `CostTelemetry`
  - Browserbase API costs alongside Claude API costs

---

## Security Invariants (All Phases)

These invariants MUST hold across all phases:

1. **Deny-by-default**: All high-risk tools blocked unless explicitly enabled in `agent.toml`
2. **No host GUI access**: `computer_use` on host display requires explicit `computer_use = true`
3. **Input scanning**: All web content passes through `input_guard.rs` before reaching agent
4. **Audit trail**: Every browser action logged to `security_audit.jsonl`
5. **Domain restrictions**: `CONTRACT.toml` domain whitelist/blacklist enforced at all layers
6. **Human-in-the-loop**: Form submissions/payments require Dashboard approval (configurable)
7. **Container isolation**: L4/L5 always run in sandboxed containers with `--network=allowlist`

---

## Quick Reference: Agent Configuration

```toml
# agent.toml — full browser capabilities example

[capabilities]
computer_use = false           # L5: virtual display (DANGEROUS)
browser_via_bash = true        # L3: Playwright via Bash
allowed_tools = []             # If non-empty, ONLY these tools allowed
denied_tools = []              # Always blocked, even if otherwise allowed

# CONTRACT.toml — browser restrictions
[browser]
enabled = true
max_tier = "headless_browser"  # Max: api_fetch | static_scrape | headless_browser | sandbox_browser | computer_use
trusted_domains = ["example.com", "*.gov.tw"]
blocked_domains = ["*.onion", "localhost", "10.*", "192.168.*"]

[browser.restrictions]
allow_form_submit = false
allow_file_download = false
max_pages_per_session = 20
max_session_minutes = 10
screenshot_audit = true
require_human_approval_for = ["form_submit", "login", "payment_*"]
```
