# DuDuClaw Development Guide

> A guide to agent development, browser automation debugging, and local environment setup.

---

## 1. Quick start

### 1.1 Local development environment

```bash
# Start dev mode (auto-configures Playwright MCP)
duduclaw dev --port 18789

# Or target a specific agent
duduclaw dev --agent my-bot --port 18789
```

Dev mode:
- Starts the gateway + dashboard (`http://localhost:18789`)
- Auto-generates `.mcp.json` (Playwright MCP) for any agent with `browser_via_bash` enabled
- Streams logs to the dashboard in real time

### 1.2 Agent directory layout

```
~/.duduclaw/agents/my-bot/
├── agent.toml          # Agent config (model, budget, capabilities)
├── SOUL.md             # Agent persona and behavior guide
├── CLAUDE.md            # Claude Code project instructions (optional)
├── CONTRACT.toml       # Behavioral contract (boundaries, browser restrictions)
├── .mcp.json           # MCP server config (auto-generated)
├── .claude/            # Claude Code settings directory
└── SKILLS/             # Agent skills directory
```

### 1.3 Decision continuity (RFC-24)

When an agent presents the user with enumerated options ("Option A/B/C", "Option 1/2"),
the user may reply later — even across a session restart or after compression — with
something like "go with C". By default, the option text may already be gone if it
fell out of conversation memory during compression. With this feature enabled, the
system stores each option in a semantic memory layer independent of conversation
history at send time, and injects a "pending decision" note into later turns so the
agent can resolve the reference correctly.

Enable it in `agent.toml` (off by default, opt-in per agent):

```toml
[memory]
decision_continuity = true
```

Detection is deterministic, costs zero LLM calls, and errs on the side of capturing
too much rather than missing an option. A failed background capture never blocks the
reply from being sent. See [RFC-24](../rfc/RFC-24-decision-continuity.md) for details.

### 1.4 Choosing an AI runtime backend (multi-runtime)

Each agent can independently choose which AI CLI backend drives it, through the
`AgentRuntime` trait abstraction. `RuntimeRegistry` auto-detects which CLIs are
installed at startup and registers them; `agent.toml` sets the choice with
`[runtime] provider` (default `claude`), and `fallback` names the backend to use
when the primary one is unavailable.

```toml
[runtime]
provider = "antigravity"   # claude | codex | gemini | antigravity | openai_compat
fallback = "claude"        # backend to fall back to when detection fails
```

| Provider | CLI binary | Auth | Notes |
|----------|-----------|------|------|
| `claude` | `claude` (always available, core) | OAuth / API key rotation | Default backend |
| `codex` | `codex` | OpenAI | — |
| `gemini` | `gemini` | `GEMINI_API_KEY` / OAuth | Personal-edition OAuth was retired on 2026-06-18; paid API keys still work |
| `antigravity` | `agy` (`~/.local/bin/agy`) | `ANTIGRAVITY_API_KEY` / OAuth | Official successor to the Gemini CLI; multi-model (Gemini 3.x + Claude + GPT-OSS) |
| `openai_compat` | HTTP (no CLI) | per-provider key | OpenAI-compatible endpoints such as Exo / llamafile / vLLM |

**Antigravity (`agy`) specifics** (see
[TODO-antigravity-cli-migration.md](../todo/TODO-antigravity-cli-migration.md)):

- The agent directory is automatically added to agy's `trustedWorkspaces`, so a
  headless run doesn't get stuck on the "trust this workspace?" prompt.
- Print mode has no JSON output, so token usage is a CJK-aware heuristic estimate,
  not an exact figure.
- You need to complete one interactive login (OAuth) on a machine with `agy`
  installed, or set `ANTIGRAVITY_API_KEY`, before the gateway can call it
  successfully.

---

## 2. Browser automation debugging (L1-L5)

### 2.1 Architecture overview

```
Agent Request → BrowserRouter (<1ms)
  ├── L1: API Fetch        (reqwest — zero cost)
  ├── L2: Static Scrape    (CSS selector — zero cost)
  ├── L3: Headless Browser (Playwright MCP — low cost)
  ├── L4: Sandbox Browser  (Container + Playwright — medium cost)
  └── L5: Computer Use     (Virtual display + Claude vision — high cost)
```

Core principle: **use the API when you can, and skip Computer Use whenever headless will do.**

### 2.2 L1 — API Fetch debugging

```bash
# Run the SSRF defense test
duduclaw test --browser

# Test the MCP tool manually
claude -p "Use web_fetch_cached to fetch https://example.com"
```

**What to verify:**
- The `file://`, `javascript:`, and `data:` schemes are blocked
- Internal IPs (127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16) are blocked
- IPv6 loopback `[::1]` is blocked
- Cache hits work (a second request to the same URL should return `cached: true`)
- Rate limiting applies (10 requests per agent per minute)

### 2.3 L2 — CSS extraction debugging

```bash
# Test CSS selector extraction
claude -p 'Use web_extract on https://example.com with selector "h1" and format "text"'
```

**Supported formats:**
- `text` — plain text content
- `html` — inner HTML
- `json` — structured JSON (tag, attributes, children)

### 2.4 L3 — Playwright MCP debugging

```bash
# Confirm Playwright MCP is configured
cat ~/.duduclaw/agents/my-bot/.mcp.json

# Generate it manually if it's missing
# Set browser_via_bash = true in agent.toml, then restart the gateway
```

**`.mcp.json` example:**
```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["@anthropic-ai/mcp-server-playwright", "--headless"],
      "env": {}
    }
  }
}
```

**Prerequisites:**
- `npm install -g @anthropic-ai/mcp-server-playwright`
- Playwright Chromium: `npx playwright install chromium`

### 2.5 L4 — Container sandbox debugging

```bash
# Build the sandbox image
docker build -f container/Dockerfile.browser-sandbox -t duduclaw/browser-sandbox .

# Start it manually (for testing)
docker run --rm --read-only --tmpfs /tmp:size=256m \
  -e ALLOWED_DOMAINS="example.com,httpbin.org" \
  duduclaw/browser-sandbox

# No domains = no network
docker run --rm --read-only --tmpfs /tmp:size=256m --network=none \
  duduclaw/browser-sandbox
```

**`CONTRACT.toml` example:**
```toml
[browser]
enabled = true
max_tier = "sandbox_browser"
trusted_domains = ["example.com", "*.gov.tw"]
blocked_domains = ["*.onion", "localhost"]

[browser.restrictions]
allow_form_submit = false
max_pages_per_session = 20
max_session_minutes = 10
screenshot_audit = true
require_human_approval_for = ["form_submit", "login", "payment_*"]
```

### 2.6 L5 — Computer Use debugging

#### Option A: container (production)

```bash
# Build the computer-use image
docker build -f container/Dockerfile.computer-use -t duduclaw/computer-use .

# Start it (with VNC)
docker run --rm -p 5900:5900 \
  -e DISPLAY_SIZE=1280x800 \
  -e VNC_ENABLED=true \
  -e VNC_PASSWORD=debug123 \
  duduclaw/computer-use

# Connect with a VNC client to watch
# macOS: open vnc://localhost:5900
```

**`CONTRACT.toml` L5 settings:**
```toml
[browser.computer_use]
enabled = true
max_actions = 50
container_required = true
display_size = "1280x800"
blur_patterns = ["input[type=password]", ".credit-card", "[data-sensitive]"]
```

#### Option B: Claude Code Computer Use MCP (local debugging only)

> **Limits**: macOS only, Pro/Max plan, interactive sessions only, machine-level lock

**Prerequisites:**
- macOS
- Claude Code v2.1.85 or newer
- A Claude Pro or Max subscription

**Enabling it:**

1. Run `/mcp` inside Claude Code
2. Find the `computer-use` server and select **Enable**
3. On first use, macOS will ask you to grant:
   - **Accessibility** (System Settings → Privacy & Security → Accessibility)
   - **Screen Recording** (System Settings → Privacy & Security → Screen Recording)

**Usage:**
```bash
# In an interactive Claude Code session
claude

# Claude will use the computer-use tools to operate the desktop directly
> Please open Safari and browse to example.com
```

**Notes:**
- Not for production use — debugging only
- Non-interactive `-p` mode is not supported
- Machine-level lock: only one Claude Code session can use it at a time
- Token usage is very high (every action needs a full screenshot)
- Coordinate precision is limited (risk of visual misreads)
- Zero setup cost (it's built into Claude Code)
- Can operate any macOS application, not just the browser

---

## 3. Security mechanisms

### 3.1 Input Guard (injection scanning)

User input entering an agent passes through `duduclaw-security`'s `input_guard` scanner, which applies a risk-scoring model (0-100): six weighted rules accumulate a score, and crossing the threshold blocks the input and writes an entry to `security_audit.jsonl`.

| Rule | Weight | Example |
|------|------|---------|
| instruction_override | 40 | "ignore previous instructions" |
| role_hijack | 35 | "act as", "your new role" |
| system_prompt_extraction | 30 | "reveal your instructions" |
| tool_abuse | 30 | Prompts that try to induce tool misuse |
| encoding_bypass | 25 | Base64 or other encoding bypass |
| data_exfiltration | 25 | "send to" + a URL |

Unicode normalization (zero-width characters, homoglyphs) adds further protection against bypasses.

> Note: content scraped by L1/L2 does not currently go through a separate content-classification scan. Protection at the `web_fetch` layer is SSRF validation (scheme / internal IP / metadata endpoint / DNS rebinding / per-redirect re-validation), a 5MB size cap, and rate limiting.

### 3.2 Emergency stop

- In-channel safe words: `!STOP` / `!停止` (single scope) and `!STOP ALL` / `!全部停止` (global) to trigger; `!RESUME` / `!恢復` to recover. These are handled by the failsafe system and require admin privileges.
- The dashboard header has a one-click E-Stop / Resume control.

### 3.3 Tool approval (HITL ApprovalBroker)

High-risk operations go through the unified ApprovalBroker (`approvals.db`; a TTL expiry is treated as a denial, fail-closed):
- `agent.toml [capabilities] approval_required_tools` declares which tools require approval
- The autopilot `require_approval` action goes through the same broker
- See the observability / capabilities docs for details

### 3.4 User pairing

Channel-level user access control, stored in `channel_settings` (global scope, per channel type):
- `require_pairing = "true"`: unpaired users must pair before they can chat
- `allowed_users` / `blocked_users`: JSON array allowlist / blocklist
- Flow: an admin runs the MCP tool `pairing_manage` (action=generate) to produce a 6-digit pairing code (valid for 5 minutes) → the user sends `/pair <code>` in the channel → once approved, it's persisted to `~/.duduclaw/access_control.json`
- Brute-force protection: 5 failures locks a code, 15 cumulative failures across regenerations, constant-time comparison, codes stored as SHA-256

### 3.5 Screenshot masking

L5 Computer Use automatically detects and masks sensitive regions:
- `input[type=password]` — password fields
- `.credit-card` — credit card forms
- `[data-sensitive]` — custom sensitive regions

Masking rules are defined in `CONTRACT.toml [browser.computer_use] blur_patterns`.

---

## 4. Browser test suite

```bash
# Run the full browser test suite
duduclaw test --browser

# Test coverage:
# [L1] SSRF prevention (4 URLs)
# [L1] HTTP fetch (httpbin.org)
# [L2] CSS extraction
# [Guard] Content injection scanner
```

---

## 5. Audit and monitoring

### 5.1 Browser audit log

All browser operations are logged to `~/.duduclaw/audit/browser/audit.jsonl`:

```bash
# View recent activity
tail -20 ~/.duduclaw/audit/browser/audit.jsonl | jq .

# Query it via the MCP tool
claude -p "Use browser_audit_log to show last 10 entries"
```

### 5.2 Screenshot audit

Screenshots are stored under `~/.duduclaw/audit/browser/screenshots/{agent_id}/`:
- Format: `{timestamp}.png`
- Retained for 7 days by default
- Browsable from the dashboard's Security page

---

## 6. Troubleshooting

### Playwright MCP connection fails
```bash
# Confirm it's installed
npx @anthropic-ai/mcp-server-playwright --version

# Confirm .mcp.json is correct
cat ~/.duduclaw/agents/my-bot/.mcp.json
```

### Docker sandbox won't start
```bash
# Confirm Docker is running
docker info

# Confirm the image was built
docker images | grep duduclaw

# Test it manually
docker run --rm duduclaw/browser-sandbox echo "OK"
```

### Emergency Stop won't recover
```bash
# Manually clear the signal file
rm ~/.duduclaw/emergency_stop

# Or via the MCP tool
claude -p 'Use emergency_stop with action "resume"'
```
