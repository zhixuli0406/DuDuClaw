# Team as employee (Team-as-Agent)

> One AI employee, four roles inside it: 規劃 / 執行 / 審核 / 合成. Each role can run on a different vendor's model. To you it is still one employee with one voice.

---

## Status

**WP-1 through WP-5 have landed: the building blocks, the carrier a role runs on, the composer that drives a round, and the `team_handoff` tool that writes the packets.** A round can now go 規劃 → 執行 → 審核 end to end. Fourteen live rounds against a real goal (Claude planner, Codex executor, Claude verifier) exposed the integration defects described below; the fourteenth round was accepted by the existing judge.

| Piece | State |
|---|---|
| `[team]` configuration schema (global + per-employee, field-wise cascade) | **Shipped** (WP-1) |
| Spec validation (runtime allowlist, model↔runtime binding, verifier-family rule, effort) | **Shipped** (WP-1) |
| `TaskPacket` — the cross-role handoff type, with its caps | **Shipped** (WP-1) — see [spec/task-packet.md](../spec/task-packet.md) |
| The decomposability gate (default Solo, L0/L1 rules) | **Shipped** (WP-1) as a pure function |
| Ephemeral **role members** (per-role `(runtime, model, effort)` scaffold, immediate teardown, separate rate/capacity budgets) | **Shipped** (WP-2) |
| Per-role effort, resolved once in `duduclaw-core::effort` and spelled per runtime at spawn | **Shipped** (WP-3) |
| Per-task spec freeze (`tasks.team_spec_json`), gate call site, three-stage round, budget degrade chain, per-role attribution (`role_turns.jsonl`) | **Shipped** (WP-4) |
| `team_handoff` MCP tool (the packet writer), packet fan-out on one leg, never-trim rendering of `constraints` / `audience` | **Shipped** (WP-5) |
| Member **native** tool events persisted as audit evidence; **artifact receipts** (path / bytes / sha256) in the verifier prompt and the settle's judge input | **Shipped** (live round 8) |
| Structured judge output for codex (`codex exec --output-schema`) | **Shipped** (live round 8) |
| Role×model capability matrix — executor + verifier cells, the bottleneck probe, `role_model_matrix.toml` | **Shipped** (P2) — see [Which model for which role](#which-model-for-which-role-p2); P2b full-team 2×2 probe completed one four-arm live smoke (all four independent verifier verdicts were FAIL); the sample is unresolved and the file is not yet read by the composer |
| Per-role cost reporting, dashboard team card | Planned |
| Task detail 「角色」 tab (stage status, model and measured token counts) | **Shipped**; packet/tool drill-down and cost in USD remain planned |

Since v1.66 `[team] enabled` defaults to **`true`**. That is a change in one flag, not in what actually runs: three independent things still have to agree before a team forms, and an install that never wrote a `[team]` section behaves exactly as it did before.

1. **The spec has to validate.** With no `[team.roles]`, the executor and verifier both cascade onto the employee's own runtime and model, so they share a model family — and a verifier that shares the executor's family is refused outright (see [rule 3](#3-the-verifier-must-not-share-the-executors-model-family)). Naming a second vendor is what turns teams on in practice; the flag only stops standing in the way.
2. **The gate has to say Team.** `auto` needs three of its four signals; an ordinary task gets Solo, byte for byte.
3. **The budget has to afford a round.** A task whose `[dispatch.team_budget]` cannot pay for even a fully degraded first round runs Solo rather than being parked for a human over work it never started.

`enabled = false` is still a real kill switch, and an employee's explicit `false` still beats a global `true`.

The refusal in (1) is deliberately **quiet**: an unconfigured deployment gets one `debug!` line and no audit row, because the alternative would stamp a `team_refused` row on every goal task of every install — which is how a real refusal stops being findable. An operator who wrote `enabled = true`, or who configured roles that then fail validation, still gets the loud audited refusal.

When a team goal is accepted, files explicitly named in its TaskPackets are archived in the employee's `attachments/` bucket even when a shell command created them without a native Write event. The archive rechecks that each source is a regular file inside the employee workspace and obeys the same size caps as ordinary goal deliverables. The task detail's 「角色」 tab reads `tasks.role_turns` under the task Viewer permission and shows unknown token usage as unknown rather than zero.

The per-role cost ledger records Claude CLI and other runtime usage under the
goal task ID when the provider reports tokens. A team role waits for its
usage write before the stage finishes. Unreported tokens remain unknown;
estimates are labeled separately from measured usage.
Failed role rows also record the stage boundary (`failure_edge`) and a closed
`fault_side`. The writer marks ambiguous failures `unknown`, and a stage with
missing or unrecognized observation fidelity cannot blame the model. These
fields locate a symptom;
they do not authorize playbook learning until the remaining attribution gates
are connected.

For role members, `SOUL.md` starts with a stable role contract, followed by a
separate cache boundary and the employee's identity text when present. Changing
that identity leaves the role contract unchanged. Task titles, packets and
acceptance criteria are sent in the per-turn dispatch prompt; they no longer
rewrite the system prefix for every short-lived member. The explicit boundary
applies to Direct API caching; CLI runtimes manage their own cache. Provider
cache hits still need measurement before any cost reduction can be claimed.
Claude CLI role spawns set its main and subagent cache TTL to one hour; other
agent spawns retain their existing TTL behavior.
When a team round starts, the source conversation receives one progress line
through the existing goal notification path. Its estimated minutes equal the
configured progress interval times the three required stages; it is a progress
estimate, not a completion guarantee.

> **Still an experiment you supervise.** One production team round has been accepted end to end. A one-case, four-arm full-team live probe has reached the independent verifier in every arm, but every matrix cell remains `unresolved`; it cannot select production models reliably. The v1.66 default flip removed a flag that was stopping the path from ever being exercised — it did not turn this into a measured production win. The moment you name a second vendor in `[team.roles]`, watch the first few rounds; `enabled = false` and `gate = "always_solo"` both take you back.

---

## The idea

An **employee** is the only unit you see: an agent directory, its SOUL.md, its channels, its memory, its playbook. That does not change.

A **team** is a role grouping *inside* one employee:

| Role | Job | UI wording |
|---|---|---|
| `planner` | Breaks the goal down and plans | 規劃 |
| `executor` | Does the work; may be fanned out | 執行 |
| `verifier` | Accepts or rejects, independently | 審核 |
| `utility` | Summaries, classification, chores; no spawn slot | 合成 |

Each role binds its own `{runtime, model, effort}`. The employee still speaks with one voice — a team is an implementation detail of how one employee gets one task done.

---

## Default Solo

A team is **not** four roles standing by from boot. Every task first passes a decomposability gate, and most tasks stay on today's single-agent path, byte for byte.

Anthropic's own guidance is the reason: "When the work is one dependent chain, or fits in a single context, the orchestrator pays for a plan, a handoff, and a merge that a single model gets for free." OneFlow (arXiv:2601.12307) finds a single agent looping over turns matches a homogeneous workflow while keeping its cache warm, and arXiv:2609.19759 finds the multi-agent advantage only appears with long horizons *and* sparse dependencies.

The gate is zero-LLM and deterministic:

**Sandbox rule, checked first:** an employee with `agent.toml [container] sandbox_enabled = true` is always Solo (reason `sandbox_enabled`). This rule runs before every mode, including `always_team`, so a sandboxed employee never forms a team and no role member runs on the host; its goal rounds run Solo inside the [task sandbox](../guides/task-sandbox.md).

**Hard exclusions — always Solo:** a live channel turn (the facade answers), a plan-first goal still awaiting approval, a plan containing an irreversible action, or fewer than 3 goal-loop rounds of budget left.

**Four signals.** Three or more fire ⇒ team:

1. **Bulk** — at least 4 work items that can run independently, with a measured zero dependency hubs.
2. **Context** — the task does not fit one context window.
3. **Capability gap** — the executor and planner candidates differ by at least the capability matrix's declared minimum detectable effect. A gap *below* the MDE is noise (Miller, arXiv:2411.00640) and does not count.
4. **Long horizon** — at least 3 acceptance criteria, and the task produces real artifacts.

**Exactly two signals ⇒ grey band.** The planner runs once — a call the task was going to pay for anyway — and the gate is re-run against what the plan actually decomposed into. Never a coin flip.

**Otherwise Solo**, and a Solo decision can still carry an effort hint: Anthropic's cost guidance records tuning effort beating an architecture change, so the cheapest win on this path is usually a knob, not a team.

An unmeasured signal simply does not fire, which biases the gate toward Solo. That direction is deliberate — the expensive mistake is forming a team for work one agent would have finished alone.

Every decision carries a stable reason token so it can be scored against the task's real outcome later. Until that calibration is statistically supported, the dashboard labels the gate experimental. A gate that has never been scored is a hypothesis.

---

## Configuring roles

Global defaults live in `config.toml`, per-employee overrides in that employee's `agent.toml`. Both use the same shape:

```toml
[team]
enabled = true             # default true since v1.66; false is a kill switch
executor_fanout = 1        # 1..=3; out of range is clamped, never rejected
gate = "auto"              # auto | always_solo | always_team

[team.roles.planner]
runtime = "claude"
model   = "claude-fable-5-1"
effort  = "high"

[team.roles.executor]
runtime = "codex"
model   = "gpt-5.5"
effort  = "medium"

[team.roles.verifier]
runtime = "antigravity"
model   = "gemini-3.7-flash"
effort  = "low"

[team.roles.utility]
runtime = "claude"
model   = "claude-haiku-4-5"
```

### Cascade

The cascade is **field-wise**, in three hops:

1. The employee's `agent.toml [team]` value, if written.
2. Otherwise `config.toml [team]`.
3. Otherwise, for a role that named neither a runtime nor a model, the employee's own `[model] preferred`.

So an employee overriding only `[team.roles.executor] effort` keeps the global runtime and model for that role. "Unset" and "explicitly false" stay different states, which is why `enabled = false` on one employee is a real opt-out rather than an inherited `true`.

### `gate`

`auto` runs the rules above. `always_solo` is the kill switch. `always_team` is **testing only** — it bypasses every hard exclusion, including the irreversible-action one. It does not bypass the sandbox rule. An unrecognised value degrades to `auto` and is reported.

### `effort`

Values: `low`, `medium`, `high`, `xhigh`, `max` (case-insensitive). The enum itself, its per-runtime ceiling and the CLI flag each vendor spells differently all live in one place (`duduclaw-core::effort`) — `[team]` keeps the key as a raw string and hands it there to parse, so the crate never carries two spellings of the same knob. An unparseable value refuses the team (`invalid_effort`).

Four of the five first-batch runtimes accept effort per call (probed on real binaries, 2026-09-24): Claude Code `--effort`, Codex `-c model_reasoning_effort=`, Antigravity `--effort`, Grok `--reasoning-effort`. Gemini CLI has no documented equivalent — a role on `gemini` may still declare an effort, and the spawn layer is where it becomes a no-op. Refusing it at config time would make the same config non-portable across runtimes for no safety gain.

An effort declared on a role that binds neither runtime nor model is inert (the role cascades to the employee's model, which carries the employee's effort). That case is reported rather than silently dropped.

---

## Three rules the validator enforces

A spec that breaks any of these does not form a team. The task runs Solo and the dashboard says why — a partial team is never formed.

### 1. A role is a `(role, runtime, model)` triple

Goose's Lead/Worker feature stored role configs holding only a model name, so a `qwen-*` lead ended up executed by the Claude backend (goose#10731). Here, a model whose family does not belong to its declared runtime is a hard refusal (`model_runtime_mismatch`), and a model whose family the runtime catalog does not recognise is refused too. The platform never guesses a provider for a model id it cannot place.

Declaring only a `model` is fine — the catalog binds it to the runtime that serves its family. Declaring only a `runtime` is also fine; the model cascades to the employee's `[model] preferred`.

**One trap in that cascade**: the employee's `preferred` must belong to the declared runtime's family. Declare `runtime = "codex"` on an employee whose `preferred` is a Claude model and every round refuses the member (`validate_role_runtime_model`), producing zero packets until `DISPATCH_FAILURE_LIMIT` ends the loop. It fails closed, but it fails on every round — so pin the model explicitly whenever the role's runtime differs from the employee's own.

### 2. First-batch runtimes only

`claude`, `codex`, `gemini` (deprecated in v1.67.0, removed in v1.69.0: use `antigravity`), `antigravity`, `grok`. Anything else — including `openai_compat`, `qwen`, `copilot`, `cursor` — is refused (`runtime_not_allowed`).

The reason is tools, not capability: these five register DuDuClaw's MCP server natively, so a role running on one gets the full tool surface. A role that silently loses its tools produces confident tool-free narration, which the verifier cannot tell apart from actual work.

### 3. The verifier must not share the executor's model family

Same family ⇒ refuse to form the team (`verifier_same_family`), not a warning. Decorrelation is the entire mechanism (arXiv:2607.13918: a correlated verifier makes the failure rate decay only polynomially — the lever is independence, not stacking more judges).

Family is derived from the runtime catalog, so `antigravity` and `gemini` collapse onto one family: both serve `gemini-*` models, and pairing them would look like two vendors in the config while buying zero independence.

A related finding worth stating plainly: in VP-CONTROL (arXiv:2609.10969), cross-model voting that **shared evidence** still let 62.9% of unsafe proposals through, while **independent evidence sources** cut that to 22.9% — evidence independence was worth 40.9 percentage points against model diversity's 11.3. So the verifier reads the frozen contract, the packet, and the tool-activity digest — not the executor's account of its own work.

### Rules 2 and 3 are re-checked every round, not just at freeze time

Validation runs once, when the spec is frozen. But the frozen spec then lives as JSON in a mutable task row, and the field that records each role's model family had, until this pass, **no reader at all** — a restored backup from before the rule existed, or a hand-patched row, produced a spec that no longer held the invariant and nothing downstream noticed.

Every round now re-checks the stored spec before it spends a spawn: each role's runtime must still be one of the five (rule 2 — previously the executor path filtered on this and the verifier path did not), and the verifier's stored family must still differ from the executor's (rule 3). A spec that fails lands as `Failed` with `frozen_spec_family_violation` / `frozen_spec_runtime_not_allowed` in the audit row, with an operator-facing message saying which role and what to fix. The check reads the **stored** family string rather than re-resolving the catalog: re-resolution answers "what would this config mean today", and the question here is whether the spec this task is actually running still holds.

---

## Roles exchange packets, not transcripts

The only thing that crosses a role boundary is a `TaskPacket`: objective, output format, tool scope, boundaries, enumerated constraints, an audience allowlist, acceptance assertions, references to artifacts / wiki / memory / state, structured findings and next steps, the observation fidelity, and a budget.

There is deliberately no transcript, no vendor tool-call block, no thinking or reasoning payload, and no untyped escape hatch — and that is enforced by the type, not by convention. Full field table, caps and a worked example: **[spec/task-packet.md](../spec/task-packet.md)**.

### How a packet is filed (WP-5)

A role hands its packet over by calling the `team_handoff` MCP tool. The tool derives the file path — it never accepts one — from `(task, round, from_role, to_role)`:

```
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.json
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.01.json
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.02.json   … up to .99
```

One leg legitimately carries several packets: a planner fanning a goal out into four sub-tasks writes four. So a **new** `packet_id` on a leg that is already occupied takes the next numbered slot, while re-filing the **same** `packet_id` overwrites its own file — a retry after a timeout is idempotent rather than duplicating the sub-task. Writes are atomic (temp file, fsync, rename) under a cross-process lock taken on the canonical path, so the composer never reads a half-written packet. That holds for **both** writers: `team_handoff` filing a new packet, and the composer rewriting one in place when it fills a missing `sha256` or corrects a mis-declared `fidelity`. Both take the lock on the same canonical leg path, so they serialise against each other rather than against nothing.

Only the caller's own `[team_member]` section can say who is filing. There is no fallback to `[agent] role`: that field is an *org* role, it has a `planner` variant of its own, and honouring it let any employee configured that way file a forged planner→executor packet into another task's directory. An identity that carries no `task_id`/`round` pin is refused outright for the same reason — both of the packet's cross-checks would simply be skipped.

The composer reads the same slots back in numeric order — canonical file first, then `.01`, `.02`, … — which is also arrival order. A file's name is never trusted: a packet is used only when its own `from_role` / `to_role` / `goal_id` / `round` match the leg being read *and* it passes validation. A file that fails any of those is skipped with an audit row rather than guessed at, and one bad file does not lose the rest of the leg; when every candidate on a leg is bad, one further row records that the stage's emptiness is corruption rather than silence.

`team_handoff` is on the self-echo deny list, so a packet a role wrote can never serve as that role's own evidence of grounding.

### Two seams worth knowing about

- **Constraints and audience are never compressed.** The composer renders those two fields under fixed headings (`## 約束`, `## 受眾`) that the prompt-compression pipeline refuses to trim — the measured failure being that a budget taxes boundaries first, silently turning an explicit constraint back into an implicit one. Everything else in the packet stays compressible; the protected run ends where the packet does.

  **The exemption is keyed on the source, not on the text.** Those four headings are ordinary markdown, and the compression pipeline's `history` is the eleven channels' conversation history — *including messages the user typed*. A header alone therefore protects nothing. The composer writes an unguessable marker line (`<!-- ddc-protected:… -->`, 32 CSPRNG bytes minted once per gateway process) directly under each heading, and only a heading followed by that exact line opens a protected run. A user cannot produce the value: it never appears in a channel reply, and the utility summarizer is handed a transcript with the protected runs already removed. Three failure directions are deliberate — a reader with no sentinel (the `duduclaw-llm` CCR preview unless its embedder passes one, any non-gateway process) protects nothing; a marker minted before a gateway restart no longer matches, so old summaries decay into ordinary compressible history; and if the OS CSPRNG were unavailable the sentinel is empty and the exemption is simply off. Over-trimming a real constraint is expensive but recoverable; handing the exemption to untrusted text is not.

  Two ceilings survive as defence in depth against a *legitimate* emitter rendering more than a packet's field caps allow: the budget floor honours at most ~6k tokens of protected content across the whole history, and the session summarizer pins at most 4 KiB of verbatim protected text, saying so in the summary when it truncates. The marker is stripped again by the composer's settle summary — the one route where rendered packet text reaches a human — so it never shows up in `/goals` or a channel push. One consequence worth knowing: an agent-supplied `## Constraints` block built by `DelegationEnvelope::to_prompt` carries no marker and is not protected (it is not part of `history` either, so nothing changed for it in practice).
- **A role member always holds `team_handoff`.** An employee whose `[capabilities] allowed_tools` predates teams cannot name a tool that did not exist, so the handoff channel is admitted for role members regardless of that allowlist. It is the team's own mechanism, with the same reach as `working_state_*` — one derived path under `team_packets/`, nothing else. Every *other* tool a role asks for is still checked against the employee's envelope, an explicit `denied_tools` entry still wins, and the agent-facing `spawn_ephemeral` path gets no such exemption at all.

---

## Where a role actually runs (WP-2)

A role is not a second employee. It is a **role member**: a throwaway agent directory under `~/.duduclaw/agents/.ephemeral/`, created for one `(task, round, role)` and removed when that round ends. The employee stays the only thing you see — the member has no place in the registry, no place in the roster, no heartbeat, no evolution, and its spend folds back onto the employee in cost reports.

What makes a member different from the transient sub-agents DuDuClaw already had is that it carries its own brain assignment. Its `agent.toml` starts as a copy of the employee's, then four things are overwritten:

| Key | Value |
|---|---|
| `[runtime] provider` | the role's runtime, canonicalised (`agy` is written as `antigravity`) |
| `[model] preferred` | the role's model id |
| `[model] effort` | the role's effort — **written only if the role declares one** |
| `[team_member] role` / `task_id` / `round` / `parent` | which slot this member fills |

Everything else the employee configured — utility model, account pool, container isolation, budget — is inherited. Two keys are deliberately *not*: an unset effort writes no key at all, so the spawn passes no flag and the vendor's own default depth applies; and an inherited `[runtime] fallback` is dropped, because silently moving a role to another vendor on its first failure would erase the executor/verifier independence the team exists for.

Alongside `agent.toml` and `SOUL.md`, the member gets its own `.mcp.json` carrying the duduclaw MCP server plus the member's identity env (`DUDUCLAW_AGENT_ID`, its signed token, and the home/port the MCP child cannot inherit). That file is the member's only route to any tool, the handoff channel included, because the CLI reads it from the working directory and the gateway's startup fixup never descends into `.ephemeral/`. The employee's other MCP servers (a browser, say) are deliberately not copied: a member's reach stays the subset of tools the round asked for. A member also does not distil memory — it is deleted when its round settles, so anything it learned would be filed under an id that no longer resolves.

### What tools each role gets

A member's `[capabilities] allowed_tools` is the role's tool subset, and it is derived from the employee — never a fixed literal:

| Role | Tools | Effective sandbox for a codex member |
|---|---|---|
| `planner` | `team_handoff`, `shared_wiki_search`, `memory_search` | `read-only` |
| `executor` | **the employee's own effective tools** + `team_handoff` + `memory_search` + `shared_wiki_read` | `workspace-write` |
| `verifier` / `utility` | `team_handoff` | `read-only` |

"The employee's own effective tools" means its `[capabilities] allowed_tools` verbatim when it has an allowlist, and otherwise the same default set an ordinary dispatch runs with (`Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `TodoWrite`, `WebFetch`, `WebSearch`, `mcp__duduclaw__*`). Either way the executor is a subset of the employee: a read-only employee still produces a read-only executor, and a tool in the employee's `denied_tools` is never requested.

Why the executor is not a small fixed list like the others: it is the role *defined* as doing the work, and the two enforcement layers both read that list. Codex maps it to a single coarse sandbox mode — a list with no write-class tool means `--sandbox read-only`, so the member cannot `mkdir`, and `apply_patch` is refused. Claude passes it to `--allowedTools` verbatim, where an allowlist without `Write` / `Edit` / `Bash` is the same refusal one layer up. Live round 5 hit both at once.

Planner and verifier stay deliberately read-only: a planner that edits files is a planner doing the executor's job, and a verifier that can rewrite the work it is judging is not an independent verifier.

### The model still is not the model's choice

The role path is the only place a raw model id is accepted, and it is reachable **only from the gateway's composer**. The agent-facing `spawn_ephemeral` tool is unchanged: it still takes one of three tier keywords (`cheap` / `standard` / `preferred`) and still rejects raw ids. An AI employee cannot name the model it wants to run on, with or without teams.

Before anything is created, the pair is checked against the runtime catalog: the runtime must be one of the five first-batch runtimes, and the model's family must be one that runtime actually serves. A family the catalog does not recognise is an error, not a best guess — `gpt-5.4` never reaches the Claude binary to fail later at someone else's API.

### A member runs in the employee's workspace, not in its own scaffold

The scaffold holds **configuration** — `agent.toml`, `SOUL.md`, `.mcp.json`, `.claude/`. The **work** happens in the employee's own directory (`~/.duduclaw/agents/<employee>/`), the same working directory a Solo round of that employee would have used.

This is not cosmetic. In the first live round that reached the executor stage, members ran with their scaffold as the working directory, wrote their notes there, and then the round-end teardown deleted them. The verifier looked for evidence that a file had been created, found none, and correctly rejected the round — the artefacts it was asked about pointed at directories that no longer existed.

Moving the working directory raises a second question: the Claude CLI discovers its MCP servers from `<cwd>/.mcp.json`, so a member sitting in the employee's directory would boot the duduclaw MCP server with the **employee's** identity and every tool call, every handoff and every audit row would be attributed to the parent. So identity is named explicitly instead of discovered: a Claude member is spawned with `--mcp-config <member_dir>/.mcp.json --strict-mcp-config` (the second flag is what stops the CLI from also merging the ambient config). A Codex member gets `--cd <employee_dir>` while its `-c mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID=…` override carries the member id — the two are independent by construction there. If the member's `.mcp.json` is missing, the stage is refused (`member_mcp_config_missing`) rather than dispatched with the parent's identity.

**Not yet supported for members: gemini, antigravity, grok, and the generic print-mode CLIs.** Those runtimes read their MCP registration from a file *inside* the working directory (`.gemini/settings.json`, `.grok/config.toml`, …), so moving the cwd would move the identity with it. A member on one of those runtimes keeps its scaffold as its working directory; its files still do not outlive the round. Use claude or codex for roles that write files until this is closed.

**The other consequence had to be closed rather than documented.** Sharing the employee's working directory also means sharing the `.claude/settings.json` in it, and the file-protect hook registered there names the *employee* as its caller — so the hook read a role member's file writes as the employee writing its own files, and waved them through. The member could therefore edit the employee's `agent.toml [capabilities]`: flip `os_native` / `computer_use`, widen `allowed_tools`, empty `denied_tools`, grant itself a `db_sources` entry — and the next round's subset check would honour the widened envelope. The platform's promise that a role member "can never hold a tool its employee does not" is read out of that exact file, and a possibly-third-party model could rewrite it.

`[capabilities]` is now frozen to the hook the same way the org fields are: any change to any key in the section, through Write / Edit / Bash, is refused. The comparison walks the union of both sides' keys rather than a hand-maintained list, so a capability key added in a later release is frozen the day it lands. Operators are unaffected — the dashboard, the MCP `agent_update` tool and a plain editor all write the file from outside the hook.

One consequence worth knowing: a Codex member whose cwd is overridden receives its role instructions inside the prompt (an XML-delimited `<role_system_prompt>` block) rather than through `AGENTS.md`. `codex exec` has no system-prompt flag on 0.156.1 — `AGENTS.md` at the working root is the only file channel — and writing the member's instructions into the employee's `AGENTS.md` would clobber the employee's own file and race with a sibling member. A prompt block is weaker placement than a real system prompt; it is said here rather than dropped silently.

### Artefact paths are checked against the workspace

A packet's `artifacts[].path` is now a claim about a real file in a real directory, so it is checked: a path that resolves outside the employee's workspace is **refused with an audit row** (`team_packet_artifact_refused`) rather than silently accepted. Existing paths are canonicalised (so a symlink planted inside the workspace that points out of it is caught); a path declared before the file exists falls back to a lexical check that `..` does not climb out. A path that is not a regular file, or is larger than 64 MiB, gets no receipt either — the composer stats before it reads, so a FIFO cannot park a worker and a huge file cannot be pulled into memory to be hashed.

A refusal is now **visible to the verifier**, not only to the audit trail. The rendered packet carries an `artifacts:` section, one line per declaration, each with the status the audit recorded: `exists` / `missing` / `mismatch` for a receipted path, `outside_workspace` for one that escaped the workspace, `unverified` for one deliberately not read (not a regular file, or over 64 MiB), and `id_only` for a declaration with an `artifacts.jsonl` id and no path. Until 2026-09-28 the renderer emitted no `artifacts[]` at all, so a refused path simply entered the verifier's input as an ordinary product and the containment check's promise that "the verifier sees the same thing the audit does" was not true. The human-facing settle summary renders the same section (with `unchecked` statuses — a summary does no filesystem work).

Those statuses come from the **one** verification that produced the audit rows: each declared path is stat'd, read and hashed once, when the packet is checked, and the renderer is handed the resulting verdicts as a lookup table. It performs no filesystem access of its own — the first version of this section re-ran the whole check while building the prompt, hashing every declared file a second time (up to 64 MiB each, on blocking I/O) and putting a *second* observation in front of the verifier that nothing guaranteed matched the audited one. A product the composer never verified (a member whose dispatch errored after filing, a slot left by an earlier attempt at the same round) is reported `unchecked` rather than re-read at prompt time or labelled from another packet's observation.

One honest limit remains: a declared path is a *claim*, never an exemption — an artefact naming agent machinery (`SOUL.md`, `CLAUDE.md`, anything under `state/`, `.claude/`, `logs/`, `memory/`, …) is excluded from the settle-time archive, so it can never become a downloadable 產物.

### What the verifier and the judge are actually shown

Two evidence blocks, both built from the same `tool_calls.jsonl` window, both read by the team verifier **and** by the settle path's evaluator and MAV panel — so a role's product is never judged on two different accounts of what happened.

**The window is the round's own start**, and getting that right is the whole difference between this mechanism working and running empty. It used to be `tasks.claimed_at`, which is structurally `None` for a team task — nothing ever *claims* one; the composer completes it as `team-composer` — so the verifier was handed `(無工具活動紀錄)` and no receipts at all, every round, while the settle path's window silently widened to `created_at` and let round 1's receipts vouch for round 3. The composer now stamps the round's start before its first member runs and passes that; the settle path reads `task_iterations.dispatched_at` for its own round, falling back to that round's earliest `role_turns.jsonl` timestamp. When neither exists there is **no** window: grounding degrades to a skip rather than reaching back into an earlier round. And a window that cannot be established renders as an explicit "時窗不明" line, never as the same empty block that means "no tools were used" — a verifier that cannot tell those apart will reject honest work, or accept a claim on the strength of an absence it never measured.

**`<tool_activity>`** — one line per distinct tool, `name: N ok, M err`, over the employee ∪ that round's members.

**`<artifact_receipts>`** — one line per artefact a packet declared, and the deterministic half of the evidence:

```
notes/a.md 128B sha256=3f2a… exists
notes/b.md missing
notes/c.md 44B sha256=aa11… mismatch (declared bb22…)
```

Every contained path is stat'd and hashed as soon as the packet is read. A packet that declared no hash gets the observed one written back into it; a packet whose declared hash **disagrees** with the bytes keeps its declared value and is recorded as `mismatch` with a `team_packet_artifact_mismatch` audit row — a swap must stay visible, so the packet is never quietly corrected. Only `exists` counts as confirmation: a `missing` or `mismatch` receipt is recorded as a failed observation and can never ground a claim.

This is where the research is unambiguous. VP-CONTROL (arXiv:2609.10969) measured cross-model verifier panels that share one evidence source letting 62.9% of unsafe proposals through, against 22.9% when the evidence sources are independent — 40.9 pp of the effect from the evidence, 11.3 pp from model diversity. A sha256 of the bytes on disk is that independent source.

### Native tool work counts as evidence

Everything downstream of a round — the verifier's digest, the zero-LLM grounding pre-check, the judge's audit digest — reads `tool_calls.jsonl`. A member that does its work with **native** tools (a codex `shell` call, a Claude `Write`) makes no MCP call and so wrote nothing there, which is how live round 8 reached a settle that said "no tool activity exists to evidence that any of the files were created" about three files that demonstrably existed.

Each of a member's native tool events is now written as one `tool_calls.jsonl` row under that member's id, carrying the tool name, the outcome, the masked call input and the masked result text, plus `source = "native"`, `evidence_source = "native_tool_event"` and the `runtime` / `model` that produced it. A failed event carries `error_class = "native_tool_error"` — a blocked or failing call must leave a trace, same rule the MCP dispatch gate follows.

Antigravity `agy` 1.2.10 now supplies terminal tool events through its `stream-json` output. The parser ignores in-progress events and uses the final usage block for measured token counts. A sandbox-denied command has been observed in a live probe; successful Antigravity tool execution still needs a separate live validation.

Two deliberate differences from an MCP row, both tightening rather than loosening:

- **Input is always captured**, even for a read-only tool name. It is only ever used to *subtract* self-echoed spans from what counts as grounding evidence.
- **Result text is suppressed for a self-echo tool.** A tool like `team_handoff` answers with substantially the caller's own words, and a codex member reports its MCP calls as native events — so persisting that output would let a role ground its claim on its own packet summary. The call itself is still recorded; only its output is withheld.

At most 200 native events per member reach the trail. Past that the excess is counted in one honest `native_tool_events_truncated` row rather than silently dropped.

### Members are cleaned up when the round ends, not hours later

Ordinary transient sub-agents get an hour's grace after completing and a 24-hour ceiling. A role member gets neither: the moment its round reaches a terminal state, the scaffold is gone.

That is arithmetic, not tidiness. One member per role per round, three roles, up to five rounds, up to three tasks in flight, is 45 live directories against a default ceiling of 32. Under the grace-window policy the overflow would queue and then expire, and a round would quietly run a role short for reasons nothing in the log connects to garbage collection. A member whose round dies before it can be torn down properly is swept on the next maintenance pass instead of waiting out the old window. Nothing about cleanup for non-team sub-agents changes.

Cost rows already written stay exactly as they were: they live in SQLite under the member's id and fold onto the employee at report time, so deleting the directory loses no accounting.

The same "terminal state" now also releases the round's **admission tickets**. When the ephemeral ceiling is full, a member's spawn is durably queued instead of hard-failing, and each waiting round removes its own ticket as it goes. That covered the ordinary path and nothing else: a round that ended before its waiters got there — an early refusal, a failed stage, a panic — left its tickets sitting in the shared queue, occupying `queue_max_depth` until the TTL expired. A round now holds a purge guard for its whole lifetime, so every exit path (accepted, rejected, needs_human, cancelled, failed, panicked) clears exactly that round's queued members and no sibling round's.

### A role never changes vendor mid-round

An ordinary dispatch may fail over to another runtime when the configured one cannot be reached, substituting a model the fallback actually serves. A role member does not: cross-family failover is **refused** for members. If a codex executor cannot spawn, the stage fails, `team_stage_failed` records the role, runtime, model and error, the ledger row says `outcome=failed`, and the round's existing degrade chain decides what happens next (executor replica → verifier without a repair pass → park for a human).

Live round 3 is why. Every codex spawn was dying at startup on a config bug; failover quietly substituted Claude, the work got done as Claude, and the ledger still said `runtime=codex … completed`. Silently swapping the vendor erases the executor/verifier independence the team exists for — and doing it without saying so makes every later measurement of "which model is good at what" a measurement of the wrong thing.

### Two limits that had to be told apart

Both of these are bounds; neither is a new escape hatch.

- **Rate.** Role-member spawns are counted under their own path kind (`role_team`) with their own budget (`[dispatch_guard] role_team_max_in_window`, default 60) instead of sharing the 20-per-minute budget that guards an employee's own spawns. A correctly configured three-role team makes 9 spawns a minute for one task and around 30 with three in flight — over the shared budget on arithmetic alone, which would have read as "the platform broke" rather than "a limit was reached".
- **Capacity.** When the live-scaffold ceiling (`[dispatch] ephemeral_max_active`) is already reached, a role member is now **durably queued** rather than refused outright, on the same queue and with the same depth and TTL settings as before. Queued members are scoped to the round that asked for them, so a round that ends purges its own pending members — nothing is left waiting for an answer nobody will read. A request that is *invalid* (bad runtime/model pair, a tool the employee does not hold) is still refused immediately and never queued: retrying it could not succeed.

Because that ceiling is shared with every other transient sub-agent, it needs to be at least `max_concurrent × iteration_cap × roles` before teams are turned on — 45 for today's defaults, against a default of 32. The team path checks that arithmetic and says so rather than letting it surface later as a missing verifier. See [`config/duduclaw.example.toml`](../../config/duduclaw.example.toml) for both keys.

---

## One round, three stages (WP-4)

With a frozen spec and a Team verdict, one goal round stops being a single wake-up message and becomes three stages inside the same employee.

### The spec is frozen per task

When a goal task is created, the merged `config.toml [team]` + `agent.toml [team]` is validated once and the result stored on the task itself. From then on it never changes — a role→model matrix or a bandit that re-ranks models tomorrow affects the *next* task, not one already running. Same discipline as the frozen acceptance criteria: you can always answer "what team was this task actually judged and worked by" from the task row alone.

A spec that fails validation forms **no team at all**. The refusal is recorded, nothing is stored, and the task runs as a single employee. There is no such thing as a partial team.

The freeze is set-once at the database level (`WHERE team_spec_json IS NULL`), so two creation paths racing on the same task is normal and safe. What used to be unsafe was the *loser's* view of it: the goal loop only handled the "I froze it" answer, so the round that lost the race kept reading its own stale copy of the task, found no spec, and quietly ran Solo — while every later round, reading a fresh row, ran as a Team. Same task, two execution shapes, no trace anywhere. The loser now re-reads the row and picks up the spec that actually won.

### 規劃 → 執行 → 審核

1. **規劃** — one role member receives the objective, the frozen acceptance criteria and the risk boundary, and hands back one packet per independently-completable sub-task, with cross-sub-task dependencies listed explicitly. It does not do the work itself. A planner that hands back nothing parks the task for a human rather than having a decomposition guessed out of its prose.
2. **執行** — one member per sub-task packet, up to the configured fan-out, **with no communication between them**. Each hands back its own packet: what it did, where the evidence is, what is still open. A claim with no evidence reference belongs in "open questions", not in findings.
3. **審核** — the verifier runs on its own vendor's model and sees three things: the frozen acceptance criteria, the executor packets, and the audit trail of tool calls that actually happened. It does **not** see the planner's narrative, and it does not see the executors' prose. If it fails the work, the same executor gets one repair pass scoped to the named gap — not a rewrite, and not a wider brief.

Whatever the executor produced last is then handed to the **existing** acceptance path — the two-stage evaluator, the three-aspect panel, gap fingerprinting, oscillation detection, the best-round pick. A team does not add a second verdict system; it changes who does the work, not who decides it is done.

### The grey band

When the gate lands on exactly two signals it has an honest answer available for the price of a call the task needed anyway: run 規劃 and count what it actually decomposed. Four or more genuinely independent sub-tasks forms the team; fewer and the round falls back to the ordinary single-employee dispatch, with the plan left on disk for a later round to read.

### When the budget runs short

A round costs spawns, and a task has a ceiling on them. One round is charged for `planner? + executors + verifier + repair?`: the verifier is a utility call rather than a scaffold, but it writes its own `role_turns.jsonl` row and the budget counter charges for exactly the rows that carry a member id. Planning used to omit that term, so every round quietly cost one more than it had planned for and a `max_spawns_per_task = 12` task that planned a fourth round died in the middle of it with "budget exhausted". Planning and billing now use the same arithmetic, and the clamp floor is 3 (planner + executor + verifier) rather than 2.

As the remaining budget shrinks, capabilities are surrendered in a fixed order rather than the round simply failing:

1. **合成** (utility) stops being used — the cheapest thing to lose.
2. The verifier's **repair pass** goes: a failed round goes straight to the acceptance path instead of getting a second try.
3. **Fan-out collapses to one executor.**
4. Nothing left to give ⇒ the task parks for a human as "budget exhausted", carrying the best round it managed.

Which step was taken is recorded, so a cheap round is legible as a cheap round rather than as a worse team.

### Configuration

```toml
# config.toml — global defaults
[dispatch.team_budget]
max_spawns_per_task = 12                 # 4 roles x 3 rounds; clamped to >= 3
max_turns_per_role  = 3
degrade_order = ["utility", "verifier_second_pass", "executor_replica"]
```

An unrecognised `degrade_order` entry is dropped with a warning; a list with nothing usable in it keeps the default chain (turning every overrun straight into a human escalation would be a harsher change than a typo asked for).

### Per-role attribution: `role_turns.jsonl`

Every stage appends one row to `<home>/role_turns.jsonl` — same advisory-locked append, same 0600 mode, same hash chaining and size-capped rotation as `tool_calls.jsonl`. Readers take the most recent rotated generation (`.jsonl.old`) **and** the live file: the spawn budget is counted off these rows, and reading only the live file meant a rotation in the middle of a live task silently reset its spent budget to zero. (One generation back is all there is — each rotation overwrites the previous `.old`. The two files' hash chains restart at the boundary, so this is a concatenation for reading and counting, never a claim that the chain is continuous.) A row carries `task_id`, `round`, `role`, `member_id`, the **requested** `runtime` and `request_model`, the **answering** `runtime_used` / `response_model` / `provider` plus a `failover` flag, `effort`, the packet it produced, the **evidence grade** of its observations (`full` / `mcp_only` / `none`, never conflated), how it ended, and a `config_fingerprint_hard` that changes when the role's runtime or model does.

The request/answer split exists because the row used to be able only to repeat the configuration. In live round 3 a codex executor silently failed over to Claude and the row said `runtime=codex … completed`; `runtime_used` and `response_model` are reported by the execution path itself, and `failover` is true whenever the answering runtime differs from the requested one. Rows written for a stage that never spawned leave all three absent rather than guessing.

The evidence grade is filled by the composer, never by the member: `full` when the runtime's own tool-event stream saw calls, `mcp_only` when the member has audit rows in its dispatch window, `none` when nothing was observed. A member's own claim is overwritten and the disagreement is audited (`team_packet_fidelity_corrected`) — before this every packet came back `none` because nothing filled the field at all. With fan-out, several executors share one leg, so the composer snapshots the leg byte for byte before dispatching and grades only the files that member created or rewrote; a sibling's packet is never re-graded on somebody else's observation.

This is the dimension the platform did not have: every other attribution surface keys on agent id, and "which agent" stops identifying who did what the moment one employee's task is worked by three models. `fault_side` **is** computed and written (deterministically, at row-write time, from the closed `FaultSide` enum — `unknown` whenever the evidence cannot establish a side). The fields the design lists that nothing can yet compute — trace/span ids and role Shapley values — are deliberately **absent** rather than written empty.

Usage fields are omitted entirely when a runtime reported none. A `0` would claim "measured, and it was free".

Usage is the **stage** total, not the last leg's. A stage can answer over several legs — an openai-compat tool loop, or a failover chain whose primary attempt burned tokens before the fallback answered — and until 2026-09-28 the row published only the last one while `cost_telemetry` recorded every call, so the two ledgers disagreed about the same stage. Each leg's numbers are now summed (saturating), a dimension only one leg measured keeps that leg's value rather than being zeroed by a leg that never reported it, and `usage_legs` says how many legs contributed. `usage_legs > 1` is the signal that this stage was not one call; its absence means no leg reported usage at all (which is also the shape of every row written before the field existed).

### What the operator sees

Nothing new in the conversation: the employee still answers with one voice, progress still arrives on the same board, and `needs_human` still carries one of the six pause classes. A team round shows up as ordinary goal-loop progress; the role detail lives in the audit log and `role_turns.jsonl` until the dashboard team card ships.

---

## What the live rounds changed

Eight rounds were run against a real goal on a second gateway instance with a temporary home (planner Claude Sonnet 4.6, executor Codex gpt-5.6-sol on Codex CLI 0.156.1, verifier Claude Sonnet 4.6). Each round got further and each one paid for itself:

| Round | Reached | What it found |
|---|---|---|
| 1 | planner ran, zero packets | Members had no `.mcp.json`, so `team_handoff` did not exist for them. |
| 2 | planner called `team_handoff` 13 times | The packet schema was unguessable from the tool description; the identity reader looked in the wrong directory for an ephemeral member. |
| 3 | full 規劃 → 執行 → 審核, judge rejected | Four defects, below. |
| 4 | codex members still would not start | `codex exec` refuses a working directory that is not a trusted git repo without `--skip-git-repo-check`, and waits on an open stdin. |
| 5 | codex member started, saw the tools, could do nothing | Two refusals, below. |
| 6–7 | six codex approval variants probed directly | Only `--approve-for-me` lets an MCP call through, and it is mutually exclusive with `--sandbox <MODE>`. |
| 8 | full pipeline on real backends, settle rejected | Two defects, below. |

Round 3's rejection was **correct** — the verifier and the settle path genuinely could not see any tool activity supporting the work. Four things were wrong:

1. **Every codex spawn was dying before it started.** `codex exec -c key=value` parses the value half as TOML; the port in the member's env block was emitted unquoted, codex read `18999` as an integer where its `mcp_servers.<id>.env` table demands a string, and the spawn exited 1. A pre-existing codex-runtime bug — it broke ordinary codex agents and the codex acceptance judge the same way. Every `-c` scalar is now a quoted TOML string.
2. **Members silently changed vendor.** See [a role never changes vendor mid-round](#a-role-never-changes-vendor-mid-round).
3. **Members worked in a directory that was about to be deleted.** See [a member runs in the employee's workspace](#a-member-runs-in-the-employees-workspace-not-in-its-own-scaffold).
4. **Nobody filled a packet's evidence grade,** so every packet read `none`. See [per-role attribution](#per-role-attribution-role_turnsjsonl).

Round 5 got the member running with the duduclaw tools in its tool list — `mcp__duduclaw__team_handoff` and `working_state_handoff` were both there — and then it could not use any of them. Two refusals, one per enforcement layer:

1. **Every MCP call came back `MCP tool call requires approval, but approval policy is never`.** That is codex being fail-closed rather than broken: a non-interactive `codex exec` has nobody to answer an approval prompt, so a tool needing one can only be denied. The per-server escape hatch is `mcp_servers.duduclaw.default_tools_approval_mode = "auto"`, now passed on the same `-c` channel as the rest of the registration. It scopes to DuDuClaw's own server only — codex's shell and patch tools keep whatever `--sandbox` allows, and any other MCP server the operator registered keeps its own default. The real authorization for a duduclaw tool lives in the MCP server (per-tool scopes, the agent's capability gate, the audit log), not in an approval prompt nobody is watching.
2. **The workspace was read-only** (`mkdir` and `apply_patch` both refused), because the executor's tool list carried no write-class tool and codex derives its single sandbox mode from that list. See [what tools each role gets](#what-tools-each-role-gets).

Round 8 finally ran the whole pipeline on real backends: planner on Claude, executor on **real codex gpt-5.6-sol** (`runtime_used = codex`, fidelity `full`) creating `notes/a.md`, `b.md` and `index.md` in the employee's workspace, packets to the verifier, verifier on Claude, settle. It was still rejected, and again for reasons that were true at the time:

1. **The evidence never got written down.** The executor's work was done with native shell and file tools. Those events were counted (they are where `fidelity: full` came from) but lived only in a task-local list that died with the member's dispatch — and every downstream reader looks at `tool_calls.jsonl`. So the round that most needed to be believed was structurally the least believable. Fixed two ways: [native tool work counts as evidence](#native-tool-work-counts-as-evidence), and [artifact receipts](#what-the-verifier-and-the-judge-are-actually-shown) check the declared paths against the filesystem instead of taking the packet's word for them.
2. **The codex judge could not produce a parseable verdict.** With `[dispatch] judge_provider = "codex"` both adjudication stages answered in prose, and both parsers refused it fail-closed ("evaluator reply has no string `decision` field", "面板回覆無法解析"). Both were right to — auto-accepting garbage is the one thing a judge parser must never do — but a judge that structurally cannot be parsed is a seam that does not work. `codex exec --output-schema <FILE>` fixes it at the source: each stage now publishes the JSON schema its own parser requires (the evaluator's `decision` enum, the panel's aspect objects), derived from the parser's contract so the two cannot drift. Runtimes without a structured-output flag log the request and ignore it — a schema is a preference, never a precondition, and an adjudication must not fail because a backend cannot constrain its output.

A further thing was structural rather than a bug: both the verifier and the acceptance path read tool evidence **scoped to one agent id**. A team's work is done by ephemeral members under their own ids, so the employee's audit window was empty and "no tool activity" was the honest reading of what they were shown. The evidence set for a team round is now the employee ∪ that round's members, on both paths; a non-team task adds no ids and sees byte-identical evidence.

Rounds 9–13 exposed path interpretation and stale second-instance state during settle. The fixes are recorded in the [design's live-test log](../../commercial/docs/DESIGN-team-as-agent-2026-09.md#43-p1-活體驗證紀錄). Round 14 passed the existing acceptance judge in one team round; it remains a single integration result, not a comparative quality measurement.

## Which model for which role (P2)

Roles can each run on a different vendor's model — so *which* model, for *which*
role? P2 answers it with a measurement rather than a leaderboard:
`duduclaw eval --matrix`. Full operator reference:
[guides/evals.md → Capability matrix](../guides/evals.md#capability-matrix---matrix).

### What gets measured

One cell per `(domain, role, runtime, model)`, where a domain is an eval-suite
directory:

- **執行 (executor)** — run the suite's cases live on that `(runtime, model)` and
  score the deterministic `[expect]` assertions. No LLM judge is involved.
- **審核 (verifier)** — show the model a case's already-recorded transcript plus
  the acceptance criteria, ask for `PASS`/`FAIL`, and score the verdict against
  the assertion outcome on that same transcript. The cell reports agreement plus
  **false-accept** and **false-reject** rates with Wilson intervals, and counts
  replies whose first line carries neither verdict as *unparseable* — kept out of
  every rate, because "cannot answer in the required shape" is a different finding
  from "judges badly".
- **規劃 (planner)** — `--team-2x2` runs four matched production-composer
  rounds per case on isolated copies of the eval home. A case must declare
  `[case] team_acceptance`; the fixed independent verifier's PASS is 1 and FAIL
  is 0. An unavailable verdict is unscored, and only complete four-arm rows
  contribute to the Shapley result and planner cells. Ordinary `--roles planner`
  remains refused because a single planner reply is not a scored team outcome.

Verifier cells legitimately replay a recorded transcript: the worker is not what
is being tested — the model asked to judge it is, and that one is always called
live. Comparing *executors* through frozen transcripts would be the Replay Gap,
which is why `--matrix --replay` is refused outright.

### Which role to spend on

Give `--weak` and `--strong` and each role gets Δ = score(strong) − score(weak) on
the same cases (paired per case, cluster-robust SEs). The bigger Δ is the role
where model spend buys the most — but it is only **named** the bottleneck when its
confidence interval excludes every other role's; otherwise the answer is
`unresolved`, which is a real answer.

This is the **decoupled** form of AgentCARD's Shapley probe (arXiv:2606.20629):
each role is measured on its own, with no joint team run. That is what makes it
affordable — 2 roles × 2 models instead of |models|^|roles| team configurations —
and equally what it cannot see. A genuine interaction (a strong verifier that only
pays off behind a weak executor) is invisible by construction. Read it as "which
role to spend on first", never as a team-level attribution.

### What it writes, and what still reads nothing

`--report` writes the JSON report plus **`role_model_matrix.toml`** beside it: one
`[header]` (declared MDE, α, power, K, cluster key, `planner = "deferred"`) and one
`[[cell]]` per measured cell, with its `n`, mean, interval, achieved MDE and
three-state `verdict`. A statistic that could not be computed is an absent key,
never a fabricated number; a cell with zero usable observations gets a report row
but no matrix cell; and the file is validated on both write and read, so a
hand-edited duplicate cell or unknown runtime id is refused rather than believed.

**The composer reads it as a prior.** Put `role_model_matrix.toml` in
`<DUDUCLAW_HOME>` and a role that left its `model` unset takes the matrix's
winner instead of falling straight through to the employee's
`[model] preferred`. Five conditions, every one of them narrowing:

- an **explicitly configured** `[team.roles.*] model` always wins — a
  configuration is a decision, a matrix is a measurement;
- **`unresolved` cells are ignored entirely.** A cell whose interval cannot
  resolve the declared MDE is not a ranking input, and a high-scoring
  unresolved cell never outranks a resolved one;
- **only cells on the role's own runtime count**, so the prior can never
  rewrite the `(role, runtime, model)` triple the spec was frozen as;
- the winner must keep the role's **model family**, or the
  executor ≠ verifier invariant would break silently under it;
- **a tie is not a winner**, and a runtime whose CLI or credentials are not
  present on this host is skipped.

Anything unclear falls back to the pre-matrix cascade, so a deployment with no
matrix file behaves byte-identically. The composer has no domain of its own — a
goal task is not an eval-suite directory — so cells for the same
`(role, runtime, model)` across domains are aggregated by an `n`-weighted mean.

The online bandit that would keep the file fresh is still P5, and the gate's
`capability_gap` signal still has no data source (see below).

### What the first smoke run found

The first probe run (2026-09-25, `hr-recruit`, 4 cases, executor + verifier over
haiku and codex) produced three findings and all three are now handled:

| Finding | Handling |
|---|---|
| Executor cells errored 12/12: the probe home had only one provisioned agent, not the suite's `hr-recruit` | `--agent <id>` runs every case under one provisioned agent. A matrix measures the model, not the persona, so that is an acceptable probe compromise — declared as `agent_override` in the report header and per run, never inferred |
| Codex verifier cells were 4/4 `unparseable`: codex does not reliably lead its reply with a bare `PASS`/`FAIL` token | Every verifier call now requests `{"verdict":"PASS"\|"FAIL","reasons":[...]}` through the existing `--output-schema` plumbing (codex-only, as before), and the parser accepts that JSON object **or** the prose first-token form, still failing closed to `unparseable` |
| Gold was FAIL for every case (the recorded transcripts are stale), so a verifier that always says FAIL scored agreement 1.00 | Each verifier cell reports `gold_pass` / `gold_fail`, and a single-class gold sets `degenerate_gold: true`, forces `verdict: "unresolved"` (reason `degenerate_gold`) and drops that role out of the bottleneck comparison |

### What smoke 3 found

The third probe run (real `hr-recruit` persona, 4 re-recorded cases with a mixed
2P/2F gold, executor + verifier over haiku / codex / sonnet) produced **real
scores** — and two bugs that would have made those scores lies:

| Finding | Handling |
|---|---|
| Every cell had one cluster (one suite directory), where the cluster-robust SE is *identically zero*. CIs collapsed to points (`mean 0.25 ci=[0.25,0.25]`), both Δ intervals were zero-width, and the bottleneck was declared **resolved** on four cases | Fewer than two clusters now reports the unclustered CLT SE and names it (`se_source: clt_single_cluster`), for cells and for the paired Δ alike. The same defect was swept on the ordinary eval path's suite-level row. A genuinely zero-width interval (n=1, or all observations identical) forces `unresolved` with `degenerate_interval`, and the bottleneck refuses to resolve on one |
| Codex verifier replies were `unparseable` because the text being parsed was `{"type":"turn.completed","usage":{…}}` — the stream's last event, not the agent's message | The verdict is now read from the agent's **message**, recovered from a raw event stream when a runtime hands one back (codex `agent_message`/`message` items, claude `assistant`/`result` events). Fail-open: a real answer passes through byte-identical. Runs where recovery fired are flagged `message_recovered_from_stream` |

The second one's root cause is **upstream and still open**:
`runtime/codex.rs::parse_codex_stdout` recognises only the `item.type ==
"message"` + `content[].type == "output_text"` shape, so a CLI emitting the
`agent_message` shape yields empty content and `CodexRuntime::execute` falls back
to `stdout.lines().last()`. Every caller of that runtime inherits it; the eval
path is now immune, but the gateway fix belongs to a wave that may edit
`duduclaw-gateway`.

**P2 debt this leaves open.** The shipped premium suites' recorded transcripts are
stale against their current assertions (the P0 live test measured 98/360 on replay,
unchanged against the previous release's binary), so **every** verifier cell over
them is `degenerate_gold`. They must be re-recorded, or their assertions fixed,
before a verifier matrix means anything. Executor cells are unaffected — they run
live and never read the recorded transcript.

### Honest limits of a small matrix

A matrix declares its own resolution. At the smoke-run size (6 cases, `K=1`) the
achieved MDE is tens of percentage points, so nearly every cell comes back
`unresolved` — enough to tell a Haiku tier from an Opus tier, nowhere near enough
to rank two adjacent models. The declared MDE is printed in the console summary
and stored in the file's header precisely so nobody later quotes a ranking the
sample size cannot support (research 13 §4.2: 40 cases at K=3 resolves ~21pp;
200 cases resolve ~9pp).

---

## What is still missing

Honest list, so nothing here reads as more finished than it is:

- **Accepted round is a single smoke result.** Round 14 passed the existing acceptance judge, but that does not establish a production win over Solo. The role×model matrix's four-case smoke cannot resolve the declared MDE.
- **Roles that write files are limited to claude and codex.** gemini / antigravity / grok / generic-CLI members keep their scaffold as their working directory, so their files do not outlive the round.
- The role×model capability matrix is now read by the composer as a prior (above), but **only where a role left its model unset**, and only from `unresolved`-free cells. Since 2026-09-29 the same file also answers the gate's capability-gap signal — the `n`-weighted distance, in percentage points, between the matrix's winning model for the executor's `(role, runtime)` and the model that role runs today, paired with the matrix header's own declared MDE so a gap below the noise floor cannot fire. Both sides of that comparison need a resolved cell, so **on every shipped matrix the signal is still dark**: all four cells are `unresolved`, which leaves the gate exactly where it was (two measurable signals of four, and three are needed for Team). It becomes live the first time an operator measures a matrix that resolves, and not before. The online bandit that would keep the file fresh is still P5. The P2b full-team producer completed a three-case, 12-arm live pipeline probe with real verifier verdicts and handoffs. All four model cells remain `unresolved`; three synthetic cases from one domain do not satisfy the formal matrix acceptance threshold — which in practice means today's shipped matrices select nothing at all.
- **The default-on flip has not been live-tested as a default.** The behaviour it produces on an unconfigured install is covered by unit tests (quiet Solo, no audit row, byte-identical dispatch); what has not been run is a fleet that had `[team.roles]` configured *and* was relying on `enabled` being off to stay Solo. Such a deployment now forms teams — `enabled = false` is the one-line revert.
- **Verifier cells only measure anything on a re-recorded suite.** The shipped premium suites' transcripts give a single-class gold, so cells over them are honestly `degenerate_gold`; smoke 3 showed that re-recording four cases with a mixed gold produces real scores, so the fix is known — it just has not been applied to the 18 shipped suites.
- **A one-directory suite cannot use the cluster-robust SE at all**, so its intervals come from the weaker CLT estimator and cannot see within-directory correlation. Real resolution needs the design's ≥8 directories (research 13 §4.3), not just more cases.
- The gateway extracts codex `agent_message` items, and the team verifier now asks codex for the same strict PASS/FAIL JSON schema as the offline verifier cell. A live team round with codex as verifier remains to be checked.
- **Current-model prices still need operator entries.** The matrix loads `<DUDUCLAW_HOME>/models.toml` before dispatch and refuses `--budget-usd` when any model has no price. Its estimate still cannot cap a provider bill exactly; large tool runs may cost more than the pre-run reserve.
- The gate's own calibration (Brier scoring of its decisions against outcomes) is designed but not wired.
- Fan-out spreads *different sub-tasks* across members on one executor model. Per-replica model diversity is in the design but is not expressible in the `[team.roles.executor]` shape that shipped.
- **A packet's `tool_scope` and `irreversible` are carried and read by nothing.** Both are validated and stored, neither reaches a spawn, and `render_packet_for_prompt` does not even render them — so a planner writing `tool_scope.denied = ["mail_send"]` gets no effect at all. A role member's real tool envelope comes from the employee's `[capabilities]` via `check_tool_subset`, and ActionGuard guards irreversible calls at the point of the call regardless of the flag. The type docs and [`spec/task-packet.md`](../spec/task-packet.md) used to claim otherwise ("a denial here is a real denial"); they now say "not wired". Wiring them is future work.
- The L0 "irreversible action in the plan" exclusion is not evaluated by the gate; ActionGuard still guards irreversible calls at the point of the call, unchanged by teams.
- ~~**The never-trim exemption is keyed on four literal markdown headings, not on the composer.**~~ Closed: the exemption is bound to the composer by a per-process sentinel marker line, and the two ceilings stay as defence in depth. See "Two seams worth knowing about".
- **`role_turns.jsonl` counts a verifier row against the spawn budget that `plan_round` did not plan for.** The verifier is a utility call, but its ledger row carries `member_id = "team-verifier"`, and `spawns_used_for_task` counts every row with one — so each round costs one spawn more than the plan projected.
- `cost.by_role` now reports measured team-stage spend by role and model, optionally narrowed by task id (`episode_id`). Old rows without a role remain unattributed. The dashboard team configuration card and packet/tool drill-down remain to be wired.
- Verifier token usage is captured when its runtime supplies a usage block; otherwise its `usage_*` fields are absent rather than zero. The same rule applies to planner and executor usage. The live probe's role cost ledger is incomplete and cannot serve as a provider bill.
- **Structured judge output is codex-only.** `--output-schema` is the one structured-output flag wired; a gemini or openai-compat judge still has to be persuaded by the prompt. The team verifier now shares the offline verifier schema and accepts strict JSON as well as leading-token prose; codex team verification still needs a live round.
- **Artifact receipts only cover paths a packet declares.** A file a member wrote without listing it in `artifacts[]` is evidenced by its native tool row but gets no hash. Sweeping the workspace for undeclared changes is future work.
- The detached composer now waits on a FIFO admission ticket when the live-scaffold ceiling is reached. Expired, missing, or unreadable tickets fail the stage explicitly; a cancelled wait removes its own ticket. Queue-state and scaffold-release tests pass; a loaded live-round test is still needed.

---

## References

- Design: `commercial/docs/DESIGN-team-as-agent-2026-09.md` (§1, §3.1–§3.6, §3.8–§3.11)
- Packet reference: [spec/task-packet.md](../spec/task-packet.md)
- Related: [13-multi-runtime.md](13-multi-runtime.md), [goal-loop guide](../guides/goal-loop.md)
