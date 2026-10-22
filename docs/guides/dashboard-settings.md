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
| Advanced → Model Extras → Advanced key/value | any `[section] key`, each row typed (string, integer, float, boolean, string array); the result is parsed as a full `AgentConfig` before writing and the whole save is refused on failure. The `agent`, `capabilities`, `container`, `permissions`, `channels`, `odoo`, `mcp` and `runtime` sections cannot be edited here |

**Admin-only fields.** `[agent] reports_to`, `department`, `name`, all of `[capabilities]`, `[container] sandbox_enabled`, `network_access`, `[permissions] can_modify_own_soul` and (v1.76) `[channels] public_trigger`: a change from a non-admin is refused, and a successful change by an admin writes the audit event `agent_authority_changed`, and a refused attempt writes `agent_authority_refused`. `org.toml` is updated only when `reports_to` or `department` really changed.

**Permission flags are enforced (behaviour change).** `can_create_agents`, `can_send_cross_agent`, `can_modify_own_skills` and `can_schedule_tasks` had no reader before. Since v1.68.0 a flag written as `false` blocks the matching tools at the MCP dispatch gate (`create_agent`; `send_to_agent`, `spawn_agent`; `create_reminder`, `tasks_create` with a `schedule`; `skill_hub_install`, `shared_skill_adopt`, `skill_graduate`, `skill_pin`, `skill_from_recording`); an absent or wrongly typed flag allows. Older templates often wrote these flags as `false`, so the first boot after the upgrade migrates every employee once: in a `[permissions]` table without the marker `permissions_enforced_since`, each of the four flags that is `false` becomes `true` and `permissions_enforced_since = "1.68.0"` is added; each reset writes the audit event `permission_flags_reset`. A `false` written afterwards on this page or in the Advanced config editor counts as an operator decision. Ephemeral role members (`agents/.ephemeral/`) are not migrated and keep least privilege.

## Settings → Advanced → Knowledge base search

Admin only (new in 1.76.0). Shows whether the document knowledge base searches by meaning or by keywords only, and why not when it does not, with the share of passages that already have vectors.

| Control | Writes | Effect |
|---|---|---|
| Where it is computed: On this computer / OpenAI / Google Gemini / Off | `[knowledge.embedding] provider` (+ `cloud_model`, `cloud_account` for cloud) | Next search. Choosing a cloud provider opens a dialog naming the vendor and host that receive passages; **Use** stays disabled until the box is ticked, and the acknowledgement is stored as `cloud_disclosure_ack`. The dialog warns when redaction is off (nothing is sent then). Choosing anything else removes the acknowledgement. |
| Install (size) | downloads into `models/embedding/<model>/` and `lib/onnxruntime/` | Background download with progress, resumable after **Cancel**; vectors for every passage are queued when it finishes. |
| Check again (offline install) | — | Hashes files unpacked from an offline bundle and marks the model installed. |
| Use this model | `local_model` | Switches the local model; passages get vectors for it in the background (vectors of the previous model stay but are not compared). |
| Remove | deletes the model files | Search becomes keyword-only with that reason shown; the runtime library stays. |
| Fill in missing search data | — | Queues vectors for every source still missing them. |

Details: [Document knowledge base](../features/63-knowledge-base.md#embedding-models).

## Advanced config editor

Location: Settings → Advanced → Advanced config editor. Admins only; RPCs `config.raw.get` / `config.raw.set`.

- **Files:** system settings (`config.toml`), inference settings (`inference.toml`) and each AI employee's `agent.toml` (a real directory under `agents/`, no symbolic links).
- **Secret masking:** values whose key ends in `_enc`, is `key` or ends in `_key`, or contains token, secret, password, passwd, api_key, apikey, widget_key, private_key, credential or service_account_json, every value in `headers`, `otlp_headers` and `env` tables, and URLs carrying a password are shown as `«set»`. Leaving `«set»` in place keeps the stored value; `«set»` where nothing is stored is refused. `[mcp_keys]` is not shown at all.
- **Validation:** TOML syntax first, with the line and column of an error; then the typed check (the section validators for `config.toml`, `InferenceConfig`, `AgentConfig`). Nothing is written on failure. An employee's `[agent] name` cannot be changed here.
- **Document knowledge base:** collections, documents, sharing and the access record are managed under Company → Document library in the sidebar; the global `[knowledge]` limits have no page yet, and their keys (`enabled`, `max_source_bytes`, `max_source_chars`, `max_sources_per_collection`, `max_collections`, `max_chunks_total`, `extract_timeout_secs`, `extract_memory_mb`, `access_log_retention_days`, `max_collection_bytes`, `max_total_bytes`, and `extract_when_unavailable` = `refuse` / `run_unsandboxed`) are type-checked here. So are the `[knowledge.embedding]` keys (`provider` = `local` / `openai` / `gemini` / `none`, `local_model`, `cloud_model`, `cloud_account`, `cloud_base_url`, `allow_private_endpoint`, `batch_size`, `threads`, `max_vector_rows`, `idle_unload_minutes`); the disclosure record `cloud_disclosure_ack` cannot be written here (the save is refused) and is set only by the disclosure dialog of Knowledge base search. An employee's `[knowledge] collections` is set under Agents → Edit Settings → Tools & permissions → Document library (only collections shared with it can be picked), or directly in that employee's `agent.toml`. See [the knowledge base page](../features/63-knowledge-base.md#settings-configtoml).
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
