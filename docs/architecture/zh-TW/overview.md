# DuDuClaw 架構總覽

## 架構總覽（依 v1.67.0 校對）

DuDuClaw 是一套**多執行環境 AI Agent 平台（Multi-Runtime AI Agent Platform）**，透過統一的 `AgentRuntime` trait 支援 **Claude Code / Codex / Antigravity / Grok** CLI（另有 OpenAI 相容 API）作為 AI 後端（Gemini CLI 後端自 v1.67.0 起棄用，v1.69.0 移除，由 Antigravity 取代），具備自動偵測與逐 Agent 設定能力。DuDuClaw 並非獨立的 LLM 產品；它是把一個（或多個）AI CLI 轉變成長駐運作 Agent 的管線層，涵蓋通道路由、對話記憶、自我演化、多帳號輪替、本機 LLM 推理、瀏覽器自動化與 IDE 整合。

## 關鍵架構決策

### 執行環境與傳輸層
- **Multi-Runtime**（`AgentRuntime` trait）— `runtime_catalog.rs` 中有 13 個 runtime id：12 個 CLI 後端（Claude、Codex、Antigravity（`agy`）、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode，以及 v1.67.0 起棄用、v1.69.0 移除的 Gemini CLI），加上 OpenAI-compat HTTP。其中 5 個 CLI 與 OpenAI-compat 各有自己的模組；其餘 7 個 CLI 共用一個通用的 print-mode runtime。`RuntimeRegistry` 自動偵測，逐 Agent 設定寫在 `agent.toml [runtime] provider`。
- **跨 runtime failover 的模型替換**（`failover.rs`）：當 `[runtime] fallback` 把呼叫轉到*不同*的 provider 時，備援 runtime 不再沿用主要 runtime 的模型 id（過去 codex Agent 的 `gpt-5.4` 會被拿去丟給 Claude runtime）。模型依四個有序分支解析：① `agent.toml [model] fallbacks` 中第一個明確屬於備援 runtime 模型家族的項目（帶 `provider/model` 前綴的會去掉前綴）；② 若該 runtime 本來就能服務所要求的模型，則保留原模型（`openai_compat` 不宣告模型家族，靠這條分支繼續代理任意 id）；③ runtime catalog 為該後端列出的第一個模型；④ 以上皆無則**拒絕 spawn**，回報 `no model configured for fallback runtime <P>`，並記為一次失敗的嘗試。每次替換都會輸出 `warn!`，帶有 `agent` / `from_runtime` / `to_runtime` / `from_model` / `to_model`。
- **MCP Server（stdio）**（`duduclaw mcp-server`）透過 stdin/stdout 上的 JSON-RPC 2.0，把通道、記憶、Agent、skill、task、共用 wiki、autopilot 等工具暴露給 AI Runtime。註冊層級在 Agent 端的 `<agent>/.mcp.json`（v1.8.5 撤回了 v1.8.4 的全域註冊，因為 Claude CLI `-p --dangerously-skip-permissions` 只會讀取專案層級的 `.mcp.json`）。Gateway 啟動時會自動為所有 Agent 建立／修復 `.mcp.json`。
- **MCP Server（HTTP/SSE）**（`duduclaw http-server --bind 127.0.0.1:8765`，v1.9.4）— Bearer 驗證的 `POST /mcp/v1/call`（單次 JSON-RPC 工具呼叫）、`GET /mcp/v1/stream`（長駐 SSE 事件串流，Bearer 或 `?api_key=`）、`POST /mcp/v1/stream/call`（非同步 + SSE 結果推送）、`GET /healthz`（免驗證）。Token bucket 速率限制（60 req/min）。`mcp_sse_store.rs` 用 broadcast channel 管理 SSE 連線。與 stdio 互補，服務外部 HTTP client。
- **ACP 與 A2A**— `duduclaw acp`（等同 `duduclaw acp client`）透過 stdio 實作 Agent Client Protocol v1，服務 Zed / JetBrains / Neovim 的 agent 面板。`duduclaw acp server`（舊名 `acp-server`，v1.68.0 前仍可解析）提供 A2A：`message/send` 會附加到 `bus_queue.jsonl`，`tasks/get` 把 bus 的觀察結果對應回 A2A 狀態，Agent Card 位於 `/.well-known/agent-card.json`（舊路徑 `/agent.json` 為別名）。
- **Agent 目錄**與 Claude Code 相容：每個目錄都包含 `.claude/`、`.mcp.json`、`SOUL.md`、`CLAUDE.md`、`CONTRACT.toml`、`agent.toml`、`wiki/`、`SKILLS/`、`memory/`、`tasks/`、`state/`。

### 通道（11 個）
- **Telegram**（long polling）— 檔案／照片／貼圖／語音、群組話題（forums/topics）、僅限提及（mention-only）、透過 OpenAI Whisper API 轉錄語音。
- **LINE**（webhook）— HMAC-SHA256 簽章、貼圖目錄、逐聊天室設定。
- **Discord**（Gateway WebSocket）— `tokio::select!` 心跳、斜線指令、自動建立討論串；語音頻道（Songbird）只存在於啟用非預設 `discord-voice` feature 的建置，正式發布的執行檔不含此 feature。v1.9.2 強化：真正的 op 6 RESUME（持久化 `session_id` + `resume_gateway_url` + sequence）、停滯監看（超過 2 倍心跳間隔無流量即中斷）、心跳 channel 容量 1→16 並改用 `try_send`、op 9 加上 1-5 秒抖動、處理 RESUMED dispatch、backoff 上限 60 秒。
- **Slack**（Socket Mode）、**WhatsApp**（Cloud API webhook，簽章驗證 fail-closed）、**Feishu**（Open Platform v2）、**Google Chat**（webhook，JWT 驗證）、**Microsoft Teams**（Azure Bot / Connector v3，JWT 驗證）、**WeCom**（HMAC-SHA1 + AES-256-CBC）、**DingTalk**（HMAC-SHA256 + 時間窗）、**WebChat**（`/ws/chat` + React 前端）。
- 通用的 `POST /webhook/{agent_id}` endpoint 從未掛載，已於 v1.66 移除；入站 webhook 屬於各個通道與 Odoo（`POST /webhook/odoo`）。
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
- **Direct API prompt 快取**（"system_and_3" 斷點策略；未公布實測命中率）。
- **壓縮摘要**在達到 50k token 門檻時注入 system prompt（而非對話輪次本身）。
- **Instruction Pinning**（v1.8.6 P0）— 使用者第一輪 → 非同步以 Haiku 萃取核心任務 → 存入 `sessions.pinned_instructions` → 注入到 system prompt 尾端（U 形注意力）。澄清回答會持續累積（≤1000 字元）。
- **Snowball Recap**（v1.8.6 P0）— 每一輪都在使用者訊息前加上 `<task_recap>`。零 LLM 成本。
- **P2 Key-Fact Accumulator**（v1.8.6）— 每個有實質內容的輪次，由 Haiku 萃取 2-4 條關鍵事實 → 存入具 FTS5 索引的 `key_facts` 表 → 挑最相關的前 3 條注入 system prompt。約 100-150 token，相較 MemGPT 的 6,500 token（−87%）。
- **CLI 輕量路徑**— `call_claude_cli_lightweight()` 搭配 `--effort medium --max-turns 1 --no-session-persistence --tools ""`，用於 metadata 任務。可降低 25-40% 成本。
- **穩定化旗標**— `--strict-mcp-config`（MCP 隔離）+ `--exclude-dynamic-system-prompt-sections`（跨輪次 prompt 穩定性，減少 10-15% token）。`--bare` 已於 v1.8.11 移除（會破壞 OS 鑰匙圈憑證查詢）。

### 演化
- **預測驅動引擎**：Active Inference + Dual Process Theory；設計上多數對話結束時不需呼叫 LLM（未公布實測比例）。中等誤差存為情節記憶；顯著誤差啟動一輪 AEE 演化；嚴重誤差（或連續三次顯著誤差）啟動緊急回合；可忽略誤差中有一小部分探索比例（ε，下限 5%）也會啟動一輪。
- **MetaCognition**：每 100 次預測自我校準誤差閾值一次。
- **MistakeNotebook**：跨迴圈的錯誤記憶，防止退化；條目現在帶有決定性的 `TrajectoryEvidence`（哪個工具／斷言失敗），使反思整併不再輕信未經查證的自陳診斷（Evolution v3）。
- **`SOUL.md` 對 Agent 唯讀**（Evolution v3 WP1.1，只有操作者／dashboard 可寫入）。舊的 Generator→Verifier→Updater 改寫路徑、`SOUL.md` 版本控制、24 小時觀察期與自動回滾已於 **2026-09-29（S11）移除**：既然沒有任何機制能代 Agent 寫這個檔案，它們守護的路徑本身早已不存在。
- **AEE（Agentic Evolution Engine，唯一的演化引擎）**：演化目標是 **playbook**，`SOUL.md` 不在其中。playbook 由一組體積小、可個別淘汰、基因狀結構的條目組成（category／signals／關聯 eval case），透過 Gate（決定性、保留否決權）與 Measure（計分、無否決權）的分離、champion + 持平或優於才提交（matches-or-improves）的提交閘，以及條目層級（而非整份檔案）的觀察期來演化。詳見 `evolution-engine.md` 第十二章與 `../../features/zh-TW/38-aee-playbook-evolution.md`。
- **Agent-as-Evaluator**：獨立的 Evaluator Agent（以 Haiku 控制成本），進行對抗式驗證並輸出結構化 JSON 判定。
- **ConversationOutcome**：零 LLM 成本的對話結果偵測（TaskType / Satisfaction / Completion），支援 zh-TW + en 雙語。
- **外部因子**：使用者回饋、安全事件、通道指標、Odoo 商業情境、同儕 Agent 訊號，皆會餵入預測引擎與演化回合。

### Wiki 知識層（v1.8.9）
- **四層架構**（受 Vault-for-LLM 啟發）：L0 Identity / L1 Core / L2 Context / L3 Deep。
- **信任權重**（frontmatter 中的 `trust`，0.0-1.0）— 搜尋結果依信任加權分數排序。
- **自動注入**：`build_system_prompt()` 會把 L0+L1 頁面自動注入 WIKI_CONTEXT，涵蓋 CLI／通道回覆／dispatcher 三條路徑，在 Claude / Codex / Antigravity / Grok / OpenAI-compat 各 runtime（以及已棄用的 Gemini runtime）間保持一致。
- **FTS5 索引**（`unicode61` tokenizer）— 每次寫入／刪除都自動同步，也可透過 `wiki_rebuild_fts` 手動重建。
- **知識圖譜**：`wiki_graph` MCP 工具匯出限制 BFS 深度的 Mermaid 圖；節點形狀依層級區分。
- **去重偵測**：`wiki_dedup` 透過標題比對 + 標籤 Jaccard 相似度（≥0.8）偵測重複頁面。
- **反向 backlink 索引**：掃描 `related` frontmatter 與內文 markdown 連結，建立雙向對應。
- **搜尋篩選**：`wiki_search`（兩種 scope 皆可）支援 `min_trust`、`layer`、`expand`（1-hop backlink 展開）。
- **Shared Wiki**：`~/.duduclaw/shared/wiki/` 存放跨 Agent 的 SOP、政策、產品規格。可見性由 `wiki_visible_to` capability 控制。

### 記憶系統
- **認知記憶**（選用）：`SqliteMemoryEngine`，情節／語意記憶分離，採 Generative Agents 三維加權檢索（Recency × Importance × Relevance）。
- **記憶衰減每日排程**：背景任務每 24 小時執行一次 `duduclaw_memory::decay::run_decay`。低重要性 + 滿 30 天 → 歸檔。已歸檔 + 滿 90 天 → 永久刪除。
- **認知記憶 MCP 工具**：`memory_search_by_layer`（情節／語意篩選）、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status`。
- **MemGPT 三層系統**（Core Memory、Recall Memory、Archival Bridge、Budget Manager、Consolidation Pipeline，共 6 個 MCP 工具）**已於 v1.8.1 移除**（−1,985 行程式碼）— 該注入方式讓每個 prompt 膨脹 6,500 token，並造成「lost in the middle」注意力衰退。

### 本機推理
- **統一 `InferenceBackend` trait**（`duduclaw-inference` crate）：OpenAI 相容 HTTP（llama-server/Ollama/vLLM/SGLang/llamafile）。行程內的 llama.cpp、mistral.rs 與 MLX 後端已於 2026-09 移除，正式發布的執行檔從未編譯過它們；請改跑本機的 OpenAI 相容伺服器。
- **Confidence Router**：LocalFast / LocalStrong / CloudAPI 三層路由，具備 CJK 感知的 token 估算。
- **InferenceManager**：自動切換的狀態機：llamafile → Direct backend → OpenAI-compat → Cloud API。
- **llamafile manager**：子行程生命週期管理、健康監測、在 localhost 提供 OpenAI 相容 API。
- **MCP 工具**：`model_list`、`model_load`、`model_unload`、`inference_status`、`hardware_info`、`route_query`、`inference_mode`、`llamafile_start/stop/list`。

### Token 壓縮
- **回覆路徑的預算管線**（`gateway/prompt_compression.rs`）：TurnTrim → DropOldestToolEchoes → BisectAndSummarize，會考量成本壓力，token 估算 CJK 安全；近期快取效率高於 50% 且預算超出低於 15% 時略過。
- 早期的 Meta-Token（LTSC）／LLMLingua-2／StreamingLLM 壓縮器及其 `compress_text` / `decompress_text` 工具已於 v1.33 移除。

### 語音管線
- **HTTP endpoints**：`POST /api/stt`（OpenAI 相容的轉錄 API 或本機指令範本；未設定 STT provider 時回 501）、`POST /api/tts`，後端為 Piper（本機）／Edge TTS／MiniMax T2A／OpenAI TTS。
- **Telegram 語音**：透過 OpenAI Whisper API 轉錄、以 Edge TTS 回覆，兩者皆為寫死的設定；dashboard 的語音設定不會作用到這條路徑。
- **正式發布的執行檔不含**：行程內 Whisper（`whisper` feature）、ONNX embedding（`onnx` feature）與 Discord 語音（`discord-voice` feature）。
- 此處過去列出的 SenseVoice、Deepgram、Silero VAD、`symphonia` 解碼與 LiveKit 語音房，都沒有對應的程式碼。詳見 [`docs/features/zh-TW/14-voice-pipeline.md`](../../features/zh-TW/14-voice-pipeline.md)。

### 安全性
- **Claude Code PreToolUse hooks**（由 `agent_hook_installer` 逐 agent 裝進 `<agent_dir>/.claude/settings.json`）：`duduclaw hook agent-file-guard`（Rust 子命令，matcher `Write|Edit|MultiEdit|Bash`；擋正規樹外的 agent 結構檔、擋寫自己的 SOUL.md、擋跨 agent 寫入，並含 `org_field_guard` 對 `reports_to`／`department`／`name`／`[capabilities]`／`[delegation]`／`[acp]` 的欄位級凍結）與 `duduclaw hook data-file-guard`（RFC-23 §14.4，matcher `Read|Bash`，只有去識別化生效時才武裝；它是 `Bash` 檔名啟發式，並非沙箱。H10 2026-09 已取代原本在 Windows 上失效的 shell 腳本）。2026-04 的三階段 shell 腳本防禦與其 GREEN／YELLOW／RED 威脅等級狀態機已於 `ba015a48` 移除，詳見 [`docs/features/zh-TW/05-security-defense.md`](../../features/zh-TW/05-security-defense.md)。
- **SOUL.md 漂移偵測**（SHA-256 指紋，`.soul_history/` 保留最多 10 個版本備份）。
- **Prompt injection 掃描器**（`input_guard`，7 類規則，阻擋門檻 60，先 NFKC 正規化，英文＋zh-TW 樣式，XML 分隔符保護）。
- **機密外洩掃描器**— 19 種機密樣式（Anthropic / OpenAI / AWS / GitHub / GitLab / Slack / Stripe / Google / SendGrid / JWT / PEM 金鑰，以及金鑰或密碼的賦值）加上高熵值檢查，由 skill 安全掃描器使用。
- **CONTRACT.toml**— `must_not` / `must_always` 邊界規則，自動注入 system prompt；`duduclaw test` 紅隊測試 CLI（內建 9 種情境）。
- **統一多來源稽核日誌**：`audit.unified_log` 把 `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` 整併成統一格式（timestamp / source / event_type / agent_id / severity / summary / details），並在 Logs 頁提供篩選 chip。
- **AES-256-GCM** 靜態加密— 逐 Agent 金鑰隔離。
- **Dashboard／WebSocket 驗證**：JWT 帳號登入（密碼以 Argon2id 雜湊存於 `users.db`）或 gateway 管理員 token。早期的 Ed25519 challenge-response 路徑已從 gateway 移除，從來沒有任何設定能啟用它。Ed25519 仍用在授權簽章、更新驗證與 relay 裝置協定。
- **容器沙箱**：有兩條獨立路徑。逐員工的*任務沙箱*（`agent.toml [container] sandbox_enabled`）把被委派任務的 AI CLI 放進唯讀、非 root、有資源上限的 Docker 容器（只支援 Docker，需要 `network_access = true`，不能用時任務失敗；見[任務沙箱指南](../../guides/zh-TW/task-sandbox.md)）。PTC `execute_program` 與 `secaudit` PoC 步驟使用的*腳本沙箱*跑在 Docker 上（Windows 先試 WSL2），`--network=none`、唯讀根檔案系統，只掛一個唯讀的私有腳本目錄；不能用時 PTC 不執行腳本，除非設 `[container.sandbox] script_when_unavailable = "run_unsandboxed"`，PoC 則永遠不在主機上執行。
- **瀏覽器自動化與 Computer Use**：agent 自行選擇的兩個抓取工具與可選的瀏覽器 server，沒有自動路由器：L1 `web_fetch_cached`（經 SSRF 閘、帶快取的 HTTP）、L2 `web_extract`（CSS 選擇器爬取）。L5 電腦操作跑在 `computer_use_orchestrator` 啟動的容器裡（映像 `ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>`，不會自動下載，動作以 `xdotool` 執行），由 agent 透過八個 `computer_*` MCP 工具驅動，`duduclaw mcp-server` 行程會把這些呼叫透過簽章的 loopback 路由轉給 gateway 持有的 session（`POST /api/internal/computer-use`、`computer_use_sessions/`；網路只通到逐 agent 的 `allowed_domains` 主機，並在 session 啟動時釘住位址）。聊天觸發的迴圈與 `native` 主機桌面模式已移除。L3 headless 是可選的逐 agent Playwright／Browserbase MCP server（`.mcp.json`）。透過 `CapabilitiesConfig`（`computer_use`／`browser_via_bash`／`allowed_tools`／`denied_tools`）預設拒絕。死碼 `browser_router.rs` 的五層路由器與它的「L4 Sandbox Browser」層級已於 2026-09 移除。
- **CJK 安全位元組切片**：`duduclaw_core::truncate_bytes` / `truncate_chars` 取代了 31 處不安全的 `s[..s.len().min(N)]` 寫法（修正 v1.8.11 的多位元組 codepoint panic）。

### 帳號與成本
- **逐 Agent 模型路由**（SDK-first）：`agent.toml [model]`— `preferred`（Claude SDK 模型）、`local.model`、`local.use_router`、`api_mode`（cli/direct/auto）、`account_pool`（詳見下方）。
- **多 OAuth 帳號輪替**：OAuth session（Claude Pro/Team/Max，透過 `claude auth status`；`setup-token` 帳號則用 `CLAUDE_CODE_OAUTH_TOKEN`）+ API key。4 種策略（Priority/LeastCost/Failover/RoundRobin）。速率限制冷卻（2 分鐘）、帳單額度用盡冷卻（24 小時）、預算強制、token 到期追蹤（30 天／7 天預警）。
- **逐 Agent 帳號池**（`agent.toml [model] account_pool`）：限制該 Agent 可使用哪些輪替帳號。作用在**候選集合**上，排在 provider／health／cooldown／budget 篩選之後、策略執行之前，因此四種策略在縮小後的集合上語意完全不變。條目比對帳號 `id` **或**其 dashboard `label`（精確比對、去除空白、ASCII 大小寫不敏感；絕不做子字串比對）。**Fail-open**：若帳號池比對不到任何*可用*帳號（id 過期、全部在冷卻中），會記一筆 `warn` 並退回完整帳號集合；過期的帳號池絕不能讓 Agent 無帳號可用。未設定／空值 ⇒ 輪替行為不變。進入點：`AccountRotator::select_with_pool` / `select_for_provider_with_pool`。
- **雙派工路徑**：子 Agent dispatcher（`claude_runner::call_with_rotation`）與面向使用者的通道回覆（`channel_reply::call_claude_cli_rotated` → `rotate_cli_spawn_with_pool`）都會經過 rotator，各自帶入回覆 Agent 的 `account_pool`。
- **`FailureReason` 分類**— RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown，各分類對應專屬的 zh-TW 使用者訊息，並記錄至 `channel_failures.jsonl` 稽核紀錄。
- **執行檔探測**：`which_claude()` / `which_claude_in_home()` 會探測 Homebrew（Intel + Apple Silicon）、Bun、Volta、npm-global、`.claude/bin`、`.local/bin`、asdf shims、NVM 版本目錄，修正由 launchd 啟動的 gateway 在 `PATH` 為空時找不到執行檔的問題。
- **CostTelemetry**：以 SQLite 追蹤 token 用量，並分析快取效率（`cache_read / (input + cache_read + cache_creation)`），200K 價格斷崖預警，自適應路由（快取效率 <30% → 轉本機）。MCP 工具：`cost_summary`、`cost_agents`、`cost_recent`。
- **逐模型成本彙總**（`CostTelemetry::summary_by_model`）：`token_usage` 自第一版 schema 起就有 `model` 欄位，但所有彙總都只按 agent / user / day 分組，「錢花在哪個模型上？」無從回答。`summary_by_model(agent_id: Option<&str>, since_unix)` 依模型分組（花費最高者在前；沒有記錄模型 id 的列歸入 `"(unknown)"`，不做猜測），並回報 `requests` / `input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_creation_tokens` / `cost_millicents` + `cost_usd` / `cache_efficiency`。成本是各列已儲存 `cost_millicents` 的加總，與其他彙總走同一條定價路徑（`cost_for`，於記錄時只套用一次），不重新推算；`cost_usd` 僅為單位換算。以加法方式對外提供：MCP 的 `cost_summary` 與 `cost_agents` 回應在同一時間窗新增 `by_model` 陣列（`cost_agents` 原本的 agent 列改放到 `agents` 鍵下，因為頂層 JSON 陣列無法附帶具名的同層欄位），儀表板 RPC `cost.by_model`（參數 `agent_id?`、`days?`，預設 7，限制在 1–365）則在與其他 `cost.*` 相同的 admin 閘門下回傳彙總。`cost_summary` / `cost_agents` 內部若彙總失敗，會退化為空的 `by_model`，不會讓呼叫者實際發出的請求失敗。
- **Direct API client**（`direct_api.rs`）：純聊天情境略過 Claude CLI，system prompt 加上 `cache_control: ephemeral`（未公布實測命中率）。使用單例 `reqwest::Client`，逾時 120 秒；於所有 OAuth 帳號皆冷卻中時作為備援。

### 排程
- **HeartbeatScheduler**：逐 Agent 統一排程，涵蓋 bus 輪詢 + GVU 靜默斷路器 + cron，以 `max_concurrent_runs` semaphore 限流。
- **CronScheduler**：讀取 `cron_tasks.jsonl`（v1.8.12 起加上 `cron_tasks.db`），依 cron 表達式觸發任務。`list_cron_tasks` 會回傳所有任務（v1.8.3 起不再依 default_agent 篩選）。排程以 `tasks_create` + `schedule` 建立；較舊的 `schedule_task` MCP 工具是已棄用的別名，於 v1.68.0 移除。
- **ReminderScheduler**：一次性提醒（相對時間 `5m`/`2h`/`1d` 或 ISO 8601），可用 `direct` 靜態訊息或 `agent_callback` 喚醒模式。

### Skill 生態系
- **六階段生命週期**：Activation → Compression（三層漸進式載入）→ Extraction → Distillation → Diagnosis → Gap Analysis。原本的 Reconstruction 階段沒有呼叫端，已於 2026-09 移除。
- **GitHub 即時索引**— Search API + 24 小時本機快取 + 加權搜尋。
- **Skill 自動合成**（Phase 3-4）：gap accumulator 偵測重複出現的領域缺口 → 從情節記憶合成 skill（受 Voyager 啟發）→ 帶 TTL 的沙箱試跑 → 跨 Agent 畢業機制；預設關閉（`agent.toml [evolution] skill_synthesis_enabled`）。MCP 工具：`skill_security_scan`、`skill_graduate`、`skill_synthesis_status`。
- **Rust 原生 Skill 安全掃描器**（`skill_lifecycle::security_scanner`）— 不需 Python 子行程；同時支撐 dashboard 審核、MCP `skill_security_scan` 工具與沙箱試跑閘。

### 任務與知識
- **Task Board**：以 SQLite 管理任務，追蹤狀態／優先順序／指派，並提供即時 Activity Feed WebSocket。儀表板 RPC：`tasks.list/create/update/remove/assign`、`activity.list`；agent MCP 工具：`tasks_list`、`tasks_create`、`tasks_update`、`tasks_claim`、`tasks_complete`、`tasks_block`、`activity_list`、`activity_post`。
- **共用知識庫**：`~/.duduclaw/shared/wiki/`，具備 Wiki 目標分類（agent/shared/both）。MCP 工具：`wiki_ls/read/write/search/stats/lint` 搭配 `scope="shared"`（`shared_wiki_*` 寫法是已棄用的別名，於 v1.68.0 移除），另有 `shared_wiki_delete` 與 `wiki_share`。
- **Autopilot 規則引擎**：自動化委派／通知／skill 執行。新規則可用的觸發事件（12 種）：`task_created`、`task_updated`、`task_status_changed`、`activity_new`、`channel_message`、`agent_idle`、`run_at_risk`、`os_file`、`os_frontmost`、`tick`、`security_event`、`odoo_event`；`cron_tick` 從不送出，v1.67.1 起建立時拒絕（[23-autopilot-engine](../../features/zh-TW/23-autopilot-engine.md)）。

### 整合
- **Odoo ERP 橋接**（`duduclaw-odoo` crate）：支援 CE/EE 的 JSON-RPC 中介層，17 個 MCP 工具（CRM/Sales/Inventory/Accounting）、EditionGate 自動偵測、事件輪詢 + `POST /webhook/odoo`（兩者預設關閉），餵給 `odoo_event` autopilot 規則。透過 `OdooConnectorPool` 做逐 Agent 憑證隔離（RFC-21 §2，v1.11.0）。Dashboard 儲存前測試：`odoo.test` RPC 接受 inline 參數（v1.13.1）— 省略憑證欄位時退回已儲存的密鑰；使用與 `odoo.configure` 相同的 SSRF／HTTPS／資料庫名稱驗證器；`scrub_odoo_error()` 將連線錯誤訊息裁剪至 240 字元，避免洩漏 HTML 或 URL。
- **Prometheus 指標**：gateway HTTP 的 `GET /metrics`，涵蓋 failover、wiki 信任、決策延續、prompt 壓縮、常駐感測（`tick_*`）、goal loop 與 live-fork 計數器。請求數／token／耗時／session／通道／預算等序列從未遞增，已於 v1.66 移除。
- **RL 軌跡收集器**：在通道互動期間，把逐 Agent 軌跡寫入 `~/.duduclaw/rl_trajectories.jsonl`。用來匯出這些軌跡的 `duduclaw rl` CLI 已於 2026-09 移除。
- **BroadcastLayer** tracing layer 將即時日誌串流給 WebSocket 訂閱者。
- **Dashboard WebSocket 心跳**：伺服器每 30 秒送一次 Ping，60 秒未收到 Pong 就關閉閒置連線。Client 端每 25 秒送一次應用層 `ping` RPC（瀏覽器無法送出 control frame）。

### 可靠性與治理（v1.9.4）
- **`duduclaw-durability` crate**（🗑️ **已於 2026-07-04 commit `b0639b96` 中移除**）— 該 crate 包含五大耐用性支柱（`idempotency`、`retry`、`circuit_breaker`、`checkpoint`、`dlq`）。經確認在正式程式碼中沒有任何呼叫端，遂被移除。gateway 的 LLM fallback 鏈改用其他機制（見 `gateway/failover.rs`）。checkpoint 保存／復原／分叉等歷史功能在現行程式碼中不可用。
- **`duduclaw-governance` crate**（🗑️ **已於 `b0639b96` 移除**）— 它的 rate／permission／quota／lifecycle 政策沒有任何執行者，dashboard 的 Governance 頁與相關 RPC 已於 v1.66 移除。速率限制、委派政策 + MCP scope、授權配額各自另行強制。
- **LLM fallback 鏈**（`gateway/failover.rs` 的 `failover::model`）— 三層備援（帳號 → 模型 → runtime，2026-09-29 起同屬一個模組樹）的第二層：主模型逾時／503／429／overloaded 時自動切換到較輕的備援模型，帳務錯誤一律不觸發。`is_llm_fallback_error` / `should_attempt_model_fallback` 是有單元測試的純函式，各派工路徑一律呼叫合併後的決策 `FailoverManager::model_fallback_for`。以 `char_indices` 確保 UTF-8 安全截斷。
- **Evolution Events 系統**（`gateway/evolution_events/`）— 30+ 種事件 schema、非同步批次 + 重試發射器、查詢介面、可靠性保證。以 HTTP endpoint 暴露在 gateway 上，並顯示於 Web 的 `ReliabilityPage`。

### 記憶評測（v1.9.4 / W21）
- **LOCOMO 評測**（`python/duduclaw/memory_eval/`）— `retrieval_accuracy`、`retention_rate`、`locomo_integrity_check`。`cron_runner` 是手動執行的 CLI 入口（`python -m memory_eval.cron_runner smoke_test|weekly_kpis|monthly_locomo`），repo 內沒有任何東西排程它。5 分鐘等級的 `smoke_test` P0 驗證基本記憶功能。`build_golden_qa.py` 建立黃金 QA 集；`data/golden_qa_set.jsonl` 收錄前 200 筆。`duduclaw-memory` 引擎新增批次查詢 API 供評測使用。
- **Python `agents/` + `mcp/` 模組**— `agents/capabilities/`（manifest + matcher）、`agents/routing/`（router + resolution + memory_resolver）。`mcp/auth/`（API Key 附遮罩）、`mcp/tools/memory/`（store / read / search / namespace / quota，於 `execute()` 入口強制嚴格 scope 檢查，修補 v1.9.3 的驗證漏洞：先前任何合法 API Key 都能繞過 scope 限制）。

### Web 儀表板
- 技術棧：React 19 + TypeScript + Tailwind CSS 4 + Base UI + 共用的 `mds` 元件庫。
- 即時日誌串流（BroadcastLayer → WebSocket）。
- OrgChart（D3.js 互動式 Agent 階層圖）。
- Memory 頁含 Key Insights 分頁（`key_facts` 卡片附 access_count 徽章）+ 自主進化分頁（停滯警示、駁回統計、playbook 規則卡）。
- Logs 頁提供來源篩選 chip + 嚴重度下拉選單 + 依嚴重度上色的左側邊框 + JSON 詳情展開。
- Toast 通知系統（模組層級 event bus、最多 5 則佇列、暖色系樣式）。
- Skill Market 三分頁（Marketplace / Shared Skills / My Skills）。
- Autopilot 設定 + Session Replay + WikiGraph。
- **Reliability 頁**：evolution 事件查詢與逐 Agent 可靠性摘要（`audit.evolutionQuery`、`audit.reliabilitySummary`）。
- i18n：zh-TW / en / ja-JP（600+ 翻譯鍵）。
- Dark/Light 主題（跟隨系統 + 手動切換）。
