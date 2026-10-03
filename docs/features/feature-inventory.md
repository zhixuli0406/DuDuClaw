# DuDuClaw Complete Feature Inventory

> v1.24.0 core + 2026-07/08 additions | Last reviewed against the code: 2026-10-02 (v1.67.0)
>
> Note: the sections below started as the v1.24.0 baseline and have been
> corrected where features were removed or changed. The **additions** blocks
> immediately after cover features up to v1.61; later additions are listed in
> `CHANGELOG.md`, which is the authoritative list. The `ja-JP/` and `zh-TW/`
> mirrors carry the same content.

---

## 2026-08 additions (v1.54 – v1.61)

| Feature | Description |
|---------|-------------|
| Calibrated forward model + held-out learning gate (v1.54) | Pre-action confidence predictions scored with proper scores (Brier/RPS, never log score) + Murphy decomposition against external tool evidence, not self-report; evidence-less inductive lessons start as shadow candidates and only promote via out-of-sample Wilson lower bound (Bonferroni-corrected) over a frozen baseline; three honest verdicts only (SUPPORTED / CANDIDATE / INDISTINGUISHABLE_FROM_LUCK). Default ON with per-layer dashboard toggles ([39-calibrated-forward-model.md](39-calibrated-forward-model.md)) |
| Notification governance (v1.55) | Every proactive push carries a mandatory L1/L2/L3 level; quiet hours defer + merge L1/L2 while L3 always delivers; opt-in daily digest that stays silent on empty days; per-category action-rate measurement (`notify.stats`, <50% precision flagged broken); decision cards collapse in place after a decision; dashboard deep links on channel pushes ([40-notification-governance.md](40-notification-governance.md)) |
| Unified pending-decisions pipeline (v1.55) | Five decision sources (goal needs_human, kickoff approval, general approvals, install sign-off, autopilot trips) converge on one action-id encoding, one authorization model (closing previously unauthenticated goal buttons), and one inbox; a fourth "take over" action; one person's decision collapses every recipient's card |
| Human takeover (v1.55) | A verified admin simply speaking in a channel conversation pauses the AI there (default 60 min; `/takeover` query/extend/end); every AI message path into that conversation is frozen, deferred, or dropped while L3 approvals still deliver. Made opt-in (default off) in v1.56 because the always-on default silenced personal-edition self-chat ([42-human-takeover.md](42-human-takeover.md)) |
| Resident sensing (v1.55) | External data streams (`http_poll` / `command` / `file_tail` / `websocket`, default off) join the autopilot bus as `tick` events with auto-derived `prev_`/`delta_`/`pct_` fields, optional local-model screening before waking an agent, in-memory TickHub ring buffer, SSRF/DNS-rebinding defenses; hardened by live-fire rounds against real market feeds ([41-resident-sensing.md](41-resident-sensing.md)) |
| Cross-invocation recent-actions injection (v1.55) | Every wake-up opens with a compressed audit-log digest of the agent's own last-24h tool calls (failures and blocked calls included) so agents answer "did you do X?" from durable records instead of live tool state alone |
| Quoted-reply context on five channels (v1.55) | Replying to (quoting) a message carries the quoted content into agent input on Telegram / Discord / Slack / Teams / WhatsApp; replying to the bot counts as a mention in mention-only groups; Telegram forwards are labeled with their origin |
| Humanized learned rules (v1.55) | Playbook rules rendered as plain-language sentences (zero-LLM templates) with "why this rule exists" evidence, a `/rules` channel command, and numbered rule injection so the agent can cite which rule drove an answer |
| Telegram Mini App approval card (v1.55) | Opt-in "view details" web-app button on high-risk approval cards — full description, simulated consequences, live expiry countdown, approve/deny — with signed `initData` verification and the same authorization as button presses ([43-telegram-miniapp.md](43-telegram-miniapp.md)) |
| Learning-pipeline observability (v1.56) | Rules recording `source_facts` get flagged `source-stale` when a source fact is superseded (deprioritized + labeled at injection); past-threshold consolidation failures logged with reasons (`consolidation_failures.jsonl`); dialogue-path rule settlement routed through the held-out gate; inductive shadow candidates finally accumulate out-of-sample samples on the conversation side too |
| Edition & config hardening (v1.56) | Personal-edition concurrency cap on simultaneous goal tasks (default 2, queue-not-reject, fail-open; RFC-27) — never an agent-count cap; team-edition-only surfaces now blocked server-side at the gateway dispatch entry; `agent.toml [model] account_pool` actually narrows rotator candidates; dashboard agent creation requires an explicit model choice |
| Real ACP server (v1.57) | `duduclaw acp` implements Agent Client Protocol v1 (stdio JSON-RPC) so Zed / JetBrains / nvim agent panels talk to your agents through the same gateway reply pipeline, streaming `tool_call` / `plan` / message chunks; the A2A `acp server` command is unchanged |
| Remote MCP + OAuth 2.1 (v1.57) | Spec-native `POST /mcp` endpoint (version negotiation, stateless mode, Origin-anchored allowlist) plus a minimal-but-complete OAuth 2.1 surface (RFC 9728/8414/7591, PKCE S256, operator consent, refresh rotation) so claude.ai custom connectors / Claude mobile / MCP Inspector connect directly to a self-hosted DuDuClaw |
| Text verdicts on five channels (v1.57) | Replying to a decision card with a whole-message verdict word (approve/deny/retry/complete/abort/pause, zh+en) equals pressing the button on Telegram / Discord / Slack / LINE / Teams — same authorization, dedup, and action-rate accounting; closes the smartwatch one-tap gap |
| Local model marketplace (v1.57) | Pick a use case → hardware-fit lights computed from this machine's memory → one-click install with auto-chosen quantization from five vetted HF publishers; MoE dual-track verdict flags "expert-offload viable" for 30B-A3B-class models on 16GB machines ([45-local-model-marketplace.md](45-local-model-marketplace.md)) |
| Working State — cross-wake authoritative state (v1.57) | Per-agent key-value posture + handoff note auto-injected into every wake-up (cron / heartbeat / goal loop / channels) as the single source of truth; explicit-tool-only updates with required reason + supersession history, `expected_value` CAS against concurrent wakes, `ttl_hours` for day-scoped rules, 32-key cap; `[memory] working_state_enabled`, default on ([44-working-state.md](44-working-state.md)) |
| Scheduled runs gain memory + visibility (v1.57) | Successful cron/dispatch executions now feed the same distillation/knowledge pipeline (hourly per-agent throttle) and land in the run-history page — previously a purely schedule-driven agent accumulated nothing and showed zero runs |
| Ecosystem & distribution surfaces (v1.57) | Six free industry starter packs (safety boundaries intact), pack registry install/publish with client-side sha256 + minisign verification, CONTRIBUTING.md + build-your-own-pack tutorial, public-website chat widget (guest mode, default off) + WordPress plugin, Chrome / VS Code extensions, wearable transcript ingestion (`POST /ingest/transcript`), LINE friend-QR/NFC kit (the `duduclaw tunnel` helper added here was removed in 2026-09); external MCP tool surface becomes scope-driven; Homebrew channel retired |
| Goal task console `/goals` (v1.58) | Dashboard page assigning goals directly to agents (same semantics as `/goal`), full per-round execution timeline (`tasks.timeline`), in-place human intervention — all dashboard needs_human decisions unified onto the same fail-closed `tasks.goal_decide` path as channel buttons |
| Foresight page (v1.58) | The LLM→LWM loop made visible: predict → act → observe → compare, per-round prediction-vs-actual (`forward.chain`), per-agent skill verdict cards (Brier + Murphy decomposition, three honest labels), world-model state buckets readable for the first time; per-aspect MAV verdicts, run links, retry/no-progress signals, and prediction sub-errors now persisted per round |
| Channel OTP fallback + settings integration (v1.58) | Login-OTP delivery tries the global bot token then every per-agent bot token (dedup, ordered) — fixing silent failure when the bot moved to a single agent; agent channel settings and channel management now share one editor/dialog; sidebar "new feature" (`newIn`) badge convention introduced |
| Belief Loop (v1.59) | Structured beliefs about the external world via `belief_submit` / `belief_settle` / `belief_stats` MCP tools; deterministic three-way Brier settlement against a submit-time baseline; calibration figures count only cross-checked settlements, and since no production path supplies a cross-check yet, every current settlement is a self-report that is counted separately and not calibrated; programmatic calibration-stats and belief-vs-live injection hooks; /foresight beliefs tab ([46-belief-loop.md](46-belief-loop.md)) |
| Per-goal contract fields + self-study (v1.59) | `duration_hours` (deadline → needs_human) and `risk_boundary` (empty ⇒ five-line baseline) on goal creation, injected every round and checked by the MAV safety aspect; `/goal` gains `時限:`/`邊界:` segments; optional structured-prediction requirement; agents with same-day belief misses get an automatic evening self-study goal |
| Dispatch engine default ON + scheduler liveness (v1.59) | `[dispatch] enabled` default flipped to true (assigned goals now execute out of the box) with a hot-reload dashboard toggle; `/healthz` returns 503 when cron/heartbeat loops stall >5 min — closing the incident where a scheduler-dead container stayed "healthy" for days |
| Two-stage judging + judge hardening (v1.60) | Cheap first-stage evaluator (`continue`/`candidate_complete`/`blocked`) in front of the MAV panel (default on; any failure degrades to full MAV, never auto-pass); four judge discipline clauses (anti-ratchet, audit-don't-fabricate, anti-scope-creep, self-claims-aren't-evidence); truncated-panel and first-token `PASS` false-positive holes closed; gap-fingerprint stall detection; bail-pattern detection; `resume_on_restart` default `pause` |
| Pluggable judge seam (v1.60) | `[dispatch] judge = mav / evaluator_only / external / human_only` (`evaluator_only` and `human_only` were removed in v1.69.0; a leftover value is handled as described in [deprecations](../guides/deprecations.md)) — external-judge failures always degrade back to MAV (stricter, audited) with its feedback treated as untrusted DATA; unknown values fall back to `mav`; dashboard selector under Settings → Automation |
| Goal contract freeze (v1.60) | Acceptance criteria frozen at creation into an immutable `acceptance_criteria_baseline` read by judges and evaluator alike; agent-identity `tasks_update` edits of a goal task's acceptance criteria, `title` or `description` refused with audit; `/goal` without criteria gets four-element guidance + suggested outcome-style criteria |
| Goal-loop human signals + admission queue (v1.60) | Closed six-way `pause_reason` classification on needs_human (statically stamped, never parsed from LLM text); overdue-progress reporting (`progress_report_minutes`); zero-LLM tool-streak advisories (3/5/8 escalation); bounded FIFO admission queue for ephemeral spawns (default `queue`); best-round handoff on budget exhaustion (deterministic pick + gap list instead of empty-handed escalation) |
| Agent Mail (v1.60) | Per-agent mailbox (`/mail` page): Gmail API / drop-folder inbound, outbound always drafts pending ApprovalBroker confirmation (a background worker is the only sender), DATA-fenced content, dedicated non-grantable scopes, cross-agent reads via delegation policy ([47-agent-mail.md](47-agent-mail.md)) |
| Agent config presets P1 (v1.60) | `duduclaw preset` command family + `agent create --preset` — named reusable config bundles; binding authority in `preset_bindings.toml`, resolution materialized outside the agent dir (self-modification-proof), org fields rejected, sensitive sections stripped; 9 built-in department presets |
| Unified assign panel + plan-first mode + gallery (v1.60) | One assign panel from every entry point (personal edition finally has a primary action) with ask / assign / plan-first ("think first") modes — plan-first parks the task at needs_human with a generated 3-8 step plan, injected once as `<execution_plan>` after approval; finished goals accept follow-up "continue" instructions; Inspiration Gallery `/gallery` fans 22 industry-team examples into one-click remake cards; task detail page gains four tabs (artifacts / files / changes / process) |
| Artifact provenance + delivery safety (v1.60) | `artifacts.jsonl` five-origin provenance ledger (declared / swept / uploaded / produced / unknown; exact-vs-inferred attribution, never guessed from time windows); goal artifacts archived into `attachments/` on accept (canonicalize containment, 20MB/100MB caps); zero-LLM delivery gate before 📎DELIVER (zero-byte / magic-mismatch / corrupt-zip hard-fail); `[limits]` DocumentLimits guarding the three downstream office/zip parsers; expert-pack zip "header lie" bypass fixed |
| Credentials P1 + secret-reference unification (v1.60) | `secret://keychain` and `secret://file` local backends, tick-source `secret://` headers, credential inventory card + `doctor --fix-residue`; `SecretRef`/`Secret` types converge seven hand-rolled decrypt dialects that could send `secret://` reference literals to vendor APIs as real credentials; WhatsApp webhook signature now fail-closed; ActionGuard judge fed a closed 21-token finding enum (attacker-controlled text structurally can't reach the judge prompt); MCP key rotation live-reloads and `denied_tools`/`allowed_tools` enforced at the MCP dispatch gate |
| Ten-channel notification unification (v1.60) | autopilot `notify`, MCP `send_message`, and reminders all route through the shared `create_sender` factory covering ten channels (WebChat honestly refuses) — fixing autopilot Slack notifications that had never been delivered and Google Chat / Teams silent-skip defects |
| Evolution measurement hardening (v1.60) | AEE commit gate splits visible vs held-out eval dimensions (fence-only: they veto but never promote); champion bootstrap made same-form; `duduclaw evolution clear-holdout-rotation` operator exit; per-round snapshots of 14 harness knobs into `aee_round` events |
| Cron day-of-week convention fix (v1.61, **BREAKING**) | Numeric day-of-week normalized at parse time from Unix crontab convention (0/7=Sunday, 1-5=Mon–Fri) to the `cron` crate's Quartz ordinals, with scheduler / heartbeat / MCP validation / dashboard sharing one normaliser — previously `* * 1-5` fired Sunday–Thursday (Sunday ghost runs + silent Friday skips); schedules deliberately written Quartz-style shift by one day after upgrade |
| `duduclaw migrate from claude-code` (v1.61) | One-way import of Claude Code memory shards (→ semantic + SPO temporal memory), CLAUDE.md (→ agent wiki context layer, zero injection budget), and session transcripts (noise-filtered to human prompts + assistant final replies — measured ~1.5% of transcript bytes are signal); everything lands as `origin=import` (trust ≤ 0.7), treated as DATA, redacted by default, injection-scanned, skills security-scanned fail-closed; nothing writes without `--apply` |
| Channel capability table (v1.61) | `channel_capabilities.rs` single authority for 11 channels × 7 capabilities (file/photo upload, interactive buttons, edit-in-place, typing, native markdown, quoted replies) + progress throttle seconds; unsupported capabilities now leave a structured log instead of a silent no-op |
| minimal_context spawn slimming (v1.61) | Every official-CLI spawn carries a curated `--tools` list + `--setting-sources project,local` (keeps the agent-file-guard hook): measured 35,892 → 10,974 fixed tokens per spawn (~69%); `estimate_tokens` CJK-recalibrated (~22% underestimate fixed); MCP `tools/list` now filtered by caller capability (discoverable ⊆ callable) |
| Credentials P2/P3 (v1.61) | Zero-restart rotation — account-pool writes invalidate the rotator cache, Telegram re-resolves its token every poll, six webhook channels verify inbound signatures per-request, Odoo reconnects on next call (Discord/Slack resident WS still need restart); spawn env scrubbed to an allowlist (all `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD` filtered, vendor keys injected explicitly by callers); opt-in per-agent `[capabilities] git_credentials` (default off) restores SSH/GPG for git-push agents, audited; second `secret://` convergence round (account_rotator + mcp.rs) |
| Console & task polish (v1.61) | ⌘K cross-source content search (conversations / artifacts / memory / wiki), `/files` search + task filters + date range, `/goals` task pin/archive/rename + pagination (20-row hard cap lifted), read-only `/presets` dashboard, mail rejection notes, `/goals` detail unified into the four-tab `/tasks/:id` page |

## 2026-08 additions (v1.53)

| Feature | Description |
|---------|-------------|
| Evolution v3: AEE + playbook | Default evolution target moved from SOUL.md rewrites to gene-shaped playbook rules — Gate/Measure split, champion + matches-or-improves commit gate, entry-level observation windows; SOUL.md is read-only for agents ([38-aee-playbook-evolution.md](38-aee-playbook-evolution.md)) |
| E1 entry assertions + anti-reward-hacking audit | Every new playbook rule carries machine-checkable assertions replayed zero-LLM against recorded transcripts (`G-Assertions`); deterministic screening of candidate rules for eval-prompt leakage / tautology / failure-suppression before commit |
| Task-level forward model | Predict-act-verify world model on the goal loop: 4-tier statistical prediction (zero LLM on cold start), fidelity-graded observation (native tool events / audit-only / none), `<state>` block + `(state, action)` visit-graph oscillation detection, deterministic task-rule induction; `[task_forward_model]`, default on since v1.54 |
| Grounded dispatch precheck | Zero-LLM evidence check before the acceptance judge — the final answer must overlap a real non-error tool result; self-echo deny-list + input-overlap subtraction defeat self-certification; `[dispatch] grounding_precheck_enabled`, default on |
| Memory novelty gate | Near-duplicate semantic-layer memory writes rejected with telemetry (0.92 char n-gram cosine) — anti fake-surprise; temporal supersession/reaffirmation exempt; `[memory] novelty_gate`, default on |
| Verified-only reflexion | MistakeNotebook entries carry programmatic `TrajectoryEvidence`; evidence-less self-reported mistakes no longer consolidate into learned rules |
| Simulate-before-act approvals | `needs_human` / approval requests attach a simulated three-step trajectory (15s cap, degrades to no-simulation rather than blocking); read-only wiki namespaces only; dashboard renders the preview |
| Eval recording isolation + bootstrap CLI | `--record` runs against a temporary `.mcp.json` (eval home + placeholder key — zero production side effects, no key leakage); max-turns runaways parse as `error_max_turns` (assessable failure baseline); `duduclaw eval-scaffold` drafts cases from SOUL rules; `duduclaw playbook migrate-soul` migrates legacy SOUL rules into playbook drafts |
| Audit log as evidence source | `tool_calls.jsonl` records masked `result_text`/`input_text` (3-pass secret masking, 16MB rotation, 0600); system-sender dispatches (goal-loop/cron/heartbeat/autopilot) attribute to the executing agent |

## 2026-07 mid/late additions (v1.33 – v1.46)

| Feature | Description |
|---------|-------------|
| Unified LLM provider layer (`duduclaw-llm`) | One normalized request/stream shape over four native protocols (Anthropic / OpenAI Responses / Gemini / OpenAI-compat, 8 presets); `ModelRegistry` pricing; stdio MCP client + provider-agnostic tool loop so API-mode agents get the full tool surface |
| Agent behavioral evals (`duduclaw eval`) | Golden-task regression per agent: deterministic tool-call/regex/grounded assertions + optional LLM judge; live and replay modes, CI-gating exit code |
| HITL ApprovalBroker | One interrupt/approval primitive across MCP tools / autopilot / bus tasks; SQLite-backed, TTL expiry = DENY (fail-closed) |
| OpenTelemetry GenAI tracing | Opt-in `gen_ai.*` spans, OTLP-exportable to Langfuse/Grafana/Jaeger/Datadog; zero overhead when off |
| Channel UX layer | Per-platform markdown rendering, typing indicators, live todo-board progress edited in place across 8 external channels |
| Autonomous Goal Loop | `/goal` → loop-to-completion with a three-aspect MAV acceptance judge; stuck escalates to a human with channel buttons ([34-goal-loop.md](34-goal-loop.md)) |
| Iterative kanban rounds | Task board `revising` state machine with per-round detail history |
| Trusted memory & judge hardening (v1.41) | Write-time origin binding with Sybil-resistant reaffirmation, GovMem promotion gate, Janus rule probation, PORTICO task-scoped capability grants, trace-grounded eval assertions |
| OS-native perception & proactive care | File watch + frontmost sensing → footprint temporal memory (restart-durable snapshots), built-in proactive care checks, LLM-scored proactive gate, one-click OS automation templates ([33-os-native-perception.md](33-os-native-perception.md)) |
| Office document suite | Real docx/xlsx/pptx/pdf output, 📎DELIVER protocol + undeclared-output sweep, gateway archive, files page with LibreOffice preview ([31-office-document-suite.md](31-office-document-suite.md)) |
| Expert packs ecosystem | Installable AI teams: security-scanned install pipeline, built-in industry catalog with category/department grouping, LLM-guided pack authoring, department × rank org placement with `--attach-under` ([32-expert-packs.md](32-expert-packs.md)) |
| Recording → skill | Browser (Playwright trace+HAR, secrets redacted in place) and desktop recorders distilled into approval-gated SKILL.md drafts ([36-recording-to-skill.md](36-recording-to-skill.md)) |
| Photo → desktop pet | Local photo → background removal → pixel quantization → Codex-Pets 8×9 spritesheet; autonomous wander engine moving a real always-on-top window ([35-photo-desktop-pet.md](35-photo-desktop-pet.md)) |
| Capability feature toggles | 16 plain-language feature groups over the raw allow/deny tool lists, backed by a completeness-guarded tool catalog |
| Google Workspace / Notion / GitHub native tools | First-party MCP tools for Gmail/Calendar/Drive/Sheets, Notion, GitHub (v1.45; hidden by default until configured) |
| Desktop shell (Tauri 2) | Native window wrapping gateway + dashboard, tray, gateway picker, transparent desktop-pet overlay window |
| Interaction pacing guard | Conversation-history framing + always-on pacing rule so greetings never re-trigger prior tool-heavy tasks |

## 2026-07 additions (post-v1.24.0)

| Feature | Description |
|---------|-------------|
| Aider-style code map (`code_map` MCP tool) | tree-sitter symbol graph over the HippoRAG-lite Personalized-PageRank engine; ranks repo files by relevance to a query |
| Semantic vector memory (`w_vec`) | third re-rank signal beside FTS/graph; zero-dep CJK-safe `NgramHashEmbedder`, opt-in `DUDUCLAW_SEMANTIC_VECTORS=1` |
| Cross-session user profile | per-user preference traits (temporal supersession) → session-stable `## About This User` reply injection (from gateway distillation and approved reviews); `user_profile_record` / `user_profile_get` MCP tools write and read a separate namespace shared by all gateway-spawned employees, so they do not feed that block (known limit, v1.67.1) |
| GDPR export/erase | `duduclaw export gdpr <contact>` / `duduclaw gdpr erase <contact> --confirm` (the older `gdpr export` spelling was removed in v1.69.0) over memory (triples + mentions + key_facts, 4-table cascade, SHA-256 tombstone) **and** the session store (`<channel>:<chat_id>` prefix) |
| Custom Dashboard Widgets | AI-guided or raw-HTML dashboard cards in a sandboxed runtime; Widget Studio share/import/export ([30-custom-widgets.md](30-custom-widgets.md)) |
| Budget circuit breaker | hard per-agent rolling-window spend caps (`[budget] daily_cap_cents`) that block LLM calls at the choke-point; `budget_events.jsonl` |
| Burn-rate cost anomaly | rolling mean+stddev outlier detection over per-day spend (`cost_anomaly.rs`) |
| Audit export + SIEM sink | `duduclaw export audit` (formerly `duduclaw audit`) — normalize + stream the JSONL audit trails to NDJSON / a webhook |
| Output guardrail hook | opt-in `[guardrails]` — secret-leak / injection-echo / deny-phrase / PII scan before send |
| CI red-team scan | `duduclaw redteam` / `duduclaw test` — 11 attack techniques × en/zh-TW from `CONTRACT.toml` `must_not`, run through the input-guard as a coverage ledger; unblocked units are "needs live validation", not findings; `duduclaw test --emit-evals` writes `duduclaw eval` cases for them |
| Security posture report | `duduclaw security` — weighted checklist of active protections |
| Backup / restore | `duduclaw backup` / `restore` — timestamped home archive + SHA-256 sidecar (verified on restore) |
| Session replay | `duduclaw session replay <id>` — print a session's turns (+ `--tools`) |
| MCP Bridge | `[[mcp.external]]` — mount external MCP servers with a deny-by-default tool filter + `env://` / `secret://` credentials; per-SaaS recipes in `guides/mcp-bridge.md` |
| Secret manager backends | 1Password Connect + Infisical adapters; `secret://<backend>/<name>` resolution wired into the MCP Bridge |
| MCP/skill trust tiering | official / active / orphan classification from repo push-age + owner type |
| Email (via Agent Mail) | `email.rs` (SMTP send with `lettre`, RFC822 parsing) is used by Agent Mail since v1.60. There is no IMAP polling, and email is not one of the channels |
| Communication Channels | now **eleven**: Google Chat and Microsoft Teams, later WeCom and DingTalk, on top of the original seven (see the channel table below) |

---

## Core Architecture

| Feature | Description |
|---------|-------------|
| Multi-Runtime AI Agent Platform | Unified `AgentRuntime` trait — 13 runtime ids in `runtime_catalog.rs`: twelve CLI backends (Claude, Codex, Gemini (deprecated), Antigravity, Grok, Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode) plus OpenAI-compat HTTP, with auto-detection ([13-multi-runtime.md](13-multi-runtime.md)) |
| MCP Server (JSON-RPC 2.0) | Exposes 249 tools (v1.67.0; `tools/list` is filtered to what the caller may call) to AI Runtime via stdin/stdout; registered at `<agent>/.mcp.json` (v1.8.5 — Claude CLI `-p` only reads project-level), gateway auto-creates/repairs on startup |
| ACP/A2A Server | Two commands: `duduclaw acp` (= `duduclaw acp client`) — Agent Client Protocol v1 for IDE agent panels (Zed / JetBrains / nvim; `initialize` / `session/new` / `session/prompt` streaming, `AUTH_REQUIRED` when unconfigured); `duduclaw acp server` (formerly `acp-server`, removed in v1.69.0) — A2A protocol (`agent/discover` / `message/send` / `tasks/*`, `/.well-known/agent-card.json` Agent Card with a legacy `/agent.json` alias) |
| Agent Directory Structure | `.claude/`, `.mcp.json`, `SOUL.md`, `CLAUDE.md`, `CONTRACT.toml`, `agent.toml`, `wiki/`, `SKILLS/`, `memory/`, `tasks/`, `state/` |
| Sub-agent Orchestration | `create_agent` / `spawn_agent` / `list_agents` with `reports_to` hierarchy + D3.js OrgChart + "## Your Team" auto-injection |
| DelegationEnvelope | Structured handoff protocol — context / constraints / task_chain / expected_output |
| TaskSpec Workflow | Multi-step task planning — dependency-aware scheduling, auto-retry (3x), replan (2x), persistence |
| Long-Response Splitting | Sub-agent replies > channel byte budget split via `channel_format::split_text` with paginated labels `📨 **agent** 的回報 (1/N)` |
| Orphan Response Recovery | `reconcile_orphan_responses` replays `bus_queue.jsonl` `agent_response` callbacks left by crash / Ctrl+C / hotswap |
| File-based IPC | `bus_queue.jsonl` for inter-agent delegation, max 5 hop tracking |
| Per-Agent Channel Token | `get_agent_channel_token` reads per-agent `bot_token_enc` first (fixes Discord thread cross-bot 401s) |

## Multi-Runtime

| Feature | Description |
|---------|-------------|
| Claude Runtime | Claude Code SDK (`claude` CLI) with JSONL streaming + `--resume` multi-turn |
| Codex Runtime | OpenAI Codex CLI with `--json` streaming events, `AGENTS.md` file for system prompt |
| Gemini Runtime (deprecated in v1.67.0, removed in v1.70.0; use Antigravity) | Google Gemini CLI with `--output-format stream-json`, `GEMINI_SYSTEM_MD` env var for system prompt, approval mode derived from the agent's capabilities (`auto_edit` by default, `--sandbox` added for read-only agents, `yolo` only for full-access ones). Retained for paid `GEMINI_API_KEY` users after Google retired the personal-tier Gemini CLI on 2026-06-18 |
| Antigravity Runtime (v1.24.0) | Google Antigravity CLI (`agy`, the 2026-06-18 Gemini-CLI successor), driven via oneshot `agy -p --dangerously-skip-permissions --print-timeout 300s`. Binary auto-resolve (PATH → `~/.local/bin/agy`); no `--system` flag so the system prompt + history are embedded in the prompt (CJK-safe); auth via Google sign-in (run `agy` in a host terminal) or API-key mode (`config.toml [antigravity] auth = "api_key"` + Gemini API key); MCP tools registered per agent workspace in `.agents/mcp_config.json`; auto-pre-seeds the agent dir into agy's `trustedWorkspaces` (cross-process lock) to avoid a headless trust-prompt hang; token usage estimated (print mode exposes no stats) |
| Grok Runtime (R4) | xAI Grok CLI ("Grok Build"), driven via oneshot `grok -p` (verified against docs.x.ai 2026-07-13). Binary `grok` (curl-installed; third-party `grok-cli` as fallback probe); `--model` selection; `--tools`/`--disallowed-tools` confinement (+ `native_sandbox` hard gate); system prompt + history embedded in the prompt (CJK-safe); duduclaw MCP server written as `[mcp_servers.duduclaw]` TOML into per-agent `<agent_dir>/.grok/config.toml` (+ agent identity forwarded via spawn env); `XAI_API_KEY` env auth; token usage estimated (plain stdout). **Residuals** (need a live CLI): `--tools` list delimiter, project-local `config.toml` discovery for `mcp_servers`, `--output-format json` schema for real usage, and the full `--model` roster (`grok models`) — only `grok-4.5` / `grok-build-0.1` are doc-confirmed |
| OpenAI-compat Runtime | HTTP endpoint (MiniMax / DeepSeek / etc.) via REST API |
| RuntimeRegistry | Auto-detection of installed CLIs, per-agent `[runtime]` config |
| Cross-Provider Failover | `FailoverManager` health tracking, cooldown, non-retryable error detection |

## Session Memory Stack (v1.8.1 + v1.8.6)

| Feature | Description |
|---------|-------------|
| Native Multi-Turn | Claude CLI `--resume` + SHA-256 deterministic session ID + history-in-prompt fallback (stale session, account rotation, unknown stream-json error) |
| Turn Trimming | >800 chars → head 300 + tail 200 + `[trimmed N chars]`, CJK-safe char-level slicing |
| Prompt Cache Strategy | Direct API "system_and_3" breakpoint placement (no measured hit rate is published) |
| Compression Summary Injection | Post-compression summaries (role=system) injected into system prompt, not conversation turns |
| Instruction Pinning | First user message → async Haiku extraction → `sessions.pinned_instructions` → injected at system prompt tail |
| Snowball Recap | Each turn prepends `<task_recap>` to user message — zero LLM cost, U-shaped attention tail |
| Clarification Accumulation | Agent-question + user-answer appended to pinned instructions (≤1000 chars) |
| P2 Key-Fact Accumulator | 2-4 facts per substantive turn → `key_facts` FTS5 table → top-3 injected (~100-150 tokens vs MemGPT 6,500, −87%) |
| CLI Lightweight Path | `call_claude_cli_lightweight()` — `--effort medium --max-turns 1 --no-session-persistence --tools ""`, 25-40% cost reduction |
| Stabilization Flags | `--strict-mcp-config` + `--exclude-dynamic-system-prompt-sections` (10-15% token reduction); `--bare` removed v1.8.11 (broke OAuth keychain) |
| CJK-Safe String Slicing | `duduclaw_core::truncate_bytes` / `truncate_chars` replaced 31 unsafe byte-index sites |

## Communication Channels (11)

| Channel | Protocol |
|---------|----------|
| Telegram | Long polling, file/photo/sticker/voice, forums/topics, mention-only, voice transcription |
| LINE | Webhook, HMAC-SHA256 signature, sticker support, per-chat settings |
| Discord | Gateway WebSocket, slash commands (`/ask /status /config /session /agent`), voice channels only in builds with the non-default `discord-voice` feature (release binaries do not include it), auto-thread (session id stable across entire thread lifetime post-v1.8.14), embed replies |
| Slack | Socket Mode, mention-only, thread replies |
| WhatsApp | Cloud API webhook, signature verification fail-closed |
| Feishu | Open Platform v2 |
| Google Chat | Webhook (JWT-verified), service-account send |
| Microsoft Teams | Azure Bot / Connector v3 (JWT-verified) |
| WeCom | HMAC-SHA1 signature + AES-256-CBC message encryption |
| DingTalk | HMAC-SHA256 signature + time window |
| WebChat | Embedded `/ws/chat` WebSocket + React frontend (Zustand store) |
| Channel Hot-Start/Stop | Dashboard-driven dynamic launch/termination |
| Media Pipeline | Auto-resize (max 1568px) + MIME detection + Vision integration |
| Sticker System | LINE sticker catalog + emotion detection + Discord emoji equivalents |
| Channel Failure Tracking | `channel_failures.jsonl` with `FailureReason` enum (RateLimited/Billing/Timeout/BinaryMissing/SpawnError/EmptyResponse/NoAccounts/Unknown) |
| Discord Gateway Hardening (v1.9.2) | Real op 6 RESUME — persists `session_id` + `resume_gateway_url` + sequence across reconnects; `select!` stall watchdog breaks after 2× heartbeat silence (fixes 18-min zombie); heartbeat channel capacity 1→16 with `try_send`; op 9 reads `d.bool` for RESUME vs IDENTIFY with 1-5s jitter; close codes 4007/4009/4003 clear session; backoff cap 300s→60s; handles `RESUMED` dispatch |

## Evolution System

> **Evolution v3 (2026-08-06) → S11 (2026-09-29)**: the evolution target moved
> from rewriting `SOUL.md` to the **playbook** (small, independently-retirable
> gene-shaped rules; `SOUL.md` is read-only for agents), and on 2026-09-29 the
> legacy SOUL rewrite path was **removed outright** — the
> `[evolution] legacy_soul_evolution` escape hatch, `SOUL.md` versioning, the
> 24-hour observation window, automatic rollback, the cap-deadlock consolidate
> rewrite, deferred-GVU retry and the `duduclaw evolution finalize` CLI all
> went with it. `[evolution] gvu_enabled` now ships `true` from the factory.
> See [38-aee-playbook-evolution.md](38-aee-playbook-evolution.md) and
> [evolution-engine.md](../architecture/evolution-engine.md) ch.12 for the
> current (and only) engine.

| Feature | Description |
|---------|-------------|
| Prediction-Driven Engine | Active Inference + Dual Process Theory; by design most conversations end without an LLM call (no measured share is published) |
| Dual Process Router | System 1 (rules) / System 2 (LLM reflection) |
| AEE (v3 default) | Agentic Evolution Engine — Generator inner loop (≤3 rounds) → Gate (deterministic, veto) / Measure (scored, no veto) split → champion + matches-or-improves commit gate → entry-level accept/rollback against linked eval cases |
| Playbook (v3 default) | Gene-shaped behavior rules (category/signals_match/eval_cases/success_streak), extends the existing rule_lifecycle store, 0.92-cosine dedup, capacity + stale/archive lifecycle |
| MistakeNotebook | Cross-loop error memory — records failure patterns, prevents regression; entries now carry deterministic `TrajectoryEvidence` (which tool/assertion failed) so unverified self-reported diagnosis no longer feeds reflection consolidation (v3) |
| MetaCognition | Self-calibrating error thresholds every 100 predictions, now with a symmetric raise-back rule so thresholds don't drift one-directionally (v3 Phase 0) |
| Stagnation Detector (v3) | Scans `evolution.db` every 30 min for consecutive-rejected / D-days-zero-apply / repeated-rejection-reason signals, posts to Activity Feed + dashboard |
| ConversationOutcome | Zero-LLM conversation result detection (TaskType / Satisfaction / Completion), zh-TW + en |
| Agent-as-Evaluator | Independent Evaluator Agent (Haiku cost control) for adversarial verification, structured JSON verdict |
| Orchestrator example | 5-step planning (Analyze → Decompose → Delegate → Evaluate → Synthesize) + complexity routing; an example under `docs/examples/orchestrator/`, copied by hand (not applied automatically) |

## Wiki Knowledge Layer (v1.8.9)

| Feature | Description |
|---------|-------------|
| 4-Layer Architecture | L0 Identity / L1 Core / L2 Context / L3 Deep — Vault-for-LLM inspired |
| Trust Weighting | `trust` (0.0-1.0) frontmatter; search ranked by trust-weighted score |
| Auto-Injection | `build_system_prompt()` injects L0+L1 into WIKI_CONTEXT across CLI / channel / dispatcher paths |
| FTS5 Full-Text Index | SQLite `unicode61` tokenizer with CJK support, auto-syncs on write/delete, manual rebuild `wiki_rebuild_fts` |
| Knowledge Graph | `wiki_graph` MCP tool exports BFS-limited Mermaid diagrams; node shapes by layer |
| Dedup Detection | `wiki_dedup` — title match + tag Jaccard similarity (≥0.8) |
| Reverse Backlink Index | Scans `related` frontmatter + body markdown links for bidirectional mapping |
| Search Filters | `min_trust` / `layer` / `expand` (1-hop related/backlink expansion) |
| Shared Wiki | `~/.duduclaw/shared/wiki/` cross-agent SOPs + policies + specs; `wiki_visible_to` capability control; MCP tools `wiki_ls/read/write/search/stats/lint` with `scope="shared"` (the six `shared_wiki_*` spellings were removed in v1.69.0), plus `shared_wiki_delete` and `wiki_share`; SoT policy via `.scope.toml` (see Identity & Access) |
| CLAUDE_WIKI Template | Included in agent CLAUDE.md on creation, provides wiki MCP tool usage guide |

## Skill Ecosystem

| Feature | Description |
|---------|-------------|
| 6-Stage Lifecycle | Activation → Compression (three-layer progressive loading) → Extraction → Distillation → Diagnosis → Gap Analysis ([15-skill-lifecycle.md](15-skill-lifecycle.md)); the former Reconstruction stage had no caller and was removed in 2026-09 |
| GitHub Live Indexing | Search API with 24h local cache, weighted search |
| Skill Marketplace | Web dashboard browsing, installation, security scanning |
| Skill Auto-Synthesis | Gap accumulator → synthesize from episodic memory (Voyager-inspired) → sandbox trial with TTL → cross-agent graduation; off by default (`agent.toml [evolution] skill_synthesis_enabled`) |
| Skill Synthesis Scheduler (W19-P1, v1.22.0) | Runs the "conversation → skill" extraction autonomously on an interval — `config.toml [skill_synthesis] auto_run / dry_run / interval_hours / lookback_days` + dashboard `skill_synthesis.get/update` RPC; `skill_synthesis_threshold` is a `u32` count (fixed the registry scan rejecting `0.7`) |
| Skill Security Scanner (Rust-native) | `skill_lifecycle::security_scanner` scans candidate skills, no Python dependency |

## Local Inference Engine

| Feature | Description |
|---------|-------------|
| OpenAI-compatible HTTP | The one shipped backend — llama-server / Ollama / vLLM / SGLang / llamafile. The in-process llama.cpp, mistral.rs and MLX backends were removed in 2026-09 (they were never compiled into a release binary) |
| Confidence Router | LocalFast / LocalStrong / CloudAPI three-tier routing, CJK-aware token estimation |
| InferenceManager | Multi-mode auto-switching: llamafile → Direct → OpenAI-compat → Cloud API |
| llamafile Manager | Subprocess lifecycle, zero-install portable inference across 6 OS |
| Model Management | `model_search` (HuggingFace), `model_download` (resume + mirror), `model_recommend` (hardware-aware) |

## Prompt Compression

| Feature | Description |
|---------|-------------|
| Reply-path budget pipeline | TurnTrim → DropOldestToolEchoes → BisectAndSummarize, cost-pressure aware, CJK-safe token estimation ([11-token-compression.md](11-token-compression.md)). The earlier Meta-Token / LLMLingua-2 / StreamingLLM compressor and its `compress_text` tool were removed in v1.33 |
| Cache-aware guard | Compression is skipped when recent cache efficiency is above 50% and the budget overshoot is below 15% |

## Voice Pipeline

See [14-voice-pipeline.md](14-voice-pipeline.md). The HTTP endpoints and the Telegram voice handler are wired separately.

| Feature | Description |
|---------|-------------|
| HTTP endpoints | `POST /api/stt` (OpenAI-compatible transcription API or a local command template; returns 501 when `[voice]` has no STT provider), `POST /api/tts`, `GET`/`POST /api/voice/config` |
| TTS providers | Piper (local ONNX voices) / Edge TTS / MiniMax T2A (picks a CJK or Latin voice) / OpenAI TTS, behind one router |
| Telegram voice | Voice messages are transcribed with the OpenAI Whisper API; `/voice` replies use Edge TTS. Both are hardcoded, so the dashboard voice settings do not affect Telegram |
| Not in release binaries | In-process Whisper (`whisper` feature of `duduclaw-inference`), ONNX embedding (`onnx` feature) and Discord voice channels (`discord-voice` feature) compile only when you build with those features |
| Never implemented | SenseVoice, Deepgram, Silero VAD, `symphonia` decoding and LiveKit voice rooms were listed here before; none of them has code |

## Security

| Feature | Description |
|---------|-------------|
| `agent-file-guard` PreToolUse hook | `duduclaw hook agent-file-guard` (Rust subcommand, matcher `Write\|Edit\|MultiEdit\|NotebookEdit\|Bash`, installed per agent by `agent_hook_installer` with `--agent` and `--home`) — blocks agent-structure files outside the canonical tree, own-SOUL.md and own-CONTRACT.toml writes, cross-agent writes, employee writes under the DuDuClaw home outside the own agent directory and `attachments/` (real path after symbolic links), and changes to the employee's own `agent.toml` outside its editable sections; Bash lane is a heuristic, `Read` not covered; rules and limits in [05-security-defense.md](05-security-defense.md) |
| `org_field_guard` | Field-level freeze inside the same hook: `[agent] reports_to`/`department`/`name`, the whole `[capabilities]` table, and `config.toml [delegation]`/`[acp]` — fail-closed on unparseable or unreconstructable writes |
| `data-file-guard` PreToolUse hook | `duduclaw hook data-file-guard` (RFC-23 §14.4, Rust subcommand since H10 2026-09, matcher `Read\|Bash`), armed only when redaction is active; a `Bash` filename heuristic, not a sandbox |
| Dashboard auth | JWT account login (Argon2id passwords, `users.db`) or the gateway admin token. The earlier Ed25519 challenge-response path has been removed; no configuration could ever enable it |
| AES-256-GCM | API key encryption at rest, per-agent key isolation |
| Prompt Injection Scanner | `input_guard` — 11 rule categories, block threshold 60, NFKC-normalized, en + zh-TW patterns, XML delimiter protection |
| SOUL.md Drift Detection | SHA-256 fingerprint comparison |
| CONTRACT.toml | Behavioral boundaries + `duduclaw test` red-team CLI (9 built-in scenarios + coverage ledger); auto-injected into system prompt for all runtimes |
| RBAC matrix (read-only view) | The Security page renders a per-agent tool/web/approval matrix derived from `agent.toml [capabilities]`. The `duduclaw-security::rbac` module was removed (zero callers); the editable source of truth is each agent's capability envelope |
| Unified Audit Log | `audit.unified_log` merges `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` — Logs page source filter + severity dropdown |
| JSONL Audit Log | Full tool call recording, async write |
| Unicode Normalization | NFKC normalization to detect homograph attacks |
| Action Claim Verifier | Signature validation for tool execution claims |
| Container Sandbox | Two separate paths. Task sandbox (`agent.toml [container] sandbox_enabled`): Docker only, runs a delegated task's AI CLI in a read-only, non-root, resource-limited container, fails closed ([guide](../guides/task-sandbox.md)). Script sandbox (PTC `execute_program`, `duduclaw secaudit` PoC): Docker (WSL2 first on Windows), `--network=none`, read-only root, only a private read-only script directory mounted; PTC refuses to run when the sandbox is unavailable unless `script_when_unavailable = "run_unsandboxed"` |
| Secret Leak Scanner | 19 secret patterns (Anthropic / OpenAI / AWS / GitHub / GitLab / Slack / Stripe / Google / SendGrid / JWT / PEM keys and key or password assignments) plus a high-entropy check, used by the skill security scanner |
| Sensitive Data Redaction (RFC-23, v1.14.0) | `duduclaw-redaction` crate — internal data (Odoo / shared wiki / file tools) is replaced with `<REDACT:CATEGORY:hash8>` tokens before reaching the LLM and auto-restored at trusted egress (user channel reply, whitelisted tools); AES-256-GCM SQLite vault (per-agent 32-byte key, 0o600), TTL 7d two-phase GC, 5 built-in profiles, five-layer enable/disable resolver, JSONL audit with 10MB rotation; field-level rules added 2026-09 — `db_field` (Odoo `model.field` / `model.*` sugar) and generic `json_path` tokenize the whole matched field value instead of pattern-matching content, plus a `duduclaw redaction verify` JSON mode to prove a rule fires against a sample tool result; `db_field`'s Odoo-only table generalized (2026-09) into a `[redaction.data_sources.*]` registry any MCP tool can bind to, and redaction's reach extended past DuDuClaw's own MCP server for the first time — see the next row |
| Data Sources & Native DB Connector (2026-09) | `[redaction.data_sources.<name>]` registry (`tools`, `table_arg`/`table`, `record_paths`, `key_alias`) lets a `db_field` rule's `source` name any tool-backed data source, not just the built-in `odoo`; `duduclaw mcp-proxy` (a spawn-time `.mcp.json` rewrite) routes a customer's own external stdio MCP servers through the same egress/result redaction DuDuClaw's own MCP server applies, and a `ToolInterceptor` hook does the in-process equivalent for the openai-compat direct-API tool loop — HTTP/SSE MCP servers and the codex/gemini/antigravity runtimes are not covered yet; new read-only `duduclaw-db` crate (sqlx: PostgreSQL/MySQL/SQLite, three-layer read-only enforcement) exposes four MCP tools (`db_sources` / `db_tables` / `db_select` / `db_query`, `db_query` refused unless `allowed_tables = ["*"]`) behind `Scope::DbRead` (`db:read`) plus a deny-by-default per-agent `[capabilities] db_sources` grant; dashboard gains 資料來源 (two tabs) and 資料表欄位規則 cards with a 試跑 dry-run and a poison banner for a broken `[redaction]` config; local files (2026-09) close a separate gap — the Claude CLI's built-in `Read`/`Bash` are not MCP tools and never passed the redaction choke point — with three new MCP tools `file_read`/`csv_read`/`xlsx_read` (path-fenced, `files:read` scope), a built-in `duduclaw_files` registry source (`db_field` rules like `customers.csv.name` / `客戶清單.xlsx.地址`), and a PreToolUse `data-file-guard` hook (`[redaction] data_file_guard`, default on) blocking the built-in route, honestly documented as a filename heuristic rather than a sandbox; **AI detection + custom rules (2026-09)**: a new `type = "ner"` rule kind and built-in `ai_pii` profile ("AI 智慧偵測") run OpenAI Privacy Filter (Apache-2.0) on-device through ONNX Runtime (`ort` `load-dynamic` — the release binary links no runtime; `redaction.model.install` downloads model + runtime with pinned sha256, `.status`/`.cancel`/`.remove` alongside; priority below every regex rule so exact patterns win; measured recall published honestly, regex profiles stay on as the first layer); dashboard-authored **custom rules** (`~/.duduclaw/redaction/profiles/custom.toml`: a data-type name + keyword list or pattern, per-rule `enabled` on every rule kind, `[meta.labels]` display names, `redaction.custom_rules.*` and `redaction.profiles.import`/`.remove` RPCs for TOML rule packs), `redaction.suggest_pattern` (paste 2–5 examples → a validated pattern; local inference → utility model → heuristic, never a fabricated pattern) and `redaction.dry_run` on unsaved draft rules ([55-data-sources.md](55-data-sources.md)) |

## Memory System

| Feature | Description |
|---------|-------------|
| Episodic / Semantic Separation | Generative Agents 3D-weighted retrieval (Recency + Importance + Relevance) |
| Full-Text Search (FTS5) | SQLite built-in |
| Vector re-rank signal | Built-in character n-gram hash embedder (`NgramHashEmbedder`, opt-in `DUDUCLAW_SEMANTIC_VECTORS=1`); it matches surface fragments, not meaning. The ONNX embedder needs the non-default `onnx` build feature, which release binaries do not include |
| Memory Decay Scheduler | Daily background task — low-importance + 30d old → archived, archived + 90d → permanent delete |
| Cognitive Memory MCP Tools | `memory_search_by_layer`, `memory_successful_conversations`, `memory_episodic_pressure`, `memory_consolidation_status` |
| Key-Fact Accumulator | `key_facts` table with FTS5 — cross-session lightweight memory (see Session Memory Stack) |
| Temporal Memory (F1, v1.19.0) | `memories` gains temporal/knowledge-graph columns (`valid_from`/`valid_until`/`superseded_by`/`supersedes`/`subject`/`predicate`/`object`/`confidence`/`metadata`) via idempotent migration; `store_temporal()` auto conflict-resolves same `(agent, subject, predicate)` and links supersession chain (since v1.67.1 only when the write is at least as trusted as the current fact); `search()` default-filters to currently-valid rows; `get_history()` / `get_at()` expose chain + point-in-time |
| Reflexion Loop (F2, v1.19.0) | Bridges existing `MistakeNotebook` — F2a injects recent unresolved mistakes into answering prompt (`## Past Mistakes to Avoid`, CJK-safe match + recency fallback); F2b consolidates ≥3 same-`MistakeCategory` mistakes into one semantic memory rule (`reflexion.rs`) then marks sources resolved. Trigger = `ErrorCategory` Significant/Critical (MetaCognition-adaptive) |
| `memory_fetch_batch` (F3, v1.19.0) | MCP tool + `get_by_ids` fetch ≤100 entries by ID in one call (namespace/ownership enforced, partial hits → `missing_ids`) |
| Bi-temporal + build-time provenance (D1) | `memories` gains `ingested_at` (transaction-time axis, distinct from world-time `valid_from`) + `invalidated_by_event`/`invalidated_at` (which source_event closed a row out, when) via idempotent migration. `store_temporal()` supersession is decided by world-time `valid_from` (out-of-order resilient — an earlier fact inserts as a bounded historical segment without disturbing the current one; no-`valid_from` writes keep legacy ingestion-order behavior); identical re-observation **reaffirms** (metadata `reaffirmed_by`, ≤20, + `access_count` bump) instead of adding a row |
| `memory_get_history` / `memory_get_at` (D1) | MCP exposure of the temporal read APIs — full supersession chain (with provenance columns) and point-in-time lookup for a `(subject, predicate)` triple (scope `memory:read`) |
| Supersession trust guard (v1.67.1) | A write whose effective `origin_trust` is strictly lower than the current fact's (stored trust capped at its class ceiling) cannot supersede it; equal or higher supersedes as before. Conversation-fact and profile-trait distillation hold refused claims for a dashboard-only `knowledge_quarantine` review (24 h, ≤20 new per agent per UTC day, beyond that audit only); other paths skip or return an error. `user_profile` origin ceiling 1.0 → 0.6. `config.toml [memory] supersession_trust_guard` (default on) ([20-memory-intelligence.md](20-memory-intelligence.md#supersession-trust-guard-v1671)) |
| `memory_invalidate_by_origin` (D1) | Source rollback primitive — expires (never deletes) every currently-valid fact from an **exact** `origin` (optionally since a cutoff), cascades `origin_trust ≤ 0.1` to `derived_from` descendants; history preserved (`invalidated_by_event = "origin_purge"`). Scope `admin`; since v1.67.1 an AI-employee caller may only invalidate `channel` / `mcp_external` / `tool_echo` (others refused, audit `memory_invalidate_refused`) |
| Graph retrieval evolution (D3) | HippoRAG-lite graph gains four fail-safe refinements (byte-identical when unused): **(1)** persistent per-agent graph cache (`RwLock`) invalidated by a per-agent generation counter that every triple-mutating write bumps, engaged only above `GRAPH_CACHE_MIN_TRIPLES = 500`; **(2)** entity alias merging via `entity_alias(agent_id, canonical, alias)` — folds surface forms onto one node before build+seeding, normalized + chain-flattened; **(3)** predicate edge labels attached to edges (PPR unchanged) feeding `engine.export_graph(agent, limit)` → serializable `{nodes, edges}` snapshot (quarantined facts flagged) for the D6 curation UI; **(4)** opt-in embedding seeding (`graph_embed_seed`) — PPR seeds = whole-word FTS ∪ query-embedding nearest entity vectors (same-model cosine, top-k, lazy `entity_embedding` cache), off by default |
| `memory_alias_add` / `memory_alias_list` (D3) | MCP tools to manage entity aliases — add folds an `alias` onto a `canonical` entity (scope `memory:write`), list returns `(canonical, alias)` pairs (scope `memory:read`); namespace-isolated |
| Decision Continuity (RFC-24, v1.23.0) | When an agent offers an enumerated choice (Option A/B/C), each option is persisted into the Temporal Memory **semantic** layer (independent of conversation compression) and open decisions are re-injected each turn; a later "use Option C" (new turn / session / process) resolves from durable state instead of being guessed. Deterministic, zero-LLM detection; `decision_resolve` / `decision_list` MCP tools + Dashboard panel + Prometheus counters; per-agent opt-in `[memory] decision_continuity = true` (TTL `decision_ttl_days`, default 7) |

## Account & Cost Management

| Feature | Description |
|---------|-------------|
| Multi-Account Rotation | OAuth + API Key, 4 strategies (Priority/LeastCost/RoundRobin/Failover) |
| Dual Dispatch Path | Both sub-agent dispatcher (`claude_runner::call_with_rotation`) and channel reply (`channel_reply::call_claude_cli_rotated`) go through rotator |
| CostTelemetry | SQLite token tracking + cache efficiency analytics + 200K price cliff warning |
| Budget Manager | Per-account monthly limits + cooldown + adaptive routing (cache_eff <30% → local) |
| Direct API | Bypass CLI, `cache_control: ephemeral` on the system prompt; paid fallback when OAuth accounts are rate-limited (no measured hit rate is published) |
| Channel Failure Tracking | `channel_failures.jsonl` with category-specific zh-TW messages |
| Binary Discovery | `which_claude()` / `which_claude_in_home()` probe Homebrew (Intel + Apple Silicon) / Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf / NVM |

## Browser Automation

| Feature | Description |
|---------|-------------|
| L1 `web_fetch_cached` | SSRF-gated, disk-cached HTTP GET (body truncated at 60k chars) |
| L2 `web_extract` | Same fetch path + CSS-selector extraction (`text` / `html` / `json`) |
| L3 headless (optional, external) | Playwright or Browserbase registered as a per-agent MCP server in `.mcp.json`; not part of the binary, no fallback into it |
| L5 Computer Use | A container virtual display started by `computer_use_orchestrator` (image `ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>`, never pulled automatically), driven by the employee through eight `computer_*` MCP tools (`session_start` / `screenshot` / `click` / `type` / `key` / `scroll` / `navigate` / `session_stop`; gateway-owned session reached over a signed loopback route, one per employee, no API key needed, network only to the hosts in `[capabilities.computer_use_config] allowed_domains`). The chat-triggered gateway loop and the `native` host-desktop mode were removed |
| Capability Gating | `agent.toml [capabilities]` deny-by-default (`computer_use` / `browser_via_bash` / `allowed_tools` / `denied_tools`); `denied_tools` enforced both as `--disallowedTools` and at the MCP dispatch gate |

## Container Sandbox

Two separate code paths share the name.

| Feature | Description |
|---------|-------------|
| Task sandbox | Per-agent (`agent.toml [container] sandbox_enabled = true`). A delegated task runs the agent's AI CLI in a Docker container: read-only root filesystem, non-root user, all capabilities dropped, memory / process / CPU limits, a private throw-away workspace, the agent directory mounted read-only at `/agent`. File and shell tools only; no platform MCP tools. Needs `network_access = true` and a locally pulled image. Docker only. When it cannot run, the task fails (audit `task_sandbox_unavailable`) unless `config.toml [container.sandbox] when_unavailable = "run_unsandboxed"`. A sandboxed employee never forms a Team (goal rounds run Solo in the sandbox) and is not woken by mail; channel replies, cron, reminders and the other conversation paths stay on the host and write the audit event `task_sandbox_not_applied`. See [Task sandbox guide](../guides/task-sandbox.md) |
| Script sandbox | Used by PTC `execute_program` and the `duduclaw secaudit` PoC step. Docker on macOS/Linux, WSL2 then Docker on Windows (WSL2 not yet run on a real Windows host), via `duduclaw-container`; the Apple Container backend is never selected. Same image as the task sandbox, never pulled; `--network=none`, read-only root, 2 GiB / 256 PIDs / 1 CPU, `/tmp` tmpfs, 600 s hard cap, only a private read-only script directory mounted. When unavailable, PTC fails by default (`[container.sandbox] script_when_unavailable`); the PoC never runs on the host ([guide](../guides/task-sandbox.md)) |

## Scheduling

| Feature | Description |
|---------|-------------|
| CronScheduler | `cron_tasks.jsonl` + `cron_tasks.db` persistent (v1.8.12); schedules are created with `tasks_create` + `schedule` (the older `schedule_task` tool was removed in v1.69.0) |
| ReminderScheduler | One-shot reminders (relative `5m`/`2h`/`1d` or ISO 8601), `direct` or `agent_callback` mode |
| HeartbeatScheduler | Per-agent unified scheduling — bus polling + GVU silence breaker + cron |
| Scheduler-Level Task-Board Pull (v1.9.3) | `poll_assigned_tasks` moved into `HeartbeatScheduler::run` tick — scans entire agent registry every 30s (no longer skips `enabled=false` agents); 1-hour LIKE-marker cooldown prevents stampedes |

## Task Board & Activity Feed

| Feature | Description |
|---------|-------------|
| Task Board | SQLite-backed task management — status / priority / assignment tracking |
| Dashboard RPC | `tasks.list/create/update/remove/assign`, `activity.list` for web UI |
| Agent MCP Tools | `tasks_list`, `tasks_create`, `tasks_update`, `tasks_claim`, `tasks_complete`, `tasks_block`, `activity_list`, `activity_post` — agents see their own queue, claim work, post progress; changing or completing another employee's task needs the delegation relationship, and control tags (`outcome:` / `grant:` / `auto-research`) are fixed for AI employees |
| Real-time Activity Feed | WebSocket-streamed activity events |
| System-Prompt Injection | Pending tasks (up to 5) auto-injected into agent system prompt |

## Autopilot Rule Engine

| Feature | Description |
|---------|-------------|
| Event Bus | `tokio::broadcast` (capacity 8192) — 12 events a new rule can subscribe to: `task_created` / `task_updated` / `task_status_changed` / `activity_new` / `channel_message` / `agent_idle` / `run_at_risk` / `os_file` / `os_frontmost` / `tick` / `security_event` / `odoo_event`; `cron_tick` is never emitted and is refused on create since v1.67.1 ([23-autopilot-engine.md](23-autopilot-engine.md)) |
| Rule Conditions | `all` / `any` + `eq/neq/in/not_in/gt/gte/lt/lte/contains` operators |
| Action Types | `delegate` (enqueue bus task), `notify` (channel), `run_skill` (skill name + target validated via alphanumeric allowlist + `canonicalize()` path containment) |
| Rule CRUD | Dashboard RPC `autopilot.list/create/update/remove/history` + agent MCP `autopilot_list`; trigger, conditions and action validated at write time; empty conditions fire on every event (v1.67.1) |
| 3-State Circuit Breaker | Per-rule `Closed` / `Open` / `HalfOpen` — 10 fires in 60s trips Open (60s cooldown), then HalfOpen probe; prevents self-reinforcing loops; transitions logged to history + Activity Feed |
| events.db Bridge | SQLite (WAL + monotonic auto-increment id + 7-day prune) replaces legacy `events.jsonl` — no rotation race, no partial-line hazard |

## Reliability & Governance

| Feature | Description |
|---------|-------------|
| LLM Fallback Chain (`gateway/failover.rs::model`, v1.9.4) | Layer 2 of the three-layer failover stack (account → model → runtime, consolidated 2026-09-29): primary timeout/503/429/overloaded auto-switches to the lighter fallback model; pure `is_llm_fallback_error` / `should_attempt_model_fallback` unit-tested, with `FailoverManager::model_fallback_for` as the single decision the dispatch paths call; hard-deadline arm returns `Err("hard timeout")` so fallback triggers reliably |
| Evolution Events System (v1.9.4) | 30+ event schema (`schema.rs`), async batch+retry emitter (`emitter.rs`), query interface (`query.rs`), reliability guarantees (`reliability.rs`); HTTP endpoints surfaced in Web `ReliabilityPage` |

## Identity & Access

| Feature | Description |
|---------|-------------|
| Identity Resolution (`duduclaw-identity`, RFC-21 §1, v1.11.0) | `IdentityProvider` async trait — `WikiCacheIdentityProvider` (`shared/wiki/identity/people/*.md`), `NotionIdentityProvider` (Notion `databases/query` + `field_map`), `ChainedProvider` (cache → upstream, graceful degrade) |
| `identity_resolve` MCP Tool | Gated by `Scope::IdentityRead`, returns canonical `ResolvedPerson` records |
| Sender Auto-Injection | Channel reply injects XML-delimited `<sender>` block into system prompt (resolved once per turn) so SOUL.md "reject non-member" rules are data-driven |
| Shared Wiki SoT Policy (RFC-21 §3, v1.11.0) | `~/.duduclaw/shared/wiki/.scope.toml` declares namespace ownership — `agent_writable` (default), `read_only { synced_from }`, `operator_only`; honoured by `wiki_write` with `scope="shared"` and `shared_wiki_delete`; `wiki_namespace_status` exposes active policy; absent/malformed ⇒ fail-safe no policy |

## Live Forking (RFC-26)

| Feature | Description |
|---------|-------------|
| Live Run Forking (`duduclaw-fork`) | pydantic-deepagents-inspired parallel branching of a live run — explore multiple continuations concurrently |
| AI Judge | Scores parallel branches to select the best continuation |
| Budget Control | `budget.rs` caps fork fan-out / cost |
| Status | Off by default (per agent `[fork] enabled`). In v1.67.0, four release-flow tests still fail on Windows CI, so do not enable it on Windows yet (see `CHANGELOG.md`) |

## CLI Runtime (one-shot PTY)

| Feature | Description |
|---------|-------------|
| One-shot PTY invocation (`duduclaw-cli-runtime`) | Spawns a CLI under a real pseudo-terminal (ConPTY on Win 10 1809+, openpty on Unix via `portable-pty`) and drains stdout to EOF, for CLIs that refuse to run when stdout is a plain pipe. Used by the Grok runtime and the CLI-login helper. `clear_env` keeps the gateway's vendor API keys out of the child; `deadline` is an absolute wall-clock cap. See [27-pty-pool-runtime](27-pty-pool-runtime.md) |
| PTY session pool — **removed 2026-09** | The long-lived sentinel-framed `claude` REPL pool, the `duduclaw-cli-worker` subprocess + supervisor, `RuntimeMode::PtyPool`, `GET /api/runtime/status`, the `pty_pool_*` / `worker_*` metrics and the `[runtime] pty_pool_enabled` / `worker_managed` keys were all removed. Reason: the Anthropic programmatic-usage split it insured against was paused on 2026-06-15 and never resumed, and pool sessions had no conversation dimension (cross-conversation context bleed), so it could not be enabled safely |

## MCP HTTP/SSE Transport (W20)

| Feature | Description |
|---------|-------------|
| HTTP Server | `duduclaw http-server --bind 127.0.0.1:8765` — Bearer-authenticated REST + SSE |
| Endpoints | `POST /mcp/v1/call` (single JSON-RPC tool call), `GET /mcp/v1/stream` (long-lived SSE), `POST /mcp/v1/stream/call` (async + SSE push), `GET /healthz` (no auth) |
| Rate Limit | Token bucket `OpType::HttpRequest`, 60 req/min |
| SSE Connection Store | `mcp_sse_store.rs` manages SSE connections with broadcast channels |

## ERP Integration

| Feature | Description |
|---------|-------------|
| Odoo Bridge | 17 MCP tools (CRM/Sales/Inventory/Accounting), JSON-RPC middleware |
| Edition Gate | CE/EE auto-detection, feature gating |
| Event sync | A poller (`[odoo] poll_enabled`) and `POST /webhook/odoo` (`[odoo] webhook_enabled`, shared secret required), both off by default, emit `odoo_event` for autopilot rules |
| Per-Agent Credential Isolation | `OdooConnectorPool` keyed by `(agent_id, profile)`; audit log carries `profile` + `ok=bool` (v1.11.0 / RFC-21 §2) |
| Dashboard Test-Before-Save | `odoo.test` accepts inline params; missing credential falls back to stored secret; inline mode reuses the same SSRF / HTTPS / db-name validators (v1.13.1) |

## RL & Observability

| Feature | Description |
|---------|-------------|
| RL Trajectory Collector | Writes `~/.duduclaw/rl_trajectories.jsonl` during channel interactions |
| Prometheus Metrics | `GET /metrics` — failover, wiki trust, decision continuity, prompt compression, resident sensing (`tick_*`), goal-loop and live-fork counters. Six request/token/duration/session/channel/budget series that were never incremented were removed in v1.66; per-request cost lives in `cost_telemetry.db` |
| Dashboard WebSocket Heartbeat | Server Ping 30s + 60s idle close; client `ping` RPC 25s |
| BroadcastLayer | Tracing layer streams real-time logs to WebSocket subscribers |

## Memory Evaluation & Python Layer

| Feature | Description |
|---------|-------------|
| LOCOMO Memory Evaluation (W21, v1.9.4) | `python/duduclaw/memory_eval/` — `retrieval_accuracy` / `retention_rate` / `locomo_integrity_check`; `cron_runner` is a manual CLI entry point (`python -m memory_eval.cron_runner smoke_test\|weekly_kpis\|monthly_locomo`), nothing in the repo schedules it; 5-min `smoke_test` P0; `build_golden_qa.py` builds gold-standard QA set; 200-entry `data/golden_qa_set.jsonl`; `duduclaw-memory` batch query API |
| Python Agents Routing (v1.9.4) | `python/duduclaw/agents/` — capability-based routing (`capabilities/` manifest loader + matcher, `routing/` router + resolution + memory_resolver) |
| Python MCP Scope Enforcement (v1.9.4) | `python/duduclaw/mcp/` — API key auth with key masking; memory tools (store/read/search/namespace/quota) with strict scope enforcement (`memory:write` / `memory:read`) at `execute()` entry |

## Web Dashboard

| Feature | Description |
|---------|-------------|
| Routes | ~74 non-redirect route paths in `web/src/App.tsx` (plus ~30 legacy redirect aliases that keep old bookmarks working). Four shells: workspace (`/`, `/chat`, `/tasks`, `/goals`, `/inbox`, `/files`, `/mail`, `/timeline`, `/foresight`, `/gallery`, `/canvas`, …), agents (`/agents`, `/agents/:id/:tab`, `/agents/new`, `/experts`, `/org`, `/presets`), `/manage/*` (channels, logs, billing, users, departments, distributors, inference, local-models, finetune, reliability, secaudit, wiki-trust, …), `/app/system/*` (settings, security, accounts, license, causal, decision-lab, ccr, …), plus standalone pages (`/login`, `/welcome`, `/webchat`, `/console`, `/mascot-overlay`, `/pet-studio`, `/world`, `/launcher`). `web/src/components/layout/nav-model.ts` is the source of truth for what appears in the sidebar |
| Tech Stack | React 19 + TypeScript + Tailwind CSS 4 + Base UI + CVA |
| DuDuClaw Design System (mds) | Shared `web/src/components/mds/` component library (OKLCH tokens, four-layer surfaces, three-tier shadows, Inter / Geist Mono) + `nav-model.ts` grouped sidebar (personal / work / company / settings) + `web/DESIGN.md` spec; every page built on the shared primitives with synchronized en/ja/zh i18n |
| Real-time Log Streaming | BroadcastLayer tracing → WebSocket push |
| Memory → Key Insights Tab | `key_facts` cards with access_count badge + timestamp + collapsible source metadata |
| Memory → Self-Improvement tab | Learning overview, stagnation warnings, rejection statistics, plain-language playbook rule cards with JSON export and manual disable ([38-aee-playbook-evolution.md](38-aee-playbook-evolution.md)) |
| Logs → History Tab Rewrite | Source filter chips + per-source counts + severity dropdown + severity-colored left borders + JSON detail expansion |
| Toast Notifications | Module-scoped event bus, max-5 queue, warm stone/amber/emerald/rose variants, respects `prefers-reduced-motion` |
| OrgChart | D3.js interactive agent hierarchy visualization |
| Session Replay | Conversation playback with timeline |
| WikiGraph | Interactive knowledge graph |
| Internationalization | zh-TW / en / ja-JP (600+ translation keys) |
| Dark/Light Theme | System preference + manual toggle |
| Experiment Logger | Trajectory recording for RL/RLHF offline analysis |
| Marketplace RPC | `marketplace.list` serves the built-in MCP catalogue: four cards since v1.67.1 (Playwright `@playwright/mcp`, Browserbase `@browserbasehq/mcp`, Filesystem, Memory), plus entries from `~/.duduclaw/marketplace.json` |
| Partner Portal | SQLite `PartnerStore` + profile/stats/customers CRUD + 7 RPCs |

## Commercial

| Feature | Description |
|---------|-------------|
| License Tiers | Nine tiers in `crates/duduclaw-license/src/tier.rs` (opensource, hobby, solo, studio, business, partner, personal_pro_self_host, self_host_pro, oem). The capability gates are `premium_templates`, `white_label` and `industry_evolution_params`; see [LICENSING.md](../../LICENSING.md) |
| Hardware Fingerprint | License binding |
| Industry Templates | Manufacturing / Restaurant / Trading (free); premium industry packs need `premium_templates` |
| CLI Tools | 12+ subcommands |
| Partner Portal | Multi-tenant reseller interface |
