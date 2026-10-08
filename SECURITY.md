# Security Policy

## Supported Versions

| Version | Supported          | Patch Timing |
|---------|--------------------|-------------|
| Latest release (Commercial) | :white_check_mark: | Immediate |
| Latest release (Community)  | :white_check_mark: | Up to 30 days delay |
| Previous minor              | :white_check_mark: | Critical only |
| Older versions              | :x:                | Not supported |

## Reporting a Vulnerability

**Do NOT open a public GitHub issue for security vulnerabilities.**

### Preferred: GitHub Private Vulnerability Reporting

1. Go to the [Security Advisories](https://github.com/zhixuli0406/DuDuClaw/security/advisories) page
2. Click **"Report a vulnerability"**
3. Fill in the details and submit

### Alternative: Email

Send an email to **louis.li@dudustudio.monster** with:

- **Subject**: `[SECURITY] Brief description`
- **Description**: Detailed description of the vulnerability
- **Impact**: What can an attacker achieve?
- **Steps to reproduce**: Minimal steps to trigger the issue
- **Affected versions**: Which versions are affected?
- **Suggested fix**: If you have one (optional)

### Response Timeline

| Severity | Acknowledgment | Fix Target | Disclosure |
|----------|---------------|------------|------------|
| Critical | 24 hours | 72 hours | After fix released |
| High     | 48 hours | 7 days | After fix released |
| Medium   | 7 days | 30 days | After fix released |
| Low      | 14 days | Next release | After fix released |

### What to Expect

1. **Acknowledgment**: We will confirm receipt within the timeline above
2. **Assessment**: We will evaluate the severity and impact
3. **Fix**: We will develop and test a patch
4. **Release**: Commercial editions receive patches immediately; Community edition follows per the maintainers' internal security patch procedure
5. **Credit**: We will credit you in the advisory (unless you prefer anonymity)

## Scope

The following are in scope for security reports:

- **DuDuClaw core** (all 24 Rust workspace crates)
- **Web Dashboard** (React frontend)
- **Python bridge** (`python/duduclaw/`)
- **Container sandbox** escape or bypass (task sandbox, script sandbox, computer-use container)
- **SOUL Guard** bypass (SHA-256 drift detection)
- **Input Guard** bypass (prompt injection scanner)
- **Credential handling** key leakage (encrypted config, `secret_ref`, spawn-env allowlist)
- **AES-256 encryption** weaknesses
- **Dashboard authentication** bypass (JWT account login, admin token; these are the only two dashboard authentication paths)
- **Authorization** privilege escalation (delegation policy, MCP scopes, `[capabilities]`)
- **CONTRACT.toml** validation bypass
- **Browser automation / computer use** capability bypass (`[capabilities] computer_use`, `denied_tools`, SSRF gate), the internal computer-use route (`/api/internal/computer-use`) and the computer-use site allowlist

### Out of Scope

- Issues in third-party dependencies (report upstream, but notify us)
- Denial of service via resource exhaustion (unless trivially exploitable)
- Social engineering attacks
- Issues requiring physical access to the machine
- Vulnerabilities in the Claude API or Claude Code SDK itself

## Security Architecture

DuDuClaw implements defense in depth. Each layer below is what the code does today:

- **Layer 1**: Input Guard — prompt-injection scanner (`duduclaw-security/src/input_guard.rs`): 11 rule categories, risk score 0-100, blocks at 60 or above (a few categories block instantly). Runs on inbound chat messages, at the MCP dispatch front door, in conversation/profile/knowledge distillation (dropped on any rule match), on `user_profile_record`, Agent Mail (flagged), reminders, `migrate from` imports and expert-pack installs. Since v1.67.1 Chinese instruction override is matched as a phrase shape (override verb + scope word + instruction noun within one clause, 12 characters) instead of four exact strings, which released versions let through when a word was inserted; this also blocks some ordinary sentences of the same shape (see [docs/features/05-security-defense.md](docs/features/05-security-defense.md))
- **Layer 2**: Authorization — there is no RBAC engine (the module was removed). Who may command whom comes from the delegation policy (`duduclaw-core/src/delegation_policy.rs`), MCP scopes (a tool missing from the scope table requires Admin) and the per-agent `agent.toml [capabilities]` grants, all enforced at the MCP dispatcher; an agent-structure and org-field PreToolUse hook (`duduclaw hook agent-file-guard`) stops agents rewriting their own `SOUL.md`, their own `CONTRACT.toml` (no opt-in; operators change it through the admin-only `contract.update` RPC), `reports_to`, `[capabilities]` or any section of their own `agent.toml` outside the editable list, and keeps an employee's writes under the DuDuClaw home to its own agent directory and `attachments/`. The same hook refuses any write by an employee to a `.mcp.json` anywhere in its directory and to its CLI configuration (`.claude/` and `.claude.json` at any depth, the top-level `.codex/`, `.gemini/`, `.grok/`, `.agents/`; names compared case-insensitively): the Claude CLI starts every MCP server that file lists, so in every released version an employee with only Write/Edit could add a server whose command ran arbitrary commands as the operator's OS user at the next spawn. New servers come from the dashboard, an approved `mcp.install_request`, an expert pack or the operator. Before every spawn that hands an employee's `.mcp.json` to the Claude CLI (channel reply, dispatch, heartbeat proactive check, live `duduclaw eval`, a live fork's parent) and at start-up the gateway regenerates the DuDuClaw entry whole and refuses the spawn (audited as `mcp_config_unverified`) when the file cannot be confirmed; such a refusal is not counted against the account and is not retried with another account. The writers' lock lives under `<home>/locks/`, outside the employee directory. Entries added before the upgrade stay: `duduclaw doctor` lists every entry DuDuClaw did not write (name and command file name only) so the operator can confirm or remove it. An employee with unrestricted Bash can still change these files, and the non-Claude runtimes, which do not run the hook, keep their MCP settings in those directories too; that class of problem is not handled for them. Its Bash rule is a heuristic speed bump (a command that hides or computes the path can pass), so real containment is not granting Bash. MCP tools that change or trigger another employee's task, cron row or reminder check the caller's relationship to that employee (see "AI employees could change other employees' records" below). Promoting a live-fork branch back into an agent directory never copies agent-structure files (`SOUL.md`, `CONTRACT.toml`, `agent.toml`, `.mcp.json`, `.claude/`, …) over the parent's
- **Layer 3**: SOUL Guard — SHA-256 drift detection of each `SOUL.md`, with up to 10 versioned backups in `.soul_history/` (`duduclaw-security/src/soul_guard.rs`)
- **Layer 4**: Secrets — channel tokens, API keys and connector credentials are stored AES-256-GCM encrypted (`duduclaw-security/src/crypto.rs`, key in `~/.duduclaw/.keyfile`) and resolved per agent through `secret_ref`; agent CLI subprocesses start from an allowlisted environment that strips `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD` variables. The old "credential proxy" module was removed
- **Layer 5**: Containers — there is no single container layer. (a) The per-agent task sandbox (Docker only, off by default, fails closed, see [docs/guides/task-sandbox.md](docs/guides/task-sandbox.md)): read-only root, non-root (a gateway with uid or gid 0 is refused), all capabilities dropped, memory/pids/CPU limits, `--rm --pull never`; the working directory is a size-capped `/workspace` tmpfs, and of the agent directory only `SOUL.md`, `IDENTITY.md`, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, `CONTRACT.toml`, `SKILLS/` and `wiki/` are mounted read-only (never `.mcp.json`, `.claude/`, `state/`, `agent.toml` or databases); leftovers are swept at gateway start and every 10 minutes; it covers delegated/dashboard tasks, heartbeat task-board wake-ups, autopilot `delegate`/`run_skill`, goal rounds (always Solo for a sandboxed employee, never a team) and plan steps, skips the Agent Mail arrival trigger, and leaves channel replies, cron, reminders, the proactive check, ephemeral agents, `duduclaw acp` and live `duduclaw eval` on the host with a once-per-process `task_sandbox_not_applied` audit event; (b) the script sandbox in `duduclaw-container` used by the security-audit PoC and PTC `execute_program`: the same image as the task sandbox (never pulled automatically), Docker on macOS/Linux and WSL2-then-Docker on Windows (the Apple Container backend is never selected), the host user (`1000:1000` when the host is root), all capabilities dropped, `no-new-privileges`, read-only root, `--network=none`, 2 GiB memory without swap, 256 PIDs, 1 CPU, a `/tmp` tmpfs, a capped log, a 600 s hard limit, only a private read-only script directory mounted; when it cannot run, PTC refuses the script by default (`[container.sandbox] script_when_unavailable`, audited as `script_sandbox_unavailable` / `script_sandbox_bypassed`) and the PoC never runs on the host; (c) the computer-use container (image `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway version>`, published by `.github/workflows/computer-use-image.yml` from `container/Dockerfile.computer-use`, overridable only by the global `config.toml [computer_use] image`; never pulled automatically: `--pull never` plus a presence check before each session, a missing image fails the start with a message; read-only root, tmpfs, 1 CPU, 512 MB, 512 PIDs, `--security-opt no-new-privileges`, Chromium and the window manager as an unprivileged user under a managed browser policy, DevTools port on container loopback only, screenshots captured in a root-only (0700) directory the browser user cannot touch; `--network=none` unless a tool-driven session has allowlist hosts that resolved at start (see "Computer-use tools" below), and only then are `--network bridge` and `NET_ADMIN` added so the domain filter can install its default-deny egress rule (loopback limited to 127.0.0.1 / ::1, Docker's resolver 127.0.0.11 rejected); if it cannot install the rule while a non-loopback route exists the container refuses to start, never running with unfiltered egress; screenshots are masked fail-closed: password inputs, `.credit-card` and `[data-sensitive]` elements are painted black, and if the in-container detection helper fails the whole screenshot is masked; the whole screenshot is also masked when the focused window's title carries a credential marker or cannot be read (command error, timeout, non-zero exit, non-UTF-8); browser UI outside the page, cross-origin iframes and shadow DOM are not detected); (d) one-shot Discovery attempt/evaluator containers
- **Layer 6**: CONTRACT.toml — `[boundaries] must_not` is matched against every outgoing channel reply and a match blocks the reply and writes an audit event; `must_always` and `max_tool_calls_per_turn` are injected into the system prompt as instructions, not enforced at runtime (see [docs/spec/contract-toml-spec.md](docs/spec/contract-toml-spec.md))
- **Layer 7**: Audit Log — append-only JSONL: `security_audit.jsonl` for security events and `tool_calls.jsonl` for every tool call, with secrets masked

For full details, see [docs/features/05-security-defense.md](docs/features/05-security-defense.md) and [docs/architecture/overview.md](docs/architecture/overview.md).

### Computer-use tools: the internal route and the site allowlist

The eight `computer_*` MCP tools run in the per-agent `duduclaw mcp-server` process, which owns no container. Each call is forwarded to the gateway as `POST /api/internal/computer-use`, and the gateway owns the session and runs every check. Details: [docs/features/08-browser-automation.md](docs/features/08-browser-automation.md).

**How the route authenticates its caller.** Every check is required and fails closed, and every failure returns the same `unauthorized` answer:

1. The TCP peer must be a loopback address (the connection's own address; forwarded headers are not read).
2. The request carries `X-Duduclaw-Agent-Id`, `X-Duduclaw-Timestamp` (unix seconds), `X-Duduclaw-Nonce` (16 random bytes as 32 lowercase hex characters) and `X-Duduclaw-Signature`: HMAC-SHA256, keyed with the gateway-internal MCP key, over the agent id, that agent's identity token, the timestamp, the nonce and the SHA-256 of the body. The gateway derives the agent token itself from `~/.duduclaw/identity.key` (missing file: refused), tries every currently valid internal key, and compares in constant time. Neither the key nor the token is sent, so a process that binds the port while the gateway is down learns nothing reusable. `Authorization` is ignored on this route.
3. The timestamp must be within 60 seconds of the gateway's clock.
4. A nonce seen in the last 120 seconds is refused (replay).

On top of that: 120 requests per agent per minute and a 64 KiB body cap. Known limit, the same as for the identity token: a process running as the same OS user can read `identity.key` and the internal key.

**What the gateway checks again.** Because the route can be called without passing through the MCP dispatcher, the gateway re-applies the calling agent's `denied_tools` / `allowed_tools`, `scoped_tools` grants and the three approval lists (`approval_required_tools`, `irreversible_tools`, `maybe_irreversible_tools`, the last one always asked, with no model judge) for every operation. One session per agent; ephemeral agents are refused, and so is `[capabilities] computer_use_mode = "native"` (the host-desktop mode was removed together with the chat-triggered loop; the `computer_*` tools are the only computer-use path). High-risk actions need a person to confirm them in the chat the agent's current turn is answering; the gateway takes that chat from its own record of live turns, never from the request, and refuses the action when there is none (unless the operator set `auto_confirm_trusted = true`). Typed text never reaches an approval text, a confirmation prompt or an audit row; only its character count does.

**The site allowlist.** `agent.toml [capabilities.computer_use_config] allowed_domains` lists exact hostnames (no wildcards, no IP addresses, at most 20). With none, the session container runs with `--network=none`. Otherwise the gateway resolves each host at session start, skips any host whose answer contains a non-public address or no IPv4 address, pins each remaining address into the container with `--add-host`, and passes the address set to the container's egress filter: outgoing traffic is dropped by default, only TCP 443 to the pinned addresses is allowed, and DNS is refused. `computer_navigate` accepts only `https://`, no user name or password, port absent or 443, and a host that is exactly one of the session's resolved hosts. The kiosk browser has no address bar, so this tool is the only way to open a page. The URL is validated before any approval is requested, the approval text names only the validated host, and the URL reaches the in-container helper on stdin, never in a process argument list.

**Inside the container.** Loopback traffic is limited to `127.0.0.1` and `::1`, and Docker's embedded resolver `127.0.0.11` is rejected, so DNS cannot be used as an exfiltration channel even on a user-defined Docker network. Chromium runs under a managed policy (`container/scripts/chromium-policy.json`): pages cannot request local-network or loopback access, so a page cannot reach the DevTools port; incognito, guest windows, file dialogs, printing and downloads are off; `file://`, `chrome://`, `devtools://`, `view-source:` and `javascript://` URLs are blocked. The masking and navigation helpers run their JavaScript in an isolated world, so a page cannot redefine the DOM functions they read. Known limits: `ctrl+n` still opens an ordinary window with an address bar (network reach is unchanged; screenshots are fully masked while more than one page is visible), and a root process started through `docker exec` still holds the container's `NET_ADMIN`, because only the entrypoint's own process tree drops it; the gateway is the only party that execs into the container.

Residual risks the operator accepts by adding a site:

- An allowlisted site receives anything the AI types or submits on it.
- A site that proxies or redirects through its own domain can relay content from or to elsewhere.
- Addresses are pinned at session start; a site whose address changes during the session stops working until a new session starts.
- The allowlist is per agent and covers tool-driven sessions only.

**Live view and takeover (unreleased).** The dashboard can watch a session and take it over. The VNC server inside the container is started only on demand, listens only on a unix socket in the root-only `/tmp/duduclaw-root` (no TCP port; the helper refuses to run if anything listens on 5900-5999) and is reached only through `docker exec … duduclaw-vnc-relay` from the gateway's `/ws/computer-view` route; no port is published. A viewer needs a single-use 30-second ticket from the `computer_sessions.*` RPCs; the account's role and bindings are re-read from `users.db` on every request and every connection (watch: Admin, a bound Manager or an account bound at Operator level; take over: Admin or a Manager bound at Operator level). The gateway parses the viewer's RFB stream and forwards keyboard, pointer and clipboard events only from the takeover holder, never forwards screen-resize or `xvp` power requests, and closes on anything else it does not recognise. While a takeover is held the employee's actions are refused (`human_has_control`). Residual risks: VNC authentication is an 8-character DES password that reaches the authorized viewer's browser (the socket and the authenticated relay are the real boundary); the live stream is not masked, so a viewer sees whatever the page shows, including fields the employee's screenshots mask; a human in control can type anything on an allowlisted site. Every view, takeover and hand back is audited without its content. Each screenshot's page text also goes through `input_guard`; a block-level hit holds the session (actions refused, screenshots fully masked) until a person resumes it from the dashboard. Details: [docs/features/08-browser-automation.md](docs/features/08-browser-automation.md#keep-alive-live-view-and-takeover).

### Outbound address check (fixed in v1.67.0)

Every outbound SSRF gate now classifies addresses with one function, `duduclaw_core::net_addr::is_public_ip`: `web_fetch_cached`, `web_extract`, media downloads, the resident-sensing sources, the relay URL check, MCP server import, the skills RPC, the Odoo URL check, the wiki-federation peer check and computer-use pinning.

**Released versions are affected.** The check used by the web tools and the gateway's other `web_fetch`-based gates knew only `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `169.254.0.0/16`, `0.0.0.0`, `::1` and `fc00::/7`. Anything else was treated as public, so an IPv4-mapped address such as `http://[::ffff:127.0.0.1]:PORT/` passed it and reached loopback, and so did NAT64 and 6to4 forms of internal addresses. Newly refused: IPv4 `0.0.0.0/8`, `100.64.0.0/10`, `192.0.0.0/24`, `192.0.2.0/24`, `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4`, `240.0.0.0/4`; IPv6 everything outside global unicast `2000::/3`, plus Teredo `2001::/32`, `2001:db8::/32` and `3fff::/20`. IPv4-mapped (`::ffff:0:0/96`), NAT64 (`64:ff9b::/96`) and 6to4 (`2002::/16`) addresses are judged by the IPv4 address inside them; IPv4-compatible `::/96` and local-use NAT64 `64:ff9b:1::/48` are refused as a class. The Odoo URL check already covered the private, loopback, link-local, CGNAT and broadcast IPv4 ranges and the IPv4-mapped, link-local and unique-local IPv6 forms; it now also refuses the documentation, benchmarking, `192.0.0.0/24`, `0.0.0.0/8`, multicast and reserved ranges and the IPv6 forms listed above. The wiki-federation peer check now classifies an IP-literal peer with the same function before its string patterns.

### Removed employees' names are reserved against AI callers (fixed in v1.67.0)

An AI employee that supervises others could `agent_remove` a subordinate and then `create_agent` the same name, getting a seat with the same name but without the controls the operator had set on the old one (`CONTRACT.toml`, `[capabilities]` restrictions, the sandbox setting). `agent_remove` moves the employee to `~/.duduclaw/agents/_trash/<id>_<14-digit timestamp>`, and now, when an AI caller creates an employee, the name is refused if:

- `_trash/` holds an entry for that id (whole-id match),
- `org.toml` still records the id while its directory is gone, or
- `_trash/` exists but cannot be listed (fail closed).

Every MCP caller counts as an AI caller; there is no operator exemption over MCP. The CLI scaffold path (`duduclaw agent create`, pack install, `migrate from`) applies the same rule when it detects an AI session through `DUDUCLAW_AGENT_ID` / `DUDUCLAW_AGENT_TOKEN`, and the `agent-file-guard` hook additionally blocks a Bash `duduclaw agent create <reserved name>` and AI writes, moves and deletes under `agents/_trash/`. Refusals are audited as `agent_name_reserved` (`requested_name`, `path_kind`, `reason`) and removals as `agent_removed`. `agent_remove` no longer hands the AI the trash path or an `rm -rf` hint.

Operators are not restricted: the dashboard and a human at a terminal can reuse a reserved name. Restoring or purging a removed employee means operating on `~/.duduclaw/agents/_trash/<id>_<timestamp>` by hand; the dashboard has no restore or purge control. Known gaps: for Claude, Codex and Gemini employees the CLI cannot detect an AI session from the Bash environment (their identity is in `.mcp.json`), so `pack install` and `migrate from` run from such an employee's Bash are not covered; the Bash rules are heuristics an employee with Bash can defeat, so real containment is not granting Bash. Verified through the real MCP server with an employee's own registration (remove, refused re-creation, a different name accepted, the hook blocks, the audit rows); the CLI scaffold path was exercised only by unit tests.

Related change: `create_agent` and `agent_remove` called over HTTP with a non-internal MCP key are now judged by that key's own client id, so the organisation-scope check applies to the real caller (previously such calls were treated as the process's default agent).

### Local-address and userinfo checks (fixed in v1.67.0)

- The `duduclaw-llm` MCP HTTP client accepts a plain `http://` endpoint only when the parsed host is exactly `localhost`, an address in `127.0.0.0/8`, or `::1`. Before, a prefix test let `http://localhost.evil.com` count as local.
- The Odoo URL check parses the URL and refuses any user name or password in it. Before, `http://localhost:3000@evil.com` was treated as local and `https://user@10.0.0.1/` could hide a private address.

### Approval lists were not enforced for per-agent MCP calls (fixed in v1.67.0)

Before this fix, the MCP approval gate read `[capabilities]` for the literal principal `gateway-internal` (the name of the gateway's internal key) instead of the agent making the call. No agent directory has that name, so for every agent started by the gateway `approval_required_tools`, `irreversible_tools` and `maybe_irreversible_tools` had no effect on calls through its own MCP server: listed tools ran without asking. The gate now resolves the acting agent, so after upgrading, tools on those lists wait for a human decision (up to 300 seconds, no answer is a refusal). The redaction vault for those calls is keyed by the same acting agent. External API keys are unaffected.

### Lower-trust memory writes could replace trusted facts (fixed in v1.67.1)

**Released versions are affected.** Every memory write carries an origin and a trust ceiling (`duduclaw-memory/src/origin.rs`), but nothing compared trusts when a new fact replaced the current one for the same `(agent, subject, predicate)`. A fact distilled from a chat conversation (origin `channel`, trust 0.3), which anyone who can talk to an AI employee can influence, therefore replaced whatever was current for the same subject and predicate regardless of its trust: for example a fact the AI employee derived itself or a row with no recorded origin (0.6), or a fact imported with `migrate from` (0.7) when the keys matched. (Operator-approved facts, 1.0, only exist from v1.67.1 on, written by the review approval described below.)

Now a write whose effective trust is strictly lower than the current fact's cannot replace it (`supersession_guard.rs`); equal or higher trust replaces as before. In the conversation-fact and profile-trait distillation paths the refused claim is held inert and becomes a review item that only a manager or admin can decide in the dashboard; chat channels get a notice without buttons or claim text, and button presses or text replies are refused. Other write paths skip the item or return an error. At most 20 new review items per AI employee per UTC day; beyond that refusals are only audited (`memory_supersession_refused`, `review_cap_hit`). In the same change the `user_profile` origin ceiling dropped from 1.0 to 0.6, so an AI-recorded profile value can no longer outrank operator-approved ones.

The review item is built only from the stored claim and shows everything approval would write; approval writes exactly that claim (bound by a digest) and writes nothing if the claim or the protected fact changed after the item was created. Claims longer than 600 characters are not queued, only audited (`not_held_reason: "too_long"`).

`memory_invalidate_by_origin` was a second way to remove trusted facts: a steered AI employee could expire every operator-level fact in its namespace in one call. Since v1.67.1 a caller treated as an AI employee may only invalidate the `channel`, `mcp_external` and `tool_echo` classes; other classes are refused and audited as `memory_invalidate_refused`. The check fails closed: any caller on the gateway's shared internal key counts as an AI employee whether or not an employee identity is present, and so does a key that belongs to an employee or an ephemeral employee (`eph-` ids); only an admin key that maps to no employee is unrestricted. The tool acts on the caller's own namespace only (since v1.68.0 the employee's own namespace, see below). `user_profile_record` now refuses pseudo-users (`system`, `anonymous`, `unknown`) and scans both the predicate and the value for prompt injection, refusing on a block-level hit.

Limits: the check compares exact subject and predicate strings, so the same fact written under a differently spelled subject or predicate is a separate triple and coexists, as before. `config.toml [memory] supersession_trust_guard = false` restores the old behaviour. Covered by unit and integration tests; not exercised on a real chat channel. Details: [docs/features/20-memory-intelligence.md](docs/features/20-memory-intelligence.md#supersession-trust-guard-v1671).

### MCP memory tools shared one namespace across employees (fixed in v1.68.0)

**Released versions v1.44.0 to v1.67.1 are affected.** Memory an AI employee wrote or read through the MCP memory tools (`memory_store`, `memory_search`, `memory_read`, `memory_fetch_batch`, `memory_alias_add` / `memory_alias_list`, `memory_get_history`, `memory_get_at`, `memory_invalidate_by_origin`, `user_profile_record`, `user_profile_get`, `user_code_profile`) used the namespace of its MCP key. Every employee the gateway spawned used the gateway's internal key, so all of them shared `internal/gateway-internal`: employees of one gateway could read and write each other's tool-stored memory, and the trust guard did not arbitrate between that pool and the gateway-written memory of each employee.

Since v1.68.0 an internal-key caller whose `DUDUCLAW_AGENT_ID` is proven by `DUDUCLAW_AGENT_TOKEN` (HMAC keyed by `identity.key`, written into each employee's `.mcp.json`) reads and writes under the employee's own id, the namespace the gateway uses for distillation, review approval and prompt injection. A caller that cannot prove an identity stays in the old shared pool (fail closed); the HTTP transport never takes the identity from its process environment; a per-agent key whose client id names an existing employee maps to that employee. Rows written before the upgrade are not moved automatically. An operator inspects, exports, assigns or archives them with `duduclaw memory migrate-namespace` (hidden, refuses to run inside an AI employee's session, dry run unless `--confirm`, skips rows that look like prompt injection unless `--include-flagged`, audited as `memory_namespace_migrated`). Details and steps: [docs/features/20-memory-intelligence.md](docs/features/20-memory-intelligence.md#memory-namespaces-v1680).

### Webhook channels and WebChat let any sender run admin chat commands (fixed in v1.68.0)

**Released versions are affected.** `!STOP`, `!STOP ALL`, `!RESUME` and `/model <name>` are meant for channel admins. WhatsApp, Feishu, Microsoft Teams, WeCom, Google Chat and DingTalk passed `is_admin = true` for every sender, and WebChat did the same, so anyone who could message the bot could stop it, resume it or switch its model; the dashboard's 通道管理員 list had no effect on those channels. They now call `chat_commands::handle_command_for_sender`, which compares the sender id or conversation id exactly with the channel's global `admin_users` setting; a missing, empty or malformed list means nobody is an admin. Google Chat and Teams can now hold an `admin_users` setting. On WebChat only an active dashboard account with the Admin role counts; website widget visitors never do.

### Dashboard-set admin token was lost on restart (fixed in v1.68.0)

**Versions v1.22.0 to v1.67.1 are affected.** Saving the 管理權杖 from the dashboard removed the plaintext `[gateway] auth_token` and wrote the encrypted `auth_token_enc`, but the gateway only read the plaintext key at startup, so after the next restart it ran without an admin token. Startup now reads `DUDUCLAW_AUTH_TOKEN`, then `auth_token_enc` (decrypted), then plaintext `auth_token`, also resolving `secret://` references; blank values count as unset.

### Non-admin owners could change an employee's authority fields without a record (fixed in v1.68.0)

**Released versions are affected.** `agents.update` only required Owner access, so a dashboard user who owned an employee could change `[agent] reports_to` / `department` / `name` (`reports_to` and `department` feed `org.toml`, the delegation authority), any `[capabilities]` key, `[container] sandbox_enabled` / `network_access` and `[permissions] can_modify_own_soul`, and nothing was audited. These fields are now admin-only; a successful change is audited as `agent_authority_changed`, a refused attempt by a non-admin is audited as `agent_authority_refused`, and `org.toml` is updated only when `reports_to` or `department` actually changed.

### Dashboard controls that looked like protection did nothing (fixed in v1.68.0)

These saved a value that nothing read, so an operator could believe a protection was on when it was off. All are wired now:

- `KILLSWITCH.toml [triggers]` (`max_replies_per_minute`, `max_consecutive_errors`, `error_rate_threshold`, `cost_limit_usd`): only keys present in the file and in range are enforced; `null` from the dashboard removes one. Details: [docs/features/05-security-defense.md](docs/features/05-security-defense.md).
- Redaction source protections `user_input`, `system_prompt` and `cron_context` (only tool results were ever redacted before); an error stops the turn. The `sub_agent` switch was removed. `purge_after_expire_days` now drives the vault cleanup (it was always 30 days).
- `agent.toml [permissions]` `can_create_agents`, `can_send_cross_agent`, `can_modify_own_skills`, `can_schedule_tasks`: an explicit `false` is enforced at the MCP dispatch gate. Because old templates wrote `false`, the first boot after upgrading resets those values to `true` once per employee and writes `permissions_enforced_since = "1.68.0"` (audited as `permission_flags_reset`); ephemeral role members are not reset.

Related hardening in the same release: changes to `acp.trusted`, `tick.allow_command_sources`, `container.sandbox.when_unavailable`, `container.sandbox.script_when_unavailable` and `memory.supersession_trust_guard` are audited as `config_protected_key_changed`; `system.update_config`, `tick.sources.*` and the new raw config editor (`config.raw.set`, admin-only, secrets masked as `«set»`, backup before write, audited as `config_raw_edited`) refuse to rewrite a `config.toml` that does not parse and refuse a write when the file changed since it was read.

### AI employees could change other employees' records (fixed in v1.69.0)

**Released versions are affected.** Creating a task or a routine already ran the delegation predicate, but the tools that change or fire an existing record did not: an AI employee could rewrite or close another department's task, pause, delete, edit or run another employee's cron row, and file a reminder that wakes another employee with its own prompt. Now `tasks_update`, `tasks_claim`, `tasks_complete`, `tasks_block`, `activity_post` with a `task_id`, `update_cron_task`, `delete_cron_task`, `pause_cron_task`, `run_cron_task` and `create_reminder` check the caller against the record's owner (`duduclaw-cli/src/mcp/record_authz.rs`):

- an operator (an admin key that maps to no employee, in a process not started for one) is not restricted;
- the owner itself is allowed (for a task: the assignee, the claimer or the creator; `tasks_complete` and `tasks_block` accept only the assignee or the claimer);
- anyone else needs the delegation relationship (same department, `reports_to` line or a whitelist pair, per `[delegation] policy`);
- an owner or caller that cannot be determined is refused (an unassigned, unclaimed task must be claimed first), and every refusal is audited.

These tools act as the employee the calling key maps to. The shared internal key in a process with no employee identity acts as the internal client id, which is no node of the organisation and therefore owns nothing. A system-sender name (`dashboard`, `cron`, `goal-loop-driver`, `heartbeat`, `autopilot`, `webhook`) is no longer accepted as an MCP process identity: the MCP `create_agent` tool, the dashboard and the `duduclaw agent create` command all refuse these names, and as long as no employee carries one the gateway never starts an MCP server with one, so only a self-asserted identity could carry it, and it now resolves to the untrusted sentinel. The substitution writes one `tool_calls.jsonl` row per MCP process (tool `mcp_identity`, reason `system_sender_identity`, with the claimed name and whether it came from the environment or `config.toml`).

Same change: on a `goal_mode` task `title` and `description` are frozen for AI employees like `acceptance_criteria`; an AI employee cannot add, remove or reorder the control tags (`outcome:…`, `grant:…`, `auto-research`) through `tasks_update` or pass them to `tasks_create` (the `outcome:` tag that `kind = "goal"` builds server-side is unaffected); `agent_update` refuses an employee changing its own `reports_to`, `db_sources` / `db_sources_add` / `db_sources_remove`, `budget_cents` or `role` (audited as `agent_authority_refused`; editing a subordinate is unchanged); and cron tools addressed by `name` act on exactly one row, refusing an ambiguous name with the candidate ids (before, `delete_cron_task` and `pause_cron_task` acted on every row with that name).

### Employees could write DuDuClaw home state and their own security settings (fixed in v1.69.0)

**Released versions are affected.** The `agent-file-guard` hook only protected agent-structure files and other employees' directories, so an AI employee with Write/Edit or Bash could change files the platform treats as evidence or authority: the audit log the grounding check and judge digest read, eval suites including held-out sets, SQLite stores, breaker state, licence and org files, global skills and the shared wiki. It could also edit the sections of its own `agent.toml` that security checks read. Changes:

- **Home allow-list.** For an employee-identified (or unverifiable) caller, the only writable places under the DuDuClaw home are its own agent directory and the shared `attachments/` directory; everything else is refused. Only a caller with no employee identity, meaning the hook command carries no `--agent` and the environment has no `DUDUCLAW_AGENT_ID`, is unaffected. The installed hook command always carries `--agent` and the hook prefers it, so an operator who runs `claude` by hand inside an employee's directory is judged as that employee (and, where identity tokens are required, as an unverified caller). An operator changes these files from the dashboard or with an ordinary editor.
- **Real paths.** Write/Edit targets are judged on where the write really lands after following symbolic links; a path whose real location cannot be determined is refused ("cannot determine where this write actually lands"). A relative path is resolved against the working directory in the hook input, or the employee's own directory when there is none.
- **The real home.** The gateway starts an employee's CLI with a scrubbed environment that has no `DUDUCLAW_HOME`, so on a deployment with a non-default home the hook judged paths against `$HOME/.duduclaw`. The installer now writes the home into the hook command (`--home`); existing installs are rewritten at the next spawn or gateway start. The hook takes its home from `--home`, then (for a caller with no employee identity) the default location, then an explicitly set `DUDUCLAW_HOME`; it is never inferred from the working directory, and an employee caller with none of these is refused for every Write/Edit/NotebookEdit/Bash call. Upgrade `duduclaw` and `duduclaw-pro` together: an older binary does not know `--home`, exits with a usage error, and Claude Code then blocks every Write/Edit/Bash call of that employee. After a downgrade, restart the gateway so boot rewrites the hook command.
- **NotebookEdit** is covered (matcher `Write|Edit|MultiEdit|NotebookEdit|Bash`).
- **Own `agent.toml`.** An employee can change only the sections listed as editable in its own `agent.toml`; every other section, including one added later, is protected. The editable list and the keys frozen inside it: [docs/features/05-security-defense.md](docs/features/05-security-defense.md#guard-4--org_field_guard-organizational-authority-freeze).
- **Unreadable files.** An existing `agent.toml`, `config.toml` or `.mcp.json` that cannot be read is refused (before, it was treated as a new file).
- **Bash.** The Bash lane applies the same allow-list to command text: apart from a short list of known read-only commands and the destination rule for copies, a command with an argument pointing at a protected home location is refused, and home database files outside the agent directories and `attachments/` are refused even for reading; the rules are in [docs/features/05-security-defense.md](docs/features/05-security-defense.md#guard-1--agent-file-guard-pretooluse-rust).

Known limits. The Bash lane is a speed bump that reads command text and cannot see computed paths (nor where an extraction or download with no explicit destination writes after a change of directory), so real containment is still not granting Bash; it also refuses a judged path that cannot be resolved, a dangling link included, even outside the home; `Read` is not covered, only the Claude runtime runs the hook, state files in an employee's own directory are not protected and `attachments/` is shared. The full list is in [docs/features/05-security-defense.md](docs/features/05-security-defense.md#what-these-guards-do-not-cover).

### Antigravity: allow rules the gateway writes into the operator's agy settings (v1.69.1)

This is a change in what the gateway writes, not a fixed vulnerability. In print mode `agy` 1.2.16 refuses every tool confirmation it cannot ask a human about, so an Antigravity employee at the default permission level could not call any DuDuClaw MCP tool. To make those calls work, each time the gateway runs an Antigravity turn it adds two entries to `permissions.allow` in the operator's user-level `~/.gemini/antigravity-cli/settings.json`, in the same locked write that already adds the workspace to `trustedWorkspaces`:

- `mcp(duduclaw/*)`, which approves every tool of the MCP server registered under the name `duduclaw`;
- `read_file(<HOME>/.gemini/antigravity-cli/mcp/duduclaw)`, which approves reading that server's tool description files (agy loads MCP tools lazily and the model reads the description first). When `HOME` canonicalizes to another path, both spellings are written.

What is not written: no rule for shell commands, file writes or URLs, and no change to the command-line flags (the default level still runs with `--sandbox`). The operator's own `allow`, `deny` and `ask` rules are kept. A `permissions` value that is not an object, or an `allow` that is not an array, is left untouched with a warning. A home path containing `(`, `)`, `,`, `*` or a line break, or that is not valid UTF-8, gets no `read_file` rule. In a test with agy 1.2.16 and a real Gemini API key, a shell command and a write outside the workspace in the same turn were still refused, as were a path-traversal read and a read through a symbolic link that points outside the directory.

What the operator should know:

- The file is shared by every `agy` of the OS user. When you use `agy` interactively in a terminal, calls to tools of an MCP server named `duduclaw`, and reads of that schema directory, are approved without asking.
- A read-only Antigravity employee gets the same rules, because a user-level file cannot depend on an employee's capability level. What such an employee may do with platform tools is decided by the MCP server's `allowed_tools`, `denied_tools` and approval lists, as for the Claude runtime. Codex differs: at its read-only level every MCP tool call is rejected.
- The rules are only added, never removed. They stay after you remove an employee or uninstall DuDuClaw. To remove them, edit `~/.gemini/antigravity-cli/settings.json` and delete the two entries from `permissions.allow`.

Not yet verified: the end-to-end test with a real key has not been rerun since the last change to this code, and it has not been run on Linux, inside a Docker container or on Windows. See [docs/features/13-multi-runtime.md](docs/features/13-multi-runtime.md).

### Computer-use workspaces: what the owner check and the approval gate do not cover (unreleased)

Computer-use workspaces (`config.toml [computer_use.workspaces]`, off by default; guide: `docs/guides/computer-workspaces.md`) keep an AI employee's files across computer-use sessions under `<home>/computer_workspaces/`. The gateway is the only writer, a session sees the files read-only at `/workspace/files` under a root-only tmpfs, and every state-changing operator action from the terminal (`fence`, `revoke`, `regrant`, `renew`, `delete`) needs an Admin's approval in the dashboard, because the terminal cannot tell the operator from an AI employee with Bash; emergencies go through the dashboard RPCs (a signed-in Admin) or the master switch. Every terminal action is audited when requested, applied and refused (`computer_workspace_cli_action`). Residual risks, stated plainly:

- **Owner isolation holds for the three `computer_workspace_*` tools only.** An AI employee with the built-in `Read` tool or with Bash can read any workspace directory on the host. One with unrestricted Bash can also get around the command-line identity check and the approval gate: it can run `duduclaw ops computer-workspaces` with no DuDuClaw variables in its environment (the agent-file-guard Bash rule is a speed bump that a variable or an alias evades, and non-Claude runtimes do not run the hook), file requests against any employee's workspace, or edit `approvals.db` (and so mark a request approved), `computer_workspaces.db` and the workspace directories directly. These gates bind the product paths (MCP tools, dashboard, channels), not an employee with a shell. Real isolation is not granting Bash, or the task sandbox.
- **Owner credential.** A workspace is bound to the employee instance that created it by a random credential kept in the registry and in `agents/<owner>/state/computer_workspaces.json`; a re-created employee of the same name without it gets nothing. The file is in the employee's own directory, and an employee with `Read` can also read a removed predecessor's copy under `agents/_trash/`.
- **Plaintext at rest.** Workspace files are not encrypted. Redaction's never-restore rule for `computer_workspace_write` only means masked tokens are written as tokens.
- **`denied_tools` does not cover attaching.** Listing the workspace tools in `denied_tools` does not stop `computer_session_start` with `workspace` from mounting a workspace read-only; the switch for that is `[capabilities.computer_use_config] workspace`.
- **Switching the feature off keeps owner reads.** It stops creates, attaches and writes; the owner can still list and read. `revoke` stops reads.
- **Mount check to `docker run`.** A program running as the gateway's OS user can swap a workspace directory for a link between the source check and the daemon resolving the path. Inside the container only root can enter `/workspace`, and no tool reads the mount from inside the container, so there is no read-out path today.
- **Container.** The browser runs with `--no-sandbox` (as before). Root `docker exec` can write the 64 KiB tmpfs at `/workspace`; `/workspace/files` is read-only. Only the gateway runs such commands.
- **Verified on** Docker Desktop on macOS arm64 only; native Linux Docker, amd64 and Windows are not verified. On Windows the feature refuses every call.

### Continuous responsibilities, mid-run directions and stopping a task: residual risks (unreleased)

This describes a new feature, off by default (`config.toml [responsibilities] enabled`, `[goal_loop] steering_enabled`), not a fixed vulnerability. A continuous responsibility lets an AI employee wake on a schedule or an event and run bounded goal tasks; the operator manages it from the command line `duduclaw responsibility …`. Guide: [docs/guides/continuous-responsibilities.md](docs/guides/continuous-responsibilities.md).

What the design relies on:

- **The command line cannot prove who typed a command.** An AI employee with Bash runs as the same OS user as the operator, so no state-changing `duduclaw responsibility` action takes effect on its own, `stop`, `pause` and `disable` included. Each one files an approval that only an Admin can decide, only in the dashboard (`decided_by` must start with `dashboard:`; channel buttons and replies are refused). The approval is bound to the action, the target, the exact change and the target's current state, including when that state last changed and the responsibility's control epoch, so an approval cannot be replayed after pause → resume → pause. It must be used within `[responsibilities] operator_approval_minutes` (default 30) of the decision and is claimed once. Identical requests are merged; one action on one target can have at most three different undecided requests, a target gets at most two pushes per hour, and these requests get no reminder pushes. A contract is validated before a request is filed. The card is built by the server; the responsibility's job text is shown as quoted data cut to 80 characters. Every request, apply and refusal is audited (`responsibility_cli_requested` / `_applied` / `_refused`).
- **The Bash-lane check is a speed bump.** The `agent-file-guard` hook refuses the `responsibility` subcommand of `duduclaw` / `duduclaw-pro` (reads included) for employees and unverified callers, and the command itself refuses to run when the employee identity variables are set. Both read text or environment: a renamed binary, a command assembled from variables or a scrubbed environment get past them. An employee with unrestricted Bash can bypass the command-line gate and can edit `approvals.db` or `tasks.db` directly. Real isolation is not granting Bash, or the task sandbox.
- **Emergency route.** The "Stop task" button on the dashboard's task page runs through an RPC with a real account identity and stops the current run at once; the responsibility keeps waking. The dashboard has no page to pause or disable a responsibility: to keep one from waking, `duduclaw responsibility disable` (after an Admin's approval) or `[responsibilities] enabled = false` in `config.toml`. A command-line `stop` waits for approval.
- **Stopping cannot interrupt a round already running.** It reports `cancel_pending` until that round ends, `stopped_uncertain` when something cannot be confirmed (a lapsed claim with no finished round, an external action with an unknown result, a tree past the scan limit), and never claims an external action was undone. Delegations a run already sent are separate tasks and are not stopped. A stop by an account with only Operator access on the employee counts as a failed run, and clearing the failure streak needs Manager, so an operator cannot reset the streak either way. A stopped tree cannot be claimed or completed, and neither the dispatch engine nor the heartbeat's task-board wake-ups dispatch a member of it.
- **Events and answers are data.** Event content and `responsibility_ask` text are scanned for prompt injection and carried as quoted data; the assignee, deadline, tags, acceptance criteria, budget and tool access come from the responsibility. Events an employee produced itself never wake its own responsibility, events from before a subscription was armed never count, and event wake-ups have a per-window cap. Run tasks cannot be reassigned or have their control fields changed by an employee (the departure hand-off leaves them too), and Activity types beginning with `responsibility.` or `task.stop` can be written only by the gateway (matched without regard to case). A task an employee creates during a round of a responsibility run is placed under the task that round works on, decided from the round the gateway is running and passed to the MCP server, never from the model; a parent the model names must lie inside that task's tree, and a round value that is present but empty or malformed is refused. Ordinary goal rounds keep the old rule. The goal-round and heartbeat markers count only at the start of a message from the system sender that writes them, and `heartbeat-scheduler` and `workflow` are reserved names no employee can take; an existing employee with one of these names is treated as untrusted. The dashboard RPCs for responsibilities, directions and stopping re-read the caller's account on every call and apply the task's audience. Where the MCP server gets no round information (Bash-started servers, the Grok and Gemini runtimes) `tasks_create` keeps working, and such a task counts toward the run only if it is placed under it.

Residual risks:

- An employee with unrestricted Bash can bypass the command-line gate (see above).
- Placing a run's tasks under it relies on the employee not being able to rewrite the `duduclaw` entry of its own `.mcp.json`. That platform fix (freezing the entry's command, arguments and environment and normalising its environment at every start) must be merged before this feature; until it is, an employee that rewrites the entry can drop the round information and create tasks outside the run. Binding the round id with an HMAC is a later hardening. Antigravity's `<work_root>/.agents/mcp_config.json` is part of the same known gap for non-Claude runtimes.
- With an employee or acceptance judge on a runtime that may not report token usage (Antigravity, Gemini CLI), one unmeasured round uses the whole per-run cap; creation and `duduclaw doctor` warn about it.
- Spend not counted toward a responsibility's caps: the trajectory simulation on a needs-human escalation, the kickoff approval narrative, the dispatch policy's choice of employee, the action-guard judge inside the MCP server process, work delegated to other employees, one-off helpers started with `spawn_agent`, cron routines and reminders created with `tasks_create` and a `schedule`, and tasks created through an MCP server that does not know the round (one started from Bash, or under the Grok and Gemini CLI runtimes, whose MCP configuration is a stored file) unless the employee names the run as their parent. A round counts as run once the dispatcher recorded its start (before the runtime is started), whatever happens to it afterwards; a run round whose spend was not measured counts as the full cap; a usage record without token counts counts as not measured. The per-run cap is checked between rounds, so the round in progress and sub-tasks running at the same time can take a run past it; the employee's monthly budget, checked before every round, is the outer bound.
- Grok reports no token usage, so its spend is estimated from text length.
- A command-line `stop` is reconciled in the command-line process and cannot see a team round the gateway is just starting; for an ordinary goal task there is a narrow window where it reports `stopped` while that round still runs.
- A responsibility narrows event sources and notification targets only; a run has exactly the tools its employee already has.
- Runs, and rounds that carry operator directions, always run with the single employee (no team round).
- If the feature is switched off and back on while the gateway is down, the gateway does not see the switch and event subscriptions keep their earlier starting point. Events older than the 7-day retention are lost after a longer outage, with a gap record.
- There is no `/responsibilities` dashboard page yet; responsibilities are managed from the command line with inbox approval.

### Forget by source: residual risks (not yet released)

`duduclaw memory forget-source` deletes the memories a source produced and blocks that source from being learned again. It is gated by a dashboard approval from an Admin, a refusal inside an employee's session, and a Bash-lane rule. Not yet verified on a live gateway with real channels. What it does not do, and where it can be bypassed:

- It does not delete conversation text (the messages are hidden from the employee, the original stays in `sessions.db`), sent messages, backups or content other employees received. Tool-call records and error notes that are put into the prompt each turn, the task board, working state, Agent Mail, goal state, hand-off copies, replies between employees, the Claude CLI's own transcripts, shared wiki copies and wiki pages the employee wrote itself (an older page has no recorded source, a newer one is only listed for review) can still carry the forgotten text back to the employee. The full list is printed with every plan.
- Memories stored through the Gemini CLI runtime are not tied to a conversation and are not reached; Grok has not been verified. An imported file copied to another path is a new source.
- What the employee learns again in a new conversation is new information and is not blocked. Restoring a backup from before a forget brings the memories back and removes the block. Memories written before the feature have no recorded source and are never deleted by it.
- The Bash-lane rule for `memory forget-source` and `memory migrate-namespace` is a speed bump. A global option between `memory` and the subcommand, a binary named by command substitution, a command fed through a pipe and other spellings that avoid matching on the command name get past it. The command's own AI-session check can be defeated by unsetting the gateway's variables. The dashboard approval is the gate. An employee that runs as the same operating-system user, can execute arbitrary commands and deliberately evades the file guard can still rewrite `approvals.db` and `memory.db` directly. Real isolation is not granting Bash, or running the employee in the task sandbox.
- The approval card says the request came from a local command line; the system cannot tell who typed it. An Admin approves on that basis.

See [docs/features/05-security-defense.md](docs/features/05-security-defense.md#forget-by-source-needs-an-admin-approval).

## Binary Distribution Security

DuDuClaw binaries are:

- Built via GitHub Actions from public source (see [`.github/workflows/release.yml`](.github/workflows/release.yml))
- Published with SHA-256 checksums (`*.sha256`) in every release
- Signed with [cosign](https://github.com/sigstore/cosign) keyless OIDC signing (`*.sig` + `*.pem`)
- Distributed only via GitHub Releases (no third-party CDN)
- Shipped to npm as platform packages through `optionalDependencies` — the `postinstall`
  script ([`npm/duduclaw/scripts/install.js`](npm/duduclaw/scripts/install.js)) only verifies the
  platform package is present; it never downloads or executes external code

What we do **not** do:

- ❌ Telemetry: no usage data or conversation content is sent to us. Network calls the gateway makes on its own: an update check against GitHub Releases every 6 hours, and, only when a paid license is installed, a license refresh (every 3–7 days depending on tier) and a daily revocation-list fetch from the license server
- ❌ Collect API keys (secrets stay on the user's machine via an AES-256-GCM vault)
- ❌ Auto-execute untrusted downloaded code
- ❌ Require root / privileged escalation

Verification instructions are in the [README Trust and security section](README.md#trust).

## Disclosure Policy

We follow [coordinated vulnerability disclosure](https://en.wikipedia.org/wiki/Coordinated_vulnerability_disclosure). We ask that you:

- Give us reasonable time to fix the issue before public disclosure
- Do not exploit the vulnerability beyond what is necessary to demonstrate it
- Do not access or modify other users' data

We commit to:

- Not pursuing legal action against good-faith security researchers
- Crediting researchers in security advisories
- Keeping researchers informed of fix progress
