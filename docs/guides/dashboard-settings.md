# Dashboard settings map (v1.68.0)

This page lists what can be set from the dashboard as of v1.68.0: which page, which configuration key it writes, and whether the gateway needs a restart afterwards. The last section covers the Advanced config editor, which edits keys that have no dedicated control.

Other languages: [繁體中文](zh-TW/dashboard-settings.md) · [日本語](ja-JP/dashboard-settings.md)

## How saving works

- System settings (`config.toml`) are written through `system.update_config`, which is admin-only. A page sends only the fields you changed; with no change nothing is sent and the page says "Nothing changed, so there is nothing to save."
- The write locks the file and re-reads it. If another writer changed the file after you loaded it, the save is refused; reload and save again.
- When `config.toml` does not parse, `system.update_config`, the live data feed editor and the Advanced config editor refuse to write; fix the syntax in the Advanced config editor first. Other settings pages (channels, accounts, Odoo and so on) do not have this check yet.
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

- Website chat widget: "Open the website chat widget" writes `[webchat] public_widget` and "Widget key" writes `[webchat] widget_key` (16 to 256 visible ASCII characters, with a Generate button; turning the widget on requires a key; the key is never returned). Applies immediately. The widget key is stored in plain text in `config.toml`: it is the public key that appears in the website's page source, so it is not treated as a secret.
- WhatsApp, Feishu, Google Chat, Teams, WeCom and DingTalk: the six webhook routes are always mounted. A route answers 404 until its channel is configured; after that it enforces the platform's signature check and answers 401 to a bad signature. `channels.add` starts the channel without a restart: `channels.add` returns `hot_started` and `restart_required: false`, with `not_started_reason` when credentials are incomplete. A per-employee Slack bot also starts when it is added.

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
| Automation → Memory → decision continuity, keep decisions for (days) | `[memory] decision_continuity`, `decision_ttl_days` (1 to 3650) |
| Advanced → Model Extras → Advanced key/value | any `[section] key`, each row typed (string, integer, float, boolean, string array); the result is parsed as a full `AgentConfig` before writing and the whole save is refused on failure. The `agent`, `capabilities`, `container`, `permissions`, `channels`, `odoo`, `mcp` and `runtime` sections cannot be edited here |

**Admin-only fields.** `[agent] reports_to`, `department`, `name`, all of `[capabilities]`, `[container] sandbox_enabled`, `network_access` and `[permissions] can_modify_own_soul`: a change from a non-admin is refused, and a successful change by an admin writes the audit event `agent_authority_changed`, and a refused attempt writes `agent_authority_refused`. `org.toml` is updated only when `reports_to` or `department` really changed.

**Permission flags are enforced (behaviour change).** `can_create_agents`, `can_send_cross_agent`, `can_modify_own_skills` and `can_schedule_tasks` had no reader before. Since v1.68.0 a flag written as `false` blocks the matching tools at the MCP dispatch gate (`create_agent`; `send_to_agent`, `spawn_agent`; `schedule_task`, `create_reminder`, `tasks_create` with a `schedule`; `skill_hub_install`, `shared_skill_adopt`, `skill_graduate`, `skill_pin`, `skill_from_recording`); an absent or wrongly typed flag allows. Older templates often wrote these flags as `false`, so the first boot after the upgrade migrates every employee once: in a `[permissions]` table without the marker `permissions_enforced_since`, each of the four flags that is `false` becomes `true` and `permissions_enforced_since = "1.68.0"` is added; each reset writes the audit event `permission_flags_reset`. A `false` written afterwards on this page or in the Advanced config editor counts as an operator decision. Ephemeral role members (`agents/.ephemeral/`) are not migrated and keep least privilege.

## Advanced config editor

Location: Settings → Advanced → Advanced config editor. Admins only; RPCs `config.raw.get` / `config.raw.set`.

- **Files:** system settings (`config.toml`), inference settings (`inference.toml`) and each AI employee's `agent.toml` (a real directory under `agents/`, no symbolic links).
- **Secret masking:** values whose key ends in `_enc`, is `key` or ends in `_key`, or contains token, secret, password, passwd, api_key, apikey, widget_key, private_key, credential or service_account_json, every value in `headers`, `otlp_headers` and `env` tables, and URLs carrying a password are shown as `«set»`. Leaving `«set»` in place keeps the stored value; `«set»` where nothing is stored is refused. `[mcp_keys]` is not shown at all.
- **Validation:** TOML syntax first, with the line and column of an error; then the typed check (the section validators for `config.toml`, `InferenceConfig`, `AgentConfig`). Nothing is written on failure. An employee's `[agent] name` cannot be changed here.
- **Conflicts:** opening a file returns a hash of its content, which the save sends back. If the file changed in between, the save is refused; reload and edit again.
- **Backup:** before writing, the file is copied to `<file>.bak-<unix time>` (mode 0600); the newest five are kept.
- **Audit:** every write records `config_raw_edited`, at Warning level when it touches `[delegation]`, `[acp]`, or an employee's `[agent]`, `[capabilities]`, `sandbox_enabled`, `network_access` or `can_modify_own_soul`.
- **Restart list:** after saving, the editor lists what needs a restart. In `config.toml`, `[gateway]`, `[server]`, `[telemetry]`, `[logging]`, `[channels]`, `[wiki]`, `[relay]` and `[decision]` are read only at boot; `general.name`, `rotation.health_check_interval_seconds` and `goal_loop.resume_on_restart` also need a restart; the log level, live data feeds and redaction are not listed when they could be applied live. Saving `inference.toml` always resets the inference engine immediately. For an employee file only `heartbeat.max_concurrent_runs` needs a restart.
