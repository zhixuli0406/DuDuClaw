# Agent Client Protocol (ACP/A2A)

> 讓 IDE 與 Agent 對話的方式，就像 Agent 與其工具對話一樣——stdio JSON-RPC 2.0、可探索、與語言無關。

---

## 比喻：餐廳的訂位專線

每家餐廳都有兩個入口：

- **用餐區**——顧客走進來、瀏覽菜單、與服務生互動的地方。
- **訂位專線**——一套電話協定。電話另一端的訂位系統不需要知道用餐區長什麼樣子；它只需要問「晚上 7 點有四人桌嗎？」並得到一個是／否。

像 Zed、JetBrains、Neovim 這樣的 IDE 想問 Agent「你能接這個任務嗎？」，而不必理解 DuDuClaw 的整套頻道基礎設施。它們需要的是訂位專線——一個乾淨、穩定、協定驅動的介面。

那正是 ACP server。

---

## 什麼是 ACP/A2A？

**ACP** 代表 Agent Client Protocol——一套用於 IDE ↔ Agent 通訊的 stdio JSON-RPC 2.0 行協定。**A2A** 是相關的「Agent to Agent」協定，用於 Agent 探索與任務交換。

兩者合在一起，讓 IDE（或另一個 Agent、CI pipeline、shell 腳本）能把一個正在執行的 DuDuClaw Agent 當作一等公民服務來對待——可以探索它、發送任務、輪詢狀態、取消任務。

DuDuClaw 提供的是 server 端：

```
duduclaw acp-server
     |
     v
Listens on stdin, writes to stdout (line-delimited JSON-RPC 2.0)
     |
     v
Responds to:
  agent/discover   → return AgentCard
  tasks/send       → queue new task
  tasks/get        → poll task status
  tasks/cancel     → cancel running task
```

在 v1.8.9 之前，`duduclaw acp-server` 只是一個佔位符，印出一段訊息就返回。v1.8.9 將它接上了真正的 `A2ATaskManager`，使其得以運作。

---

## Agent Card

每個 ACP server 都能描述自己。當客戶端連線時，它可以發出 `agent/discover` 並收到一份 **Agent Card**——一份包含身分、能力與技能的 JSON 文件：

```json
{
  "name": "duduclaw-pm",
  "description": "Project manager for DuDuClaw v1.9 roadmap",
  "url": "stdio://duduclaw acp-server --agent duduclaw-pm",
  "version": "1.8.14",
  "capabilities": {
    "streaming": true,
    "multi_turn": true,
    "tool_use": true
  },
  "skills": [
    {
      "name": "task_planning",
      "description": "Break down features into TaskSpec workflows",
      "tags": ["planning", "orchestration"]
    },
    {
      "name": "sprint_review",
      "description": "Summarize sprint outcomes from memory + tasks",
      "tags": ["reporting", "retrospective"]
    }
  ]
}
```

這與 A2A 的 `.well-known/agent.json` 探索端點是同一份文件格式。IDE 可以快取它、在 UI 中顯示可用技能，並決定是否要把請求路由到這個特定的 Agent。

### `.well-known` 產生

對於透過 HTTP 暴露的 Agent（未來的擴充），DuDuClaw 可以產生一份 `.well-known/agent.json` 檔案，讓外部客戶端不必先連線就能探索該 Agent：

```
/etc/duduclaw/agents/dudu/.well-known/agent.json
     ↓
https://your-host.example.com/.well-known/agent.json
```

任何相容於 A2A 的客戶端都能讀取它、得知該 Agent 的能力，並決定是否要繼續。

---

## JSON-RPC 迴圈

stdio server 是一個簡單的、以行為界的 JSON-RPC 2.0 迴圈：

```
loop {
    read line from stdin
    parse JSON-RPC 2.0 request
    dispatch to handler:
        agent/discover  → return AgentCard
        tasks/send      → TaskManager.send(params)
        tasks/get       → TaskManager.get(id)
        tasks/cancel    → TaskManager.cancel(id)
    write JSON-RPC 2.0 response to stdout
}
```

stdio 上的 JSON-RPC 與 MCP 使用的傳輸方式相同——如果你已經建好了一個 MCP server，那你已經建好了一個 ACP server 的 95%。

### 範例工作階段

```
→ {"jsonrpc":"2.0","id":1,"method":"agent/discover"}
← {"jsonrpc":"2.0","id":1,"result":{
     "name":"duduclaw-pm",
     "version":"1.8.14",
     "capabilities":{"streaming":true,"multi_turn":true,"tool_use":true},
     "skills":[...]
   }}

→ {"jsonrpc":"2.0","id":2,"method":"tasks/send","params":{
     "task":"Draft the v1.9 release notes from the last 50 commits",
     "priority":"high"
   }}
← {"jsonrpc":"2.0","id":2,"result":{
     "task_id":"t_abc123",
     "status":"queued"
   }}

→ {"jsonrpc":"2.0","id":3,"method":"tasks/get","params":{"task_id":"t_abc123"}}
← {"jsonrpc":"2.0","id":3,"result":{
     "task_id":"t_abc123",
     "status":"completed",
     "output":"# DuDuClaw v1.9 Release Notes\n\n..."
   }}
```

---

## A2ATaskManager

在 `tasks/send`、`tasks/get` 與 `tasks/cancel` 背後，是 `A2ATaskManager`。它的職責是：

1. **排入佇列**——將傳入的任務排入 Agent 既有的任務系統（`TaskSpec`、`tasks/` 目錄）。
2. **追蹤**狀態轉換（queued → running → completed/failed/cancelled）。
3. **路由**任務執行至 Agent 的正常 runtime（Claude / Codex / Gemini / OpenAI-compat）。
4. **暴露**任務封套中的結果，讓客戶端能輪詢取得。

這代表透過 ACP 提交的任務，會流經與透過頻道或 MCP 工具提交的任務**相同**的管線——單一事實來源，在 Logs/Activity dashboard 中統一可觀測。

---

## 為什麼 IDE 整合很重要

### Zed

[Zed](https://zed.dev) 提供一個「agent panel」，能與任何相容於 ACP 的 Agent 對話。把它指向 `duduclaw acp-server --agent <your-agent>`，Zed 就能原生存取：
- 任務路由（透過 `tasks/send`）
- 編輯器內的行內回應
- IDE 內的多輪後續追問

### JetBrains

IntelliJ 平台的 AI Assistant 可以透過外掛擴充以說 ACP。一旦連線，Agent 就能瀏覽專案、提出流經 dispatcher 的重構建議，並透過 worktree 隔離層落地提交。

### Neovim

`nvim-acp` 外掛直接使用 stdio 行協定——`duduclaw acp-server` 是一個即插即用的後端。你不必離開編輯器就能取得命令列驅動的 Agent 存取。

### CI/CD Pipeline

一個 pipeline 步驟可以透過 ACP 發送任務並輪詢直到完成：

```yaml
- name: Generate release notes via DuDuClaw
  run: |
    echo '{"jsonrpc":"2.0","id":1,"method":"tasks/send","params":{"task":"..."}}' \
      | duduclaw acp-server --agent duduclaw-pm
```

不需要 HTTP server、不需要 auth token、不需要管理連接埠——只需容器內的 stdio。

---

## DuDuClaw 所說的三種 Stdio 協定

值得梳理一下，因為命名有所重疊：

| 協定 | 用途 | 方向 | 指令 |
|----------|---------|-----------|---------|
| **MCP** | 將 DuDuClaw 的工具（channel、memory、agent、wiki、task……）暴露給 AI runtime | Runtime → DuDuClaw | `duduclaw mcp-server` |
| **ACP/A2A** | 讓外部客戶端（IDE、pipeline、其他 Agent）向 DuDuClaw 發送任務 | IDE → DuDuClaw | `duduclaw acp-server` |
| **Runtime stdio** | DuDuClaw 衍生一個 runtime（Claude/Codex/Gemini）子行程並透過 stdio JSON 與它對話 | DuDuClaw → Runtime | *內部* |

它們是三場各自獨立的對話，全都在 stdio 上，全都與 JSON-RPC 相鄰。同一個 Agent 在執行期同時參與這三者。

---

## 為什麼是 Stdio，而非 HTTP？

對 IDE 整合而言，stdio 有幾個實務上的優勢：

- **零設定**——不必挑連接埠、不必 TLS 憑證、不必防火牆規則。
- **行程範圍**——ACP server 與 IDE 工作階段同生共死。不會有孤兒監聽器。
- **OS 層級驗證**——如果你能衍生這個行程，你就已經擁有所需的權限。不需要 API key。
- **與傳輸無關**——同一套行協定可以透過 SSH 隧道傳輸、在容器內、或跨 VS Code remote 工作階段。

HTTP 對 Dashboard 與 Prometheus metrics 仍然可用，但對 IDE ↔ Agent 而言，stdio 更簡單也更安全。

---

## 串流與多輪

Agent Card 宣告 `streaming: true` 與 `multi_turn: true`。這向客戶端傳達：

- **串流**：長時間執行的任務可以在同一條 stdio 連線上發出進度事件，而不只是單一回應。
- **多輪**：一個任務情境可以橫跨多個請求／回應對（釐清、後續追問）而不喪失狀態。

這些能力對映 Session Memory Stack——釘選指令、滾雪球式回顧與關鍵事實，都會橫跨多輪 ACP 對話延續，方式與頻道訊息中相同。

---

## 安全考量

ACP 與 MCP 一樣，繼承 DuDuClaw 的安全邊界：

- **CONTRACT.toml**——must_not/must_always 規則依然適用；經由 ACP 提交的任務無法違反它們。
- **能力閘控**——`agent.toml [capabilities]` 的預設拒絕仍然閘控工具存取。
- **稽核日誌**——經由 ACP 提交的任務會以 source=`acp` 出現在 `audit.unified_log` 中。
- **沙箱化**——任務仍然會流經 worktree 層，並（可選地）流經容器沙箱。

客戶端是 IDE 並不會授予較高的信任——Agent 自身的策略才是最後一道防線。

---

## 與其他系統的互動

- **Task Board**：經由 ACP 提交的任務流經與經由頻道提交者相同的 `TaskStore`。兩者都會顯示在 Dashboard Activity Feed 中。
- **Runtime 選擇**：Agent 的正常 runtime（Claude/Codex/Gemini/OpenAI）處理 ACP 任務——相同的工作階段記憶、相同的 prompt cache 策略、相同的帳號輪替。
- **演化**：ACP 任務在關鍵事實萃取與預測錯誤校準上，計為「實質性輪次」。
- **稽核日誌**：所有 ACP 請求都以 source=`acp` 記錄，與其他四個稽核來源（security / tool_calls / channel_failures / feedback）並列。

---

## 為什麼這很重要

### 以標準為基礎的整合

ACP 是一個有真實客戶端（Zed、nvim-acp、實驗性 JetBrains 外掛）的真實協定。支援它讓 DuDuClaw 進入一個成長中的生態系，而不必為每個 IDE 製作客製整合。

### 同一個 Agent，新的介面

沒有新 Agent、沒有新設定、沒有新的 runtime 邊界。既有的 Agent（SOUL.md、CONTRACT.toml、記憶、技能、wiki）只是從一個新的進入點變得可達。所有對 Agent 行為的投資都得以延續。

### 加速開發者迴圈

開發者不必在聊天 app 中問 Agent 問題、再把回應複製貼上到編輯器，而能直接從工作之處呼叫 Agent。摩擦降至近乎為零，而 Agent 的回應就*落在情境中*。

### 可組合的編排

一個 Agent 可以是另一個 Agent 的 A2A 客戶端。編排者風格的 Agent 可以透過 `agent/discover` 探索子 Agent、檢查它們的技能標籤，並透過 `tasks/send` 路由任務——對於跨行程情境，這是 DuDuClaw 內部以檔案為基礎的 IPC 的一個結構化、標準的替代方案。

---

## 總結

一個好的 Agent 應該能從工作發生的任何地方被觸及。用餐區（頻道）是給終端使用者的；訂位專線（ACP/A2A）是給那些需要以程式化方式與它協作的 IDE、pipeline 與對等 Agent 的。同一個 Agent、同一顆大腦、同一套契約——只是前門換上了一個更乾淨的協定。
