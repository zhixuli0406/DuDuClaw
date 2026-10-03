# DuDuClaw Feature Highlights

> DuDuClaw v1.62.0 | Last updated: 2026-09-04

This directory contains detailed introductions to DuDuClaw's standout features. Each article explains the design rationale, system behavior, and operational flow — aimed at developers who want to understand *how things work* without diving into source code.

---

## Feature Index

| # | Article | One-liner |
|---|---------|-----------|
| 1 | [Prediction-Driven Evolution](01-prediction-driven-evolution.md) | 90% of conversations evolve at zero LLM cost |
| 3 | [Confidence Router & Local Inference](03-confidence-router.md) | Smart model selection that saves 80%+ on API bills |
| 4 | [File-Based IPC Message Bus](04-file-based-ipc.md) | Structured inter-agent delegation with TaskSpec workflows |
| 5 | [Security Defense](05-security-defense.md) | Four live guards, where each runs, and what none of them covers |
| 7 | [Multi-Account Rotation](07-account-rotation.md) | Cross-provider credential scheduling with failover |
| 8 | [Browser Automation and Computer Use](08-browser-automation.md) | Two fetch tools and an optional browser server the agent picks between, plus computer-use sessions driven through eight `computer_*` MCP tools; no auto-router |
| 9 | [Behavioral Contracts & Red-Team Testing](09-behavioral-contracts.md) | Machine-enforceable agent boundaries |
| 10 | [Cognitive Memory System](10-cognitive-memory.md) | Human-inspired memory with forgetting curves |
| 11 | [Prompt Budget Enforcement](11-token-compression.md) | Estimate the prompt, walk three stages, or refuse the request |
| 12 | [Industry Templates & Odoo ERP Bridge](12-industry-templates.md) | Out-of-the-box business intelligence |
| 13 | [Multi-Runtime Agent Execution](13-multi-runtime.md) | Claude / Codex / Antigravity / Grok / OpenAI-compat and more behind one backend (Gemini CLI deprecated) |
| 14 | [Voice Pipeline](14-voice-pipeline.md) | STT/TTS over two paths — a fail-closed HTTP pair and a hardcoded Telegram handler |
| 15 | [Skill Lifecycle Engine](15-skill-lifecycle.md) | 6-stage automated skill extraction and management |
| 16 | [Session Memory Stack](16-session-memory-stack.md) | Pinned instructions + snowball recap + key-fact accumulator |
| 17 | [Wiki Knowledge Layer](17-wiki-knowledge-layer.md) | L0-L3 trust-weighted knowledge with auto-injection |
| 19 | [Agent Client Protocol (ACP/A2A)](19-agent-client-protocol.md) | `duduclaw acp` speaks Agent Client Protocol v1 for IDE panels (Zed / JetBrains / nvim); `duduclaw acp server` is the A2A stdio surface |
| 20 | [Memory Intelligence](20-memory-intelligence.md) | Temporal facts + reflexion loop + batch fetch |
| 23 | [Autopilot Rule Engine](23-autopilot-engine.md) | Event-driven automation + circuit breaker |
| 24 | [Task Board & Activity Feed](24-task-board.md) | Agent-as-teammate task management |
| 25 | [Identity Resolution](25-identity-resolution.md) | WikiCache / Notion / Chained providers (RFC-21 §1) |
| 26 | [MCP HTTP/SSE Transport](26-mcp-http-sse.md) | Bearer-authed REST + SSE endpoints (W20) |
| 27 | [One-shot PTY invocation](27-pty-pool-runtime.md) | Give a CLI a real terminal; the session pool was removed in 2026-09 |
| 28 | [Live Run Forking](28-live-forking.md) | Parallel branches + AI judge; filtered copies, retained branch adoption and test timeout cleanup |
| 29 | [Evolution Events](29-evolution-events.md) | Black-box recorder with batch+retry delivery |
| 30 | [Custom Dashboard Widgets](30-custom-widgets.md) | AI-guided or raw-HTML dashboard cards in a sandbox |
| 31 | [Office Document Suite](31-office-document-suite.md) | Real docx/xlsx/pptx/pdf output with the DELIVER protocol, archive and preview |
| 32 | [Expert Packs](32-expert-packs.md) | Installable AI teams: built-in catalog, LLM-guided authoring, department × rank org placement |
| 33 | [OS-Native Perception & Proactive Care](33-os-native-perception.md) | File watch + frontmost sensing → footprint memory, care checks, one-click automations |
| 34 | [Autonomous Goal Loop](34-goal-loop.md) | /goal → MAV acceptance, recorded round state and costs; weekly survival report shows sample limits |
| 35 | [Photo → Desktop Pet](35-photo-desktop-pet.md) | Local photo-to-pixel-pet pipeline with a Codex-Pets spritesheet and wander engine |
| 36 | [Recording → Skill](36-recording-to-skill.md) | Browser/desktop recordings distilled into approval-gated SKILL.md drafts |
| 37 | [Delegation Isolation](37-delegation-isolation.md) | Org-boundary delegation policy: hierarchy / department / white-list enforcement |
| 38 | [Agentic Evolution Engine & Playbook](38-aee-playbook-evolution.md) | SOUL.md becomes a read-only persona layer; the playbook is what learns and can be retired rule by rule |
| 39 | [Calibrated Forward Model & Held-Out Learning Gate](39-calibrated-forward-model.md) | Every guess gets scored against reality; self-derived lessons stay on the bench until the numbers back them |
| 40 | [Notification Governance](40-notification-governance.md) | Severity levels, quiet-hour deferral, one digest a day, and action-rate metrics for every push |
| 41 | [Resident Sensing & Signal Wake-Up](41-resident-sensing.md) | External data streams wake the AI employee only when a rule actually fires |
| 42 | [Human Takeover](42-human-takeover.md) | An admin typing directly in a channel silences the AI for that one conversation until they hand it back |
| 43 | [Telegram Mini App Approval Card](43-telegram-miniapp.md) | Read the full approval and decide inside Telegram, no dashboard switch (spike, off by default) |
| 44 | [Working State](44-working-state.md) | One authoritative cross-wake record of each AI employee's current rules, changed only through audited tool calls |
| 45 | [Local Model Marketplace](45-local-model-marketplace.md) | Pick a model by purpose, see whether it fits this machine, install in one click |
| 46 | [Belief Loop](46-belief-loop.md) | State a prediction about the outside world, get scored against reality, see your own calibration next time |
| 47 | [Agent Mail](47-agent-mail.md) | Per-agent inbox for incoming mail; nothing goes out until a person confirms it |
| 48 | [Goal Intent Router](48-goal-intent-router.md) | Chat channels notice task delegation and offer to create a goal; never auto-created |
| 49 | [Code Security Audit](49-code-security-audit.md) | `duduclaw secaudit`: static scanners + AI deep audit + adversarial review + sandboxed PoC |
| 50 | [DuDuClaw OS Appliance](50-duduclaw-os-appliance.md) | Bootable appliance image — LAN dashboard onboarding, device page, sysd privilege separation, webhook relay |
| 51 | [OS Keyboard Shortcuts](51-os-keyboard-shortcuts.md) | Every DuDuClaw OS shortcut — global compositor bindings, shell UI, first-run setup, lock screen |
| 52 | [DuDuClaw OS Desktop Edition](52-desktop-edition.md) | One machine shared by a person and the AI — shadow workspace, human input always wins, explicit hand-back, off-by-default co-driving |
| 53 | [Local Models on the Device](53-local-models.md) | Six verified GGUFs, one click to download and one to switch on; hybrid by default, honest about speed |
| 54 | [Fine-tuning and Post-training](54-finetune.md) | Curate the dataset here, train it on a GPU elsewhere, import the GGUF/LoRA back — this machine never trains |
| 55 | [Data Sources & Native DB Connector](55-data-sources.md) | A registry any `db_field` rule can bind to, an MCP proxy that redacts a customer's own MCP servers, and a first-party read-only PostgreSQL/MySQL/SQLite connector |
| 56 | [Team as Employee (Team-as-Agent)](56-team-as-agent.md) | 規劃／執行／審核／合成 four roles inside one employee, each on its own runtime and model — the switch is on by default but naming a second vendor under `[team.roles]` is what forms a team, a decomposability gate decides per task, and roles exchange a TaskPacket instead of a transcript |
| 57 | [UCCI Calibrated Cascade](57-ucci-calibrated-cascade.md) | Experimental, opt-in local-routing tier — an isotonic-calibrated router decides LocalFast→LocalStrong→cloud escalation from token-margin uncertainty instead of the legacy post-hoc confidence gate; fitted offline, off by default |
| 58 | [Night Engine](58-night-engine.md) | Idle-window memory tidy-up: four sub-passes (two deterministic, two on the utility model), a per-pass spend cap and a daily circuit breaker, behind two separate opt-in switches; productive passes appear in Activity Feed |
| 59 | [Local Proxy](59-local-proxy.md) | `duduclaw proxy` — an OpenAI-compatible localhost endpoint so Aider / Cline / Codex can borrow the account pool; Bearer-gated, loopback by default, honest about OAuth seats not being forwardable |
| 60 | [Discovery](60-discovery.md) | Approved-workspace exploration, budgeted tree records, verified artifacts and zero-LLM held-out policy comparisons; integration validation in progress |

---

## Companion Articles

| Article | One-liner |
|---------|-----------|
| [Live Forking: When to Use It](live-forking.md) | Usage-scenario companion to #28 — when to fork, when not to, and how it differs from `duduclaw eval` |
| [ERP / CRM Support Matrix](erp-support-matrix.md) | One-page coverage table for sales and customer conversations |

---

## Translations

- [繁體中文版 (zh-TW)](zh-TW/README.md)
- [日本語版 (ja-JP)](ja-JP/README.md)

---

## Full Feature Inventory

For a complete list of all features (not just highlights), see [feature-inventory.md](feature-inventory.md).
