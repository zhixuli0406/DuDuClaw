# DuDuClaw vs OpenClaw 深度比較

> 更新日期：2026-03-29
> OpenClaw 版本：v2026.3.24 | DuDuClaw 版本：v0.8.12

---

## 一、定位與設計哲學

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **一句話定位** | 全功能自託管 AI 個人助手 | Claude Code 擴充層（plumbing） |
| **核心哲學** | 萬用平台 — 什麼都做，什麼都能接 | 專精延伸 — Claude Code 做大腦，DuDuClaw 做水管 |
| **口號** | "Your own personal AI assistant. Any OS. Any Platform." | "Claude Code SDK (brain) ↔ DuDuClaw (plumbing)" |
| **GitHub Stars** | ~339k | 新興專案 |
| **授權** | MIT | 私有 |
| **目標用戶** | 廣泛個人用戶、開發者、企業 | 台灣開發者、Claude Code 重度用戶 |

**關鍵差異**：OpenClaw 是「AI 中的瑞士刀」，試圖成為通用 AI 助手平台；DuDuClaw 則選擇不重造 AI 大腦，而是延伸 Claude Code SDK 的能力邊界 — 加上頻道路由、記憶管理、自我演化、本地推理等 Claude Code 本身不具備的能力。

---

## 二、技術棧

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **主要語言** | TypeScript (Node.js) | Rust 2024 edition |
| **Runtime** | Node.js 24（推薦）/ Node.js 22.16+ | 單一靜態二進位（tokio async） |
| **AI 核心** | 自建 LLM Provider 抽象層 | Claude Code SDK (`claude` CLI) |
| **MCP 協定** | 透過 Gateway 內部 RPC | JSON-RPC 2.0 stdin/stdout（標準 MCP Server） |
| **Python 擴充** | 無 | PyO3 bridge（頻道插件、演化引擎） |
| **Web 框架** | React (Control UI) | React 19 + Tailwind CSS 4（rust-embed 嵌入） |
| **資料庫** | SQLite + sqlite-vec | SQLite + FTS5 |
| **非同步模型** | Node.js event loop | tokio + async-trait |
| **序列化** | JSON5 (Zod schema) | serde (JSON + TOML) |
| **加密** | Node.js crypto | ring (AES-256-GCM + Ed25519) |
| **Crate 數量** | 單體架構 (monorepo) | 12 個專用 crate |

**分析**：OpenClaw 選擇 TypeScript 降低社群貢獻門檻，換取最大生態覆蓋；DuDuClaw 選擇 Rust 獲得記憶體安全、極致效能、單一二進位部署。OpenClaw 超過 600k 行程式碼，DuDuClaw 以模組化 crate 保持精簡。

---

## 三、通訊頻道

| 頻道 | OpenClaw | DuDuClaw |
|------|----------|----------|
| Telegram | ✅ | ✅（long polling） |
| LINE | ✅ | ✅（webhook） |
| Discord | ✅ | ✅（Gateway WebSocket + tokio heartbeat） |
| WhatsApp | ✅ | ❌ |
| Slack | ✅ | ❌ |
| Signal | ✅ | ❌ |
| iMessage | ✅ | ❌ |
| Google Chat | ✅ | ❌ |
| Microsoft Teams | ✅ | ❌ |
| Matrix | ✅ | ❌ |
| 飛書 (Feishu) | ✅ | ❌ |
| WeChat | ✅ | ❌ |
| IRC | ✅ | ❌ |
| Zalo | ✅ | ❌ |
| **總計** | **24+** | **3** |

**分析**：OpenClaw 追求「所有平台都能接」，DuDuClaw 只支援台灣最常用的三大平台（LINE、Telegram、Discord），但每個頻道的實作品質更深入（例如 Discord Gateway WebSocket 的 tokio::select! heartbeat 設計比 OpenClaw 的 bot library 封裝更底層、更可控）。

---

## 四、AI 模型與推理

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **模型支援** | 多模型（OpenAI、Claude、Gemini、Groq、本地等） | 以 Claude 為核心 + 本地推理 |
| **Provider 切換** | 設定檔切換 LLM Provider | Per-agent `agent.toml [model]` 路由 |
| **本地推理** | ❌ 無原生支援（依賴 Ollama 等外部） | ✅ 原生整合（llama.cpp / mistral.rs / OpenAI-compat） |
| **推理後端** | N/A | llama.cpp (Metal/CUDA/Vulkan/CPU)、mistral.rs (ISQ/PagedAttention/Speculative Decoding)、HTTP 相容 |
| **P2P 叢集** | ❌ | ✅ Exo P2P（235B+ 模型跨機器） |
| **llamafile** | ❌ | ✅ 子程序管理（零安裝可攜式推理） |
| **MLX bridge** | ❌ | ✅ Apple Silicon 本地反思（Micro/Meso 演化） |
| **信心路由** | ❌ | ✅ 三層 Confidence Router (LocalFast → LocalStrong → CloudAPI) |
| **硬體偵測** | ❌ | ✅ 自動偵測 GPU/加速器 |
| **模型管理** | 外部工具（Ollama） | ✅ GGUF 管理 + HuggingFace 搜尋 + 策展推薦 |
| **模型 MCP 工具** | ❌ | `model_list`、`model_load`、`model_unload`、`inference_status`、`hardware_info`、`route_query` |

**分析**：這是 DuDuClaw 最大的差異化優勢。OpenClaw 的本地推理完全依賴外部服務（Ollama、llama.cpp server），DuDuClaw 則在 Rust 二進位中原生整合多個推理後端，支援 P2P 叢集、信心路由、自動降級。

---

## 五、Agent 架構與演化

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **Agent 概念** | 多 Agent 路由（隔離工作區） | Claude Code 相容目錄（.claude/ + SOUL.md + agent.toml） |
| **Agent 隔離** | Session 隔離 | 檔案系統隔離 + 容器沙箱（Docker / Apple Container） |
| **Agent 間通訊** | 無原生 IPC | ✅ bus_queue.jsonl 檔案 IPC + AgentDispatcher |
| **組織架構** | 扁平 | ✅ `reports_to` 階層 + D3.js 組織圖 |
| **MCP 編排** | Gateway 內部 | `create_agent` / `spawn_agent` / `list_agents` MCP 工具 |
| **自我演化** | ❌ 靜態配置 | ✅ 預測驅動演化（Active Inference + GVU 自我對弈） |
| **認知記憶** | ❌ | ✅ episodic/semantic 分離 + Generative Agents 3D 加權檢索 |
| **人格定義** | Agent 設定檔 | SOUL.md（可版本化 + 漂移偵測） |
| **行為契約** | ❌ | ✅ CONTRACT.toml（must_not / must_always） |
| **紅隊測試** | ❌ | ✅ `duduclaw test <agent>`（9 種內建場景） |

**分析**：OpenClaw 的 Agent 是「路由到不同工作區的 AI 實例」；DuDuClaw 的 Agent 是「具有人格、會自我演化、有行為邊界的 AI 實體」。預測驅動演化引擎 + GVU 自我對弈是 DuDuClaw 獨有的能力 — ~90% 的對話零 LLM 成本，只有真正的預測誤差才觸發 SOUL.md 演化。

---

## 六、記憶與壓縮

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **記憶架構** | SQLite + sqlite-vec 向量嵌入 | SQLite + FTS5 全文搜尋 |
| **向量搜尋** | ✅ 嵌入向量相似度 | ❌（使用 FTS5 關鍵字 + CJK bigram） |
| **記憶類型** | 統一記憶儲存 | 情景記憶 (episodic) + 語意記憶 (semantic) 分離 |
| **Session 壓縮** | Session pruning（截斷） | ✅ 50k token 自動壓縮（CJK 感知） |
| **Token 壓縮** | ❌ | ✅ Meta-Token LTSC（27-47% 無損壓縮） |
| **語意壓縮** | ❌ | ✅ LLMLingua-2（2-5x 有損壓縮） |
| **注意力管理** | ❌ | ✅ StreamingLLM（attention sink + sliding window） |
| **記憶快照** | Session 逐字稿 (JSONL) | 每日記憶快照 (memory/YYYYMM/YYYYMMDD.md) |
| **MCP 工具** | 內部 API | `memory_search`、`compress_text`、`decompress_text` |

**分析**：OpenClaw 使用向量嵌入做語意搜尋（更適合模糊匹配），DuDuClaw 使用 FTS5 全文搜尋（更適合精確查詢和 CJK 內容）。DuDuClaw 的三種壓縮策略（無損 BPE、有損語意、注意力管理）在長對話場景中顯著節省 token 用量。

---

## 七、安全架構

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **已知 CVE** | 8+ 個（含 CVSS 8.8 高危 RCE） | 0 |
| **容器隔離** | ✅ 選用 Docker 沙箱 | ✅ Docker + Apple Container + WSL2 |
| **網路隔離** | 可配置 | ✅ `--network=none` 預設 + 按 Agent 切換 |
| **認證機制** | DM pairing code | ✅ Ed25519 challenge-response |
| **金鑰儲存** | 設定檔（明文 JSON5） | ✅ AES-256-GCM 加密（base64） |
| **人格漂移偵測** | ❌ | ✅ SOUL.md SHA-256 校驗 |
| **Prompt 注入偵測** | ❌ | ✅ 6 類規則掃描器 |
| **安全 Hooks** | 基本攔截 | ✅ 三層漸進防禦（確定性黑名單 → 混淆偵測 → Haiku AI 審查） |
| **威脅等級** | ❌ | ✅ GREEN → YELLOW → RED 狀態機（自動升降級） |
| **稽核日誌** | 基本日誌 | ✅ JSONL 非同步稽核（Rust audit.rs 相容） |
| **行為契約** | ❌ | ✅ CONTRACT.toml + `duduclaw test` 紅隊 |
| **技能市集安全** | ⚠️ ClawHub 曾發現 341 個惡意技能 | ✅ 技能有 A/B 效能驗證 + 漸進式注入 |

**分析**：OpenClaw 因為社群龐大，成為攻擊目標，已累積多個高危 CVE。DuDuClaw 從設計上就採取縱深防禦（三層 Hooks、行為契約、紅隊測試），特別是 Prompt 注入偵測和 SOUL.md 漂移偵測是生態系中獨有的安全特性。

---

## 八、帳號與成本管理

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **多模型 Provider** | ✅（OpenAI, Claude, Gemini 等） | Claude SDK + 本地推理 |
| **帳號輪替** | 單一 Provider failover | ✅ 雙模式（OAuth + API Key）四策略 |
| **輪替策略** | failover | Priority / LeastCost / Failover / RoundRobin |
| **OAuth 支援** | Gateway token | ✅ Claude Pro/Team/Max session 零成本使用 |
| **預算管控** | ❌ | ✅ 按金鑰月預算上限 |
| **速率限制冷卻** | 基本 | ✅ 2 分鐘預設冷卻 + 健康追蹤 |
| **成本最佳化** | 手動切換便宜模型 | ✅ LeastCost 策略自動 OAuth 優先 → API 備援 |
| **本地推理降本** | ❌ | ✅ 信心路由自動切本地模型 |

**分析**：DuDuClaw 的帳號管理更為精密 — 不只支援多帳號輪替，還結合本地推理做成本最佳化。Confidence Router 可以將低複雜度查詢自動路由到本地模型，僅在需要時才使用雲端 API。

---

## 九、Skill 生態系統

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **Skill 數量** | 5,400+（ClawHub + awesome-openclaw-skills） | 新建中（相容 OpenClaw 格式） |
| **Skill 格式** | SKILL.md (YAML frontmatter) | ✅ 相容 OpenClaw SKILL.md 格式 |
| **Skill 市集** | ClawHub（clawhub.com） | ✅ GitHub Search API 即時索引（24h 快取） |
| **安裝方式** | ClawHub / npm / 手動 | Dashboard 市集頁面 + MCP `skill_search` / `skill_list` |
| **漸進式注入** | ❌ 全量載入 | ✅ Layer 0 (名稱) → Layer 1 (摘要) → Layer 2 (完整內容) |
| **效能衡量** | ❌ | ✅ A/B lift measurement (errors_with vs errors_without) |
| **成熟度生命週期** | ❌ | ✅ 診斷 → 啟動 → A/B 測試 → 蒸餾到 SOUL.md → 歸檔釋放 token |
| **惡意偵測** | ⚠️ 曾爆發 ClawHavoc 攻擊 | ✅ Jaccard + CJK bigram 排名驗證 |

**分析**：OpenClaw 擁有壓倒性的 Skill 數量優勢，DuDuClaw 透過相容 OpenClaw 格式來借力。但 DuDuClaw 的 Skill 生命週期管理（漸進注入、A/B 效能衡量、自動蒸餾）遠比 OpenClaw 的靜態載入精密。

---

## 十、Dashboard 與使用體驗

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **UI 框架** | React (Control UI) | React 19 + shadcn/ui + Tailwind CSS 4 |
| **部署方式** | 獨立 Web 服務 | rust-embed 嵌入二進位（零外部託管） |
| **即時日誌** | WebSocket | ✅ BroadcastLayer tracing → WebSocket |
| **Agent 管理** | 設定檔 | ✅ 視覺化卡片 + 沙箱開關 |
| **組織圖** | ❌ | ✅ D3.js 互動式組織圖 |
| **Skill 市場** | ClawHub 網站 | ✅ 內嵌 Dashboard 頁面（GitHub 即時搜尋） |
| **安全稽核** | 外部工具 | ✅ SecurityAudit 頁面 |
| **記憶瀏覽** | ❌ | ✅ MemoryBrowser 頁面 |
| **i18n** | 英文為主 | ✅ zh-TW + en 雙語 |
| **主題** | 深色 | ✅ 暖色調（amber）+ 自動暗/亮模式 |
| **語音功能** | ✅ 喚醒詞 + TTS | ❌ |
| **Canvas** | ✅ A2UI 視覺工作區 | ❌ |
| **瀏覽器控制** | ✅ CDP Chromium | ❌ |
| **Companion App** | ✅ macOS + iOS + Android | ❌ |

**分析**：OpenClaw 在前端體驗上更豐富（語音、Canvas、瀏覽器控制、原生 App）。DuDuClaw 的 Dashboard 更專注於運維面（Agent 監控、安全稽核、記憶瀏覽），且「單一二進位包含 Web UI」的部署體驗更簡潔。

---

## 十一、ERP 與商業整合

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **ERP 整合** | ❌ 無原生支援 | ✅ Odoo CE/EE（JSON-RPC middleware） |
| **CRM** | ❌ | ✅ 聯絡人 CRUD |
| **銷售** | ❌ | ✅ 訂單管線 |
| **庫存** | ❌ | ✅ 庫存查詢 |
| **會計** | ❌ | ✅ 日記帳 |
| **MCP 工具數** | N/A | 15 個 Odoo MCP 工具 |
| **版本偵測** | N/A | ✅ EditionGate 自動偵測 CE/EE |

**分析**：DuDuClaw 的 Odoo 整合是整個 Claw 生態中獨有的能力，讓 AI Agent 能直接操作企業 ERP 系統。

---

## 十二、部署與運維

| 面向 | OpenClaw | DuDuClaw |
|------|----------|----------|
| **安裝方式** | npm / Homebrew / Docker | cargo install / Homebrew / 單一二進位 |
| **系統需求** | Node.js 24 + npm | Rust 編譯環境（或預編譯二進位） |
| **二進位大小** | ~200MB+ (node_modules) | 單一靜態二進位 |
| **記憶體用量** | ~150-300MB (Node.js) | ~20-50MB (Rust) |
| **啟動時間** | 2-5 秒 | <1 秒 |
| **設定格式** | JSON5 (Zod 驗證) | TOML + JSON (Rust 型別驗證) |
| **CLI 指令** | `openclaw onboard/gateway/agent/doctor` | `duduclaw serve/agent/mcp-server/test/migrate` |
| **Tailscale** | ✅ Serve/Funnel | ❌ |
| **Kubernetes** | ✅ 社群 operator | ❌ |

---

## 十三、總結 — 選擇指南

### 選 OpenClaw 的情境

- 需要 **24+ 通訊頻道**（特別是 WhatsApp、Slack、iMessage）
- 想用 **多種 AI 模型**（不只 Claude）
- 需要 **語音互動、Canvas 視覺化、瀏覽器控制**
- 追求 **最大社群生態** 和 5,400+ 現成 Skill
- 需要 **Companion App**（macOS/iOS/Android）
- 需要 **Kubernetes 生產級部署**

### 選 DuDuClaw 的情境

- 是 **Claude Code 重度用戶**，想延伸而非替換 Claude Code
- 需要 **本地 LLM 推理**（llama.cpp / mistral.rs / P2P 叢集）
- 需要 **Agent 自我演化**（預測驅動 + GVU 自我對弈 + 認知記憶）
- 需要 **精密安全防護**（三層 Hooks、行為契約、紅隊測試、Prompt 注入偵測）
- 需要 **ERP 整合**（Odoo CRM/銷售/庫存/會計）
- 需要 **帳號成本最佳化**（OAuth 輪替 + 信心路由 + 本地推理降本）
- 需要 **極致部署效率**（單一 Rust 二進位、<1 秒啟動、~30MB 記憶體）
- 主要在台灣使用（LINE + Telegram + Discord 已足夠）
- 追求 **Token 壓縮** 以降低長對話成本

### 並非二選一

DuDuClaw **刻意相容 OpenClaw 的 Skill 格式**，可以直接使用 5,400+ OpenClaw 社群 Skill。兩個專案的定位互補而非競爭 — OpenClaw 是通用 AI 助手平台，DuDuClaw 是 Claude Code 的深度擴充。
