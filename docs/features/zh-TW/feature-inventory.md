# DuDuClaw 完整功能清單

> v1.8.14 | 最後更新：2026-04-21

---

## 核心架構

| 功能 | 說明 |
|------|------|
| Multi-Runtime AI Agent 平台 | 統一 `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat 四後端自動偵測 |
| MCP Server（JSON-RPC 2.0） | 透過 stdin/stdout 向 AI Runtime 暴露 80+ 工具；註冊於 `<agent>/.mcp.json`（Claude CLI `-p` 僅讀取專案層級），gateway 啟動時自動建立/修復 |
| ACP/A2A Server | `duduclaw acp-server` — stdio JSON-RPC 2.0，提供 `agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`；輸出 `.well-known/agent.json` AgentCard；支援 Zed / JetBrains / Neovim IDE 整合 |
| Agent 目錄結構 | `.claude/`、`.mcp.json`、`SOUL.md`、`CLAUDE.md`、`CONTRACT.toml`、`agent.toml`、`wiki/`、`SKILLS/`、`memory/`、`tasks/`、`state/` |
| Sub-agent 編排 | `create_agent` / `spawn_agent` / `list_agents` + `reports_to` 階層 + D3.js 組織圖 + 系統 prompt 自動注入「## Your Team」名冊 |
| DelegationEnvelope | 結構化交接協議 — context / constraints / task_chain / expected_output |
| TaskSpec 工作流 | 多步驟任務規劃 — dependency-aware、auto-retry (3x)、replan (2x)、persistence |
| 長回應分頁 | 子 Agent 回報超過通道 byte budget 時，以 `channel_format::split_text` 分頁並標示 `📨 **agent** 的回報 (1/N)` |
| 孤兒回應恢復 | dispatcher 啟動時 `reconcile_orphan_responses` 原子重播 crash/Ctrl+C/hotswap 後殘留的 `agent_response` callback |
| 檔案式 IPC | `bus_queue.jsonl` 跨 Agent 委派，最多 5 hop 追蹤 |
| Per-Agent Channel Token | `get_agent_channel_token` 優先讀取每 agent `bot_token_enc`，修復 Discord thread 跨 bot 401 |

## Multi-Runtime

| 功能 | 說明 |
|------|------|
| Claude Runtime | Claude Code SDK (`claude` CLI) + JSONL streaming + `--resume` 原生多輪 |
| Codex Runtime | OpenAI Codex CLI + `--json` streaming，以 `AGENTS.md` 檔案傳遞 system prompt |
| Gemini Runtime | Google Gemini CLI + `--output-format stream-json`，以 `GEMINI_SYSTEM_MD` env 傳遞 system prompt，`--approval-mode yolo` |
| OpenAI-compat Runtime | HTTP 端點（MiniMax / DeepSeek 等）REST API |
| RuntimeRegistry | 自動偵測已安裝 CLI，per-agent `[runtime]` 設定 |
| Cross-Provider Failover | `FailoverManager` 健康追蹤、冷卻、不可重試錯誤偵測 |

## Session 記憶堆疊（v1.8.1 + v1.8.6）

| 功能 | 說明 |
|------|------|
| 原生多輪 Session | Claude CLI `--resume` + SHA-256 確定性 session ID + history-in-prompt fallback（stale session / 帳號輪替 / unknown stream-json error 自動重試）|
| Hermes turn trimming | >800 chars → 首 300 + 尾 200 + `[trimmed N chars]`，CJK-safe 字元切片 |
| Prompt Cache 策略 | Direct API 「system_and_3」斷點配置，多輪命中率 ~75% |
| 壓縮摘要注入 | 壓縮摘要（role=system）注入 system prompt，非對話輪次 |
| Instruction Pinning | 首訊息 → async Haiku 擷取 → `sessions.pinned_instructions` → 注入 system prompt 尾端（U-shape 高注意力）|
| Snowball Recap | 每輪 user message 前置 `<task_recap>`，零 LLM 成本、U-shape 注意力尾端 |
| Clarification 累積 | 代理人提問 + 使用者回答 → pinned instructions 追加（≤1000 字元）|
| P2 Key-Fact Accumulator | 每輪 2-4 則事實 → `key_facts` FTS5 表 → 注入 top-3（~100-150 tokens vs MemGPT 6,500，−87%）|
| CLI 輕量路徑 | `call_claude_cli_lightweight()` — `--effort medium --max-turns 1 --no-session-persistence --tools ""`，25-40% 成本節省 |
| 穩定化旗標 | `--strict-mcp-config` + `--exclude-dynamic-system-prompt-sections`（10-15% token 節省）；`--bare` 於 v1.8.11 移除（破壞 OAuth keychain）|
| CJK-Safe 字串切片 | `duduclaw_core::truncate_bytes` / `truncate_chars` 取代 31 處 byte-index 切片 |

## 通訊通道（7 個）

| 通道 | 協定 |
|------|------|
| Telegram | Long polling，檔案/照片/貼圖/語音、forums/topics、mention-only、語音轉錄 |
| LINE | Webhook + HMAC-SHA256 簽章、貼圖、per-chat 設定 |
| Discord | Gateway WebSocket、斜線指令、語音頻道（Songbird）、auto-thread（v1.8.14 修復 thread session id 跨輪飄移）、embed 回覆 |
| Slack | Socket Mode、mention-only、thread 回覆 |
| WhatsApp | Cloud API |
| 飛書 | Open Platform v2 |
| WebChat | 內嵌 `/ws/chat` WebSocket + React 前端（Zustand store）|
| 通道熱啟停 | Dashboard 驅動動態啟停 |
| Generic Webhook | `POST /webhook/{agent_id}` + HMAC-SHA256 簽章驗證 |
| Media Pipeline | 自動縮放（max 1568px）+ MIME 偵測 + Vision 整合 |
| Sticker 系統 | LINE 貼圖目錄 + 情緒偵測 + Discord emoji 等價映射 |
| 通道失敗追蹤 | `channel_failures.jsonl` + `FailureReason` 分類（RateLimited/Billing/Timeout/BinaryMissing/SpawnError/EmptyResponse/NoAccounts/Unknown）|

## 演化系統

| 功能 | 說明 |
|------|------|
| 預測驅動引擎 | Active Inference + Dual Process Theory，約 90% 零 LLM 成本 |
| 雙系統路由器 | System 1（規則）/ System 2（LLM 反思）|
| GVU² 雙迴圈 | 外迴圈（Behavioral GVU — SOUL.md）+ 內迴圈（Task GVU — 即時重試）|
| 4+2 層驗證 | L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety |
| MistakeNotebook | 跨迴圈錯誤記憶 — 記錄失敗模式、防止退化 |
| SOUL.md 版本控制 | 24h 觀察期 + 原子回滾 + SHA-256 指紋 |
| MetaCognition | 每 100 次預測自動校準誤差閾值 |
| Adaptive Depth | MetaCognition 驅動 GVU 迭代深度（3-7 輪）|
| Deferred GVU | gradient 累積 + 延遲重試（最多 3 次 deferral、72h 跨度、9-21 輪有效迭代）|
| ConversationOutcome | 零 LLM 對話結果偵測（TaskType / Satisfaction / Completion），zh-TW + en |
| Agent-as-Evaluator | 獨立 Evaluator Agent（Haiku 成本控制）進行對抗式驗證 |
| Orchestrator 模板 | 5 步規劃（Analyze → Decompose → Delegate → Evaluate → Synthesize）+ 複雜度路由 |

## Wiki 知識分層（v1.8.9）

| 功能 | 說明 |
|------|------|
| 4 層架構 | L0 Identity / L1 Core / L2 Context / L3 Deep — Vault-for-LLM 啟發 |
| 信任權重 | `trust` (0.0-1.0) frontmatter；搜尋以 trust-weighted score 排名 |
| 自動注入 | `build_system_prompt()` 於 CLI / 頻道 / dispatcher 三條路徑注入 L0+L1 至 WIKI_CONTEXT |
| FTS5 全文索引 | SQLite `unicode61` tokenizer + CJK 支援，寫入/刪除時自動同步，`wiki_rebuild_fts` 手動重建 |
| 知識圖譜 | `wiki_graph` MCP 工具輸出 BFS 限制的 Mermaid 圖；節點形狀依分層而異 |
| Dedup 偵測 | `wiki_dedup` — 標題匹配 + 標籤 Jaccard 相似度（≥0.8）|
| 反向 backlink 索引 | 掃描 `related` frontmatter + body markdown 連結 |
| 搜尋篩選 | `min_trust` / `layer` / `expand`（1-hop related/backlink 擴充）|
| 共享 Wiki | `~/.duduclaw/shared/wiki/` 跨 Agent SOP / 政策 / 規格；`wiki_visible_to` 可見度控制 |
| CLAUDE_WIKI 模板 | 新 Agent 建立時納入 CLAUDE.md，教導 LLM 使用 wiki MCP 工具 |

## 技能生態

| 功能 | 說明 |
|------|------|
| 7 階段生命週期 | 啟動 → 壓縮 → 萃取 → 重構 → 蒸餾 → 診斷 → 差距分析 |
| GitHub 即時索引 | Search API + 24h 本地快取 + 加權搜尋 |
| 技能市集 | Web Dashboard 瀏覽、安裝、安全掃描 |
| 技能自動合成 | 差距累積器 → 從情境記憶合成（Voyager 啟發）→ 沙箱試用（TTL）→ 跨 Agent 畢業 |
| Python Skill Vetter | 子程序安全掃描候選技能 |

## 本地推論引擎

| 功能 | 說明 |
|------|------|
| llama.cpp | Metal/CUDA/Vulkan/CPU（透過 `llama-cpp-2` crate）|
| mistral.rs | Rust 原生，ISQ、PagedAttention、Speculative Decoding |
| OpenAI 相容 HTTP | Exo/llamafile/vLLM/SGLang |
| 信心路由器 | LocalFast / LocalStrong / CloudAPI 三層路由 + CJK-aware token 估算 |
| InferenceManager | 多模式自動切換：Exo P2P → llamafile → Direct → OpenAI-compat → Cloud API |
| llamafile 管理 | 子程序生命週期、零安裝跨 6 OS |
| Exo P2P 叢集 | 分散式推論，235B+ 模型跨機器、cluster discovery、endpoint failover |
| MLX Bridge | Apple Silicon `mlx_lm` + LoRA 本地反思 |
| 模型管理 | `model_search`（HuggingFace）/ `model_download`（resume + mirror）/ `model_recommend`（硬體感知）|

## 壓縮引擎

| 功能 | 說明 |
|------|------|
| Meta-Token（LTSC）| Rust 原生無損 BPE-like，結構化輸入 27-47% 壓縮率 |
| LLMLingua-2 | Microsoft token 重要性剪枝，2-5x 有損壓縮 |
| StreamingLLM | Attention sink + 滑動窗口 KV-cache，無限長對話 |
| 策略選擇器 | `compress_text` `strategy` 參數 — `meta_token` / `llmlingua` / `streaming_llm` / `auto` |

## 語音管線

| 功能 | 說明 |
|------|------|
| ASR | Whisper.cpp（本地）/ SenseVoice ONNX（本地）/ OpenAI Whisper API / Deepgram（streaming）|
| TTS | Piper ONNX（本地）/ MiniMax T2A（自動偵測 CJK/Latin）/ Edge TTS / OpenAI TTS |
| VAD | Silero ONNX |
| 音訊解碼 | symphonia：OGG Opus / MP3 / AAC / WAV / FLAC → PCM |
| Discord Voice | Songbird 整合 |
| LiveKit | WebRTC 多 Agent 語音房 |
| ONNX Embedding | BERT WordPiece tokenizer + ONNX Runtime |

## 安全層

| 功能 | 說明 |
|------|------|
| 3 階段防禦 | 確定性黑名單（<50ms）/ 混淆偵測（YELLOW+）/ Haiku AI 判讀（RED only）|
| 威脅等級狀態機 | GREEN → YELLOW → RED 自動升降級，24h 無事件降一級 |
| Ed25519 認證 | 挑戰-回應式 WebSocket 認證 |
| AES-256-GCM | API 金鑰靜態加密、per-agent 隔離 |
| Prompt Injection 掃描 | 6 規則類別 + XML 分隔標籤保護 |
| SOUL.md 漂移偵測 | SHA-256 指紋比對 |
| CONTRACT.toml | 行為邊界 + `duduclaw test` 紅隊測試（9 場景）；自動注入所有 runtime 的 system prompt |
| RBAC | 角色存取控制矩陣 |
| 統一多源審計日誌 | `audit.unified_log` 合併 `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` |
| JSONL 審計日誌 | async 寫入，格式相容 Rust `AuditEvent` schema |
| Unicode 正規化 | NFKC 偵測同形字攻擊 |
| Action Claim Verifier | 工具呼叫聲明的簽章驗證 |
| 容器沙盒 | Docker (Bollard) / Apple Container / WSL2 — `--network=none`、tmpfs、read-only rootfs、512MB limit |
| Secret 洩漏掃描 | 20+ 模式（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL 等）|

## 記憶系統

| 功能 | 說明 |
|------|------|
| 情節/語意分離 | Generative Agents 3D 加權檢索（Recency × Importance × Relevance）|
| FTS5 全文搜尋 | SQLite 內建 |
| 向量索引 | Embedding 語意搜尋（ONNX BERT / Qwen3-Embedding）|
| 記憶衰減排程 | 每日背景執行 — 低重要度 + 30 天以上歸檔，歸檔 + 90 天以上永久刪除 |
| 認知記憶 MCP 工具 | `memory_search_by_layer` / `memory_successful_conversations` / `memory_episodic_pressure` / `memory_consolidation_status` |
| 聯邦記憶 | 跨 Agent 知識共享（Private / Team / Public）|
| Key-Fact Accumulator | `key_facts` + FTS5 — 跨 session 輕量記憶（見 Session 記憶堆疊）|

## Git Worktree 隔離（v1.6.0）

| 功能 | 說明 |
|------|------|
| L0 隔離層 | 每任務 git worktree — 比容器沙箱便宜，防止並行 Agent 檔案衝突 |
| 原子合併 | dry-run pre-check → abort → 乾淨時真實合併；全域 `Mutex` 保護 |
| Snap 工作流 | create → execute → inspect → merge/cleanup；純函數決策邏輯 |
| 友善分支名 | `wt/{agent_id}/{adjective}-{noun}`，50×50 word list |
| copy_env_files | 路徑遍歷 jail + 拒絕 symlink + 1MB 大小上限 |
| AgentExitCode | 結構化退出碼 — Success / Error / Retry / KeepAlive |
| 資源上限 | 每 agent 5 個、全域 20 個 |

## 帳號與成本管理

| 功能 | 說明 |
|------|------|
| 多帳號輪替 | OAuth + API Key，4 種策略（Priority/LeastCost/RoundRobin/Failover）|
| 雙 dispatch 路徑 | 子 Agent dispatcher 與頻道回覆皆走 rotator |
| CostTelemetry | SQLite token 追蹤 + 快取效率分析 + 200K 價格懸崖警告 |
| 預算管理 | 每帳號月度上限 + 冷卻 + 自適應路由（cache_eff <30% → 本地）|
| Direct API | 繞過 CLI，`cache_control: ephemeral`，95%+ cache hit rate |
| 失敗分類 | `FailureReason` 枚舉 + 分類 zh-TW 訊息 + `channel_failures.jsonl` 紀錄 |
| 二進位探測 | `which_claude()` / `which_claude_in_home()` 掃描 Homebrew / Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf / NVM |

## 瀏覽器自動化

| 功能 | 說明 |
|------|------|
| 5 層路由 | API Fetch / 靜態爬取 / 無頭 Playwright / 沙盒容器 / Computer Use |
| 能力閘門 | `agent.toml [capabilities]` 預設拒絕 |
| Browserbase | 雲端瀏覽器（L5 替代）|
| bash-gate.sh | Layer 1.5 allowlist（需 `DUDUCLAW_BROWSER_VIA_BASH=1`）|

## 容器沙盒

| 功能 | 說明 |
|------|------|
| Docker | Bollard API，全平台 |
| Apple Container | macOS 15+ 原生 |
| WSL2 | Windows Linux 子系統 |

## 排程系統

| 功能 | 說明 |
|------|------|
| CronScheduler | `cron_tasks.jsonl` + `cron_tasks.db` 永久化（v1.8.12），`schedule_task` MCP schema 修正（含 `agent_id` + `name`）|
| ReminderScheduler | 一次性提醒（相對 `5m`/`2h`/`1d` 或 ISO 8601），`direct` / `agent_callback` 兩種模式 |
| HeartbeatScheduler | 每 Agent 統一排程 — bus polling + GVU 沉默喚醒 + cron |

## ERP 整合

| 功能 | 說明 |
|------|------|
| Odoo Bridge | 15 個 MCP 工具（CRM/銷售/庫存/會計）、JSON-RPC 中間層 |
| Edition Gate | CE/EE 自動偵測、功能閘門 |
| 事件輪詢 | Odoo 狀態變化主動通知 Agent |
| Per-agent 認證隔離 | `OdooConnectorPool` keyed by `(agent_id, profile)`，audit 紀錄帶 `profile` + `ok=bool`（v1.11.0 / RFC-21 §2）|
| Dashboard 測試後再儲存 | `odoo.test` 接受 inline params；credential 留空 fallback 到已儲存金鑰；inline 模式同樣套用 SSRF / HTTPS / db-name 驗證鏈（v1.13.1）|

## RL 與可觀測性

| 功能 | 說明 |
|------|------|
| RL Trajectory Collector | 頻道互動期間寫入 `~/.duduclaw/rl_trajectories.jsonl` |
| `duduclaw rl` CLI | `export` / `stats` / `reward` — 複合獎勵（outcome × 0.7 + efficiency × 0.2 + overlong × 0.1）|
| Prometheus 指標 | `GET /metrics` — requests / tokens / duration histogram / channel status |
| Dashboard WS 心跳 | Server Ping 30s + 60s 空閒關閉；client `ping` RPC 25s |
| BroadcastLayer | tracing layer 即時串流至 WebSocket 訂閱者 |

## Web 儀表板

| 功能 | 說明 |
|------|------|
| 23 頁面 | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export |
| 技術棧 | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui，暖色 amber 主題 |
| 即時日誌串流 | BroadcastLayer tracing → WebSocket |
| Memory Key Insights | `key_facts` 卡片 + access_count badge + 時間戳 + 來源 metadata |
| Memory Evolution | SOUL.md 版本歷史 + 前後度量差異 + 狀態徽章 |
| Logs 歷史重寫 | 來源篩選 chips + per-source 計數 + 嚴重度下拉 + 嚴重度著色左框 + JSON 細節展開 |
| Toast 通知 | 模組級事件匯流排，max-5 queue，暖色 stone/amber/emerald/rose 變體，尊重 `prefers-reduced-motion` |
| OrgChart | D3.js 互動式 Agent 階層 |
| Session Replay | 對話回放元件 + 時間軸 |
| WikiGraph | 互動式知識圖譜 |
| 國際化 | zh-TW / en / ja-JP（600+ 翻譯鍵）|
| 深淺色主題 | 系統偏好 + 手動切換 |
| Experiment Logger | Trajectory recording，供 RL/RLHF 離線分析 |
| Marketplace RPC | `marketplace.list` 提供真實 MCP 目錄（Playwright / Browserbase / Filesystem / GitHub / Slack / Postgres / SQLite / Memory / Fetch / Brave Search）|
| Partner Portal | SQLite `PartnerStore` + 7 RPCs（profile/stats/customers CRUD）|

## 商業功能

| 功能 | 說明 |
|------|------|
| 授權分層 | Free / Pro / Enterprise |
| 硬體指紋 | 授權綁定 |
| 產業模板 | 製造業 / 餐飲業 / 貿易業 |
| CLI 工具 | 12+ 子命令 |
| Partner Portal | 多租戶經銷商介面 |
