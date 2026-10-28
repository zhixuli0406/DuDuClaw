# Multi-Runtime Agent 執行

> 一個平台，十二個 runtime id：十一個 CLI 後端（Claude、Codex、Antigravity、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode），加上以 HTTP 連接任何 OpenAI 相容端點的 `openai_compat`。

## 執行失敗與明確備援

`[runtime] fallback` 缺省或無效時，不會自動改試 Claude。只有明確設定的 runtime 備援會進入既有的健康、政策與相容模型檢查；帳號輪替、模型備援及已設定的本地推論仍保留。非 Claude 對話失敗不會再額外呼叫 Anthropic Direct API。

錯誤訊息及診斷會區分原選後端、明確備援與未實際執行的原因。GPT／Codex 或 API 後端的認證錯誤不會要求登入 Claude；只有真正的 Claude CLI 認證失敗會開啟 Claude 警報，真正的 Claude CLI 成功才會清除。OpenAI 相容端點不一定是 OpenAI，請依實際設定檢查憑證。原先仰賴隱式 Claude 備援的設定，需要由管理員明確填入 `fallback`。

## 模型與執行環境的一致性

建立員工、套用編輯過的板模、儲存模型及管理員的 `/model` 指令，共用相同的模型家族校準規則。可確定模型屬於另一個執行環境時，儲存會一併修正 provider；保留明確的 API 相容後端、備援與未知的自訂模型。推定變更仍受原本的管理員權限與產品政策限制。同值 `/model` 也可修正舊的錯配，回覆會區分修正成功與熱重載失敗。

未經儲存的舊設定若有確定的模型／provider 錯配，請管理員儲存修正後再重試。讀取與路由不會偷偷改設定檔，也不會把這種設定問題當成 Claude 未登入。未知模型不會僅憑名稱片段被改派。

## Discord 的員工選擇

一般訊息、文字指令（例如 `/model`）與 `/ask` 共用明確選擇順序：員工專屬 Bot 綁定、伺服器或私訊設定、全域設定。伺服器或私訊的空值會沿用全域設定；明確指定的員工已刪除或改名時會拒絕，不默默換成主要員工。

完全沒有明確指定時，一般對話保留原本的頻道／討論串綁定、允許頻道與預設員工路由，再依發問者權限處理 `@` 呼叫。文字指令沿原預設路由決定操作對象，不把指令文字當成員工呼叫。`/agent` 與切換員工選單仍須「管理伺服器」權限才能改綁；回覆署名以回覆流程實際選到的員工為準，包含頻道綁定與 `@` 呼叫的員工。文件交付也沿用該員工目錄的既有檢查；沒有選到回覆員工時，不會掃描主要員工的檔案。

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

每個後端（Claude、Codex、Antigravity 或任何 OpenAI 相容端點）都實作相同的介面。系統的其餘部分不需要知道、也不在乎是哪個後端在處理特定請求。

### Runtime 目錄（Runtime Catalog）

每個後端只描述一次，寫在單一份編譯期表格（`crates/duduclaw-core/src/runtime_catalog.rs`）。偵測、一鍵安裝、模型探索、CLI 登入、模型↔供應商推斷全都讀這張表，所以不會再出現「裝得起來卻偵測不到」或「設定得了卻登入不了」的 runtime。

| Runtime | 執行檔 | 安裝管道 | Headless 呼叫 | 輸出 | 登入方式 | 憑證位置 |
|---|---|---|---|---|---|---|
| Claude Code | `claude` | npm `@anthropic-ai/claude-code` | `-p <prompt> --output-format stream-json` | jsonl | `claude setup-token`（貼回驗證碼） | `~/.claude/.credentials.json` |
| OpenAI Codex | `codex` | npm `@openai/codex` | `exec --json <prompt>` | jsonl | `codex login --device-auth`（裝置碼，適用 Docker／遠端） | `~/.codex/auth.json` |
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

**Dashboard 一鍵登入**提供 Claude、Codex、Grok、Kimi、Copilot、Kiro 和 OpenCode。Codex 改用裝置碼登入：在瀏覽器開啟 CLI 顯示的驗證頁並輸入裝置碼，等待 CLI 完成；不需要把 OAuth callback URL 的 `code` 貼回 Dashboard。Cursor 仍可作為 runtime，但瀏覽器回呼無法可靠地從遠端 Dashboard 完成，因此不列入一鍵登入選單。Antigravity 沒有腳本化登入子指令，Qwen 與 Mistral Vibe 走 API key。帳號頁 API Token 卡片現在可用「刪除」移除，須確認帳號 id；刪除的是 DuDuClaw 儲存的該筆帳號，不會撤銷供應商端的金鑰。

Gemini CLI 已從 DuDuClaw runtime 與 Docker 套件移除。既有 `runtime = "gemini"` 設定須遷移到 Antigravity；獨立的 Gemini API provider 仍受支援。見[移除與遷移說明](../../guides/zh-TW/deprecations.md)。

模型選擇每家寫法不同，目錄一併記下是哪一種：獨立旗標（`--model <id>`）、Copilot 文件寫的等號式（`--model=<id>`）、完全沒有旗標時改用環境變數（Mistral Vibe 的 `VIBE_ACTIVE_MODEL`），或是沒有（Kiro 的模型走 `kiro-cli settings`，不是每次呼叫指定）。

**啟用 runtime 前該讀的廠商條款：**

- **Kiro**：AWS FAQ 明文寫著：不允許透過第三方自動化 harness、將請求繞過 Kiro 原生介面。用 DuDuClaw 驅動 Kiro 正屬此類；自行在 CI 直接呼叫 `kiro-cli` 則被允許。所以 Kiro 的安裝管道是手動的：這個決定要由你自己明確做出。
- **Anthropic**：[法律與合規說明](https://code.claude.com/docs/en/legal-and-compliance)寫明，第三方開發者不得在自己的產品裡提供 Claude.ai 登入，也不得用 Free、Pro、Max 方案的憑證替使用者轉送請求，更不得收集、儲存或中介 Claude.ai 的憑證或 session token。允許的是你本人用自己的訂閱登入未經修改的 Claude Code；Pro 和 Max 的用量上限以一般使用為前提。Anthropic 可以不事先通知就執行，也有使用者回報帳號被停權。Anthropic 為產品記載的做法是 Claude Console 的 API key。
- **Google**：[Gemini CLI 條款](https://geminicli.com/docs/resources/tos-privacy/)寫明，用第三方軟體、工具或服務透過 Gemini CLI 的 OAuth 直接存取其背後的服務屬於違規，可能導致帳號被停權或終止。[Antigravity 條款](https://antigravity.google/terms)第 6 節對第三方軟體存取 Antigravity 也有同樣規定，可能停權或終止 Antigravity 和／或 Gemini CLI 帳號；同一節也把搭配非 Google 提供的產品使用該服務列為濫用。DuDuClaw 執行的是原版 Antigravity CLI，算不算在條款範圍內由 Google 判斷。Google 的 [Gemini Code Assist 個人版停用公告](https://developers.google.com/gemini-code-assist/docs/deprecations/code-assist-individuals)（2026-10-09 更新）另外寫明，自 2026-06-18 起，Gemini Code Assist 個人版、Google AI Pro 與 Google AI Ultra 帳號已不能在 Gemini CLI 使用 Login with Google，付費 API key（Gemini API 或企業方案）仍可使用。請走 API key。
- **Qwen**：免費 OAuth 方案已於 2026-04-15 停用，只剩 API key（ModelStudio／DashScope）。
- **OpenCode**：MIT 授權、本身無限制，但它在 1.3.0 移除 Anthropic 訂閱 plugin，理由同上。請用各供應商的 API key。
- **OpenAI**：OpenAI 有官方的 Sign in with ChatGPT 計畫（[條款](https://openai.com/policies/sign-in-with-chatgpt-terms/)，2026-09-29 版）。[快速入門](https://developers.openai.com/siwc/quickstart)寫明，Sign in with ChatGPT 目前以有限試行的方式開放給選定的商業合作夥伴，ChatGPT 方案額度開放給所有開源合作夥伴與選定的私人客戶；[說明文章](https://help.openai.com/en/articles/20001542-using-your-chatgpt-plan-in-other-apps-and-sites)寫明受支援的開源工具照常可用，商業合作夥伴要填表申請。也就是說，商業產品要先取得 OpenAI 同意。條款還要求：使用者不必付費給合作夥伴或升級方案就能使用自己的 ChatGPT 方案；驗證 token 若要長期保存，只能存在使用者自己掌控的本機，不能放在遠端或託管環境；不得藉這個登入提供給其他工具或無關請求使用的通用 API；違反時可能被停用 Sign in with ChatGPT。OpenAI 的 Codex 負責人在 2026-09-08 [發文](https://x.com/thsottiaux/status/2097131394199896166)回應某個第三方應用程式：「Sorry, but this is not an approved use of Sign in With ChatGPT. We support pure OSS clients and others we have partnerships with, but you need to reach out and talk to us for that.」DuDuClaw 跑在遠端主機或容器裡時，Codex 的 token（`~/.codex/auth.json`）也存在那裡。[使用條款](https://openai.com/policies/terms-of-use/)仍然適用。要穩定使用請走 API key。
- **xAI**：沒有找到 xAI 對第三方產品操作 SuperGrok／X Premium+ 訂閱的公開立場，[服務條款](https://x.ai/legal/terms-of-service)仍然適用。

Dashboard 會顯示對應的條款提示，要求勾選同意後才開始或儲存訂閱登入；gateway 也會拒絕沒有帶同意旗標的呼叫（見[訂閱登入風險告知](#訂閱登入風險告知)）。

### 後端怎麼被驅動

四個 CLI 後端（`claude`、`codex`、`antigravity`、`grok`；見 `runtime/mod.rs` 的 `BESPOKE_RUNTIME_IDS`）有各自的 runtime 模組，因為它們各有無法共用的廠商專屬接線，包括帳號輪替（Claude）、以該 CLI 自己的格式注入 MCP 設定、能力→sandbox 旗標的轉譯、空輸出時的 PTY 補救。其餘全部由**同一支**通用 print-mode runtime（`runtime/generic_cli.rs`）直接依目錄項目驅動：用模板 argv 啟動執行檔，把 prompt 以參數或 stdin 送進去，把 text／JSON／JSONL 解析回最終回覆文字，並把非零離開碼或「需要登入」的訊號對應成 failover 鏈看得懂的具名錯誤。其餘七個 CLI（Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode）都走這支通用 runtime。`openai_compat` 不是 CLI，它有自己的 HTTP 模組（`runtime/openai_compat.rs`）。所以手寫模組一共五個。

### 主要後端

下面介紹 Claude、Codex 與 OpenAI 相容端點。Antigravity 與 Grok 在後面各自說明。

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

2026-09-24 針對已安裝的執行檔實測（記錄於內部實測筆記，未公開），不是從文件推測：

| Runtime | 實測版本 | effort 的表達方式 | CLI 接受的值 |
|---|---|---|---|
| `claude` | 2.1.258 | `--effort <v>` | `low` `medium` `high` `xhigh` `max` |
| `codex` | 0.156.1 | `-c model_reasoning_effort=<v>`（設定覆寫，沒有專用旗標） | `low` `medium` `high` `xhigh` |
| `antigravity`（`agy`） | 1.2.10 | `--effort <v>` | `low` `medium` `high` |
| `grok` | 1.0.41 | `--reasoning-effort <v>`（別名 `--effort`） | *`--help` 未列舉* |
| `openai_compat` | — | 請求內文的 `reasoning_effort` | `low` `medium` `high` |

### 降階對照表

各家接受的集合不同，所以你的設定會**向下**降到目標 runtime 吃得下的值。不會被靜默丟掉，也不會送出 CLI 會拒絕的值：

| 你設定的值 | claude | codex | antigravity | grok | openai_compat |
|---|---|---|---|---|---|---|
| `low` | `low` | `low` | `low` | `low` | `low` |
| `medium` | `medium` | `medium` | `medium` | `medium` | `medium` |
| `high` | `high` | `high` | `high` | `high` | `high` |
| `xhigh` | `xhigh` | `xhigh` | **`high`** | **`high`** | **`high`** |
| `max` | `max` | **`xhigh`** | **`high`** | **`high`** | **`high`** |

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

### Codex：員工的其他 MCP 伺服器（2026-10-19）

Codex 不讀 `.mcp.json`，所以 Codex spawn 用登記 duduclaw 的同一種方式登記員工的其他伺服器：`-c mcp_servers.<name>.…` 覆寫（也寫進 `<agent_dir>/.codex/config.toml`）。之前 spawn 只登記 duduclaw，`.mcp.json` 裡像券商連線這類伺服器在 Codex 上會消失。規則如下：

- `.mcp.json` 讀不到或解析失敗時，這次 spawn 停止（行為變更：以前 Codex 不讀這個檔）。沒有這個檔，或檔裡只有 DuDuClaw 自己的伺服器（團隊角色的骨架寫的就是這樣），維持原本的啟動流程：不跑閘門、不寫檔。在員工 runtime 上跑的輔助呼叫（摘要、分類、裁判）不會拿到員工的伺服器，和 Claude 的輔助呼叫一樣。
- 有第三方伺服器時，先跑 `.mcp.json` spawn 閘門，和 Claude spawn 一樣；閘門拒絕就停止，而且不寫任何東西。通過時可能改寫檔裡 DuDuClaw 自己的項目（其他項目保留），和 Claude 每次 spawn、gateway 每次開機做的修復相同。
- 每個第三方 stdio 伺服器都登記成 `duduclaw mcp-proxy --server <name> -- <command> …`，一律如此（Claude 只在有 `action_rules` 或唯讀通道時才這樣做），因為 Codex 以 `approval_policy=never` 執行，這些伺服器又以 `default_tools_approval_mode = "auto"` 登記，真正做決定的是 proxy 的第三方工具閘門。遠端橋接照原樣登記（它們自己管制）；直接的 `url`／`type` 項目會被移除，並記稽核 `third_party_remote_entry_dropped`。無法包裝的項目、名稱不是單純 TOML 鍵的項目，不會登記。
- `.mcp.json` 裡用 DuDuClaw 名稱（`duduclaw`、`duduclaw-pro`、`duduclaw-<instance>`）的項目，永遠取代不了 gateway 自己產生的伺服器。
- 第三方伺服器自己的環境變數（它的憑證）放進 `<home>/tmp/mcp-proxy-env/`（0700 目錄）裡這次 spawn 專用、只有擁有者能讀的檔案，由 `DUDUCLAW_MCP_PROXY_ENV_FILE` 指向，不放進 `argv`，也不寫進 `.codex/config.toml`；gateway 啟動時會刪掉超過 15 分鐘的舊檔。所以只靠 `.codex/config.toml` 手動跑 `codex` 時，這些伺服器啟動時不會帶它們宣告的環境變數。`.codex/config.toml` 從建立那一刻起就是只有擁有者能讀（先寫暫存檔再改名），已存在的檔案也會重設成這個權限。`DUDUCLAW_MCP_API_KEY` 照上表處理（0.157 以上走 `env_vars`）。
- Codex 會清空 MCP 子行程的環境，所以唯讀通道以 `env.DUDUCLAW_LANE` 傳給每個第三方伺服器。

### Grok、Antigravity 與工具迴圈：員工的其他 MCP 伺服器（2026-10-20）

其他自己登記 MCP 伺服器的 runtime 也有同樣的缺口，Grok 上的情況比少一台伺服器更糟。現在它們都和 Codex 走同一套管制改寫：stdio 伺服器經過 `duduclaw mcp-proxy`，遠端橋接照原樣，直接的 `url`／`type` 項目會被移除並記稽核，用 DuDuClaw 名稱的項目取代不了 gateway 自己的伺服器，名稱含英數字、`_`、`-` 以外字元（或超過 64 字）的項目不登記。

**Grok。** Grok 1.0.62 會自己讀工作目錄的 `.mcp.json`（`grok inspect` 把它列為來源，內附文件也這樣寫）。對員工來說就是 `<agent_dir>/.mcp.json`，所以員工的其他伺服器其實一直由 Grok 直接啟動，沒有經過 proxy，也就沒有 `action_rules`、核准流程或唯讀通道的限制，`url` 項目也是直接連線。Grok 的 `.grok/config.toml` 優先於 `.mcp.json`，Grok 讀的不只這個（`grok inspect` 實測，內附的 `07-mcp-servers.md`、`05-configuration.md` 也這樣寫）：還有 `<cwd>/.cursor/mcp.json`（Cursor 相容，預設開啟），以及目錄在 git repository 裡時，一路到 repository 根目錄每一層的 `.mcp.json` 與 `.grok/config.toml`。它不讀 `<cwd>/.claude.json`，也不讀 `.claude/settings*.json` 的 `mcpServers`。這些來源的優先順序都低於工作目錄的 `.grok/config.toml`，所以 runtime 現在把它們列出的每個名稱都寫進這個檔：員工自己 `.mcp.json` 裡通過管制的伺服器寫成 spawn 範圍的 proxy（見下），其他的（包括 `duduclaw-pro` 這類 DuDuClaw 名稱的多餘項目）寫成停用項目，把原本那一項蓋掉。來源存在卻解析不了時拒絕 spawn。員工已經不再列出的伺服器，runtime 先前寫的項目會被移除；管理者手動加的項目不動，每次 spawn 記一次警告。員工目錄裡的 `.cursor/` 現在和 `.grok/` 一樣，員工不能改。

**Antigravity。** agy 只讀 `.agents/mcp_config.json`（1.4.2 實測：`.mcp.json` 裡的伺服器從不啟動），所以這些伺服器之前就是不存在。現在每個通過管制的伺服器都登記在那裡，只寫 `command` 和 `args`，並在 `~/.gemini/antigravity-cli/settings.json` 加上 `mcp(<伺服器>/*)` 規則和它 schema 目錄的讀取規則；少了這兩條，agy 的 print 模式會拒絕呼叫。真正做決定的是 proxy，和 Codex 的 `default_tools_approval_mode = "auto"` 相同。

**spawn 範圍的 proxy（Grok 與 Antigravity）。** 這兩個檔案都長期存在，同一個目錄的每次 spawn 共用，兩個 CLI 也都沒有單次執行的 MCP 覆寫，所以檔案裡不放任何單次 spawn 的東西，也不放上游憑證。每個伺服器登記成 `duduclaw mcp-proxy --spawn-scoped --server <名稱> -- …`。spawn 會把上游伺服器的環境變數寫進 `<home>/tmp/mcp-proxy-env/` 底下一個新的、只有擁有者能進的目錄，並用 `DUDUCLAW_MCP_PROXY_ENV_DIR` 指向它：agy 會把自己的環境交給它啟動的 MCP 子行程，Grok 則在載入時展開項目 `env` 裡的 `${DUDUCLAW_MCP_PROXY_ENV_DIR:-}`。CLI 結束時目錄就刪掉，gateway 啟動時也會清掉超過 15 分鐘的殘留。spawn 範圍的 proxy 找不到目錄、或目錄裡沒有自己伺服器的檔案時，什麼都不啟動。所以這兩個 runtime 上的輔助呼叫拿不到員工的伺服器，管理者在該目錄手動執行 `grok` 或 `agy` 也拿不到。同一位員工同時跑兩次 spawn 時，寫進檔案的內容相同，各自有自己的目錄。Grok 的項目還放了 proxy 自己的身分（agent id、home，以及同一個檔案裡 `duduclaw` 項目本來就有的 MCP 憑證），不放 agent token。上游伺服器從 agent CLI 的允許清單環境加上它宣告的環境變數起跑，CLI 自己的廠商金鑰（`XAI_API_KEY`、`GEMINI_API_KEY`）不會給它。

已知限制：agy 的權限規則加在作業系統使用者的設定檔裡，之後不會移除（管理者手動使用的工作區若出現同名伺服器，也會被自動放行）；`.grok/config.toml` 或 `.agents/mcp_config.json` 裡任何不是 DuDuClaw 寫的項目（`duduclaw` 除外）都照原樣保留，每次 spawn 記一次警告，CLI 會不經 proxy 啟動它；借用的工作根目錄裡，兩位員工若有同名伺服器會共用同一個項目，實際執行的那次 spawn 用自己的環境檔；spawn 讀完之後、Grok 自己讀之前才改動的 `.mcp.json`（或 `.cursor/mcp.json`）不在保護範圍內；兩個只差大小寫的伺服器名稱會拒絕 spawn（在不分大小寫的磁碟上會共用同一個環境檔）；Grok 使用者層級的 `~/.grok/config.toml` 在員工目錄之外，不會被覆蓋。Grok 的 Claude／Cursor 相容 MCP 讀取（`~/.claude.json`、`~/.cursor/mcp.json`、`<cwd>/.cursor/mcp.json`）在 DuDuClaw 啟動的每次 spawn 都以 `GROK_CLAUDE_MCPS_ENABLED=false`、`GROK_CURSOR_MCPS_ENABLED=false` 關閉（2026-10-20 owner 拍板；`grok inspect` 會把這些伺服器標成 `[disabled]`）。Grok 的 Claude／Cursor 相容 hook 讀取也用同樣方式關閉（`GROK_CLAUDE_HOOKS_ENABLED=false`、`GROK_CURSOR_HOOKS_ENABLED=false`），作業系統使用者 `~/.claude/settings.json` 與 `~/.cursor/hooks.json` 的 hook 不會在員工的回合裡執行。這樣 Grok 也不再讀員工的 `.claude/settings.json`，所以 DuDuClaw 的 `agent-file-guard` 與 `data-file-guard` 在每次 spawn 前寫成 Grok 的專案 hook 檔 `<cwd>/.grok/hooks/duduclaw-guards-<員工>.json`，比對規則與指令相同；寫不進去的 spawn 會被拒絕。管理者另外加在員工 `.claude/settings.json` 的 hook，Grok 員工不再執行；Grok 自己的全域 hook（`~/.grok/hooks/`）仍會執行。Claude／Cursor 相容的 rules、skills、agents 讀取也一併關閉（`GROK_CLAUDE_RULES_ENABLED`、`GROK_CLAUDE_SKILLS_ENABLED`、`GROK_CLAUDE_AGENTS_ENABLED` 與對應的三個 `GROK_CURSOR_*`，都設成 `false`），作業系統使用者的 `~/.claude/CLAUDE.md`、`~/.claude/rules/`、`~/.claude/skills/`、`~/.cursor/skills/` 不會進到員工的回合。工作目錄自己的 `AGENTS.md` 與 `CLAUDE.md` 是 Grok 原生讀取，照常載入（員工目錄上方沒有 git repository 時，Grok 不讀上層目錄的 `AGENTS.md` 或 `CLAUDE.md`）；管理者放在員工 `.claude/rules/` 或 `.claude/skills/` 的 rule 與 skill，Grok 員工不再載入（DuDuClaw 自己的 skill 放在 `SKILLS/`，不受影響）。Grok 1.0.62 的已知限制：agents 開關關掉後，`grok inspect` 仍列出 `~/.claude/agents/` 的子代理定義；Cursor rules 開關不論開或關，`grok inspect` 都沒有列出任何 `.cursor/rules/` 檔，所以這個開關有設但效果未驗證；Grok 原生的使用者層級來源（`~/.grok/`、`~/.agents/skills/`）不屬於相容開關，仍會載入。

**openai-compat、Direct API 與本地推論。** 這三條路徑共用一個工具迴圈的建構函式（`claude_runner::build_mcp_tool_registry`），之前只掛載 DuDuClaw 的伺服器和 `agent.toml [[mcp.external]]`。現在也會掛載 `.mcp.json` 的伺服器，每個都照管制改寫的結果啟動，而且從 agent CLI 的允許清單環境起跑，不繼承 gateway 的環境，所以 gateway 環境裡其他廠商的金鑰不會流到第三方伺服器。這條路徑不寫任何檔案。唯讀通道裡它們照樣掛載，但帶 `DUDUCLAW_LANE=explore`，只有唯讀工具能通過 proxy，和 Claude 在唯讀通道的 spawn 相同；`agent.toml` 的外部伺服器在唯讀通道仍然不掛載，因為沒有東西管制它們。兩個檔案都宣告的同一台伺服器（同名，或指令與參數相同）只掛載一次，用 `agent.toml` 那一份，它帶著工具允許清單和 `secret://` 參照。

**失敗時拒絕。** 有第三方伺服器時，`.mcp.json` 讀不到或解析失敗、或 spawn 閘門拒絕，這次執行就以 spawn 閘門錯誤停止，容錯切換也會停。openai-compat runtime 拒絕這次回覆；Direct API 與本地推論的工具迴圈改成不帶工具回答。Grok 的設定寫不進去時也會拒絕 spawn（少了停用項目，Grok 就會自己啟動沒有管制的伺服器）；`.mcp.json` 格式壞掉時，Grok 的輔助呼叫現在也會被拒絕，因為 Grok 自己會去讀那個檔。Antigravity 有第三方伺服器而登記失敗時拒絕 spawn。沒有任何第三方伺服器時，這些 spawn 都和以前一樣。Grok 和 Antigravity 在這些步驟之前就會拒絕唯讀通道。

**尚未驗證。** 真正的 `grok -p` 對話（需要已登入的 Grok；設定是在暫時的 `HOME` 下用 `grok mcp doctor` 與 `grok inspect` 檢查的）；任何一個 runtime 的真實模型實際呼叫第三方工具；需要核准的第三方工具能否在 CLI 的時限內等到核准。

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

### Antigravity 的工具權限（v1.69.1）

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

擁有者 2026-10-16 決定：訂閱登入相關功能維持原樣（setup-token 擷取、OAuth 帳號、自動沿用主機登入、帳號輪替、CLI 一鍵登入），改的是風險告知。

**告知內容。** Anthropic 的[法律與合規說明](https://code.claude.com/docs/en/legal-and-compliance)寫明：第三方開發者不得在自己的產品裡提供 Claude.ai 登入，也不得用 Free、Pro、Max 方案的憑證替使用者轉送請求，更不得收集、儲存或中介 Claude.ai 的憑證或 session token。允許的是你本人用自己的訂閱登入未經修改的 Claude Code，Pro 和 Max 的用量上限以一般使用為前提。消費者條款另外規定帳號不得提供給其他人使用。Anthropic 可以不事先通知就執行；帳號停權是媒體與使用者的回報，非 Anthropic 官方聲明。告知會直接點出 DuDuClaw 裡和規範衝突的用法：一份訂閱替其他人服務（頻道上的客戶）、輪替多個訂閱帳號避開上限、讓 DuDuClaw 保存 token。告知不說 token「被封鎖」，因為 OAuth token 跑 `claude -p` 在技術上仍可用（v1.66.0）。

**依廠商顯示。** 上面的完整告知用在 Claude 登入與 Claude 帳號。Codex、Antigravity、Grok 的登入（以及新增帳號選了非 Anthropic provider 的 OAuth 路徑）改顯示該廠商自己的說明與條款連結，勾選框相同：Google 的 Antigravity 條文如上；OpenAI 顯示上面整理的 Sign in with ChatGPT 計畫（開源與經核准的商業合作夥伴、token 留在本機）並連到計畫條款；xAI 則寫明立場不明、條款仍適用。不認得的 provider 顯示不指名廠商、沒有連結的提醒。每一句都對應到廠商頁面；舊的「Google 自 2026-03 起在伺服器端封鎖」找不到原始出處，已移除。廠商對照表在 `duduclaw_core::subscription_risk::vendor_policy`（gateway、CLI）與 `web/src/lib/subscription-risk.ts`（dashboard）。

**出現在哪裡。** CLI 登入彈窗、引導式 QR code 設定精靈、新增帳號的 OAuth Token 路徑，在任何憑證步驟之前都會先顯示完整告知與勾選框。歡迎頁的 Claude 訂閱卡片（以及「已登入」卡片，也就是自動沿用主機登入的情況）、runtime 設定卡片的登入列、帳號頁的訂閱主卡片、CLI 一鍵登入選單、CLI 憑證清單、員工編輯頁 API 模式選「訂閱帳號（CLI）」時，以及帳號池選了訂閱帳號時，都會顯示一行提醒和規範連結。

**伺服器端關卡。** `auth.cli_login.start`（所有有腳本化登入的 runtime）、`accounts.setup_token_start`、`type = "oauth"` 的 `accounts.add`，以及送出的 `config.toml` 新增了儲存檔裡沒有的 `type = "oauth"` 帳號（新的 id，或既有 id 改成 `oauth`）的 `config.raw.set`，請求沒有帶 `accept_subscription_risk: true` 就拒絕。這對自己寫的 WebSocket 用戶端是破壞性變更，隨 v1.74.0 發布（見[已棄用名稱](../../guides/zh-TW/deprecations.md)）。拒絕時回傳錯誤碼 `subscription_risk_not_accepted` 的結構化錯誤，附上 `provider` 與對應的 `policy_url`，dashboard 會以使用者的語言顯示；被拒的進階編輯不會寫檔，編輯器會就地顯示該廠商的說明，勾選後再儲存一次。每次同意都會在 `security_audit.jsonl` 寫入一筆 `subscription_risk_accepted`，記錄時間、方法、provider、帳號 id（知道的話）、操作的 dashboard 使用者、廠商的 `policy_url` 與 `outcome = "acknowledged"`；`note` 寫明這筆代表「已同意」，不代表登入或儲存成功。一次進階編輯新增多個 oauth 帳號時，每個各寫一筆。`auth.cli_login.finalize` 與 `accounts.setup_token_submit` 內部重用 `accounts.add` 時不會再問一次，因為它們收尾的流程已由同意過的 `start` 開啟。API 金鑰帳號不受關卡影響。

**終端機與日誌。** `duduclaw onboard` 在「偵測到 Claude … 登入」旁邊印出風險提醒。帳號輪替器自動沿用主機的 Claude 登入時（`[account_loading] inherit_host_credentials`，預設仍是開），gateway 每個行程記一次 WARN，內容相同。只要 `config.toml` 設定了 Claude 的 OAuth 帳號（provider 為 `anthropic` 或未設定），`duduclaw doctor` 與 dashboard 健康檢查都會多一列「訂閱帳號政策風險」；Copilot 座位等其他廠商的 oauth 帳號不算在內，因為這一列引用的是 Anthropic 的規範。把 GitHub Copilot 座位存進設定的操作者指令 `duduclaw auth device --provider copilot`，會先印出附 GitHub 條款連結的說明，沒有 `--accept-subscription-risk` 就停止；加上旗標時寫一筆同樣的稽核紀錄（操作者記為 `operator-cli`）。

---

## 總結

AI 領域是多供應商的。只基於單一 CLI 構建就像只為單一作業系統寫軟體：能用，直到不能用。`AgentRuntime` trait 抽象化了差異，讓 DuDuClaw 將 Claude、Codex、Antigravity 和任何 OpenAI 相容端點視為可互換的後端。你的 Agent 每次都能取得最佳可用的大腦。
