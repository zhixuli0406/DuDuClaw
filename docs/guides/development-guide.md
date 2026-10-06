# DuDuClaw Development Guide

> A guide to agent development, browser automation debugging, and local environment setup.

---

## 1. Quick start

### 1.1 Local development environment

```bash
# Start the server (gateway + channels + heartbeat + dashboard)
duduclaw run

# Same server without the interactive prompts
duduclaw gateway
```

There is no separate `duduclaw dev` mode. The server:
- Serves the dashboard on the address in `config.toml [gateway]` (`bind` / `port`, default `http://127.0.0.1:18789`)
- Streams logs to the dashboard in real time
- Does not add a browser MCP server to any agent; see section 2.4 for how one gets into `.mcp.json`

### 1.2 Agent directory layout

```
~/.duduclaw/agents/my-bot/
├── agent.toml          # Agent config (model, budget, capabilities)
├── SOUL.md             # Agent persona and behavior guide
├── CLAUDE.md            # Claude Code project instructions (optional)
├── CONTRACT.toml       # Behavioral contract ([boundaries] only)
├── .mcp.json           # Per-agent MCP servers (DuDuClaw writes its own entry; browser servers are added by you)
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
provider = "antigravity"   # claude | codex | gemini (deprecated) | antigravity | openai_compat
fallback = "claude"        # backend to fall back to when detection fails
```

| Provider | CLI binary | Auth | Notes |
|----------|-----------|------|------|
| `claude` | `claude` (always available, core) | OAuth / API key rotation | Default backend |
| `codex` | `codex` | OpenAI | — |
| `gemini` | `gemini` | `GEMINI_API_KEY` / OAuth | **Deprecated in v1.67.0, removed in v1.71.0** (see [Deprecations](deprecations.md#gemini-cli-runtime)). Personal-edition OAuth was retired on 2026-06-18; paid API keys still work |
| `antigravity` | `agy` (`~/.local/bin/agy`) | Google sign-in (`agy` in a terminal) / `GEMINI_API_KEY` | Official successor to the Gemini CLI; multi-model (Gemini 3.x + Claude + GPT-OSS) |
| `openai_compat` | HTTP (no CLI) | per-provider key | OpenAI-compatible endpoints such as Exo / llamafile / vLLM |

**Antigravity (`agy`) specifics** (see
[TODO-antigravity-cli-migration.md](../todo/TODO-antigravity-cli-migration.md)):

- The agent directory is automatically added to agy's `trustedWorkspaces`, so a
  headless run doesn't get stuck on the "trust this workspace?" prompt.
- Print mode has no JSON output, so token usage is a CJK-aware heuristic estimate,
  not an exact figure.
- Authenticate once before the gateway can call it: either run `agy` in a
  terminal on the host and follow the Google sign-in prompts (there is no
  `agy login` subcommand), or use API-key mode: set `config.toml [antigravity]
  auth = "api_key"` and supply a Gemini API key (a `gemini` provider account or
  the `GEMINI_API_KEY` environment variable). Use API-key mode in a container
  or remote host with no keyring or browser.

### 1.5 Live validation home

Validate a build against a real gateway in an isolated home, not in `~/.duduclaw`:

```bash
scripts/live-test/make-home.sh /tmp/ddc-live --port 18977
HOME=/tmp/ddc-live/os-home DUDUCLAW_HOME=/tmp/ddc-live duduclaw run --yes &   # boots once, writes .mcp.json
scripts/live-test/mcp-probe.sh /tmp/ddc-live plain
scripts/live-test/mcp-probe.sh /tmp/ddc-live prod-shaped
```

Always start the gateway with `HOME` pointing at the `os-home` directory that
`make-home.sh` creates inside the isolated home. The gateway and the AI CLI it
spawns look for logins and settings under `HOME`. If only `DUDUCLAW_HOME` is
changed, the spawned `claude` uses the operator's own login and spends the
operator's own quota, and Antigravity's API-key mode rewrites the operator's own
settings file. `mcp-probe.sh` starts the MCP server with the same `os-home`.

The home holds two employees: `plain` (no allowlist) and `prod-shaped`
(`allowed_tools = ["mcp__duduclaw__*", ...]`, denied and approval lists, explicit
permissions, budget, contract). The v1.67.0 regression where the wildcard allowlist
refused every platform tool slipped through because the test employee had no
allowlist. Rule: after upgrading a production home, probe real tools through each
employee's own MCP registration (`mcp-probe.sh ~/.duduclaw <agent-id>`). An isolated
home does not isolate the operator's Claude connectors (Drive, Gmail); tell test
employees not to query external services. When the build cache fills the disk, run
`scripts/clean-build-cache.sh --dry-run` first; it keeps third-party artifacts and
refuses to run while `cargo` or `rustc` is alive. Details: `scripts/live-test/README.md`.

---

## 2. Browser automation and computer use debugging

### 2.1 Architecture overview

There is no router. The agent sees the L1 and L2 MCP tools and an optional L3 server and chooses between them itself; L5 is a container session that the agent drives through the `computer_*` MCP tools. Nothing escalates automatically from one to the next (the old "BrowserRouter" was deleted in 2026-09, see [Browser automation](../features/08-browser-automation.md)).

```
Agent
  ├── L1: web_fetch_cached   (HTTP GET, SSRF-gated, disk cache)
  ├── L2: web_extract        (same fetch path + CSS selector)
  ├── L3: external headless-browser MCP server (optional, per-agent .mcp.json)
  └── L5: computer-use session in a container with a virtual display,
           driven by the agent through eight computer_* MCP tools (gateway-owned session)
```

There is no L4: the container that wraps a whole task is the task sandbox (section 2.5), not a browser tier. Capability gates live in `agent.toml [capabilities]`: `computer_use` (default `false`, fail-closed; gates the `computer_*` tools), `browser_via_bash`, `allowed_tools`, `denied_tools`. `denied_tools` is enforced at the MCP dispatcher as well as passed to the CLI as `--disallowedTools`.

### 2.2 L1 — `web_fetch_cached` debugging

Exercise it through an agent, or ask the model directly:

```bash
claude -p "Use web_fetch_cached to fetch https://example.com"
```

**What to verify** (`crates/duduclaw-gateway/src/web_fetch.rs`, `crates/duduclaw-cli/src/mcp/web.rs`):
- Only `http` and `https` are accepted; other schemes (`file:`, `javascript:`, `data:`) are blocked
- `localhost`, cloud metadata hostnames and every address that `duduclaw_core::net_addr::is_public_ip` does not call public are blocked: IPv4 `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10`, `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.0.0.0/24`, `192.0.2.0/24`, `192.168.0.0/16`, `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4`, `240.0.0.0/4`; IPv6 outside `2000::/3`, plus `2001::/32`, `2001:db8::/32` and `3fff::/20`; IPv4-mapped, NAT64 (`64:ff9b::/96`) and 6to4 addresses are judged by the IPv4 address inside them (try `http://[::ffff:127.0.0.1]/`, which must be refused). The same classifier serves every other outbound gate (resident sensing, media, relay, MCP import, skills RPC, Odoo, wiki federation, computer-use pinning)
- A second request to the same URL returns `cached: true` (`ttl_seconds` sets the cache lifetime)
- Rate limit: 10 requests per minute per agent, shared with `web_extract`
- The returned body is truncated at 60,000 characters

### 2.3 L2 — `web_extract` debugging

```bash
claude -p 'Use web_extract on https://example.com with selector "h1" and format "text"'
```

It uses the same fetch path, so the SSRF, cache and rate-limit checks above apply. Neither tool runs JavaScript: a single-page app returns its empty shell.

**Supported formats:**
- `text` — plain text content
- `html` — inner HTML
- `json` — structured JSON (tag, attributes, children)

### 2.4 L3 — external headless browser MCP server debugging

DuDuClaw does not bundle a headless browser and does not add one for you. If an agent needs JavaScript-rendered pages, register a browser MCP server in that agent's own `.mcp.json` (separate from the globally registered DuDuClaw MCP server). `crates/duduclaw-agent/src/mcp_template.rs` provides `playwright_mcp_config` / `browserbase_mcp_config` helpers that build such an entry, but no gateway code calls them to install it automatically, so write the entry yourself.

```bash
# Look at what is registered for the agent
cat ~/.duduclaw/agents/my-bot/.mcp.json
```

**`.mcp.json` example (the shape `playwright_mcp_config(true)` produces):**
```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp", "--headless"],
      "env": {}
    }
  }
}
```

**Prerequisites:**
- Node.js with `npx`. `npx -y` downloads `@playwright/mcp` on first start; no global install is needed.
- A browser the server can launch. Which one and how to install it are described in the `@playwright/mcp` README; this guide does not repeat them.
- Before v1.67.1 this example named `@anthropic-ai/mcp-server-playwright`, which does not exist on npm. An `.mcp.json` written from the old example still has that name and fails at start; change it to the line above.

Whether the agent may call this server's tools is decided by its `[capabilities] allowed_tools` / `denied_tools`.

### 2.5 Container sandbox debugging (task sandbox)

There is no separate "L4 sandbox browser" tier: browser work goes through L1, L2, an optional L3 MCP server, or L5 (section 2.6). The container that wraps an agent's whole task is the **task sandbox** (`agent.toml [container] sandbox_enabled = true`). To debug it, see [Task sandbox guide](task-sandbox.md) for the settings; the steps below are for finding out why a sandboxed task failed.

```bash
# 1. Preconditions: Docker reachable, image present, which agents are sandboxed
duduclaw doctor

# 2. Audit events written by the sandbox
grep -E 'task_sandbox_(unavailable|bypassed|tool_violation)' ~/.duduclaw/security_audit.jsonl | tail

# 3. Containers that are still around (the sandbox labels its containers and removes them when the task ends)
docker ps -a --filter name=dudu-task-
```

- `task_sandbox_unavailable` carries a reason code (`docker_unreachable`, `image_missing`, `network_disabled`, `root_user`, `no_account`, `unsupported_runtime`, `invalid_config`, `unsupported_platform`).
- `task_sandbox_bypassed` means `when_unavailable = "run_unsandboxed"` let a task run without isolation.
- `task_sandbox_tool_violation` means the AI used a tool outside the file and shell set and the task was stopped.

To poke around in the image with similar hardening (this does not reproduce the task's credentials, mounts or supervisor; it only shows what the image contains and whether the CLI starts under a read-only root):

```bash
docker run --rm -it --read-only --user "$(id -u):$(id -g)" \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --tmpfs /tmp:rw,exec,nosuid,nodev,size=256m,mode=1777 \
  --memory 4g --pids-limit 128 --cpus 1 \
  --entrypoint /bin/sh <sandbox-image>
```

Use the image from `config.toml [container.sandbox] image` (default `ghcr.io/zhixuli0406/duduclaw:v<version>`). The sandbox needs a bridge network to the model provider, so this example leaves Docker's default network on. The script sandbox used by PTC and `secaudit` is separate and does use `--network=none`.

### 2.6 L5 — Computer Use debugging

A session starts one Docker container through `computer_use_orchestrator` (`crates/duduclaw-gateway/src/computer_use_orchestrator.rs`) and needs `[capabilities] computer_use = true`. The agent calls the eight `computer_*` MCP tools. The MCP side (`crates/duduclaw-cli/src/mcp/computer_use_client.rs`) is a thin client: it signs each call and posts it to the gateway on loopback, `POST /api/internal/computer-use` with `{op: start | screenshot | action | stop | status, …}`. The gateway side (`crates/duduclaw-gateway/src/computer_use_sessions/`) authenticates the caller (`auth.rs`), re-checks the tool gates and approval lists (`gates.rs`), validates and risk-rates each action (`actions.rs`), checks `computer_navigate` URLs against the allowlist (`navigation.rs`), resolves the chat for a high-risk confirmation from its own record of live turns (`turns.rs`), and reaps and sweeps containers (`mod.rs`, `sweep.rs`). The network allowlist is `[capabilities.computer_use_config] allowed_domains`; see [Browser automation](../features/08-browser-automation.md) for the rules. To see whether the agent currently holds a session, look for its container (below) or its `session_start` / `session_end` rows in the browser audit log (section 5.1).

There is no other way in. The gateway-run loop that a chat message used to trigger (Anthropic `computer_20251124` tool, progress posted to the channel) and the `native` host-desktop mode were removed; a message mentioning computer-use keywords now takes the normal reply path. `[capabilities] computer_use_mode = "native"` still parses but the tools refuse it with `native_unsupported`; `"auto"` or no key behaves as `"container"`.

Per-agent limits come from `agent.toml [capabilities.computer_use_config]`: `max_actions` (50), `max_session_minutes` (10), `display_width` / `display_height` (1280x800), `allowed_apps`, `blocked_actions` (default `delete_file`, `terminal`, `system_preferences`), `auto_confirm_trusted`, and `allowed_domains`. The session manager also blocks actions that match `CONTRACT.toml` `must_not` rules, read from a `[must_not] rules = [...]` table (`computer_use_sessions/mod.rs`), which is not the `[boundaries]` table the rest of the contract system uses. There are no `[browser.*]` keys in CONTRACT.toml; nothing reads them.

#### Option A: container (production)

```bash
# Pull the image this gateway version runs by default (published by
# .github/workflows/computer-use-image.yml from the first release tag after v1.66.1)
docker pull ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>

# Or build it locally from the repo root, then point the gateway at the local tag:
#   config.toml  ->  [computer_use]
#                    image = "duduclaw-computer-use:latest"
docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .

# Start it by hand with VNC to watch the virtual display
# (shown with the local tag; the pulled ghcr image works the same way).
# Publishing a port needs a network, so the domain filter needs NET_ADMIN,
# and a non-empty ALLOWED_DOMAINS so that VNC reply packets are let out.
docker run --rm -p 5900:5900 \
  --cap-add=NET_ADMIN \
  -e ALLOWED_DOMAINS=example.com \
  -e DISPLAY_SIZE=1280x800 \
  -e VNC_ENABLED=true \
  -e VNC_PASSWORD=debug123 \
  duduclaw-computer-use:latest

# Connect with a VNC client to watch
# macOS: open vnc://localhost:5900
```

The image (about 1.09 GB) is built on `debian:trixie-slim` with the Debian `chromium` package, Xvfb, `openbox` as window manager, optional VNC (`x11vnc`), `xdotool`, `scrot`, the domain filter, a `xdotool getactivewindow` health check and Python 3 for the `duduclaw-eval-dom` helper (section 3.5). Openbox and Chromium run as the unprivileged `sandbox` user; the entrypoint stays root only to program iptables. Chromium starts in kiosk mode at 0,0 covering the whole virtual display with device scale factor 1, so page coordinates equal screenshot pixels, and its DevTools port listens on 127.0.0.1 inside the container only. If the browser exits (for example the agent closed the window), the entrypoint restarts it. Chromium reads a managed policy from `container/scripts/chromium-policy.json` (no local-network or loopback access for pages, no incognito/guest windows, no file dialogs, printing or downloads, pop-ups and device permissions blocked, `file://` / `chrome://` / `devtools://` / `view-source:` / `javascript://` blocked; see [Browser automation](../features/08-browser-automation.md) for the list). `DeveloperToolsAvailability` is left unset on purpose: setting it also disables the loopback DevTools protocol the helpers need. `duduclaw-navigate` reads the URL from stdin only (`printf 'https://example.com/\n' | docker exec -i <container> duduclaw-navigate`); any argument is a usage error. Screenshots are captured to `/tmp/duduclaw-root/screen.png` (root-owned directory, mode 0700, created before the browser starts), where the Chromium log also lives.

Which image a session runs: by default `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway version>`, published by `.github/workflows/computer-use-image.yml` on git tags `v*` (or manual dispatch). That workflow builds `linux/amd64` and `linux/arm64` on native runners and smoke-tests each build with the gateway's own container flags (window manager up, a screenshot, `duduclaw-eval-dom` returning `[]`) before pushing `:<tag>` and `:latest`. It first runs on the first release tag after v1.66.1, so no published image exists for v1.66.1 or earlier, and `scripts/release.sh verify` does not check it. The only override is the global `config.toml [computer_use] image = "<ref>"` (a digest reference is accepted); there is no per-agent image key. An invalid `[computer_use]` section makes computer use unavailable with a message and never falls back to the default (`crates/duduclaw-gateway/src/computer_use_image.rs`). The image is never pulled automatically: `docker run` carries `--pull never` and a presence check (`docker image inspect`) runs before each session. A machine that only has a locally built `duduclaw-computer-use:latest` (the old default) must pull the versioned image or set the override key.

The containers the orchestrator starts itself run with `--read-only`, a 256 MB tmpfs `/tmp`, 1 CPU, 512 MB memory, 512 PIDs (Chromium's threads count against this limit; the earlier 100 was reached by an ordinary page with web workers), and `--network=none` unless the session has allowlist hosts that resolved at start. Every container also gets `--security-opt no-new-privileges`. With allowlist hosts the gateway adds `--network bridge` explicitly, one `--add-host <host>:<address>` per host, passes the addresses to the domain filter as `ALLOWED_IPS`, and adds `--cap-add=NET_ADMIN`, which the filter needs to install its default-deny egress rule (only TCP 443 to those addresses, no DNS, loopback only to `127.0.0.1` / `::1`, Docker's resolver `127.0.0.11` rejected; the same loopback rule applies when a container has a network but no allowlist); without the capability, a container with a network route refuses to start. Their names start with `duduclaw-cu-`, so `docker ps -a --filter name=duduclaw-cu-` shows them. Each carries the labels `com.duduclaw.computer-use.home` (which DuDuClaw home owns it) and `com.duduclaw.computer-use.deadline` (unix seconds); a sweep at gateway start and every 10 minutes removes this home's containers that have exited or are more than 600 seconds past that deadline.

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

User input entering an agent passes through `duduclaw-security`'s `input_guard` scanner, which applies a risk-scoring model (0-100, capped): seven weighted rules accumulate a score, and reaching the threshold (default 60) blocks the input and writes an entry to `security_audit.jsonl`. `instruction_override`, `role_hijack`, `tool_abuse` and `data_exfiltration` also block on a single match regardless of the score (`crates/duduclaw-security/src/input_guard.rs`).

| Rule | Weight | Example |
|------|------|---------|
| instruction_override | 40 | "ignore previous instructions" |
| role_hijack | 35 | "act as", "your new role" |
| system_prompt_extraction | 30 | "reveal your instructions" |
| tool_abuse | 30 | Prompts that try to induce tool misuse |
| encoding_bypass | 25 | Base64 or other encoding bypass |
| data_exfiltration | 25 | "send to" + a URL |
| termination_manipulation | 30 | "the task is never complete" |

Unicode normalization (zero-width characters, homoglyphs) adds further protection against bypasses; more than three zero-width characters in the original text add 20 to the score.

> Note: content scraped by L1/L2 does not currently go through a separate content-classification scan. Protection at the `web_fetch` layer is SSRF validation (scheme / internal IP / metadata endpoint / DNS rebinding / per-redirect re-validation), a 5MB size cap, and rate limiting.

### 3.2 Emergency stop

- In-channel safe words: `!STOP` / `!停止` (single scope) and `!STOP ALL` / `!全部停止` (global) to trigger; `!RESUME` / `!恢復` to recover. These are handled by the failsafe system and require admin privileges.
- The dashboard has no working E-Stop control in this build: its `browser.emergency_stop` RPC answers with the error "Browser automation features require the Pro edition". Halt state lives in memory in the failsafe manager (`crates/duduclaw-security/src/failsafe.rs`), so a gateway restart also clears it.

### 3.3 Tool approval (HITL ApprovalBroker)

High-risk operations go through the unified ApprovalBroker (`approvals.db`; a TTL expiry is treated as a denial, fail-closed):
- `agent.toml [capabilities] approval_required_tools` declares which tools require approval; `irreversible_tools` always ask, `maybe_irreversible_tools` ask when a model judge says the call may be irreversible (fail-closed). The lists are read for the agent making the call. Before v1.67.0, calls an agent made through its own MCP server were checked against the gateway's internal key name instead, so these three lists had no effect there.
- Requests for an ordinary tool are filed as kind `mcp_call` and worded as a tool call; install-class tools keep kind `mcp_install`. The eight `computer_*` tools are asked about by the gateway's computer-use route instead (always, for any of the three lists), so a human is never asked twice.
- The autopilot `require_approval` action goes through the same broker
- See the observability / capabilities docs for details

### 3.4 User pairing

Channel-level user access control, stored in `channel_settings` (global scope, per channel type):
- `require_pairing = "true"`: unpaired users must pair before they can chat
- `allowed_users` / `blocked_users`: JSON array allowlist / blocklist
- Flow: an admin runs the MCP tool `pairing_manage` (action=generate) to produce a 6-digit pairing code (valid for 5 minutes) → the user sends `/pair <code>` in the channel → once approved, it's persisted to `~/.duduclaw/access_control.json`
- Brute-force protection: 5 failures locks a code, 15 cumulative failures across regenerations, constant-time comparison, codes stored as SHA-256

### 3.5 Screenshot masking

Every screenshot the L5 orchestrator takes goes through `capture_masked_screenshot` (`crates/duduclaw-gateway/src/computer_use_orchestrator.rs`) before it reaches the model or the audit folder:

- It asks the container for the bounding boxes of elements that match three fixed CSS selectors, `input[type=password]`, `.credit-card` and `[data-sensitive]` (`MaskingConfig::default()` in `crates/duduclaw-gateway/src/computer_use.rs`), and paints them black.
- Detection runs `docker exec <container> duduclaw-eval-dom '<js>'`, with a 10-second timeout on the gateway side. The helper (`container/scripts/duduclaw-eval-dom`, Python standard library only) connects to Chromium's DevTools port on 127.0.0.1 inside the container and evaluates the expression in the single visible page, with its own 5-second limit. The expression, the geometry read-back and the `document.visibilityState` check all run in an isolated world (`Page.createIsolatedWorld`, `container/scripts/duduclaw_cdp.py`), so a page that redefines those functions cannot move the rectangles. The helper exits with status 3 for more than one visible page and 1 for other failures; the gateway maps the status (never the text) to `mask_reason` `several_pages` or `helper_failed`. More than one visible page (for example a window opened with `ctrl+n`, which no browser policy prevents) makes the helper fail and the screenshot fully masked until the next `computer_navigate` closes the extra pages. It converts the rectangles from CSS pixels to screenshot pixels by the device pixel ratio (so browser zoom is covered), rounds outward, pads each by 1 px, clips them to the screen and drops rectangles with no visible area. Measured on a test page: no sensitive pixel left uncovered, at most 2 px over-coverage per side, also after browser zoom.
- The helper exits non-zero, and the gateway then paints the whole screenshot black (fail closed), when: the browser is not running, no page or more than one page is visible, the browser window is not at 0,0 or does not cover the display, the visual viewport is pinch-zoomed or scrolled, the JavaScript throws, or the 5-second limit passes.
- Not detected: browser UI that is not part of the page (the zoom bubble, permission prompts, autofill drop-downs, alert dialogs), content inside cross-origin iframes, and content inside shadow DOM. A sensitive field shown in any of these stays visible in the screenshot.
- After DOM masking, the gateway reads the focused window's title. If it contains a credential marker (`1password`, `bitwarden`, `lastpass`, `keepass`, `keychain`, `密碼`, `password`, `credential`, `ssh`, `gpg`, `pgp`), the whole screenshot is masked. If the title cannot be read (command error, timeout, non-zero exit, non-UTF-8 output), the whole screenshot is masked as well (fail closed). An empty title that was read successfully does not trigger masking.

The selectors and the fill color are not configurable: nothing reads a masking key from `CONTRACT.toml` or `agent.toml`.

---

## 4. Browser test suite

There is no browser test command: `duduclaw test` takes an agent name and an optional `--bank` file and red-teams the agent's contract and input scanner (see the [CONTRACT.toml spec](../spec/contract-toml-spec.md)); it has no `--browser` flag. The browser code is covered by unit tests in the source tree:

```bash
# L1 / L2 fetch path: URL validation, SSRF gate, cache
cargo test -p duduclaw-gateway web_fetch

# L5 screenshot masking, action parsing
cargo test -p duduclaw-gateway computer_use

# L5 audit log and screenshot storage
cargo test -p duduclaw-gateway screenshot_audit
```

---

## 5. Audit and monitoring

### 5.1 Where browser and computer-use activity is recorded

| Record | Written by | What it holds |
|---|---|---|
| `~/.duduclaw/tool_calls.jsonl` | the MCP dispatcher (`crates/duduclaw-cli/src/mcp/dispatch.rs`) | One row per call of a state-changing tool, with masked input and result text. This covers all eight `computer_*` tools, with reduced input: `computer_type` as `chars=<n>`, `computer_navigate` as host and path length (no path, no query), `computer_screenshot` without the image. A row here is a call; whether it ran is in the next row. `web_fetch_cached` and `web_extract` are read-only and are not written here. |
| `~/.duduclaw/audit/browser/audit.jsonl` | the computer-use session manager (`crates/duduclaw-gateway/src/screenshot_audit.rs`) | Hash-chained rows (`_prev_hash`), tier `L5a`. Sessions write `session_start`, `screenshot` (with `fully_masked` and `mask_reason`: `several_pages`, `title_sensitive`, `title_unreadable`, `helper_failed`, or null), one row per executed action (`left_click`, `type`, `key`, `scroll`, …) with its risk rating, `navigate` (with `url` = `https://<host><path>` without query or fragment, and `domain`), `action_refused` and `session_end`. Typed text appears only as a character count. Rotates to `audit.jsonl.old` past 16 MiB, keeping the chain. L1 and L2 do not write here. |
| `~/.duduclaw/audit/browser/screenshots/<agent_id>/<UTC timestamp>.png` | the same manager | The masked image of each `computer_screenshot` call. |
| `~/.duduclaw/security_audit.jsonl` | input guard, task sandbox and other security events | Blocked inputs and `task_sandbox_*` events (section 2.5). |

```bash
# Recent L5 actions
tail -20 ~/.duduclaw/audit/browser/audit.jsonl | jq .

# Recent computer_* tool calls
grep '"tool_name":"computer_' ~/.duduclaw/tool_calls.jsonl | tail
```

In a channel, `/replay [n]` (default 5) lists the agent's last `n` rows of `audit/browser/audit.jsonl`. There is no `browser_audit_log` MCP tool, and the dashboard's `browser.audit_log` RPC answers with the same "Pro edition" error as `browser.emergency_stop`.

### 5.2 Screenshot retention

Saved screenshots are kept for 7 days: the computer-use sweep (gateway start, then every 10 minutes) deletes older files. Each employee's folder also holds at most 500 files and 200 MiB; saving a new screenshot deletes the oldest ones past either limit. No dashboard page shows them.

---

## 6. Troubleshooting

### The agent has no headless-browser tools
```bash
# Confirm a browser server is registered for this agent
cat ~/.duduclaw/agents/my-bot/.mcp.json
```
Nothing adds one automatically. Write the entry yourself (section 2.4) or install `playwright` / `browserbase` for that agent from the dashboard's MCP marketplace (`marketplace.install`). Then check that `[capabilities] denied_tools` does not block its tools.

### Task sandbox won't start
```bash
# Confirm Docker is running
docker info

# Preconditions of the task sandbox (Docker, image, sandboxed agents)
duduclaw doctor

# Confirm the sandbox image is on this machine (it is never pulled automatically)
docker image inspect ghcr.io/zhixuli0406/duduclaw:v<version>
```
See the [Task sandbox guide](task-sandbox.md) for the error table.

### Computer use cannot start
`computer_session_start` returns an error that starts with 「電腦操作無法啟動」 and names the cause:
- The image is not on this machine: it names the image and the two remedies, `docker pull <image>`, or build locally (`docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .` from the repo root) and set `config.toml [computer_use] image = "duduclaw-computer-use:latest"`.
- Docker did not answer the presence check: start Docker and retry.
- `[computer_use]` in `config.toml` is invalid (unknown key, unusable image reference, unparsable file): fix it; there is no fallback to the default image.

```bash
# The 電腦操作 row: image in use, whether it is present, which agents have computer_use = true
duduclaw doctor
```
The row passes when no agent uses computer use, and warns when one does and the image is missing, Docker is unreachable or the config is invalid. It also warns, and names them, when any agent is still set to `computer_use_mode = "native"`: delete that key or set it to `"container"`. The row also lists each agent's usable allowlist hosts and ignored entries.

Other common tool answers:
- The tool says it cannot connect to the gateway at `127.0.0.1:<port>`: the tools need the gateway running; they never start a container themselves.
- The tool says access was refused (`unauthorized`): the MCP process was not started by the gateway (no internal key or agent token in its environment), the clocks differ by more than 60 seconds, or `~/.duduclaw/identity.key` is missing. The gateway logs the exact reason at debug level only.
- `computer_navigate` refused: read the message. With no `allowed_domains` it names the setting to add; otherwise it lists the hosts this session can open. A host added to the allowlist after the session started needs a new session.

### Computer-use container exits at start
```bash
# Read the start-up log of a leftover session container, if one is still there
docker logs <container>

# Or reproduce the start by hand with network access and an allowlist
docker run --rm -e ALLOWED_DOMAINS=example.com <computer-use-image>
```
`[domain-filter] FATAL: cannot install the default-deny egress policy` means the container has a network route but no `NET_ADMIN` capability, so the filter refused to run with unfiltered egress. The gateway only adds the capability together with `ALLOWED_IPS` (a tool session with resolved allowlist hosts) or a non-empty `ALLOWED_DOMAINS`; setting both is refused by the filter. A container started by hand with a network needs `--cap-add=NET_ADMIN`; with `--network=none` it starts without the capability.

### Computer-use screenshots are fully black
The masking helper failed, so the whole screenshot was masked (section 3.5). Run it by hand against the session's container:
```bash
docker exec <container> duduclaw-eval-dom 'JSON.stringify([])'
```
It prints `[]` when it works. Otherwise its stderr gives the reason (browser not running, no visible page, several visible pages, window not at 0,0, zoomed visual viewport, timeout). An older image without the helper prints an "executable file not found" error: pull the current image or rebuild it (section 2.6).

### Emergency Stop won't recover
Send `!RESUME` (or `!恢復`) in the channel from an admin account. The halt state is held in memory, so restarting the gateway also clears it. There is no signal file to delete and no `emergency_stop` MCP tool.
