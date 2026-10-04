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
- **Layer 2**: Authorization — there is no RBAC engine (the module was removed). Who may command whom comes from the delegation policy (`duduclaw-core/src/delegation_policy.rs`), MCP scopes (a tool missing from the scope table requires Admin) and the per-agent `agent.toml [capabilities]` grants, all enforced at the MCP dispatcher; an agent-structure and org-field PreToolUse hook (`duduclaw hook agent-file-guard`) stops agents rewriting their own `SOUL.md`, their own `CONTRACT.toml` (no opt-in; operators change it through the admin-only `contract.update` RPC), `reports_to`, `[capabilities]` or any section of their own `agent.toml` outside the editable list, and keeps an employee's writes under the DuDuClaw home to its own agent directory and `attachments/`. Its Bash rule is a heuristic speed bump (a command that hides or computes the path can pass), so real containment is not granting Bash. MCP tools that change or trigger another employee's task, cron row or reminder check the caller's relationship to that employee (see "AI employees could change other employees' records" below). Promoting a live-fork branch back into an agent directory never copies agent-structure files (`SOUL.md`, `CONTRACT.toml`, `agent.toml`, `.mcp.json`, `.claude/`, …) over the parent's
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

Verification instructions are in the [README Trust & Security section](README.en.md#-trust--security).

## Disclosure Policy

We follow [coordinated vulnerability disclosure](https://en.wikipedia.org/wiki/Coordinated_vulnerability_disclosure). We ask that you:

- Give us reasonable time to fix the issue before public disclosure
- Do not exploit the vulnerability beyond what is necessary to demonstrate it
- Do not access or modify other users' data

We commit to:

- Not pursuing legal action against good-faith security researchers
- Crediting researchers in security advisories
- Keeping researchers informed of fix progress
