# Native Linux MCP resource routes (security revision, 2026-11-05)

**PARTIAL: full product parity is not ready.** Employee permissions remain the product contract; missing adapters are startup failures, not newly acceptable product restrictions. The endpoint now invokes `require_complete_services()` before admitting MCP traffic. The inventory tests use explicit discovery doubles for missing connectors and do not prove provider execution or production boot readiness.

**CONTRACT CHANGE: capability canonical encoding v2.** `EmployeeScope { id }` is an explicit MAC-covered resource variant (tag 9), bound to the verified employee. Service/Connector calls use either exact resource grants or current live employee authority. A scope cannot be combined with task/table/wiki/mailbox/recipient/connector grants or a task-limited run. Workspace and URL-origin entries may accompany scope as explicit File/A/E inputs. Tag 10, `Confined { resource }`, carries other exact File/A/E input/output grants (for example a wiki namespace or a worker connector account); it requires EmployeeScope and forbids nesting. Neither form narrows or expands employee-scope Service/Connector authority. Older capability MACs are not reused.

Every Service/Connector tool enters `BoundaryRoute::Resource`. Exact grants are resolved and checked before the legacy handler. A capability for task-a refuses task-b. Employee-scope calls retain the original live employee gates. Explicit `tasks_list`, `activity_list`, `list_cron_tasks`, `goals_list`, `list_reminders` and `responsibility_get` use finite queries/readers with immutable IDs and employee ownership. Finite `db_query` accepts a strict single-table SELECT subset lowered into the parameterized SelectRequest builder; joins, functions, subqueries and other unsupported grammar refuse. EmployeeScope retains its existing product SQL policy.

A/Execution requires a backend that explicitly supports retained workspace authority. `SpawndBackend::with_workspace_bridge` installs the trusted root-receipt hookup and verifies the held directory, exact reservation/runtime/workspace/cwd and live authority. The production B/H receipt producer is still external; default connect refuses workspace plans. Worker/file plans retain authorized tokens through queued work and checked transport. These are implementation readiness requirements, not Linux acceptance.

Every connector requires an installed per-tool live selection policy, active catalog account, binding verification and bound executor. Executor source coverage is 62/62: native Google/Notion/GitHub (28), Odoo (18), DB (4), channels (8), models (2), computer login (1), search (1). The typed boot registry requires complete selection/execution support before dispatcher construction; the production integration host has not adopted that entry point. Google uses its sealed gateway owner for personal/home OAuth and consent/refresh; actual requester and non-OAuth variants remain pending. Credentials are not copied into the run. Source coverage does not establish complete product/provider parity. See the [round 3 report](native-linux-isolation-security-fixes-2026-11-05.md).

Existing scope, allowed/denied tools, permissions, approvals, record/audience, egress and input guards remain in force. Legacy Service I/O checks after every await are not yet fully instrumented. The typed handler carries authority through awaits; core TaskStore mutations recheck after locks and before writes/commits. Child tasks need explicit scope propagation. The entry check and revocation watch alone do not close that review item. The complete status and verification boundaries are in [the security-fix report](native-linux-isolation-security-fixes-2026-11-05.md).

| Refusal category | Reason |
| --- | --- |
| Existing Admin/operator gate | The original employee policy decides access. |
| HostAuthority | A run cannot control or observe the host desktop, devices, power, network configuration or local model services. |
| Missing adapter | Boot fails; this is unfinished parity, not an intentional tool restriction. |

| Tool | Disposition |
| --- | --- |
| `google_status` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gmail_search` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gmail_read` | Resource → Native account-bound executor; product-policy/boot adapter pending |
| `gmail_create_draft` | Resource → Native account-bound executor; product-policy/boot adapter pending |
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
| `create_agent` | Resource → AuthorizedToolCall → original live employee handler |
| `list_agents` | Resource → AuthorizedToolCall → original live employee handler |
| `create_task` | A ExecutionBackend + ExecutionPlanFactory |
| `check_responses` | Resource → AuthorizedToolCall → original live employee handler |
| `task_status` | Resource → AuthorizedToolCall → original live employee handler |
| `agent_status` | Resource → AuthorizedToolCall → original live employee handler |
| `spawn_agent` | A ExecutionBackend + ExecutionPlanFactory |
| `spawn_ephemeral` | A ExecutionBackend + ExecutionPlanFactory |
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
| `skill_extract` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `execute_program` | A ExecutionBackend + ExecutionPlanFactory |
| `office_script` | A ExecutionBackend + ExecutionPlanFactory |
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
| `fork_run` | A ExecutionBackend + ExecutionPlanFactory |
| `inspect_branches` | A ExecutionBackend + ExecutionPlanFactory |
| `diff_branches` | A ExecutionBackend + ExecutionPlanFactory |
| `merge_or_select` | A ExecutionBackend + ExecutionPlanFactory |
| `terminate_branch` | A ExecutionBackend + ExecutionPlanFactory |
| `fork_cost` | A ExecutionBackend + ExecutionPlanFactory |
| `credential_declare` | Resource → AuthorizedToolCall → original live employee handler |
| `discovery_catalog` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_list` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_tree` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_artifact` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_cancel` | A ExecutionBackend + ExecutionPlanFactory |
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
| `shared_skill_adopt` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `plan_start` | Resource → AuthorizedToolCall → original live employee handler |
| `memory_improve` | Resource → AuthorizedToolCall → original live employee handler |
| `browser_record_start` | A ExecutionBackend + ExecutionPlanFactory |
| `browser_record_stop` | A ExecutionBackend + ExecutionPlanFactory |
| `desktop_record_start` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `desktop_record_stop` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `skill_from_recording` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
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
| `code_map` | A ExecutionBackend + ExecutionPlanFactory |
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
| `skill_security_scan` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_graduate` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_synthesis_status` | Resource → AuthorizedToolCall → original live employee handler |
| `skill_synthesis_run` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_hub_install` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_curator_status` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
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
| `meeting_transcribe` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_upload_link` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_transcribe_status` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_transcribe_cancel` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `synthesize_speech` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `knowledge_search` | Resource → AuthorizedToolCall → original live employee handler |
| `knowledge_get_passage` | Resource → AuthorizedToolCall → original live employee handler |
| `knowledge_list_collections` | Resource → AuthorizedToolCall → original live employee handler |
| `knowledge_add_source` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
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
| `file_read` | AuthorizedFile; live authority on each read |
| `csv_read` | AuthorizedFile → checked E table parser |
| `xlsx_read` | AuthorizedFile → checked E table parser |
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
| `web_fetch_cached` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `web_extract` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `inference_status` | Resource → AuthorizedToolCall → original live employee handler |
| `model_list` | Resource → AuthorizedToolCall → original live employee handler |
| `model_load` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `model_unload` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `hardware_info` | A ExecutionBackend + ExecutionPlanFactory |
| `route_query` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `inference_mode` | Resource → AuthorizedToolCall → original live employee handler |
| `llamafile_start` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `llamafile_stop` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `llamafile_list` | Resource → AuthorizedToolCall → original live employee handler |
| `model_search` | Resource → public HF checked search + boot hardware snapshot; no ambient token/probe/cache |
| `model_download` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `model_recommend` | Resource → selected model registry → boot hardware snapshot + curated model rules |
| `submit_feedback` | Resource → AuthorizedToolCall → original live employee handler |
| `evolution_toggle` | Resource → AuthorizedToolCall → original live employee handler |
| `evolution_status` | Resource → AuthorizedToolCall → original live employee handler |
| `audit_trail_query` | Resource → AuthorizedToolCall → original live employee handler |
| `reliability_summary` | Resource → AuthorizedToolCall → original live employee handler |
| `channel_config` | Resource → selected exact settings scope → validated key/value and checked transaction |
| `channel_config_list` | Resource → selected exact settings scope → bounded settings read |
| `channel_status` | Resource → selected exact channel/scope → scoped status/session read |
| `pairing_manage` | Resource → selected exact subject → checked pairing state; finite list |
