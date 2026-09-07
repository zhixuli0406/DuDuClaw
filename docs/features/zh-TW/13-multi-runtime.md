# Multi-Runtime Agent 執行

> 一個平台，十二種 AI 後端：Claude、Codex、Gemini、Antigravity、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode，以及任何 OpenAI 相容端點。

---

## 比喻：多語辦公室

想像一間需要翻譯人員的辦公室。與其聘請一位只會法語的翻譯，不如建立一個翻譯台，可以將工作分配給法語、德語、日語翻譯，或任何會說客戶語言的自由譯者。

翻譯台不在乎*哪位*翻譯處理工作。它在乎的是翻譯品質。如果法語翻譯正忙，就轉給下一位。

DuDuClaw 的 Multi-Runtime 架構就是那個翻譯台，但對象是 AI 後端。

---

## 運作方式

### AgentRuntime Trait

核心是一個統一介面（`AgentRuntime`），所有後端都實作它：

```
AgentRuntime trait:
  fn execute(prompt, tools, context) → Response
  fn stream(prompt, tools, context) → Stream<Event>
  fn health_check() → Status
```

每個後端（Claude、Codex、Gemini 或任何 OpenAI 相容端點）都實作相同的介面。系統的其餘部分不需要知道、也不在乎是哪個後端在處理特定請求。

### Runtime 目錄（Runtime Catalog）

每個後端只描述一次，寫在單一份編譯期表格（`crates/duduclaw-core/src/runtime_catalog.rs`）。偵測、一鍵安裝、模型探索、CLI 登入、模型↔供應商推斷全都讀這張表——所以不會再出現「裝得起來卻偵測不到」或「設定得了卻登入不了」的 runtime。

| Runtime | 執行檔 | 安裝管道 | Headless 呼叫 | 輸出 | 登入方式 | 憑證位置 |
|---|---|---|---|---|---|---|
| Claude Code | `claude` | npm `@anthropic-ai/claude-code` | `-p <prompt> --output-format stream-json` | jsonl | `claude setup-token`（貼回驗證碼） | `~/.claude/.credentials.json` |
| OpenAI Codex | `codex` | npm `@openai/codex` | `exec --json <prompt>` | jsonl | `codex login`（localhost 回呼） | `~/.codex/auth.json` |
| Gemini CLI | `gemini` | npm `@google/gemini-cli` | `-p --output-format stream-json <prompt>` | jsonl | `gemini auth login`（localhost 回呼） | `~/.gemini/oauth_creds.json` |
| Google Antigravity | `agy` | `antigravity.google/cli/install.sh` | `-p <prompt>` | text | `agy login`（localhost 回呼） | — |
| Grok Build | `grok` | `x.ai/cli/install.sh`（手動） | `-p <prompt>` | text | `grok login --device-code` | `~/.grok/auth.json` |
| Qwen Code | `qwen` | npm `@qwen-code/qwen-code` | `-p <prompt> --yolo --output-format json` | json | 無（僅 API key） | `~/.qwen/.env` |
| Kimi Code | `kimi` | npm `@moonshot-ai/kimi-code` | `-p <prompt> --output-format stream-json` | jsonl | `kimi login`（裝置碼） | `~/.kimi-code/credentials/` |
| GitHub Copilot CLI | `copilot` | npm `@github/copilot` | `-p <prompt> -s --no-ask-user --allow-all-tools` | text | `copilot login --device-code` | `~/.copilot/config.json` |
| Kiro CLI | `kiro-cli` | `cli.kiro.dev/install`（手動） | `chat --no-interactive --trust-all-tools <prompt>` | text | `kiro-cli login --use-device-flow` | `~/.kiro/settings/cli.json` |
| Cursor CLI | `cursor-agent` | `cursor.com/install` | `-p <prompt> --force --output-format json` | json | `cursor-agent login`（瀏覽器） | `~/.cursor/cli-config.json` |
| Mistral Vibe | `vibe` | PyPI `mistral-vibe` | `-p <prompt> --yolo --trust --output json` | json | 無（僅 API key） | `~/.vibe/.env` |
| OpenCode | `opencode` | `opencode.ai/install` | `run <prompt> --auto --format json` | jsonl | `opencode auth login` | `~/.local/share/opencode/auth.json` |
| OpenAI 相容端點 | *(HTTP)* | — | — | json | 無（僅 API key） | — |

模型選擇每家寫法不同，目錄一併記下是哪一種：獨立旗標（`--model <id>`）、Copilot 文件寫的等號式（`--model=<id>`）、完全沒有旗標時改用環境變數（Mistral Vibe 的 `VIBE_ACTIVE_MODEL`），或是沒有（Kiro 的模型走 `kiro-cli settings`，不是每次呼叫指定）。

**啟用 runtime 前該讀的廠商條款：**

- **Kiro**——AWS FAQ 明文寫著：不允許透過第三方自動化 harness、將請求繞過 Kiro 原生介面。用 DuDuClaw 驅動 Kiro 正屬此類；自行在 CI 直接呼叫 `kiro-cli` 則被允許。所以 Kiro 的安裝管道是手動的：這個決定要由你自己明確做出。
- **Anthropic／Google**——2026-03 起，第三方產品使用消費者訂閱 token 會在伺服器端被封鎖，已有帳號被停權。請走 API key。
- **Qwen**——免費 OAuth 方案已於 2026-04-15 停用，只剩 API key（ModelStudio／DashScope）。
- **OpenCode**——MIT 授權、本身無限制，但它在 1.3.0 移除 Anthropic 訂閱 plugin，理由同上。請用各供應商的 API key。
- **OpenAI**——對「第三方產品驅動 ChatGPT 訂閱登入」的政策不明，API key 才是受支援的路徑。

Dashboard 會顯示對應的條款提示，並要求勾選「我了解風險」才開始訂閱登入。

### 後端怎麼被驅動

五個後端有各自的 runtime 模組，因為它們各有無法共用的廠商專屬接線——帳號輪替（Claude）、以該 CLI 自己的格式注入 MCP 設定、能力→sandbox 旗標的轉譯、空輸出時的 PTY 補救。其餘全部由**同一支**通用 print-mode runtime（`runtime/generic_cli.rs`）直接依目錄項目驅動：用模板 argv 啟動執行檔，把 prompt 以參數或 stdin 送進去，把 text／JSON／JSONL 解析回最終回覆文字，並把非零離開碼或「需要登入」的訊號對應成 failover 鏈看得懂的具名錯誤。

### 原本的四種後端

**Claude Runtime** — 呼叫 Claude Code CLI（`claude`），採用 JSONL 串流輸出。這是功能最完整的後端，內建 MCP 工具支援、bash 執行、web search 和檔案操作。

```
Agent 設定：runtime = "claude"
     |
     v
啟動：claude --json --print ...
     |
     v
解析 JSONL 串流事件
     |
     v
擷取回應 + 工具呼叫
```

**Codex Runtime** — 呼叫 OpenAI Codex CLI，使用 `--json` 旗標取得結構化串流事件。

```
Agent 設定：runtime = "codex"
     |
     v
啟動：codex --json ...
     |
     v
解析 JSONL STDOUT 事件
     |
     v
擷取回應
```

**Gemini Runtime** — 呼叫 Google Gemini CLI，使用 `--output-format stream-json` 取得結構化輸出。

```
Agent 設定：runtime = "gemini"
     |
     v
啟動：gemini --output-format stream-json ...
     |
     v
解析串流 JSON 事件
     |
     v
擷取回應
```

**OpenAI-compatible Runtime** — 呼叫任何支援 OpenAI chat completions API 的 HTTP 端點（MiniMax、DeepSeek、本地伺服器等）。

```
Agent 設定：runtime = "openai-compat"
            api_url = "http://localhost:8080/v1"
     |
     v
HTTP POST /v1/chat/completions
     |
     v
解析 SSE 串流
     |
     v
擷取回應
```

### RuntimeRegistry：自動偵測

DuDuClaw 啟動時，**RuntimeRegistry** 會掃描系統中可用的 CLI 工具：

```
啟動掃描（對 runtime 目錄跑一次迴圈）：
     |
     v
  目錄中每個有執行檔的項目：
     PATH → ~/.local/bin、Homebrew、bun/volta/npm-global/asdf shim、
     /opt/duduclaw/runtimes/bin、/usr/bin、/bin
       找到？ → 註冊（有專屬模組就用專屬的，否則用通用 print-mode）
     |
     v
  一律註冊：OpenAI 相容端點（HTTP 端點，看的是 API key 不是執行檔）
     |
     v
Registry 知道哪些後端可用
```

`/opt/duduclaw/runtimes/bin` 是 DuDuClaw OS 值班機映像放內建 CLI 的位置——即使 gateway 沒有繼承到互動式 `PATH`，映像內建的 runtime 一樣找得到。

Agent 可以在 `agent.toml` 中指定偏好的 runtime：

```toml
[runtime]
preferred = "claude"    # 主要後端
fallback = "gemini"     # 主要不可用時的備案
```

若未設定偏好，Registry 使用第一個可用的後端。

### Per-Agent 設定

不同 Agent 可以同時使用不同後端：

```
Agent "dudu"（客服）     → Claude（最佳推理能力）
Agent "coder"（程式產生）→ Codex（針對程式碼最佳化）
Agent "analyst"（資料分析）→ Gemini（大型上下文視窗）
Agent "local"（隱私敏感）→ OpenAI-compat（本地端點）
```

這意味著單一 DuDuClaw 安裝可以協調跨多個 AI 供應商的 Agent，每個都使用最適合其任務的後端。

---

## 跨供應商容錯

當某個後端變得不可用（限速、當機或出錯），**FailoverManager** 會自動切換到下一個可用後端：

```
Claude runtime：限速中（冷卻：2 分鐘）
     |
     v
FailoverManager 檢查 agent 設定：
  fallback = "gemini"
     |
     v
路由到 Gemini runtime
     |
     v
Claude 冷卻結束 → 恢復主要路由
```

容錯對使用者透明；無論哪個後端處理，使用者都能看到回應。每個後端的健康狀態獨立追蹤：

- **Healthy**：正常運作
- **Rate-Limited**：短冷卻（2 分鐘）
- **Error**：指數退避
- **Non-Retryable**：需人工介入（驗證失敗、帳單問題）

---

## 為什麼這很重要

### 無供應商鎖定

DuDuClaw 不押注在單一 AI 供應商。如果 Claude 漲價，可以將 Agent 轉移到 Codex 或 Gemini。如果 Gemini 推出殺手級功能，可以直接採用而無需重建基礎設施。

### 為每項任務選擇最佳工具

程式碼產生可能在 Codex 上效果更好。複雜推理可能在 Claude 上更強。資料分析可能受益於 Gemini 的大型上下文視窗。Multi-Runtime 讓你為正確的任務匹配正確的大腦。

### 韌性

如果一個供應商當機，其他的繼續運行。結合本地推論後備，DuDuClaw 能承受任何單一供應商的故障。

### 成本最佳化

不同供應商有不同定價。`LeastCost` 輪替策略可以為每種查詢類型路由到性價比最高的供應商。

---

## 與其他系統的互動

- **Account Rotator**：跨所有供應商管理認證，具備跨供應商容錯。
- **Confidence Router**：位於 runtime 層之下（決定本地 vs. 雲端）。Runtime 層決定*哪個*雲端。
- **CostTelemetry**：追蹤每個供應商的成本，支援明智的路由決策。
- **MCP Server**：工具暴露給所有支援的後端（Claude 透過原生 MCP，其他透過工具注入）。
- **Agent Config**：每個 Agent 的 `agent.toml` 指定其 runtime 偏好和備案鏈。

---

## Provider 感知的帳號（WP-A，2026-09）

`accounts.add`——dashboard 帳號頁與 OOBE「AI Runtime 授權」步驟背後共用的 gateway RPC——現在除了既有的 `type`（`api_key` | `oauth`）之外，還接受 `provider` id。可接受的值來自平台的統一 provider 對照表（`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`）：`anthropic`、`openai`、`gemini`/`google`、`deepseek`、`minimax`、`groq`、`together`、`mistral`、`openrouter`、`xai`、`qwen`。不帶 `provider` 時預設為 `"anthropic"`，因此這個功能上線前寫的每一個呼叫端都會維持原本行為不變；未知的 id 會被拒絕，不會被靜默接受。

金鑰仍然寫進 `config.toml` 的 `[[accounts]]` 陣列，只是現在會標上它的 provider：

```toml
[[accounts]]
id = "openai-prod"
type = "api_key"
provider = "openai"
api_key_enc = "..."          # anthropic 維持舊有的 anthropic_api_key_enc 欄位
```

`AccountRotator::select_for_provider`——Claude CLI 路徑與跨供應商 Direct-API 的 `duduclaw-llm` provider 路徑早就在用的同一套挑選邏輯——嚴格依這個欄位篩選，所以用這種方式新增的 OpenAI／Gemini／xAI／DeepSeek…金鑰，會被套進和 Anthropic 帳號完全相同的輪替、預算追蹤與冷卻機制，讀取端不需要為每個 provider 另開一條程式碼路徑。`accounts.list` 與 `accounts.budget_summary` 現在都會在每筆帳號回傳 `provider`，讓 dashboard 帳號頁能顯示每把金鑰屬於哪家服務商；`AddAccountDialog` 新增服務商選單，附上各家的金鑰格式提示與前往該服務商主控台取得金鑰的連結。

## 訂閱登入風險告知

每一個「用你的訂閱帳號一鍵登入」的流程——CLI 登入彈窗、引導式 QR code 設定精靈，以及只會開啟這兩者之一的 OOBE runtime 設定卡片——在開始前都會先顯示風險告知：Anthropic 與 Google 自 2026 年 3 月起已在伺服器端封鎖第三方產品使用消費者訂閱帳號登入，且已有帳號因此被停權；OpenAI 目前政策不明。使用者必須勾選「我了解風險並自行承擔」，流程本身的登入步驟（CLI 子行程、瀏覽器回呼，或裝置碼輪詢）才會開始。API 金鑰路徑不受此關卡影響，仍是預設建議。

---

## 總結

AI 領域是多供應商的。只基於單一 CLI 構建就像只為單一作業系統寫軟體：能用，直到不能用。`AgentRuntime` trait 抽象化了差異，讓 DuDuClaw 將 Claude、Codex、Gemini 和任何 OpenAI 相容端點視為可互換的後端。你的 Agent 每次都能取得最佳可用的大腦。
