---
title: "MCP + A2A 雙協議 Bridge 規格 v1.1"
created: 2026-05-01T03:00:00Z
updated: 2026-05-01T04:40:00Z
status: tl-approved
author: duduclaw-pm
reviewed_by: duduclaw-tl
tags: [w20, p1, spec, mcp, a2a, bridge, protocol, rust, adapter, gateway]
layer: deep
trust: 0.92
changelog:
  - version: v1.0
    date: 2026-05-01
    author: duduclaw-pm
    changes: "W20-P1 初稿：PM 規格設計"
  - version: v1.1
    date: 2026-05-01
    author: duduclaw-tl
    changes: "TL 審核通過：5 項 TL 決策落地、補充 3 項工程強化要求（AdapterError 型別、壓測環境、external Agent 處理策略）、調整驗收標準"
---

# MCP + A2A 雙協議 Bridge 規格 v1.1

> **狀態**：✅ TL 審核通過（2026-05-01，TL-DuDuClaw）
> **優先級**：W20-P1
> **工程實作**：W21
> **調研依據**：`reports/a2a-protocol-deep-dive-2026-W20.md`
> **關聯規格**：`specs/handoffpacket-spec-v0.2.md`、`specs/mcp-server-spec-draft.md`
> **ADR**：見第 12 節

---

## 1. 背景與目標

### 1.1 問題陳述

DuDuClaw 目前協議生態系現況：

```
現狀（孤島）：
┌─────────────────────────────────────────────────────────┐
│                    DuDuClaw                             │
│  ┌─────────────────────────────────────────────────┐   │
│  │  Internal HandoffPacket v0.2                    │   │
│  │  （僅限 DuDuClaw 內部 Agent 間通訊）              │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘

外部世界：CrewAI ─A2A→ ??? / Figma MCP ─MCP→ ???
```

```
目標（Bridge 後）：
┌─────────────────────────────────────────────────────────┐
│                    DuDuClaw                             │
│  ┌──────────────┐  ┌─────────────────────────────────┐ │
│  │ Protocol     │  │  Internal HandoffPacket v0.2    │ │
│  │ Bridge Layer │◄─►│  （保持不變）                   │ │
│  └──────┬───────┘  └─────────────────────────────────┘ │
│         │                                               │
└─────────┼───────────────────────────────────────────────┘
          │
    ┌─────┴──────┐
    │            │
  A2A         MCP Client
  ─────       ──────────
  CrewAI      Figma MCP
  LangGraph   Notion MCP
  Google ADK  Brave Search
  MAF         任意外部 MCP Server
```

### 1.2 Bridge 的雙重職責

| 職責 | 方向 | 說明 |
|------|------|------|
| **A2A Adapter** | 雙向 | HandoffPacket ↔ A2A Message 相互轉換 |
| **MCP Client Adapter** | 入站 | 外部 MCP Server 工具 → DuDuClaw 工具列表 |

### 1.3 設計原則

1. **零侵入**：現有 HandoffPacket v0.2 Schema 和 Orchestrator 邏輯完全不變
2. **向後兼容**：舊版 DuDuClaw 內部流量繼續走 HandoffPacket，Bridge 僅處理跨協議流量
3. **最小改動 gateway**：現有 `duduclaw-gateway` 新增一個 Bridge Middleware，不重寫路由核心
4. **可插拔架構**：每個外部協議（A2A / MCP / 未來協議）為獨立 Adapter，可獨立部署和更新

---

## 2. 整體架構設計

### 2.1 Bridge Layer 位置

```
┌────────────────────────────────────────────────────────────────┐
│                     DuDuClaw Gateway                           │
│                                                                │
│  ┌───────────────────────────────────────────────────────┐    │
│  │                   Request Router                      │    │
│  │                                                       │    │
│  │  如果 Content-Type: application/vnd.a2a+json          │    │
│  │    → A2A Adapter                                      │    │
│  │  如果 X-MCP-Protocol-Version 存在                      │    │
│  │    → MCP Handler                                      │    │
│  │  其他                                                  │    │
│  │    → 原有 DuDuClaw 內部路由（不變）                    │    │
│  └───────────────────────────────────────────────────────┘    │
│         │                    │                                 │
│  ┌──────▼──────┐    ┌────────▼────────┐                       │
│  │ A2A Adapter │    │  MCP Adapter    │                       │
│  │（Rust impl）│    │（Rust impl）    │                       │
│  └──────┬──────┘    └────────┬────────┘                       │
│         │                    │                                 │
│  ┌──────▼────────────────────▼──────────────────────────┐     │
│  │            HandoffPacket v0.2 Engine                 │     │
│  │            （現有邏輯，完全不動）                      │     │
│  └──────────────────────────────────────────────────────┘     │
└────────────────────────────────────────────────────────────────┘
```

### 2.2 Rust trait-based Adapter 設計

```rust
// ============================================================
// Core Trait: 所有協議 Adapter 必須實作
// ============================================================

#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    /// 協議識別名稱（用於日誌和指標）
    fn protocol_name(&self) -> &str;

    /// 判斷是否能處理此請求
    fn can_handle(&self, request: &IncomingRequest) -> bool;

    /// 外部協議格式 → HandoffPacket（入站轉換）
    async fn decode_to_handoff(
        &self,
        request: IncomingRequest,
    ) -> Result<HandoffPacket, AdapterError>;

    /// HandoffPacket → 外部協議格式（出站轉換）
    async fn encode_from_handoff(
        &self,
        packet: HandoffPacket,
        context: &ConversionContext,
    ) -> Result<OutgoingResponse, AdapterError>;
}

// ============================================================
// AdapterError：完整錯誤型別定義（TL 補充要求）
// ============================================================

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// 協議格式不合法（如 A2A JSON-RPC malformed）
    #[error("Protocol format error: {0}")]
    ProtocolFormat(String),

    /// HandoffPacket Schema 驗證失敗
    #[error("HandoffPacket validation failed: {0}")]
    HandoffValidation(String),

    /// Agent Registry 查找失敗（agent_id 或 URL 無對應）
    #[error("Agent registry lookup failed: {agent_ref}")]
    AgentRegistryMiss { agent_ref: String },

    /// 外部 MCP Server 呼叫失敗
    #[error("MCP client call failed: server={server}, tool={tool}, cause={cause}")]
    McpClientCall { server: String, tool: String, cause: String },

    /// A2A 認證失敗（token 驗證或 scope 不足）
    #[error("A2A authentication failed: {0}")]
    AuthFailure(String),

    /// 轉換超時（轉換本身耗時 > 50ms 視為異常）
    #[error("Adapter conversion timeout after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },

    /// 不支援的協議版本
    #[error("Unsupported protocol version: {version}")]
    UnsupportedVersion { version: String },

    /// 其他內部錯誤
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

// ============================================================
// Adapter Registry：Bridge Layer 的核心
// ============================================================

pub struct BridgeLayer {
    adapters: Vec<Box<dyn ProtocolAdapter>>,
    agent_registry: Arc<AgentRegistry>,
    handoff_engine: Arc<HandoffPacketEngine>,
}

impl BridgeLayer {
    pub async fn handle(&self, request: IncomingRequest) -> Response {
        // 1. 找到能處理此請求的 Adapter
        let adapter = self.adapters
            .iter()
            .find(|a| a.can_handle(&request));

        match adapter {
            Some(adapter) => {
                // 2. 外部格式 → HandoffPacket
                let packet = adapter.decode_to_handoff(request).await?;

                // 3. 走現有 HandoffPacket 引擎（不動）
                let result_packet = self.handoff_engine.process(packet).await?;

                // 4. HandoffPacket → 外部格式回應
                adapter.encode_from_handoff(result_packet, &context).await?
            }
            None => {
                // 5. 無匹配 Adapter → 走原有路由（不變）
                self.handoff_engine.route_internal(request).await
            }
        }
    }
}
```

---

## 3. A2A Adapter 詳細規格

### 3.1 A2A Adapter 職責

```rust
pub struct A2AAdapter {
    agent_registry: Arc<AgentRegistry>,
    config: A2AConfig,
}

pub struct A2AConfig {
    /// 本 DuDuClaw 實例的 A2A 端點 URL
    pub base_url: Url,
    /// AgentCard 快取 TTL（秒）
    pub agent_card_cache_ttl: u64,
    /// 是否啟用 Signed AgentCard（v1.2）— TL 決策：W22 再啟用
    pub enable_signed_cards: bool,
    /// OAuth 2.0 配置（用於驗證外部請求）
    pub oauth_config: OAuthConfig,
}
```

### 3.2 HandoffPacket v0.2 → A2A Message 轉換邏輯

```rust
impl A2AAdapter {
    /// HandoffPacket → A2A Message（出站）
    pub fn encode_packet_to_a2a_message(
        &self,
        packet: &HandoffPacket,
    ) -> Result<A2AMessage, AdapterError> {

        // 1. 解析 payload 為 A2A Parts
        let parts = self.encode_payload_to_parts(&packet.payload)?;

        // 2. 建立 A2A Message
        let mut message = A2AMessage {
            message_id: packet.packet_id.clone(),      // Direct 映射
            role: MessageRole::Agent,
            parts,
            context_id: packet.task_id.clone(),        // task_id 作 context
            task_id: Some(packet.task_id.clone()),     // Direct 映射
            timestamp: packet.created_at.clone(),       // Direct 映射
            metadata: serde_json::Map::new(),
        };

        // 3. 將 Progress Ledger 放入 x-duduclaw metadata 擴展
        if let Some(ledger) = &packet.progress_ledger {
            message.metadata.insert(
                "x-duduclaw".to_string(),
                serde_json::json!({
                    "stagnation_count": ledger.stagnation_count,
                    "stagnation_threshold": ledger.stagnation_threshold,
                    "task_ledger_reset": ledger.task_ledger_reset,
                    "last_meaningful_progress": ledger.last_meaningful_progress,
                    "progress_checkpoint": ledger.progress_checkpoint,
                    "confidence": packet.metadata.confidence,
                    "attempt_number": packet.metadata.attempt_number,
                    "execution_time_ms": packet.metadata.execution_time_ms,
                }),
            );
        }

        // 4. 如果 task_ledger_reset=true，在 A2A 層觸發 TaskResubscriptionRequest
        if let Some(ledger) = &packet.progress_ledger {
            if ledger.task_ledger_reset {
                message.metadata.insert(
                    "x-duduclaw-replan-required".to_string(),
                    serde_json::Value::Bool(true),
                );
            }
        }

        Ok(message)
    }

    /// Payload → A2A Parts
    fn encode_payload_to_parts(
        &self,
        payload: &HandoffPayload,
    ) -> Result<Vec<A2APart>, AdapterError> {
        let mut parts = Vec::new();

        // text → TextPart
        if let Some(text) = &payload.text {
            parts.push(A2APart::Text(TextPart { text: text.clone() }));
        }

        // data → DataPart
        if let Some(data) = &payload.data {
            parts.push(A2APart::Data(DataPart { data: data.clone() }));
        }

        // files → FilePart
        for file in &payload.files {
            parts.push(A2APart::File(FilePart {
                file: FileContent {
                    name: file.name.clone(),
                    mime_type: file.mime_type.clone(),
                    bytes: file.bytes.clone(),
                    uri: file.uri.clone(),
                }
            }));
        }

        Ok(parts)
    }
}
```

### 3.3 A2A Message → HandoffPacket 轉換邏輯

```rust
impl A2AAdapter {
    /// A2A Message → HandoffPacket（入站）
    pub fn decode_a2a_to_packet(
        &self,
        message: &A2AMessage,
        task: &A2ATask,
        from_agent_card: &AgentCard,
    ) -> Result<HandoffPacket, AdapterError> {

        // 1. 解析 Parts → payload
        let payload = self.decode_parts_to_payload(&message.parts)?;

        // 2. 解析 x-duduclaw 擴展欄位
        let progress_ledger = message.metadata
            .get("x-duduclaw")
            .and_then(|v| serde_json::from_value::<ProgressLedger>(v.clone()).ok());

        // 3. 路由解析：A2A AgentCard URL → DuDuClaw Agent ID
        //    TL 補充：external: 前綴 Agent 需記錄至 activity_feed，由 TL 人工審核後加入 Registry
        let from_agent = self.agent_registry
            .lookup_by_url(&from_agent_card.url)
            .unwrap_or_else(|| {
                tracing::warn!(
                    external_agent_id = %from_agent_card.agent_id,
                    url = %from_agent_card.url,
                    "Unknown external A2A Agent; tagging as external — requires manual registry approval"
                );
                format!("external:{}", from_agent_card.agent_id)
            });

        // 4. 組裝 HandoffPacket
        Ok(HandoffPacket {
            packet_id: message.message_id.clone(),
            protocol_version: "0.2".to_string(),
            from_agent,
            to_agent: self.resolve_to_agent(&task)?,
            task_id: task.id.clone(),
            created_at: message.timestamp.clone(),
            payload,
            progress_ledger: progress_ledger.unwrap_or_default(),
            metadata: HandoffMetadata {
                confidence: 1.0,  // 外部 A2A 不提供 confidence，給予預設最高信任值
                attempt_number: 1,
                execution_time_ms: 0,
            },
        })
    }
}
```

### 3.4 AgentCard 生成規格

DuDuClaw 需要為每個 Agent 生成 A2A AgentCard：

```json
{
  "agentId": "duduclaw-pm-v1",
  "name": "PM-DuDuClaw",
  "description": "Product Manager Agent — competitive research, feature proposals, protocol specs",
  "url": "https://api.duduclaw.ai/a2a/v1",
  "version": "1.0.0",
  "provider": {
    "organization": "DuDuClaw",
    "url": "https://duduclaw.ai"
  },
  "capabilities": {
    "streaming": true,
    "pushNotifications": true,
    "stateTransitionHistory": true,
    "extensions": ["x-duduclaw-progress-ledger"]
  },
  "skills": [
    {
      "id": "competitive-analysis",
      "name": "Competitive Analysis",
      "description": "Deep competitive research for AI agent ecosystem frameworks and protocols",
      "tags": ["research", "competitive", "pm"],
      "inputModes": ["text/plain"],
      "outputModes": ["text/plain", "application/json"]
    },
    {
      "id": "feature-proposal",
      "name": "Feature Proposal",
      "description": "Generate structured feature proposals from research insights",
      "tags": ["pm", "product"],
      "inputModes": ["text/plain", "application/json"],
      "outputModes": ["text/plain"]
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
          "scopes": {
            "agent:invoke": "Invoke agent tasks",
            "agent:read": "Read agent status"
          }
        }
      }
    }
  }
}
```

**AgentCard 端點**：
- `GET /.well-known/agent.json` → 公開 AgentCard（未認證）
- `POST /a2a/v1` → A2A JSON-RPC 端點（含 `agent/authenticatedExtendedCard` method）

> **TL 注意**：`/a2a/v1/stream` (SSE) 端點使用獨立路由（非 POST 同端點回 SSE），此設計已確認符合 A2A v1.0 spec 的 transport 彈性條款，可接受。

---

## 4. MCP Client Adapter 詳細規格

### 4.1 MCP Client Adapter 職責

```rust
pub struct MCPClientAdapter {
    /// 已配置的外部 MCP Server 列表
    servers: Vec<MCPServerConfig>,
    /// 動態工具快取（TL 決策：TTL = 5 分鐘）
    tool_cache: Arc<RwLock<ToolRegistry>>,
}

pub struct MCPServerConfig {
    pub name: String,               // e.g. "figma", "notion", "brave_search"
    pub transport: MCPTransport,
    pub auth: MCPAuth,
    pub tool_namespace: String,     // e.g. "mcp.figma"
    pub priority: u8,               // 工具衝突解決優先序
    /// 工具快取 TTL（秒）— TL 決策：固定 300 秒（5 分鐘）
    pub cache_ttl_secs: u64,
}

pub enum MCPTransport {
    Stdio { command: String, args: Vec<String> },
    Http { url: Url, sse_url: Option<Url> },
}

pub enum MCPAuth {
    None,
    ApiKey { key: String, header: String },
    OAuth2 { config: OAuth2Config },
}
```

### 4.2 工具動態載入流程

```
1. DuDuClaw 啟動時：
   → 連接已配置的 MCP Server（stdio / HTTP）
   → 呼叫 MCP initialize + tools/list
   → 快取工具列表，加入 namespace 前綴（TTL = 5 分鐘）

2. Agent 請求工具列表時：
   → 返回 [原生工具] + [MCP 工具（帶 namespace）]
   → Agent 無需感知工具來源差異

3. Agent 呼叫 MCP 工具時（e.g. mcp.brave_search.web）：
   → MCPClientAdapter 剝除 namespace 前綴
   → 轉換為 MCP tools/call 請求
   → 回傳結果合併入 Agent 上下文
```

### 4.3 三個 MCP Server 接入配置

#### Brave Search MCP Server（P0，W21 Day 1-5 優先實作）

```toml
# agent.toml MCP Client 配置
[[mcp.client.servers]]
name = "brave_search"
transport = "http"
url = "https://api.bravesearch.com/mcp/v1"
auth_type = "api_key"
auth_header = "X-Subscription-Token"
# TL 決策：API Key W21 手動輪換，在 .env 標記輪換提醒
# BRAVE_SEARCH_API_KEY_ROTATE_REMINDER=2026-W22
tool_namespace = "mcp.brave_search"
priority = 10
cache_ttl_secs = 300

[[mcp.client.servers.tools_allowlist]]
# 安全白名單（TL 確認：images/videos 暫不開放，PoC 僅需 web+news）
names = ["web", "news"]
```

**接入後可用工具**：
- `mcp.brave_search.web(query: string, count: int)` → 網頁搜尋結果
- `mcp.brave_search.news(query: string, count: int, freshness: string)` → 新聞搜尋

---

#### Figma MCP Server（P1，W21 Day 6-10）

```toml
[[mcp.client.servers]]
name = "figma"
transport = "http"
url = "https://api.figma.com/mcp/v1"
auth_type = "oauth2"
oauth_client_id = "${FIGMA_CLIENT_ID}"
oauth_client_secret = "${FIGMA_CLIENT_SECRET}"
tool_namespace = "mcp.figma"
priority = 20
cache_ttl_secs = 300

[[mcp.client.servers.tools_allowlist]]
names = ["get_file", "get_node", "get_components", "get_variables", "get_image"]
```

**接入後可用工具**：
- `mcp.figma.get_file(file_key: string)` → 設計稿全文
- `mcp.figma.get_components(file_key: string)` → 元件庫
- `mcp.figma.get_variables(file_key: string)` → Design Tokens

---

#### Notion MCP Server（P1，W21 Day 6-10）

```toml
[[mcp.client.servers]]
name = "notion"
transport = "http"
url = "https://api.notion.com/mcp/v1"
auth_type = "oauth2"
oauth_client_id = "${NOTION_CLIENT_ID}"
oauth_client_secret = "${NOTION_CLIENT_SECRET}"
tool_namespace = "mcp.notion"
priority = 20
cache_ttl_secs = 300

[[mcp.client.servers.tools_allowlist]]
names = ["search", "get_page", "create_page", "update_page", "query_database", "append_block"]
```

**接入後可用工具**：
- `mcp.notion.search(query: string, filter: object)` → 頁面搜尋
- `mcp.notion.get_page(page_id: string)` → 讀取頁面
- `mcp.notion.create_page(parent_id: string, title: string, content: array)` → 建立頁面

---

### 4.4 工具列表呈現給 Agent 的格式

```json
{
  "tools": [
    {
      "name": "memory_search",
      "description": "Search DuDuClaw memory",
      "source": "native",
      "inputSchema": {}
    },
    {
      "name": "mcp.brave_search.web",
      "description": "[MCP:brave_search] Web search powered by Brave Search API",
      "source": "mcp:brave_search",
      "inputSchema": {
        "type": "object",
        "properties": {
          "query": {"type": "string", "description": "Search query"},
          "count": {"type": "integer", "default": 10}
        },
        "required": ["query"]
      }
    }
  ]
}
```

---

## 5. 向後兼容策略

### 5.1 現有 duduclaw-gateway 最小改動

**不需要修改的部分**：
- HandoffPacket v0.2 Schema（完全不動）
- Orchestrator 重規劃邏輯（完全不動）
- Agent 內部 WorkerConfig（完全不動）
- Evolution Events（完全不動）

**需要修改的部分**（最小改動）：

```rust
// 修改前（現有 gateway 路由）：
pub async fn route(request: Request) -> Response {
    handoff_engine.process(request).await
}

// 修改後（加入 Bridge Middleware）：
pub async fn route(request: Request) -> Response {
    // Bridge Layer 優先判斷
    if bridge_layer.can_handle(&request) {
        return bridge_layer.handle(request).await;
    }
    // 原有邏輯不變
    handoff_engine.process(request).await
}
```

**改動量估算**：gateway 主路由約 **5-10 行**修改。Bridge Layer 為獨立模組。

### 5.2 協議版本協商

```
入站 A2A 請求 → 檢查 A2A version header：
  - v1.0 / v1.2：完整支援
  - 未來版本：降級處理 + 警告 log + AdapterError::UnsupportedVersion

出站 A2A 回應 → 固定使用 v1.0（最大兼容性）
  - Signed AgentCard（v1.2）：W22 啟用（TL 決策，見 ADR-001）
```

### 5.3 DuDuClaw 私有擴展（x-duduclaw namespace）

所有 HandoffPacket 獨有欄位（Progress Ledger、confidence、attempt_number）透過 `x-duduclaw` metadata namespace 傳遞：

```json
{
  "metadata": {
    "x-duduclaw": {
      "stagnation_count": 2,
      "stagnation_threshold": 3,
      "task_ledger_reset": false,
      "last_meaningful_progress": "2026-05-01T02:00:00Z",
      "progress_checkpoint": "Brave Search completed, parsing failed",
      "confidence": 0.85,
      "attempt_number": 2,
      "execution_time_ms": 1250
    }
  }
}
```

> **TL 決策（ADR-002）**：`x-duduclaw` 擴展 W21 不加版本欄位，保持簡單。W22 Sprint Planning 時根據欄位穩定性重新評估。

**接收方（外部 A2A 框架）**：不識別 `x-duduclaw` → 忽略（A2A spec 允許未知 metadata）。
**接收方（DuDuClaw）**：完整解析 `x-duduclaw` → 恢復 HandoffPacket Progress Ledger。

---

## 6. Agent Registry

A2A 路由從「Agent ID」模型轉為「URL 尋址」模型，需要 Agent Registry 橋接：

```rust
pub struct AgentRegistry {
    /// agent_id → A2A endpoint URL
    /// 注意：使用 String（而非 Url）作為 HashMap key，避免 Url Hash impl 依賴 crate 差異
    id_to_url: HashMap<String, String>,
    /// A2A endpoint URL → agent_id（反向查詢）
    url_to_id: HashMap<String, String>,
    /// 外部 Agent 的 AgentCard 快取（key: agent_card_url）
    external_cards: HashMap<String, (AgentCard, Instant)>,
}

impl AgentRegistry {
    /// 查詢 Agent 的 A2A endpoint URL
    pub fn get_endpoint(&self, agent_id: &str) -> Option<&str> {
        self.id_to_url.get(agent_id).map(|s| s.as_str())
    }

    /// 從 URL 反查 agent_id（用於入站請求解析）
    pub fn lookup_by_url(&self, url: &str) -> Option<String> {
        self.url_to_id.get(url).cloned()
    }

    /// 動態發現外部 Agent（從 AgentCard URL 解析）
    pub async fn discover(&mut self, agent_card_url: &str) -> Result<AgentCard, AdapterError> {
        // 1. GET {agent_card_url}/.well-known/agent.json
        // 2. 解析 AgentCard
        // 3. 快取 URL → agent_id 映射
        // 4. 回傳 AgentCard（供呼叫方決定是否加入 Trusted Registry）
    }
}
```

**預配置的 DuDuClaw Agent 映射**：

```toml
[agent_registry]
[agent_registry.agents.duduclaw-pm]
a2a_url = "https://api.duduclaw.ai/a2a/v1/agents/pm"

[agent_registry.agents.duduclaw-tl]
a2a_url = "https://api.duduclaw.ai/a2a/v1/agents/tl"

[agent_registry.agents.duduclaw-eng]
a2a_url = "https://api.duduclaw.ai/a2a/v1/agents/eng"
```

> **TL 補充**：`external:` 前綴 Agent（Registry 未知的入站 A2A 請求）需自動觸發 `activity_post` 警示事件，供 TL 人工審核並決定是否加入 Trusted Registry。這是安全邊界的重要環節。

---

## 7. A2A Server 端點規格

DuDuClaw 需暴露的 HTTP 端點（讓外部框架接入）：

| 端點 | HTTP 方法 | 功能 |
|------|----------|------|
| `/.well-known/agent.json` | GET | 公開 AgentCard（無認證）|
| `/a2a/v1` | POST | A2A JSON-RPC 端點（所有 11 個方法）|
| `/a2a/v1/stream` | GET/SSE | SSE 串流端點 |

**A2A JSON-RPC 支援矩陣（Phase 1 W21）**：

| 方法 | W21 Phase 1 | 說明 |
|------|------------|------|
| `tasks/send` | ✅ 必實作 | 接收外部 Agent 任務 |
| `tasks/get` | ✅ 必實作 | 查詢任務狀態 |
| `tasks/cancel` | ✅ 必實作 | 取消任務 |
| `tasks/sendSubscribe` | ✅ 必實作 | SSE 串流任務 |
| `tasks/subscribe` | ⚠️ Phase 2 | 複雜性較高 |
| `tasks/resubscribe` | ⚠️ Phase 2 | 複雜性較高 |
| `tasks/pushNotificationConfig/*` | ⚠️ Phase 2 | Webhook 支援 |
| `agent/authenticatedExtendedCard` | ✅ 必實作 | 認證後 AgentCard |
| `message/send` | ✅ 必實作 | 輕量訊息 |
| `message/stream` | ⚠️ Phase 2 | 對話式串流 |

---

## 8. 安全設計

### 8.1 入站 A2A 請求驗證

```
1. OAuth 2.0 Token 驗證
   → Bearer token → DuDuClaw Auth Server 驗證
   → 解析 scope：agent:invoke 必須存在

2. Signed AgentCard 驗證（可選，v1.2）
   → W22 啟用（TL 決策，ADR-001）

3. Rate Limiting（與現有 MCP Server 相同策略）
   → Level 1（外部）：60 tasks/min
   → Level 2（受信任外部）：300 tasks/min

4. external: 前綴 Agent 流量監控（TL 新增要求）
   → 自動觸發 activity_post 警示
   → 限制 external: Agent 預設只能存取 message/send（最低權限）
   → 需 TL 手動升級為 Trusted Registry 才可執行 tasks/send
```

### 8.2 MCP Client 安全邊界

```
1. 工具白名單：每個 MCP Server 配置 tools_allowlist，只暴露允許的工具
2. 輸入清洗：MCP 工具輸入在傳出前做 schema 驗證
3. 輸出過濾：MCP 工具回傳內容不直接存入 DuDuClaw 記憶（需明確指令）
4. 憑證隔離：MCP Server 認證憑證不洩漏給 Agent（由 MCPClientAdapter 持有）
5. API Key 輪換提醒（TL 新增）：.env 中加入 ROTATE_REMINDER 欄位，W22 評估 Secret Manager
```

---

## 9. 監控與可觀測性

### 9.1 Bridge 指標

```
# A2A Adapter 指標
bridge.a2a.encode_latency_ms        # HandoffPacket → A2A 轉換延遲
bridge.a2a.decode_latency_ms        # A2A → HandoffPacket 轉換延遲
bridge.a2a.tasks_received_total     # 接收外部 A2A 任務數
bridge.a2a.tasks_sent_total         # 送出 A2A 任務數
bridge.a2a.extension_used_total     # x-duduclaw 擴展使用次數
bridge.a2a.replan_triggered_total   # A2A 層觸發重規劃次數
bridge.a2a.external_agent_unknown   # external: 前綴 Agent 出現次數（安全監控）

# MCP Client 指標
bridge.mcp_client.tool_calls_total  # 外部 MCP 工具呼叫次數（by server）
bridge.mcp_client.tool_latency_ms   # MCP 工具呼叫延遲
bridge.mcp_client.cache_hits_total  # 工具列表快取命中率（TTL=5min）
bridge.mcp_client.errors_total      # 外部 MCP Server 錯誤數
```

### 9.2 日誌結構

```json
{
  "level": "INFO",
  "component": "bridge.a2a_adapter",
  "event": "packet_converted",
  "direction": "a2a_to_handoff",
  "packet_id": "msg-001",
  "from_framework": "crewai",
  "extension_fields_count": 7,
  "conversion_latency_ms": 2
}
```

---

## 10. 實作路徑（W20-W21）

### 10.1 本週（W20）— 規格完成

- [x] A2A Protocol 深度調研報告（`reports/a2a-protocol-deep-dive-2026-W20.md`）
- [x] Bridge 規格文件 v1.0（PM 初稿）
- [x] **TL 審核通過**（v1.1，2026-05-01，TL-DuDuClaw）

### 10.2 W21 Phase 1 — 工程實作

**Week 1（W21 Day 1-3）**：
- A2A Adapter 核心（HandoffPacket ↔ A2A 轉換）
- AgentCard 端點（`/.well-known/agent.json`）
- `tasks/send`、`tasks/get`、`tasks/cancel` 方法
- Agent Registry（DuDuClaw 內部 Agent 預配置）
- AdapterError 型別完整實作（參照第 2.2 節）

**Week 1（W21 Day 4-5）**：
- Brave Search MCP Client Adapter（P0）
- `tasks/sendSubscribe` SSE 串流
- `message/send` 輕量訊息
- `agent/authenticatedExtendedCard`

**Week 2（W21 Day 1-3）**：
- Figma MCP Client Adapter（P1）
- Notion MCP Client Adapter（P1）
- external: Agent 警示 activity_post 流程

**Week 2（W21 Day 4-5）**：
- 整合測試：DuDuClaw ↔ CrewAI A2A 端對端
- 整合測試：Brave Search MCP Client 呼叫
- **壓測**：Bridge 轉換延遲目標 P99 < 10ms（需部署壓測環境，ENG 確認 W21 Day 1 前建立）

### 10.3 驗收標準（W21 QA，4 輪深度審查）

**功能驗收**：
- [ ] A2A `tasks/send` → HandoffPacket 轉換正確（逐欄位驗證，18 個欄位全部覆蓋）
- [ ] HandoffPacket `task_ledger_reset=true` → A2A 層正確設定 `x-duduclaw-replan-required`
- [ ] External A2A Client（如 Google ADK Python）可成功呼叫 DuDuClaw A2A Server
- [ ] Brave Search MCP 工具可被 DuDuClaw PM-Agent 呼叫
- [ ] 3 個 MCP Server 工具正確加入 Agent 工具列表（帶 namespace）
- [ ] `external:` 前綴入站請求自動觸發 activity_post 警示
- [ ] 原有 HandoffPacket 內部流量不受 Bridge 影響（回歸測試，含現有 778 gateway tests 零退化）

**效能驗收**（壓測環境）：
- [ ] Bridge A2A 轉換延遲 P50 < 2ms、P99 < 10ms
- [ ] MCP Client 工具呼叫額外延遲（扣除外部 API 延遲）P99 < 5ms
- [ ] 工具快取命中率 > 80%（steady state）

**安全驗收**：
- [ ] `external:` Agent 僅能執行 `message/send`，tasks/send 被拒（401）
- [ ] MCP Client 憑證不出現在任何 Agent 工具回應中
- [ ] Brave Search API Key 不出現在日誌或指標 label 中

---

## 11. TL 審核決策紀錄

> **審核日期**：2026-05-01
> **審核人**：TL-DuDuClaw

### 11.1 五項 TL 決策（已落地至規格）

| 問題 | TL 決策 | 理由 | ADR 編號 |
|------|--------|------|---------|
| A2A Server 是否整合進現有 gateway 還是獨立 Service？ | ✅ **整合進 gateway**（Option A）| W21 快速交付優先；Bridge Layer 已設計為可插拔，W22+ 如需擴展可獨立部署而無需重寫業務邏輯 | ADR-003 |
| x-duduclaw 擴展欄位是否需要版本化？ | ✅ **不加版本（Option B）** | W21 欄位結構尚未穩定，過早版本化增加複雜度；W22 Sprint Planning 重新評估 | ADR-002 |
| Brave Search API Key 輪換策略？ | ✅ **W21 手動輪換（Option A）** | PoC 階段使用量低，手動足夠；.env 加入 ROTATE_REMINDER=2026-W22；W22 評估 Secret Manager 整合 | ADR-004 |
| MCP Client 工具快取 TTL？ | ✅ **5 分鐘（300 秒）** | 即時性優先（PoC 階段工具配置頻繁調整）；生產穩定後 W22 可調升至 15-30 分鐘 | — |
| Signed AgentCard（A2A v1.2）W21 是否啟用？ | ✅ **W22 再評估（Option B）** | A2A v1.2 DNS 驗證增加 W21 交付複雜度；多數外部框架仍在 v1.0；W21 先交付核心功能 | ADR-001 |

### 11.2 TL 新增工程強化要求（v1.1 補充）

1. **AdapterError 型別完整定義**（已補入第 2.2 節）：工程師必須實作所有列出的 Error variants，不允許以 `anyhow::Error` 包覆所有錯誤。

2. **壓測環境要求**：W21 Day 1 前 ENG 需確認已建立壓測環境，否則 P99 < 10ms 的驗收標準無法驗證。

3. **external: Agent 處理策略**（已補入第 3.3 節、第 6 節、第 8.1 節）：未知 A2A 入站流量需有明確的安全邊界（最低權限 + 自動警示），不可靜默放行。

### 11.3 整體審核評語

**A2A 深度調研報告**：研究品質優秀。18 欄位對應表系統完整，Progress Ledger 作為差異化優勢的定位準確。CrewAI 競品分析的量化資料（45,900 Stars、12M daily tasks、82% 成功率）具體有力。建議 PM 在 W21 結束後更新報告，加入實際 Bridge 接入後的互通性驗證結果。

**Bridge 規格 v1.0**：架構設計清晰，「零侵入、最小改動 gateway」原則貫徹到位。Rust trait-based Adapter 的設計符合 DuDuClaw 架構一致性要求。主要補強：AdapterError 型別、external Agent 安全策略、壓測要求。

**整體評分**：**通過（附條件）** — v1.1 已解決所有條件，W21 可直接接手實作。

---

## 12. Architecture Decision Records（ADR）

### ADR-001：Signed AgentCard 延至 W22

- **狀態**：已決定
- **背景**：A2A v1.2 Signed AgentCard 需要 DNS TXT record 設定和私鑰管理基礎設施
- **決策**：W21 僅實作 A2A v1.0，v1.2 延至 W22 Sprint 評估
- **影響**：W21 期間 Agent 身份驗證依賴 OAuth 2.0，無 DNS 層級防偽造保護

### ADR-002：x-duduclaw 擴展暫不版本化

- **狀態**：已決定
- **背景**：Progress Ledger 欄位在 W21 期間仍可能調整
- **決策**：W21 不加 `x-duduclaw-schema-version` 欄位
- **重新評估點**：W22 Sprint Planning，若欄位結構穩定則加入版本化
- **影響**：若 W21 期間欄位定義改變，外部已接入的 DuDuClaw 實例需手動同步更新

### ADR-003：A2A Server 整合進現有 gateway

- **狀態**：已決定
- **背景**：獨立 Service 提供更清晰隔離，但增加 W21 部署複雜度
- **決策**：整合進 `duduclaw-gateway`，利用已有的 auth、rate limiting 等中間件
- **重新評估點**：W23，若 A2A 流量超過 gateway 承載能力或需獨立 SLA，則拆分
- **影響**：A2A 與內部流量共用 gateway 資源，需監控壓測結果

### ADR-004：Brave Search API Key 手動輪換

- **狀態**：已決定（暫時）
- **背景**：PoC 階段使用量低，Secret Manager 整合需要額外工程工作
- **決策**：W21 手動輪換，.env 加入提醒欄位
- **重新評估點**：W22，若 Brave Search 接入轉為生產正式使用，則必須整合 Secret Manager
- **影響**：W21 期間若 API Key 洩漏，響應時間取決於人工發現速度

---

## 13. 參照

- A2A 深度調研報告：`reports/a2a-protocol-deep-dive-2026-W20.md`
- HandoffPacket v0.2 Spec：`specs/handoffpacket-spec-v0.2.md`
- MCP Server 規格草案：`specs/mcp-server-spec-draft.md`
- arXiv 2601.13671：The Orchestration of Multi-Agent Systems（MCP + A2A 雙協議）
- Agnes A2A 指令：`research/daily/2026-04-27-agnes-response.md`

---

*起草人：PM-DuDuClaw*
*TL 審核：TL-DuDuClaw（v1.1，2026-05-01）*
*版本：v1.1 — TL Approved*
*[W20-P1]*
