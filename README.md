# DuDuClaw 🐾

> **Multi-Runtime AI Agent Platform** — 統一 Claude / Codex / Gemini 三大 CLI，打造你的多通道 AI 助理

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.9+-blue?logo=python)](https://www.python.org/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-1.15.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)

---

> 🎉 **v1.15.0 — Cross-Platform PTY Pool + Worker**（[Release](https://github.com/zhixuli0406/DuDuClaw/releases/tag/v1.15.0)）
>
> Anthropic 已封鎖 OAuth 訂閱帳號的 `claude -p`（推用戶用真互動式 TUI）；v1.15.0 上了一條跨平台 PTY 通道作為 OAuth 帳號的官方替代路徑，**預設關閉**，per-agent 透過 `agent.toml [runtime] pty_pool_enabled = true` 開啟。
>
> - **新 crate `duduclaw-cli-runtime`** — `portable-pty` 跨平台抽象（ConPTY on Win 10 1809+、openpty on Unix），sentinel-framed in-band 回應協定（不用 scrollback scraping 也不用 sidecar），`PtySession` lifecycle、`PtyPool` per-agent semaphore + idle eviction + 健康檢查 + supervisor + restart policy
> - **新 crate `duduclaw-cli-worker`** — 把 PTY pool 包成 localhost JSON-RPC 子進程（Bearer token + `/healthz`），gateway 可選 in-process 或 out-of-process 兩種模式，後者透過 `[runtime] worker_managed = true` 啟用
> - **Gateway 整合**：`pty_runtime` adapter（`RuntimeMode::{FreshSpawn, PtyPool}` per-agent 路由）、`worker_supervisor` 把 SIGTERM/SIGKILL 順序排進 graceful shutdown future、`runtime_status` 暴露 `GET /api/runtime/status` JSON 端點（loopback-only）
> - **`channel_reply` PTY 分支** — OAuth 帳號走互動式 REPL、API-key 帳號走 `oneshot_pty_invoke + claude -p`，新增 `parse_claude_stream_json_complete` + `StreamDiagnostics` 讓 `channel_failures.jsonl` 有可診斷的 stream-json 統計
> - **Phase 8 Production Observability**：`pty_pool_*` Prometheus counters（acquires / cache-hit / spawn / 三種驅逐原因）、invoke outcome（ok/empty/error/timeout）+ duration histogram、worker health misses + restarts、`pty_pool_managed_worker_active` 模式 gauge
> - **降級安全**：所有 PTY 路徑錯誤都會 fallback 回 legacy `tokio::process::Command + claude -p`；missing worker / 不健康 pool / spawn 失敗都可恢復
> - **跨平台煙霧驗證**：`scripts/smoke-pty-pool.sh`（Unix/macOS）+ `.ps1`（Windows）build runtime + 跑 cli-runtime / gateway routing helper / stream-json parser 單元測試，`CLAUDE_SPIKE=1` 跑活的互動式 spike（消耗 OAuth 配額）

<details>
<summary><strong>v1.10.0 → v1.14.0 累積亮點</strong></summary>

- **v1.14.0** — RFC-23 Sensitive Data Redaction：新 crate `duduclaw-redaction`，內部資料（Odoo / shared wiki / file tools）以 `<REDACT:CATEGORY:hash8>` token 取代後才送 LLM，受信邊界（user channel reply、whitelist 工具 egress）自動還原；AES-256-GCM 加密 SQLite vault（per-agent 32-byte key，0o600 權限）+ TTL 7d 兩階段 GC（mark→30 天 purge）+ 5 個內建 profile + 五層 enable/disable resolver + JSONL audit 10MB rotation
- **v1.13.1** — Odoo Test-Before-Save：`odoo.test` RPC 接受 inline params，Dashboard「測試連線」直接用表單目前的值打 Odoo 不必先儲存；inline credential 留空 fallback 已儲存的金鑰；同樣的 SSRF / HTTPS / db-name 驗證鏈、`scrub_odoo_error()` 截 240 字防 HTML 錯誤頁外洩
- **v1.13.0** — Runtime-health overhaul（16 個 issue / 兩輪修補）：恢復 GVU/SOUL 自我演化、新增 `[prompt] mode = "minimal"` Anthropic Skills 風格系統提示、`[budget] max_input_tokens` 壓縮管線、async session summarizer、TF-IDF wiki 相關性排序、`duduclaw lifecycle flush` 季度冷熱分離 CLI
- **v1.12.x** — W22-P0 ADR-002 `x-duduclaw` capability negotiation（HTTP 422 早期失敗）+ ADR-004 Secret Manager + RFC-22 多 agent 協調修補（agnes 偽造子 agent 回應 / autopilot 大量誤觸發 / channel 路徑 token 未紀錄）+ `duduclaw weekly-report` 子命令
- **v1.11.0** — RFC-21（[Issue #21](https://github.com/zhixuli0406/DuDuClaw/issues/21)）：`duduclaw-identity` crate（IdentityProvider trait + Wiki/Notion/Chained 三實作）+ Odoo per-agent 認證隔離（`OdooConnectorPool` 取代全域 admin 單例）+ shared wiki `.scope.toml` SoT 命名空間政策
- **v1.10.0** — Wiki RL Trust Feedback：`WikiTrustStore` per-agent SQLite trust、`CitationTracker` 雙級 LRU + bounded-time eviction 防 DoS、`WikiJanitor` 每日 pass（自動標 corrected / archive / frontmatter 同步）+ sub-agent turn_id 貫通 + multi-process flock + atomic batch upsert
- **v1.9.4** — `duduclaw-durability` 五大持久性機制（idempotency / retry / circuit breaker / checkpoint / DLQ）+ `duduclaw-governance` PolicyRegistry + MCP HTTP/SSE Transport + LOCOMO 記憶評測系統（每日 03:00 UTC 評測 + 200 筆 golden QA）+ LLM Fallback + Discord RESUME + Web ReliabilityPage

</details>

---

## 什麼是 DuDuClaw？

DuDuClaw 是一個 **Multi-Runtime AI Agent 平台**——同時支援 **Claude Code / Codex / Gemini** 三大 CLI 作為 AI 後端，並透過統一的 `AgentRuntime` trait 實現無縫切換與自動偵測。

它不綁定任何單一 AI 供應商，而是為你的 AI Agent 接上通訊通道、記憶、自我進化、本地推論與帳號管理等完整基礎設施。

核心概念：

- **Multi-Runtime** — `AgentRuntime` trait 統一 Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，per-agent 設定
- **Plumbing = DuDuClaw** — 負責通道路由、session 管理、記憶搜尋、帳號輪替、本地推論等基礎設施
- **橋接 = MCP Protocol** — `duduclaw mcp-server` 作為 MCP Server，將通道與記憶工具暴露給 AI Runtime

```
AI Runtime (brain) — Claude CLI / Codex CLI / Gemini CLI / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat
  ├─ Multi-Runtime — Claude / Codex / Gemini / OpenAI-compat 自動偵測 + per-agent 設定
  ├─ Session Memory Stack — 原生 --resume + Instruction Pinning + Snowball Recap + Key-Fact Accumulator
  ├─ MCP Server — 80+ 工具（通訊、記憶、Agent、Skill、推論、任務、知識庫、ERP），per-agent 註冊
  ├─ Evolution Engine — GVU² 雙迴圈進化 + 預測驅動 + MistakeNotebook
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX / ONNX
  ├─ Voice Pipeline — ASR (SenseVoice / Whisper) + TTS (Piper / MiniMax) + VAD (Silero)
  ├─ Account Rotator — 多 OAuth + API Key 輪替、預算追蹤、健康檢查、Cross-Provider Failover
  ├─ Browser Automation — 5 層自動路由（API Fetch → Scrape → Headless → Sandbox → Computer Use）
  ├─ Worktree Isolation — Git worktree L0 沙箱、原子合併、每 Agent 5 個上限
  ├─ Wiki Knowledge Layer — L0-L3 四層知識架構 + 信任權重 + FTS5 + 自動注入
  ├─ ACP/A2A Server — `duduclaw acp-server` stdio JSON-RPC 2.0，Zed/JetBrains/Neovim 整合
  └─ Web Dashboard — React 19 SPA（23 頁面），透過 rust-embed 嵌入 binary
```

---

## 核心特色

### 通訊與通道

| 特色 | 說明 |
|------|------|
| **七通道支援** | Telegram（long polling）、LINE（webhook）、Discord（Gateway WebSocket，op 6 RESUME + stall watchdog + 1-5s jitter）、Slack（Socket Mode）、WhatsApp（Cloud API）、Feishu（Open Platform v2）、WebChat（WebSocket）|
| **Per-Agent Bot** | 每個 Agent 可擁有獨立的 Bot Token，同平台多 Agent 並行 |
| **通道熱啟停** | Dashboard 新增/移除通道即時生效，無需重啟 gateway |
| **WebChat** | 內建 `/ws/chat` WebSocket 端點，React 前端即時對話 |
| **Generic Webhook** | `POST /webhook/{agent_id}` + HMAC-SHA256 簽章驗證 |
| **Media Pipeline** | 圖片自動縮放（max 1568px）+ MIME 偵測 + Vision 整合 |
| **Sticker 系統** | LINE 貼圖目錄 + 情緒偵測 + Discord emoji 等價映射 |

### AI 執行與推論

| 特色 | 說明 |
|------|------|
| **MCP Server 架構** | `duduclaw mcp-server` 提供 80+ 工具，涵蓋通訊、記憶、Agent 管理、推論、排程、Skill 市場、任務看板、共享知識庫、Odoo ERP。註冊於每個 agent 目錄的 `.mcp.json`（Claude CLI `-p --dangerously-skip-permissions` 僅讀取專案級設定），gateway 啟動時自動建立/修復 |
| **Multi-Runtime** | `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，per-agent 設定 |
| **本地推論引擎** | 統一 `InferenceBackend` trait — llama.cpp（Metal/CUDA/Vulkan）/ mistral.rs（ISQ + PagedAttention）/ Exo P2P 叢集 / llamafile / MLX（Apple Silicon）/ OpenAI-compat HTTP |
| **三層信心路由** | LocalFast → LocalStrong → CloudAPI，基於啟發式信心評分自動分流，CJK-aware token estimation |
| **InferenceManager** | 多模式自動切換：Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API，週期性健康檢查 + 自動 failover |
| **原生多輪 Session** | Claude CLI `--resume` 搭配 SHA-256 確定性 session ID + history-in-prompt fallback（帳號輪替/stale session 自動重試）；Hermes 風格 turn trimming（>800 chars, CJK-safe）；Direct API "system_and_3" 斷點快取策略 |
| **Session 記憶堆疊** | Instruction Pinning（首訊 Haiku 擷取核心任務 → session prompt 尾端注入）+ Snowball Recap（每輪 `<task_recap>` 前置零成本回顧）+ P2 Key-Fact Accumulator（每輪 2-4 則事實 → FTS5 索引 → 注入 top-3，僅 100-150 tokens vs MemGPT 6,500 tokens，−87%）|
| **Claude CLI 輕量路徑** | `call_claude_cli_lightweight()` 以 `--effort medium --max-turns 1 --no-session-persistence --tools ""` 處理 metadata 任務（壓縮、instruction/key-fact 擷取），25-40% 成本節省 |
| **Claude CLI 穩定化旗標** | `--strict-mcp-config`（MCP 隔離）+ `--exclude-dynamic-system-prompt-sections`（跨輪 prompt 穩定，10-15% token 節省），`--bare` 因破壞 OAuth 鑰匙圈於 v1.8.11 移除 |
| **Direct API** | 繞過 CLI 直接呼叫 Anthropic Messages API，`cache_control: ephemeral` 達 95%+ 快取命中率 |
| **Token 壓縮** | Meta-Token（BPE-like 27-47%）、LLMLingua-2（2-5x 有損）、StreamingLLM（無限長對話）|
| **Cross-Provider Failover** | `FailoverManager` 健康追蹤、冷卻、不可重試錯誤偵測 |
| **Cross-Platform PTY Pool**（v1.15.0）| OAuth 帳號專用互動式 REPL 通道 — 跨平台 `portable-pty`（ConPTY on Win 10 1809+、openpty on Unix）+ sentinel-framed in-band 回應協定（無 scrollback scraping / 無 sidecar）+ per-agent semaphore + idle eviction + health-check supervisor + restart policy。預設關閉，per-agent `agent.toml [runtime] pty_pool_enabled = true` 開啟；可選 out-of-process 模式（`worker_managed = true`）把 pool 移到 `duduclaw-cli-worker` 子進程透過 localhost JSON-RPC 通訊 |
| **PTY Pool Observability** | Phase 8 production-rollout 指標 — `pty_pool_*` Prometheus counters（acquires / cache-hit / spawn / 三種驅逐原因 / 4 種 invoke outcome / duration histogram）+ `worker_health_misses_total` + `worker_restarts_total` + `pty_pool_managed_worker_active` 模式 gauge + `GET /api/runtime/status` JSON 端點（loopback-only） |
| **Browser 自動化** | 5 層路由（API Fetch → Static Scrape → Headless Playwright → Sandbox Container → Computer Use），deny-by-default |

### 語音與多媒體

| 特色 | 說明 |
|------|------|
| **ASR 語音辨識** | ONNX SenseVoice（本地）+ Whisper.cpp（本地）+ OpenAI Whisper API |
| **TTS 語音合成** | ONNX Piper（本地）+ MiniMax T2A |
| **VAD 語音活動偵測** | ONNX Silero VAD |
| **Discord 語音頻道** | Songbird 整合，Discord 語音對話 |
| **LiveKit 語音房** | WebRTC 多 Agent 語音會議 |
| **ONNX 嵌入** | BERT WordPiece tokenizer + ONNX Runtime 向量嵌入 |

### Agent 編排與進化

| 特色 | 說明 |
|------|------|
| **Sub-Agent 編排** | `create_agent` / `spawn_agent` / `list_agents` MCP 工具 + `reports_to` 組織層級 + D3.js 架構圖；system prompt 自動注入 "## Your Team" 子 Agent 名冊 + 長回報訊息自動分頁（Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900 byte budget，標籤 `📨 **agent** 的回報 (1/N)`）|
| **跨系統 prompt 注入** | CLAUDE.md + CONTRACT.toml（must_not/must_always）+ SOUL.md + Wiki L0+L1 + key_facts top-3 + pinned_instructions 於 CLI/channel/dispatcher 三條路徑一致注入，Claude/Codex/Gemini/OpenAI 四 runtime 行為對齊 |
| **孤兒回應恢復** | dispatcher 啟動時 `reconcile_orphan_responses` 掃描 `bus_queue.jsonl`，原子重播 crash/Ctrl+C/hotswap 後殘留的 `agent_response` callback |
| **GVU² 雙迴圈進化** | 外迴圈（Behavioral GVU — SOUL.md 進化）+ 內迴圈（Task GVU — 即時任務重試），MistakeNotebook 跨迴圈記憶 |
| **預測驅動進化** | Active Inference + Dual Process Theory，~90% 對話零 LLM 成本；MetaCognition 每 100 預測自校準閾值 |
| **4+2 層驗證** | L1-Format / L2-Metrics / **L2.5-MistakeRegression** / L3-LLMJudge / **L3.5-SandboxCanary** / L4-Safety，前 4 層零成本 |
| **Adaptive Depth** | MetaCognition 驅動 GVU 迭代深度（3-7 輪），根據歷史成功率自動調整 |
| **Deferred GVU** | gradient 累積 + 延遲重試（最多 3 次 deferral，72h 跨度 9-21 輪有效迭代）|
| **ConversationOutcome** | 零 LLM 對話結果偵測（TaskType / Satisfaction / Completion），zh-TW + en 雙語 |
| **SOUL.md 版控** | 24h 觀察期 + 自動回滾，atomic write（SHA-256 fingerprint）|
| **Agent-as-Evaluator** | 獨立 Evaluator Agent（Haiku 成本控制）進行對抗式驗證，結構化 JSON verdict |
| **DelegationEnvelope** | 結構化交接協議 — context / constraints / task_chain / expected_output，向後相容 Raw payload |
| **TaskSpec 工作流** | 多步驟任務規劃 — dependency-aware scheduling / auto-retry（3x）/ replan（最多 2 次）/ persistence |
| **Orchestrator 模板** | 5 步規劃策略（Analyze → Decompose → Delegate → Evaluate → Synthesize）+ 複雜度路由 |
| **Skill 生命週期** | 7 階段管理 — Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| **Skill 自動合成** | 偵測重複領域缺口 → 從情境記憶合成新 Skill → 沙箱試用（TTL 管理）→ 跨 Agent 畢業升級 |
| **Task Board** | SQLite 任務管理 — 狀態/優先級/指派追蹤 + 即時 Activity Feed（WebSocket 推播）|
| **Autopilot 規則引擎** | 自動化任務委派、通知、Skill 觸發 — 支援任務建立/狀態變更/頻道訊息/閒置偵測/Cron 排程 |
| **共享知識庫** | `~/.duduclaw/shared/wiki/` 跨 Agent 共享知識（SOP、政策、產品規格）+ 作者歸屬 |
| **Wiki 知識分層** | Vault-for-LLM 啟發 — L0 Identity / L1 Core（自動注入每次對話）/ L2 Context（每日更新）/ L3 Deep（按需搜尋），每頁附 `trust` (0.0-1.0) 權重；FTS5 unicode61 tokenizer 支援 CJK 全文搜尋；`wiki_dedup` 偵測重複頁、`wiki_graph` 輸出 Mermaid 知識圖 |
| **Wiki 自動注入** | `build_system_prompt()` 自動將 L0+L1 頁面注入 WIKI_CONTEXT；涵蓋 CLI 互動、頻道回覆、dispatcher/cron 三條系統 prompt 組裝路徑，Claude/Codex/Gemini/OpenAI 四 runtime 一致 |
| **Git Worktree L0 隔離** | 每任務獨立 worktree 工作區（比容器沙箱便宜），atomic merge（dry-run pre-check + global `Mutex`），`wt/{agent_id}/{adjective}-{noun}` 友善分支名；每 agent 上限 5 個、全域 20 個；Snap workflow：create → execute → inspect → merge/cleanup |
| **ACP/A2A Protocol Server** | `duduclaw acp-server` 提供 stdio JSON-RPC 2.0 伺服器（`agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`），相容 Agent Client Protocol，支援 Zed / JetBrains / Neovim IDE 整合；輸出 `.well-known/agent.json` AgentCard |
| **Reminder 排程** | 一次性提醒（相對時間 `5m`/`2h`/`1d` 或 ISO 8601 絕對時間），`direct` 靜態訊息或 `agent_callback` 喚醒模式 |

### 可靠性與治理（v1.9.x 新增）

| 特色 | 說明 |
|------|------|
| **`duduclaw-durability` crate** | 五大持久性機制 — idempotency key 管理、指數退避重試（jitter）、三態斷路器（Closed/Open/HalfOpen）、checkpoint 斷點續傳、Dead Letter Queue 終態失敗訊息處理 |
| **`duduclaw-governance` crate** | PolicyRegistry + 4 種 PolicyType（Rate/Permission/Quota/Lifecycle）+ quota_manager（soft/hard 配額）+ error_codes（QUOTA_EXCEEDED / POLICY_DENIED 標準化）+ YAML 熱重載 + audit log |
| **LLM Fallback** | 主模型 timeout/503/429/overloaded 時自動切換 fallback 模型，`is_llm_fallback_error` / `should_attempt_model_fallback` 純函式，hard deadline 統一回傳 hard timeout 錯誤觸發 fallback |
| **Evolution Events 系統** | 30+ event schema、async emitter（batch + retry）、query 介面、reliability 機制；HTTP endpoint 暴露於 gateway，Web ReliabilityPage 視覺化 |
| **MCP HTTP/SSE Transport**（W20-P1/P2）| `duduclaw http-server --bind 127.0.0.1:8765` — `POST /mcp/v1/call`（單次 JSON-RPC 工具呼叫）+ `GET /mcp/v1/stream`（SSE 長連接事件流）+ `POST /mcp/v1/stream/call`（async + SSE push）+ Bearer 認證 + token bucket rate limit |
| **記憶 MCP scope 強制驗證** | `memory:read` / `memory:write` scope 在 `store/read/search` execute() 進入點檢查，修補 v1.9.3 之前任意有效 API Key 可繞過 scope 的認證缺口 |
| **LOCOMO 記憶評測** | `memory_eval/` — retrieval_accuracy / retention_rate / locomo_integrity_check + cron_runner（每日 03:00 UTC）+ 5 分鐘 smoke_test P0 + 200 筆 golden QA 黃金集 |

### 安全防護

| 特色 | 說明 |
|------|------|
| **Claude Code Security Hooks** | 三層漸進式防禦 — Layer 1 黑名單（<50ms）→ Layer 2 混淆偵測（YELLOW+）→ Layer 3 Haiku AI 研判（RED only）|
| **威脅等級狀態機** | GREEN → YELLOW → RED 自動升降級，24h 無事件降一級 |
| **SOUL.md 漂移檢測** | SHA-256 fingerprint 即時比對 |
| **Prompt Injection 掃描** | 6 類規則，XML 分隔標籤防注入 |
| **Secret 洩漏掃描** | 20+ 模式（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL 等）|
| **敏感檔案保護** | Read/Write/Edit 三方向保護 `secret.key`、`.env*`、`SOUL.md`、`CONTRACT.toml` |
| **行為契約** | `CONTRACT.toml` 定義 `must_not` / `must_always` 邊界 + `duduclaw test` 紅隊測試（9 項場景）|
| **統一多源審計日誌** | `audit.unified_log` 合併 4 條 JSONL（`security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl`）為統一信封（timestamp / source / event_type / agent_id / severity / summary / details），Logs 頁面支援來源篩選、嚴重度下拉、即時與歷史分頁 |
| **JSONL 審計日誌** | async 寫入，格式相容 Rust `AuditEvent` schema |
| **CJK-Safe 字串切片** | `truncate_bytes` / `truncate_chars` 新模組取代 31 處 `s[..s.len().min(N)]` byte-index 切片（修復 v1.8.11 多位元組 codepoint panic）|
| **Per-Agent 密鑰隔離** | AES-256-GCM 加密儲存，agent 間密鑰互不可見 |
| **容器沙箱** | Docker / Apple Container（`--network=none`、tmpfs、read-only rootfs、512MB limit）|
| **Browser 自動化** | 5 層路由（API Fetch → Static Scrape → Headless → Sandbox → Computer Use），deny-by-default |

### 帳號與成本

| 特色 | 說明 |
|------|------|
| **雙模式帳號輪替** | OAuth 訂閱（Pro/Team/Max）+ API Key 混合 — 4 策略（Priority/LeastCost/Failover/RoundRobin）|
| **健康追蹤** | Rate limit 冷卻（2min）、帳單耗盡冷卻（24h）、Token 過期追蹤（30d/7d 預警）|
| **成本遙測** | SQLite token 追蹤、快取效率分析、200K 價格懸崖警告、自適應路由（快取效率 <30% 自動切本地）|
| **Claude CLI 二進位探測** | `which_claude()` / `which_claude_in_home()` 掃描 Homebrew（Intel + Apple Silicon）/ Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf shims / NVM 版本目錄，修復 launchd 啟動時找不到 binary 的問題 |
| **結構化失敗分類** | `FailureReason` 枚舉（RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown）+ 分類 zh-TW 訊息 + `channel_failures.jsonl` 審計紀錄 |

### 整合與擴充

| 特色 | 說明 |
|------|------|
| **Odoo ERP 整合** | `duduclaw-odoo` 中間層 — 15 個 MCP 工具（CRM/銷售/庫存/會計/通用搜尋報表），支援 CE/EE，EditionGate 自動偵測。Dashboard 設定頁支援**測試後再儲存**（v1.13.1，credential 留空時 fallback 已儲存金鑰）+ **per-agent 認證隔離**（v1.11.0，`OdooConnectorPool` 取代全域 admin 單例）|
| **Skill 市場** | GitHub Search API 即時索引 + 24h 本地快取 + 安全掃描 + Dashboard 市場頁面 |
| **Prometheus 指標** | `GET /metrics` — requests、tokens、duration histogram、channel status |
| **CronScheduler** | `cron_tasks.jsonl` + cron 表達式，定時任務自動觸發 |
| **ONNX 嵌入** | BERT WordPiece tokenizer + ONNX Runtime 向量嵌入，語意搜尋支援 |
| **Experiment Logger** | Trajectory recording，支援 RL/RLHF 離線分析 |
| **Memory Decay 排程** | 每 24h 背景執行 `run_decay`：低重要度 + 30 天以上歸檔 → 封存 90 天以上永久刪除 |
| **RL Trajectory Collector** | 頻道互動期間寫入 `~/.duduclaw/rl_trajectories.jsonl`，`duduclaw rl` CLI 提供 export/stats/reward 功能，複合獎勵（outcome×0.7 + efficiency×0.2 + overlong×0.1）|
| **Marketplace RPC** | `marketplace.list` 服務真實 MCP 目錄（Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres, SQLite, Memory, Fetch, Brave Search），可透過 `~/.duduclaw/marketplace.json` 合併使用者自訂 |
| **Partner Portal** | SQLite `PartnerStore`（`~/.duduclaw/partner.db`）+ 7 RPCs（profile/stats/customers CRUD）+ 銷售統計 |

### Web Dashboard

| 特色 | 說明 |
|------|------|
| **技術棧** | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui，溫暖 amber 色系 |
| **24 個頁面** | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export / **Reliability**（v1.9.4 新增）|
| **Reliability 儀表板** | circuit breaker 狀態 / retry 統計 / DLQ 佇列深度 / evolution events 即時資料；`/reliability` 路由，整合 `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems` API |
| **即時日誌** | BroadcastLayer tracing → WebSocket 推播，WS 心跳 ping/pong（server 30s / client 25s）+ 60s 空閒關閉 |
| **Logs 歷史頁重寫** | 來源篩選 chips（全部 / 安全 / 工具呼叫 / 通道失敗 / 回饋）+ 即時人次計數 + 嚴重度下拉 + 嚴重度著色左框（emerald/amber/rose）+ 點擊展開 JSON 細節 |
| **Memory 頁 Key Insights** | 第四分頁呈現 P2 Key-Fact Accumulator 累積的結構化洞察（`key_facts` 表）+ `access_count` badge + 時間戳 + 來源 metadata |
| **Memory 頁演化歷史** | SOUL.md 版本歷史 + 前/後度量差異（positive feedback / prediction error / user corrections）+ 狀態徽章（Confirmed / RolledBack / Observing）|
| **Toast 通知系統** | 模組作用域事件匯流排、max-5 queue、自動關閉、暖色系 stone/amber/emerald/rose 變體、尊重 `prefers-reduced-motion` |
| **組織架構圖** | D3.js 互動式 Agent 層級視覺化 |
| **深淺色切換** | 跟隨系統偏好，支援手動切換 |
| **國際化** | zh-TW / en / ja-JP 三語支援（600+ 翻譯鍵）|
| **Skill Market 三分頁** | Marketplace / Shared Skills / My Skills 三分頁架構 + Skill 採用流程 |
| **Autopilot 設定** | 自動化規則建立/管理/監控 + 歷史紀錄檢視 |
| **Session Replay** | 對話回放元件，支援時間軸檢視 |

---

## 競品對比

| | **DuDuClaw** | **OpenClaw** | **IronClaw** | **Moltis** | **Dify** |
|---|---|---|---|---|---|
| 語言 | Rust | TypeScript | Rust | Rust | Python |
| 頻道 | 7 | 25+ | 8 | 5 | 0 (API) |
| Multi-Runtime | **4 後端（Claude/Codex/Gemini/OpenAI）** | - | - | - | 多 LLM |
| MCP Server | **80+ 工具** | - | - | - | - |
| 自我演化引擎 | **GVU² 雙迴圈** | - | - | - | - |
| 本地推論 | **6 後端 + 三層信心路由** | - | - | - | - |
| 語音 (ASR/TTS) | **4 ASR + 4 TTS provider** | - | - | - | - |
| Token 壓縮 | **3 種策略** | - | - | - | - |
| Browser 自動化 | **5 層路由** | - | - | - | - |
| 成本遙測 | **快取效率分析** | - | 基礎 | 基礎 | 基礎 |
| 行為合約 | **CONTRACT.toml + 紅隊** | - | WASM 沙箱 | - | - |
| ERP 整合 | **Odoo 15 工具** | - | - | - | - |
| 安全稽核 | **三層防禦 + Hooks** | CVE-2026-25253 | WASM | 基礎 | 中等 |
| 授權 | **Apache 2.0 (Open Core)** | MIT | 開源 | 開源 | $59+/月 |

---

## Agent 目錄結構

每個 Agent 是一個資料夾，結構與 Claude Code 完全相容：

```
~/.duduclaw/agents/
├── dudu/                    # 主 Agent
│   ├── .claude/             # Claude Code 設定
│   │   └── settings.local.json
│   ├── .mcp.json            # MCP Server 設定（DuDuClaw platform tools + agent 專屬 MCP 如 Playwright）
│   │                        # gateway 啟動時自動建立/修復；Claude CLI `-p` 模式僅讀此檔
│   ├── SOUL.md              # 人格定義（SHA-256 保護）
│   ├── CLAUDE.md            # Claude Code 指引（含 CLAUDE_WIKI 模板）
│   ├── CONTRACT.toml        # 行為契約（must_not / must_always），自動注入 system prompt
│   ├── agent.toml           # DuDuClaw 設定（模型、預算、心跳、runtime、capabilities）
│   ├── SKILLS/              # 技能集（可由進化引擎自動產出）
│   ├── wiki/                # Wiki 知識庫（L0-L3 分層 + trust 權重 + FTS5）
│   ├── memory/              # 每日筆記 + memory.db（預測偏差）+ key_facts 表
│   ├── tasks/               # TaskSpec 工作流持久化（JSON）
│   └── state/               # 運行時狀態（SQLite：sessions.pinned_instructions 等）
│
└── coder/                   # 另一個 Agent
    └── ...
```

使用 `duduclaw migrate` 可將舊版 `agent.toml` 自動轉換為 Claude Code 相容格式。

---

## Security Hooks 安全防禦系統

DuDuClaw 在 Claude Code 的 Hook 系統上建構了三層漸進式防禦：

```
                    ┌─────────────────────────────────────┐
  SessionStart ──→  │ session-init.sh                     │  密鑰權限驗證 + 環境初始化
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  UserPrompt   ──→  │ inject-contract.sh                  │  CONTRACT.toml 規則注入
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ bash-gate.sh (Bash)                 │  Layer 1: 黑名單 (<50ms)
     (Bash)         │   ├─ Layer 2: 混淆偵測 (YELLOW+)    │  Layer 2: base64/eval/外滲
                    │   └─ Layer 3: Haiku AI (RED only)   │  Layer 3: AI 安全研判
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ file-protect.sh → ai-review.sh     │  敏感檔案保護 + AI 審查
  (Write|Edit|Read) └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PostToolUse  ──→  │ secret-scanner.sh → audit-logger.sh │  Secret 掃描 → async 審計
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  Stop         ──→  │ threat-eval.sh                      │  威脅等級重新評估
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  ConfigChange ──→  │ config-guard.sh                     │  配置篡改偵測
                    └─────────────────────────────────────┘
```

### 威脅等級狀態機

| 等級 | 觸發條件 | 防禦行為 |
|------|---------|---------|
| **GREEN** (預設) | 正常操作 | Layer 1 黑名單 + 檔案保護 + Secret 掃描 |
| **YELLOW** | 1 小時內 ≥ 2 次攔截 | +Layer 2 混淆偵測 + 外部網路限制 |
| **RED** | 偵測到注入/eval 攻擊 | +Layer 3 Haiku AI 研判所有命令 + AI 檔案審查 |

降級：24 小時無事件自動降一級（RED→YELLOW→GREEN）。

---

## 安裝

### npm（推薦）

```bash
npm install -g duduclaw
```

安裝完成後會自動下載對應平台的預編譯 binary（支援 macOS ARM64/x64、Linux x64/ARM64、Windows x64）。

### Homebrew（macOS / Linux）

```bash
brew install zhixuli0406/tap/duduclaw
```

### 一行安裝

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

### Python SDK（必要依賴）

DuDuClaw 的進化引擎（Skill Vetter）與部分通道橋接需要 Python 環境：

```bash
pip install duduclaw
```

此命令會安裝以下必要依賴：

| 套件 | 最低版本 | 用途 |
|------|---------|------|
| `anthropic` | ≥ 0.40 | Claude API 直接呼叫、Skill 安全掃描 |
| `httpx` | ≥ 0.27 | 非同步 HTTP 客戶端（帳號輪替、健康檢查）|

開發環境額外安裝：

```bash
pip install duduclaw[dev]
# 包含：pytest>=8, pytest-asyncio>=0.24, ruff>=0.8
```

### 從原始碼建構

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# 安裝 Python 依賴
pip install duduclaw

# 建構 Dashboard
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# 建構 Rust binary（含 Dashboard）
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# 首次設定
./target/release/duduclaw onboard

# 啟動
./target/release/duduclaw run
```

> **前置需求**：[Rust](https://rustup.rs/) 1.85+、[Python](https://www.python.org/) 3.9+、[Node.js](https://nodejs.org/) 20+，以及至少一個 AI CLI：[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Gemini CLI](https://github.com/google-gemini/gemini-cli)（擇一或多個）

---

## CLI 指令

```
duduclaw onboard             # 互動式首次設定
duduclaw run                 # 一鍵啟動（gateway + channels + heartbeat + cron + dispatcher）
duduclaw migrate             # 將 agent.toml 轉換為 Claude Code 格式
duduclaw mcp-server          # 啟動 MCP Server（供 AI Runtime 使用，stdio JSON-RPC 2.0）
duduclaw http-server         # 啟動 MCP HTTP/SSE Transport（Bearer 認證，預設 127.0.0.1:8765）
duduclaw acp-server          # 啟動 ACP/A2A Server（IDE 整合：Zed/JetBrains/Neovim）
duduclaw gateway             # 僅啟動 WebSocket gateway server

duduclaw agent               # CLI 互動式對話
duduclaw agent list          # 列出所有 Agent
duduclaw agent create        # 建立新 Agent（可指定產業模板）
duduclaw agent inspect       # 檢視 Agent 詳情
duduclaw agent pause         # 暫停 Agent
duduclaw agent resume        # 恢復 Agent
duduclaw agent edit          # 編輯 Agent 設定
duduclaw agent remove        # 移除 Agent

duduclaw test <agent>        # 紅隊安全測試（9 項內建場景 + JSON 報告）
duduclaw status              # 系統健康快照
duduclaw doctor              # 健康診斷
duduclaw wizard              # 產業模板互動式設定
duduclaw evolution finalize  # 一次性回收逾期 SOUL.md 觀察視窗（--dry-run / --agent <id>）

duduclaw rl export           # 匯出 RL trajectory（~/.duduclaw/rl_trajectories.jsonl）
duduclaw rl stats            # 每 Agent trajectory 統計
duduclaw rl reward           # 計算複合獎勵（outcome×0.7 + efficiency×0.2 + overlong×0.1）

duduclaw service install     # 安裝為系統服務
duduclaw service start/stop  # 啟停系統服務
duduclaw service status      # 服務狀態
duduclaw service logs        # 服務日誌
duduclaw service uninstall   # 移除系統服務

duduclaw license activate    # 啟用授權
duduclaw license status      # 授權狀態
duduclaw license verify      # 驗證授權
duduclaw update              # 檢查並安裝更新
duduclaw version             # 版本資訊
```

---

## 專案結構

```
DuDuClaw/
├── crates/                         # Rust crates (20 個)
│   ├── duduclaw-core/              # 共用型別、traits (Channel, MemoryEngine)、錯誤定義
│   ├── duduclaw-agent/             # Agent 註冊、心跳、預算、契約、skill loader/registry
│   ├── duduclaw-auth/              # 多用戶認證（Argon2 密碼、JWT、ACL 角色權限）
│   ├── duduclaw-security/          # AES-256-GCM、SOUL guard、input guard、audit、key vault
│   ├── duduclaw-container/         # Docker / Apple Container / WSL2 沙箱執行
│   ├── duduclaw-memory/            # SQLite + FTS5 全文搜尋 + 向量嵌入 + 評測 batch query API
│   ├── duduclaw-inference/         # 本地推論引擎（llama.cpp / mistral.rs / ONNX / Exo / llamafile）
│   ├── duduclaw-gateway/           # Axum 伺服器、7 通道、session、GVU²、prediction、cron、dispatcher、LLM fallback、evolution events、PTY pool 整合
│   ├── duduclaw-bus/               # tokio broadcast + mpsc 訊息路由
│   ├── duduclaw-bridge/            # PyO3 Rust↔Python 橋接層
│   ├── duduclaw-odoo/              # Odoo ERP 中間層 (JSON-RPC, CE/EE, 15 MCP tools)
│   ├── duduclaw-cli/               # clap CLI 入口 + MCP server (stdio + HTTP/SSE) + migrate + test
│   ├── duduclaw-dashboard/         # rust-embed 嵌入 React SPA
│   ├── duduclaw-desktop/           # 桌面端 wrapper（macOS/Windows/Linux）
│   ├── duduclaw-durability/        # 持久性框架（idempotency / retry / circuit breaker / checkpoint / DLQ）— v1.9.4 新增
│   ├── duduclaw-governance/        # PolicyRegistry / quota_manager / error_codes / audit / approval — v1.9.4 新增
│   ├── duduclaw-identity/          # IdentityProvider trait + Wiki/Notion/Chained 三實作 — v1.11.0 新增
│   ├── duduclaw-redaction/         # 來源感知 redaction + 可還原 vault（AES-256-GCM）+ 5 profile + JSONL audit — v1.14.0 新增
│   ├── duduclaw-cli-runtime/       # 跨平台 PTY pool runtime（portable-pty / sentinel-framed）— v1.15.0 新增
│   └── duduclaw-cli-worker/        # standalone PTY pool worker subprocess（localhost JSON-RPC + Bearer token）— v1.15.0 新增
│
├── python/duduclaw/                # Python 擴充層
│   ├── channels/                   # LINE / Telegram / Discord 通道插件
│   ├── sdk/                        # Claude Code SDK chat + 多帳號輪替
│   ├── evolution/                  # Skill Vetter 安全掃描
│   ├── tools/                      # Agent 動態管理工具
│   ├── agents/                     # capability manifest + capability-based router + memory_resolver（v1.9.4）
│   ├── mcp/                        # MCP API Key auth（含 key masking）+ memory tools（store/read/search/namespace/quota）
│   └── memory_eval/                # LOCOMO 記憶評測（retrieval/retention + cron + 200 筆 golden QA）— v1.9.4 新增
│
├── npm/                            # npm 發布套件
│   ├── duduclaw/                   # 主套件（平台無關 wrapper + postinstall binary 下載）
│   ├── darwin-arm64/               # macOS Apple Silicon 預編譯 binary
│   ├── darwin-x64/                 # macOS Intel 預編譯 binary
│   ├── linux-x64/                  # Linux x86-64 預編譯 binary
│   ├── linux-arm64/                # Linux ARM64 預編譯 binary
│   └── win32-x64/                  # Windows x64 預編譯 binary
│
├── web/                            # React Dashboard
│   └── src/
│       ├── components/             # UI 元件 (OrgChart, ApprovalModal, SessionReplay)
│       ├── pages/                  # 24 個頁面（含 ReliabilityPage v1.9.4 新增）
│       ├── stores/                 # Zustand 狀態管理 (8 stores)
│       ├── lib/                    # API client (WebSocket JSON-RPC + evolution events / reliability HTTP)
│       └── i18n/                   # zh-TW / en / ja-JP
│
├── templates/                      # 產業模板 + Agent 角色模板
│   ├── restaurant/                 # 餐飲業（客服、訂位、FAQ、主動推播）
│   ├── manufacturing/              # 製造業（設備監控、SOP、異常告警）
│   ├── trading/                    # 貿易業（報價、訂單、庫存、價目表）
│   ├── evaluator/                  # Evaluator Agent（對抗式驗證）
│   ├── orchestrator/               # Orchestrator Agent（任務編排）
│   └── wiki/                       # Wiki 知識庫模板
│
├── .claude/                        # Claude Code Hook 安全系統
│   ├── settings.local.json         # Hook 設定（6 事件 × 10 腳本）
│   └── hooks/                      # 三層漸進式防禦腳本
│
├── docs/                           # 公開文件
│   ├── spec/                       # 格式規範（SOUL.md / CONTRACT.toml）
│   ├── api/                        # WebSocket RPC + OpenAPI spec
│   ├── guides/                     # 開發指南（自訂 MCP 工具等）
│   └── *.md                        # 架構、部署、進化引擎等
│
├── ARCHITECTURE.md                 # 完整架構設計文件
└── CLAUDE.md                       # AI 協作設計上下文
```

---

## 技術決策

| 項目 | 選擇 | 理由 |
|------|------|------|
| AI 對話 | **Multi-Runtime（Claude / Codex / Gemini CLI）** | 不綁定單一供應商、自動偵測 + per-agent 設定 |
| 核心語言 | **Rust** | 記憶體安全、高效能、單 binary 部署 |
| 擴充語言 | **Python (PyO3)** | Claude Code SDK 整合、通道插件彈性 |
| 前端框架 | **React 19 + TypeScript** | 即時資料更新、生態成熟 |
| UI 風格 | **shadcn/ui + Tailwind CSS 4** | 溫暖可自訂、效能佳 |
| 資料庫 | **SQLite + FTS5** | 零依賴、嵌入式、全文搜尋 |
| 工具協議 | **MCP (Model Context Protocol)** | Claude Code 原生支援、stdin/stdout JSON-RPC |
| 本地推論 | **ONNX Runtime + llama.cpp** | 跨平台、Metal/CUDA/Vulkan GPU 加速 |
| 語音辨識 | **SenseVoice + Whisper.cpp** | 多語言、本地離線、零 API 成本 |
| 即時通訊 | **WebRTC (LiveKit)** | 低延遲語音、多人會議 |

---

## 測試

```bash
# Rust 測試
cargo test --workspace --exclude duduclaw-bridge

# Python 測試
pip install pytest pytest-asyncio ruff
ruff check python/
pytest tests/python/ -v

# 前端型別檢查
cd web && npx tsc --noEmit
```

---

## 文件

- [ARCHITECTURE.md](ARCHITECTURE.md) — 完整系統架構設計
- [CLAUDE.md](CLAUDE.md) — AI 協作設計上下文與原則
- [CHANGELOG.md](CHANGELOG.md) — 版本變更紀錄
- [docs/features/README.md](docs/features/README.md) — 特色功能詳解（19 篇，含 zh-TW / ja-JP 翻譯）
- [docs/features/feature-inventory.md](docs/features/feature-inventory.md) — 完整功能清單
- [docs/spec/soul-md-spec.md](docs/spec/soul-md-spec.md) — SOUL.md 格式規範 v1.0
- [docs/spec/contract-toml-spec.md](docs/spec/contract-toml-spec.md) — CONTRACT.toml 格式規範 v1.0
- [docs/api/README.md](docs/api/README.md) — WebSocket RPC 協議 + JSON-RPC 2.0 介面
- [docs/evolution-engine.md](docs/evolution-engine.md) — Evolution Engine v2 設計文件
- [docs/deployment-guide.md](docs/deployment-guide.md) — 生產環境部署指南
- [docs/development-guide.md](docs/development-guide.md) — 開發者設定與 Agent 開發
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md) — 自訂 MCP 工具教學

---

## 授權

**Open Core 模式** — 核心程式碼採用 [Apache License 2.0](LICENSE)，完全自由使用、修改、分發。

商業加值模組（`commercial/` 目錄）為閉源付費，包含：產業模板、演化參數集、企業儀表板、授權驗證。

詳見 [LICENSING.md](LICENSING.md)。

---

<p align="center">
  🐾 Built with louis.li
</p>
