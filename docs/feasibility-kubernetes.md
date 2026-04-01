# DuDuClaw Kubernetes 叢集支援可行性評估

> 更新日期：2026-03-30
> DuDuClaw 版本：v0.12.0
> 目的：評估 DuDuClaw 支援 Kubernetes 部署的技術可行性、架構衝擊、工作量與 ROI

---

## 一、現狀分析

### 1.1 當前部署架構

DuDuClaw 是 **單一 Rust 二進位 + 嵌入式 Dashboard** 的單機 monolith：

```
┌──────────────────────────────────────────┐
│              duduclaw binary              │
│  ┌─────────┬──────────┬────────────────┐ │
│  │ Gateway  │ Channels │  Dashboard     │ │
│  │ (Axum)   │ TG/LINE/ │  (React SPA)  │ │
│  │ :18789   │ Discord  │  embedded     │ │
│  ├─────────┼──────────┼────────────────┤ │
│  │ Dispatcher│ Inference│  Memory       │ │
│  │ IPC poll  │ llama.cpp│  SQLite+FTS5  │ │
│  │ 5s cycle  │ Exo/MLX │              │ │
│  ├─────────┴──────────┴────────────────┤ │
│  │  Claude CLI subprocesses (max 4)     │ │
│  │  bus_queue.jsonl (file-based IPC)    │ │
│  │  ~/.duduclaw/ (config + state)       │ │
│  └──────────────────────────────────────┘ │
└──────────────────────────────────────────┘
```

**關鍵依賴**：
- **檔案系統 IPC**：`bus_queue.jsonl` 透過原子 rename 實現 crash-safe 通訊
- **嵌入式 SQLite**：Session、Memory、CostTelemetry 皆使用本地 SQLite
- **本地行程管理**：Claude CLI 以子行程方式啟動，Dispatcher 透過 semaphore 控制並行數
- **Docker socket**：Sandbox 模式需要掛載 `/var/run/docker.sock`
- **本地模型檔**：`~/.duduclaw/models/` 存放 GGUF 檔案（通常 4-30 GB）
- **llamafile 子行程**：本地推理引擎透過子行程管理生命週期

### 1.2 現有容器化支援

| 項目 | 狀態 |
|------|------|
| Dockerfile (server) | ✅ 多階段建構，含 Node + Rust + Python |
| Dockerfile (agent sandbox) | ✅ 輕量 agent 容器 |
| docker-compose.yml | ✅ 單機部署（server + watchtower + tailscale） |
| Health check endpoint | ✅ `/health` HTTP |
| Kubernetes 相關程式碼 | ❌ 完全沒有 |
| Helm chart | ❌ 無 |
| Operator | ❌ 無 |

---

## 二、Kubernetes 支援範圍定義

Kubernetes 支援可分為 **三個層級**，複雜度遞增：

### Level 1：K8s-Compatible（可在 K8s 上跑）
最低限度修改，讓現有單體 binary 能作為 K8s Pod 運行。

### Level 2：K8s-Native（利用 K8s 原語）
拆分為微服務，利用 K8s Service / StatefulSet / PVC / HPA 等原語。

### Level 3：K8s-Operator（自訂控制器）
開發 CRD + Operator，以聲明式 YAML 管理 Agent/Channel/Inference。

---

## 三、Level 1 — K8s-Compatible 評估

### 3.1 需要變更的項目

| 項目 | 變更內容 | 工作量 | 風險 |
|------|---------|--------|------|
| **IPC 機制** | `bus_queue.jsonl` 需改為同 Pod 內共享 Volume 或保持單 Pod | 0 天（單 Pod 不變） | 低 |
| **SQLite 存儲** | 掛載 PVC (ReadWriteOnce) | 0.5 天 | 低 |
| **模型檔案** | PVC 或 NFS / S3 掛載 | 1 天 | 中 — 大檔案首次載入慢 |
| **Health probe** | `/health` 已存在，加 readiness/liveness 區分 | 0.5 天 | 低 |
| **Graceful shutdown** | 確認 SIGTERM 處理完整（正在跑的 Claude CLI 子行程） | 1 天 | 中 |
| **Helm chart** | 基礎 Deployment + Service + PVC + ConfigMap | 2 天 | 低 |
| **環境變數配置** | ConfigMap/Secret 注入 API key、OAuth token | 0.5 天 | 低 |
| **Docker socket** | Sandbox 模式不可用（K8s 內無 Docker socket） | 1 天（改用 K8s Job） | 高 |
| **llamafile 子行程** | Pod 內啟動子行程可能需調整資源限制 | 0.5 天 | 中 |

**Level 1 總工作量：~6-7 天**

### 3.2 Level 1 架構圖

```
K8s Cluster
┌───────────────────────────────────────────────────┐
│  Namespace: duduclaw                              │
│                                                   │
│  ┌─────────────────────────────────────────────┐  │
│  │ Deployment: duduclaw-server (replicas: 1)   │  │
│  │  ┌───────────────────────────────────────┐  │  │
│  │  │ Pod                                   │  │  │
│  │  │  duduclaw binary (monolith)           │  │  │
│  │  │  + Claude CLI subprocesses            │  │  │
│  │  │  Volumes:                             │  │  │
│  │  │   /data (PVC) — SQLite + bus_queue    │  │  │
│  │  │   /models (PVC/NFS) — GGUF files      │  │  │
│  │  │   /config (ConfigMap) — config.toml   │  │  │
│  │  └───────────────────────────────────────┘  │  │
│  └─────────────────────────────────────────────┘  │
│                                                   │
│  Service: duduclaw (ClusterIP :18789)             │
│  Ingress: duduclaw.example.com → :18789           │
│                                                   │
│  Secret: duduclaw-credentials                     │
│   (API keys, OAuth tokens, channel tokens)        │
└───────────────────────────────────────────────────┘
```

### 3.3 Level 1 關鍵限制

1. **無法水平擴展**：單 Pod、replicas=1，因為 SQLite 不支援多寫者
2. **Sandbox 模式失效**：K8s Pod 內無 Docker daemon，需改架構
3. **GPU 調度**：需要 `nvidia.com/gpu` 或 `apple.com/gpu` 資源聲明，但 K8s 對 Apple Silicon GPU 無原生支援
4. **檔案系統耦合**：bus_queue.jsonl 是程序內通訊，單 Pod 沒問題，但無法跨 Pod
5. **有狀態**：SQLite + 本地模型 = StatefulSet 更合適但增加複雜度

---

## 四、Level 2 — K8s-Native 微服務化評估

### 4.1 需要拆分的元件

```
┌──────────────────────────────────────────────────────────────┐
│  K8s Cluster                                                  │
│                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐       │
│  │ gateway      │  │ channel-tg   │  │ channel-line │       │
│  │ (Deployment) │  │ (Deployment) │  │ (Deployment) │       │
│  │ Axum + WS    │  │ TG poller    │  │ LINE webhook │       │
│  │ replicas: 2  │  │ replicas: 1  │  │ replicas: 2  │       │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘       │
│         │                 │                  │               │
│         ├─────────────────┼──────────────────┘               │
│         ▼                 ▼                                   │
│  ┌──────────────────────────────┐                            │
│  │ Message Queue (NATS / Redis  │                            │
│  │ Stream / RabbitMQ)           │                            │
│  └──────────────┬───────────────┘                            │
│                 │                                             │
│    ┌────────────┼────────────┐                               │
│    ▼            ▼            ▼                                │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐                     │
│  │dispatcher│ │ memory   │ │ inference│                     │
│  │(Deploym.)│ │(Stateful)│ │(Deploym.)│                     │
│  │Agent CLI │ │PostgreSQL│ │GPU nodes │                     │
│  │replicas:N│ │replicas:1│ │replicas:M│                     │
│  └──────────┘ └──────────┘ └──────────┘                     │
│                                                              │
│  ┌──────────────────────────────┐                            │
│  │ PostgreSQL (StatefulSet)     │                            │
│  │ — sessions, telemetry, audit │                            │
│  └──────────────────────────────┘                            │
└──────────────────────────────────────────────────────────────┘
```

### 4.2 架構變更需求

| 項目 | 現狀 | K8s-Native 需求 | 工作量 | 影響 |
|------|------|-----------------|--------|------|
| **IPC** | `bus_queue.jsonl` 本地檔案 | NATS/Redis Stream 訊息佇列 | 3-4 週 | **破壞性** — 核心通訊機制重寫 |
| **資料庫** | SQLite (嵌入式) | PostgreSQL (叢集模式) | 2-3 週 | **破壞性** — 所有 DB 層重寫 |
| **Memory 引擎** | SQLite FTS5 | PostgreSQL `pg_trgm` 或 pgvector | 2 週 | **破壞性** — 搜尋引擎重寫 |
| **Session 管理** | 本地 SQLite | PostgreSQL + 分散式鎖 | 1-2 週 | **破壞性** |
| **Channel 拆分** | 單行程內 tokio task | 獨立微服務 + gRPC/NATS | 2-3 週 | **破壞性** |
| **Dispatcher** | 檔案輪詢 + 子行程 | K8s Job/CronJob | 2-3 週 | **破壞性** |
| **Inference** | 程序內 + 子行程 | 獨立 inference 服務 + GPU 調度 | 2-3 週 | 中等 — 已有 HTTP 介面 |
| **CostTelemetry** | 本地 SQLite | PostgreSQL + 跨服務匯聚 | 1 週 | 中等 |
| **Config 管理** | 本地 TOML 檔案 | ConfigMap + 動態 reload | 1 週 | 低 |
| **Agent Sandbox** | Docker socket | K8s Job API (in-cluster RBAC) | 1-2 週 | 中等 |
| **Dashboard** | 嵌入 binary | 獨立前端 Pod 或 CDN | 1 週 | 低 |
| **服務探索** | 無（單行程） | K8s Service + DNS | 1 週 | 低 |

**Level 2 總工作量：~4-5 個月（1 位全職工程師）**

### 4.3 Level 2 關鍵風險

| 風險 | 嚴重度 | 說明 |
|------|--------|------|
| **核心架構重寫** | 🔴 Critical | IPC + DB 兩大基石同時替換，等同重寫 50%+ 程式碼 |
| **SQLite → PostgreSQL 遷移** | 🔴 Critical | FTS5、WAL mode、file-level lock 全部失效，需重新實作 |
| **延遲增加** | 🟡 High | 訊息佇列 + 跨 Pod 網路 vs 程序內直接呼叫，P99 延遲增加 5-20ms |
| **運維複雜度** | 🟡 High | 從「一個 binary + SQLite」變成「6+ 微服務 + PostgreSQL + 訊息佇列」|
| **本地推理 + K8s GPU** | 🟡 High | NVIDIA GPU 有 device-plugin，但 Apple Silicon 無 K8s GPU 支援 |
| **開發速度下降** | 🟡 High | 微服務開發循環慢（build + push + deploy vs cargo run） |
| **成本** | 🟠 Medium | K8s 叢集本身成本（EKS $73/月 control plane + 節點） |
| **單人維護不可行** | 🟡 High | 微服務架構需要 SRE 技能，超出個人開發者 / 小團隊能力 |

---

## 五、Level 3 — K8s Operator 評估

### 5.1 CRD 設計草案

```yaml
apiVersion: duduclaw.io/v1alpha1
kind: Agent
metadata:
  name: dudu-main
spec:
  soul: |
    你是 DuDu，一隻友善的爪子...
  model:
    preferred: claude-sonnet-4-6
    local:
      model: qwen3-8b
      backend: llama_cpp
  channels:
    - type: telegram
      token:
        secretRef: telegram-bot-token
    - type: line
      channelSecret:
        secretRef: line-channel-secret
  budget:
    monthlyLimitCents: 5000
  sandbox:
    enabled: true
    resources:
      limits:
        memory: 512Mi
        cpu: "2"
---
apiVersion: duduclaw.io/v1alpha1
kind: InferencePool
metadata:
  name: local-gpu
spec:
  backend: llama_cpp
  models:
    - name: qwen3-8b
      path: /models/qwen3-8b-q4_k_m.gguf
  resources:
    limits:
      nvidia.com/gpu: 1
  replicas: 2
```

### 5.2 Operator 工作量

| 項目 | 工作量 | 說明 |
|------|--------|------|
| CRD 定義 (Agent, Channel, InferencePool) | 2 週 | OpenAPI schema + validation |
| Reconciler 邏輯 | 4-6 週 | Rust kube-rs 或 Go controller-runtime |
| Agent lifecycle（啟動/暫停/終止/演化） | 3-4 週 | 對映 agent.toml 語義到 K8s 原語 |
| Inference 調度（GPU 感知） | 2-3 週 | 節點親和、GPU 資源管理 |
| Channel 管理（熱啟停） | 2 週 | 不重啟 Gateway 即可增減頻道 |
| Webhook 自動設定（LINE/Telegram） | 1 週 | cert-manager + Ingress 整合 |
| 可觀測性（Prometheus + Grafana） | 1-2 週 | ServiceMonitor, dashboards |
| 測試 + CI | 2-3 週 | E2E on kind/k3d |

**Level 3 總工作量：Level 2 + 4-5 個月 = 合計 8-10 個月**

---

## 六、關鍵技術挑戰

### 6.1 SQLite → PostgreSQL 遷移

這是最大的障礙。DuDuClaw 深度耦合 SQLite：

```
受影響的元件：
├── duduclaw-memory/src/engine.rs     — FTS5 全文搜索
├── duduclaw-gateway/src/session.rs   — Session 持久化
├── duduclaw-gateway/src/cost_telemetry.rs — Token 追蹤
├── duduclaw-agent/src/registry.rs    — Agent 狀態
└── duduclaw-security/               — 審計日誌
```

**遷移策略選項**：
1. **抽象層**：在 `duduclaw-core/traits.rs` 定義 `Storage` trait，SQLite / PostgreSQL 各自實作 → 保持向下相容但工作量大
2. **直接替換**：全部改 PostgreSQL → 簡單粗暴但失去「零依賴」優勢
3. **混合模式**：本地 SQLite（開發/小規模） + PostgreSQL（K8s/生產） → 維護兩套路徑

**建議**：選項 1（抽象層），但這本身就是 2-3 週的重構。

### 6.2 檔案式 IPC → 訊息佇列

`bus_queue.jsonl` 是整個 Agent 間通訊的基礎：

```
影響鏈：
bus_queue.jsonl
├── dispatcher.rs     — 每 5 秒輪詢，原子 rename
├── ipc.rs            — 訊息序列化/反序列化
├── heartbeat.rs      — 排程 bus 輪詢
├── handlers.rs       — agents.delegate 寫入佇列
└── cron_scheduler.rs — 排程任務寫入佇列
```

**替代方案**：
| 方案 | 優點 | 缺點 |
|------|------|------|
| **NATS JetStream** | 輕量、Rust 客戶端成熟、at-least-once | 新增外部依賴 |
| **Redis Streams** | 簡單、普及 | 記憶體限制、持久化需調優 |
| **PostgreSQL LISTEN/NOTIFY** | 不新增依賴（若已遷移 PG） | 非持久化、大量訊息效能差 |
| **K8s ConfigMap polling** | 零依賴 | 效能差、不適合即時通訊 |

**建議**：NATS JetStream — 零組態、嵌入式模式可嵌入測試、Rust crate `async-nats` 成熟。

### 6.3 Apple Silicon GPU 在 K8s 中的支援

| 平台 | GPU K8s 支援 | 狀態 |
|------|-------------|------|
| NVIDIA | ✅ 成熟（device-plugin + GPU Operator） | 生產可用 |
| AMD ROCm | ⚠️ 早期（amd-gpu device plugin） | 有限支援 |
| Intel ARC | ⚠️ 早期（intel-device-plugins） | 有限支援 |
| **Apple Silicon** | ❌ 完全不支援 | Apple 無 K8s GPU scheduler |

**影響**：DuDuClaw 的核心優勢之一是 Apple Silicon Metal 推理。在 K8s 環境中：
- `llama.cpp` Metal backend → 無法使用
- `MLX bridge` → 無法使用
- `Exo P2P` → 可獨立於 K8s 運行，但失去叢集管理統一性

**變通方案**：
1. 混合架構 — K8s 管理 Gateway/Channel/Memory，Inference 跑在 K8s 外的 GPU 機器
2. 僅支援 NVIDIA GPU 節點 — 放棄 Apple Silicon 優勢
3. 使用 Exo P2P 作為推理層 — K8s 內只部署 Exo HTTP 客戶端

### 6.4 Claude CLI 子行程管理

DuDuClaw 透過 `std::process::Command` 啟動 `claude` CLI：

```
問題：
1. K8s Pod 內需要安裝 Claude CLI（~200MB Node.js 依賴）
2. 子行程的 stdout/stderr 在容器中需要特殊處理
3. OAuth token 需要透過 Secret 注入
4. Pod 重啟時正在執行的 Claude CLI 會被 SIGTERM 殺掉
```

**影響**：中等。Docker image 已包含 Claude CLI，但需要確保：
- `CLAUDE_CODE_OAUTH_TOKEN` 從 K8s Secret 注入
- Graceful shutdown 等待所有子行程完成（preStop hook）

### 6.5 Agent Sandbox → K8s Job

目前 Sandbox 透過 Docker socket 啟動隔離容器。K8s 內：

| 方案 | 優點 | 缺點 |
|------|------|------|
| **K8s Job API** | 原生整合、RBAC 控制 | 啟動延遲高（Cold start ~5-15s） |
| **Sidecar container** | 快速、共享 Volume | 隔離性差 |
| **gVisor (runsc)** | 安全性高 | 相容性問題、效能損失 |
| **Kata Containers** | VM 級隔離 | 重量級、需特殊 runtime |
| **Docker-in-Docker** | 無需改程式碼 | 安全風險大、不建議 |

**建議**：K8s Job API — 與架構最一致，但需要修改 `duduclaw-container` crate 加入 `KubernetesRuntime` backend。

---

## 七、ROI 分析

### 7.1 潛在收益

| 收益 | 適用場景 | 實際需求程度 |
|------|---------|-------------|
| **水平擴展** | 高流量多頻道 | 🟡 低 — 單機已可處理數百 concurrent 連線 |
| **高可用 (HA)** | 7x24 服務 | 🟡 低 — `duduclaw service install` + systemd 已提供基本 HA |
| **滾動更新** | 零停機部署 | 🟡 低 — 單 binary 重啟 < 1 秒 |
| **GPU 排程** | 多模型推理 | 🟠 中 — 但僅 NVIDIA，放棄 Apple Silicon |
| **多租戶** | SaaS 模式 | 🟠 中 — 若未來考慮 SaaS |
| **企業合規** | ISO 27001 等 | 🟡 低 — 個人開發者工具定位 |

### 7.2 投入成本

| Level | 工作量 | 架構風險 | 維護負擔 |
|-------|--------|---------|---------|
| Level 1 | ~1 週 | 低 | 低（Helm chart 維護） |
| Level 2 | ~5 個月 | 🔴 極高 | 🔴 極高（6+ 微服務 + PG + MQ） |
| Level 3 | ~10 個月 | 🔴 極高 | 🔴 極高（+ Operator CRD 版本管理） |

### 7.3 目標用戶分析

DuDuClaw 的定位是 **個人開發者 / 小團隊的 Claude Code 擴充層**：

| 用戶類型 | 佔比（估計） | 需要 K8s？ |
|---------|-------------|-----------|
| 個人開發者（Mac） | 60% | ❌ `brew install` + `duduclaw run` |
| 小團隊（2-10 人） | 25% | ❌ Docker Compose 足夠 |
| 中型團隊（10-50 人） | 10% | 🟡 可能，但 Docker Compose + Tailscale 通常夠用 |
| 企業（50+ 人） | 5% | ✅ 但 DuDuClaw 不是針對企業設計的 |

---

## 八、替代方案

### 8.1 強化 Docker Compose（推薦）

與其追求 K8s，不如強化現有 Docker Compose 部署：

```yaml
# docker-compose.production.yml
services:
  duduclaw:
    image: ghcr.io/lizhixu/duduclaw:latest
    restart: always
    deploy:
      resources:
        limits:
          memory: 4G
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:18789/health"]
      interval: 30s
      retries: 3
    volumes:
      - duduclaw-data:/home/duduclaw/.duduclaw
      - /path/to/models:/home/duduclaw/.duduclaw/models:ro
    env_file: .env

  watchtower:
    image: containrrr/watchtower
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
    command: --interval 3600

  tailscale:
    image: tailscale/tailscale
    # ... 公網暴露 LINE webhook
```

工作量：**1-2 天**，零架構風險。

### 8.2 Fly.io / Railway 部署

對於想要「雲端一鍵部署」的用戶：

```toml
# fly.toml
app = "my-duduclaw"
[build]
  dockerfile = "container/Dockerfile.server"
[mounts]
  source = "duduclaw_data"
  destination = "/home/duduclaw/.duduclaw"
[[services]]
  internal_port = 18789
  protocol = "tcp"
  [services.concurrency]
    hard_limit = 100
```

工作量：**1 天**，提供雲端部署選項。

### 8.3 Nomad（輕量級容器編排）

如果真的需要叢集但不想用 K8s：

| 特性 | Kubernetes | Nomad |
|------|-----------|-------|
| 學習曲線 | 🔴 高 | 🟢 低 |
| 二進位直接排程 | ❌ 需容器化 | ✅ 原生支援 |
| 單機到叢集 | 架構跳躍大 | 平滑過渡 |
| 生態系統 | 🟢 巨大 | 🟡 中等 |
| GPU 排程 | ✅ 成熟 | ✅ 有支援 |
| 維護負擔 | 🔴 高 | 🟢 低 |

---

## 九、建議與結論

### 建議：**僅實施 Level 1，優先強化 Docker Compose**

```
投入產出比排名：
1. 🟢 強化 Docker Compose（1-2 天，覆蓋 85% 用戶）   ← 立即做
2. 🟢 Fly.io 部署範本（1 天，覆蓋雲端需求）           ← 立即做
3. 🟡 Level 1 Helm chart（1 週，覆蓋有 K8s 的用戶）   ← Phase 3+
4. 🔴 Level 2 微服務化（5 個月，破壞核心架構）          ← 不建議
5. 🔴 Level 3 Operator（10 個月，過度工程）             ← 不建議
```

### 理由

1. **定位不符**：DuDuClaw 是個人/小團隊的 Claude Code 擴充，不是企業 SaaS 平台。K8s 是殺雞用牛刀。

2. **單體優勢**：「單一 Rust 二進位 + <1 秒啟動 + ~30MB 記憶體 + 零依賴」是 DuDuClaw 的**核心差異化**之一（見 gap-analysis 第十五項）。K8s 微服務化會直接摧毀這個優勢。

3. **Apple Silicon 不可替代**：DuDuClaw 的本地推理生態（llama.cpp Metal + MLX + Exo P2P）高度依賴 Apple Silicon。K8s 不支援 Apple GPU 排程，會失去最有價值的推理能力。

4. **ROI 極低**：5% 的企業用戶 × P3 優先級 = 不值得投入 5-10 個月重寫核心架構。

5. **Docker Compose 足夠**：搭配 Tailscale/ngrok 解決公網暴露，搭配 Watchtower 解決自動更新，足以覆蓋生產場景。

### 唯一值得做的 K8s 相關工作

如果有少數 K8s 用戶要求，提供一份 **社群維護的 Helm chart**（Level 1）：
- 單 Pod StatefulSet + PVC
- ConfigMap + Secret
- Ingress 範本
- 明確文件說明限制（replicas=1、無 GPU 排程、無 Sandbox）

這只需 **1 週**，且不影響核心架構。

---

## 十、決策矩陣總覽

| 方案 | 工作量 | 架構風險 | ROI | 建議 |
|------|--------|---------|-----|------|
| Docker Compose 強化 | 1-2 天 | ✅ 零 | 🟢 極高 | ✅ **立即做** |
| Fly.io 部署範本 | 1 天 | ✅ 零 | 🟢 高 | ✅ **立即做** |
| Level 1 Helm chart | 1 週 | ✅ 低 | 🟡 中 | 🟡 Phase 3+ 視需求 |
| Level 2 微服務化 | 5 個月 | 🔴 極高 | 🔴 低 | ❌ **不建議** |
| Level 3 Operator | 10 個月 | 🔴 極高 | 🔴 極低 | ❌ **不建議** |
| Nomad 替代 | 2-3 週 | 🟡 中 | 🟡 中 | 🟡 若叢集需求確認 |

---

## 附錄 A：若未來 SaaS 化的最小路徑

若 DuDuClaw 未來決定轉型 SaaS，建議的最小路徑不是 K8s，而是：

1. **SQLite → Turso (libSQL)**：分散式 SQLite，保持 API 相容，無需重寫 → 2 週
2. **bus_queue.jsonl → Turso 表**：利用 libSQL 的 embedded replica → 1 週
3. **多租戶隔離**：每個租戶一個 Turso database → 1 週
4. **部署到 Fly.io**：每個租戶一個 Fly Machine → 1 週

總計 **5 週**，遠優於 K8s 的 5-10 個月，且保持「單 binary」優勢。
