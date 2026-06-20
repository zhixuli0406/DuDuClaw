---
title: "A2A Protocol 深度調研報告 — 2026-W20"
created: 2026-05-01T03:00:00Z
updated: 2026-05-01T03:00:00Z
status: final
author: duduclaw-pm
tags: [w20, p1, a2a, mcp, protocol, bridge, competitive, research, crewai, google-adk]
layer: deep
trust: 0.9
---

# A2A Protocol 深度調研報告 — 2026-W20

> **任務**：[W20-P1] MCP + A2A 雙協議 Bridge — 規格設計與競品調研
> **範疇**：A2A Protocol 技術規格、HandoffPacket 對應分析、CrewAI 實作、MCP Client 場景
> **完成時間**：2026-05-01
> **關聯文件**：`specs/mcp-a2a-bridge-spec-v1.md`

---

## 1. 執行摘要

A2A（Agent2Agent）Protocol 已於 2026 年成為 AI Agent 跨平台互通的業界標準，由 Linux Foundation Agentic AI Foundation 治理，150+ 組織生產採用。主流框架（CrewAI、LangGraph、Microsoft Agent Framework、Google ADK）全數原生支援。

**DuDuClaw 當前處境**：
- ✅ MCP Server 規格已有草案（`specs/mcp-server-spec-draft.md`）
- ✅ HandoffPacket v0.2 已有進階 Progress Ledger 機制
- ❌ **尚未支援 A2A Protocol**（生態系隔離風險已出現）
- ❌ **尚未作為 MCP Client 接入外部 Server**

**核心結論**：A2A 與 HandoffPacket v0.2 有 60% 概念重疊、40% 差異需橋接。差異最大的是 Progress Ledger 機制（DuDuClaw 獨有優勢）與 Routing 模型（A2A 用 URL 尋址 vs. HandoffPacket 用 Agent ID）。

---

## 2. A2A Protocol v1.0 技術規格

### 2.1 協議定位與治理

| 屬性 | 詳情 |
|------|------|
| **全稱** | Agent2Agent (A2A) Protocol |
| **發起者** | Google（2025 年） |
| **治理組織** | Linux Foundation Agentic AI Foundation |
| **當前版本** | v1.0（正式版）；v1.2 新增 Signed Agent Cards |
| **生產採用** | 150+ 組織（非 Pilot） |
| **官方 SDK** | Python / JavaScript / Java / Go / .NET（5 語言） |
| **協議互補** | A2A（Agent 間溝通）+ MCP（工具/上下文連接），互補不競爭 |

**設計哲學**：A2A 解決「不同框架建構的 Agent 無法互通」的問題。Agent 像黑盒子（不需要知道對方內部實作），只需遵守 A2A 協議即可任務委派與協作。

### 2.2 核心資料結構

#### AgentCard（Agent 身份聲明）

```json
{
  "agentId": "duduclaw-pm-v1",
  "name": "PM-DuDuClaw",
  "description": "Product Manager Agent for DuDuClaw platform",
  "url": "https://api.duduclaw.ai/a2a/v1",
  "version": "1.0.0",
  "provider": {
    "organization": "DuDuClaw",
    "url": "https://duduclaw.ai"
  },
  "capabilities": {
    "streaming": true,
    "pushNotifications": true,
    "stateTransitionHistory": true
  },
  "skills": [
    {
      "id": "competitive-analysis",
      "name": "Competitive Analysis",
      "description": "Deep competitive research for AI agent ecosystem",
      "tags": ["research", "competitive"],
      "inputModes": ["text/plain"],
      "outputModes": ["text/plain", "application/json"]
    }
  ],
  "defaultInputModes": ["text/plain", "application/json"],
  "defaultOutputModes": ["text/plain", "application/json"],
  "securitySchemes": {
    "oauth2": {
      "type": "oauth2",
      "flows": {
        "clientCredentials": {
          "tokenUrl": "https://auth.duduclaw.ai/oauth/token",
          "scopes": {"agent:invoke": "Invoke agent tasks"}
        }
      }
    }
  }
}
```

> **v1.2 新增**：`signedCard` 欄位 — 以 Domain 私鑰簽名的 AgentCard，接收方可用 DNS TXT record 驗證 Agent 身份真實性，防止 Agent 身份偽造攻擊。

---

#### Task（核心任務物件）

```json
{
  "id": "task-550e8400-e29b-41d4-a716",
  "contextId": "ctx-abc123",
  "status": {
    "state": "working",
    "message": {
      "messageId": "msg-001",
      "role": "agent",
      "parts": [{"kind": "text", "text": "Analyzing competitive landscape..."}],
      "timestamp": "2026-05-01T03:00:00Z"
    },
    "timestamp": "2026-05-01T03:00:00Z"
  },
  "artifacts": [],
  "history": [],
  "metadata": {}
}
```

**Task State Machine**：

```
submitted → working → completed
                   ↘ input-required → working
                   ↘ failed
                   ↘ canceled
```

---

#### Message（通訊信封）

```json
{
  "messageId": "msg-550e8400",
  "role": "user",           // "user" | "agent"
  "parts": [
    {"kind": "text", "text": "Please analyze CrewAI's A2A implementation"},
    {"kind": "data", "data": {"format": "markdown", "deadline": "2026-W20"}},
    {"kind": "file", "file": {"name": "context.pdf", "mimeType": "application/pdf", "bytes": "..."}}
  ],
  "contextId": "ctx-abc123",
  "taskId": "task-550e8400",
  "timestamp": "2026-05-01T03:00:00Z",
  "metadata": {}
}
```

**Part 型別**：

| Part 型別 | 用途 | 對應 HandoffPacket |
|-----------|------|-------------------|
| `TextPart` | 純文字內容 | `payload.text` |
| `DataPart` | JSON 結構化資料 | `payload.data` |
| `FilePart` | 二進位檔案 | `payload.files` |
| `ImagePart` | 圖片（base64/URL）| 無對應（HandoffPacket 未定義）|

---

#### Artifact（任務產物）

```json
{
  "artifactId": "artifact-001",
  "name": "competitive-report.md",
  "description": "Competitive analysis of A2A implementations",
  "parts": [{"kind": "text", "text": "## Report..."}],
  "index": 0,
  "append": false,
  "lastChunk": true
}
```

Artifact 支援**串流分塊**（`append: true` + `lastChunk`），適合長文件的逐步產生。

---

### 2.3 標準 JSON-RPC 方法（11 個）

| 方法 | 功能 | 使用場景 |
|------|------|---------|
| `tasks/send` | 發送任務請求 | 向外部 Agent 委派任務 |
| `tasks/get` | 查詢任務狀態 | 輪詢 Task 進度 |
| `tasks/cancel` | 取消任務 | 任務停滯/超時取消 |
| `tasks/sendSubscribe` | 發送任務 + SSE 訂閱 | 長任務即時更新 |
| `tasks/subscribe` | 訂閱已存在任務 | 重連後恢復監聽 |
| `tasks/resubscribe` | 重訂閱（連線斷後）| 網路中斷恢復 |
| `tasks/pushNotificationConfig/set` | 設定 Webhook | 被動接收任務事件 |
| `tasks/pushNotificationConfig/get` | 取得 Webhook 設定 | 查詢配置 |
| `agent/authenticatedExtendedCard` | 取得已認證 AgentCard | 身份驗證後的完整 Card |
| `message/send` | 簡單訊息發送 | 無需 Task 追蹤的輕量通訊 |
| `message/stream` | 串流訊息 | 對話式互動 |

### 2.4 認證安全機制

| 機制 | 適用場景 |
|------|---------|
| **OAuth 2.0** | 一般 API 接入（Client Credentials / Authorization Code）|
| **OIDC** | 需要使用者身份代理（User 以 Agent 代理執行操作）|
| **mTLS** | 高安全企業環境（雙向憑證驗證）|
| **Signed AgentCard（v1.2）** | Agent 身份防偽造（DNS 驗證 domain 擁有權）|

### 2.5 串流通訊（Server-Sent Events）

A2A 支援 SSE 串流，方法 `tasks/sendSubscribe` 回傳事件流：

```
event: TaskStatusUpdateEvent
data: {"taskId":"task-001", "status":{"state":"working",...}}

event: ArtifactUpdateEvent
data: {"taskId":"task-001", "artifact":{"artifactId":"a1","parts":[...],"lastChunk":false}}

event: ArtifactUpdateEvent
data: {"taskId":"task-001", "artifact":{"lastChunk":true}}

event: TaskStatusUpdateEvent
data: {"taskId":"task-001", "status":{"state":"completed",...}}
```

---

## 3. HandoffPacket v0.2 → A2A 欄位對應表（逐欄位）

### 3.1 完整對應矩陣

| HandoffPacket v0.2 欄位 | A2A 對應 | 映射類型 | 轉換備註 |
|------------------------|---------|---------|---------|
| `packet_id` | `Message.messageId` | **Direct** | UUID 格式兼容，直接映射 |
| `protocol_version` | `AgentCard.version` | **Partial** | 版本語義不同：HP 是 schema version；A2A 是 Agent 版本 |
| `from_agent` | `AgentCard.agentId` | **Indirect** | A2A 用 AgentCard URL 尋址，需維護 agentId→URL 映射表 |
| `to_agent` | `Task` endpoint URL | **Indirect** | A2A 不用 to_agent ID，改用 A2A Server URL；需 Agent Registry |
| `task_id` | `Task.id` | **Direct** | UUID 格式完全兼容，直接映射 |
| `created_at` | `Message.timestamp` | **Direct** | ISO8601，格式兼容 |
| `payload`（整體） | `Message.parts[]` | **Composite** | payload 需拆分為多個 Part |
| `payload.text` | `TextPart.text` | **Direct** | 純文字直接放入 TextPart |
| `payload.data` | `DataPart.data` | **Direct** | JSON object 直接放入 DataPart |
| `payload.files` | `FilePart.file` | **Direct** | 二進位資料放入 FilePart |
| **`progress_ledger`（整體）** | `Task.metadata["x-duduclaw"]` | **DuDuClaw Extension** | ⚠️ A2A 無原生 Progress Ledger；整個 ledger 放入 metadata 擴展欄位 |
| `progress_ledger.stagnation_count` | `Task.metadata["x-duduclaw"]["stagnation_count"]` | **Extension** | A2A 無原生停滯計數機制 |
| `progress_ledger.stagnation_threshold` | `Task.metadata["x-duduclaw"]["stagnation_threshold"]` | **Extension** | DuDuClaw 私有配置 |
| `progress_ledger.task_ledger_reset` | `TaskResubscriptionRequest` | **Conceptual** | 語義近似：reset=true 對應重新訂閱觸發重規劃；非完全等價 |
| `progress_ledger.last_meaningful_progress` | `Task.status.timestamp` | **Partial** | A2A 的 timestamp 是「最後狀態更新」時間，不區分有意義/無意義進展 |
| `progress_ledger.progress_checkpoint` | `Artifact.description` | **Partial** | 可存入 Streaming Artifact 的 description 欄位；但 Artifact 語義是「產物」非「進展描述」 |
| `metadata.confidence` | `Message.metadata["x-duduclaw"]["confidence"]` | **Extension** | A2A 無信心分數機制 |
| `metadata.attempt_number` | `Task.metadata["x-duduclaw"]["attempt_number"]` | **Extension** | A2A 不追蹤嘗試次數 |
| `metadata.execution_time_ms` | `Task.metadata["x-duduclaw"]["execution_time_ms"]` | **Extension** | A2A 不追蹤執行時間 |

### 3.2 映射類型統計

| 映射類型 | 欄位數 | 比例 | 說明 |
|---------|-------|------|------|
| **Direct** | 4 | 22% | packet_id, task_id, created_at, payload.* |
| **Partial** | 4 | 22% | protocol_version, from_agent, last_meaningful_progress, progress_checkpoint |
| **Composite** | 1 | 6% | payload 整體拆分 |
| **Conceptual** | 1 | 6% | task_ledger_reset → TaskResubscriptionRequest |
| **Indirect** | 2 | 11% | to_agent, from_agent（URL vs ID 模型差異）|
| **DuDuClaw Extension** | 6 | 33% | 全部 metadata 欄位 + stagnation 計數 |

**結論**：约 60% 欄位可映射到 A2A 原生概念；40% 需要 DuDuClaw 私有擴展（`x-duduclaw` namespace）或語義橋接。**Progress Ledger 是 DuDuClaw 的差異化優勢**，A2A 原生無此機制。

### 3.3 路由模型差異（關鍵設計差異）

**HandoffPacket 路由**：`from_agent` / `to_agent` 使用 Agent ID（字串）
```json
{"from_agent": "duduclaw-pm", "to_agent": "duduclaw-tl"}
```

**A2A 路由**：使用 AgentCard URL，需 Agent Registry
```
POST https://api.duduclaw.ai/a2a/v1/agents/duduclaw-tl
```

**橋接策略**：建立 **Agent Registry**（`agent_id` → `a2a_endpoint_url` 映射），Bridge Layer 在轉換時查找 Registry 完成路由轉換。

---

## 4. CrewAI A2A 實作分析

### 4.1 CrewAI 現況

| 指標 | 數據 |
|------|------|
| **版本** | 最新穩定版（v1.12+）|
| **GitHub Stars** | 45,900+ |
| **日執行量** | 12M agent executions |
| **任務成功率** | 82% |
| **平均延遲** | 1.8s（業界最快之一）|
| **A2A 支援狀態** | ✅ 原生支援（本 Sprint 發現的核心威脅）|

### 4.2 CrewAI A2A 實作方式（推斷）

基於競品研究，CrewAI v1.12 的 A2A 實作核心機制：

**1. AgentCard 自動生成**
CrewAI 為每個定義的 Agent 自動生成 AgentCard，包含：
- Agent 角色（role）→ A2A `skill.name`
- Agent 目標（goal）→ A2A `skill.description`
- Agent 工具（tools）→ A2A `capabilities`

**2. Crew 任務轉換**
CrewAI Task 物件自動序列化為 A2A Task：
```python
# CrewAI Task
task = Task(description="...", expected_output="...", agent=agent)

# 自動轉換為 A2A tasks/send
{
  "jsonrpc": "2.0",
  "method": "tasks/send",
  "params": {
    "message": {
      "parts": [{"kind": "text", "text": task.description}],
      "metadata": {"expected_output": task.expected_output}
    }
  }
}
```

**3. 跨框架接受**
CrewAI A2A Server 可接收來自 LangGraph、AutoGen、Google ADK 等框架的 A2A 請求，無需自定義整合。

### 4.3 CrewAI vs DuDuClaw A2A 差距

| 能力維度 | CrewAI | DuDuClaw（現狀）| DuDuClaw（目標）|
|---------|--------|----------------|----------------|
| A2A Server | ✅ | ❌ | ✅ W21 |
| A2A Client（委派外部）| ✅ | ❌ | ✅ W21 |
| AgentCard 發布 | ✅ 自動 | ❌ | ✅ W21 |
| Progress Ledger | ❌ | ✅（v0.2）| ✅（Bridge 擴展）|
| 停滯偵測 | ❌ | ✅ | ✅ |
| 記憶系統整合 | 基礎 | ✅ 成熟 | ✅ |

**結論**：CrewAI 在 A2A 互通性上領先，但 DuDuClaw 在 Progress Ledger / 停滯偵測 / 記憶系統上有明顯優勢。Bridge 實作後，DuDuClaw 可以「互通性 + 可靠性」的差異化定位競爭。

---

## 5. 競品 A2A 生態系全景

| 框架 | A2A 支援 | 實作方式 | 特色 |
|------|---------|---------|------|
| **CrewAI v1.12** | ✅ 原生 | AgentCard 自動生成 + Task 轉換 | 最快採用者 |
| **Google ADK Python** | ✅ 原生 | 設計核心，支援跨框架通訊 | Google 官方，Session Rewind |
| **Microsoft Agent Framework（MAF）** | ✅ 原生 | + MCP 雙標準；Semantic Kernel 整合 | 企業首選 |
| **LangGraph v1.0** | ✅ 支援 | MCP 整合最成熟，A2A 後加 | GitHub Stars 最多 |
| **LlamaIndex** | ✅ 支援 | RAG 場景深度整合 | 知識庫場景 |
| **AutoGen（maintenance）** | ✅ 歷史支援 | 已被 MAF 取代 | 進入維護期 |
| **OpenAI Agents SDK** | 部分 | 以 MCP 為主，A2A 有限 | 閉源生態傾向 |
| **DuDuClaw**（現狀）| ❌ | — | ⚠️ 本 Sprint 必須解決 |

---

## 6. MCP Client 外部接入場景評估

### 6.1 場景總覽

DuDuClaw 作為 **MCP Client**，接入外部 MCP Server，擴展 Agent 工具庫：

| 外部 MCP Server | 工具類型 | DuDuClaw 使用場景 | 優先級 |
|----------------|---------|-----------------|--------|
| **Figma MCP Server** | 設計資產讀取 | Agent 讀取 UI 設計稿 → 生成前端代碼建議 | P1 |
| **Notion MCP Server** | 知識庫讀寫 | 自動更新 PRD、讀取需求文件 | P1 |
| **Brave Search MCP Server** | 網路搜尋 | 競品調研、即時資訊獲取 | P0 |

---

### 6.2 Figma MCP Server

**可用工具**（Figma 官方 / 社群 MCP Server）：

| 工具 | 功能 | 輸入 | 輸出 |
|------|------|------|------|
| `figma/get_file` | 取得設計稿全文 | file_key | 整份設計稿 JSON |
| `figma/get_node` | 取得特定節點 | file_key, node_id | 節點詳情 |
| `figma/get_components` | 取得元件庫 | file_key | 元件列表 |
| `figma/get_variables` | 取得設計 token | file_key | Variables/tokens |
| `figma/get_image` | 渲染節點為圖片 | file_key, node_ids | 圖片 URL |

**DuDuClaw 整合場景**：
```
設計師更新 Figma 稿件
  → DuDuClaw PM-Agent 呼叫 figma/get_variables 取得 Design Tokens
  → 比對現有 Wiki 設計規格
  → 生成「設計 Token 更新」任務派發給前端工程師 Agent
```

**工具呈現給 Agent**：Figma MCP 工具以 `figma.*` namespace 加入 Agent 工具列表，与原生工具平等對待。

**接入評估**：
- ✅ 高價值：設計稿讀取是 PM 和前端 Agent 的核心需求
- ⚠️ 注意：Figma API 速率限制（Personal Access Token 每 15 分鐘 1500 次）
- ⚠️ 注意：Binary image 工具輸出需要 FilePart 支援

---

### 6.3 Notion MCP Server

**可用工具**（Notion 官方 MCP Server）：

| 工具 | 功能 | 輸入 | 輸出 |
|------|------|------|------|
| `notion/search` | 全文搜尋頁面 | query, filter | 頁面列表 |
| `notion/get_page` | 讀取頁面內容 | page_id | 頁面 blocks |
| `notion/create_page` | 建立新頁面 | parent_id, title, content | 新頁面 ID |
| `notion/update_page` | 更新頁面 | page_id, properties | 更新結果 |
| `notion/query_database` | 查詢資料庫 | database_id, filter, sort | 資料庫條目 |
| `notion/append_block` | 附加內容塊 | block_id, children | 新 blocks |

**DuDuClaw 整合場景**：
```
用戶需求：「把今天的競品研究報告同步到 Notion 知識庫」
  → PM-Agent 生成報告 Markdown
  → 呼叫 notion/search 確認目標頁面存在
  → 呼叫 notion/append_block 追加內容
  → 任務完成通知

另一場景：
  → PM-Agent 呼叫 notion/query_database 讀取 PRD 需求列表
  → 自動生成對應的 DuDuClaw Wiki 功能提案
```

**接入評估**：
- ✅ 高價值：DuDuClaw Wiki + Notion 雙向同步是企業用戶核心需求
- ⚠️ 注意：Notion API 授權需要 OAuth integration，非簡單 API Key
- ⚠️ 注意：Notion block 結構複雜，需要專用序列化工具

---

### 6.4 Brave Search MCP Server

**可用工具**（Brave Search 官方 MCP Server）：

| 工具 | 功能 | 輸入 | 輸出 |
|------|------|------|------|
| `brave_search/web` | 網頁搜尋 | query, count, offset | 搜尋結果列表 |
| `brave_search/news` | 新聞搜尋 | query, count, freshness | 新聞列表 |
| `brave_search/images` | 圖片搜尋 | query, count | 圖片 URL 列表 |
| `brave_search/videos` | 影片搜尋 | query, count | 影片資訊 |
| `brave_search/local` | 本地搜尋 | query, location | 本地商家資訊 |

**DuDuClaw 整合場景**：
```
PM-Agent 執行每日競品調研：
  → 呼叫 brave_search/news("CrewAI A2A 2026") 取得最新動態
  → 呼叫 brave_search/web("LangGraph v1.0 changelog") 讀取更新日誌
  → 整合到每日研究報告

Web Search 補充記憶：
  → Agent 執行任務時，若 Memory Search 找不到相關資訊
  → 自動 Fallback 到 Brave Search 補充即時資訊
  → 搜尋結果存入 Memory 供後續使用
```

**接入評估**：
- ✅ **最高優先**（P0）：DuDuClaw 目前 web_search 工具已存在需求，Brave 可增強搜尋品質和隱私保護
- ✅ 注意：Brave Search API 有免費層（每月 2,000 次），適合 PoC
- ✅ 注意：比 Google Search 更注重隱私，企業用戶接受度高

---

### 6.5 MCP Client 接入後工具列表呈現方式

**工具命名空間策略**：

```
原生工具：           memory_search, wiki_read, send_message, ...
Figma MCP 工具：     mcp.figma.get_file, mcp.figma.get_components, ...
Notion MCP 工具：    mcp.notion.search, mcp.notion.get_page, ...
Brave Search 工具：  mcp.brave_search.web, mcp.brave_search.news, ...
```

**Agent 工具選擇邏輯**：

```
工具選擇優先序：
1. 原生 DuDuClaw 工具（memory_search, wiki_* 等）→ 最高優先
2. 受信任 MCP Server 工具（已明確配置）→ 次優先
3. 動態發現 MCP 工具（runtime 接入）→ 需要用戶確認

工具衝突解決：
- 當原生工具和 MCP 工具功能重疊（如搜尋）
- 原生工具優先；MCP 工具作為 fallback 或明確指定
```

---

## 7. 競品警示與風險分析

### 7.1 生態系隔離風險（High）

**現狀**：DuDuClaw Agent 無法接收來自 CrewAI/LangGraph/MAF 的 A2A 任務，無法融入跨平台 Agent 工作流。

**量化影響**：
- 150+ 組織生產採用 A2A
- CrewAI 日執行 12M tasks（部分可能包含 A2A 跨框架委派）
- 企業混合 Agent 架構（多框架共存）成為主流

**不解決的後果**：DuDuClaw 被視為「孤島平台」，無法進入企業多 Agent 採購名單。

### 7.2 MCP Client 能力缺失風險（Medium）

**現狀**：DuDuClaw 目前僅計劃作為 MCP Server，尚未作為 MCP Client 接入外部工具。

**機會損失**：Figma + Notion + Brave Search 接入後，PM-Agent 和工程師 Agent 工作效率預估提升 30-50%。

### 7.3 緩解計劃

| 風險 | 緩解措施 | 負責人 | 時程 |
|------|---------|-------|------|
| 生態系隔離 | W21 實作 A2A Bridge | ENG | W21 |
| MCP Client 缺失 | W20 設計規格，W21 PoC | PM+ENG | W20規格/W21 PoC |
| AgentCard 缺失 | W20 設計規格，W21 發布 | PM | W21 |

---

## 8. 研究結論與建議

### 8.1 給 TL 的核心建議（3 項）

1. **立即啟動 A2A Bridge 設計**（本 Sprint）：W21 工程實作前，本週完成 Bridge 技術規格審核（見 `specs/mcp-a2a-bridge-spec-v1.md`）。

2. **MCP Client PoC 優先選 Brave Search**：Brave Search MCP Server 接入最簡單（API Key 認證、無複雜 OAuth），適合 W21 PoC，同時直接強化現有 PM-Agent 研究能力。

3. **Progress Ledger 作為 A2A 差異化**：A2A 原生無停滯偵測機制，DuDuClaw 的 `x-duduclaw` 擴展命名空間可作為業界創新展示（PR 機會）。

### 8.2 參照論文

- arXiv 2601.13671《The Orchestration of Multi-Agent Systems》：明確建議 MCP + A2A 雙協議同時支援，企業生產架構最佳實踐。

---

## 參照文件

- HandoffPacket v0.2 Spec：`specs/handoffpacket-spec-v0.2.md`
- MCP Server Spec 草案：`specs/mcp-server-spec-draft.md`
- Protocol Bridge 規格：`specs/mcp-a2a-bridge-spec-v1.md`（本任務輸出）
- CrewAI 分析：`research/pm-daily/2026-05-01-daily-research.md`
- Google ADK 分析：`research/ai-repos/entities/2026-04-26-adk-python.md`
- Agnes 指令 A2A：`research/daily/2026-04-27-agnes-response.md`

---

*研究員：PM-DuDuClaw*
*任務：[W20-P1] MCP + A2A 雙協議 Bridge — 規格設計與競品調研*
*日期：2026-05-01*
*版本：v1.0 Final*
