# Multi-Runtime Agent 執行

> 一個平台，四種 AI 後端——Claude、Codex、Gemini，以及任何 OpenAI 相容端點。

---

## 比喻：多語辦公室

想像一間需要翻譯人員的辦公室。與其聘請一位只會法語的翻譯，不如建立一個翻譯台——可以將工作分配給法語、德語、日語翻譯，或任何會說客戶語言的自由譯者。

翻譯台不在乎*哪位*翻譯處理工作——它在乎的是翻譯品質。如果法語翻譯正忙，就轉給下一位。

DuDuClaw 的 Multi-Runtime 架構就是那個翻譯台——但對象是 AI 後端。

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

每個後端——Claude、Codex、Gemini 或任何 OpenAI 相容端點——都實作相同的介面。系統的其餘部分不需要知道、也不在乎是哪個後端在處理特定請求。

### 四種後端

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
啟動掃描：
     |
     v
  PATH 中有 `claude`？ → 註冊 Claude runtime
  PATH 中有 `codex`？  → 註冊 Codex runtime
  PATH 中有 `gemini`？ → 註冊 Gemini runtime
  有設定的 HTTP 端點？  → 註冊 OpenAI-compat runtimes
     |
     v
Registry 知道哪些後端可用
```

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

容錯對使用者透明——無論哪個後端處理，使用者都能看到回應。每個後端的健康狀態獨立追蹤：

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
- **Confidence Router**：位於 runtime 層之下——決定本地 vs. 雲端。Runtime 層決定*哪個*雲端。
- **CostTelemetry**：追蹤每個供應商的成本，支援明智的路由決策。
- **MCP Server**：工具暴露給所有支援的後端（Claude 透過原生 MCP，其他透過工具注入）。
- **Agent Config**：每個 Agent 的 `agent.toml` 指定其 runtime 偏好和備案鏈。

---

## 總結

AI 領域是多供應商的。只基於單一 CLI 構建就像只為單一作業系統寫軟體——能用，直到不能用。`AgentRuntime` trait 抽象化了差異，讓 DuDuClaw 將 Claude、Codex、Gemini 和任何 OpenAI 相容端點視為可互換的後端。你的 Agent 每次都能取得最佳可用的大腦。
