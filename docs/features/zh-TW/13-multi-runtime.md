# Multi-Runtime Agent 執行

> 一個平台，十三個 runtime id：十二個 CLI 後端（Claude、Codex、Gemini、Antigravity、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode），加上以 HTTP 連接任何 OpenAI 相容端點的 `openai_compat`。

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

每個後端只描述一次，寫在單一份編譯期表格（`crates/duduclaw-core/src/runtime_catalog.rs`）。偵測、一鍵安裝、模型探索、CLI 登入、模型↔供應商推斷全都讀這張表，所以不會再出現「裝得起來卻偵測不到」或「設定得了卻登入不了」的 runtime。

| Runtime | 執行檔 | 安裝管道 | Headless 呼叫 | 輸出 | 登入方式 | 憑證位置 |
|---|---|---|---|---|---|---|
| Claude Code | `claude` | npm `@anthropic-ai/claude-code` | `-p <prompt> --output-format stream-json` | jsonl | `claude setup-token`（貼回驗證碼） | `~/.claude/.credentials.json` |
| OpenAI Codex | `codex` | npm `@openai/codex` | `exec --json <prompt>` | jsonl | `codex login`（localhost 回呼） | `~/.codex/auth.json` |
| Gemini CLI（v1.67.0 棄用，v1.70.0 移除） | `gemini` | npm `@google/gemini-cli` | `-p --output-format stream-json <prompt>` | jsonl | `gemini auth login`（localhost 回呼） | `~/.gemini/oauth_creds.json` |
| Google Antigravity | `agy` | `antigravity.google/cli/install.sh` | `-p <prompt>` | stream-json（v1.2.10） | 在終端機執行 `agy` 完成 Google 登入（沒有 `login` 子指令），或 API key 模式 | OS keyring |
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

- **Kiro**：AWS FAQ 明文寫著：不允許透過第三方自動化 harness、將請求繞過 Kiro 原生介面。用 DuDuClaw 驅動 Kiro 正屬此類；自行在 CI 直接呼叫 `kiro-cli` 則被允許。所以 Kiro 的安裝管道是手動的：這個決定要由你自己明確做出。
- **Anthropic／Google**：2026-03 起，第三方產品使用消費者訂閱 token 會在伺服器端被封鎖，已有帳號被停權。請走 API key。
- **Qwen**：免費 OAuth 方案已於 2026-04-15 停用，只剩 API key（ModelStudio／DashScope）。
- **OpenCode**：MIT 授權、本身無限制，但它在 1.3.0 移除 Anthropic 訂閱 plugin，理由同上。請用各供應商的 API key。
- **OpenAI**：對「第三方產品驅動 ChatGPT 訂閱登入」的政策不明，API key 才是受支援的路徑。

Dashboard 會顯示對應的條款提示，並要求勾選「我了解風險」才開始訂閱登入。

### 後端怎麼被驅動

五個 CLI 後端（`claude`、`codex`、`gemini`、`antigravity`、`grok`；見 `runtime/mod.rs` 的 `BESPOKE_RUNTIME_IDS`）有各自的 runtime 模組，因為它們各有無法共用的廠商專屬接線，包括帳號輪替（Claude）、以該 CLI 自己的格式注入 MCP 設定、能力→sandbox 旗標的轉譯、空輸出時的 PTY 補救。其餘全部由**同一支**通用 print-mode runtime（`runtime/generic_cli.rs`）直接依目錄項目驅動：用模板 argv 啟動執行檔，把 prompt 以參數或 stdin 送進去，把 text／JSON／JSONL 解析回最終回覆文字，並把非零離開碼或「需要登入」的訊號對應成 failover 鏈看得懂的具名錯誤。其餘七個 CLI（Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode）都走這支通用 runtime。`openai_compat` 不是 CLI，它有自己的 HTTP 模組（`runtime/openai_compat.rs`）。所以手寫模組一共六個。

### 原本的四種後端

下面是 DuDuClaw 最早出貨的四種後端。另外兩個有專屬模組的 CLI，Antigravity 與 Grok，在後面各自的段落說明。

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

> **v1.67.0 棄用，v1.70.0 移除。** Google 在 2026-06-18 停止以 Gemini CLI 服務個人帳號（免費、AI Pro、AI Ultra），請改用 Antigravity runtime。移除前仍可照常使用。Gemini API provider 不受影響。遷移步驟見[已棄用名稱](../../guides/zh-TW/deprecations.md#gemini-cli-runtime)。

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

`/opt/duduclaw/runtimes/bin` 是 DuDuClaw OS 值班機映像放內建 CLI 的位置，即使 gateway 沒有繼承到互動式 `PATH`，映像內建的 runtime 一樣找得到。

Agent 在 `agent.toml` 中指定使用的 runtime：

```toml
[runtime]
provider = "claude"          # 主要後端
fallback = "antigravity"     # 主要不可用時的備案
```

沒有寫 `provider` 時，Agent 使用 Claude。無法辨識的值會記一筆警告，同樣退回 Claude。

### Per-Agent 設定

不同 Agent 可以同時使用不同後端：

```
Agent "dudu"（客服）     → Claude（最佳推理能力）
Agent "coder"（程式產生）→ Codex（針對程式碼最佳化）
Agent "analyst"（資料分析）→ Antigravity
Agent "local"（隱私敏感）→ OpenAI-compat（本地端點）
```

這意味著單一 DuDuClaw 安裝可以協調跨多個 AI 供應商的 Agent，每個都使用最適合其任務的後端。

---

## Effort

新一代推理模型除了選哪個模型之外，還有一個獨立的**深度**旋鈕：這一次呼叫要想多用力。每家廠商的寫法不同，接受的值也不一樣。DuDuClaw 把它做成一個設定，再由系統負責翻譯。

在 agent 上設定：

```toml
# <home>/agents/<id>/agent.toml
[model]
preferred = "claude-opus-5"
effort    = "high"          # low | medium | high | xhigh | max
```

不設定是預設值，代表*完全不傳任何旗標*：spawn 的內容與沒有這個功能的 DuDuClaw 逐位元相同，沿用供應商自己的預設深度。

### 各 runtime 的旗標對應

2026-09-24 針對已安裝的執行檔實測（`research/multi-model-routing-2026-09/17-P0-cli-flag-probe.md` §4 + §6），不是從文件推測：

| Runtime | 實測版本 | effort 的表達方式 | CLI 接受的值 |
|---|---|---|---|
| `claude` | 2.1.258 | `--effort <v>` | `low` `medium` `high` `xhigh` `max` |
| `codex` | 0.156.1 | `-c model_reasoning_effort=<v>`（設定覆寫，沒有專用旗標） | `low` `medium` `high` `xhigh` |
| `antigravity`（`agy`） | 1.2.10 | `--effort <v>` | `low` `medium` `high` |
| `grok` | 1.0.41 | `--reasoning-effort <v>`（別名 `--effort`） | *`--help` 未列舉* |
| `gemini` | — | **沒有這個旗標**，只記 debug 紀錄並忽略 | — |
| `openai_compat` | — | 請求內文的 `reasoning_effort` | `low` `medium` `high` |

### 降階對照表

各家接受的集合不同，所以你的設定會**向下**降到目標 runtime 吃得下的值。不會被靜默丟掉，也不會送出 CLI 會拒絕的值：

| 你設定的值 | claude | codex | antigravity | grok | openai_compat | gemini |
|---|---|---|---|---|---|---|
| `low` | `low` | `low` | `low` | `low` | `low` | — |
| `medium` | `medium` | `medium` | `medium` | `medium` | `medium` | — |
| `high` | `high` | `high` | `high` | `high` | `high` | — |
| `xhigh` | `xhigh` | `xhigh` | **`high`** | **`high`** | **`high`** | — |
| `max` | `max` | **`xhigh`** | **`high`** | **`high`** | **`high`** | — |

Grok 刻意上限在 `high`：它的 `--help` 提到這個旗標卻沒列出可用的值，轉送 `xhigh`/`max` 可能換來一個「unexpected value」而讓整次 spawn 失敗。`openai_compat` 的八個異質 preset 也基於同一個理由設上限。兩個上限都集中在同一處（`duduclaw-core/src/effort.rs`），等實際跑過確認可用的值之後可以調高。

### Direct-API 對應

API 層路徑（`duduclaw-llm`）把同一個值帶進各家的原生欄位：

| 協定 | 欄位 |
|---|---|
| Anthropic Messages | `output_config.effort`（GA，不需 beta header） |
| OpenAI Responses | `reasoning.effort` |
| OpenAI-compat chat/completions | `reasoning_effort`（最上層） |
| Gemini `generateContent` | `generationConfig.thinkingConfig.thinkingLevel`，**未驗證**，見下 |

> Gemini 這個鍵是唯一**尚未確認**的對應。容器用 `thinkingConfig` 已經驗證（既有的 `thinkingBudget` 就用它，且已出貨），但同層的 `thinkingLevel` 鍵無法確認：兩次抓取 ai.google.dev，得到的 `GenerationConfig` 參考頁都被截斷、沒有提到它，而 Interactions API 寫成 `generation_config.thinking_level`，所以 `generateContent` 的 camelCase 版本只是推論。它只在欄位有設定時才會送出，所以沒設 effort 就不可能送出。依賴它之前請重新驗證。

### 成本與快取

調高之前要知道兩件事：

- **effort 要花 token。** 它是繼免費優化（快取、prompt 整理）之後第一個拿品質換成本的槓桿，範圍的最高段只在真正困難的工作上才值回票價。寫程式與長時程的 agent 任務反應明顯；聊天、分類與高流量的路由通常 `low` 就夠。
- **對話中途改 effort 會讓多數模型的 prompt 快取失效**，因為 effort 屬於快取前綴的一部分。每個 agent 挑一個值，之後不要動，不要逐回合調整。

唯一刻意*不*由 agent 決定 effort 的地方是輕量擷取路徑（session 壓縮、GVU、wiki 匯入），固定在 `medium`。機械式的擷取不應該因為某個對話 agent 被調到 `max` 而變貴。

### PTY pool

*（2026-09 已移除。）* effort 以前是 PTY pool 的 **session 快取鍵**的一部分，所以兩次想要不同 effort 的呼叫會得到兩個各自獨立的池化 session。pool 已經不存在，現在每次 spawn 都帶自己的 `--effort` 旗標。

---

## 跨供應商容錯

當某個後端變得不可用（限速、當機或出錯），**FailoverManager** 會自動切換到下一個可用後端：

```
Claude runtime：限速中（冷卻：2 分鐘）
     |
     v
FailoverManager 檢查 agent 設定：
  fallback = "antigravity"
     |
     v
路由到 Antigravity runtime
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

### Codex 非互動核准（2026-09）

Codex 0.156.x 會把每一次 MCP 工具呼叫都擋在核准請求後面。在 `approval_policy=never` 下，那個請求會被自動拒絕，`mcp_servers.<id>.default_tools_approval_mode` 與 `projects.<cwd>.trust_level` 都改變不了結果。`--approve-for-me`（自動審查）是受支援的非互動出口，且與 `-s/--sandbox` 互斥。Agent 目錄不是 git 儲存庫，所以一律帶 `--skip-git-repo-check`，並關閉 stdin。

**每個 capability 等級對應一組旗標**（2026-09-28 變更，限制 agent 之前請先讀 ReadOnly 那一列）：

| `[capabilities]` 等級 | Codex 旗標 | agent 能做什麼 |
|---|---|---|
| ReadOnly（沒授予寫入工具，或全部被拒） | `-s read-only -c approval_policy=never` | 能讀、能推理。寫入**確實被擋下**。**每一次 MCP 工具呼叫都會被自動拒絕**，所以那一輪 agent 沒有任何 duduclaw 工具。每次 spawn 會留一筆 `warn!` 說明這點。 |
| WorkspaceWrite（預設） | `--approve-for-me -c approval_policy=never -c sandbox_mode="workspace-write"` | 可寫入工作區內；擁有完整的 duduclaw MCP 工具。 |
| FullAccess（明確設定 `computer_use = true`） | `--dangerously-bypass-approvals-and-sandbox` | 沒有任何限制。只能由操作者明確授予。 |

在 2026-09-28 之前，ReadOnly 也用 `--approve-for-me` 加 `-c sandbox_mode="read-only"`。那個做法**失效時是放行的**：`--approve-for-me` 的自動審查在 workspace-write sandbox 裡執行，所以 read-only 宣告只是參考，受能力限制的 agent 仍然寫得了檔案。現在改帶真正生效的旗標，代價是 MCP 工具介面。如果需要 agent 保有工具，請授予 WorkspaceWrite；在 Codex 上，ReadOnly 的意思是「什麼都不准改」，工具也包含在內。

### Codex MCP 憑證：0.157+ 用 `env_vars`，更舊的版本退回 `argv`（2026-09-28）

Codex spawn 透過每次呼叫的 `-c` 設定覆寫來註冊 duduclaw MCP server，這份註冊帶的值裡有兩個是機密：`DUDUCLAW_MCP_API_KEY` 與 `DUDUCLAW_AGENT_TOKEN`。每次 Codex spawn 都會做這份註冊（以及下面所有處理），ReadOnly 也一樣：ReadOnly 時 server 照樣註冊，但 Codex 會自動拒絕對它的每一次呼叫，也就是上表所說的情況。

**為什麼憑證不能直接放在環境變數。** 2026-09-28 實測，並對照 Codex 原始碼確認：Codex 會對每個 stdio MCP server 子行程做 `env_clear()`，只補回 11 個名稱的預設白名單（`HOME`、`PATH`、`SHELL`、`USER`、`LOGNAME`、`TERM`、`TMPDIR`、`TZ`、`LANG`、`LC_ALL`、`__CF_USER_TEXT_ENCODING`），再加上設定裡宣告的項目。Gateway 自己的行程環境到不了 MCP server，所以單用 `Command::env()` 什麼都送不到，設定通道是唯一的通道。

**DuDuClaw 現在的做法。** 設定通道有兩種形狀，依該 Codex 執行檔回報的版本，逐個執行檔決定：

| Codex 版本 | 憑證形狀 | `ps` 看得到 |
|---|---|---|
| **≥ 0.157.0** | `-c mcp_servers.duduclaw.env_vars=["DUDUCLAW_MCP_API_KEY", "DUDUCLAW_AGENT_TOKEN"]`，值設在 Codex **行程**的環境，由 Codex 複製進 MCP 子行程 | 只有變數**名稱** |
| **< 0.157.0**，或讀不到版本 | `-c mcp_servers.duduclaw.env.<K>="<value>"`（先前的行為） | 憑證**值** |

非憑證項目（`DUDUCLAW_HOME`、`DUDUCLAW_PORT`、`DUDUCLAW_AGENT_ID`、`DUDUCLAW_INSTANCE`）在兩條路徑上都維持 `env.<K>="<value>"` 的形式。它們不是機密，留在設定表裡，即使行程環境日後被清掉，註冊仍然有效。「憑證」以名稱結尾精確判定：`_API_KEY`、`_TOKEN`、`_SECRET`、`_PASSWORD`（ASCII 不分大小寫），與 `duduclaw-core` 的 spawn-env 白名單採用同一套形狀慣例。

**為什麼要依版本判斷，而不是一律使用。** `env_vars` 已在 `codex-cli 0.157.1` 驗證可用，但最低接受這個鍵的版本尚未確認，而且 `RawMcpServerConfig` 帶有 `deny_unknown_fields`。在舊到不認得它的 Codex 上，整個執行會在解析設定時死掉（每次 spawn 都失敗），或是靜默丟掉憑證（agent 失去所有 duduclaw 工具，卻沒有任何錯誤）。所以 gateway 對每個執行檔路徑、每個行程只跑一次 `codex --version`，解析 `codex-cli X.Y.Z`，低於 `0.157.0`、無法解析或無法探測（spawn 失敗、非零離開碼、5 秒逾時）都視為「不支援」，退回舊的 `argv` 形狀並留一筆 `warn!`。探測失敗絕對不會讓 spawn 失敗。

**如果你走的是退回路徑**（較舊的 Codex、共用或多租戶主機），暴露是真實的：命令列參數同一台主機上的任何行程都讀得到（`ps -ww`、`/proc/<pid>/cmdline`）。把 Codex CLI 升級到 0.157.1 或更新，憑證就會離開 `argv`，不需要改任何設定。

**兩條路徑共通的緩解。** 允許進入 `argv` 的 env 鍵集合由測試鎖定在已知的 `DUDUCLAW_*` 區塊，新增機密無法悄悄加入；每個鍵在插入前都驗證為單純的 TOML 鍵（包含 `env_vars` 陣列內的名稱）；每個值都經過 TOML 引號處理。

### 工作目錄覆寫涵蓋每個 CLI 後端（2026-09-28）

呼叫端可以要求某一次 spawn 在 agent 自己目錄以外的地方執行。目前唯一的呼叫端是團隊組成器（team composer），它把角色成員放進員工的工作區，這樣成員寫出的檔案才不會在用完即丟的 scaffold 於成員結束的瞬間被回收時一起消失。

在 2026-09-28 之前，只有 Codex 後端會照辦。Gemini、Antigravity 與 Grok 無論如何都在 agent 目錄 spawn，所以跑在這三者上的角色成員，工作做在幾秒後就被刪掉的目錄裡。現在四者都透過同一個共用 helper 解析工作根目錄，該 helper 也會檢查要求的路徑是否為真實目錄，否則警告並退回 agent 目錄，不會 spawn 到不存在的地方。原生 OS sandbox 的範圍與同一個根目錄一致，所以被覆寫的根目錄才是取得寫入權限的那一個。

覆寫只移動**工作目錄**。Agent 身分（MCP server 註冊、工具用來驗證的 agent id、agent 自己的設定）仍留在 agent 目錄。

兩個後端專屬的影響，先寫明，不留給使用者自己發現：

- **Antigravity** 會預先信任工作根目錄（`agy` 會跳出互動式的「信任這個工作區？」提示，這會讓 headless 執行卡住），並以 `--add-dir` 傳入。
- **Grok** 的 MCP 註冊（`.grok/config.toml`）與 sandbox profile 名稱（`.grok/sandbox.toml`）都從工作目錄解析，不是從 agent 目錄。所以被覆寫的根目錄會得到這兩個檔案各自的副本，否則成員 spawn 時會沒有工具，也找不到可解析的 sandbox profile。已知限制：這兩個檔案以目錄為鍵，所以兩個共用同一工作區的 Grok 角色成員會互相覆寫對方宣告的 env 區塊。成員之間的 command/args 部分相同，MCP 子行程實際用來驗證的是各行程自己的身分，所以影響範圍只限宣告的區塊。

### Antigravity 的認證與 MCP 工具（2026-10-01）

`agy` 沒有 `login` 子指令，所以儀表板不提供它的一鍵登入。認證有兩種方式：

- **Google 登入**：在跑 DuDuClaw 的主機終端機執行 `agy`，照提示完成。憑證存在 OS keyring，所以容器或沒有 keyring、瀏覽器的遠端主機做不到。
- **API key 模式**：在 `config.toml` 設定 `[antigravity] auth = "api_key"`，並提供 Gemini API key，可以是 `gemini` provider 帳號，或環境變數 `GEMINI_API_KEY`。Gateway 會自己把 `modelProvider` 寫進 agy 的設定。`auth = "login"` 切回 Google 登入，並移除那筆 `modelProvider`。如果從來沒設過 `auth`，gateway 不會動 `modelProvider`，也不傳 Gemini key，agy 維持原本的認證方式。

沒有 `ANTIGRAVITY_API_KEY` 這個變數。平台的 MCP 工具註冊在各 agent 工作區的 `<agent workspace>/.agents/mcp_config.json`。

切換 API key 模式前要知道的三件事：

- 這個設定對整個作業系統使用者生效。agy 的 `modelProvider` 存在使用者層的設定檔，所以你自己在同一個帳號下互動使用的 `agy` 也會改走 API key。
- 同一個使用者底下跑兩個 gateway、`auth` 設成不同值時，兩邊會互相覆蓋這個欄位。
- 用過 `api_key` 之後要回到 Google 登入，請明確寫 `auth = "login"`。只刪掉 `auth` 這一行不夠：沒有設定時 gateway 不動 `modelProvider`，先前寫入的 `"gemini"` 會留在 agy 的設定裡，gateway 只會在紀錄裡提醒。`login` 模式下，gateway 不會把 `GEMINI_API_KEY`／`GOOGLE_API_KEY` 傳給 agy 與它執行的指令；`api_key` 模式下，員工的 shell 讀得到這把金鑰（agy 必須從環境變數取得它）。

### Antigravity 的工具權限（尚未發布，v1.69.0 之後）

`agy` 1.2.16 在 print mode 會自動拒絕所有無法詢問人類的確認，而呼叫 MCP 工具需要一次確認。這個修正之前，預設能力等級（帶 `--sandbox`）的 Antigravity 員工用不了任何平台工具，只有傳入 `--dangerously-skip-permissions` 的完全放行等級可以。這個缺陷從 v1.67.0 就存在，2026-10-04 用真的 Gemini API key 驗證時才發現。

閘道每次執行 Antigravity 回合時，本來就會把工作根目錄寫進 agy 使用者層設定檔 `~/.gemini/antigravity-cli/settings.json` 的 `trustedWorkspaces`。同一次加鎖寫入現在也會在 `permissions.allow` 補上兩條規則：

- `mcp(duduclaw/*)` 放行名為 `duduclaw` 的 MCP 伺服器的所有工具。
- `read_file(<HOME>/.gemini/antigravity-cli/mcp/duduclaw)` 放行讀取該伺服器的工具說明檔。agy 的 MCP 工具是延後載入的，模型每次呼叫前要先讀說明檔，這個讀取在 print mode 同樣會被拒。`HOME` 正規化後路徑不同時（例如 macOS 的 `/var` 與 `/private/var`），兩種寫法都會寫入。

閘道不加任何終端機指令、寫檔或網址的規則，命令列旗標也沒有改，所以預設等級仍是 `--sandbox`。以 agy 1.2.16 與真的 Gemini API key 實測：預設等級的員工可以呼叫 DuDuClaw 工具；同一輪的終端機指令與寫到工作區以外的檔案仍被拒絕，路徑穿越的讀取與透過指向目錄外的符號連結的讀取也被拒絕。把 `--sandbox` 與「全部自動核准」旗標合用會讓寫檔工具寫到工作區以外，所以沒有採用。

操作者的規則會保留：既有的 `allow`、`deny`、`ask` 項目原樣不動。`permissions` 不是物件、或 `allow` 不是陣列時，閘道不改寫它並記一筆警告；工作區信任與 `modelProvider` 照寫，執行失敗時錯誤訊息會說明規則寫不進去。HOME 路徑含 `(`、`)`、`,`、`*` 或換行，或不是合法 UTF-8 時，會略過 `read_file` 規則並記警告，只寫 `mcp(duduclaw/*)`。

**唯讀等級。** 這兩條規則寫在使用者層設定檔，無法依員工的能力等級區分，所以唯讀的 Antigravity 員工拿到同樣兩條規則，也可以呼叫平台工具；它能做什麼由 MCP 伺服器自己的 `allowed_tools`、`denied_tools` 與審批清單決定。這與 Claude runtime 相同，與 Codex 不同：Codex 在唯讀等級下所有 MCP 工具呼叫都會被拒（見上方表格）。

**要知道的副作用。**

- 規則寫在該作業系統使用者所有 `agy` 共用的設定檔裡。你自己在終端機互動使用 `agy` 時，名為 `duduclaw` 的 MCP 伺服器的工具呼叫與那個說明檔目錄的讀取，也會自動放行而不詢問。
- 規則只增不減。移除員工、解除安裝 DuDuClaw 或不再使用 Antigravity 之後，規則會留在檔案裡，`trustedWorkspaces` 的項目原本就是這樣。要移除，編輯 `~/.gemini/antigravity-cli/settings.json`，從 `permissions.allow` 刪掉這兩條。
- 同一個作業系統使用者跑兩個閘道時，兩邊寫的規則相同，不會互相覆蓋成不同內容。

**錯誤訊息。** agy 拒絕工具時，閘道回報的錯誤現在會指名被拒的工具，並附上 agy 自己的錯誤文字，金鑰在截斷之前先遮蔽。agy 回報成功、回覆卻是空的、而且有工具被拒時，這次執行判為錯誤；以前會把原始的結果 JSON 當成員工的回答。回覆正常但有工具被拒時，回覆保留並記一筆警告。

**尚未驗證。** 最後一次修改這段程式之後，真金鑰的端對端測試還沒有重跑；也沒有在 Linux、Docker 容器內與 Windows 上測過。另有兩個已知的 Antigravity 問題這次沒有處理：agy 遇到 503 重試成功後仍可能回報失敗，閘道會把完整的回覆當成失敗；Antigravity 失敗後，跨廠商容錯可能改用 Claude。

### Antigravity 串流解析改為降級而非失敗（2026-09-28）

`agy --output-format stream-json` 的解碼器以前在六個獨立的地方都很嚴格：一行無法解析、缺少 `result` 事件、缺少 `response` 欄位，或 `usage` 區塊少一個整數，整次執行就變成錯誤。已經*回答完畢*的 `agy` 被回報成 spawn 失敗，角色成員也跟著丟了。

形狀不符現在改為降級，事實則不會。無法解析的行會略過。缺少 result 時，以串流中最後一個非空行當作答案。缺少或不完整的 usage 區塊得到「未知 token」，不會編出一個 0。每次降級都留一筆 `warn!`，指出缺了什麼。仍然直接失敗的只有一種情況：明確的非 `SUCCESS` 狀態，那是 `agy` 告訴我們這次執行失敗，不是我們沒認出的形狀。

### 備援 runtime 拿到哪個模型（2026-09）

切換到另一個 runtime 時，絕不會把原本的模型 id 原封不動轉送（Codex agent 的 `gpt-5.4` 不能交給 Claude CLI）。FailoverManager 依四個有順序的分支解析備援模型，都不適用時拒絕 spawn：

1. `agent.toml [model] fallbacks` 中第一個其家族明確屬於備援 runtime 的項目（`openai/gpt-5.4` 這類帶前綴的 id，以 Direct-API 鏈所用的同一套 `split_model_id` 規則去掉前綴）；
2. 原本的模型，如果它本來就屬於備援 runtime；
3. 該 runtime 的目錄預設值（`fallback_models[0]`，與 dashboard 在即時探索失敗時提供的清單相同）；
4. 否則這次嘗試記為失敗，訊息為 `no model configured for fallback runtime <name>`，不 spawn。

每次替換都以 `warn` 層級記錄 `agent / from_runtime / to_runtime / from_model / to_model`。

**Judge 與 evaluator 呼叫完全不參與跨家族 failover**（2026-09-28 更正）。操作者指定 judge runtime 或模型（`[dispatch] judge_provider` / `judge_model`）時，這次呼叫的重點就是*由哪一家回答*，所以失敗的 judge spawn 絕不會靠換成另一家的模型來救援，呼叫端會明確、可見地降級。這個不參與的條件以前只有在 judge 提示把 provider 從解析出的預設值*移開*時才觸發，所以當 judge 家族與預設的 utility 家族碰巧相同（例如兩者都是 `codex`，正是去相關 judge 設定在 Codex 也被設為預設 utility runtime 後會落入的情況），替換就會悄悄恢復。現在只要指名了某個家族，就是要求那一家，不論它是否同時是預設。沒有指名的 utility 呼叫，failover 行為不變。

- **Account Rotator**：跨所有供應商管理認證，具備跨供應商容錯。
- **Confidence Router**：位於 runtime 層之下（決定本地 vs. 雲端）。Runtime 層決定*哪個*雲端。
- **CostTelemetry**：追蹤每個供應商的成本，支援明智的路由決策。
- **MCP Server**：工具暴露給所有支援的後端（Claude 透過原生 MCP，其他透過工具注入）。
- **Agent Config**：每個 Agent 的 `agent.toml` 指定其 runtime 偏好和備案鏈。

---

## Provider 感知的帳號（WP-A，2026-09）

`accounts.add`（dashboard 帳號頁與 OOBE「AI Runtime 授權」步驟背後共用的 gateway RPC）現在除了既有的 `type`（`api_key` | `oauth`）之外，還接受 `provider` id。可接受的值來自平台的統一 provider 對照表（`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`）：`anthropic`、`openai`、`gemini`/`google`、`deepseek`、`minimax`、`groq`、`together`、`mistral`、`openrouter`、`xai`、`qwen`。不帶 `provider` 時預設為 `"anthropic"`，因此這個功能上線前寫的每一個呼叫端都會維持原本行為不變；未知的 id 會被拒絕，不會被靜默接受。

金鑰仍然寫進 `config.toml` 的 `[[accounts]]` 陣列，只是現在會標上它的 provider：

```toml
[[accounts]]
id = "openai-prod"
type = "api_key"
provider = "openai"
api_key_enc = "..."          # anthropic 維持舊有的 anthropic_api_key_enc 欄位
```

`AccountRotator::select_for_provider`（Claude CLI 路徑與跨供應商 Direct-API 的 `duduclaw-llm` provider 路徑早就在用的同一套挑選邏輯）嚴格依這個欄位篩選，所以用這種方式新增的 OpenAI／Gemini／xAI／DeepSeek…金鑰，會被套進和 Anthropic 帳號完全相同的輪替、預算追蹤與冷卻機制，讀取端不需要為每個 provider 另開一條程式碼路徑。`accounts.list` 與 `accounts.budget_summary` 現在都會在每筆帳號回傳 `provider`，讓 dashboard 帳號頁能顯示每把金鑰屬於哪家服務商；`AddAccountDialog` 新增服務商選單，附上各家的金鑰格式提示與前往該服務商主控台取得金鑰的連結。

## 訂閱登入風險告知

每一個「用你的訂閱帳號一鍵登入」的流程（CLI 登入彈窗、引導式 QR code 設定精靈，以及只會開啟這兩者之一的 OOBE runtime 設定卡片）在開始前都會先顯示風險告知：Anthropic 與 Google 自 2026 年 3 月起已在伺服器端封鎖第三方產品使用消費者訂閱帳號登入，且已有帳號因此被停權；OpenAI 目前政策不明。使用者必須勾選「我了解風險並自行承擔」，流程本身的登入步驟（CLI 子行程、瀏覽器回呼，或裝置碼輪詢）才會開始。API 金鑰路徑不受此關卡影響，仍是預設建議。

---

## 總結

AI 領域是多供應商的。只基於單一 CLI 構建就像只為單一作業系統寫軟體：能用，直到不能用。`AgentRuntime` trait 抽象化了差異，讓 DuDuClaw 將 Claude、Codex、Gemini 和任何 OpenAI 相容端點視為可互換的後端。你的 Agent 每次都能取得最佳可用的大腦。
