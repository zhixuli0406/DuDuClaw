# Task sandbox: run an agent's delegated tasks in a locked-down container

The task sandbox runs a delegated task for one agent inside a Docker container: the agent's AI CLI starts in a read-only, non-root, resource-limited container with a private throw-away workspace, and only the final reply text comes back. It is off by default and is switched on per agent.

This is not the script sandbox used by `duduclaw secaudit` (the PoC step) and PTC. That one is a separate code path: it runs a script, not an AI CLI, with no network by default. The two share one thing, the image: the script sandbox reads the same `config.toml [container.sandbox] image` key, defaults to the same platform image, and is never pulled automatically either, so one `docker pull` covers both.

## Prerequisites

- Docker, reachable by the user that runs the gateway. Only Docker is supported for the task sandbox.
- The gateway must not run as root. A uid of 0 is refused before anything starts, and a gid of 0 is refused when the container is created.
- A sandbox image on the same machine. The default is the platform's own published image for the running version, `ghcr.io/zhixuli0406/duduclaw:v<version>`. It is never pulled automatically:

  ```bash
  docker pull ghcr.io/zhixuli0406/duduclaw:v<version>
  ```

  Replace `<version>` with your gateway version (the tag carries a `v` prefix, for example `v1.67.0`), or set your own image (see the reference table below).
- An account the AI inside the container can use (see [Credentials](#credentials-per-runtime)).
- A supported runtime: Claude, Codex, Gemini (deprecated), Antigravity, Grok, or OpenAI-compatible.

## Enable it for one agent

In the agent's `agent.toml`, turn the sandbox on and give it network access:

```toml
[container]
sandbox_enabled = true
network_access = true   # required: the AI inside must reach its model provider
timeout_ms = 600000     # per-task time limit (optional)
```

`network_access = false` is refused before a container is created, because the AI CLI cannot work without reaching its provider. The sandbox does not open the network behind your back. Limiting egress to the provider only is not implemented (see [Known limits](#known-limits)).

The task time limit stays in `agent.toml [container] timeout_ms`.

## What the switch covers

With `sandbox_enabled = true`, every way the gateway can start the employee's AI CLI falls into one of three groups.

**Runs in the sandbox.** Tasks the gateway hands to the employee as a whole unit of work:

- a task delegated by another agent or sent from the dashboard (the inter-agent bus),
- a heartbeat wake-up that picks up work from the task board,
- an autopilot `delegate` or `run_skill` action,
- a goal-loop round (always Solo, see below),
- the steps of a multi-step task plan.

**Never runs that way for a sandboxed employee.**

- Team rounds: an employee with the sandbox on never forms a team. The decomposability gate returns Solo with the reason `sandbox_enabled` before any other rule, including `gate = "always_team"`, so every goal round runs Solo inside the sandbox and no role member (planner, executor, verifier, synthesiser) runs on the host.
- The Agent Mail arrival trigger is skipped. The triggered run's effect depends on platform tools that do not exist inside the sandbox, and it handles untrusted inbound content. The message stays in the employee's inbox for a person to handle; nothing runs. The audit event below is written with `path = "mail"` and `action = "skipped"`.

**Still runs on the host, by design.** These need the platform tools and the conversation state that the sandbox withholds:

- channel replies (the employee answering a chat message),
- scheduled (cron) tasks,
- reminders,
- the heartbeat's proactive check (`PROACTIVE.md`),
- ephemeral agents spawned on the employee's behalf,
- `duduclaw acp` sessions,
- live `duduclaw eval` runs.

None of this is silent. The first time one of these paths runs (or is skipped) for a sandbox-enabled employee, the gateway writes the audit event `task_sandbox_not_applied` with the details `{path, action}` and one warning to the log. `path` is one of `channel_reply`, `cron`, `reminder`, `mail`, `proactive`, `ephemeral`, `acp`, `eval`; `action` is `ran_on_host` or `skipped`. The event is written once per (employee, path) per gateway process, so a busy channel does not flood the audit log; after a gateway restart it is written again on first use. An employee with the sandbox off gets no event.

## `config.toml [container.sandbox]` reference

All keys are optional. The section is read on every sandboxed task, so a change applies to the next task without a restart. An unknown key or an out-of-range value makes the whole section invalid, and an invalid section means the sandbox is unavailable (it does not fall back to defaults). The exceptions are `when_unavailable` and `script_when_unavailable`, which are each read on their own, so an escape hatch keeps working while you fix another key. An unknown value of either one means `"fail"`.

| Key | Default | Meaning |
|---|---|---|
| `image` | `ghcr.io/zhixuli0406/duduclaw:v<running version>` | Image to run. Must already be on the machine. |
| `[container.sandbox.executables]` | `claude`, `codex`, `gemini`: `/usr/bin/<name>`; `antigravity`: `/usr/local/bin/agy`; `grok`: `/usr/local/bin/grok`; `openai-compat`: `/usr/local/bin/python3` | Absolute path of each runtime's executable inside the image, keyed by `claude`, `codex`, `gemini`, `antigravity`, `grok`, `openai-compat`. Needed only for a custom image. |
| `memory_bytes` | `4294967296` (4 GiB) | Memory limit (swap equals memory). At most 64 GiB. |
| `pids` | `128` | Maximum processes. |
| `cpu_millis` | `1000` | CPU limit in thousandths of a core. |
| `tmp_bytes` | `268435456` (256 MiB) | Size of the writable `/tmp` (a tmpfs). At most 16 GiB. |
| `workspace_bytes` | `536870912` (512 MiB) | Size of the `/workspace` working directory (a tmpfs). At most 16 GiB. |
| `max_turns` | `30` | Step limit for one task. |
| `when_unavailable` | `"fail"` | What happens to a delegated task when the task sandbox cannot run: `"fail"` or `"run_unsandboxed"`. Task sandbox only. |
| `script_when_unavailable` | `"fail"` | What happens to a PTC `execute_program` script when the script sandbox cannot run: `"fail"` or `"run_unsandboxed"`. Script sandbox only. |

Example:

```toml
[container.sandbox]
image = "ghcr.io/zhixuli0406/duduclaw:v<version>"
memory_bytes = 4294967296
max_turns = 30
when_unavailable = "fail"
```

Every number must be a positive integer. The other upper bounds are 65536 for `pids`, 256000 for `cpu_millis` and 1000 for `max_turns`. Both tmpfs mounts are charged to the container's memory limit, so `tmp_bytes + workspace_bytes` must not exceed `memory_bytes`. Breaking any of these rules makes the whole section invalid, and the sandbox is then unavailable.

## Credentials per runtime

The sandbox takes an account from the account rotator for the agent's runtime, and respects the agent's `account_pool`. Only credentials that can be handed to a container work.

| Runtime | Usable account |
|---|---|
| Claude | An API key, or an OAuth account that carries a token (created with `claude setup-token`). An account that only exists as a login in the host keychain cannot enter a container. |
| Codex | An API key, or a credential document: an OAuth account whose stored secret is the CLI's `auth.json`. |
| Grok | An API key, or a credential document (the CLI's `auth.json`). |
| Gemini / Antigravity | A Gemini API key. |
| OpenAI-compatible | That provider's API key. |

If no account fits, the task fails and the error says which kind of credential this runtime needs.

## What is and is not available inside

Available:

- A read-only root filesystem, a non-root user, all Linux capabilities dropped, `no-new-privileges`.
- Memory, process and CPU limits from the table above.
- The working directory `/workspace`: a tmpfs of `workspace_bytes`, owned by the user the container runs as and discarded with the container. Nothing the task writes there reaches the host disk.
- A writable `/tmp` tmpfs of `tmp_bytes`.
- A few entries of the agent directory, each mounted read-only on its own at `/agent/<name>` when it exists: `SOUL.md`, `IDENTITY.md`, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, `CONTRACT.toml`, `SKILLS/`, `wiki/`. An entry that is a symlink, a hard-linked file, of the wrong kind (a file where a directory is expected, or the reverse), or that resolves outside the agent directory is skipped with a warning in the gateway log.
- File and shell tools.

Not available:

- The rest of the agent directory. `.mcp.json` (which holds the agent's MCP key and identity token), `.claude/`, `state/`, `agent.toml`, the databases and any other entry not listed above are not mounted.
- Platform MCP tools (memory, tasks, channels).
- Web tools and sub-agents.
- Writing back to the agent directory. File changes inside the sandbox are discarded; the task's output is its final reply text.

If the AI uses a tool outside the allowed file and shell set, the task is stopped and the audit event `task_sandbox_tool_violation` is written. If it reaches the step limit, you get its last reply text, or an error when there is none.

## When the sandbox cannot run

With `sandbox_enabled = true`, a task fails (it does not run unsandboxed) when any of these holds: Docker is unreachable, the image is missing, the gateway runs as root, the host is not a unix system, the configuration is invalid, `network_access = false`, no usable account exists, or the runtime is not supported. Each failure writes the audit event `task_sandbox_unavailable` with a reason code and returns a readable error.

Escape hatch: set `when_unavailable = "run_unsandboxed"` in `config.toml [container.sandbox]`. Tasks then run on the host without isolation, as older versions did, and every such task writes the audit event `task_sandbox_bypassed`. Use it only when you accept running without isolation.

The script sandbox that PTC `execute_program` uses has its own key, `script_when_unavailable`; `when_unavailable` does not affect it, and neither key affects the other. They are separate because the risks differ: one lets a delegated AI task run on the host without isolation, the other lets a submitted script run on the host. With the default `"fail"`, a script whose sandbox cannot run is not run at all: the tool returns `Script sandbox unavailable (<code>): …` with the `docker pull <image>` command, and the audit event `script_sandbox_unavailable` is written. With `script_when_unavailable = "run_unsandboxed"`, the script runs on the host as in older versions, and `script_sandbox_bypassed` is written every time. Both events carry a `reason` and the script `language`. The reason codes are `invalid_config`, `no_runtime`, `runtime_unhealthy`, `image_missing`, `create_failed` and `start_failed`. The `duduclaw secaudit` PoC step has no escape hatch: it never runs on the host.

## Cleanup of leftovers

Task containers are created with `--rm` and `--pull never`, and the gateway removes each one when its task ends. When a gateway was killed mid-task, a background sweep removes what is left. It runs at gateway start and then every 10 minutes, for this gateway home only (containers of other homes on the same Docker daemon are never touched):

- containers that have exited or are dead, or that are more than 600 seconds past their deadline;
- run directories under `<home>/sandbox/runs/` that no remaining container uses and that are older than 600 seconds.

If Docker cannot be listed, nothing is deleted. A failed cleanup, by the sweep or when a task ends, writes the audit event `task_sandbox_cleanup_failed` with a `reason` and a `count`. A home where the sandbox never ran is skipped without contacting Docker.

## Verify with `duduclaw doctor`

```bash
duduclaw doctor
```

The task sandbox check reports whether Docker is reachable, whether the sandbox image is present locally, which agents have the sandbox enabled, and warns about any of them with `network_access = false`. When at least one employee has the sandbox on, the row adds a line saying that those employees' channel replies, scheduled tasks and reminders still run on the host, that their goal rounds always run Solo (no team), and that new mail does not wake them. The check is a precondition check; it does not run a task.

## Troubleshooting

A failed sandbox task replies to whoever delegated it. When the sandbox refused to run, the reply reads `⚠️ 子任務未執行（任務沙箱）：Task sandbox unavailable (<code>): …`; that is the first group of rows. When the task started and then failed, it reads `⚠️ 子任務失敗（任務沙箱，<code>）：<message>`. The reply carries the reason and the remedy only. Raw output from the AI CLI or from Docker is never put in the reply; it is written to the gateway log (one `warn` line per failure, with the agent and the reason code).

| What you see | Cause | Fix |
|---|---|---|
| `(docker_unreachable)` | The Docker daemon is not running or the gateway user cannot use it. | Start Docker; check that the gateway user can run `docker version`. |
| `(image_missing)` | The image is not on this machine. | Run the `docker pull <image>` command shown in the error, or point `image` at an image you already have. |
| `(network_disabled)` | The agent has `network_access = false`. | Set `network_access = true` in the agent's `[container]`. |
| `(root_user)` | The gateway runs as uid 0. | Run the gateway as a normal user. |
| `(unsupported_platform)` | The host is not a unix system (for example Windows). | Run the gateway on Linux or macOS. |
| `(no_account)` | No account fits this runtime inside a container. | Add an account of the kind listed in the error (see the credentials table). |
| `(unsupported_runtime)` | The agent's runtime cannot run in the sandbox. | Use one of the supported runtimes. |
| `(invalid_config)` | A bad value in `[container.sandbox]` (including `tmp_bytes + workspace_bytes` above `memory_bytes`), or the agent has no model or a zero timeout. | Fix the key named in the error. |
| `authentication failed: ...` | The provider rejected the credential. | Replace the credential of the account named in the error. |
| `... rate-limited or rejected the request for quota` | Provider quota. | Wait, or add another account. |
| `... used a tool outside the allowed file/shell surface` | The AI tried a tool the sandbox does not allow. | Delegate this task to an agent without the sandbox, or change the task. |
| `... reached the sandbox step limit` | `max_turns` was reached before a reply. | Raise `max_turns` or split the task. |
| `... timed out after N s` | The agent's `timeout_ms` was reached. | Raise `timeout_ms`. |
| `... sandbox container refused: the gateway runs as root (uid or gid 0)` | The gateway's group id is 0. | Run the gateway as a user whose primary group is not root. |
| `Script sandbox unavailable (<code>): ...` (from `execute_program`) | The script sandbox cannot run and `script_when_unavailable` is `"fail"`. The code is one of `invalid_config`, `no_runtime`, `runtime_unhealthy`, `image_missing`, `create_failed`, `start_failed`. | Start Docker and run the `docker pull <image>` command shown in the error, or fix `[container.sandbox]` for `invalid_config`. Set `script_when_unavailable = "run_unsandboxed"` only if you accept scripts running on the host. |

## Known limits

- Egress is not limited to the model provider; the container has an ordinary bridge network. It can therefore reach anything the host's network can reach, including a cloud metadata endpoint (`169.254.169.254`) and, on Docker Desktop, services listening on the host.
- The credential handed to the AI CLI is visible to the AI inside the container (environment variables and the CLI's own credential file), and with open egress it can be sent elsewhere. Use an account whose loss you can tolerate and rotate, not your primary one.
- Only the task kinds in the first group under [What the switch covers](#what-the-switch-covers) are sandboxed. Channel replies, cron tasks, reminders and the other host paths listed there run unisolated; the `task_sandbox_not_applied` audit event records that they did, it does not stop them.
- No platform MCP tools inside the sandbox.
- File output is not written back to the agent directory.
- Docker only; Apple Container and WSL2 are not used by the task sandbox.
- Images are not pulled automatically.
- The script sandbox has no cleanup at start. If the process running a script (the `duduclaw mcp-server` serving the agent, or `duduclaw secaudit`) dies mid-run, the script's container keeps running until the script exits on its own.
- On Windows the script sandbox tries WSL2 first and then Docker. The WSL2 path, including the conversion of Windows paths to WSL paths, has only been cross-compiled; it has not been run on a real Windows host.
- Running the task sandbox from a gateway that itself runs in a container is not covered by this guide and has not been verified.
- What has been run for real: with the published platform image (`v1.66.1`), the Codex, Antigravity and Grok CLIs start inside the sandbox and reach their provider (checked with an invalid key, which the provider rejects), and one complete delegated task ran with Codex (that run used a locally built image with the same CLIs, not the published one). Claude, Gemini CLI and the OpenAI-compatible runtime have not been run inside the sandbox against a real provider.
