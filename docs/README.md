# DuDuClaw Documentation

> Public documentation for the DuDuClaw Multi-Runtime AI Agent Platform (v1.21.1).

---

## Feature Highlights

Detailed introductions to DuDuClaw's standout features, with metaphors and flow diagrams for developers.

| Document | Description |
|----------|-------------|
| [features/README.md](features/README.md) | Feature index + full inventory |
| [features/01-prediction-driven-evolution.md](features/01-prediction-driven-evolution.md) | Prediction-driven evolution — 90% zero-cost conversations |
| [features/02-gvu-self-play-loop.md](features/02-gvu-self-play-loop.md) | GVU self-play loop — agent self-improvement pipeline |
| [features/03-confidence-router.md](features/03-confidence-router.md) | Confidence router & local inference — smart model selection |
| [features/04-file-based-ipc.md](features/04-file-based-ipc.md) | File-based IPC — zero-dependency agent communication |
| [features/05-security-defense.md](features/05-security-defense.md) | Three-phase security defense — layered threat filtering |
| [features/06-soul-versioning.md](features/06-soul-versioning.md) | SOUL.md versioning — atomic updates with auto-rollback |
| [features/07-account-rotation.md](features/07-account-rotation.md) | Multi-account rotation — intelligent credential scheduling |
| [features/08-browser-automation.md](features/08-browser-automation.md) | 5-layer browser automation — progressive escalation |
| [features/09-behavioral-contracts.md](features/09-behavioral-contracts.md) | Behavioral contracts — machine-enforceable agent boundaries |
| [features/10-cognitive-memory.md](features/10-cognitive-memory.md) | Cognitive memory — human-inspired memory with forgetting |
| [features/11-token-compression.md](features/11-token-compression.md) | Token compression triad — lossless, lossy, and streaming |
| [features/12-industry-templates.md](features/12-industry-templates.md) | Industry templates & Odoo ERP bridge |
| [features/13-multi-runtime.md](features/13-multi-runtime.md) | Multi-runtime agent execution — Claude / Codex / Gemini / OpenAI |
| [features/14-voice-pipeline.md](features/14-voice-pipeline.md) | Voice pipeline — ASR / TTS / VAD / LiveKit |
| [features/15-skill-lifecycle.md](features/15-skill-lifecycle.md) | Skill lifecycle engine — 7-stage automated extraction |
| [features/16-session-memory-stack.md](features/16-session-memory-stack.md) | Session memory stack — pinned instructions + snowball recap + key facts |
| [features/17-wiki-knowledge-layer.md](features/17-wiki-knowledge-layer.md) | Wiki knowledge layer — L0-L3 trust-weighted auto-injection |
| [features/18-worktree-isolation.md](features/18-worktree-isolation.md) | Git worktree L0 isolation — lightweight per-task sandbox |
| [features/19-agent-client-protocol.md](features/19-agent-client-protocol.md) | Agent Client Protocol — `duduclaw acp` (ACP v1 for IDE panels: Zed / JetBrains / nvim) + `duduclaw acp-server` (A2A over stdio) |
| [features/20-memory-intelligence.md](features/20-memory-intelligence.md) | Memory intelligence — temporal facts + reflexion loop + batch fetch |
| [features/21-governance-layer.md](features/21-governance-layer.md) | Governance layer — policy registry + per-agent quotas |
| [features/22-durability-framework.md](features/22-durability-framework.md) | Durability — idempotency / retry / circuit breaker / checkpoint / DLQ |
| [features/23-autopilot-engine.md](features/23-autopilot-engine.md) | Autopilot rule engine — event-driven automation + circuit breaker |
| [features/24-task-board.md](features/24-task-board.md) | Task Board & Activity Feed — agent-as-teammate task management |
| [features/25-identity-resolution.md](features/25-identity-resolution.md) | Identity resolution — WikiCache / Notion / Chained providers |
| [features/26-mcp-http-sse.md](features/26-mcp-http-sse.md) | MCP HTTP/SSE transport — Bearer-authed REST + SSE |
| [features/27-pty-pool-runtime.md](features/27-pty-pool-runtime.md) | Cross-platform PTY pool + worker — drive interactive claude REPL |
| [features/28-live-forking.md](features/28-live-forking.md) | Live run forking — parallel branches + AI judge (duduclaw-fork) |
| [features/29-evolution-events.md](features/29-evolution-events.md) | Evolution events — black-box recorder with batch+retry delivery |
| [features/30-custom-widgets.md](features/30-custom-widgets.md) | Custom dashboard widgets — sandboxed HTML cards, AI-guided authoring, instance sharing |
| [features/31-office-document-suite.md](features/31-office-document-suite.md) | Office document suite — real docx/xlsx/pptx/pdf output, DELIVER protocol, archive + preview |
| [features/32-expert-packs.md](features/32-expert-packs.md) | Expert packs — installable AI teams, built-in catalog, LLM-guided authoring, org placement |
| [features/33-os-native-perception.md](features/33-os-native-perception.md) | OS-native perception & proactive care — sensing, footprint memory, care checks, automations |
| [features/34-goal-loop.md](features/34-goal-loop.md) | Autonomous goal loop — /goal to completion with an MAV acceptance judge |
| [features/35-photo-desktop-pet.md](features/35-photo-desktop-pet.md) | Photo → desktop pet — local pixel-art pipeline + wander engine |
| [features/36-recording-to-skill.md](features/36-recording-to-skill.md) | Recording → skill — approval-gated skill drafts from browser/desktop recordings |
| [features/37-delegation-isolation.md](features/37-delegation-isolation.md) | Delegation isolation — who can hand work to whom, decided by the reports_to tree, departments, and a white-list |
| [features/38-aee-playbook-evolution.md](features/38-aee-playbook-evolution.md) | Agentic Evolution Engine + playbook — SOUL.md becomes read-only; the playbook learns and retires rule by rule |
| [features/39-calibrated-forward-model.md](features/39-calibrated-forward-model.md) | Calibrated forward model + held-out learning gate — Brier/RPS scoring, Murphy decomposition, luck-vs-skill labels |
| [features/40-notification-governance.md](features/40-notification-governance.md) | Notification governance — four-level escalation ladder, quiet-hour deferral, daily digest, action-rate metrics |
| [features/41-resident-sensing.md](features/41-resident-sensing.md) | Resident sensing — external data streams (http_poll / command / file_tail / websocket) wake agents only on rule hits |
| [features/42-human-takeover.md](features/42-human-takeover.md) | Human takeover — an admin speaking in a channel silences the AI for that conversation until hand-back (`/takeover` lifecycle) |
| [features/43-telegram-miniapp.md](features/43-telegram-miniapp.md) | Telegram Mini App approval card — full approval details + decision inside Telegram, HMAC-verified `initData` (spike, off by default) |
| [features/44-working-state.md](features/44-working-state.md) | Working state — authoritative cross-wake state per agent; tool-only writes with reasons, CAS protection, TTL rules |
| [features/45-local-model-marketplace.md](features/45-local-model-marketplace.md) | Local model marketplace — purpose-based picker, hardware-fit lights, one-click install |
| [features/46-belief-loop.md](features/46-belief-loop.md) | Belief loop — structured predictions about the outside world, deterministically scored against reality |
| [features/47-agent-mail.md](features/47-agent-mail.md) | Agent Mail — per-agent inbox (Gmail / drop folder); outbound mail always requires human confirmation |
| [features/48-goal-intent-router.md](features/48-goal-intent-router.md) | Goal intent router — chat channels notice task delegation and offer to create a goal; never auto-created |
| [features/49-code-security-audit.md](features/49-code-security-audit.md) | Code security audit — `duduclaw secaudit`: static scanners + AI deep audit + adversarial review + sandboxed PoC |
| [features/50-duduclaw-os-appliance.md](features/50-duduclaw-os-appliance.md) | DuDuClaw OS appliance — bootable image, LAN dashboard onboarding, device page, sysd privilege separation, webhook relay |
| [features/51-os-keyboard-shortcuts.md](features/51-os-keyboard-shortcuts.md) | DuDuClaw OS keyboard shortcuts — global compositor bindings, shell UI, first-run setup, lock screen (zh-TW) |
| [features/live-forking.md](features/live-forking.md) | Live forking usage scenarios — when to use, when not to, vs `duduclaw eval` |
| [features/erp-support-matrix.md](features/erp-support-matrix.md) | ERP / CRM support matrix — sales-facing coverage table |

---

## Format Specifications

Open standards that define the DuDuClaw agent ecosystem.

| Document | Description | Status |
|----------|-------------|--------|
| [spec/soul-md-spec.md](spec/soul-md-spec.md) | SOUL.md agent identity format v1.0 | Draft |
| [spec/contract-toml-spec.md](spec/contract-toml-spec.md) | CONTRACT.toml behavioral boundary format v1.0 | Draft |
| [spec/contract-toml-schema.json](spec/contract-toml-schema.json) | CONTRACT.toml JSON Schema | Draft |
| [spec/expert-pack-spec.md](spec/expert-pack-spec.md) | Expert Pack format v1.0 — 可攜「完整 AI 員工/團隊」單位：layout、expert.toml 欄位、安裝語意（拓撲/深合併/hooks 隔離）、與 SKILL.md/AGENTS.md/.af/agent card 的互補定位與映射 | Draft |

## Architecture & Technical Reference

| Document | Description | Status |
|----------|-------------|--------|
| [architecture/overview.md](architecture/overview.md) | System architecture overview | Current |
| [architecture/evolution-engine.md](architecture/evolution-engine.md) | Evolution Engine — Prediction + GVU (legacy SOUL.md path) + AEE/Playbook (v3 default, ch.12) + Cognitive Memory | Current |

## Design Proposals (RFC / ADR)

| Document | Description |
|----------|-------------|
| [rfc/RFC-21-identity-credential-isolation.md](rfc/RFC-21-identity-credential-isolation.md) | Identity resolution & per-agent credential isolation |
| [rfc/RFC-21-operator-guide.md](rfc/RFC-21-operator-guide.md) | RFC-21 operator migration playbook |
| [rfc/RFC-22-multi-agent-coordination-principles.md](rfc/RFC-22-multi-agent-coordination-principles.md) | Multi-agent coordination principles |
| [rfc/RFC-24-decision-continuity.md](rfc/RFC-24-decision-continuity.md) | Cross-session decision/proposal durability (fixes session-chain breakage) |
| [rfc/RFC-26-deep-agents-alignment.md](rfc/RFC-26-deep-agents-alignment.md) | Deep-agents / live-forking alignment |
| [adr/ADR-002-x-duduclaw-capability-negotiation.md](adr/ADR-002-x-duduclaw-capability-negotiation.md) | ACP capability negotiation decision |
| [adr/ADR-003-excluded-channels.md](adr/ADR-003-excluded-channels.md) | Excluded channels (Signal / personal WeChat / Viber) |
| [adr/ADR-004-erp-connector-abstraction.md](adr/ADR-004-erp-connector-abstraction.md) | ERP connector abstraction (`trait ErpConnector`) |
| [adr/ADR-005-document-export.md](adr/ADR-005-document-export.md) | Document export selection (md → Slide / Word / PPT / PDF) |
| [adr/ADR-006-local-ocr.md](adr/ADR-006-local-ocr.md) | Local OCR for sensitive images — measure before choosing |

## Planning (TODO)

| Document | Description |
|----------|-------------|
| [todo/TODO-bootstrap-admin-ws-deadlock.md](todo/TODO-bootstrap-admin-ws-deadlock.md) | 🟠 Bootstrap admin deadlock — a fresh containerised install logs in fine yet spins forever, because `must_change_password` blocks the WS handshake and the passwordless escape needs Personal + loopback |
| [todo/TODO-spawn-env-allowlist-fallout.md](todo/TODO-spawn-env-allowlist-fallout.md) | 🔴 The v1.61.0 spawn-env allowlist silently broke every consumer that relied on inheritance — one instance put a real-money agent on a mock broker, another killed all gateway dispatch |
| [todo/TODO-rate-limit-warning-misread-as-failure.md](todo/TODO-rate-limit-warning-misread-as-failure.md) | 🟡 The CLI's `allowed_warning` quota notice is dropped by the stream parser and reported as a failed call, so successful runs get retried near the ceiling |
| [todo/TODO-client-ws-protocol-mismatch.md](todo/TODO-client-ws-protocol-mismatch.md) | 🟠 VS Code / Chrome / Stream Deck send JSON-RPC 2.0 frames the gateway's `WsFrame` protocol rejects — every dashboard RPC dies as a misleading "connection closed" |
| [todo/TODO-agent-toml-silent-skip.md](todo/TODO-agent-toml-silent-skip.md) | 🟠 One missing `agent.toml` field drops the whole agent with a single WARN — `agents.list` goes empty and `FirstRunGate` shows a working install as brand-new |
| [todo/TODO-agent-honesty.md](todo/TODO-agent-honesty.md) | Agent honesty / anti-hallucination tasks |
| [todo/TODO-agent-cross-invocation-continuity.md](todo/TODO-agent-cross-invocation-continuity.md) | Agent 跨 invocation 行動連續性（否認/遺忘自己排程時的行動）修復 |
| [todo/TODO-dispatch-run-visibility.md](todo/TODO-dispatch-run-visibility.md) | 排程／派工執行紀錄可觀測性——cron 路徑不落 run 紀錄，RunsPage 只看得到頻道對話 |
| [todo/TODO-skill-extraction-cron-path.md](todo/TODO-skill-extraction-cron-path.md) | 技能萃取的排程路徑——cron 場景無使用者回饋，成功訊號分級（判官 accept/成功 run）替代方案已定向 |
| [todo/TODO-cron-scheduler-sleep-drift.md](todo/TODO-cron-scheduler-sleep-drift.md) | CronScheduler 宿主睡眠後永久停擺（monotonic 漂移、健康檢查測不到的靜默失效）——wall-clock 對齊修法定向 |
| [todo/TODO-rfc24-decision-continuity.md](todo/TODO-rfc24-decision-continuity.md) | RFC-24 decision-continuity implementation tracking |
| [todo/TODO-rfc26-live-forking.md](todo/TODO-rfc26-live-forking.md) | RFC-26 live-forking implementation tracking |
| [todo/TODO-telegram-reply-context.md](todo/TODO-telegram-reply-context.md) | Telegram 回覆/引用訊息內容遺失（reply_to_message 未解析）修復 |
| [todo/TODO-channel-quote-context-remaining.md](todo/TODO-channel-quote-context-remaining.md) | 其餘通道引用/回覆上下文缺口（全通道掃描結果）追蹤 |
| [todo/TODO-gateway-store-reopen-per-rpc.md](todo/TODO-gateway-store-reopen-per-rpc.md) | gateway 三個 SQLite store 每次 dashboard RPC 都重開（日誌噪音；已排除洩漏嫌疑） |

## User & Developer Guides

| Document | Description | Status |
|----------|-------------|--------|
| [guides/memory-and-knowledge.md](guides/memory-and-knowledge.md) | 記憶與知識庫完整使用說明（兩者差異、知識庫寫入/分類/L0–L3 層級、自動帶入 vs 主動搜尋、記憶刪除、FAQ） | Current |
| [guides/memory-vs-knowledge.html](guides/memory-vs-knowledge.html) | 記憶 vs 知識庫（終端使用者版，自包含 HTML，可直接寄給客戶；WP5c 全自動分流上線後改寫） | Current |
| [guides/goal-loop.md](guides/goal-loop.md) | 自主目標迴圈（`/goal` 入口、AutonomyLevel 五級、`[goal_loop]`/`[dispatch]`/`[dispatch_guard]` 設定、needs_human 按鈕） | Current |
| [guides/topology-evolution.md](guides/topology-evolution.md) | 半自動拓撲演化（D5，human-gated 路由改派提案、`[topology_evolution]` 設定、觀察期自動回滾、`topology.list` RPC） | Current |
| [guides/line-touch-nfc.md](guides/line-touch-nfc.md) | 實體觸點：QR 桌牌列印、自製 NFC 桌牌（NTAG213 寫入/鎖定）、LINE Touch readiness 檢查清單（藍盾認證＋OA Shop 標籤時程） | Current |
| [guides/build-your-own-pack.md](guides/build-your-own-pack.md) | Expert pack 創作教學（最小可用包 10 分鐘、本機測試迴路、團隊/wiki/requires 進階、convert-teams/claude-plugin 轉出、品質建議） | Current |
| [guides/shortcuts-and-wearables.md](guides/shortcuts-and-wearables.md) | 手腕與穿戴：Apple Watch 捷徑打 HTTP API、Bee=外部 MCP 純設定、Omi/Plaud webhook→`/ingest/transcript` 直灌記憶 | Current |
| [guides/remote-mcp.md](guides/remote-mcp.md) | Remote MCP：claude.ai 自訂連接器直連自家 DuDuClaw（標準 `/mcp` 端點＋OAuth 2.1 流程、scope 收斂模型、tunnel 部署與撤銷） | Current |
| [guides/hardware-requirements.md](guides/hardware-requirements.md) | DuDuClaw OS 硬體需求與相容性指南（硬性條件 x86-64+AVX2／UEFI／SSD、最低/建議/舒適配置表、自組 PC 相容性檢查清單、x86 筆電評估、推薦迷你主機 N100/N305/8845HS、驅動缺口 MT7927/RTL8125、樹莓派/Arduino/ESP32 為何跑不了 OS＋作為 resident sensing 感測端點的接入方式、兩層總矩陣、為何不能用 ARM/Mac 模擬、燒 USB 而非光碟） | Current |
| [guides/deployment-guide.md](guides/deployment-guide.md) | Production deployment (Tailscale/ngrok/Docker/systemd) | Current |
| [guides/development-guide.md](guides/development-guide.md) | Developer setup, agent development, browser automation | Current |
| [guides/custom-mcp-tool.md](guides/custom-mcp-tool.md) | Extending MCP tools — step-by-step guide | Current |
| [guides/mcp-bridge.md](guides/mcp-bridge.md) | Mounting external MCP servers (`[[mcp.external]]`, stdio + Streamable HTTP `url` mounts) + `secret://`/`bearer_token` credentials + per-SaaS recipes (Gmail/Plane/Chatwoot/Invoice Ninja/WooCommerce) | Current |
| [guides/google-workspace-integration.md](guides/google-workspace-integration.md) | Google Workspace 設定指南（選路徑導覽：自建 OAuth client／服務帳號網域委派／Apps Script 橋接三選一，逐步操作 + 11 個 scope 用途一覽 + 設定頁按鈕對照，D5：不預埋官方憑證） | Current |
| [guides/google-mcp.md](guides/google-mcp.md) | Google 官方 remote MCP 掛載（**進階選項**：`preset = "google:<svc>"`、preview 資格與不可出貨條款、與原生工具的覆蓋對照） | Current |
| [guides/docuseal.md](guides/docuseal.md) | DocuSeal 簽署工作流（`duduclaw-docuseal-mcp` 開源 wrapper：10 工具、cloud+self-hosted、webhook 驗簽） | Current |
| [guides/professional-software.md](guides/professional-software.md) | 專業軟體整合（Photoshop / AutoCAD 社群 MCP）：支援矩陣、per-agent `.mcp.json` 安裝、capability 治理、ezdxf 降級、RCE 風險聲明 | Current |
| [guides/google-workspace.md](guides/google-workspace.md) | Native Google Workspace integration — all eight services (Gmail / Calendar / Sheets / Drive / Docs / Slides / Forms / Tasks), nineteen MCP tools, GA APIs only (no preview gate), OAuth setup, draft-only + append-only safety | Current |
| [guides/google-no-oauth-client.md](guides/google-no-oauth-client.md) | Google 兩條免自建 OAuth client 的路徑（服務帳號網域委派、Apps Script 橋接），含覆蓋度差異、安全性質與已排除方案的實測數據 | Current |
| [guides/notion.md](guides/notion.md) | Native Notion integration (search / page read / page append, four MCP tools, OAuth setup, external-knowledge-source boundary) | Current |
| [guides/github.md](guides/github.md) | Native GitHub integration (issue/PR search + read + comment, five MCP tools, OAuth App setup, public-comment safety) | Current |
| [guides/evals.md](guides/evals.md) | Agent behavior evals / regression suite (`duduclaw eval`), CI gate, GVU/AEE yardstick (`--case`/`--exclude-dir`/`--report`) | Current |
| [guides/evolution-switches.md](guides/evolution-switches.md) | Evolution switches — master kill-switch, per-feature toggles, AEE vs legacy SOUL.md path, `strategy`/`noise_band`, freeze/unfreeze | Current |
| [guides/docker.md](guides/docker.md) | Docker build & run | Current |
| [guides/multi-instance.md](guides/multi-instance.md) | Running multiple instances on one machine (DUDUCLAW_HOME / PORT / INSTANCE) | Current |
| [guides/observability.md](guides/observability.md) | OpenTelemetry GenAI tracing + OTLP export (`--features otel`, `[telemetry]` config) | Current |
| [guides/personal-edition-portability.md](guides/personal-edition-portability.md) | 個人版資料可攜：自架 ↔ 代管互轉 | Current |
| [guides/channels-googlechat-teams.md](guides/channels-googlechat-teams.md) | Google Chat & Microsoft Teams channel setup + per-channel formatting/typing matrix | Current |
| [guides/migrate-from.md](guides/migrate-from.md) | 從 OpenClaw / Hermes / paperclip 無痛轉移（`duduclaw migrate-from`，預設 dry-run） | Current |
| [guides/white-label.md](guides/white-label.md) | White-label branding (reseller logo/name) + distributor key console (`/manage/distributors`, `[distributor] issuer_key_path`) | Current |
| [guides/recording-to-skill.md](guides/recording-to-skill.md) | 錄製 → 技能：瀏覽器/桌面示範錄製、HAR 脫敏、蒸餾成 SKILL.md 草稿＋審批安裝（`[capabilities] recording`） | Current |
| [guides/feedback-page.md](guides/feedback-page.md) | 問題回報與建議網頁（GitHub Pages 表單 → issue 預填 → Actions + Haiku 自動分類/格式化/上標籤） | Current |
| [guides/appliance-build.md](guides/appliance-build.md) | Community build guide for the DuDuClaw OS appliance image — `build.sh` usage, Docker/QEMU prerequisites, self-install USB flow, known limitations | Current |

## API Reference

| Document | Description | Status |
|----------|-------------|--------|
| [api/README.md](api/README.md) | WebSocket RPC protocol, JSON-RPC 2.0 interface | Current |
| [api/openapi.yaml](api/openapi.yaml) | OpenAPI specification | Current |

---

## Directory Structure

```
docs/                                  # L1 PUBLIC — product & developer documentation
├── README.md                          # This index
├── architecture/                      # System architecture & engine design (+ ja-JP, zh-TW)
│   ├── overview.md                    #   Architecture overview
│   └── evolution-engine.md            #   Evolution Engine spec (legacy GVU + v3 AEE/Playbook)
├── rfc/                               # Request-for-Comments design proposals
│   ├── RFC-21-identity-credential-isolation.md
│   ├── RFC-21-operator-guide.md
│   ├── RFC-22-multi-agent-coordination-principles.md
│   ├── RFC-24-decision-continuity.md
│   └── RFC-26-deep-agents-alignment.md
├── adr/                               # Architecture Decision Records (+ ja-JP, zh-TW)
│   ├── ADR-002-x-duduclaw-capability-negotiation.md
│   ├── ADR-003-excluded-channels.md
│   ├── ADR-004-erp-connector-abstraction.md
│   ├── ADR-005-document-export.md
│   ├── ADR-006-local-ocr.md
│   └── ADR-007-board-governance-mode.md
├── todo/                              # Public planning / tracking docs
│   ├── TODO-agent-honesty.md
│   └── TODO-rfc26-live-forking.md
├── features/                          # Feature highlight articles (+ ja-JP, zh-TW)
│   ├── README.md
│   ├── feature-inventory.md
│   └── 01-…-50-…                      #   50 feature deep-dives
├── spec/                              # Open format specifications
│   ├── soul-md-spec.md                #   SOUL.md format v1.0
│   ├── contract-toml-spec.md          #   CONTRACT.toml format v1.0
│   └── contract-toml-schema.json
├── guides/                            # User & developer guides (+ ja-JP, zh-TW)
│   ├── deployment-guide.md
│   ├── development-guide.md
│   ├── custom-mcp-tool.md
│   ├── evals.md
│   ├── observability.md
│   ├── docker.md
│   └── appliance-build.md
└── api/
    ├── README.md                      # WebSocket RPC protocol
    └── openapi.yaml                   # OpenAPI spec
```

> **Confidentiality tiers** — `docs/` is **Public**. Internal operational reports (daily/sprint/eval) live under `wiki/` and `reports`-style trees; commercial plans, competitive analysis, and research notes are **Confidential** and kept in the gitignored `commercial/` and `research/` trees. See the project root `CLAUDE.md` → "Documentation Classification & Placement" for the full rule.
