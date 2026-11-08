# Extension gates and independent entry points

Since v1.80.0, the gateway reserves core RPC handling, validates extension HTTP registration and closes managed third-party MCP transport gaps. These are product entry-point controls. A native `GatewayExtension` still runs inside the gateway process; it is trusted code, not a sandboxed plugin. A feature-family switch cannot be enforced merely by hiding dashboard navigation or intercepting extension RPC calls.

This inventory records the entry points reviewed for the Odoo Live Chat and voice families, plus the shared extension/MCP boundaries. It includes related Odoo events and meeting transcription because they reach the same external systems or audio providers independently. “Outside the extension gate” means the call does not traverse that gate; it does **not** mean the call is unauthenticated. Existing guards are listed separately. Source locations are a review snapshot for this change and should be refreshed when moving these functions.

## Registration and migration contract

### RPC

Core dispatch runs first. An extension declares a stable `rpc_methods()` list under `ext.<extension-id>.<method>`; undeclared calls never reach `handle_method`. Invalid, duplicate or core-name declarations disable that extension's RPC registration and produce a startup warning. The allowlist is captured during registration, so changing the declaration after startup cannot expand it. Core role, edition and credential checks continue to run at their existing locations.

Extension authors must rename formerly unscoped RPC methods and update their clients. Returning `Some` for a core method from an extension handler cannot override the core response, including an authorization refusal.

### HTTP

Extensions declare individual `http_routes()` entries, each carrying a literal path, method router and mandatory `minimum_role` (`UserRole`). The path must be below `/ext/<extension-id>/`; captures, wildcards, encoded traversal and noncanonical paths are refused. Core routes own all other prefixes. Registration validates the declarations before merging the router.

Every extension route requires live bearer authentication, password-state checks and the declared minimum role before its handler runs. `ExtensionRoute::new(path, methods)` defaults to Admin; choosing Manager or Employee requires an explicit `minimum_role` declaration. The core re-reads the user's role on every request, so a stale Admin JWT cannot retain access after demotion. The outer gate also checks origin and enforces a hard 2 MiB request-body limit. There is no Public mode or extension webhook exemption. An extension cannot remove that outer gate with its own inner middleware; resource and business authorization remain the handler's responsibility.

Opaque `extra_routes()` routers cannot be inspected safely and are refused at startup. Existing wrappers must migrate to the declared route API before upgrading. These checks do not modify or validate any out-of-tree wrapper.

### Third-party MCP

Managed stdio servers go through `duduclaw mcp-proxy`; managed remote connections use `mcp-remote-bridge`. `[[mcp.external]]` stdio and Streamable HTTP mounts use the in-process registry boundary backed by the same `ThirdPartyGate`, preserving annotations (including scoped Google mounts) and enforcing the gate for both discovery and calls. Utility and explore-lane callers retain the existing policy of not mounting `[[mcp.external]]` servers; the registry boundary also retains lane-aware checks as defense in depth. All retain the configured tool-name restrictions and approval requirements. `destructiveHint` can raise the effect from Modify to Delete; upstream `readOnlyHint` never lowers it. The legacy `trusted_read_hint_servers` setting no longer grants a read downgrade. No host-owned read exception is implemented, so third-party tools remain unavailable in Explore.

Known direct remote URL/type entries in an employee's `.mcp.json` are removed instead of being passed unguarded to the runtime. With the third-party gate enabled, unclassifiable entries (including missing/non-string commands and unknown or conflicting transport shapes) refuse the entire spawn. Gated stdio definitions accept only `command`, string-array `args`, string-map `env` and optional `type = "stdio"`; remove unsupported `cwd`, `shell` or other transport fields before retrying. Operators should establish a managed remote MCP connection using `mcp.remote_connect`; credentials are not silently migrated. This is separate from `[[mcp.external]]` HTTP mounts, which remain supported through the registry gate.

**Codex external configuration gate (since v1.80.0):** Employees, Explore, utility, Task Sandbox and Discovery may run when external layers are absent or define no MCP servers; detected MCP or unreadable/unparseable existing layers refuse launch with the source named. A refused explicit Codex judge fails acceptance without retrying the default judge. This checks a local/cache snapshot before launch, not complete isolation from subsequent cloud refresh. 

Codex checks the currently present external sources before using its gateway-owned `CODEX_HOME` and proxied MCP configuration:

- Unix: `/etc/codex/config.toml`, `/etc/codex/requirements.toml`, `/etc/codex/managed_config.toml`.
- Windows: the ProgramData Known Folder `OpenAI/Codex/config.toml` and `requirements.toml`, including the 64-bit `HKLM\SOFTWARE\Policies\OpenAI\Codex` `RequirementsPath` override; the non-Unix managed layer under the original Codex home.
- macOS: forced CFPreferences for `com.openai.codex`, reading both `config_toml_base64` and `requirements_toml_base64`; ordinary `defaults` output is not evidence of the effective MDM policy.
- Documented profile files `<CODEX_HOME>/<name>.config.toml` and project `.codex/config.toml` files along the working directory's ancestors. A successfully written gateway-generated proxy configuration is exempt from the unmanaged-layer check.
- Cached enterprise bundles `<CODEX_HOME>/cloud-config-bundle-cache.json` and `managed-config-*.json`, including their embedded TOML fragments.

Missing sources and parsed layers without MCP definitions permit launch. Detected MCP definitions, unreadable existing sources, and malformed source/payload data refuse launch and name the file or managed source. Containers check their own sources before starting Codex. Empty `-c mcp_servers={}` alone cannot establish isolation.

**Limit:** this preflight examines the local files, forced preferences and enterprise cache snapshot present at inspection time. It does not freeze those sources or prevent a subsequent Codex remote cloud refresh. It is a detect-then-refuse control, not a guarantee of complete native-MCP isolation or live cloud-policy verification. Same-user replacement remains outside this boundary.

The private Codex home links the original `auth.json` (a symlink on Unix, a hard link on non-Unix) so the upstream 0.166 CLI's truncate-in-place token refresh updates the shared authentication store. This preserves file-backed login continuity; it does not add a separate credential-isolation boundary. Keyring-only authentication and native Windows execution have not been verified for this follow-up.


Managed Claude spawns always use an owner-only temporary configuration with `--strict-mcp-config`, including built-in-only configurations, so ambient MCP discovery cannot add servers. Managed Grok spawns refuse unmanaged project MCP entries and any global MCP entries under the actual `HOME` or `GROK_HOME`; unreadable or malformed configuration is also refused. The effective Grok configuration root must be absolute: correct a relative `HOME` or `GROK_HOME` before retrying. The files are preserved and operators must move server definitions into the employee MCP configuration. Native CLI tools, standalone operator CLI runs, same-user file replacement and arbitrary Bash execution remain outside this gate. Annotation classification is a policy input, not proof that an upstream implementation behaves as advertised.

## Shared entry-point map

Paths in the next tables are relative to the repository root. `G` means `crates/duduclaw-gateway/src/`; `C` means `crates/duduclaw-cli/src/`; `A` means `crates/duduclaw-agent/src/`. Each location is written as `file:line` with the owning function or dispatch branch.

| Surface | Source location | Boundary / remaining responsibility |
|---|---|---|
| RPC registration and dispatch | `G/handlers/lifecycle.rs:43` (`with_extension`); `G/handlers/mod.rs:424` (`handle`) | Core dispatch precedes declared extension fallback. An RPC-only check does not cover HTTP, background tasks or CLI paths. |
| Extension update provider | `G/extension.rs:72` (`update_provider`); `G/server.rs:2320` | Native extension code can supply an update source and signature key independently of RPC/HTTP registration. The existing signed updater contract remains a separate trust boundary. |
| Extension API | `G/extension.rs:23` (`GatewayExtension`) | Native code runs in-process. Declaration validation constrains routing, not arbitrary Rust code. |
| HTTP construction | `G/extension/http.rs:48` (`merge_routes`); `G/server.rs:3274` (extension route registration) | Declared extension routes receive shared outer gates; core HTTP handlers continue using their existing gates. |
| Claude spawn artifact | `A/mcp_template.rs:545` (`prepare_mcp_config_for_spawn`); `G/redaction_proxy.rs:217` (mandatory tool gate) | The first gate verifies the host MCP configuration; the runtime rewrite proxies managed third-party stdio entries and refuses unsupported direct remote transports. The temporary configuration is always passed with strict discovery, even when it contains no third-party server. |
| Grok additional configuration sources | `G/runtime/grok/third_party.rs:182` (`refuse_unmanaged`), `:203` (`refuse_global_sources`); `G/runtime/grok.rs:545` | Managed spawns refuse unmanaged project entries and global MCP definitions before starting the CLI. The effective configuration root must be absolute. Project entries are checked again while holding the writer lock; existing MCP tables, including built-in entries, are rewritten to the managed canonical form. A failed write in a borrowed work root refuses the spawn; existing files are not removed. |
| Runtime third-party registration | `G/runtime/third_party_mcp.rs:185` (spawn gate invocation) | Codex/Grok/tool-loop integration must preserve the same managed-server boundary. |
| External mounts | `G/mcp_external.rs:297` (`load_external_mcp_servers_resolved`) | Resolves stdio/HTTP/credentials; tool registration must also attach the `ThirdPartyGate` boundary. Credential resolution alone is not tool authorization. |
| Shared tool gate | `G/third_party_tools.rs:214` (`ThirdPartyGate`); `G/egress_registry.rs:15` (registry boundary) | Annotation/effect/name/approval decisions apply at list and call time. |
| Remote stdio bridge | `G/remote_mcp/bridge.rs:675` (gate construction) | Remote transport uses the same policy gate; OAuth authorization is an additional, independent boundary. |
| Proxy CLI | `C/mcp_proxy.rs:963` (gate construction) | Proxy commands must retain verified employee identity and host policy; direct operator execution and native tools are separate trust paths. |

## Odoo Live Chat inventory

| Surface | Source location | Existing guards and why a family gate must cover it |
|---|---|---|
| Dashboard RPC read/status | `G/handlers/dispatch_system.rs:503`; `G/handlers/odoo_livechat_rpc.rs:31` | `odoo.livechat.get/status` are Admin-only core RPCs. Core-first dispatch protects them from extension shadowing, but does not disable the family. |
| Dashboard RPC configure/test/resume | `G/handlers/dispatch_system.rs:511`; `G/handlers/odoo_livechat_rpc.rs:51`, `:161`, `:184` | Admin-only. Set validates/encrypts/commits configuration; test can connect to Odoo; resume changes takeover state. Each remains a direct core path. |
| Raw configuration RPC | `G/handlers/dispatch_system.rs:175` (`config.raw.get/set`) | Admin configuration route is independent of Live Chat's settings page; removing only that page does not prevent configuration changes. |
| Gateway boot | `G/server.rs:1527` (`odoo_livechat::spawn`) | The worker is always spawned so enablement can change without restart. A family gate at RPC registration cannot prevent its startup. |
| Poll-loop admission | `G/odoo_livechat/worker.rs:70` (`run`) | Every loop checks Live Chat `enabled`, configured Odoo connection and gateway instance ownership. No shared extension/family gate is consulted. |
| Connection and child tasks | `G/odoo_livechat/worker.rs:217` (`connect`), `:250`, `:265` | Bot/admin credentials are resolved separately; connections start schema scans. A family stop needs to account for these child tasks too. |
| Presence writes | `G/odoo_livechat/worker.rs:284` (`heartbeat`) | Keeps the bot online through Odoo bus/mail APIs; may use an admin connection for Odoo 19. This is an external write outside MCP approval dispatch. |
| Poll and per-session spawn | `G/odoo_livechat/worker.rs:344` (`poll_once`), `:487` | Filters allowed channels, tracks takeover/cursors and bounds concurrent turns. Already-spawned turns carry their config snapshot and do not re-enter registration. |
| Greetings/handoff/replies | `G/odoo_livechat/turn.rs:41`, `:76`, `:105`, `:124`, `:137` | Fixed greeting and handoff text can post directly; generated replies use `build_guarded_reply_for_agent` and recheck operator takeover before posting. A family gate must cover the final sends, not just AI generation. |
| Shutdown/offline cleanup | `G/server.rs:3405`; `G/odoo_livechat/mod.rs:187` | Best-effort marks the bot offline. Cleanup is an independent lifecycle action that a disabled family may still need to permit. |
| Related Odoo HTTP webhook | `G/odoo_events.rs:228` (`router`), `:260` (`decide_webhook`); `G/server.rs:3240` | `/webhook/odoo` is a core route, controlled by its webhook enablement/shared secret. It belongs to Odoo events rather than Live Chat and is not an extension route. |
| Related Odoo event poller | `G/server.rs:2134`; `G/odoo_events.rs:334` (`spawn_odoo_poller`) | Separate background integration; stopping Live Chat alone does not stop Odoo event polling. |
| Related native Odoo MCP | `C/mcp/odoo.rs:3` (`handle_odoo_tool`), `:74` (model feature gate) | Native Odoo tools use MCP authorization, per-agent connections and Odoo model feature switches. There is no dedicated Live Chat MCP tool in this snapshot; do not treat the generic Odoo tool family as controlled by Live Chat enablement. |
| Operator CLI | `C/lib.rs:4120` (gateway entry), `:5458` (decision Odoo export dispatch) | No dedicated `odoo livechat` CLI subcommand was found. Starting the gateway activates the boot path; related Odoo export is a separate CLI path. Direct operator file editing is outside RPC registration. |

No dedicated inbound Live Chat HTTP route was found: visitor messages arrive through the Odoo polling path. The Odoo event webhook above must not be mistaken for that transport.

## Voice and audio inventory

| Surface | Source location | Existing guards and why a family gate must cover it |
|---|---|---|
| Dashboard STT HTTP | `G/server.rs:3049` (route), `:15689` (`handle_stt`) | Bearer authentication, upload cap and provider configuration checks; unconfigured STT returns 501. Direct core HTTP path, independent of extension RPC. |
| Dashboard TTS HTTP | `G/server.rs:3104` (route), `:15848` (`handle_tts`) | Live caller lookup, per-user rate limit, text/concurrency limits and provider availability. Provider synthesis does not dispatch through MCP. |
| Voice configuration HTTP | `G/server.rs:15961`, `:16023` | GET requires bearer; POST requires Admin and validates/encrypts STT settings. It is a core route outside extension route registration. |
| Voice configuration RPC | `G/handlers/dispatch_system.rs:165`; `G/handlers/system_update_config.rs:890` | Admin `system.config/update_config` reads/writes TTS preferences independently of STT HTTP configuration. Raw config RPC remains another configuration path. |
| Kokoro model lifecycle RPC | `G/handlers/dispatch_core.rs:297` (`voice.kokoro_status/install`) | Admin-only; installation can start background work. Hiding synthesis tools does not remove model lifecycle operations. |
| Telegram inbound audio | `G/telegram.rs:1063`, `:1088`, `:2830` | Channel processing downloads audio and calls shared channel STT. The audio conversion is outside extension/MCP dispatch. |
| Telegram outbound voice | `G/telegram.rs:1612`, `:1623` | Channel reply flow creates `TtsRouter` and synthesizes directly, subject to channel/provider settings. |
| LINE inbound audio | `G/line.rs:840` | Channel ingress calls shared STT directly; its channel guards are distinct from a feature-family gate. |
| Shared channel STT fallback | `G/stt.rs:457` (`transcribe_channel_audio`) | Explicit provider settings are honored; missing settings can fall back to the Whisper API environment key. A future family-off state cannot be represented merely by removing STT configuration. |
| Native audio MCP tools | `C/mcp/dispatch.rs:406`, `:411`; `C/mcp/media_tools.rs:3`, `:43`; `C/mcp_auth/scope.rs:316` | `transcribe_audio`/`synthesize_speech` use native MCP name/scope/effect/egress gates. Transcription directly uses Whisper API; synthesis creates the home TTS router. They are not third-party tools and do not traverse `ThirdPartyGate`. |
| Meeting MCP tools | `C/mcp/dispatch.rs:408`; `C/mcp/meeting_tools.rs:26` | Meeting creation/status/cancel/upload links use employee and transcription policies with their own signed gateway path. They are a separate audio admission surface. |
| Meeting HTTP uploads/jobs | `G/meeting_uploads/http.rs:11`; `G/meeting_jobs/http.rs:18` | Upload/status/cancel/download and internal job routes have their own caller/ownership/signature checks. They do not go through extension HTTP registration. |
| Meeting boot and worker | `G/server.rs:3149`; `G/meeting_jobs/mod.rs:142`, `:190`, `:236` | Primary-gateway ownership, job recovery and policy checks govern execution; the worker calls transcription independently and checks before chunks. |
| Meeting provider execution | `G/meeting_transcription.rs:33` (`execute`) | Shared provider/file/redaction pipeline. Both background jobs and the operator CLI can reach it; a gate only on job creation misses the operator path. |
| Operator meeting CLI | `C/lib.rs:4391`; `C/meeting_cmd.rs:28`, `:47` | `meeting transcribe` refuses AI-session markers and calls transcription directly. It is not an extension RPC or third-party MCP call. |
| Operator diagnostics | `C/lib.rs:9493` (`voice_check`) | `doctor` constructs the TTS router to inspect configured providers without a synthesis/network probe. Distinguish read-only diagnostics from execution when defining family disablement. |
| Shared provider construction | `G/stt.rs:123`; `G/tts.rs:621`, `:695` | Configuration and credential availability constrain providers; neither constructor is an extension registration point. Native library callers need their own policy admission. |

## Validation boundary

The inventory is based on source inspection, not live Odoo, voice-provider or third-party-server acceptance. Regression tests cover routing collisions, unauthenticated extension requests and MCP transport gate coverage. A passing source inspection or test does not establish compatibility with an out-of-tree extension.

A later family-wide enable/disable design must choose a shared admission point for each row, define how active work is stopped, and preserve required cleanup. The v1.80.0 gates intentionally leave those existing product paths and their independent authorization policies in place.

## ModuleLease core spike (v1.81.0, RM-102)

`duduclaw-core::module_lease` provides a sealed four-module registry, typed
`ModuleLease<M>` admission proofs, and a transactional `ModuleStore` contract.
The pilots are `odoo_livechat`, `voice`, `discovery`, and `computer_use`.
Live Chat defaults to Disabled; config-dependent voice and Discovery start
Disabled until future gateway initialization resolves their configuration.
Computer use defaults to Active, matching its existing lack of a master switch;
its employee permissions remain a separate gate.

The state machine is `Active → Draining → Disabled → Active`, with
`Disabled → Purging → Purged` for removal. Only drain reconciliation can enter
Disabled from Draining. New execution is denied outside Active. Existing Work
can resume before the drain deadline while a matching inflight row exists.
`Cleanup` remains available in every state; Q5's explicit `ReadAccess` kind keeps
status/list/history reads available even while disabled. These exceptional
proofs authorize only their own kind, so future service boundaries must check
`kind()` as well as the module type. Leases have private fields and no public
constructor, Clone, or Default; Drop releases their slot.

Core has no rusqlite dependency. To preserve that boundary, it contains the
in-memory transactional store; `duduclaw-memory::module_lease::SqliteModuleStore`
implements the same contract using its existing rusqlite dependency. Explicit
`initialize(home)` creates `modules.db` in WAL mode with `modules` and `inflight`
tables. `open(home)` never creates a missing database. Existing missing rows,
corrupt states, I/O errors and locks that exceed the busy timeout deny even
read/cleanup admission. Each operation rereads the database under an immediate
transaction, so independent handles/processes share admission and drain state.
State transitions increment generation; the transition primitive itself does
not perform Admin authorization and must remain behind future gateway checks.

Only Work leases are persisted. Each guard (including a resumed guard) owns a
unique slot, so dropping one guard cannot release another guard's work. Short
calls use the design's 420-second grace after entering Draining; drain completes
on a successful state/acquire/release operation once that grace passes and the
Work slots are empty. There is no background timer in this spike. Unix rows
whose PID is definitely dead (ESRCH) are reclaimed; permission errors, PID reuse,
and non-Unix uncertainty retain the rows. Heartbeats are recorded at admission;
no age-based eviction or renewal worker is implemented. Live overdue Work is
retained conservatively until Drop: a deadline refuses resume but does not prove
execution stopped. Future lifecycle wiring must apply `needs_human`/approval
expiry before reclaiming such work. Failed Drop writes are logged and retain the
slot, keeping drain closed.

This is internal groundwork only: no gateway RPC, HTTP, MCP, worker, CLI, or
channel entry point uses these proofs yet. Runtime integration and its live
cross-process consistency measurement belong to v1.82.0. Purge transitions do
not delete module data or binaries in this spike.
