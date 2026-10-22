# Dashboard settings map (v1.68.0)

This page lists what can be set from the dashboard as of v1.68.0: which page, which configuration key it writes, and whether the gateway needs a restart afterwards. The last section covers the Advanced config editor, which edits keys that have no dedicated control.

Other languages: [繁體中文](zh-TW/dashboard-settings.md) · [日本語](ja-JP/dashboard-settings.md)

## How saving works

- System settings (`config.toml`) are written through `system.update_config`, which is admin-only. A page sends only the fields you changed; with no change nothing is sent and the page says "Nothing changed, so there is nothing to save."
- The write locks the file and re-reads it. If another writer changed the file after you loaded it, the save is refused; reload and save again.
- When `config.toml` does not parse, `system.update_config`, the live data feed editor and the Advanced config editor refuse to write; fix the syntax in the Advanced config editor first. The other settings pages that write `config.toml` (channels, accounts, Odoo and so on) refuse in the same way.
- A save edits the file in place: only the keys you changed are rewritten. Comments, blank lines, key order and the formatting of every untouched key stay exactly as you wrote them, and a changed value keeps the comment at the end of its line. A new key goes at the end of its section and a new section at the end of the file. This holds for every settings page that writes `config.toml` or `inference.toml`, not only the Advanced config editor.
- Keys that only take effect after a restart come back in `restart_required`. The Settings and Channels pages show "These settings take effect after the gateway restarts" until the gateway has restarted (you can also dismiss it). The Inference page and the employee edit page do not show this banner.
- Each change to one of these keys also writes the audit event `config_protected_key_changed` with the old and new value: `acp.trusted`, `tick.allow_command_sources`, `container.sandbox.when_unavailable`, `container.sandbox.script_when_unavailable`, `memory.supersession_trust_guard`.

## Settings → Advanced → Automation

| Section → control | Key | Takes effect |
|---|---|---|
| Goal loop & dispatch → Dispatch policy, new option "One employee in four roles" | `[dispatch] policy = "role_team"` | Immediately (the dispatch engine restarts) |
| Goal loop & dispatch → Runtime for the acceptance judge | `[dispatch] judge_provider` | Immediately |
| Goal loop & dispatch → Model for the acceptance judge | `[dispatch] judge_model` | Immediately |
| Knowledge & memory → Night tidy-up (model stage) | `[night] llm_enabled` | Immediately |
| Human takeover (own card) | `[takeover] enabled`, `duration_minutes`, `max_duration_minutes` (the pause may not exceed the maximum) | Immediately |
| AI employee mailbox (own card) | `[mail] enabled`, `gmail_enabled`, `dropfolder_enabled`, `default_agent`, `auto_trigger` | Immediately |
| One employee, four roles (global default) | `[team] enabled`, `gate`, `[team.roles.<role>] runtime` / `model` / `effort` (the synthesizer role is stored as `utility`) | Next goal |
| Live data feeds → enable, default pace, allow command sources, address lookup cache (seconds) | `[tick] enabled`, `preset`, `allow_command_sources`, `dns_ttl_secs` | The feeds restart immediately when the gateway has the live-feed runtime loaded; otherwise after a restart |
| Live data feeds → Feeds (add, edit, remove) | `[[tick.sources]]` through `tick.sources.list` / `upsert` / `remove` (admin) | Same as above. Header values are never returned, only their count. Writing a `command` source also records `config_protected_key_changed` |

## Settings → Advanced → System

| Section → control | Key | Takes effect |
|---|---|---|
| General → Log format | `[logging] format` (`json`; any other value is plain text) | After restart |
| Display name | `[general] name` | After restart (the local-network announcement is built at boot) |
| Account health check interval | `[rotation] health_check_interval_seconds` | After restart |
| Task sandbox → sandbox image, when the sandbox is unavailable (tasks), when the sandbox is unavailable (scripts) | `[container.sandbox] image`, `when_unavailable`, `script_when_unavailable` | Immediately (read per task) |
| Task sandbox → Resource limits | `memory_bytes`, `tmp_bytes`, `workspace_bytes`, `pids`, `cpu_millis`, `max_turns`; the section is validated as a whole (`tmp_bytes + workspace_bytes` may not exceed `memory_bytes`) | Immediately |
| Task sandbox → Computer-use image | `[computer_use] image` | Immediately |
| Memory, tracing and trust → Memory trust guard | `[memory] supersession_trust_guard` | New sessions |
| Memory, tracing and trust → Send OpenTelemetry traces to | `[telemetry] otlp_endpoint`; shown only when the build includes the `otel` feature (`otel_compiled` in `system.status`) | After restart |
| Memory, tracing and trust → GitHub tools | `[integrations] github` (can also be turned off) | Immediately |
| Memory, tracing and trust → Trust outside A2A requests (admins only) | `[acp] trusted` | Immediately |
| Local file folders | `[files] allowed_roots` (full paths, not the filesystem root, at most 64) | Immediately |
| Secret Manager → Advanced: secret backend | 1Password: `onepassword_host`, `onepassword_vault`, access token; Infisical: `infisical_addr`, `infisical_project_id`, `infisical_environment`, access token. Tokens are stored encrypted as `*_enc` | No restart reported |

`[general] log_level` (Everyday → General → Log Level) applies as soon as it is saved, unless the environment variable `RUST_LOG` is set, in which case the save reports a restart. Saving the account rotation strategy or the rate-limit cooldown clears the rotation cache, so the next call uses the new value (before, the change could lag by up to 30 minutes).

## Channels

- Group "mention only" (`mention_only`, per group): since v1.76 a LINE group with it on answers only messages that call an AI employee by `@` (or ask for the `@` list) and the one message a called employee is waiting for; before, it silenced the group entirely. The shared Telegram bot also accepts employee mentions. See [Calling AI staff with `@`](../features/62-calling-ai-staff.md).
- Website chat widget: "Open the website chat widget" writes `[webchat] public_widget` and "Widget key" writes `[webchat] widget_key` (16 to 256 visible ASCII characters, with a Generate button; turning the widget on requires a key; the key is never returned). Applies immediately. The widget key is stored in plain text in `config.toml`: it is the public key that appears in the website's page source, so it is not treated as a secret.
- WhatsApp, Feishu, Google Chat, Teams, WeCom and DingTalk: the six webhook routes are always mounted. A route answers 404 until its channel is configured; after that it enforces the platform's signature check and answers 401 to a bad signature. `channels.add` starts the channel without a restart: `channels.add` returns `hot_started` and `restart_required: false`, with `not_started_reason` when credentials are incomplete. A per-employee Slack bot also starts when it is added.

## Integrations → Odoo → Website live chat

Saved with its own button through `odoo.livechat.set` (admin) into `[odoo_livechat]`, a separate section from `[odoo]` because the Odoo connection save rewrites the whole `[odoo]` section. The bot API key is stored encrypted as `bot_api_key_enc` and never returned; leaving the field empty keeps it. Every save and every **Hand back to AI** is written to the audit log. See [features/61-odoo-livechat.md](../features/61-odoo-livechat.md).

| Control | Key | Takes effect |
|---|---|---|
| Answer website live chat | `enabled` | Within seconds (the bridge re-reads config every poll; turning it off sets the bot offline in Odoo) |
| Bot user login / API key | `bot_username`, `bot_api_key_enc` | Next poll (the bridge reconnects) |
| Default AI employee, greeting, check interval | `default_agent`, `greeting`, `poll_interval_seconds` (2–60) | Next poll |
| Live Chat channels to serve | `livechat_channel_ids` | Next poll |
| Hand off to a person | `handoff_user_ids`, `handoff_keywords`, `handoff_message` | Next poll |
| Audience rules | `[[odoo_livechat.audience_rules]]` (`name`, `agent`, `enabled`, `livechat_channel_ids`, `chatbot_script_ids`, `country_codes`, `langs`, `visitor`, `partner_tags`, `keywords`); every named AI employee must exist or the save is refused | New chats (a chat keeps the employee it was routed to) |

## Inference

After a successful save (`inference.update`, admin) the gateway resets the inference engine, so the next reply uses the new settings without a restart.

- Confidence Router → Advanced: `[router] local_tools`, `ucci_fast_router`, `ucci_strong_router`, `ucci_observations`, `ucci_shadow_strong`, `ucci_shadow_max_inflight` (1 to 16), `ucci_drop_stop_token`; `[generation] capture_logprobs`, `capture_top_logprobs`.
- llamafile local server: `[llamafile] enabled`, `dir`, `default_file`, `host`, `port`, `gpu_layers`, `context_size`, `extra_args`. Known limit: clearing a field on the page does not remove the saved value; use the Advanced config editor for that.

## Employee edit page

The page writes the employee's `agent.toml` through `agents.update` and sends only changed fields, so a save no longer clears the heartbeat schedule (`[heartbeat] cron`).

| Tab → section | Key |
|---|---|
| Budget → Daily cap | `[budget] daily_cap_cents` |
| Tools & permissions → Tools that need a checkpoint (always wait for approval, treat as irreversible, let the judge decide first, needs a task grant) | `[capabilities] approval_required_tools`, `irreversible_tools`, `maybe_irreversible_tools`, `scoped_tools` |
| Tools & permissions → Parallel branches | `[fork] enabled` |
| Tools & permissions → Outgoing reply protection | `[guardrails] enabled`, `block_secrets`, `block_injection_echo`, `redact_pii`, `deny_phrases` |
| Tools & permissions → Permissions | the four `[permissions]` flags (below) |
| Brain & engine → Reasoning effort | `[model] effort` |
| Brain & engine → Provider / Fallback | `[runtime] provider` / `fallback`, now including qwen, kimi, copilot, kiro, cursor, vibe, opencode |
| Brain & engine → Slim startup context | `[runtime] minimal_context` |
| Brain & engine → One employee, four roles | `[team] enabled`, `[team.roles.<role>]` |
| Automation → Night-time tidy-up | `[night_engine] enabled` |
| Channels & integrations → Callable by outside contacts (v1.76) | `[channels] public_trigger` (admin-only; see [Calling AI staff with `@`](../features/62-calling-ai-staff.md)) |
| Automation → Memory → decision continuity, keep decisions for (days) | `[memory] decision_continuity`, `decision_ttl_days` (1 to 3650) |
| Advanced → Model Extras → Advanced key/value | any `[section] key`, each row typed (string, integer, float, boolean, string array); the result is parsed as a full `AgentConfig` before writing and the whole save is refused on failure. The `agent`, `capabilities`, `container`, `permissions`, `channels`, `odoo`, `mcp`, `runtime`, `prompt`, `os_watch` and `budget` sections cannot be edited here (unreleased: `prompt`, `os_watch` and `budget` added; they hold admin-only keys such as `prompt.cli_bare_mode`, which an admin now changes in the Advanced config editor) |

**Admin-only fields.** `[agent] reports_to`, `department`, `name`, all of `[capabilities]`, `[container] sandbox_enabled`, `network_access`, `readonly_project`, the five `[permissions]` flags (`can_modify_own_soul`, `can_create_agents`, `can_send_cross_agent`, `can_schedule_tasks`, `can_modify_own_skills`) and (unreleased) `[permissions] allowed_channels`, `[guardrails] enabled` and `block_secrets`, (v1.76) `[channels] public_trigger`, and (unreleased) the per-employee bot tokens `[channels.discord]`, `[channels.telegram]`, `[channels.slack]`, the whole `[odoo]` table, `[runtime] provider` and `fallback`, `[prompt] cli_bare_mode`, the whole `[os_watch]` table, `[model] account_pool`, `role = "main"` (becoming or leaving it), and loosening the budget: raising `monthly_limit_cents` or `daily_cap_cents`, removing a cap (0 = no cap) or turning `hard_stop` off. An employee's Owner may still add or lower a cap and turn `hard_stop` on. A change from a non-admin is refused, and a successful change by an admin writes the audit event `agent_authority_changed`, and a refused attempt writes `agent_authority_refused`. When a model change would switch the runtime automatically, a non-admin gets a message saying so. `org.toml` is updated only when `reports_to` or `department` really changed. `agents.inspect` returns the matching form fields as `admin_only_fields`; the edit page disables them for non-admins.

**Who may save (unreleased, behaviour change).** `agents.update` re-reads the caller's role and employee binding from the account database on every save, and again right before writing. Removing someone's binding, suspending them or requiring a password change takes effect on their next save without waiting for their session to expire. Every save of `agent.toml` from any path (this page, `/model` in a chat, the Advanced config editor, automatic edits) now keeps comments and key order, keeps a version (see [Profile files](#profile-files-unreleased)) and writes `agent_file_changed`. When the registry cannot be reloaded after a save, the message reads 「已儲存，但暫時無法立即套用；請稍後重新整理，仍未生效請重新啟動服務」 (there is no periodic reload).

**Permission flags are enforced (behaviour change).** `can_create_agents`, `can_send_cross_agent`, `can_modify_own_skills` and `can_schedule_tasks` had no reader before. Since v1.68.0 a flag written as `false` blocks the matching tools at the MCP dispatch gate (`create_agent`; `send_to_agent`, `spawn_agent`; `create_reminder`, `tasks_create` with a `schedule`; `skill_hub_install`, `shared_skill_adopt`, `skill_graduate`, `skill_pin`, `skill_from_recording`); an absent or wrongly typed flag allows. Older templates often wrote these flags as `false`, so the first boot after the upgrade migrates every employee once: in a `[permissions]` table without the marker `permissions_enforced_since`, each of the four flags that is `false` becomes `true` and `permissions_enforced_since = "1.68.0"` is added; each reset writes the audit event `permission_flags_reset`. A `false` written afterwards on this page or in the Advanced config editor counts as an operator decision. Ephemeral role members (`agents/.ephemeral/`) are not migrated and keep least privilege.

## Profile files (unreleased)

RPCs `agent_files.list / get / diff / history / save / rollback / withdraw / pending_get`; also used by the boundaries editor on the employee edit page (`contract.update`). Files: persona (`SOUL.md`), self-introduction (`IDENTITY.md`), work rules (`INSTRUCTIONS.md`), boundaries (`CONTRACT.toml`), basic settings (`agent.toml`, read-only and masked here; edit it on the edit page or, as an admin, in the Advanced config editor), and the employee's own skills (existing `.md` files under `SKILLS/`, file key `skill:<path>`, for example `skill:quote/SKILL.md`). Skills can only be edited here, not created or deleted (use the custom skill flow to add one), and scripts next to a skill are never touched; every path level must be a single name without `..`, at most four levels deep, and symlinks are refused. Limits: 64 KB per Markdown or skill file, 32 KB for `CONTRACT.toml`, at most 100 `must_not` and 100 `must_always` entries of up to 500 characters.

- **Profile files tab:** the 設定檔 (Profile files) tab on the employee detail page (`/agents/<employee>/files`). On the left, a plain-language list with the real file name in small grey text and the badges "Waiting for review", "Changed outside the dashboard" and "View only". On the right the file is shown read-only; its owner and admins press Edit to change it, with the size and limit below and the buttons Show changes, Save and check, and Discard changes. The check result uses the same review dialog as the boundaries editor. If someone else saved first, your text and the current text are shown side by side with Copy my content and Reload. Version history (Operator and up) lists time, who and where each version came from, and can view, compare with the current text or restore. An unsaved edit asks before switching file, switching tab or closing the page. People who can only view see "You can view this. Only this staff member's owner or an administrator can change it."
- **Who:** anyone bound to the employee reads the files; Operator and up read versions and diffs; the employee's Owner and admins save. Every call re-reads the account database, not the session. Admins decide approval requests.
- **Check on every save:** fixed rules first (size, control characters, a strict `CONTRACT.toml` parse, hidden or invisible content, injection patterns in the lines this change adds or in new `must_always` entries, key-like text, a persona that loses most of its text, a contract that drops or rewrites a `must_not` / `must_always` entry or raises / removes the tool-call limit; skills additionally: a leading `---` block must read the way the skill loader reads it and its `name` must be a single name, and findings of the skill scanner that this change introduces count, with commands, sending data out and injection patterns as high risk, bare links and mentions of a forbidden item as needs attention), then a short review of the diff by the deployment's utility model (`[runtime] utility_provider` / `utility_model`, override with `[agent_files] review_provider` / `review_model`; billed to the deployment as `agent-file-review`, never to the employee's budget). The model can only make the result stricter. Results: `pass` applies at once; `warn` needs a reason (at least 4 characters) or a request to an admin; `block` can only be sent to an admin by a non-admin, and an admin applies it only with a reason and "I understand the risk" (audited as Critical). If the review cannot finish (no utility model, timeout, an unusable answer, a quota reached) or the change was too long to show in full, the result is at least `warn` (or `block` with `[agent_files] on_analysis_failure = "block"`), a non-admin can only send it to an admin, and an admin confirms it only with "I understand the risk"; it is never a silent pass and cannot be switched off. Quotas per day: 200 per deployment, 40 per person, 60 per employee (`review_daily_limit`, `review_daily_limit_per_user`, `review_daily_limit_per_agent`), plus at most 10 checks per person per minute (`checks_per_minute`); the same change checked again within 15 minutes reuses the result. Confirming does not run the review again; a confirmation must match the same person, file, starting version and content within 15 minutes, the fixed rules are re-run at the person's current permission level (stricter now ⇒ check again), and confirming a `block` re-checks inside the file lock that the person is still an admin. A SOUL.md or INSTRUCTIONS.md changed outside the dashboard is checked as a whole and compared with the last approved version (reason "changed outside").
- **Saving:** under the file lock, with the hash the editor read (`base_hash`); a file changed by someone else in between is refused as a conflict. The new text takes effect from the employee's next turn; channels get a one-line notice with the next reply.
- **Versions:** kept outside the employee folder in `<home>/agent_file_history/<employee>/<file>/` (owner-only files), 20 per file by default (`[agent_files] history_keep`, 5–100). The first change also keeps what the file contained before. Restoring a version runs the same check. Removing an employee moves its history to `agent_file_history/_trash/`; a new employee with the same id starts with none.
- **Baselines:** saving SOUL.md or INSTRUCTIONS.md updates its approved fingerprint (INSTRUCTIONS.md has one now too, checked by the heartbeat as `_instructions_integrity_drift`). A file changed outside the system shows as changed-outside; restore the approved version or check and accept the current content (save it as it is: the whole file is checked, at least a reason or an approval request is needed, and once confirmed the file is not rewritten; the current text is kept as a version and becomes the approved baseline. Saving an unchanged file that was not changed outside is still "unchanged"). Renaming an employee updates the name inside SOUL.md / IDENTITY.md and keeps an approved SOUL approved, while a SOUL that had already changed outside stays flagged. The name must be plain text (1–64 characters, no line breaks, invisible characters or `< > [ ] { }` / backticks), and a renamed file that trips a fixed rule is left as it was (listed in `identity_files_skipped`).
- **Approval requests:** kind `agent_file_change`, decided only in the dashboard by a current admin, 3 days to decide (expiry counts as a denial). Channels only get a notice without buttons. The proposed text is stored in `<home>/agent_file_pending/` until the request ends. Approving applies first; if the file changed while the request waited, the request is denied and the requester has to send it again.
- **Audit:** `agent_file_changed`, `agent_file_change_refused`, `agent_file_review_failed`, `agent_file_change_requested` (hashes only, never the text).

## Thinking display

No dedicated control yet; set these in the Advanced config editor (below). Both are read on every turn, so a change applies from the next reply.

| File | Key | Values |
|---|---|---|
| `config.toml` | `[thinking] display` | `summary` (default: timer, tokens and masked thinking text in WebChat), `status` (timer and tokens only), `off` (no thinking frames; WebChat still shows its own timer) |
| `config.toml` | `[thinking] max_chars` | thinking text per turn, default 4000, clamped to 200–20000 |
| `config.toml` | `[thinking] anthropic_display` | `summarized` (default) or `updates` (beta, only some Claude models) |
| `agent.toml` | `[model] show_thinking` | `summary` / `status` / `off`; unset follows `[thinking] display`. The stricter of the two wins |

A `config.toml` that cannot be read or parsed, or an unknown `display` value, counts as `status`. Only signed-in dashboard users in WebChat can see thinking text; messaging apps never do. Details: [Thinking display](../features/64-thinking-display.md).

## Advanced config editor

Location: Settings → Advanced → Advanced config editor. Admins only; RPCs `config.raw.get` / `config.raw.set`.

- **Files:** system settings (`config.toml`), inference settings (`inference.toml`) and each AI employee's `agent.toml` (a real directory under `agents/`, no symbolic links).
- **Secret masking:** values whose key ends in `_enc`, is `key` or ends in `_key`, or contains token, secret, password, passwd, api_key, apikey, widget_key, private_key, credential or service_account_json, every value in `headers`, `otlp_headers` and `env` tables, and URLs carrying a password are shown as `«set»`. Leaving `«set»` in place keeps the stored value; `«set»` where nothing is stored is refused. `[mcp_keys]` is not shown at all.
- **Validation:** TOML syntax first, with the line and column of an error; then the typed check (the section validators for `config.toml`, `InferenceConfig`, `AgentConfig`). Nothing is written on failure. An employee's `[agent] name` cannot be changed here.
- **Document knowledge base:** collections, documents, sharing and the access record are managed under Company → Document library in the sidebar; the global `[knowledge]` limits have no page yet, and their keys (`enabled`, `max_source_bytes`, `max_source_chars`, `max_sources_per_collection`, `max_collections`, `max_chunks_total`, `extract_timeout_secs`, `extract_memory_mb`, `access_log_retention_days`, `max_collection_bytes`, `max_total_bytes`, and `extract_when_unavailable` = `refuse` / `run_unsandboxed`) are type-checked here. An employee's `[knowledge] collections` is set under Agents → Edit Settings → Tools & permissions → Document library (only collections shared with it can be picked), or directly in that employee's `agent.toml`. See [the knowledge base page](../features/63-knowledge-base.md#settings-configtoml).
- **Conflicts:** opening a file returns a hash of its content, which the save sends back. If the file changed in between, the save is refused; reload and edit again.
- **Backup:** before writing, the file is copied to `<file>.bak-<unix time>` (mode 0600); the newest five are kept.
- **Audit:** every write records `config_raw_edited`, at Warning level when it touches `[delegation]`, `[acp]`, or an employee's `[agent]`, `[capabilities]`, `sandbox_enabled`, `network_access` or `can_modify_own_soul`.
- **Restart list:** after saving, the editor lists what needs a restart. In `config.toml`, `[gateway]`, `[server]`, `[telemetry]`, `[logging]`, `[channels]`, `[wiki]`, `[relay]` and `[decision]` are read only at boot; `general.name`, `rotation.health_check_interval_seconds`, `goal_loop.resume_on_restart` and `knowledge.max_source_bytes` (it sizes the upload route's body limit) also need a restart; the log level, live data feeds and redaction are not listed when they could be applied live. Saving `inference.toml` always resets the inference engine immediately. For an employee file only `heartbeat.max_concurrent_runs` needs a restart.

## Maintenance SSH dashboard (unreleased)

In Device → Maintenance mode, an Admin selects the duration and capabilities,
re-enters their password and types `MAINTENANCE`. Selecting SSH also requires
one technician public key in OpenSSH format. The dashboard checks the input
and immediately clears a pasted private key; the gateway and device helper
remain the authority. RSA keys must be 3072–8192 bits. Unknown support or an
old/partly updated image disables SSH, while diagnostic details remain
available. Five wrong passwords lock opening for 15 minutes.

An open window shows `ssh duduclaw-maint@duduclaw.local`, available private
IPv4 addresses, the technician and device fingerprints, expiry, connections
and failed sign-ins. Compare both fingerprints with the Admin notification.
If no notification channel is available, use the local device screen; this
dashboard alone cannot authenticate the device. A configured channel does
not prove delivery. Port 22 accepts private/link-local and VPN CGNAT
`100.64.0.0/10` sources, not global IPv6; a matching range does not establish
trust. The menu permits selected diagnostics and restarting the local model
or gateway. Restarting the gateway closes the window and disconnects SSH.

An unconfirmed close stays visible as a warning. Expand Maintenance history
for recent window fingerprints and connection events. The UI polls every
30 seconds, so an expiry or device-side close may appear on the next poll.
The complete OS image and real SSH/notification flow are not yet verified;
this is an unreleased interface, not acceptance of the assembled device.
