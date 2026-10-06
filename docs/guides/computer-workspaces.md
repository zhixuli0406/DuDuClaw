# Computer-use workspaces: files that outlive the computer-use session

A computer-use container (the `computer_*` tools) is removed when the session ends, and whatever the AI employee gathered in the browser goes with it. A computer-use workspace is a folder the gateway keeps: during one session the employee writes the text it has put together into it, and in a later session (even after a gateway restart) it attaches the same workspace and carries on.

A workspace belongs to one AI employee. Only the gateway writes to it; a container that attaches it can see it read-only. The feature is off by default and needs both a global switch and the employee's own switch.

macOS and Linux only. On Windows every entry point refuses (it never falls back to something else).

## Prerequisites

- Computer use already works: Docker, the computer-use image, the employee's `computer_use = true`. See [Browser automation and computer use](../features/08-browser-automation.md).
- The gateway is running. The three workspace tools are forwarded to it.
- Free disk space above `min_free_bytes` (default 512 MiB), otherwise writes are refused.

## Switching it on

The global switch is in `config.toml`:

```toml
[computer_use.workspaces]
enabled = true             # default false
max_per_agent = 3          # workspaces per employee, 1–20
max_bytes = 67108864       # capacity per workspace (64 MiB), 1 MiB–1 GiB
max_files = 1000           # files per workspace, 1–10000
retention_days = 30        # unused this long ⇒ expired; 0 = never, at most 3650
min_free_bytes = 536870912 # disk space the host must keep free (512 MiB)
admin_approval_minutes = 30  # how long a dashboard approval of a terminal action stays usable, 1–1440
```

The section is checked strictly: a wrong type, an out-of-range value or an unknown key makes the whole section invalid, which means the feature is off (the defaults are not used) and `duduclaw doctor` fails the row. The section is read on every use, so changes apply without a restart.

The employee's switch is in its `agent.toml`:

```toml
[capabilities]
computer_use = true

[capabilities.computer_use_config]
workspace = true
```

Without both switches nothing can be created, attached or written.

## How an employee uses it

| Tool | What it does |
|---|---|
| `computer_session_start` with `workspace` | `"new"` creates a workspace and attaches it; a workspace id (`ws-…`) attaches an existing one. The answer carries `workspace_id`, `mount_path`, the revision and usage |
| `computer_workspace_list` | The employee's own workspaces: state, revision, usage, quota, expiry, whether a session holds it, and the files (path, size, sha256) |
| `computer_workspace_read` | Read one UTF-8 text file (at most 48 KiB). The content comes back inside a data fence with injection-scan flags |
| `computer_workspace_write` | Write one UTF-8 text file into the workspace **this employee's live session attached**. Optional `expected_revision` (the revision last seen): refused if the workspace changed since |

A typical run: `computer_session_start(workspace="new")` → browse and take screenshots → `computer_workspace_write` the notes → stop the session. Later, `computer_workspace_list` gives the id and `computer_session_start(workspace="ws-…")` attaches it again.

Reading and listing need no session; writing always does.

## Where the files are

- On the host: `<home>/computer_workspaces/<id>/data/`, directories 0700, readable only by the user that runs the gateway.
- In the container: `/workspace/files`, read-only. It sits under a small root-only tmpfs (`/workspace`, mode 0700), so the browser's `sandbox` account cannot reach it; only commands the gateway runs as root can.
- The employee's state file `agents/<employee>/state/computer_workspaces.json` holds the owner credential of each of its workspaces (see "Removed or re-created employees").

## Rules and limits

- Paths: relative, at most 4 levels, each level letters, digits, inner spaces and `-_.()（）` only, never starting with `.`, no leading or trailing space. Paths are NFC-normalised.
- One file is at most 48 KiB of UTF-8 text. The tool request itself is capped at 64 KiB after JSON encoding, so content full of quotes, backslashes, newlines or control characters grows when escaped and the practical limit drops below 48 KiB; split such content over several files.
- Quota: a write that would pass `max_bytes` or `max_files` is not performed and every existing file stays.
- Writes are atomic: temp file, `fsync`, `rename`, then a directory sync. A failure halfway leaves the old file as it was.
- One write per workspace at a time (a cross-process file lock). Two writes with the same `expected_revision`: exactly one succeeds, the other gets a revision conflict.
- Unprocessable items: files over 48 KiB, hard links, symbolic links, special files and names that break the path rules are never read and never named; `computer_workspace_list` only reports how many there are (`unprocessable_items`). They only appear when someone puts files there from the host.
- Hashes: the listed sha256 comes from the gateway's ledger, not from re-reading the folder each time. A file the gateway cannot read is reported with `sha256: null` and `hash_unknown: true`.
- Writes are refused while the session is stopped or paused or the threat level is not GREEN; reading and listing still work.

## Retention

A workspace not attached for `retention_days` becomes **expired**: it cannot be attached or written, its files are not deleted, and the owner can still list and read it. Expired workspaces do not count towards `max_per_agent`. An operator can extend it with `renew` (below).

## The lease: one session at a time

A workspace is attached by at most one session. The gateway renews the lease every 15 seconds; a lease lasts 90 seconds.

- When the session ends, the lease is released at once.
- If the gateway dies during normal operation, another session can attach within 90 seconds.
- **If the gateway dies while a session is starting** (waiting for an approval or for the container), the start lease covers the whole start budget, so the workspace cannot be attached again for up to about 8.5 minutes (90 seconds plus the 415-second start budget).

**Only one gateway maintains the registry.** Only the gateway that holds the data directory's instance lock reconciles the workspace registry (expired leases, write intents, unfinished creates and deletes, retention) and removes stale workspace containers, at start and on every 10-minute pass. A second gateway on the same data directory serves its own sessions and renews its own leases, but does no registry maintenance and removes no workspace containers; leftover containers that are exited or past their deadline are still cleaned by every gateway. Dashboard and command-line actions are unaffected.

## Removed or re-created employees

A workspace belongs to the employee that created it, not merely to the name. At creation a random credential is written both into the registry and into that employee's state file, and every attach, read, write and list compares the two.

The workspace becomes **ownerless** when either holds:

- the employee was removed (`agent.toml` is gone, or `_trash` holds an entry of that name newer than the workspace), or
- the employee now carrying the name was re-created and does not hold the credential.

An ownerless workspace can only be handled by an operator (deleted). Re-assigning a workspace to another employee is not available in this version.

## Operator commands

```bash
duduclaw ops computer-workspaces list [--owner <employee>]
duduclaw ops computer-workspaces fence <workspace id> [--reason <reason>]
duduclaw ops computer-workspaces revoke <workspace id>
duduclaw ops computer-workspaces regrant <workspace id>
duduclaw ops computer-workspaces renew <workspace id>
duduclaw ops computer-workspaces delete <workspace id> --confirm
```

| Action | Effect | Approval |
|---|---|---|
| `list` | Every workspace (state, owner, usage, lease end) | not needed |
| `fence` | Takes control away from the current session | needed |
| `revoke` | Suspends it: status only, no read, write or attach | needed |
| `regrant` | Lifts a revoke | needed |
| `renew` | Extends retention; an expired workspace becomes usable again | needed |
| `delete` | Deletes the workspace and all its files, irreversibly | needed |

**Every state-changing action on the command line needs a dashboard approval** (only `list` does not). The terminal cannot tell the operator from an AI employee with Bash; if `fence` / `revoke` applied at once, any employee with Bash could stop someone else's workspace. **For an emergency, use the dashboard** (`computer_workspaces.*`, run by a signed-in Admin, immediate, and it waits for the in-flight action) **or switch `[computer_use.workspaces]` off**.

The flow: the first run only files a request in the dashboard inbox, prints `請到儀表板的待辦核准` ("approve it in the dashboard inbox") and exits non-zero. After an Admin approves it in the dashboard, **run the same command again** within `admin_approval_minutes` (default 30) and it acts.

- An approval is bound to the action, the workspace id and the workspace's state version. If the state changes after the approval (a session attaches, a fence), the approval is void and a new request is needed.
- An approval is used once. If the action fails after the approval was used, the message says the approval was used and a new request is needed.
- Only an Admin decides, and only in the dashboard; buttons and replies in chat channels never count, and non-Admin dashboard accounts cannot approve.
- One pending request per workspace and action: a state change updates that request instead of filing another, without a new notification. At most 2 notifications per workspace per hour; beyond that the request stays in the inbox and an audit event is written.
- The notice carries only the workspace id, owner, action, file and byte counts, and the sentence 「這筆請求由本機指令列建立，系統無法確認下指令的人是誰」 ("this request came from the local command line; the system cannot confirm who typed it").

**Other rules:**

- The command refuses to run when any DuDuClaw session or identity variable is present (for example `DUDUCLAW_AGENT_ID`, `DUDUCLAW_TURN_ID`).
- The agent-file-guard hook refuses `duduclaw` / `duduclaw-pro` `ops computer-workspaces` in an AI employee's Bash (and for unverified callers).
- Terminal actions are audited as `本機指令列（身分未驗證）` (local command line, identity not verified).
- Every terminal action writes a security audit row when it is requested, applied and refused (`security_audit.jsonl`, event `computer_workspace_cli_action`).
- An approved `fence` / `revoke` / `delete` applied from the terminal changes the registry only (the terminal process holds no sessions): an action of a running session that has not passed its last check is refused at once, one that already passed it finishes, and the session ends at its next operation or at the next lease renewal (about 15 seconds).

## `duduclaw doctor`

The 「電腦操作工作區」 (computer-use workspaces) row:

- Off: pass, saying it is off.
- Unparsable section, unsupported platform, unsafe `computer_workspaces/` root, registry that cannot be opened: fail.
- No Docker daemon id, disk below `min_free_bytes`, workspaces left deleting / failed to create / ownerless, writes whose outcome is unknown, writes that landed after the lease was lost, a data directory that cannot be read: warn, naming the workspace ids.

When the feature is off and no workspace was ever created, the gateway does not create `computer_workspaces.db`.

## Switching it off

Setting `enabled = false` (or the employee's `workspace = false`) stops new creates, attaches and writes, and an attached session ends at its next operation. The owner can still list and read existing content. To stop reads too, `revoke`; to remove the content, `delete`.

## Known limitations

- **Owner isolation holds for the three workspace tools only.** An AI employee with `Read` or Bash can read `<home>/computer_workspaces/` on the host directly. One with unrestricted Bash can also get around the command-line identity check and the approval gate: edit `approvals.db`, `computer_workspaces.db` and the workspace directories directly, or mark its own request approved in the database. The Bash check is a speed bump. Real isolation is not granting Bash, or the [task sandbox](task-sandbox.md).
- The owner credential lives in the employee's own directory, which that employee can read and write; an employee with `Read` can also read a removed predecessor's credential under `agents/_trash/`. The check stops a re-created employee that uses only the product tools, not one with unrestricted file access.
- Files are plaintext on disk. Redaction's "never restore" only means masked tokens are not turned back into values on write; it does not mean no personal data is on disk.
- `denied_tools` listing the workspace tools does not stop `computer_session_start` with `workspace` from mounting the workspace read-only into the container. The switch for that is `[capabilities.computer_use_config] workspace`.
- With the feature off, the owner can still list and read existing content (previous section).
- The browser runs with `--no-sandbox` (as computer use already did).
- There is a short window between the mount-source check and `docker run`; a program running as the gateway's OS user could use it. Inside the container only root can enter `/workspace`, and no tool reads the workspace from inside the container today.
- Root `docker exec` can write the 64 KiB tmpfs at `/workspace` (`/workspace/files` itself is read-only); only the gateway runs such commands.
- When a start is cut off (time-out or dropped connection) after the lease was taken, the lease is not released at once; it lapses with the start lease (up to about 8.5 minutes).
- An ownerless workspace can only be deleted, not re-assigned.
- Real-container tests ran only on Docker Desktop on macOS arm64; native Linux Docker, amd64 and Windows are not verified. On Windows the feature is unavailable.
