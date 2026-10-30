# Outgoing data guard

Available from v1.77.0. The guard tracks private reads during an AI employee's turn and checks later outgoing MCP calls before execution, outside the model.

## Choose a mode

Open **Security → Outgoing data guard**. The default is **Observe only** (`shadow`): calls continue, with a `would_ask` entry and source tool names in the tool-call audit. Choose **Ask before sending** (`ask`) to require approval, **Block sending** (`block`) to refuse, or **Off** (`off`) to disable this layer.

```toml
# config.toml
[security]
egress_guard = "ask"

# agent.toml
[capabilities]
egress_guard = "block"
```

The stricter setting wins: `off < shadow < ask < block`. An employee override cannot weaken the global setting. A missing global configuration file defaults to `shadow`; a missing employee policy adds no override. Other read errors or invalid configuration block the affected call. Existing denied-tool lists, permissions and action rules continue to apply.

## What triggers it

A private read reserves the turn in the gateway before execution. Pending reads count as tainted across MCP processes. Any model-visible payload converts the reservation to a source, including error-shaped data; only a confirmed payload-less failure releases it. Cancelled or uncertain calls retain taint. Sources include memory, wiki and knowledge-base reads, local files, databases, mail, Google data, tasks, activity and working state. Every third-party MCP tool is considered a private read. Exact tool-name tables are checked against every advertised platform tool; unknown tools are private by default.

`wiki_write` with final `scope="shared"` is Publish; private scope remains Modify. Classification uses the arguments after policy rewrites.

After a private read, the guard covers platform tools classified as **Send**, **Publish** or **Purchase**, and tools that can send arguments to a URL or destination, including web fetch/extraction. The third-party MCP proxy checks non-read decisions. A fetch may send private information in its URL even though its result is a read. The explicit outgoing table also covers remote search/query arguments (Google, Notion, GitHub, Odoo and database queries), browser navigation/input, remote speech processing, model/skill searches and downloads, and notification destinations in task or schedule updates.

The gateway owns an in-memory ledger keyed by `DUDUCLAW_TURN_ID`, with a six-hour TTL. MCP processes reach it through signed loopback requests and keep a local tainted cache for six hours. Each cache entry holds at most 512 sources, including an `additional_private_sources` summary for excess sources. Only a valid turn registered by the gateway can be reported clean. Missing or unknown turn identity, an expired ledger entry, a gateway restart, an unavailable gateway or an unverifiable reply is treated as private data already read.

External API-key and HTTP MCP callers also use the global policy. They have no gateway-owned turn and are always treated as tainted; they cannot inherit the MCP process employee's clean turn.

For authenticated employees, missing or unknown turn IDs still go through the gateway. In `ask`/`block`, any active sink lease belonging to that employee refuses a private read, including reads using another, forged or missing turn ID. When the lease store is readable and no active lease exists, these reads proceed without a reservation and audit their source. Standalone `duduclaw mcp init` and external API-key callers keep their separate external identity and can still read. Their outgoing calls remain tainted and require approval in `ask` or are refused in `block`. An unreachable gateway or an unverifiable reply refuses employee reads in enforcing modes even without a turn ID; `shadow` allows and audits the failure.

Active sink leases are written to the gateway-owned `<home>/egress_leases.db` before admission (SQLite WAL, synchronous FULL; database and sidecars are 0600 on Unix). Rows contain the lease ID, authenticated employee ID, turn ID, phase and UTC acquisition time. The durable phase is armed as `Dispatched` before granting the lease, because the receiving process may dispatch immediately; the transport retains its local not-dispatched state for safe cancellation. Release deletes the row before removing the in-memory fence. Insert failure refuses the sink in `ask`/`block`; `shadow` allows and audits. Delete failure retains the fence.

Before exposing loopback routes, the gateway restores every well-formed row as dispatched with an unknown terminal outcome. These restored leases can only expire, even if a surviving old process sends a release. Each restart starts a fresh six-hour monotonic fence for every restored row, regardless of its UTC timestamp or clock jumps. Rows are deleted only after a confirmed terminal release for a live lease or monotonic expiry during continuous uptime. Repeated restarts can extend the fence; the per-employee and global caps still apply. HTTP admission and periodic store maintenance run in the blocking pool under the same ledger mutex, sharing a limit of 64 queued/running jobs. Non-loopback peers are rejected before scheduling blocking work; saturated admission returns HTTP 503 immediately, which enforcing callers refuse. If opening, reading or migrating the store fails, all employees' private reads (tracked and untracked) and new sinks are refused in `ask`/`block`; `shadow` allows and audits. Opening is retried every five seconds, with errors logged at most once per minute. There is no fixed 300-second restart window. Turn taint itself remains in memory and is lost on restart; unknown turns remain tainted.

The store holds an exclusive `<home>/egress_leases.lock` file lock for its lifetime (0600 on Unix). A second gateway using the same home refuses private reads and new sinks in `ask`/`block` and retries; `shadow` allows and audits. After taking ownership it restores all leases before admitting reads. Never delete the lock file while a gateway is running. Duplicate active lease IDs are refused before insertion without fencing other employees.

Registration history uses a separate bounded Bloom filter per employee and is never cleared during that gateway process, so re-registering an evicted or expired turn cannot make it clean. Hash collisions or a saturated filter can conservatively taint a new turn for that employee. History holds at most 1,024 employees (32 KiB each, 32 MiB total); turns for additional employees are always treated as tainted. History is not persisted across restart.

## Observation report (unreleased E3)

The Admin-only Security report compares the last 7 or 30 UTC days. The approval estimate counts distinct calls that would ask; pre-check and dispatch confirmation share a call ID. Refusal observations appear in the breakdowns but are not approval tickets. Breakdowns show employee, tool, closed private-source class, date and CLI built-in versus MCP entry point. An empty result means no recorded observations, not proof of safe sending. Collection failures do not change gate decisions and can leave the report incomplete.

`<home>/egress_shadow.db` uses SQLite WAL and Unix 0600 permissions, retains 35 days, and prunes on writes at most hourly. It stores no content, recipients or free-text source names. Existing tool-call audit rotation does not limit this report. `security.egress_shadow_report` accepts only `days: 7` or `days: 30` and requires Admin access.

The report covers MCP outgoing gate evaluations, computer actions and gateway hook checks. Read-reservation conflicts, unsupported-runtime spawn warnings and local hook transport failures remain in the tool audit log and are not included in these call counts.

## Recovery

`duduclaw doctor` reports lease count, oldest age and whether the store is readable. Check gateway logs for `egress lease store unavailable; private reads fenced` and its `error` field when the store is unhealthy. In `ask`/`block`, an unreadable store refuses private reads and new outgoing actions.

Before clearing leases, stop every gateway using this home, disable automatic restart, and confirm that employee CLI processes and their MCP/tool children have stopped their outgoing actions. If an old action may still be running, keep its fence and investigate first.

```sh
duduclaw egress-leases list --json
duduclaw egress-leases clear --all --confirm <token-from-list>
# To clear one lease, use that lease's token from list:
duduclaw egress-leases clear --lease <id> --confirm <lease-token>
```

Run these commands from the operator's terminal; AI sessions are refused. Both commands require the gateway to be stopped and the lease lock to be available. An unreadable gateway lock is a refusal, not evidence of a stopped gateway. `clear` holds the exclusive lease lock, lists the current leases, creates a timestamped private database backup, checks the token against the sorted selected lease IDs, deletes in one transaction and writes a security audit event. A changed set invalidates the token. Keep the backup and never unlink `egress_leases.lock`.

Clearing a lease removes its protection against concurrent private reads. Restart only after confirming the associated outgoing actions have stopped, then check doctor and gateway behavior. The command does not repair a corrupt or unreadable database; a refusal must be investigated rather than bypassed by deleting database or lock files. Recovery uses local offline confirmation because dashboard approval requires a running gateway.

## Review before approving

Only a current Admin may decide an outgoing-data approval, including requests without a workflow binding. Channel decisions are refused. The Admin-only dashboard outgoing-data approval shows the destination, full arguments expanded in a scrollable panel, source tools and expiry. Send, Publish and Purchase approvals also show full arguments. One approval binds one employee, tool and digest of the complete canonical arguments and can be used once. An action-rule approval and the outgoing-data reason share one request.

Action-rule approvals show the matching tool or effect rule, including for read and draft calls. Purchase and Delete actions cannot receive new standing capability or workflow grants; grants saved before this feature are ignored without changing the stored records. Other gates and explicit approvals still apply.

## CLI built-in tools (unreleased E1)

Claude and Grok receive a `PreToolUse` hook for `WebFetch`, `WebSearch` and literal Bash network commands. Grok aliases include `Fetch`, `Shell`, `web_fetch`, `web_search` and `run_terminal_command`; their actual matcher and environment behavior still need live acceptance. Ordinary non-network Bash returns locally without contacting the gateway. Detection recognizes `curl wget nc ncat netcat socat ssh scp sftp rsync ftp telnet http httpie aria2c lftp`, and `git push/fetch/clone/pull/ls-remote`. Shell `-c`, `eval` and echo/printf arguments use bounded reparsing (two levels, 64 KiB per payload). Echo/printf command-shaped data can conservatively match even when not executed.

| Mode | Clean turn | Private data read | Ledger unavailable or unknown turn |
|---|---|---|---|
| `off` | Allow | Allow | Allow |
| `shadow` | Allow | Allow, audit `would_ask` | Allow, audit `would_refuse_ledger_unavailable` |
| `ask` | Allow | File approval, refuse until retry | Refuse |
| `block` | Allow | Refuse | Refuse |

The hook posts raw tool arguments to `/api/internal/egress-guard/hook-check`. The gateway canonicalizes and hashes them and builds the card; client digests and summaries are rejected. Requests and responses are signed; the port comes from `config.toml`, not an inherited port variable. Hook checks have their own per-employee 120/minute bucket. The live host principal context supplies the requester for self-approval checks; an unavailable context cannot create an ask ticket.

In `ask`, retry the exact same arguments in the same turn after a current Admin approves in the dashboard. The ticket binds employee, turn, tool, complete arguments and covered sources; `WebFetch` includes the full URL and query. Identical pending requests reuse one ticket. New sources invalidate the previous ticket. Consumption is single-use, so a second retry needs another approval. Bash tickets require one direct simple command and one network verb: no expansion, substitution, pipes, redirects, `@file`, wrappers or interpreters. Unsupported shapes are refused with an explanation when approval would be required. This binds text, not DNS results, executable contents, configuration files or symlink targets.

In `ask`/`block`, handled hook errors and panics exit 2, and hook installation failure stops the spawn without account/runtime rotation. `shadow` logs and continues. In enforcing modes Claude loads only the identity owner's `.claude/settings.json` via `--setting-sources "" --settings <file>`, even when cwd differs (ambient project/local/user settings are excluded); `cli_bare_mode` is refused in enforcing modes because it skips hooks. Grok adds `--disable-web-search`; enforcing modes currently refuse borrowed/shared work roots or another employee's guard file in the same hook directory. Use an independent employee directory or Claude/Codex for these runs.

Codex `ask`/`block` spawns add `-c sandbox_workspace_write.network_access=false -c web_search="disabled"`; FullAccess is reduced to WorkspaceWrite, while ReadOnly stays narrower and the MCP approver is retained. This is a spawn restriction, not per-command approval. Generic CLI runtimes remain unsupported and audit `would_refuse_unsupported_runtime` in enforcing modes. Antigravity is unchanged; OpenAI-compatible tool loops retain their MCP gate.

Cron, dispatch, reminder, mail, heartbeat and ephemeral execution entry points now register a host turn and propagate `DUDUCLAW_TURN_ID` independently of channel requester identity, including the resolved employee behind `default`. Existing taint is never cleared; unknown client turns remain tainted.

**Bash hooks are a speed bump and telemetry, not a containment boundary.** Native `Read` and Bash file reads do not mark taint. Interpreters, scripts, aliases, renamed binaries, `/dev/tcp` and dynamic command assembly can bypass lexical detection. A missing/killed/timed-out hook may be ignored by the upstream CLI. Same-OS-user access to hook files, keys or process environment is not isolated by request signatures. Native preflight does not hold the MCP sink lease through execution or prevent concurrent reads after the check. Deny Bash or use the task sandbox for containment; the task sandbox covers delegated tasks, not every channel/CLI run. The Security page lists enforcing-mode employees with Bash and no enabled task sandbox.

E1 validation status: tests were written but not run, and no compilation or live CLI probes were performed in this work package. Pending: Claude/Grok hook matching, inherited turn/principal context, approval/retry/single use, gateway-outage modes, Claude borrowed cwd, and Codex 0.164.0 network/search restrictions with MCP still working.

## Known limitations

Direct `[[mcp.external]]` mounts in the API tool loop (stdio, HTTP and the Google closed bridge) also pass the guard. Their tool registry does not retain MCP annotations, so these calls are conservatively classified as **Modify**, even when the server describes them as read-only; exact per-tool action rules still apply. The MCP proxy and remote bridge retain their annotation-based decisions with `trusted_read_hint_servers`. A tainted direct-mounted read can therefore require outgoing approval.

Taint is tracked per turn; active sink fences apply per employee. This MCP guard does not provide a network sandbox. Unreleased E1 adds the native-tool checks described above; they do not provide a runtime-wide network boundary. Private data supplied in prompts or read outside the covered MCP routes is not automatically classified. A new turn has a new ledger key; the guard does not follow information copied across turns. Turn taint is not durable across gateway restarts; active sink leases are persisted separately. Observation mode allows outgoing calls and only records what would require approval.

See [Security defense](05-security-defense.md) and [Dashboard settings](../guides/dashboard-settings.md).

Verification scope: automated tests cover classification, gate and approval behavior. End-to-end behavior on a live employee and real outgoing service has not yet been verified.

For direct external scoped/closed connections, the existing operation deadline can expire before the approval card. A later approval does not restart the expired call; submit a new call. The old card may remain pending until its displayed expiry.

Third-party pending calls are capped at 512 per bridge/proxy, reject duplicate in-flight IDs, and expire after five minutes. Terminal paths clear correlation slots; cancellation and timeout keep gateway taint. A process that already cached taint stays conservative even after a payload-less failure.

Outgoing employee calls acquire a gateway-owned sink lease immediately before dispatch and recheck taint after any approval wait. The fence uses the authenticated employee identity across turns; it does not block other employees. Reads are refused immediately with a retriable message in `ask`/`block`; `shadow` permits them and audits `would_refuse_read_during_sink`. The lease has not-dispatched and dispatched phases, marked before the first potentially partial write. Cancellation before dispatch releases it. After dispatch, only a correlated terminal response or confirmed upstream cancellation permits release; local timeout, cancellation, task abort, panic, partial-write failure or connection loss keeps the fence for **six hours from lease acquisition**, independently of turn expiry. Each employee holds at most 512 leases, with a gateway-wide limit of 65,536. Turn pruning and capacity pressure never evict active leases. A full lease ledger refuses new leases in enforcing modes. Turns have a per-employee quota of 4,096 and a global cap of 65,536. Before refusing a turn registration, the gateway may evict a clean turn with no pending read, lease or source; evicted or refused turns remain tainted. Turn capacity does not refuse another employee's lease. Newly observed sources after approval require a fresh call and approval.

The remote bridge uses generation-specific wire IDs and discards expired responses, so a late response cannot terminate a newer sink that reused the caller ID. Caller request IDs in the reserved `duduclaw-egress:` namespace receive an explicit refusal.

The proxy also bounds response-redaction contexts to 512 entries for five minutes, refuses duplicate IDs without replacing their context, and removes locally refused calls. Forwarded calls use unique wire IDs so late responses cannot use a newer call's redaction rules; expired responses are withheld. Completed call tasks are reaped continuously, with at most 512 active tasks.

Built-in completion uses an exact list of checked handler contracts. Local mail drafts, task/agent queues and proposals complete when their durable admission finishes. Errors retain the fence conservatively. `model_search`, `model_download`, `skill_search`, `codrive_run`, `browser_record_start` and `os_open` also retain it until the lease TTL even after an apparent success: their reports, fallback attempts or background work do not confirm every outgoing action has ended.
