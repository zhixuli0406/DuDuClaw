# Browser Automation and Computer Use

> From a plain HTTP fetch to a full virtual desktop: two built-in fetch tools, an optional external browser server, and computer-use sessions in an isolated container, driven by the employee through eight `computer_*` MCP tools. There is no automatic router.

---

## A note on history

Earlier versions of this page described a **5-layer automatic router** that escalated L1 → L2 → L3 → L4 → L5 on its own, including an "L4 Sandbox Browser" tier.

That router (`browser_router.rs`) was written in 2026-04, never acquired a call site, and never appeared in the CHANGELOG. It was deleted in 2026-09. The L4 tier it described never existed as a separate capability at all.

What ships is simpler: two built-in MCP fetch tools that the agent chooses between, an optional external MCP server for headless browsing, and computer-use sessions in a container. Cost discipline comes from the agent's own judgment and the tool descriptions, not from a routing engine.

---

## What actually ships

### L1 — `web_fetch_cached`

A plain HTTP GET with SSRF protection, disk caching and rate limiting. Returns status, content type and body, truncated at 60k characters. `ttl_seconds` controls the cache (default 86400).

The SSRF gate (`web_fetch::validate_url` + `resolve_public_addrs`) rejects internal hosts and cloud metadata endpoints, resolves DNS at request time, requires every resolved address to be public, and then pins the answer for the request. The same gate protects the resident-sensing `http_poll` and `websocket` source kinds.

"Public" has one definition in the whole workspace, `duduclaw_core::net_addr::is_public_ip`, used by `web_fetch_cached`, `web_extract`, media downloads, the resident-sensing sources, the relay URL check, MCP server import, the skills RPC, the Odoo URL check, the wiki-federation peer check and computer-use pinning. Refused IPv4 blocks: `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10`, `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.0.0.0/24`, `192.0.2.0/24`, `192.168.0.0/16`, `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4`, `240.0.0.0/4`. For IPv6 only global unicast (`2000::/3`) is public, minus Teredo `2001::/32` and the documentation blocks `2001:db8::/32` and `3fff::/20`. Forms that embed an IPv4 address (IPv4-mapped `::ffff:0:0/96`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) are judged by that embedded address, so `http://[::ffff:127.0.0.1]/` is refused; IPv4-compatible `::/96` and local-use NAT64 `64:ff9b:1::/48` are refused as a class. Before this change the web tools' check knew only the private, loopback, link-local and `0.0.0.0` IPv4 ranges plus `::1` and `fc00::/7`, so an address such as `[::ffff:127.0.0.1]` passed it and reached loopback.

Use it for: documented APIs, JSON endpoints, server-rendered pages you only need the raw bytes of.

### L2 — `web_extract`

Fetches a URL through the same cached, SSRF-validated path, then extracts elements with a CSS selector. Output format is `text` (default), `html`, or `json` (structured, with attributes and children).

Use it for: traditional server-rendered sites where the content is in the initial HTML.

Neither L1 nor L2 executes JavaScript. A single-page app returns its empty shell.

### L3 — Playwright or Browserbase, as an external MCP server

For JavaScript-rendered pages, the agent gets a headless browser by having a browser MCP server registered in its own `<agent_dir>/.mcp.json`. This is a per-agent MCP server, separate from the globally registered DuDuClaw MCP server.

Nothing writes this entry automatically. `crates/duduclaw-agent/src/mcp_template.rs` contains `playwright_mcp_config`, `browserbase_mcp_config`, `ensure_playwright_in_config` and `ensure_browserbase_in_config`, but no code calls them. The operator either writes the entry by hand or installs the `playwright` / `browserbase` item for that agent from the dashboard's MCP marketplace (`marketplace.install`, admin only). A hand-written entry in the shape `playwright_mcp_config(true)` would build:

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

For Browserbase, `browserbase_mcp_config` and the marketplace `browserbase` card build the same entry: `npx -y @browserbasehq/mcp` with `BROWSERBASE_API_KEY`, `BROWSERBASE_PROJECT_ID` and `GEMINI_API_KEY` (the last one is for the server's default model) in `env`. The values are entered when the card is installed (`marketplace.install` takes them as `env: { NAME: value }`; an install with a missing value is refused and the message names the variable) and are written into the agent's `.mcp.json` as literal strings, the way `claude mcp add -e` stores them. The file is owner-only (0600), and `mcp.list` reports each value only as `set`, `not_set` or `reference`. A `${NAME}` reference does not work here: the CLI expands it from its own environment, and the gateway starts each employee's CLI with an allowlisted environment that drops every `*_API_KEY`-shaped name. Before this change the card wrote such references, so the server started without keys. Before v1.67.1 the marketplace cards and the generated Playwright entry named packages that do not exist on npm (`@anthropic-ai/mcp-server-playwright`, `@anthropic-ai/mcp-server-browserbase` and the other `@anthropic-ai/mcp-server-*` names), and the generated Browserbase entry used the deprecated `@browserbasehq/mcp-server-browserbase`; an entry already written with one of those names is left as it is and fails when the CLI tries to start it, so edit or reinstall it.

It is not part of the DuDuClaw binary and there is no fallback from L2 into it. The agent's `allowed_tools` / `denied_tools` decide whether the agent may call it.

### L5 — Computer Use

Computer use is one path: the employee calls eight `computer_*` MCP tools and decides every step itself. It needs `[capabilities] computer_use = true` on the employee. This works from any runtime whose CLI gets the DuDuClaw MCP tools (see [Multi-runtime](13-multi-runtime.md)). No Anthropic API key is involved.

| Tool | Parameters | What it does |
|---|---|---|
| `computer_session_start` | `task` (string, optional, kept in the audit log), `width` (integer, 320–1920), `height` (integer, 240–1200); size defaults come from `[capabilities.computer_use_config]` (1280x800) | Starts the employee's session and reports the display size, the limits, whether high-risk actions can be confirmed in a chat, and which sites `computer_navigate` can open |
| `computer_screenshot` | none | Returns the masked screen as an MCP image block (PNG) plus a text line with actions used and time left. When the whole picture had to be hidden, the text says so, why, and what to do next. Actions never return a screenshot, so the agent calls this to check a result |
| `computer_click` | `x`, `y` (integers, screenshot pixels from 0), `button` (`left` default or `right`), `double` (boolean, left button only) | One click |
| `computer_type` | `text` (string, 1–2,000 characters) | Types into the focused element |
| `computer_key` | `key` (string, letters, digits, `+`, `-`, `_`, at most 64 characters, for example `Return`, `ctrl+s`) | Presses a key or combination; an invalid key is refused, never replaced |
| `computer_scroll` | `x`, `y` (integers), `direction` (`up` or `down`, default `down`), `amount` (integer 1–20, default 3) | Scrolls with the pointer at that pixel |
| `computer_navigate` | `url` (string) | Opens a page in the container's browser. The browser runs in kiosk mode with no address bar, so this is the only way to open a page |
| `computer_session_stop` | `session_id` (string, optional) | Ends the session and removes its container |

How a call travels: the tools run in `duduclaw mcp-server`, a separate process per agent CLI that does not own any container. Each call becomes a signed `POST` to the gateway on loopback (`/api/internal/computer-use`, body `{op: start | screenshot | action | stop | status, …}`), and the gateway runs every check and owns the container (`crates/duduclaw-gateway/src/computer_use_sessions/`). The gateway therefore has to be running; when it is not, the tool returns an error saying so. A tool reports a session as started or an action as executed only when the gateway says it was. [SECURITY.md](../../SECURITY.md) describes how the route authenticates its caller.

Session rules:

- **One session per employee.** A second `computer_session_start` while one is live is refused and names the live session. Only that employee can see or drive it.
- **Five sessions at once across the gateway.**
- **Limits.** A session ends after `max_session_minutes` (default 10), after 2 minutes without any operation (time spent waiting for an approval, an open dashboard viewer or a human takeover do not count as idle; with `keep_alive_minutes` the container is paused instead, see [Keep-alive, live view and takeover](#keep-alive-live-view-and-takeover)), or on `computer_session_stop`. Each click, type, key, scroll and navigate counts against `max_actions` (default 50); screenshots do not. After the budget is spent the session can still take screenshots and stop.
- **Refused callers and modes.** Ephemeral agents (including Team-as-Agent role members, which copy their parent's capabilities) cannot start a session. An employee whose `[capabilities] computer_use_mode` is `"native"` is refused with error code `native_unsupported` and a message saying the mode was removed and to delete the key or set it to `"container"`; there is no silent fallback to a container. `"auto"` or no key behaves as `"container"`.
- **Threat level.** A new session starts only while `~/.duduclaw/threat_level` is GREEN (or absent). YELLOW allows only screenshots and stop; RED ends every session. A chat message that is exactly an emergency-stop word (`停止`, `stop`, `abort`, `やめて`, …) ends every computer-use session.
- **Cleanup.** A reaper checks every 15 seconds for sessions past their deadline or idle limit. Containers carry a label naming the owning DuDuClaw home and a deadline label; a sweep at gateway start and every 10 minutes removes this home's containers that have exited or are more than 10 minutes past their deadline, and removes nothing when Docker cannot be listed.
- **Request limits.** At most 120 requests per employee per minute, request bodies up to 64 KiB.

Gates the gateway applies itself, because the route can be reached without going through the MCP dispatcher:

- `[capabilities] denied_tools` / `allowed_tools` and `scoped_tools` (a listed tool needs an active task-scoped grant) are checked for every operation, under the tool's own name.
- A tool listed in `approval_required_tools`, `irreversible_tools` or `maybe_irreversible_tools` waits for a human decision through the ApprovalBroker (up to 300 seconds; no answer counts as a refusal). A maybe-irreversible `computer_*` tool is always asked about; there is no model judge here. The approval text names the tool; for `computer_type` it gives only the number of characters, and for `computer_navigate` only the validated host (no path or query). A `computer_navigate` URL is validated before anything is asked, so a URL that would be refused never creates an approval request. The MCP-side approval gate skips these eight tools so nobody is asked twice.
- Every action is risk-rated by fixed rules (`risk_detector.rs`). An action is refused when the focused window's title matches `blocked_actions` (default `delete_file`, `terminal`, `system_preferences`), when it matches a `CONTRACT.toml` `[must_not] rules` entry, or when the focused window's title cannot be read. A high-risk action (input aimed at a sensitive field, typed text that looks like a credential, or a window outside a non-empty `allowed_apps`) needs a person to confirm it in the conversation's channel within 60 seconds when the tool call comes from a turn that is answering a channel conversation; the gateway finds that channel from its own record of the employee's live turns, never from the request. With no channel to ask (a scheduled run, a delegated task) the high-risk action is refused. `auto_confirm_trusted = true` skips the confirmation.

#### What happened to the chat-triggered loop

Earlier versions also had a second path: when a channel message looked like a computer-use request (a keyword list), the gateway itself called the Anthropic Messages API's `computer_20251124` tool, executed the returned actions, and posted progress and screenshots to the channel; a `computer_use_mode = "native"` variant was meant to drive the host desktop. Both are removed. Neither could complete a session in any released build: in v1.66.1 the container used an image that nothing built or published, whose Dockerfile could not start (a snap-stub browser, an entrypoint that aborted under `--network=none`, no window manager), and native mode called the same container start before any host action.

Now a channel message that mentions clicking or screenshots takes the normal reply path, and the employee decides whether to call the tools. Computer use reads no Anthropic API key (the direct-API reply fallback still does).

**The image.** `container/Dockerfile.computer-use` builds a Debian (`debian:trixie-slim`) image of about 1.09 GB: Xvfb, the `openbox` window manager, Chromium (the real Debian `chromium` package) in kiosk mode, an on-demand VNC server for the dashboard's live view (unix socket only), `xdotool` and `scrot` for actions and screenshots, the domain filter, the `duduclaw-eval-dom` masking helper and the `duduclaw-navigate` helper that `computer_navigate` uses. The browser and window manager run as an unprivileged `sandbox` user. Until 2026-10-01 this image never worked (its Ubuntu browser package was a snap stub that cannot start in a container, the entrypoint aborted under `--network=none`, and there was no window manager or masking helper); it was repaired and verified on that date.

The release workflow `.github/workflows/computer-use-image.yml` publishes it. It runs on git tags `v*` or manual dispatch, builds `linux/amd64` and `linux/arm64` on native runners, and smoke-tests each build with the gateway's own container flags (window manager up, a screenshot taken, the DOM helper returning `[]`, `duduclaw-navigate` failing cleanly without a network) before pushing `ghcr.io/zhixuli0406/duduclaw-computer-use:<tag>` and `:latest`. The workflow first runs on the first release tag after v1.66.1, so no published image exists for v1.66.1 or earlier. `scripts/release.sh verify` does not check this image.

**Which image runs.** By default `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway version>`. One global key overrides it: `config.toml [computer_use] image = "<ref>"` (a digest reference is accepted). There is no per-agent image setting. An invalid `[computer_use]` section (unknown key, unusable reference, unparsable `config.toml`) makes computer use unavailable with a message; it never falls back to the default. The image is never pulled automatically: `docker run` carries `--pull never`, and a presence check runs before a session starts. When the image is missing, the message names it and gives two remedies: `docker pull <image>`, or build it locally from the repo root and point the override key at the local tag:

```bash
docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .
```

```toml
[computer_use]
image = "duduclaw-computer-use:latest"
```

Upgrade note: a machine that only has a locally built `duduclaw-computer-use:latest` (the old default) must either pull the versioned image or set the override key.

**`duduclaw doctor`.** The 電腦操作 row shows the image that would be used, whether it is present locally, and which employees have `[capabilities] computer_use = true`. It passes when no employee uses computer use, and warns when someone does and the image is missing, Docker is unreachable or the config is invalid. It also warns, on its own, when any employee is still set to `computer_use_mode = "native"`, and names them. It lists, per employee, how many allowlist hosts are usable and how many entries were ignored.

**Screenshot masking.** Before a screenshot reaches the model or the audit folder, the gateway asks the helper for the on-screen rectangles of `input[type=password]`, `.credit-card` and `[data-sensitive]` elements (fixed defaults, not configurable) and paints them black. The helper evaluates that query in the single visible page through Chromium's DevTools port, which listens on 127.0.0.1 inside the container only. All helper JavaScript runs in an isolated world (`Page.createIsolatedWorld`), so a page that redefines `document.visibilityState`, `querySelectorAll` or `getBoundingClientRect` cannot change the rectangles; this was checked with such a page. Chromium covers the whole virtual display from 0,0 with device scale factor 1, so page coordinates equal screenshot pixels; rectangles are scaled by the device pixel ratio, rounded outward, padded by 1 px and clipped to the screen. Measured on a test page, no sensitive pixel was left uncovered and over-coverage was at most 2 px per side, also after browser zoom. If the helper fails for any reason (browser not running, no page or several visible pages, window moved or not covering the display, zoomed visual viewport, JavaScript error, 5-second limit, or the gateway's 10-second timeout), the whole screenshot is masked. The helper exits with status 3 when more than one page is visible and 1 for any other failure, and the gateway decides the reason from that status, never from the helper's text. Not detected: browser UI that is not part of the page (zoom bubble, permission prompts, autofill drop-downs, alert dialogs), cross-origin iframes and shadow DOM content. The screenshot itself is written to `/tmp/duduclaw-root/screen.png`; `/tmp/duduclaw-root` is owned by root with mode 0700 and is created by the entrypoint before any browser process starts, so the browser user cannot pre-create or swap the file. The Chromium log is kept there too.

After DOM masking the gateway reads the focused window's title. A title containing a credential marker masks the whole screenshot. A title that cannot be read (command error, timeout, non-zero exit, non-UTF-8 output) also masks the whole screenshot. An empty title that was read successfully leaves the screenshot as it is.

A fully masked screenshot says so. The gateway's answer carries `fully_masked` (true/false) and `mask_reason` (`several_pages`, `title_sensitive`, `title_unreadable`, `helper_failed`, `injection_suspected`, `text_unscanned`, or none; the last two are described under [Injection pause](#injection-pause)); when the helper fails, the title is not consulted. The tool text then tells the AI that the whole picture was hidden for safety and what to do: for several windows, call `computer_navigate` again, which closes the extra windows; for a sensitive or unreadable front window, the screen cannot be shown while that window is in front; otherwise take another screenshot, and if it keeps happening stop the session and start a new one. Partially masked and unmasked screenshots keep the usual text.

**Network and the site allowlist.** A session has no network unless the employee has a site allowlist in `agent.toml`:

```toml
[capabilities.computer_use_config]
allowed_domains = ["example.com", "docs.example.com"]
```

- Entries are exact hostnames, compared after trimming and lowercasing. Wildcards (`*.example.com`), IP addresses, numeric forms such as `0x7f000001`, ports, paths, URLs and `user@host` are ignored, as is everything after the first 20 distinct hosts. A subdomain is not covered by its parent: `example.com` does not allow `docs.example.com`. Ignored entries are logged, counted in the `computer_session_start` result, and reported by `duduclaw doctor`. A value that is not an array is read as an empty list.
- With no usable entry the container runs with `--network=none`, and `computer_navigate` is refused with a message telling the operator which setting to add.
- At session start the gateway resolves each host itself (5-second limit per host). A host is skipped for this session when it does not resolve, when any address in the answer is not a public address, or when the answer has no IPv4 address. The start result lists the reachable and the skipped hosts, without addresses.
- For the hosts that resolved, the container is attached to Docker's default bridge explicitly (`--network bridge`) and gets `--add-host <host>:<address>` for each one and `NET_ADMIN`, so the entrypoint can install the egress filter before it drops privileges. The filter (`container/scripts/domain-filter.sh`, pinned-address mode) drops all outgoing traffic by default, allows only TCP 443 to the pinned addresses, and refuses DNS, so names resolve only through the pinned entries. Loopback is limited to `127.0.0.1` and `::1`; Docker's embedded resolver `127.0.0.11` is rejected (on a user-defined Docker network it would otherwise answer any name, which is a way to send data out through DNS queries). A blocked connection is refused at once rather than left to time out: the filter lets its own TCP reset and ICMP unreachable answers through on loopback, so an off-list address on port 443 or a pinned address on port 80 is refused immediately and an off-list name fails to resolve immediately.
- `computer_navigate` accepts only `https://` URLs with no user name or password, port absent or 443, a host that is exactly one of this session's reachable hosts (an IP address is refused), at most 2,000 bytes, and no whitespace or control characters. The URL is checked before any approval is asked for.
- Links on a page that lead to a site off the allowlist do not load: the browser shows its own error page. A site that redirects to another domain fails the same way.

Verified on a live session: opening `https://example.com/` and seeing it in a screenshot; refusals for an unlisted host, a look-alike host (`example.com.evil.test`), `http://`, a non-443 port, a URL with a user name, and `file://`; inside the container the allowlisted name resolved to the pinned address, other names did not resolve, and other addresses and ports were refused; clicking a link to a site off the allowlist showed the browser's DNS error page; the container was removed at stop.

What the allowlist does not protect against:

- An allowlisted site receives whatever the AI types or submits on it. The allowlist decides where data can go, not what is sent.
- A site that proxies or redirects through its own domain (a translation proxy, a URL shortener on the same host, a search engine's cache) can relay content from or to elsewhere.
- Addresses are pinned when the session starts. A site whose address changes during the session stops working until a new session starts.
- The allowlist is per employee.

**Browser policy.** Chromium runs with a managed policy (`container/scripts/chromium-policy.json`, installed as `/etc/chromium/policies/managed/duduclaw.json`); each entry was checked as accepted on `chrome://policy` in the Chromium of the current image. Pages cannot request local-network or loopback access and no permission prompt is shown (a page's fetch or WebSocket to `127.0.0.1:9222` fails at once). Name resolution goes through `/etc/hosts` only (built-in DNS client and DNS-over-HTTPS off). Incognito, guest and add-person windows are off, file dialogs and printing are off, downloads and pop-ups are blocked, the notification, geolocation, USB, serial, HID, file-system, direct-sockets and window-management permissions are blocked, audio, video and screen capture are off, the password manager and autofill are off, the bookmark bar and bookmark editing are off, and the new tab page is `about:blank`. The URL blocklist covers `file://`, `chrome://`, `chrome-untrusted://`, `devtools://`, `view-source:` and `javascript://`. `DeveloperToolsAvailability` is deliberately not set, because it would also disable the loopback DevTools protocol that the masking and navigation helpers use; the DevTools front end is blocked through the `devtools://` entry instead.

Known limit: no policy stops `ctrl+t` or `ctrl+n`, and `ctrl+n` opens an ordinary window with an address bar (its network reach is still only the pinned hosts). While more than one page is visible, screenshots are fully masked with `mask_reason` `several_pages`, and the tool text tells the AI to call `computer_navigate` again, which closes the extra pages and keeps one.

**Other egress mode.** The orchestrator still has an older egress mode that resolves an allowed-domains list inside the container (`ALLOWED_DOMAINS`, with `NET_ADMIN` added only in that case); nothing in the gateway enables it. In any mode, if the filter cannot install its default-deny rule while the container has a non-loopback route, the container refuses to start instead of running with unfiltered egress.

Use it for: anything a person sitting at a computer could do — logins, drag-and-drop, visual pattern recognition. It is the slowest and most expensive option by a wide margin.

**Not implemented, and not yet verified.**

- No drag, mouse-move or hover tool, and no file upload or download tool.
- Computer use never drives the host desktop; native mode was removed.
- Verified with a real model (Claude Sonnet 5.5, through the real MCP server): it started a session, opened an allowlisted page, read the page text from the screenshot, clicked a link at coordinates taken from the screenshot, recognised the off-allowlist error page, and stopped the session. Not yet verified: a session started from inside a gateway-spawned agent turn answering a real channel.
- The high-risk confirmation in a chat has not been exercised with a real channel. On WebChat the confirmation prompt may not reach the person (unverified).
- A channel message containing computer-use keywords taking the normal reply path was not exercised on a real channel.
- The image workflow has not yet had a GitHub Actions run with these changes.
- The egress filter's IPv6 rules are installed but were not exercised with traffic (the default bridge has no IPv6 route).
- Only Docker Desktop on macOS (arm64) was exercised; no amd64 host and no native Linux Docker Engine. Windows (WSL2) hosts are unverified.
- Download and print blocking were confirmed as accepted policies and by their shortcut keys opening nothing, not by triggering a real download.

---

## Security: deny by default

Every tier past L2 needs explicit authorization in `agent.toml`:

```toml
[capabilities]
computer_use = false        # computer-use sessions and the eight computer_* tools
browser_via_bash = false    # launching a browser from a Bash tool call
allowed_tools = [...]       # allowlist
denied_tools = [...]        # denylist

[capabilities.computer_use_config]
allowed_domains = []        # sites computer_navigate may open; empty = no network
```

- `computer_use = false` (the default) means the gateway never starts a computer-use session for that employee, the eight `computer_*` tools are hidden from `tools/list`, and a direct call is refused, checked fail-closed: an absent file, malformed TOML, or a wrong-typed key all deny. The gateway re-reads the setting during a session; turning it off ends the session at its next operation.
- `denied_tools` is passed to the CLI as `--disallowedTools` and is *also* enforced at the MCP dispatcher front door.
- `browser_via_bash` no longer sets an environment flag. The `bash-gate.sh` allowlist that used to read `DUDUCLAW_BROWSER_VIA_BASH` was removed with the rest of the shell hooks in `ba015a48`. The capability still takes effect: it feeds `disallowed_tools()` and `CapabilitiesConfig::sandbox_level()`, which is how the codex and gemini runtimes decide between `ReadOnly` and `WorkspaceWrite` sandboxes.
- The restriction knobs that only ever existed as fields on the dead router (trusted/blocked domains, per-session page caps) were never implemented in that form. What exists instead: the per-employee site allowlist (`allowed_domains` above), the session action budget, the high-risk confirmation in the chat, and approval gating through `ApprovalBroker` and `agent.toml [capabilities] approval_required_tools` / `irreversible_tools` / `maybe_irreversible_tools`, which applies to the `computer_*` tools like any other tool. Screenshot audit is not a switch: it is always written (see the audit bullet below).

---

## Rough cost comparison

| Tier | Startup | Memory | Executes JS | Needs a container |
|---|---|---|---|---|
| L1 `web_fetch_cached` | ~0 ms | ~1 MB | no | no |
| L2 `web_extract` | ~0 ms | ~5 MB | no | no |
| L3 Playwright / Browserbase MCP | seconds | hundreds of MB | yes | no (external process or cloud) |
| L5 computer use | ~10 s | 500 MB+ | yes | yes |

Reaching for L5 when L1 would answer the question is the expensive mistake, and it is the agent's to avoid: nothing in the platform stops it.

---

## Interaction with other systems

- **Container isolation** — L5 runs in its own Docker container (default image `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway version>`, never pulled automatically): read-only root filesystem, a 256 MB tmpfs `/tmp`, 1 CPU, 512 MB memory, 512 processes (Chromium's threads count against this limit), and `--network=none` unless the session has resolved allowlist hosts, in which case `--network bridge`, the pinned hosts and `NET_ADMIN` are added for the domain filter (see the network paragraphs above). Every computer-use container runs with `--security-opt no-new-privileges`. This is launched by `computer_use_orchestrator` and is separate from the per-agent task sandbox (see the [Task sandbox guide](../guides/task-sandbox.md)).
- **Security defense** — capability enforcement and the audit trail are described in [05-security-defense.md](05-security-defense.md).
- **Resident sensing** — `http_poll` / `websocket` tick sources share L1's SSRF gate. See [41-resident-sensing.md](41-resident-sensing.md).
- **Audit log** — all eight `computer_*` tools are recorded in `tool_calls.jsonl`, with a reduced input: `computer_type` as its character count only, `computer_navigate` as host and path length (no query string), `computer_screenshot` without the image. `web_fetch_cached` and `web_extract` are read-only and are not. Sessions also append hash-chained rows (tier `L5a`) to `~/.duduclaw/audit/browser/audit.jsonl` (session start and end, each screenshot with `fully_masked` and `mask_reason`, each action with its risk rating, each navigation with host and path but no query, each refusal; typed text again only as a character count) and save masked screenshots under `~/.duduclaw/audit/browser/screenshots/<agent_id>/`. Screenshots are kept for 7 days and at most 500 files or 200 MiB per employee, oldest deleted first; `audit.jsonl` rotates to `audit.jsonl.old` past 16 MiB. Details: [Development guide, section 5](../guides/development-guide.md#5-audit-and-monitoring).

---

## The takeaway

The honest version is less impressive than the router story and easier to operate: four ways to touch the web, each with its own cost and its own switch, and an agent that has to choose. When the routing engine was deleted, this page had to stop describing one.

## Durable channel decisions

High-risk Computer Use confirmations use the inbound account and exact conversation/thread. Reply with `確認 <full UUID>` or `取消 <full UUID>`; questions use `回答 <full UUID> <answer>` and never grant tool permission. Bare yes/A/B cannot select a request. Before execution, the host rechecks the live screen, title, policy and cancellation gates. Restart invalidates old GUI approvals instead of replaying coordinates; an execution without a receipt becomes `uncertain` and requires Admin reconciliation. See the [decision guide](../guides/durable-channel-decisions.md) for the exact supported inbound routes and current limitations.

## Durable workspaces (files that outlive a session)

A computer-use container is removed when its session ends. To keep what an employee gathered, a session can attach a **computer-use workspace**: `computer_session_start` with `workspace = "new"` or an existing `ws-…` id. The gateway is the only writer (`computer_workspace_write`, into the workspace the employee's live session attached); `computer_workspace_list` and `computer_workspace_read` work without a session. Inside the container the files are read-only at `/workspace/files`, under a root-only tmpfs the browser's account cannot enter. Off by default: `config.toml [computer_use.workspaces] enabled = true` plus the employee's `[capabilities.computer_use_config] workspace = true`. macOS and Linux only.

Quotas, retention, the one-session lease, operator commands (`duduclaw ops computer-workspaces`, where every state-changing action needs an Admin's approval in the dashboard; emergencies go through the dashboard or the master switch) and the known limitations, including that owner isolation holds for the three workspace tools only and not for an employee with `Read` or Bash, are in the [computer-use workspaces guide](../guides/computer-workspaces.md).

## Keep-alive, live view and takeover

Three additions let a person watch an employee's computer and step in, and let a session survive a pause in the work. All of them only narrow what the employee can do. Code: `crates/duduclaw-gateway/src/computer_use_sessions/` (`keepalive.rs`, `live_view.rs`, `live_ops.rs`, `view_ws.rs`, `rfb.rs`), dashboard `web/src/components/agent/ComputerSessionPanel.tsx`.

### Keep-alive

```toml
[capabilities.computer_use_config]
keep_alive_minutes = 30      # default 0 = end after 2 idle minutes, as before; at most 240
takeover_idle_minutes = 10   # default 10; at most 60
```

With `keep_alive_minutes` above 0, a session idle for 2 minutes is not ended: the reaper pauses its container (`docker pause`). The next `computer_*` call of the employee, or a dashboard viewer, resumes it (`docker unpause`) and the call goes on. A session paused for longer than `keep_alive_minutes` ends. Everything else still applies while paused: the `max_session_minutes` deadline keeps counting, `max_actions` is unchanged, a RED threat level, the chat emergency stop, a revoked capability, a lost workspace lease and `computer_session_stop` end it. A paused session keeps its slot, so it counts toward the limit of five sessions. Lowering or removing `keep_alive_minutes` takes effect on a session that is already running; raising it does not. A container that will not resume ends the session (`resume_failed`). The sweep removes a paused container past its deadline label without the usual 10-minute grace, in the gateway holding the instance lock, after unpausing it; every gateway still removes it 10 minutes after the deadline. Pauses and resumes are audited (`session_pause`, `session_resume`).

### Live view

The employee page has a **Computer** tab (shown when the employee has `computer_use`). It shows whether the session is running, paused, held for review or under human control, the actions used, the time left, the keep-alive window and how many people are watching, refreshed every 5 seconds. **Watch** opens a live picture of the screen.

Who may do what (role and bindings re-read from `users.db` on every request and every viewer connection):

| | Admin | Manager bound to the employee | Account bound at Operator level or above |
|---|---|---|---|
| Status, watch, stop | yes | yes (any binding level) | yes |
| Take over, hand back, resume after an injection pause | yes | only when bound at Operator level or above | no |

How the picture travels: when someone watches, the gateway starts `x11vnc` inside the container (`docker exec … duduclaw-vnc start viewonly`, with a new random 8-character password passed on stdin). It listens only on a unix socket in the root-only `/tmp/duduclaw-root`, never on a TCP port (the helper refuses to keep running if anything listens on 5900–5999), so neither the network nor the browser's unprivileged account can reach it. Nothing is published on the host. The dashboard asks the `computer_sessions.view` RPC for a single-use ticket valid for 30 seconds and opens `/ws/computer-view?ticket=…` on the gateway, which checks the Origin like the dashboard socket, consumes the ticket, re-reads and re-authorizes the account, allows at most four viewers per session, and pipes the WebSocket to `docker exec -i … duduclaw-vnc-relay` (a unix-socket-to-stdio relay). The connection closes when the session ends and when the account loses access (checked every 30 seconds). The dashboard draws the picture with noVNC.

A published port on 127.0.0.1 relayed by the gateway was the other option; it was not used because any process of any user on the host could connect to such a port, while a `docker exec` relay needs access to the Docker daemon.

The gateway reads the viewer's side of the protocol (RFB 3.7/3.8): keyboard, mouse, clipboard and extended-key messages go through only from the account holding the takeover; requests to resize the screen (`SetDesktopSize`, which would break screenshot masking) and `xvp` shutdown or reboot requests are always dropped; anything it does not recognise closes the connection. The VNC server additionally runs with `-viewonly` while nobody holds a takeover, and with `-noremote -nocmds -nosel` always (no clipboard either way). Every viewer connection writes `view_start` and `view_stop` (who, how long, how many input messages were forwarded) to the browser audit log; the picture and keystrokes are not recorded.

### Take over and hand back

**Take over** gives the account a lease on the session. While the lease is held:

- every `computer_click`, `computer_type`, `computer_key`, `computer_scroll`, `computer_navigate` and `computer_workspace_write` of the employee is refused with `human_has_control` and a message telling it to wait; screenshots, status and stop still work, and screenshots are masked as always;
- the stream restarts in input mode with a new password, and only the holder's keyboard and mouse reach the screen;
- the session does not idle out or pause.

The lease ends on **Hand back** (the holder or an Admin), after `takeover_idle_minutes` without input from the holder, when the stream cannot be started, or when the session ends. The stream then returns to view-only. The employee gets a structured `continue` handoff through its working state (the same store `working_state_handoff` writes): who took over, for how long, how many inputs were sent, why it ended, the optional note from the person (passed through `input_guard`; a suspicious note is replaced by a marker) and the previous handoff, shortened, with the next step "take a screenshot first, do not reuse old coordinates". Audit rows: `takeover_start`, `takeover_end`. An action that had already passed its last check when the lease was taken finishes; the lease stops everything after it.

### Injection pause

Every `computer_screenshot` also reads the visible page's text (`document.body.innerText`, up to 32,768 characters) through `duduclaw-eval-dom` in an isolated world and runs it through `input_guard` at the blocking threshold. On a block-level hit the session is held:

- the screenshot is fully masked with `mask_reason` `injection_suspected`, and so is every screenshot until a person resumes;
- every action and workspace write of the employee is refused with `injection_suspected`;
- an Activity Feed entry and an L3 notice to the employee's notification channel name the matched rule categories;
- the audit row `injection_suspected` records the rule categories only, never the page text.

A person resumes it from the Computer tab (**Resume**, audited `injection_resume`). When the page text cannot be read, the screenshot is fully masked with `mask_reason` `text_unscanned` (unless it already was) and the session is not held. The text is read right after the screenshot, so a page that changes in between is scanned in its newer state. The guard's limits apply: it matches sentence shapes, so ordinary pages can trip it and well-disguised text can pass.

### Not covered and not verified

- No Docker was available where this was built: pause/resume and the stream were tested with fakes and unit tests only. The v1.72.0 image smoke did run and refused to start: Debian x11vnc 0.9.17 linked to libvncserver 0.9.15 still listened on IPv6 `::1:5900` with `-rfbport 0 -unixsock -localhost -noipv6`, so the helper stopped the server. A local arm64 probe of `-rfbport 0 -rfbportv6 -1 -unixsock` (the other flags unchanged) had no LISTEN on 5900–5999 and returned `RFB 003.008` on the unix socket. Both-arch image CI with that flag, and noVNC in a real browser, have not run.
- The noVNC picture in a real browser against a real container, and `docker pause` / `docker unpause` and `docker rm --force` on paused containers, are unverified.
- VNC authentication uses an 8-character password and DES; the real protection is the root-only unix socket and the gateway's authenticated, filtered relay. The password reaches the authorized viewer's browser.
- A Manager or Operator bound to the employee sees the whole screen (masking applies to the employee's screenshots, not to the live stream), including what a site displays.
- Switching between view-only and input mode restarts the VNC server, so open viewers disconnect and reconnect.
- Keep-alive, takeover and the viewer count live in the gateway process: a gateway restart ends sessions as before.
- Both settings are on the employee edit page (computer-use section, Admin only, through `agents.update`): keep-alive 0–240 minutes, takeover idle limit 1–60 minutes; the server refuses any other value or type.

