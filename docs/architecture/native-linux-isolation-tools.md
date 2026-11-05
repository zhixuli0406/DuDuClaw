# Package D round 5 tool dispositions

Isolation changes where employee code runs and where credentials are held. It does not change employee permissions. All gateway handler calls use the employee verified by `RequestContext`, the original dispatcher and the original handler: scopes, allowed/denied tools, permissions, action rules, approvals, record authorization, egress/private-data and input guards stay in force. Credentials remain inside the gateway.

The capability authorizes the call; cron, reminders, tasks, mail and knowledge remain employee records. Their existing ownership/delegation/audience policies decide access, including subsequent runs after the creating capability expires. The gateway schedules those executions through `ExecutionBackend`, just as it schedules other employee work. Computer tools operate gateway-owned containers, and durable workspaces retain their existing lease/revision protocol.

| Refusal category | Reason |
| --- | --- |
| Existing Admin/operator gate | A caller without the authority required by the original scope/permission/handler gate is refused there; isolation adds no operator-only tool list. |
| HostAuthority | A run cannot control or observe the host desktop, devices, power, network configuration or local model services. |

`Refused: HostAuthority` rows below are the complete isolation exception set. Every other row is served subject to the same original employee gates, even when its scope is Admin. Missing A/E factories are wiring errors, not acceptable product restrictions: `require_complete_services()` rejects incomplete boot assembly. The matrix and tests describe implementation, not live provider or native acceptance.

| Tool | Disposition |
| --- | --- |
| `google_status` | Original gateway handler; existing employee gates |
| `gmail_search` | Original gateway handler; existing employee gates |
| `gmail_read` | Original gateway handler; existing employee gates |
| `gmail_create_draft` | Original gateway handler; existing employee gates |
| `calendar_list_events` | Original gateway handler; existing employee gates |
| `calendar_create_event` | Original gateway handler; existing employee gates |
| `sheets_read` | Original gateway handler; existing employee gates |
| `sheets_append` | Original gateway handler; existing employee gates |
| `forms_get` | Original gateway handler; existing employee gates |
| `forms_list_responses` | Original gateway handler; existing employee gates |
| `gtasks_lists` | Original gateway handler; existing employee gates |
| `gtasks_list` | Original gateway handler; existing employee gates |
| `gtasks_create` | Original gateway handler; existing employee gates |
| `gtasks_complete` | Original gateway handler; existing employee gates |
| `drive_search` | Original gateway handler; existing employee gates |
| `drive_read` | Original gateway handler; existing employee gates |
| `docs_read` | Original gateway handler; existing employee gates |
| `docs_append` | Original gateway handler; existing employee gates |
| `slides_read` | Original gateway handler; existing employee gates |
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
| `create_agent` | Original gateway handler; existing employee gates |
| `list_agents` | Original gateway handler; existing employee gates |
| `create_task` | A ExecutionBackend + ExecutionPlanFactory |
| `check_responses` | Original gateway handler; existing employee gates |
| `task_status` | Original gateway handler; existing employee gates |
| `agent_status` | Original gateway handler; existing employee gates |
| `spawn_agent` | A ExecutionBackend + ExecutionPlanFactory |
| `spawn_ephemeral` | A ExecutionBackend + ExecutionPlanFactory |
| `agent_update` | Original gateway handler; existing employee gates |
| `agent_remove` | Original gateway handler; existing employee gates |
| `agent_update_soul` | Original gateway handler; existing employee gates |
| `odoo_connect` | Original gateway handler; existing employee gates |
| `odoo_status` | Original gateway handler; existing employee gates |
| `odoo_crm_leads` | Original gateway handler; existing employee gates |
| `odoo_crm_create_lead` | Original gateway handler; existing employee gates |
| `odoo_crm_update_stage` | Original gateway handler; existing employee gates |
| `odoo_sale_orders` | Original gateway handler; existing employee gates |
| `odoo_sale_create_quotation` | Original gateway handler; existing employee gates |
| `odoo_sale_confirm` | Original gateway handler; existing employee gates |
| `odoo_inventory_products` | Original gateway handler; existing employee gates |
| `odoo_inventory_check` | Original gateway handler; existing employee gates |
| `odoo_invoice_list` | Original gateway handler; existing employee gates |
| `odoo_payment_status` | Original gateway handler; existing employee gates |
| `odoo_search` | Original gateway handler; existing employee gates |
| `odoo_execute` | Original gateway handler; existing employee gates |
| `odoo_report` | Original gateway handler; existing employee gates |
| `odoo_partner_search` | Original gateway handler; existing employee gates |
| `odoo_schema_overview` | Original gateway handler; existing employee gates |
| `odoo_schema_fields` | Original gateway handler; existing employee gates |
| `skill_extract` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `execute_program` | A ExecutionBackend + ExecutionPlanFactory |
| `office_script` | A ExecutionBackend + ExecutionPlanFactory |
| `skill_bank_feedback` | Original gateway handler; existing employee gates |
| `computer_screenshot` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_click` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_type` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_login` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_key` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_scroll` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_session_start` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_navigate` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_session_stop` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_workspace_list` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_workspace_read` | Original gateway container/workspace handler; existing session and lease gates |
| `computer_workspace_write` | Original gateway container/workspace handler; existing session and lease gates |
| `session_restore_context` | Original gateway handler; existing employee gates |
| `fork_run` | A ExecutionBackend + ExecutionPlanFactory |
| `inspect_branches` | A ExecutionBackend + ExecutionPlanFactory |
| `diff_branches` | A ExecutionBackend + ExecutionPlanFactory |
| `merge_or_select` | A ExecutionBackend + ExecutionPlanFactory |
| `terminate_branch` | A ExecutionBackend + ExecutionPlanFactory |
| `fork_cost` | A ExecutionBackend + ExecutionPlanFactory |
| `credential_declare` | Original gateway handler; existing employee gates |
| `discovery_catalog` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_list` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_tree` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_artifact` | A ExecutionBackend + ExecutionPlanFactory |
| `discovery_cancel` | A ExecutionBackend + ExecutionPlanFactory |
| `tasks_list` | Original gateway handler; existing employee gates |
| `tasks_create` | Original gateway handler; existing employee gates |
| `tasks_update` | Original gateway handler; existing employee gates |
| `tasks_claim` | Original gateway handler; existing employee gates |
| `tasks_renew` | Original gateway handler; existing employee gates |
| `tasks_complete` | Original gateway handler; existing employee gates |
| `responsibility_get` | Original gateway handler; existing employee gates |
| `responsibility_followup` | Original gateway handler; existing employee gates |
| `responsibility_ask` | Original gateway handler; existing employee gates |
| `tasks_block` | Original gateway handler; existing employee gates |
| `capability_request` | Original gateway handler; existing employee gates |
| `goals_create` | Original gateway handler; existing employee gates |
| `goals_list` | Original gateway handler; existing employee gates |
| `plan_get` | Original gateway handler; existing employee gates |
| `plan_update_step` | Original gateway handler; existing employee gates |
| `activity_post` | Original gateway handler; existing employee gates |
| `activity_list` | Original gateway handler; existing employee gates |
| `autopilot_list` | Original gateway handler; existing employee gates |
| `shared_skill_list` | Original gateway handler; existing employee gates |
| `shared_skill_share` | Original gateway handler; existing employee gates |
| `shared_skill_adopt` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `plan_start` | Original gateway handler; existing employee gates |
| `memory_improve` | Original gateway handler; existing employee gates |
| `browser_record_start` | A ExecutionBackend + ExecutionPlanFactory |
| `browser_record_stop` | A ExecutionBackend + ExecutionPlanFactory |
| `desktop_record_start` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `desktop_record_stop` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `skill_from_recording` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `memory_search` | Original gateway handler; existing employee gates |
| `memory_store` | Original gateway handler; existing employee gates |
| `working_state_set` | Original gateway handler; existing employee gates |
| `working_state_clear` | Original gateway handler; existing employee gates |
| `working_state_handoff` | Original gateway handler; existing employee gates |
| `working_state_get` | Original gateway handler; existing employee gates |
| `team_handoff` | Original gateway handler; existing employee gates |
| `belief_submit` | Original gateway handler; existing employee gates |
| `belief_settle` | Original gateway handler; existing employee gates |
| `belief_stats` | Original gateway handler; existing employee gates |
| `mail_list` | Original gateway handler; existing employee gates |
| `mail_read` | Original gateway handler; existing employee gates |
| `mail_send` | Original gateway handler; existing employee gates |
| `codrive_run` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `codrive_status` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `memory_read` | Original gateway handler; existing employee gates |
| `memory_fetch_batch` | Original gateway handler; existing employee gates |
| `memory_alias_add` | Original gateway handler; existing employee gates |
| `memory_alias_list` | Original gateway handler; existing employee gates |
| `memory_get_history` | Original gateway handler; existing employee gates |
| `memory_get_at` | Original gateway handler; existing employee gates |
| `memory_invalidate_by_origin` | Original gateway handler; existing employee gates |
| `memory_search_by_layer` | Original gateway handler; existing employee gates |
| `code_map` | A ExecutionBackend + ExecutionPlanFactory |
| `user_profile_record` | Original gateway handler; existing employee gates |
| `user_profile_get` | Original gateway handler; existing employee gates |
| `user_code_profile` | Original gateway handler; existing employee gates |
| `memory_successful_conversations` | Original gateway handler; existing employee gates |
| `memory_episodic_pressure` | Original gateway handler; existing employee gates |
| `memory_consolidation_status` | Original gateway handler; existing employee gates |
| `decision_list` | Original gateway handler; existing employee gates |
| `decision_resolve` | Original gateway handler; existing employee gates |
| `skill_search` | Original gateway handler; existing employee gates |
| `skill_list` | Original gateway handler; existing employee gates |
| `skill_gaps` | Original gateway handler; existing employee gates |
| `skill_security_scan` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_graduate` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_synthesis_status` | Original gateway handler; existing employee gates |
| `skill_synthesis_run` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_hub_install` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_curator_status` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `skill_pin` | Original gateway handler; existing employee gates |
| `notion_status` | Original gateway handler; existing employee gates |
| `notion_search` | Original gateway handler; existing employee gates |
| `notion_page_read` | Original gateway handler; existing employee gates |
| `notion_page_append` | Original gateway handler; existing employee gates |
| `github_status` | Original gateway handler; existing employee gates |
| `github_search_issues` | Original gateway handler; existing employee gates |
| `github_issue_read` | Original gateway handler; existing employee gates |
| `github_pr_read` | Original gateway handler; existing employee gates |
| `github_issue_comment` | Original gateway handler; existing employee gates |
| `cost_summary` | Original gateway handler; existing employee gates |
| `cost_agents` | Original gateway handler; existing employee gates |
| `cost_users` | Original gateway handler; existing employee gates |
| `cost_recent` | Original gateway handler; existing employee gates |
| `cost_multi_vs_single` | Original gateway handler; existing employee gates |
| `transcribe_audio` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_transcribe` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_upload_link` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_transcribe_status` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `meeting_transcribe_cancel` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `synthesize_speech` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `knowledge_search` | Original gateway handler; existing employee gates |
| `knowledge_get_passage` | Original gateway handler; existing employee gates |
| `knowledge_list_collections` | Original gateway handler; existing employee gates |
| `knowledge_add_source` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `wiki_ls` | Original gateway handler; existing employee gates |
| `wiki_read` | Original gateway handler; existing employee gates |
| `wiki_write` | Original gateway handler; existing employee gates |
| `wiki_search` | Original gateway handler; existing employee gates |
| `wiki_lint` | Original gateway handler; existing employee gates |
| `wiki_stats` | Original gateway handler; existing employee gates |
| `wiki_export` | Original gateway handler; existing employee gates |
| `wiki_dedup` | Original gateway handler; existing employee gates |
| `wiki_graph` | Original gateway handler; existing employee gates |
| `wiki_rebuild_fts` | Original gateway handler; existing employee gates |
| `wiki_trust_audit` | Original gateway handler; existing employee gates |
| `wiki_trust_history` | Original gateway handler; existing employee gates |
| `shared_wiki_delete` | Original gateway handler; existing employee gates |
| `wiki_namespace_status` | Original gateway handler; existing employee gates |
| `canvas_push` | Original gateway handler; existing employee gates |
| `canvas_clear` | Original gateway handler; existing employee gates |
| `identity_resolve` | Original gateway handler; existing employee gates |
| `wiki_share` | Original gateway handler; existing employee gates |
| `db_sources` | Original gateway handler; existing employee gates |
| `db_tables` | Original gateway handler; existing employee gates |
| `db_select` | Original gateway handler; existing employee gates |
| `db_query` | Original gateway handler; existing employee gates |
| `file_read` | FD read |
| `csv_read` | E table parser |
| `xlsx_read` | E table parser |
| `group_history_read` | Original gateway handler; existing employee gates |
| `send_message` | Original gateway handler; existing employee gates |
| `send_photo` | Original gateway handler; existing employee gates |
| `send_sticker` | Original gateway handler; existing employee gates |
| `web_search` | Original gateway handler; existing employee gates |
| `send_to_agent` | Original gateway handler; existing employee gates |
| `list_cron_tasks` | Original gateway handler; existing employee gates |
| `update_cron_task` | Original gateway handler; existing employee gates |
| `delete_cron_task` | Original gateway handler; existing employee gates |
| `pause_cron_task` | Original gateway handler; existing employee gates |
| `run_cron_task` | Original gateway handler; existing employee gates |
| `create_reminder` | Original gateway handler; existing employee gates |
| `list_reminders` | Original gateway handler; existing employee gates |
| `cancel_reminder` | Original gateway handler; existing employee gates |
| `web_fetch_cached` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `web_extract` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `inference_status` | Original gateway handler; existing employee gates |
| `model_list` | Original gateway handler; existing employee gates |
| `model_load` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `model_unload` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `hardware_info` | A ExecutionBackend + ExecutionPlanFactory |
| `route_query` | E WorkerPlanFactory + WorkerLauncher/WorkerSession |
| `inference_mode` | Original gateway handler; existing employee gates |
| `llamafile_start` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `llamafile_stop` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `llamafile_list` | Original gateway handler; existing employee gates |
| `model_search` | A ExecutionBackend + ExecutionPlanFactory |
| `model_download` | Refused: HostAuthority — controls or observes host desktop, devices or system services. |
| `model_recommend` | A ExecutionBackend + ExecutionPlanFactory |
| `submit_feedback` | Original gateway handler; existing employee gates |
| `evolution_toggle` | Original gateway handler; existing employee gates |
| `evolution_status` | Original gateway handler; existing employee gates |
| `audit_trail_query` | Original gateway handler; existing employee gates |
| `reliability_summary` | Original gateway handler; existing employee gates |
| `channel_config` | Original gateway handler; existing employee gates |
| `channel_config_list` | Original gateway handler; existing employee gates |
| `channel_status` | Original gateway handler; existing employee gates |
| `pairing_manage` | Original gateway handler; existing employee gates |
