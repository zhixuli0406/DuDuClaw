# Dashboard settings map (v1.68.0)

This page lists what can be set from the dashboard as of v1.68.0: which page, which configuration key it writes, and whether the gateway needs a restart afterwards. The last section covers the Advanced config editor, which edits keys that have no dedicated control.

Other languages: [繁體中文](zh-TW/dashboard-settings.md) · [日本語](ja-JP/dashboard-settings.md)

## How saving works

Enterprise setting (v1.76.0, User Management → Organisation settings): `users.org_settings.get/update` controls `[people] manager_team_history` (default on for valid config). Inform staff before enabling it. The switch takes effect immediately, advances the activation boundary when re-enabled, and is protected/audited in the raw editor too. See [Read-only team conversations](../features/68-manager-team-history.md).

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
| Global background tasks → Global utility provider / model | `[runtime] utility_provider` / `utility_model` | Immediately (read per call) |
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

Global background tasks without an assigned employee (such as employee selection and redaction-rule creation) default to Claude / `claude-haiku-4-5`. Select a supported provider and enter a compatible model; removed `gemini` and unknown provider values are refused by the server. Clearing the model removes the key: Claude uses `claude-haiku-4-5`; OpenAI-compatible endpoints require an explicit model. Codex is also supported in its restricted utility profile and uses the CLI default when the model is blank. Grok, Antigravity and generic CLI providers are refused at execution even when their settings can be saved. A model from a known different vendor family shows an inline warning for non-Claude providers. Saves require admin access and use the standard config commit path. An inline alert appears when the selected global provider is Claude and no employee uses the Claude runtime; it does not check whether Claude is logged in.

`[general] log_level` (Everyday → General → Log Level) applies as soon as it is saved, unless the environment variable `RUST_LOG` is set, in which case the save reports a restart. Saving the account rotation strategy or the rate-limit cooldown clears the rotation cache, so the next call uses the new value (before, the change could lag by up to 30 minutes).

## Channels

- Group "mention only" (`mention_only`, per group): since v1.76 a LINE group with it on answers only messages that call an AI employee by `@` (or ask for the `@` list) and the one message a called employee is waiting for; before, it silenced the group entirely. The shared Telegram bot also accepts employee mentions. See [Calling AI staff with `@`](../features/62-calling-ai-staff.md).
- Website chat widget: "Open the website chat widget" writes `[webchat] public_widget` and "Widget key" writes `[webchat] widget_key` (16 to 256 visible ASCII characters, with a Generate button; turning the widget on requires a key; the key is never returned). Applies immediately. The widget key is stored in plain text in `config.toml`: it is the public key that appears in the website's page source, so it is not treated as a secret.
- WhatsApp, Feishu, Google Chat, Teams, WeCom and DingTalk: the six webhook routes are always mounted. A route answers 404 until its channel is configured; after that it enforces the platform's signature check and answers 401 to a bad signature. `channels.add` starts the channel without a restart: `channels.add` returns `hot_started` and `restart_required: false`, with `not_started_reason` when credentials are incomplete. A per-employee Slack bot also starts when it is added.
- Thinking display in messaging apps (no dedicated control; set in the Advanced config editor, read on every reply): `[thinking] channel_audience` = `internal` (default: a sender whose verified channel identity belongs to an active dashboard account that is an Admin or is bound to the answering employee sees the timer and a masked summary, everyone else the timer only), `all` (every sender of a one-to-one chat sees the summary) or `none` (nothing); `[thinking] channel_group_display` = `status` (default: group chats show the timer only), `summary` or `off`. The stricter of `[thinking] display` and the answering employee's `[model] show_thinking` still caps both. An unreadable `config.toml` or an unknown value shows nothing. Website visitors (Odoo live chat, the WebChat widget) never see thinking. See [Thinking display in messaging apps](../features/66-thinking-in-messaging-apps.md).

## Integrations → Odoo → Connection

"Allow Odoo on an internal network" writes `[odoo] allow_private_network` (admin only, default off; saved together with the connection through `odoo.configure`, tested by the same switch value in **Test connection**). Off: the Odoo url, global or per employee, must be HTTPS to a public host (plain `http://` only for `localhost`). On: addresses such as 192.168.x.x or 10.x.x.x are allowed, over HTTPS or HTTP; the local machine, link-local and cloud metadata addresses (169.254.169.254, `metadata.google.internal`) stay refused, the resolved addresses are pinned for the connection and no proxy is used. The first boot after the upgrade turns it on by itself when a configured Odoo url already resolves to an internal address (audit event `odoo_allow_private_network_auto_enabled`), so existing installs keep working. Takes effect on the next connection.

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
| Skills → This staffer's skills (v1.76.0) | not a config key: switches are stored in `<home>/skill_state/<agent_id>.json` (`skills.set_enabled`), removed skills move to `<home>/skill_archive/<agent_id>/` (`skills.remove` / `skills.restore`); Owner of the employee or admin. See [Skill library page](../features/15-skill-lifecycle.md#the-skill-library-page-v1760) |
| Tools & permissions → Tools that need a checkpoint (always wait for approval, treat as irreversible, let the judge decide first, needs a task grant) | `[capabilities] approval_required_tools`, `irreversible_tools`, `maybe_irreversible_tools`, `scoped_tools` |
| Tools & permissions → Parallel branches | `[fork] enabled` |
| Tools & permissions → Outgoing reply protection | `[guardrails] enabled`, `block_secrets`, `block_injection_echo`, `redact_pii`, `deny_phrases` |
| Tools & permissions → Permissions | the four `[permissions]` flags (below) |
| Brain & engine → Reasoning effort | `[model] effort` |
| Brain & engine → Provider / Fallback | `[runtime] provider` / `fallback`, now including qwen, kimi, copilot, kiro, cursor, vibe, opencode |
| Brain & engine → Slim startup context | `[runtime] minimal_context` |
| Brain & engine → Per-run time limit | `[runtime] timeout_secs`: integer seconds, default 300, range 30–3600; Codex / Grok / Antigravity / other CLI runtimes / OpenAI-compatible API; Claude retains its fixed 30-minute limit |
| Brain & engine → Utility model | `[model] utility`: blank follows the main model on Codex and OpenAI-compatible APIs; Claude defaults to `claude-haiku-4-5`; Grok, Antigravity and generic CLIs use global background settings |
| Brain & engine → One employee, four roles | `[team] enabled`, `[team.roles.<role>]` |
| Automation → Night-time tidy-up | `[night_engine] enabled` |
| Integrations → Own channels (v1.76.0) | `[channels.telegram] bot_token_enc`, `[channels.googlechat] project_number` / `service_account_json_enc` through `agents.channels.*` (admin binds; the owner sees status; see [Own channels](per-employee-channels.md)) |
| Channels & integrations → Callable by outside contacts (v1.76) | `[channels] public_trigger` (admin-only; see [Calling AI staff with `@`](../features/62-calling-ai-staff.md)) |
| Automation → Memory → decision continuity, keep decisions for (days) | `[memory] decision_continuity`, `decision_ttl_days` (1 to 3650) |
| Advanced → Model Extras → Advanced key/value | any `[section] key`, each row typed (string, integer, float, boolean, string array); the result is parsed as a full `AgentConfig` before writing and the whole save is refused on failure. The `agent`, `capabilities`, `container`, `permissions`, `channels`, `odoo`, `mcp`, `runtime`, `prompt`, `os_watch` and `budget` sections cannot be edited here (v1.76.0: `prompt`, `os_watch` and `budget` added; they hold admin-only keys such as `prompt.cli_bare_mode`, which an admin now changes in the Advanced config editor) |

The per-run time limit is saved separately from the container task timeout. The server rejects values outside 30–3600 seconds and non-integers. For manually edited files, out-of-range integers are clamped and wrong-typed values use the default, with a warning. Antigravity keeps its outer backstop 30 seconds longer. OpenAI-compatible employees require a model in either `utility` or `preferred`; Codex can use its CLI default; Grok, Antigravity and generic CLIs use global background settings. A vendor-family mismatch in the utility model shows an inline warning because background work such as conversation compression and fact extraction will fail.

Increasing the effective `timeout_secs` requires an administrator. An Owner may lower it; clearing the setting restores 300 seconds and needs an administrator only when that increases the current effective limit.

Claude utility calls use the lightweight lane with the CLI's available credentials and do not enter account rotation. Normal chat retains its existing account rotation.

Utility calls, including classifiers and judges, use a dedicated execution profile. Claude uses the tool-free, single-turn lightweight lane; OpenAI-compatible APIs send one completion without tools and never enter the tool loop. Codex uses a restricted profile: a read-only sandbox, no DuDuClaw MCP overrides, a fresh owner-only empty temporary working directory, ignored user config, and no persisted session. The temporary directory is removed after the call, including errors and timeouts; the system prompt travels in the prompt without writing `AGENTS.md`. This also applies to explicit Codex judge and team-verifier hints. Grok, Antigravity and generic CLIs remain unsupported for utility execution, so employee background work resolves to the global utility provider/model (default Claude / `claude-haiku-4-5`), with one warning per employee/runtime. The global provider must be Claude, Codex or OpenAI-compatible; an unsupported global provider or explicit hint is refused. Existing higher-level judge degradation may retry without an unsupported hint, subject to these same profiles. Codex may retain built-in reading tools; see [Security defense](../features/05-security-defense.md) for the residual risk.

**Admin-only fields.** `[agent] reports_to`, `department`, `name`, all of `[capabilities]`, `[container] sandbox_enabled`, `network_access`, `readonly_project`, the five `[permissions]` flags (`can_modify_own_soul`, `can_create_agents`, `can_send_cross_agent`, `can_schedule_tasks`, `can_modify_own_skills`) and (v1.76.0) `[permissions] allowed_channels`, `[guardrails] enabled` and `block_secrets`, (v1.76) `[channels] public_trigger`, and (v1.76.0) the per-employee bot tokens `[channels.discord]`, `[channels.telegram]`, `[channels.slack]`, `[channels.googlechat]`, the whole `[odoo]` table, `[runtime] provider` and `fallback`, `[prompt] cli_bare_mode`, the whole `[os_watch]` table, `[model] account_pool`, `role = "main"` (becoming or leaving it), and loosening the budget: raising `monthly_limit_cents` or `daily_cap_cents`, removing a cap (0 = no cap) or turning `hard_stop` off. An employee's Owner may still add or lower a cap and turn `hard_stop` on. A change from a non-admin is refused, and a successful change by an admin writes the audit event `agent_authority_changed`, and a refused attempt writes `agent_authority_refused`. When a model change would switch the runtime automatically, a non-admin gets a message saying so. `org.toml` is updated only when `reports_to` or `department` really changed. `agents.inspect` returns the matching form fields as `admin_only_fields`; the edit page disables them for non-admins.

**Who may save (v1.76.0, behaviour change).** `agents.update` re-reads the caller's role and employee binding from the account database on every save, and again right before writing. Removing someone's binding, suspending them or requiring a password change takes effect on their next save without waiting for their session to expire. Every save of `agent.toml` from any path (this page, `/model` in a chat, the Advanced config editor, automatic edits) now keeps comments and key order, keeps a version (see [Profile files](#profile-files-v1760)) and writes `agent_file_changed`. When the registry cannot be reloaded after a save, the message reads 「已儲存，但暫時無法立即套用；請稍後重新整理，仍未生效請重新啟動服務」 (there is no periodic reload).

**Permission flags are enforced (behaviour change).** `can_create_agents`, `can_send_cross_agent`, `can_modify_own_skills` and `can_schedule_tasks` had no reader before. Since v1.68.0 a flag written as `false` blocks the matching tools at the MCP dispatch gate (`create_agent`; `send_to_agent`, `spawn_agent`; `create_reminder`, `tasks_create` with a `schedule`; `skill_hub_install`, `shared_skill_adopt`, `skill_graduate`, `skill_pin`, `skill_from_recording`); an absent or wrongly typed flag allows. Older templates often wrote these flags as `false`, so the first boot after the upgrade migrates every employee once: in a `[permissions]` table without the marker `permissions_enforced_since`, each of the four flags that is `false` becomes `true` and `permissions_enforced_since = "1.68.0"` is added; each reset writes the audit event `permission_flags_reset`. A `false` written afterwards on this page or in the Advanced config editor counts as an operator decision. Ephemeral role members (`agents/.ephemeral/`) are not migrated and keep least privilege.

## Manage → Members: direct manager and org chart (Enterprise, 1.76.0)

Admins only. The Personal edition has no such page.

| Control | Writes | Effect |
|---|---|---|
| Create / edit member → Direct manager | `manager_user_id` on `users.create` / `users.update` (choosing "None" clears it) | The next message and the next dashboard action follow the new reporting line. The list offers only active people whose role is Manager or Admin, minus the person and everyone below them; the gateway checks again and, when it refuses, says why in plain words: the line would loop, it would go past 8 levels, the manager's role is too low, or (when you change someone with reports to Employee) they still have direct reports. |
| List → Role column | — | Shows "Manager: …". |
| List / Org chart | — | Display only. The org chart draws the reporting lines from the direct manager, with each person's bound AI staff next to them; people with no manager and no reports are listed under "No manager set". Offboarded people are left out; suspended people are shown and marked. Click a name to edit. |

What a manager gets through their reports: see [Calling AI staff with @](../features/62-calling-ai-staff.md#managers-reach-their-teams-employees).

## Organization → My team (Enterprise, 1.76.0)

Only people whose role is Manager or Admin see it (sidebar group "Organization"). The page lists the people who report to you (down to 8 levels; suspended people included, offboarded people not) and the AI staff you can reach through each of them:

- Tag "Can assign work" or "Chat only": when the report's binding is Owner or Operator you get Operator and may assign work; when it is Viewer you may only chat.
- "N open tasks": tasks assigned to that employee that are not done, cancelled or failed.
- **Chat** opens WebChat with `@name ` already typed. Disabled while the employee is not active.
- **Assign** asks what should be done (and, optionally, how you will know it is done), creates a goal task (`tasks.create` with `kind = "goal"`). You stay on the page with an "Assigned to …" confirmation; a manager cannot open a report's task itself yet.

The data comes from the RPC `users.my_team`, computed with the same rules every call and assignment uses, so everything listed can be reached. It is refused in the Personal edition and for the Employee role.

## Employee page → Data sources (v1.76.0)

Visible to everyone bound to the employee (Viewer and up); adding and removing files needs the employee's Owner binding or Admin, re-read from the account database on every call, and on top of that the editor or owner role on the employee's own collection.

| Control | Writes | Effect |
|---|---|---|
| Add files (first time): sensitivity, "Also let the colleagues who work with this employee now read them" | a new collection `<display name> 的資料` in `knowledge.db`, its grants, and the employee's `[knowledge] collections` when that list exists | Immediately. Grants: the creator (owner), the employee (viewer), and when ticked every active account bound to the employee (Owner → owner, Operator / Viewer → viewer). |
| Add files | a source in the own collection | Read in the background; the list refreshes every 3 seconds while a file is still being read. |
| Remove (optionally "also forget") | deletes the source; with the box ticked files forget requests for an Admin | Unsearchable at once; forget requests wait for approval. |
| Manage in the document library | — | Opens the collection (sharing, test search, access record, settings). |

Archiving or deleting the employee archives its own collection (only people owners keep access); restoring the employee puts back the grants archiving removed. The first upload also adds the collection to an existing `[knowledge] collections` list; later uploads leave that list alone. Details: [Document knowledge base](../features/63-knowledge-base.md#using-the-dashboard).

## Employee page → Connections (v1.76.0)

Everyone bound to the employee (Viewer and up) sees which Google account it uses and whether that works right now (`agents.google_identity_get`). Changing it and **Test connection** are Admin only.

| Control | Writes | Effect |
|---|---|---|
| Change → Company's shared account / Act as a colleague (domain delegation) / Do not use | `agent.toml [google] identity` = `home` / `delegate:<email>` / `none` (`agents.google_identity_set`) | Next Google tool call. `none` hides the 19 Google tools from the employee (tools list changes). The email must be in an allowed domain. |
| "If the colleague's account cannot be used, use the company's shared account" | `[google] fallback = "home"` (unticked: the key is left out) | Covers only: no key imported, colleague suspended or offboarded, Google rejected or unreachable. A refused requester, a setting error or a protection problem never falls back. |
| Test connection | nothing | Mints for the colleague (full list, then per service), checks Gmail reports the same address. Sends nothing. |

`[google] requesters` (`bound_people` default, `owner_only`, `anyone`) has no control on this page yet; set it with `agents.google_identity_set` (`anyone` needs `confirm_anyone: true`) or the Advanced config editor (Admin). A non-admin Owner editing `agent.toml` may only narrow `[google]` (switch to `none`, drop `fallback`, tighten `requesters`). Details: [Per-employee Google accounts](google-workspace.md#per-employee-google-accounts-company-delegation-v1760).

## Integrations → Google → Company delegation (v1.76.0)

Admin only, both editions.

| Control | Writes | Effect |
|---|---|---|
| Import key file / Replace key file | `<home>/protected/google-service-account.sealed`, sealed with the credential-protection key (`google.delegation.import_key`) | Immediately. Refused until Security → Credential protection is in the system keychain. The key never comes back to the browser; the panel shows the service account, client id, scope list and Admin console path. |
| Remove | deletes the sealed key (`google.delegation.remove_key`, confirmation) | Every employee acting as a colleague stops using Google (or falls back, where allowed). |
| Allowed domains | `config.toml [integrations.google_identity] allowed_subject_domains` (`google.delegation.settings_set`) | Next call. Exact domain match. Empty: only the domain of `[integrations.google_service_account] subject`, or nobody. |
| (no control) `default_identity` | `[integrations.google_identity] default_identity` = `home` (default) / `none` | What employees without `[google]` use. |

The list at the bottom shows each employee acting as a colleague and why it is blocked, if it is.

## Settings → Advanced → Knowledge base search

Admin only (new in 1.76.0). Shows whether the document knowledge base searches by meaning or by keywords only, and why not when it does not, with the share of passages that already have vectors.

| Control | Writes | Effect |
|---|---|---|
| Where it is computed: On this computer / OpenAI / Google Gemini / Off | `[knowledge.embedding] provider` (+ `cloud_model`, `cloud_account` for cloud) | Next search. Choosing a cloud provider opens a dialog naming the vendor and host that receive passages; **Use** stays disabled until the box is ticked, and the acknowledgement is stored as `cloud_disclosure_ack`. The dialog warns when redaction is off (nothing is sent then). Choosing anything else removes the acknowledgement. |
| Install (size) | downloads into `models/embedding/<model>/` and `lib/onnxruntime/` | Background download with progress, resumable after **Cancel**; vectors for every passage are queued when it finishes. |
| Check again (offline install) | — | Checks the signature of the offline bundle (`.tar` + `.tar.minisig`, still packed, in `models/embedding/`) against the release key, then unpacks it, hashes every file and marks the model installed. |
| Use this model | `local_model` | Switches the local model; passages get vectors for it in the background (vectors of the previous model stay but are not compared). |
| Remove | deletes the model files | Search becomes keyword-only with that reason shown; the runtime library stays. |
| Fill in missing search data | — | Queues vectors for every source still missing them. |

Details: [Document knowledge base](../features/63-knowledge-base.md#embedding-models).

## Profile files (v1.76.0)

RPCs `agent_files.list / get / diff / history / save / rollback / withdraw / pending_get`; also used by the boundaries editor on the employee edit page (`contract.update`). Files: persona (`SOUL.md`), self-introduction (`IDENTITY.md`), work rules (`INSTRUCTIONS.md`), boundaries (`CONTRACT.toml`), basic settings (`agent.toml`, read-only and masked here; edit it on the edit page or, as an admin, in the Advanced config editor), and the employee's own skills (existing `.md` files under `SKILLS/`, file key `skill:<path>`, for example `skill:quote/SKILL.md`). Skills can only be edited here, not created or deleted (use the custom skill flow to add one), and scripts next to a skill are never touched; every path level must be a single name without `..` or invisible characters (zero-width, direction controls), at most four levels deep; symlinks and hard links are refused, and a save checks under the file lock that no folder was swapped before it writes (the lock file lives outside the employee's folder). Other writers of skill files (custom skills, the employee's own skill tools, skill graduation) do not go through here, keep no versions and do not take this lock. Limits: 64 KB per Markdown or skill file, 32 KB for `CONTRACT.toml`, at most 100 `must_not` and 100 `must_always` entries of up to 500 characters.

- **Profile files tab:** the 設定檔 (Profile files) tab on the employee detail page (`/agents/<employee>/files`). On the left, a plain-language list with the real file name in small grey text and the badges "Waiting for review", "Changed outside the dashboard" and "View only". On the right the file is shown read-only; its owner and admins press Edit to change it, with the size and limit below and the buttons Show changes, Save and check, and Discard changes. The check result uses the same review dialog as the boundaries editor. If someone else saved first, your text and the current text are shown side by side with Copy my content and Reload. Version history (Operator and up) lists time, who and where each version came from, and can view, compare with the current text or restore. An unsaved edit asks before switching file, switching tab or closing the page (leaving through the sidebar or other links does not ask yet). People who can only view see "You can view this. Only this staff member's owner or an administrator can change it."
- **Work rules and AGENTS.md / Agent.md:** the list and the heading read "Work rules (AGENTS.md / Agent.md)" because many people look for it under the industry name; the real file name `INSTRUCTIONS.md` stays in the small grey text. The import merges AGENTS.md, CLAUDE.md, GEMINI.md, Agent.md and copilot-instructions into this file, and it is given to the employee on every conversation whichever runtime it uses. When a new employee has no such file, the tab shows "Create work rules" (the button is only visible to the owner and admins); it opens a short template and nothing is written until Save and check, through the same save, check and approval path (no separate write path).
- **Files generated for the runtime (read-only):** a collapsed block at the bottom of the tab, read the first time it is opened. It shows the `CLAUDE.md` and `AGENTS.md` the system generated for the employee's runtime (RPC `agent_files.runtime_files`, Viewer and up; only these two fixed names, the employee folder checks are reused, symlinks and non-regular files are never read, at most 64 KB per file). They cannot be edited here; change the rules in Work rules.
- **Search:** typing `agent.md`, `agents.md`, `CLAUDE.md` or "work rules" in the command palette (Cmd/Ctrl+K) lists one row "<employee> · Profile files" per employee that opens that employee's profile files tab.
- **Who:** anyone bound to the employee reads the files; Operator and up read versions and diffs; the employee's Owner and admins save. Every call re-reads the account database, not the session. Admins decide approval requests.
- **Check on every save:** fixed rules first (size, control characters, a strict `CONTRACT.toml` parse, hidden or invisible content, injection patterns in the lines this change adds or in new `must_always` entries, key-like text, a persona that loses most of its text, a contract that drops or rewrites a `must_not` / `must_always` entry or raises / removes the tool-call limit; skills additionally: a leading `---` block must read the way the skill loader reads it and its `name` must be a single name, and the skill scanner's findings on the lines this change adds count, with commands, sending data out and injection patterns as high risk (decided by the actual added lines, so a second line of the same kind is still new) and bare links and mentions of a forbidden item as needs attention; high-risk content already in the file is listed as "already in this skill"; a skill changed outside the dashboard since its last save here is checked as a whole against that last saved version), then a short review of the diff by the deployment's utility model (`[runtime] utility_provider` / `utility_model`, override with `[agent_files] review_provider` / `review_model`; billed to the deployment as `agent-file-review`, never to the employee's budget). The model can only make the result stricter. Results: `pass` applies at once; `warn` needs a reason (at least 4 characters) or a request to an admin; `block` can only be sent to an admin by a non-admin, and an admin applies it only with a reason and "I understand the risk" (audited as Critical). If the review cannot finish (no utility model, timeout, an unusable answer, a quota reached) or the change was too long to show in full, the result is at least `warn` (or `block` with `[agent_files] on_analysis_failure = "block"`), a non-admin can only send it to an admin, and an admin confirms it only with "I understand the risk"; it is never a silent pass and cannot be switched off. Quotas per day: 200 per deployment, 40 per person, 60 per employee (`review_daily_limit`, `review_daily_limit_per_user`, `review_daily_limit_per_agent`), plus at most 10 checks per person per minute (`checks_per_minute`); the same change checked again within 15 minutes reuses the result. Confirming does not run the review again; a confirmation must match the same person, file, starting version and content within 15 minutes, the fixed rules are re-run at the person's current permission level (stricter now ⇒ check again), and confirming a `block` re-checks inside the file lock that the person is still an admin. A SOUL.md or INSTRUCTIONS.md changed outside the dashboard is checked as a whole and compared with the last approved version (reason "changed outside").
- **Saving:** under the file lock, with the hash the editor read (`base_hash`); a file changed by someone else in between is refused as a conflict. The new text takes effect from the employee's next turn; channels get a one-line notice with the next reply.
- **Versions:** kept outside the employee folder in `<home>/agent_file_history/<employee>/<file>/` (owner-only files), 20 per file by default (`[agent_files] history_keep`, 5–100). The first change also keeps what the file contained before. Restoring a version runs the same check. Removing an employee moves its history to `agent_file_history/_trash/`; a new employee with the same id starts with none.
- **Baselines:** saving SOUL.md or INSTRUCTIONS.md updates its approved fingerprint (INSTRUCTIONS.md has one now too, checked by the heartbeat as `_instructions_integrity_drift`). A file changed outside the system shows as changed-outside; restore the approved version or check and accept the current content (save it as it is: the whole file is checked, at least a reason or an approval request is needed, and once confirmed the file is not rewritten; the current text is kept as a version and becomes the approved baseline. Saving an unchanged file that was not changed outside is still "unchanged"). Renaming an employee updates the name inside SOUL.md / IDENTITY.md and keeps an approved SOUL approved, while a SOUL that had already changed outside stays flagged. The name must be plain text (1–64 characters, no line breaks, invisible characters or `< > [ ] { }` / backticks), and a renamed file that trips a fixed rule is left as it was (listed in `identity_files_skipped`).
- **Approval requests:** kind `agent_file_change`, decided only in the dashboard by a current admin, 3 days to decide (expiry counts as a denial). Channels only get a notice without buttons. The proposed text is stored in `<home>/agent_file_pending/` until the request ends. Approving applies first; if the file changed while the request waited, the request is denied and the requester has to send it again.
- **Audit:** `agent_file_changed`, `agent_file_change_refused`, `agent_file_review_failed`, `agent_file_change_requested` (hashes only, never the text).

## 交辦任務 page → When, and Goals → Assigned by me → Scheduled (v1.76.0)

| Control | Writes | Effect |
|---|---|---|
| 交辦任務 page → **Complex task** → **When**: now / once at a date and time / every day / weekdays / weekly on chosen days / every hour / custom cron (Admins) | a row in `tasks.db` `assign_schedules` (through `tasks.goal_create` with `schedule` and `schedule_timezone` = the browser's zone) | Nothing is assigned now. At each due time the gateway creates the goal on your behalf, after re-reading your account; no right left means the run is skipped and the schedule paused. |
| Goals → Assigned by me → Scheduled: **Pause / Resume / Remove** | the row's `status` | At once. Resume re-checks the creator's right and continues from the next slot after now; a one-time schedule whose time passed cannot resume. Remove keeps the row as `removed` (never listed or run again). |
| **Show everyone's** (Admins) | — | Lists every person's schedules with the creator's name. |

Caps live in `config.toml` (no control yet; Advanced config editor): `[goal_defaults] assign_schedule_max_active` (default 20 per person, 0..200) and `assign_schedule_min_interval_minutes` (non-Admins, default 60, can only be raised). Existing schedules also recheck the interval at every run and resume. Pause/remove prevents a run whose task has not yet been stored; stop an already stored task separately. Details: [Scheduling an assignment](../features/67-assigning-tasks.md#scheduling-an-assignment).

## System → Updates (v1.76.2)

The page shows one update order for the whole install. When the desktop app is attached to a gateway it does not own (a launchd/systemd/Enterprise gateway, or a remote one), the page lists both components: the gateway with its current and next version, and the desktop app with its current and downloaded version. One button updates the gateway first, waits for it to come back, then updates the app. The app is only updated up to the gateway's version; a newer app download waits and the page says so. An app that runs its own gateway sidecar updates both together, as before.

When a newer gateway binary is already on disk but not running (an npm install, a manual swap, an update whose restart never happened), the button reads "Restart the gateway to apply v…" and calls `system.restart_pending_update` (admin). The gateway refuses it unless the on-disk version really differs from the running one, and refuses it inside a container or for the desktop sidecar, which restart through their owner. Restarting the desktop app does not restart an external gateway.

## Thinking display

No dedicated control yet; set these in the Advanced config editor (below). Both are read on every turn, so a change applies from the next reply.

| File | Key | Values |
|---|---|---|
| `config.toml` | `[thinking] display` | `summary` (default: timer, tokens and masked thinking text in WebChat), `status` (timer and tokens only), `off` (no thinking frames; WebChat still shows its own timer) |
| `config.toml` | `[thinking] max_chars` | thinking text per turn, default 4000, clamped to 200–20000 |
| `config.toml` | `[thinking] anthropic_display` | `summarized` (default) or `updates` (beta, only some Claude models) |
| `agent.toml` | `[model] show_thinking` | `summary` / `status` / `off`; unset follows `[thinking] display`. The stricter of the two wins |

A `config.toml` that cannot be read or parsed, or an unknown `display` value, counts as `status`. Thinking text is shown to signed-in dashboard users in WebChat and, in messaging apps, only to senders allowed by `[thinking] channel_audience` (see the Channels section). Details: [Thinking display](../features/64-thinking-display.md).

## Advanced config editor

Location: Settings → Advanced → Advanced config editor. Admins only; RPCs `config.raw.get` / `config.raw.set`.

- **Files:** system settings (`config.toml`), inference settings (`inference.toml`) and each AI employee's `agent.toml` (a real directory under `agents/`, no symbolic links).
- **Secret masking:** values whose key ends in `_enc`, is `key` or ends in `_key`, or contains token, secret, password, passwd, api_key, apikey, widget_key, private_key, credential or service_account_json, every value in `headers`, `otlp_headers` and `env` tables, and URLs carrying a password are shown as `«set»`. Leaving `«set»` in place keeps the stored value; `«set»` where nothing is stored is refused. `[mcp_keys]` is not shown at all.
- **Validation:** TOML syntax first, with the line and column of an error; then the typed check (the section validators for `config.toml`, `InferenceConfig`, `AgentConfig`). Nothing is written on failure. An employee's `[agent] name` cannot be changed here.
- **Document knowledge base:** collections, documents, sharing and the access record are managed under Company → Document library in the sidebar; the global `[knowledge]` limits have no page yet, and their keys (`enabled`, `max_source_bytes`, `max_source_chars`, `max_sources_per_collection`, `max_collections`, `max_chunks_total`, `extract_timeout_secs`, `extract_memory_mb`, `access_log_retention_days`, `max_collection_bytes`, `max_total_bytes`, `extract_when_unavailable` = `refuse` / `run_unsandboxed`, `auto_retrieve`, `inject_budget_chars_max`) are type-checked here. So are the `[knowledge.embedding]` keys (`provider` = `local` / `openai` / `gemini` / `none`, `local_model`, `cloud_model`, `cloud_account`, `cloud_base_url`, `allow_private_endpoint`, `batch_size`, `threads`, `max_vector_rows`, `idle_unload_minutes`); the disclosure record `cloud_disclosure_ack` cannot be written here (the save is refused) and is set only by the disclosure dialog of Knowledge base search. An employee's `[knowledge] collections` is set under Agents → Edit Settings → Tools & permissions → Document library (only collections shared with it can be picked), or directly in that employee's `agent.toml`, which also holds the reply keys `auto_retrieve`, `answer_mode` = `assist` / `kb_only`, `top_k` and `inject_budget_chars`. See [the knowledge base page](../features/63-knowledge-base.md#settings-configtoml).
- **Conflicts:** opening a file returns a hash of its content, which the save sends back. If the file changed in between, the save is refused; reload and edit again.
- **Backup:** before writing, the file is copied to `<file>.bak-<unix time>` (mode 0600); the newest five are kept.
- **Audit:** every write records `config_raw_edited`, at Warning level when it touches `[delegation]`, `[acp]`, or an employee's `[agent]`, `[capabilities]`, `sandbox_enabled`, `network_access` or `can_modify_own_soul`.
- **Restart list:** after saving, the editor lists what needs a restart. In `config.toml`, `[gateway]`, `[server]`, `[telemetry]`, `[logging]`, `[channels]`, `[wiki]`, `[relay]` and `[decision]` are read only at boot; `general.name`, `rotation.health_check_interval_seconds`, `goal_loop.resume_on_restart` and `knowledge.max_source_bytes` (it sizes the upload route's body limit) also need a restart; the log level, live data feeds and redaction are not listed when they could be applied live. Saving `inference.toml` always resets the inference engine immediately. For an employee file only `heartbeat.max_concurrent_runs` needs a restart.

## Maintenance SSH dashboard (v1.75.0)

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
the page works only with an OS image that ships the maintenance account, and it is not acceptance of the assembled device.

## Security → Credential protection (v1.76.0)

The **Credential protection** card on the Security page (Admin, both editions) shows one
line: "Credential protection: Files" or "Credential protection: System keychain (…)", followed by what that
means. It covers only credentials that let an AI employee act as a specific
person; every other setting keeps working the same way whatever the card
says.

- **Move to system keychain** (`security.credential_protection_enable`)
  creates the protected key in the macOS keychain or Windows Credential
  Manager. Linux and Docker show "this platform has no supported system keychain" and no button.
- **Needs re-authorization** appears on macOS after an upgrade or while the
  keychain is locked. Run `duduclaw credentials authorize` in Terminal on that
  Mac and allow the dialog once.
- **Protection key missing / replaced / settings error** stop personal
  accounts. **Re-enable** (`reset: true`) makes a new key; saved personal
  credentials stop working and people reconnect.
- **Replace key** (also `reset: true`) is offered while everything looks fine,
  for when the key and its record may both have been swapped; it has the same
  effect as Re-enable. On a macOS build without hardened runtime the line says
  "readable by programs under the same user account", because code injected
  into DuDuClaw could read the key.
- **Turn off** (`security.credential_protection_disable`, `confirm: true`,
  typed confirmation) deletes the key; saved personal credentials stop
  working.

`duduclaw credentials status` (add `--json` for scripts) prints the same
state from the terminal. Details: [Security defense](../features/05-security-defense.md#person-credentials-in-the-os-keychain-v1760).

Direct-binding history privacy: `[people] bound_history_privacy = true` is enabled by default (missing/invalid values stay restricted). Admins may explicitly set `false` in the advanced editor for migration; this restores old broad Viewer/Operator history. Owner/Admin and chat/call permissions are unchanged. See feature 68 for search/export/attachment scope and NER limits.
