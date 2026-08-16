# DuDuClaw 架構總覽

## 架構總覽（v1.13.1）

DuDuClaw 是一套**多執行環境 AI Agent 平台（Multi-Runtime AI Agent Platform）**，透過統一的 `AgentRuntime` trait 支援 **Claude Code / Codex / Gemini** CLI 作為 AI 後端，具備自動偵測與逐 Agent 設定能力。DuDuClaw 並非獨立的 LLM 產品；它是把一個（或多個）AI CLI 轉變成長駐運作 Agent 的管線層，涵蓋通道路由、對話記憶、自我演化、多帳號輪替、本機 LLM 推理、瀏覽器自動化與 IDE 整合。

## 關鍵架構決策

### 執行環境與傳輸層
- **Multi-Runtime**（`AgentRuntime` trait）— Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，逐 Agent 設定寫在 `agent.toml [runtime]`。
- **MCP Server（stdio）**（`duduclaw mcp-server`）透過 stdin/stdout 上的 JSON-RPC 2.0，把通道、記憶、Agent、skill、task、共用 wiki、autopilot 等工具暴露給 AI Runtime。註冊層級在 Agent 端的 `<agent>/.mcp.json`（v1.8.5 撤回了 v1.8.4 的全域註冊，因為 Claude CLI `-p --dangerously-skip-permissions` 只會讀取專案層級的 `.mcp.json`）。Gateway 啟動時會自動為所有 Agent 建立／修復 `.mcp.json`。
- **MCP Server（HTTP/SSE）**（`duduclaw http-server --bind 127.0.0.1:8765`，v1.9.4）— Bearer 驗證的 `POST /mcp/v1/call`（單次 JSON-RPC 工具呼叫）、`GET /mcp/v1/stream`（長駐 SSE 事件串流，Bearer 或 `?api_key=`）、`POST /mcp/v1/stream/call`（非同步 + SSE 結果推送）、`GET /healthz`（免驗證）。Token bucket 速率限制（60 req/min）。`mcp_sse_store.rs` 用 broadcast channel 管理 SSE 連線。與 stdio 互補，服務外部 HTTP client。
- **ACP/A2A Server**（`duduclaw acp-server`）— stdio JSON-RPC 2.0 迴圈，提供 `agent/discover`、`tasks/send`、`tasks/get`、`tasks/cancel` 方法，並輸出 `.well-known/agent.json` AgentCard。透過 Agent Client Protocol 支援 Zed / JetBrains / Neovim 等 IDE 整合。
- **Agent 目錄**與 Claude Code 相容：每個目錄都包含 `.claude/`、`.mcp.json`、`SOUL.md`、`CLAUDE.md`、`CONTRACT.toml`、`agent.toml`、`wiki/`、`SKILLS/`、`memory/`、`tasks/`、`state/`。

### 通道（7 個 + 通用 Webhook）
- **Telegram**（long polling）— 檔案／照片／貼圖／語音、群組話題（forums/topics）、僅限提及（mention-only）、Whisper 轉錄。
- **LINE**（webhook）— HMAC-SHA256 簽章、貼圖目錄、逐聊天室設定。
- **Discord**（Gateway WebSocket）— `tokio::select!` 心跳、斜線指令、自動建立討論串、語音頻道（Songbird）。v1.9.2 強化：真正的 op 6 RESUME（持久化 `session_id` + `resume_gateway_url` + sequence）、停滯監看（超過 2 倍心跳間隔無流量即中斷）、心跳 channel 容量 1→16 並改用 `try_send`、op 9 加上 1-5 秒抖動、處理 RESUMED dispatch、backoff 上限降為 60 秒。
- **Slack**（Socket Mode）、**WhatsApp**（Cloud API）、**Feishu**（Open Platform v2）、**WebChat**（`/ws/chat` + React 前端）。
- **通用 Webhook**：`POST /webhook/{agent_id}` + HMAC-SHA256。
- **通道熱啟動／熱停止**：Dashboard 的 `channels.add` / `channels.remove` 可直接啟動／中止通道任務，不需重啟 gateway。
- **媒體管線**：圖片自動縮放（最大 1568px）+ MIME 偵測 + Vision 整合。

### 子 Agent 協作編排
- `create_agent` / `spawn_agent` / `list_agents` MCP 工具，搭配 `reports_to` 階層。
- System prompt 會自動注入「## Your Team」子 Agent 名冊。
- **結構化交接**：`DelegationEnvelope`（context / constraints / task_chain / expected_output），失敗時退回 Raw 格式。
- **TaskSpec 工作流**：多步驟任務規劃，具備依賴感知排程、自動重試（3 次）、重新規劃（2 次）、持久化。
- **長回應切分**：子 Agent 回覆超過通道位元組上限時，會用 `channel_format::split_text` 切分，並附上 `📨 **agent** 的回報 (1/N)` / `(續 2/N)` 標籤（Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900）。
- **孤兒回應復原**：`reconcile_orphan_responses` 會以原子方式重播 crash／Ctrl+C／熱替換遺留下的 `bus_queue.jsonl` 紀錄。

### 對話記憶堆疊
- **原生多輪對話**：Claude CLI `--resume <session-id>`，搭配 SHA-256 決定性 session ID；`--resume` 失敗時（過期 handle、帳號輪替、未知 stream-json 錯誤）自動退回歷史注入 prompt 的方式。
- **逐輪裁剪（Turn trimming）**（超過 800 字元 → 保留頭 300 + 尾 200 + `[trimmed N chars]`，CJK 安全）。
- **Direct API prompt 快取**（"system_and_3" 斷點策略，多輪對話約 75% 命中率；純 system-prompt 快取則 95%+）。
- **壓縮摘要**在達到 50k token 門檻時注入 system prompt（而非對話輪次本身）。
- **Instruction Pinning**（v1.8.6 P0）— 使用者第一輪 → 非同步以 Haiku 萃取核心任務 → 存入 `sessions.pinned_instructions` → 注入到 system prompt 尾端（U 形注意力）。澄清回答會持續累積（≤1000 字元）。
- **Snowball Recap**（v1.8.6 P0）— 每一輪都在使用者訊息前加上 `<task_recap>`。零 LLM 成本。
- **P2 Key-Fact Accumulator**（v1.8.6）— 每個有實質內容的輪次，由 Haiku 萃取 2-4 條關鍵事實 → 存入具 FTS5 索引的 `key_facts` 表 → 挑最相關的前 3 條注入 system prompt。約 100-150 token，相較 MemGPT 的 6,500 token（−87%）。
- **CLI 輕量路徑**— `call_claude_cli_lightweight()` 搭配 `--effort medium --max-turns 1 --no-session-persistence --tools ""`，用於 metadata 任務。可降低 25-40% 成本。
- **穩定化旗標**— `--strict-mcp-config`（MCP 隔離）+ `--exclude-dynamic-system-prompt-sections`（跨輪次 prompt 穩定性，減少 10-15% token）。`--bare` 已於 v1.8.11 移除（會破壞 OS 鑰匙圈憑證查詢）。

### 演化
- **預測驅動引擎**：Active Inference + Dual Process Theory，約 90% 對話零 LLM 成本。可忽略／中等誤差 → 零成本；顯著誤差 → 觸發 GVU 反思；嚴重誤差 → 觸發緊急 GVU 迴圈。
- **MetaCognition**：每 100 次預測自我校準誤差閾值一次；驅動 Adaptive Depth（3-7 輪 GVU）。
- **GVU² 自我博弈迴圈**（Generator→Verifier→Updater）：TextGrad 回饋，4+2 層驗證（L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety）。**自 Evolution v3 起為非預設的 legacy 路徑**（`agent.toml [evolution] legacy_soul_evolution = true`）— 詳見下方 AEE。
- **Deferred GVU**：梯度累積 + 延遲重試（最多延後 3 次、72 小時區間、相當於 9-21 次有效輪次）。
- **MistakeNotebook**：跨迴圈的錯誤記憶，防止退化；條目現在帶有決定性的 `TrajectoryEvidence`（哪個工具／斷言失敗），使反思整併不再輕信未經查證的自陳診斷（Evolution v3）。
- **SOUL.md 版本控制**：24 小時觀察期 + 自動回滾，寫入採原子操作（暫存檔 + rename）並附 SHA-256 指紋。此機制適用於上方的 legacy GVU 路徑；**自 Evolution v3 起 `SOUL.md` 對 Agent 預設為唯讀**（僅操作者／dashboard 可寫入）。
- **AEE（Agentic Evolution Engine，v3 預設路徑）**：預設的演化目標已從 `SOUL.md` 換成 **playbook**：由一組體積小、可個別淘汰、基因狀結構的條目組成（category／signals／關聯 eval case），透過 Gate（決定性、保留否決權）與 Measure（計分、無否決權）的分離、champion + 持平或優於才提交（matches-or-improves）的提交閘，以及條目層級（而非整份檔案）的觀察期來演化。詳見 `evolution-engine.md` 第十二章與 `../../features/zh-TW/38-aee-playbook-evolution.md`。
- **Agent-as-Evaluator**：獨立的 Evaluator Agent（以 Haiku 控制成本），進行對抗式驗證並輸出結構化 JSON 判定。
- **ConversationOutcome**：零 LLM 成本的對話結果偵測（TaskType / Satisfaction / Completion），支援 zh-TW + en 雙語。
- **外部因子**：使用者回饋、安全事件、通道指標、Odoo 商業情境、同儕 Agent 訊號，皆會餵入預測引擎與 GVU 反思。

### Wiki 知識層（v1.8.9）
- **四層架構**（受 Vault-for-LLM 啟發）：L0 Identity / L1 Core / L2 Context / L3 Deep。
- **信任權重**（frontmatter 中的 `trust`，0.0-1.0）— 搜尋結果依信任加權分數排序。
- **自動注入**：`build_system_prompt()` 會把 L0+L1 頁面自動注入 WIKI_CONTEXT，涵蓋 CLI／通道回覆／dispatcher 三條路徑，在 Claude / Codex / Gemini / OpenAI 各 runtime 間保持一致。
- **FTS5 索引**（`unicode61` tokenizer）— 每次寫入／刪除都自動同步，也可透過 `wiki_rebuild_fts` 手動重建。
- **知識圖譜**：`wiki_graph` MCP 工具匯出限制 BFS 深度的 Mermaid 圖；節點形狀依層級區分。
- **去重偵測**：`wiki_dedup` 透過標題比對 + 標籤 Jaccard 相似度（≥0.8）偵測重複頁面。
- **反向 backlink 索引**：掃描 `related` frontmatter 與內文 markdown 連結，建立雙向對應。
- **搜尋篩選**：`wiki_search` / `shared_wiki_search` 支援 `min_trust`、`layer`、`expand`（1-hop backlink 展開）。
- **Shared Wiki**：`~/.duduclaw/shared/wiki/` 存放跨 Agent 的 SOP、政策、產品規格。可見性由 `wiki_visible_to` capability 控制。

### 記憶系統
- **認知記憶**（選用）：`SqliteMemoryEngine`，情節／語意記憶分離，採 Generative Agents 三維加權檢索（Recency × Importance × Relevance）。
- **記憶衰減每日排程**：背景任務每 24 小時執行一次 `duduclaw_memory::decay::run_decay`。低重要性 + 滿 30 天 → 歸檔。已歸檔 + 滿 90 天 → 永久刪除。
- **認知記憶 MCP 工具**：`memory_search_by_layer`（情節／語意篩選）、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status`。
- **MemGPT 三層系統**（Core Memory、Recall Memory、Archival Bridge、Budget Manager、Consolidation Pipeline，共 6 個 MCP 工具）**已於 v1.8.1 移除**（−1,985 行程式碼）— 該注入方式讓每個 prompt 膨脹 6,500 token，並造成「lost in the middle」注意力衰退。

### Worktree 隔離（v1.6.0）
- **Git worktree L0 隔離層**— 逐任務檔案系統隔離，成本比容器沙箱低。
- **WorktreeManager**：create / remove / list / cleanup_stale 生命週期管理。
- **原子合併**：dry-run 預檢 → abort → 乾淨才真正合併。以全域 `Mutex` 保護。
- **Snap 工作流**：create → execute → inspect → merge/cleanup（判斷邏輯採純函式設計，便於測試）。
- **分支命名**：`wt/{agent_id}/{adjective}-{noun}`，取自 50×50 字詞清單。
- **copy_env_files**：路徑穿越防護（path traversal jail）+ 拒絕 symlink + 1MB 大小上限。
- **資源上限**：每個 Agent 最多 5 個 worktree，總計上限 20 個。

### 本機推理
- **統一 `InferenceBackend` trait**（`duduclaw-inference` crate）：llama.cpp（Metal/CUDA/Vulkan/CPU）、mistral.rs（ISQ + PagedAttention + Speculative Decoding）、OpenAI 相容 HTTP（Exo/llamafile/vLLM/SGLang）。
- **Confidence Router**：LocalFast / LocalStrong / CloudAPI 三層路由，具備 CJK 感知的 token 估算。
- **InferenceManager**：自動切換的狀態機：Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API。
- **Exo P2P 叢集**（`exo_cluster.rs`）：分散式推理，可跨機器運行 235B+ 參數模型。
- **llamafile manager**：子行程生命週期管理、健康監測、在 localhost 提供 OpenAI 相容 API。
- **MLX bridge**：在 Apple Silicon 上以 Python 子行程呼叫 `mlx_lm`，用於本機反思 + LoRA。
- **MCP 工具**：`model_list`、`model_load`、`model_unload`、`inference_status`、`hardware_info`、`route_query`、`inference_mode`、`llamafile_start/stop/list`、`compress_text`、`decompress_text`。

### Token 壓縮
- **Meta-Token（LTSC）**— Rust 原生、無損、類 BPE 演算法，對結構化輸入可壓縮 27-47%。
- **LLMLingua-2**— 微軟的 token 重要性剪枝法，可做到 2-5 倍有損壓縮。
- **StreamingLLM**— attention sink + 滑動視窗 KV-cache。
- **策略選擇器**：`compress_text` 接受 `strategy` 參數（meta_token / llmlingua / streaming_llm / auto）。

### 語音管線
- **ASR**：Whisper.cpp（本機）/ SenseVoice ONNX（本機）/ OpenAI Whisper API / Deepgram（串流）。
- **TTS**：Piper ONNX（本機）/ MiniMax T2A / Edge TTS / OpenAI TTS。
- **VAD**：Silero ONNX。
- **音訊解碼**：symphonia（OGG Opus、MP3、AAC、WAV、FLAC → PCM）。
- **Discord Voice**（Songbird）+ **LiveKit** 多 Agent 語音房。
- **ONNX Embedding**：BERT WordPiece tokenizer + ONNX Runtime 向量嵌入。

### 安全性
- **Claude Code 安全 hooks**（`.claude/hooks/`）：三階段漸進式防禦，分別是 Layer 1 決定性黑名單（<50ms）、Layer 2 混淆／外洩偵測（YELLOW 以上觸發）、Layer 3 Haiku AI 判斷（僅 RED 觸發）。
- **威脅等級狀態機**：GREEN → YELLOW → RED，自動升級／降級（24 小時無事件 → 降 1 級）。
- **SOUL.md 漂移偵測**（SHA-256 指紋）。
- **Prompt injection 掃描器**（6 類規則 + XML 分隔符保護）。
- **機密外洩掃描器**— 20+ 種樣式（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/資料庫連線字串）。
- **CONTRACT.toml**— `must_not` / `must_always` 邊界規則，自動注入 system prompt；`duduclaw test` 紅隊測試 CLI（內建 9 種情境）。
- **統一多來源稽核日誌**：`audit.unified_log` 把 `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` 整併成統一格式（timestamp / source / event_type / agent_id / severity / summary / details），並在 Logs 頁提供篩選 chip。
- **AES-256-GCM** 靜態加密— 逐 Agent 金鑰隔離。
- **Ed25519 challenge-response** WebSocket 驗證。
- **容器沙箱**（Docker / Apple Container / WSL2）— `--network=none`、tmpfs、唯讀 rootfs、512MB 上限。
- **瀏覽器自動化**（五層自動路由）：L1 API Fetch → L2 Static Scrape → L3 Headless → L4 Sandbox Container → L5 Computer Use。透過 `CapabilitiesConfig` 預設拒絕；`bash-gate.sh` 作為 Layer 1.5，白名單放行 Playwright/Puppeteer。
- **CJK 安全位元組切片**：`duduclaw_core::truncate_bytes` / `truncate_chars` 取代了 31 處不安全的 `s[..s.len().min(N)]` 寫法（修正 v1.8.11 的多位元組 codepoint panic）。

### 帳號與成本
- **逐 Agent 模型路由**（SDK-first）：`agent.toml [model]`— `preferred`（Claude SDK 模型）、`local.model`、`local.use_router`、`api_mode`（cli/direct/auto）、`account_pool`（詳見下方）。
- **多 OAuth 帳號輪替**：OAuth session（Claude Pro/Team/Max，透過 `claude auth status`；`setup-token` 帳號則用 `CLAUDE_CODE_OAUTH_TOKEN`）+ API key。4 種策略（Priority/LeastCost/Failover/RoundRobin）。速率限制冷卻（2 分鐘）、帳單額度用盡冷卻（24 小時）、預算強制、token 到期追蹤（30 天／7 天預警）。
- **逐 Agent 帳號池**（`agent.toml [model] account_pool`）：限制該 Agent 可使用哪些輪替帳號。作用在**候選集合**上，排在 provider／health／cooldown／budget 篩選之後、策略執行之前，因此四種策略在縮小後的集合上語意完全不變。條目比對帳號 `id` **或**其 dashboard `label`（精確比對、去除空白、ASCII 大小寫不敏感；絕不做子字串比對）。**Fail-open**：若帳號池比對不到任何*可用*帳號（id 過期、全部在冷卻中），會記一筆 `warn` 並退回完整帳號集合；過期的帳號池絕不能讓 Agent 無帳號可用。未設定／空值 ⇒ 輪替行為不變。進入點：`AccountRotator::select_with_pool` / `select_for_provider_with_pool`。
- **雙派工路徑**：子 Agent dispatcher（`claude_runner::call_with_rotation`）與面向使用者的通道回覆（`channel_reply::call_claude_cli_rotated` → `rotate_cli_spawn_with_pool`）都會經過 rotator，各自帶入回覆 Agent 的 `account_pool`。
- **`FailureReason` 分類**— RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown，各分類對應專屬的 zh-TW 使用者訊息，並記錄至 `channel_failures.jsonl` 稽核紀錄。
- **執行檔探測**：`which_claude()` / `which_claude_in_home()` 會探測 Homebrew（Intel + Apple Silicon）、Bun、Volta、npm-global、`.claude/bin`、`.local/bin`、asdf shims、NVM 版本目錄，修正由 launchd 啟動的 gateway 在 `PATH` 為空時找不到執行檔的問題。
- **CostTelemetry**：以 SQLite 追蹤 token 用量，並分析快取效率（`cache_read / (input + cache_read + cache_creation)`），200K 價格斷崖預警，自適應路由（快取效率 <30% → 轉本機）。MCP 工具：`cost_summary`、`cost_agents`、`cost_recent`。
- **Direct API client**（`direct_api.rs`）：純聊天情境略過 Claude CLI，system prompt 加上 `cache_control: ephemeral` → 快取命中率 95%+。使用單例 `reqwest::Client`，逾時 120 秒；於所有 OAuth 帳號皆冷卻中時作為備援。

### 排程
- **HeartbeatScheduler**：逐 Agent 統一排程——bus 輪詢 + GVU 靜默斷路器 + cron，以 `max_concurrent_runs` semaphore 限流。
- **CronScheduler**：讀取 `cron_tasks.jsonl`（v1.8.12 起加上 `cron_tasks.db`），依 cron 表達式觸發任務。`list_cron_tasks` 會回傳所有任務（v1.8.3 起不再依 default_agent 篩選）。`schedule_task` MCP 工具的 schema 已於 v1.8.12 修正，補上 `agent_id` 與 `name` 欄位。
- **ReminderScheduler**：一次性提醒（相對時間 `5m`/`2h`/`1d` 或 ISO 8601），可用 `direct` 靜態訊息或 `agent_callback` 喚醒模式。

### Skill 生態系
- **七階段生命週期**：Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis。
- **GitHub 即時索引**— Search API + 24 小時本機快取 + 加權搜尋。
- **Skill 自動合成**（Phase 3-4）：gap accumulator 偵測重複出現的領域缺口 → 從情節記憶合成 skill（受 Voyager 啟發）→ 帶 TTL 的沙箱試跑 → 跨 Agent 畢業機制。MCP 工具：`skill_security_scan`、`skill_graduate`、`skill_synthesis_status`。
- **Rust 原生 Skill 安全掃描器**（`skill_lifecycle::security_scanner`）— 不需 Python 子行程；同時支撐 dashboard 審核、MCP `skill_security_scan` 工具與沙箱試跑閘。

### 任務與知識
- **Task Board**：以 SQLite 管理任務，追蹤狀態／優先順序／指派，並提供即時 Activity Feed WebSocket。MCP 工具：`tasks.list/create/update/assign`、`activity.list/subscribe`。
- **共用知識庫**：`~/.duduclaw/shared/wiki/`，具備 Wiki 目標分類（agent/shared/both）。MCP 工具：`shared_wiki_ls/read/write/search/delete/stats`、`wiki_share`。
- **Autopilot 規則引擎**：自動化委派／通知／skill 執行。觸發條件：任務建立、狀態變更、通道訊息、閒置偵測、cron 排程。

### 整合
- **Odoo ERP 橋接**（`duduclaw-odoo` crate）：支援 CE/EE 的 JSON-RPC 中介層，15 個 MCP 工具（CRM/Sales/Inventory/Accounting）、EditionGate 自動偵測、事件輪詢 + webhook。透過 `OdooConnectorPool` 做逐 Agent 憑證隔離（RFC-21 §2，v1.11.0）。Dashboard 儲存前測試：`odoo.test` RPC 接受 inline 參數（v1.13.1）— 省略憑證欄位時退回已儲存的密鑰；使用與 `odoo.configure` 相同的 SSRF／HTTPS／資料庫名稱驗證器；`scrub_odoo_error()` 將連線錯誤訊息裁剪至 240 字元，避免洩漏 HTML 或 URL。
- **Prometheus 指標**：gateway HTTP 的 `GET /metrics`— 請求數、token 用量、耗時直方圖、通道狀態。
- **RL 軌跡收集器**：在通道互動期間，把逐 Agent 軌跡寫入 `~/.duduclaw/rl_trajectories.jsonl`。`duduclaw rl export|stats|reward` CLI（複合獎勵：outcome × 0.7 + efficiency × 0.2 + overlong × 0.1）。
- **BroadcastLayer** tracing layer 將即時日誌串流給 WebSocket 訂閱者。
- **Dashboard WebSocket 心跳**：伺服器每 30 秒送一次 Ping，60 秒未收到 Pong 就關閉閒置連線。Client 端每 25 秒送一次應用層 `ping` RPC（瀏覽器無法送出 control frame）。

### 可靠性與治理（v1.9.4）
- **`duduclaw-durability` crate**— 五大耐用性支柱：
  - `idempotency.rs`：以 key 為基礎的去重，防止重複執行。
  - `retry.rs`：指數退避 + 抖動策略。
  - `circuit_breaker.rs`：Closed / Open / HalfOpen 三態，搭配 `probe_inflight` 計數（v1.9.4 修正：OPEN→HALF_OPEN 轉換時會遞增 `probe_inflight`，避免幽靈探測超額）。
  - `checkpoint.rs`：可續跑的任務進度。
  - `dlq.rs`：給終局失敗訊息用的 Dead Letter Queue。
- **`duduclaw-governance` crate**（W19-P1 M1-A）— `PolicyRegistry`（YAML + 熱重載 + 逐 Agent 優先序合併 + fail-safe + 並行 upsert 安全）、四種 `PolicyType`（Rate / Permission / Quota / Lifecycle）、`quota_manager.rs`（逐 Agent／逐政策的軟性與硬性配額）、`error_codes.rs`（QUOTA_EXCEEDED / POLICY_DENIED / ...）、審批工作流 + 稽核日誌。預設政策集在 `policies/global.yaml`。
- **LLM fallback 鏈**（`gateway/llm_fallback.rs`）— 主模型逾時／503／429／overloaded 時自動切換到備援模型。`is_llm_fallback_error` / `should_attempt_model_fallback` 是有單元測試的純函式。以 `char_indices` 確保 UTF-8 安全截斷。
- **Evolution Events 系統**（`gateway/evolution_events/`）— 30+ 種事件 schema、非同步批次 + 重試發射器、查詢介面、可靠性保證。以 HTTP endpoint 暴露在 gateway 上，並顯示於 Web 的 `ReliabilityPage`。

### 記憶評測（v1.9.4 / W21）
- **LOCOMO 評測**（`python/duduclaw/memory_eval/`）— `retrieval_accuracy`、`retention_rate`、`locomo_integrity_check`。`cron_runner` 每日 UTC 03:00 觸發。5 分鐘等級的 `smoke_test` P0 驗證基本記憶功能。`build_golden_qa.py` 建立黃金 QA 集；`data/golden_qa_set.jsonl` 收錄前 200 筆。`duduclaw-memory` 引擎新增批次查詢 API 供評測使用。
- **Python `agents/` + `mcp/` 模組**— `agents/capabilities/`（manifest + matcher）、`agents/routing/`（router + resolution + memory_resolver）。`mcp/auth/`（API Key 附遮罩）、`mcp/tools/memory/`（store / read / search / namespace / quota，於 `execute()` 入口強制嚴格 scope 檢查，修補 v1.9.3 的驗證漏洞：先前任何合法 API Key 都能繞過 scope 限制）。

### Web 儀表板（24 頁）
- 技術棧：React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui，暖琥珀色主題。
- 即時日誌串流（BroadcastLayer → WebSocket）。
- OrgChart（D3.js 互動式 Agent 階層圖）。
- Memory 頁含 Key Insights 分頁（`key_facts` 卡片附 access_count 徽章）+ Evolution 分頁（SOUL.md 版本歷史，附前後差異）。
- Logs 頁提供來源篩選 chip + 嚴重度下拉選單 + 依嚴重度上色的左側邊框 + JSON 詳情展開。
- Toast 通知系統（模組層級 event bus、最多 5 則佇列、暖色系樣式）。
- Skill Market 三分頁（Marketplace / Shared Skills / My Skills）。
- Autopilot 設定 + Session Replay + WikiGraph。
- **ReliabilityPage**（v1.9.4，`/reliability` 路由）— circuit breaker 狀態、重試統計、DLQ 深度儀表板。呼叫 `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems`。
- i18n：zh-TW / en / ja-JP（600+ 翻譯鍵）。
- Dark/Light 主題（跟隨系統 + 手動切換）。
