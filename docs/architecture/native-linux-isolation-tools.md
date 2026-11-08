# Native Linux MCP resource routes (security revision, 2026-11-05)
# Native Linux tool dispositions — I1b round 6 (v1.80.0)

**PARTIAL: full product parity is not ready.** Employee permissions remain the product contract; missing adapters are startup failures, not newly acceptable product restrictions. The endpoint now invokes `require_complete_services()` before admitting MCP traffic. The inventory tests use explicit discovery doubles for missing connectors and do not prove provider execution or production boot readiness.

**CONTRACT CHANGE: capability canonical encoding v2.** `EmployeeScope { id }` is an explicit MAC-covered resource variant (tag 9), bound to the verified employee. Service/Connector calls use either exact resource grants or current live employee authority. A scope cannot be combined with task/table/wiki/mailbox/recipient/connector grants or a task-limited run. Workspace and URL-origin entries may accompany scope as explicit File/A/E inputs. Tag 10, `Confined { resource }`, carries other exact File/A/E input/output grants (for example a wiki namespace or a worker connector account); it requires EmployeeScope and forbids nesting. Neither form narrows or expands employee-scope Service/Connector authority. Older capability MACs are not reused.

The production human/background issuer now emits canonical EmployeeScope and
Confined mailbox/table/wiki grants from its own host roots. Task-limited
background work removes EmployeeScope (and any PublicOrigins or NewTask grant)
and unwraps those exact grants. Inventory tests consume this issuer output.

v1.81 Package H (H9 selectors in `isolation_mcp/selectors.rs`, H10 issuer hook in
`isolation_mcp/boot.rs`) adds four issuer-selected, MAC-covered grants, each read
at issuance from its authoritative store and fail-closed when that store is
unsafe or unreadable, then re-read after the issuer's lifecycle/users.db
rechecks (a change refuses the run):

- `Workspace { employee-work }` with the brokered root (H6) for `file_read`,
  `csv_read` and `xlsx_read`: only when boot installed a root work broker, the
  run's tool snapshot lists a file tool and the employee's current `agent.toml`
  allowed/denied tools still allow it. W1: production boot installs run-bound
  brokers (`isolation_mcp/run_broker.rs`) over the protected backend, which has
  spawnd's checked readiness report as its feature source; "installed" then
  also requires that report to advertise `work_broker` at issuance. Each
  capability gets its own broker (H5 `SpawndWorkspaceBridge` underneath), bound
  to the employee id and incarnation the issuer pinned and, at its first open,
  to that run and its control connection; every open needs exact equality of
  all of them plus the `work_broker` bit in that run's own `Reserved.features`.
  `BootSources.work_broker` remains an optional host override.
  HX5: the same grant is also selected for the in-run helper tools
  `execute_program`, `code_map` and `office_script` (an employee without file
  tools still sees and can call them): only when a root work broker is
  installed, spawnd's readiness view advertises `helper_calls` at issuance
  (`SpawndBackend::features`), the snapshot lists a helper tool and the profile
  still allows it. No broker, no `helper_calls` bit or an unreadable view
  withholds it (fail closed); losing the bit during issuance refuses the run.
  Each call still requires the run's own `Reserved.features` bit.
- `PublicOrigins { * }` (H7) for `web_fetch_cached` and `web_extract`: only for
  an employee-scope run and only when the admin set `agent.toml [isolation]
  public_web = true` for that employee (owner decision 2026-11-08: its own
  admin-only switch, default `false` = no web authority; `[container]
  network_access` keeps controlling only the task sandbox). `[isolation]` is
  frozen against the employee's own writes and refused by the dashboard
  key/value editor; it is changed in the configuration file (or the admin raw
  configuration editor). Never derived from tool arguments.
- `NewTask { requester }` (CONTRACT CHANGE, capability tag 12, H9 owner
  decision 3) for `create_task` in an interactive (chat) run: only when the run
  is not record-backed, lists `create_task` and the profile still allows it.
  `requester` is the run's authenticated human; the capability decoder and
  signer refuse the grant unless the record principal is `Human` with that
  user id, the record has its own EmployeeScope and no task, and it is a plain
  (not Confined) single grant, so a Background principal never holds it. A
  Personal employee's requester is its owner (B1 at issuance; the resolver
  rechecks it). At call time the boundary accepts only `goal` and `steps`
  (every step with the existing step keys), so no parent task, goal,
  dependency, assignee or creator can be selected; the gateway resolver
  requires every step to target this employee (or none) and returns exactly
  the own Employee grant plus the signed NewTask grant; the handler files the
  TaskSpec as this employee on behalf of that requester.
- the record `Task` grant (H8) for `create_task`: only for a task/schedule run
  whose record task is assigned to the employee in `tasks.db` at issuance.
  Owner decision 2026-11-08: a run whose task was reassigned or deleted is
  refused outright, both before any approval card (`isolation_background`
  `scope_record`, reason shown to the scheduler) and in the issuer (`Revoked`);
  each refusal writes an `isolation_task_run_refused` security-audit row
  (`reason: task_not_owned`, `stage: schedule` or `issuance`).

Workspace and URL-origin selection for the other File/A/E tools is still
missing, including browser recording (`browser_record_start` /
`browser_record_stop`, task HX6). `employee-work` is also the A execution
workspace handle, so when the issuer selects it (for the file tools or the HX5
helpers) it leaves every other listed tool whose eligibility would rest on that
grant out of the signed tool snapshot (`Selection::signed_tools`, `mcp-boundary`
`workspace_selection::drops_from_signed`): every tool in
`workspace_selection::PENDING_WORKSPACE_SELECTOR_TOOLS`, plus any other
non-File tool that needs a Workspace. The in-run helpers are signed only when
the grant was selected for them. Those dropped tools stay hidden and refused.
The parity test tracks this gap explicitly, using that same list; it is
unfinished wiring, not another intentional isolation refusal or evidence of
full employee parity. Since W1 production boot installs the work broker and the
spawnd feature source, so the file tools and the HX5 in-run helpers can be
listed in production; they stay hidden (fail closed) while spawnd's readiness
report lacks `work_broker` (helpers: also `helper_calls`) or the backend has no
feature source, and the readiness report then names
`workspace_broker_unavailable`, `helper_launcher_unavailable{reason}` and
`helper_runtime_missing{runtime}`. The own-skill tools below (H12a) never receive a Workspace (or
Recipient) grant from these selectors.

H12a (v1.81): the own-skill tools `skill_extract`, `shared_skill_adopt`,
`skill_security_scan`, `skill_graduate` and `skill_curator_status` no longer
require a workspace grant. They are eligible only under EmployeeScope with the
record's own `Employee` grant (plus exactly one own `agent:` Confined wiki for
`skill_extract`), take no subject selector (a legacy `agent_id` must equal the
bound employee; `target_agent` refuses), parse skill bytes only in the E worker,
and publish only into the caller's own wiki or own `SKILLS/`. Global graduation
needs the dashboard admin card; the curator (D-H4) reports and runs only over the
caller's own skills.

H13 (v1.81): `spawn_agent` and `spawn_ephemeral` require no workspace grant and no
new capability grant. They are eligible with the record's own `Employee` grant
(a task/schedule run also needs the signed `Task` grant of its record task) and
accept only their exact schemas (`agent_id`/`task`/`session_key`;
`instruction`/`context`/`tools`/`tier`); delegation metadata and identity keys
refuse. Every call re-reads the record task's live ownership (a reassigned or
deleted task refuses and writes `isolation_task_run_refused`). `spawn_agent`:
the target must pass the live delegation policy and be a confirmed, enabled
protected employee: a Shared target in the source's tenant, or (H13b, owner
decision 2026-11-09) a Personal target whose lifecycle owner is exactly the
owner of a Personal source whose run that owner started — it then runs as the
target's background work without a card. Another owner's Personal employee,
and any Personal target of a Shared source, is refused; a Personal source's
delegation to a Shared target still waits for its owner's approval card
(`isolation_personal_delegation`).
`spawn_ephemeral` (D-H3): never scaffolds a new agent in protected mode; the
child is a pending job (`isolation-ephemeral-jobs.db`) under the parent run's
own employee id, incarnation and binding, with the requested tools intersected
with the parent run's signed tool snapshot; a Personal parent spawns only when
its lifecycle owner personally started the parent run (fresh lifecycle and
users.db read when the child's receipt is captured). D8: the job is never
awaited. H13b: the dispatcher tick releases it once the parent run's
capability was revoked in this gateway epoch and no other run of the employee
holds one (an unknown run waits and expires after 24 hours) and the employee is
still the same id and incarnation, then starts it as a self-delegation of the
parent employee (the dispatcher's only self-delegation exception); the child
holds only the job's tools, uses the parent's model routing (`tier` is
recorded only) and cannot spawn another child.

Service/Connector tools enter `BoundaryRoute::Resource` unless an explicit host handler is selected by `boundary_route` (including the Linux bundled model catalog). Exact grants are resolved and checked before the legacy handler. A capability for task-a refuses task-b. Employee-scope calls retain the original live employee gates. Explicit `tasks_list`, `activity_list`, `list_cron_tasks`, `goals_list`, `list_reminders` and `responsibility_get` use finite queries/readers with immutable IDs and employee ownership. Finite `db_query` accepts a strict single-table SELECT subset lowered into the parameterized SelectRequest builder; joins, functions, subqueries and other unsupported grammar refuse. EmployeeScope retains its existing product SQL policy.

A/Execution requires a backend that explicitly supports retained workspace authority. `SpawndBackend::with_workspace_bridge` installs the trusted root-receipt hookup and verifies the held directory, exact reservation/runtime/workspace/cwd and live authority. The production B/H receipt producer is still external; default connect refuses workspace plans. Worker/file plans retain authorized tokens through queued work and checked transport. These are implementation readiness requirements, not Linux acceptance.

Calls routed to a connector require an installed per-tool live selection policy, active catalog account, binding verification and bound executor. Executor source coverage is 66/66: native Google/Notion/GitHub (30), Odoo (20, including RM-120 `odoo_create` / `odoo_write`), DB (4), channels (8), models (2), computer login (1), search (1). The typed boot registry requires complete selection/execution support before dispatcher construction; the production integration host has not adopted that entry point. Google uses its sealed gateway owner for personal/home OAuth and consent/refresh; actual requester and non-OAuth variants remain pending. Credentials are not copied into the run. Source coverage does not establish complete product/provider parity. See the [round 3 report](native-linux-isolation-security-fixes-2026-11-05.md).

RM-103 S3 holds in protected mode: Codex, Grok and registered generic CLIs check `CapabilitiesConfig::native_tool_constraints` before the isolated route (`runtime::protected_coarse_native_gate`, repeated in `isolation_mcp::runtime::execute_native` / `execute_registered_cli` before the one-shot authority is consumed). The isolated native profile is root-derived and has no per-run read-only mode, so a native shell **or** write constraint (including the fail-closed config of a malformed `[capabilities]`) refuses the spawn. Protected Claude runs with `--tools ""` (no native tools).

Existing scope, allowed/denied tools, permissions, approvals, record/audience, egress and input guards remain in force. Legacy Service I/O checks after every await are not yet fully instrumented. The typed handler carries authority through awaits; core TaskStore mutations recheck after locks and before writes/commits. Child tasks need explicit scope propagation. The entry check and revocation watch alone do not close that review item. The complete status and verification boundaries are in [the security-fix report](native-linux-isolation-security-fixes-2026-11-05.md).

| Refusal category | Reason |
| --- | --- |
| Existing Admin/operator gate | The original employee policy decides access. |
| HostAuthority | A run cannot control or observe the host desktop, devices, power, network configuration or local model services. |
| Missing adapter | Boot fails; this is unfinished parity, not an intentional tool restriction. |
| ProposalRequired | v1.81 protected mode: `create_agent` is refused; a protected employee exists only through an approved Employee proposal and its root allocation. |
| ForkNextRelease | v1.80.0 protected TEAM only: `fork_run`, `merge_or_select`, `terminate_branch` are deferred until fresh child capability/account/budget authority and immutable result apply are complete. Non-protected installations retain their existing behavior. |

`Refused: HostAuthority`, `Refused: ProposalRequired` and `Refused: ForkNextRelease` rows below are the complete isolation exception set. Every other row is served subject to the same original employee gates, even when its scope is Admin. The three deferred fork tools remain in this inventory, are hidden from protected tools/list, and return a localized refusal on tools/call; an injected service cannot enable them. Apart from that explicit v1.80.0 deferral, missing A/E factories are wiring errors: `require_complete_services()` rejects incomplete boot assembly. The matrix and tests describe implementation, not live provider or native acceptance.

A readiness excludes exactly the three deferred mutations and requires all five signed helper/control producers plus the three host fork readers. An absent or changed signed helper publication still blocks readiness. This does not enable the other composite components, claim native/provider acceptance, or remove any of the original 22 A tools from the inventory.

Operator/user messages follow `[general] default_language`:

- zh-TW: 受保護模式下的分支試算將於下一版開放。
- en: Branch trials in protected mode will be available in the next release.
- ja: 保護モードでの分岐試行は次のバージョンで利用可能になります。

The matrix is checked for unique tool names, complete policy inventory, and exact refusal categories by `python3 scripts/ci/check_isolation_tool_matrix.py`.

| Tool | Disposition |
| --- | --- |
| `google_status` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gmail_search` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gmail_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gmail_create_draft` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `google_chat_spaces` | Resource → Native account-bound executor (mailbox grant, admin Chat opt-in, Direct OAuth only); product-policy/boot adapter pending |
| `google_chat_messages` | Resource → Native account-bound executor (mailbox grant, admin Chat opt-in, Direct OAuth only); product-policy/boot adapter pending |
| `calendar_list_events` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `calendar_create_event` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `sheets_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `sheets_append` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `forms_get` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `forms_list_responses` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gtasks_lists` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gtasks_list` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gtasks_create` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gtasks_complete` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `drive_search` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `drive_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `docs_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `docs_append` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `slides_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `os_notify` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_watch_status` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_open` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_frontmost` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_spotlight_search` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_calendar_today` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_device_status` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_system_status` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_check_update` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_backup_list` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_network_info` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_wifi_status` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_wifi_scan` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_wifi_connect` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_apply_update` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_boot_assessment` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_update_rollback` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_backup_create` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_power` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_factory_reset` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_doctor_repair` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_display_get` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_display_set` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_audio_get` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `os_audio_set` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `create_agent` | Refused: ProposalRequired — protected employees are created only through an approved Employee proposal. |
| `list_agents` | Resource → AuthorizedToolCall → original live employee handler |
| `create_task` | Host authenticated target queue admission; fresh target run authority required; each call is authorized against the capability before the TaskSpec is written; a task/schedule run plans under its record Task grant (H8 selector, issued only while the employee owns the task, otherwise the run is refused); an interactive run uses the NewTask grant (tag 12) for new top-level plans only, assigned to the calling employee and filed for the signed human requester |
| `check_responses` | Resource → AuthorizedToolCall → original live employee handler |
| `task_status` | Resource → AuthorizedToolCall → original live employee handler |
| `agent_status` | Resource → AuthorizedToolCall → original live employee handler |
| `spawn_agent` | Host authenticated target queue admission; fresh target run authority required; H13: each call is authorized against the own Employee grant (and the live record task) before any queue write; the target must pass the live delegation policy and be a confirmed Shared employee of the same tenant, or (H13b) a Personal employee of the same owner as a Personal source started by that owner (other owners' Personal targets refused); a Personal source waits for its owner's delegation card for a Shared target |
| `spawn_ephemeral` | Host pending job under the parent run's own employee id, incarnation and binding (D-H3), never a new scaffold; tools = requested ∩ parent run snapshot; Personal parent only when its owner started the run; refused for a foreign record task; released only after the parent run exits (D8), then started by the dispatcher as a self-delegation of the same employee id + incarnation with only the job's tools (H13b) |
| `agent_update` | Resource → AuthorizedToolCall → original live employee handler |
| `agent_remove` | Resource → AuthorizedToolCall → original live employee handler |
| `agent_update_soul` | Resource → AuthorizedToolCall → original live employee handler |
| `odoo_connect` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_status` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_crm_leads` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_crm_create_lead` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_crm_update_stage` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_sale_orders` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_sale_create_quotation` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_sale_confirm` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_inventory_products` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_inventory_check` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_invoice_list` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_payment_status` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_search` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_execute` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_report` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_partner_search` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_schema_overview` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_schema_fields` | Resource → bound Odoo provider; original approval/business gates; live RPC checks; production boot pending |
| `odoo_create` | Resource → bound Odoo provider; RM-120 gates on the bound connector (explicit grant + per-table generic opt-in, never-writable list, module/block list, x2many commands, related-model authorization, account ACL and field checks); live RPC checks; production boot pending |
| `odoo_write` | Resource → bound Odoo provider; RM-120 gates on the bound connector (explicit grant + per-table generic opt-in, never-writable list, module/block list, x2many commands, related-model authorization, account ACL and field checks); live RPC checks; production boot pending |
| `skill_extract` | E WorkerPlanFactory + WorkerLauncher/WorkerSession; EmployeeScope + own Employee + exactly one own Wiki output grant (Confined Wiki), rechecked at publication; own SKILLS bytes parsed by E as data; publishes only into the caller's own wiki, body pages read back before the completion marker (H12a) |
| `execute_program` | HX5 `Route::InRunHelper`: in-run helper in the caller's own run (`ExecutionToolBridge::execute_in_run`, `ExecutionPlanFactory::prepare_in_run`, spawnd `HelperCall`); synchronous result; transport run == target run == `record.run`; needs `Workspace{employee-work}` (H9 selects it for the helpers, no file tool needed) and spawnd `helper_calls`; signed only when that selection ran |
| `office_script` | HX5 `Route::InRunHelper`: in-run helper in the caller's own run (`ExecutionToolBridge::execute_in_run`, `ExecutionPlanFactory::prepare_in_run`, spawnd `HelperCall`); synchronous result; transport run == target run == `record.run`; needs `Workspace{employee-work}` (H9 selects it for the helpers, no file tool needed) and spawnd `helper_calls`; signed only when that selection ran |
| `skill_bank_feedback` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_screenshot` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_click` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_type` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_login` | Resource → selected exact credential metadata/label → original computer session/origin/threat gates |
| `computer_key` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_scroll` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_session_start` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_navigate` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_session_stop` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_workspace_list` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_workspace_read` | Resource → AuthorizedToolCall → original live employee handler |
| `computer_workspace_write` | Resource → AuthorizedToolCall → original live employee handler |
| `session_restore_context` | Resource → AuthorizedToolCall → original live employee handler |
| `fork_run` | Refused: ForkNextRelease — protected TEAM v1.80.0 limitation; unchanged on non-protected installations. |
| `inspect_branches` | Host fork store; exact actor/generation and workspace grants; no legacy-row import. |
| `diff_branches` | Host recorded-manifest comparison; exact actor/grants; not live or immutable byte verification. |
| `merge_or_select` | Refused: ForkNextRelease — protected TEAM v1.80.0 limitation; unchanged on non-protected installations. |
| `terminate_branch` | Refused: ForkNextRelease — protected TEAM v1.80.0 limitation; unchanged on non-protected installations. |
| `fork_cost` | Host owner-checked query; measurement_status=unavailable and null spend until measured usage is implemented; never fabricated zero. |
| `credential_declare` | Resource → AuthorizedToolCall → original live employee handler |
| `discovery_catalog` | Host discovery catalog handler; original employee gates |
| `discovery_list` | Host discovery list handler; original employee gates |
| `discovery_tree` | Host discovery tree handler; original employee gates |
| `discovery_artifact` | Host bounded discovery artifact handler on Linux; checked recorded-artifact access |
| `discovery_cancel` | Host discovery job control; original employee gates |
| `tasks_list` | Resource → finite SQL adapter for explicit grants; original live handler for EmployeeScope |
| `tasks_create` | Resource → AuthorizedToolCall → original live employee handler |
| `tasks_update` | Resource → AuthorizedToolCall → original live employee handler |
| `tasks_claim` | Resource → AuthorizedToolCall → original live employee handler |
| `tasks_renew` | Resource → AuthorizedToolCall → original live employee handler |
| `tasks_complete` | Resource → AuthorizedToolCall → original live employee handler |
| `responsibility_get` | Resource → EmployeeScope original handler / finite owner + responsibility IDs |
| `responsibility_followup` | Resource → AuthorizedToolCall → original live employee handler |
| `responsibility_ask` | Resource → AuthorizedToolCall → original live employee handler |
| `tasks_block` | Resource → AuthorizedToolCall → original live employee handler |
| `capability_request` | Resource → AuthorizedToolCall → original live employee handler |
| `goals_create` | Resource → AuthorizedToolCall → original live employee handler |
| `goals_list` | Resource → EmployeeScope original handler / finite signed task IDs + employee SQL |
| `plan_get` | Resource → AuthorizedToolCall → original live employee handler |
| `plan_update_step` | Resource → AuthorizedToolCall → original live employee handler |
| `activity_post` | Resource → AuthorizedToolCall → original live employee handler |
| `activity_list` | Resource → finite SQL adapter for explicit grants; original live handler for EmployeeScope |
| `autopilot_list` | Resource → AuthorizedToolCall → original live employee handler |
| `shared_skill_list` | Resource → AuthorizedToolCall → original live employee handler |
| `shared_skill_share` | Resource → AuthorizedToolCall → original live employee handler |
| `shared_skill_adopt` | E WorkerPlanFactory + WorkerLauncher/WorkerSession; EmployeeScope + own Employee; shared skill scanned and prepared by E as data; scanned body copied only into the caller's own SKILLS (no clobber, read back); no target argument (H12a) |
| `plan_start` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_improve` | Resource → AuthorizedToolCall → original live employee handler |
| `browser_record_start` | A ExecutionBackend + ExecutionPlanFactory; protected route pending (HX6): never signed with `employee-work` (`PENDING_WORKSPACE_SELECTOR_TOOLS`) |
| `browser_record_stop` | A ExecutionBackend + ExecutionPlanFactory; protected route pending (HX6): never signed with `employee-work` (`PENDING_WORKSPACE_SELECTOR_TOOLS`) |
| `desktop_record_start` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `desktop_record_stop` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `skill_from_recording` | Resource → original authenticated host orchestration; checked E parser required |
| `memory_search` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_store` | Resource → AuthorizedToolCall → original live employee handler |
| `working_state_set` | Resource → AuthorizedToolCall → original live employee handler |
| `working_state_clear` | Resource → AuthorizedToolCall → original live employee handler |
| `working_state_handoff` | Resource → AuthorizedToolCall → original live employee handler |
| `working_state_get` | Resource → AuthorizedToolCall → original live employee handler |
| `team_handoff` | Resource → AuthorizedToolCall → original live employee handler |
| `belief_submit` | Resource → AuthorizedToolCall → original live employee handler |
| `belief_settle` | Resource → AuthorizedToolCall → original live employee handler |
| `belief_stats` | Resource → AuthorizedToolCall → original live employee handler |
| `mail_list` | Resource → AuthorizedToolCall → original live employee handler |
| `mail_read` | Resource → AuthorizedToolCall → original live employee handler |
| `mail_send` | Resource → AuthorizedToolCall → original live employee handler |
| `codrive_run` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `codrive_status` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `memory_read` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_fetch_batch` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_alias_add` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_alias_list` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_get_history` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_get_at` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_invalidate_by_origin` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_search_by_layer` | Resource → AuthorizedToolCall → original live employee handler |
| `code_map` | HX5 `Route::InRunHelper`: in-run helper in the caller's own run (`ExecutionToolBridge::execute_in_run`, `ExecutionPlanFactory::prepare_in_run`, spawnd `HelperCall`); synchronous result; transport run == target run == `record.run`; needs `Workspace{employee-work}` (H9 selects it for the helpers, no file tool needed) and spawnd `helper_calls`; signed only when that selection ran |
| `user_profile_record` | Resource → AuthorizedToolCall → original live employee handler |
| `user_profile_get` | Resource → AuthorizedToolCall → original live employee handler |
| `user_code_profile` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_successful_conversations` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_episodic_pressure` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_consolidation_status` | Resource → AuthorizedToolCall → original live employee handler |
| `decision_list` | Resource → AuthorizedToolCall → original live employee handler |
| `decision_resolve` | Resource → AuthorizedToolCall → original live employee handler |
| `skill_search` | Resource → AuthorizedToolCall → original live employee handler |
| `skill_list` | Resource → AuthorizedToolCall → original live employee handler |
| `skill_gaps` | Resource → AuthorizedToolCall → original live employee handler |
| `skill_security_scan` | E WorkerPlanFactory + WorkerLauncher/WorkerSession; EmployeeScope + own Employee; own (else global) skill bytes scanned by E as data with own CONTRACT patterns; report only, findings rendered as one-line data (H12a) |
| `skill_graduate` | E WorkerPlanFactory + WorkerLauncher/WorkerSession; EmployeeScope + own Employee; own skill scanned and prepared by E; global publication needs the dashboard admin card bound to the scanned source, then the journaled atomic publication (H12a) |
| `skill_synthesis_status` | Resource → AuthorizedToolCall → original live employee handler |
| `skill_synthesis_run` | Resource → original authenticated host orchestration; checked E parser required |
| `skill_hub_install` | Resource → original authenticated host orchestration; checked E parser required |
| `skill_curator_status` | Resource → original authenticated host orchestration; checked E parser required; EmployeeScope + own Employee; status and `run=true` limited to the caller's own skills (D-H4), run admin-approved, report returned and not published to the shared wiki (H12a) |
| `skill_pin` | Resource → AuthorizedToolCall → original live employee handler |
| `notion_status` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `notion_search` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `notion_page_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `notion_page_append` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `github_status` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `github_search_issues` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `github_issue_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `github_pr_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `github_issue_comment` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `cost_summary` | Resource → AuthorizedToolCall → original live employee handler |
| `cost_agents` | Resource → AuthorizedToolCall → original live employee handler |
| `cost_users` | Resource → AuthorizedToolCall → original live employee handler |
| `cost_recent` | Resource → AuthorizedToolCall → original live employee handler |
| `cost_multi_vs_single` | Resource → AuthorizedToolCall → original live employee handler |
| `transcribe_audio` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_transcribe` | Resource → original authenticated host orchestration; checked E parser required |
| `meeting_upload_link` | Resource → original authenticated meeting job control; no media parsing |
| `meeting_transcribe_status` | Resource → original authenticated meeting job control; no media parsing |
| `meeting_transcribe_cancel` | Resource → original authenticated meeting job control; no media parsing |
| `synthesize_speech` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `knowledge_search` | Resource → AuthorizedToolCall → original live employee handler |
| `knowledge_get_passage` | Resource → AuthorizedToolCall → original live employee handler |
| `knowledge_list_collections` | Resource → AuthorizedToolCall → original live employee handler |
| `knowledge_add_source` | Resource → AuthorizedToolCall → original live employee proposal handler; own Wiki grant required (Confined Wiki under EmployeeScope); text/URL only, local paths refused |
| `wiki_ls` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_read` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_write` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_search` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_lint` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_stats` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_export` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_dedup` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_graph` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_rebuild_fts` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_trust_audit` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_trust_history` | Resource → AuthorizedToolCall → original live employee handler |
| `shared_wiki_delete` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_namespace_status` | Resource → AuthorizedToolCall → original live employee handler |
| `canvas_push` | Resource → AuthorizedToolCall → original live employee handler |
| `canvas_clear` | Resource → AuthorizedToolCall → original live employee handler |
| `identity_resolve` | Resource → AuthorizedToolCall → original live employee handler |
| `wiki_share` | Resource → AuthorizedToolCall → original live employee handler |
| `db_sources` | Resource → bounded account collection; each member repeats owner/catalog/binding/product checks |
| `db_tables` | Resource → bound DB provider; live source/table policy and post-pool I/O checks; production boot pending |
| `db_select` | Resource → bound DB provider; live source/table policy and post-pool I/O checks; production boot pending |
| `db_query` | Resource → selected DB; finite strict single-table SQL lowers to parameterized SelectRequest; EmployeeScope retains product SQL policy |
| `file_read` | AuthorizedFile (workspace dirfd, or `employee-work` root broker sealed read-only snapshot, never reopened by path); 512 KiB cap; live authority on each read; root refusals show fixed zh-TW reasons without paths; H10 issues Workspace employee-work (brokered root, H6) when a root work broker is installed and the employee profile allows the tool |
| `csv_read` | AuthorizedFile (dirfd or `employee-work` root broker sealed snapshot) → checked E table parser, ReadTable op; root refusals show fixed zh-TW reasons without paths; same H10 brokered Workspace grant as file_read |
| `xlsx_read` | AuthorizedFile (dirfd or `employee-work` root broker sealed snapshot) → checked E table parser, ReadTable op; root refusals show fixed zh-TW reasons without paths; same H10 brokered Workspace grant as file_read |
| `group_history_read` | Resource → selected exact channel/group → original live retained-history gates |
| `send_message` | Resource → selected exact bot/channel/chat → checked sender; late failure is uncertain |
| `send_photo` | Resource → selected exact channel/chat → checked media send; unsupported subtype/input refuses |
| `send_sticker` | Resource → selected exact channel/chat → checked sticker send; unsupported subtype/input refuses |
| `web_search` | Resource → bound search provider; checked E parser required; production boot pending |
| `send_to_agent` | Resource → AuthorizedToolCall → original live employee handler |
| `list_cron_tasks` | Resource → finite owner-and-grant SQL adapter; original live handler for EmployeeScope |
| `update_cron_task` | Resource → AuthorizedToolCall → original live employee handler |
| `delete_cron_task` | Resource → AuthorizedToolCall → original live employee handler |
| `pause_cron_task` | Resource → AuthorizedToolCall → original live employee handler |
| `run_cron_task` | Resource → AuthorizedToolCall → original live employee handler |
| `create_reminder` | Resource → AuthorizedToolCall → original live employee handler |
| `list_reminders` | Resource → EmployeeScope original handler / finite immutable reminder IDs + bounded reader |
| `cancel_reminder` | Resource → AuthorizedToolCall → original live employee handler |
| `web_fetch_cached` | Resource → original authenticated cache/fetch handler; existing egress and cache policy; H10 issues PublicOrigins * (H7) only to an employee-scope run of an employee whose admin set agent.toml isolation public_web = true (default none; independent of the task sandbox network_access) |
| `web_extract` | E WorkerPlanFactory + WorkerLauncher/WorkerSession; same H10 PublicOrigins grant as web_fetch_cached |
| `inference_status` | Resource → AuthorizedToolCall → original live employee handler |
| `model_list` | Resource → AuthorizedToolCall → original live employee handler |
| `model_load` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `model_unload` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `hardware_info` | Host bounded Linux RAM/CPU/PCI snapshot; no external probe or fabricated VRAM |
| `route_query` | Resource → original authenticated inference router; existing employee policy |
| `inference_mode` | Resource → AuthorizedToolCall → original live employee handler |
| `llamafile_start` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `llamafile_stop` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `llamafile_list` | Resource → AuthorizedToolCall → original live employee handler |
| `model_search` | Host bundled model catalog on Linux; live RAM filter; no provider search or ambient credentials |
| `model_download` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `model_recommend` | Host bundled model recommendations on Linux; live RAM filter; unknown GPU memory stays unknown |
| `submit_feedback` | Resource → AuthorizedToolCall → original live employee handler |
| `evolution_toggle` | Resource → AuthorizedToolCall → original live employee handler |
| `evolution_status` | Resource → AuthorizedToolCall → original live employee handler |
| `audit_trail_query` | Resource → AuthorizedToolCall → original live employee handler |
| `reliability_summary` | Resource → AuthorizedToolCall → original live employee handler |
| `channel_config` | Resource → selected exact settings scope → validated key/value and checked transaction |
| `channel_config_list` | Resource → selected exact settings scope → CONFIG_KEYS ∪ MCP_ACCESS_KEYS projection |
| `channel_status` | Resource → selected exact channel/scope → scoped status/session read; MCP_STATUS_KEYS guild projection |
| `pairing_manage` | Resource → selected exact subject → checked pairing state; finite list |
