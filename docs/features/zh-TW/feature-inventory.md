# DuDuClaw 完整功能清單

> v1.1.0 | 最後更新：2026-04-07

---

## 核心架構

| 功能 | 說明 |
|------|------|
| Claude Code 擴展層 | 並非獨立 AI——提供通道路由、Session 管理、記憶、演化等管道工程 |
| MCP Server（JSON-RPC 2.0） | 透過 stdin/stdout 向 Claude Code 暴露 52+ 工具 |
| Agent 目錄結構 | 每個 Agent 包含 `.claude/`、`SOUL.md`、`CLAUDE.md`、`.mcp.json` |
| Sub-agent 編制 | `create_agent` / `spawn_agent` / `list_agents`，支援 `reports_to` 階層 |
| Session Manager | SQLite 持久化，50k token 自動壓縮（CJK 感知） |
| 檔案式 IPC | `bus_queue.jsonl` 跨 Agent 任務委派 |

## 通訊通道（7 個）

| 通道 | 協定 |
|------|------|
| Telegram | Long polling，支援檔案/照片/貼圖 |
| LINE | Webhook，支援貼圖 |
| Discord | Gateway WebSocket，斜線指令、語音頻道 |
| Slack | Socket Mode |
| WhatsApp | Cloud API |
| 飛書 | Open Platform v2 |
| WebChat | 內嵌 `/ws/chat` WebSocket + React 前端 |
| 通道熱插拔 | Dashboard 即時啟停，無需重啟 |

## 演化系統

| 功能 | 說明 |
|------|------|
| 預測驅動引擎 | Active Inference，約 90% 零 LLM 成本 |
| 雙系統路由器 | System 1（規則）/ System 2（LLM 反思） |
| GVU 自我博弈迴圈 | Generator-Verifier-Updater，TextGrad 回饋，最多 3 輪 |
| SOUL.md 版本控制 | 24 小時觀察期、原子回滾、SHA-256 指紋 |
| MetaCognition | 每 100 次預測自動校準誤差閾值 |

## 技能生態

| 功能 | 說明 |
|------|------|
| 7 階段生命週期 | 啟動、壓縮、萃取、重構、蒸餾、診斷、差距分析 |
| GitHub 即時索引 | Search API + 24 小時本地快取 |
| 技能市集 | Web Dashboard 瀏覽與安裝 |

## 本地推論引擎

| 功能 | 說明 |
|------|------|
| llama.cpp | Metal/CUDA/Vulkan/CPU |
| mistral.rs | Rust 原生，ISQ、PagedAttention、Speculative Decoding |
| OpenAI 相容 HTTP | Exo/llamafile/vLLM/SGLang |
| 信心路由器 | LocalFast / LocalStrong / CloudAPI 三層路由 |
| InferenceManager | 多模式自動切換狀態機 + 健康檢查 |
| llamafile 管理 | 子程序生命週期管理，跨平台零安裝推論 |
| Exo P2P 叢集 | 分散式推論，235B+ 模型跨機器執行 |
| MLX Bridge | Apple Silicon 本地反思 + LoRA |

## 壓縮引擎

| 功能 | 說明 |
|------|------|
| Meta-Token（LTSC） | Rust 原生無損 BPE-like，27-47% 壓縮率 |
| LLMLingua-2 | Microsoft token 重要性剪枝，2-5x 有損壓縮 |
| StreamingLLM | Attention sink + 滑動窗口 KV-cache |

## 語音管線

| 功能 | 說明 |
|------|------|
| ASR | Whisper.cpp / SenseVoice ONNX / Deepgram |
| TTS | Piper（本地 ONNX）/ MiniMax（遠端） |
| VAD | Silero（ONNX） |
| LiveKit Voice | WebRTC 語音房 |

## 安全層

| 功能 | 說明 |
|------|------|
| 3 階段防禦 | 確定性黑名單 / 混淆偵測 / AI 判讀 |
| Ed25519 認證 | 挑戰-回應式 WebSocket 認證 |
| AES-256-GCM | API 金鑰靜態加密 |
| Prompt Injection 掃描 | 6 規則類別 |
| SOUL.md 漂移偵測 | SHA-256 指紋比對 |
| CONTRACT.toml | 行為邊界 + `duduclaw test` 紅隊測試 |
| RBAC | 角色存取控制 |
| JSONL 審計日誌 | 全工具呼叫紀錄 |

## 記憶系統

| 功能 | 說明 |
|------|------|
| 情節/語意分離 | Generative Agents 3D 加權檢索 |
| 全文搜尋（FTS5） | SQLite 內建 |
| 向量索引 | Embedding 語意搜尋 |
| 記憶衰減 | 間隔重複遺忘曲線 |
| 聯邦記憶 | 跨 Agent 知識共享 |
| Wiki 知識庫 | 全文搜尋 + 知識圖譜視覺化 |

## 帳號與成本管理

| 功能 | 說明 |
|------|------|
| 多帳號輪替 | OAuth + API Key，4 種策略 |
| CostTelemetry | Token 用量追蹤 + 快取效率分析 |
| 預算管理 | 每帳號月度上限 + 冷卻機制 |
| Direct API | 繞過 CLI，95%+ cache hit rate |

## 瀏覽器自動化

| 功能 | 說明 |
|------|------|
| 5 層路由器 | API Fetch / 靜態爬取 / 無頭瀏覽器 / 沙盒瀏覽器 / Computer Use |
| 能力閘門 | `agent.toml [capabilities]` 預設拒絕 |

## 容器沙盒

| 功能 | 說明 |
|------|------|
| Docker | Bollard API，全平台 |
| Apple Container | macOS 15+ 原生 |
| WSL2 | Windows Linux 子系統 |

## 排程系統

| 功能 | 說明 |
|------|------|
| CronScheduler | `cron_tasks.jsonl` cron 表達式排程 |
| ReminderScheduler | 一次性提醒（相對/絕對時間） |
| HeartbeatScheduler | 每 Agent 統一排程 |

## ERP 整合

| 功能 | 說明 |
|------|------|
| Odoo Bridge | 15 個 MCP 工具（CRM/銷售/庫存/會計） |
| Edition Gate | CE/EE 自動偵測 |

## Web 儀表板

| 功能 | 說明 |
|------|------|
| 23 頁面 | 總覽、Agent 管理、通道、記憶、安全、帳單等 |
| 即時日誌串流 | BroadcastLayer tracing → WebSocket |
| WikiGraph | 互動式知識圖譜 |
| OrgChart | Agent 階層視覺化 |

## 商業功能

| 功能 | 說明 |
|------|------|
| 授權分層 | Free / Pro / Enterprise |
| 硬體指紋綁定 | 防止授權濫用 |
| 產業模板 | 製造業 / 餐飲業 / 貿易業 |
| CLI 工具 | 12 子命令 |
