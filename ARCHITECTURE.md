# DuDuClaw 系統架構設計

> 版本：0.1.0-draft
> 日期：2026-03-15

---

## 目錄

1. [設計決策](#一設計決策)
2. [多 Agent 架構](#二多-agent-架構)
3. [系統總架構](#三系統總架構)
4. [Rust 核心層](#四rust-核心層)
5. [Python 擴充層](#五python-擴充層)
6. [安全系統](#六安全系統)
7. [OpenClaw 相容 API](#七openclaw-相容-api)
8. [Claude Code SDK 多帳號輪替](#八claude-code-sdk-多帳號輪替)
9. [自主提升引擎](#九自主提升引擎)
10. [專案結構](#十專案結構)
11. [安裝與部署系統](#十一安裝與部署系統)
12. [實作路線圖](#十二實作路線圖)

---

## 一、設計決策

| 決策項目 | 選擇 | 理由 |
|----------|------|------|
| 核心語言 | Rust | 安全關鍵路徑（Gateway、容器、憑證代理）需要記憶體安全 |
| 擴充語言 | Python (PyO3) | Claude Code SDK 官方支援、通道插件開發門檻低 |
| 容器引擎 | Docker + Apple Container + WSL2 | 跨平台支援（macOS/Linux/Windows） |
| 部署目標 | 純伺服器（第一階段） | 降低複雜度 |
| 支援平台 | macOS / Linux / Windows | Rust 跨平台編譯 + 條件編譯 |
| 首批通道 | LINE + Telegram + Discord | 台灣市場 + 國際社群 |
| 技能安全 | 自動啟用 + 安全掃描 | 平衡自主性與安全性 |
| API 相容 | OpenClaw WebSocket RPC | 可直接使用 OpenClaw 生態客戶端 |
| 模型 SDK | Claude Code SDK (Python) | 多帳號輪替、OAuth/API Key 混合 |

---

## 二、多 Agent 架構

### 2.1 設計哲學

研究了三種多 Agent 模式後的取捨：

| 模式 | 代表專案 | 優點 | 缺點 |
|------|----------|------|------|
| **資料夾 + SOUL.md** | TaiwanClaw/PicoClaw | 簡單直覺，零學習曲線 | 無動態管理、無權限隔離 |
| **資料庫 + 心跳驅動** | Paperclip | 企業級治理、預算控管 | 過度工程、不適合個人助理 |
| **群組隔離 + IPC** | NanoClaw | 安全隔離好、容器化 | 群組概念抽象、不夠靈活 |

**DuDuClaw 的選擇：資料夾為基礎 + 資料庫索引 + 心跳驅動的混合模式**

取 TaiwanClaw 的直覺性（資料夾 + SOUL.md）、Paperclip 的心跳協議與預算控管、NanoClaw 的安全隔離。

### 2.2 Agent 目錄結構

```
~/.duduclaw/
├── config.toml                     # 全域設定
├── secret.key                      # 主加密金鑰
├── agents/                         # Agent 註冊目錄
│   ├── _defaults/                  # 全域預設值（所有 Agent 繼承）
│   │   ├── SOUL.md                 # 預設人格基底
│   │   ├── TOOLS.md                # 預設工具指南
│   │   └── SKILLS/                 # 全域技能
│   │
│   ├── dudu/                       # 主 Agent（isMain = true）
│   │   ├── agent.toml              # Agent 設定（見下方）
│   │   ├── SOUL.md                 # 「我是 DuDu，一個溫暖的 AI 助理...」
│   │   ├── IDENTITY.md             # 身份定義（名稱、觸發詞、語言）
│   │   ├── MEMORY.md               # 長期記憶
│   │   ├── HEARTBEAT.md            # 心跳任務
│   │   ├── TOOLS.md                # 工具使用指南（覆蓋 _defaults）
│   │   ├── SKILLS/                 # Agent 專屬技能
│   │   │   ├── research.md
│   │   │   └── coding.md
│   │   ├── sessions/               # 對話歷史
│   │   ├── memory/                 # 每日筆記
│   │   │   └── 202603/
│   │   │       └── 20260315.md
│   │   └── state/                  # 運行時狀態
│   │       └── state.db            # SQLite
│   │
│   ├── coder/                      # 編碼專用 Agent
│   │   ├── agent.toml
│   │   ├── SOUL.md                 # 「我是嚴謹的軟體工程師...」
│   │   ├── IDENTITY.md
│   │   ├── MEMORY.md
│   │   ├── SKILLS/
│   │   │   ├── rust-patterns.md
│   │   │   └── code-review.md
│   │   └── ...
│   │
│   └── researcher/                 # 研究專用 Agent
│       ├── agent.toml
│       ├── SOUL.md                 # 「我是深度研究分析師...」
│       └── ...
```

### 2.3 Agent 設定檔 (`agent.toml`)

```toml
[agent]
name = "dudu"
display_name = "DuDu"
role = "main"                       # main | specialist | worker
status = "active"                   # active | paused | terminated
trigger = "@DuDu"                   # 通道內的觸發詞
reports_to = ""                     # 空 = 頂層 Agent（無上級）
icon = "🐾"

[agent.model]
preferred = "claude-sonnet-4-6"     # 預設模型
fallback = "claude-haiku-4-5"       # 降級模型
account_pool = ["main", "backup"]   # 帳號池引用（對應 config.toml 的帳號）

[agent.container]
timeout_ms = 1_800_000              # 30 分鐘
max_concurrent = 3                  # 該 Agent 最大並行容器
readonly_project = true             # 專案目錄唯讀掛載
additional_mounts = [
    { host = "~/projects", container = "projects", readonly = false },
]

[agent.heartbeat]
enabled = true
interval_seconds = 3600             # 每小時
max_concurrent_runs = 1
cron = ""                           # 或用 cron 表達式覆蓋 interval

[agent.budget]
monthly_limit_cents = 10000         # $100 USD / 月
warn_threshold_percent = 80         # 80% 時告警
hard_stop = true                    # 到限額自動暫停

[agent.permissions]
can_create_agents = false           # 僅 main Agent 可為 true
can_send_cross_agent = true         # 可向其他 Agent 發訊息
can_modify_own_skills = true        # 可自我修改技能
can_modify_own_soul = false         # 不可自我修改人格（安全護欄）
can_schedule_tasks = true
allowed_channels = ["*"]            # 或限制特定通道 ["line", "telegram"]

[agent.evolution]
micro_reflection = true             # 每次對話後反思
meso_reflection = true              # 心跳時反思
macro_reflection = true             # 每日反思
skill_auto_activate = true          # 自產技能自動啟用
skill_security_scan = true          # 啟用前安全掃描
```

### 2.4 Agent 路由與分派

```
                     訊息進入
                        │
                        ▼
              ┌─────────────────┐
              │  Trigger Router  │
              │                 │
              │ "@DuDu 幫我..."  │ → dudu agent
              │ "@Coder 寫個..." │ → coder agent
              │ "幫我研究..."    │ → dudu agent (預設)
              └────────┬────────┘
                       │
                       ▼
              ┌─────────────────┐
              │  Agent Resolver  │
              │                 │
              │ 1. 觸發詞匹配   │
              │ 2. 通道綁定檢查  │
              │ 3. 預設 Agent    │
              │ 4. 權限驗證     │
              └────────┬────────┘
                       │
                       ▼
              ┌─────────────────┐
              │  Agent Runtime   │
              │                 │
              │ 載入 agent.toml  │
              │ 載入 SOUL.md     │
              │ 載入 IDENTITY.md │
              │ 載入 SKILLS/     │
              │ 載入 MEMORY.md   │
              │ 選擇帳號        │
              │ 啟動容器        │
              └─────────────────┘
```

### 2.5 Agent 間通訊

採用 **IPC 訊息佇列**（非直接呼叫），確保隔離性：

```
Agent A (容器內)
  │
  │ 寫入 IPC JSON 檔案
  │ { "type": "delegate", "target_agent": "coder", "prompt": "..." }
  │
  ▼
主機 IPC Watcher
  │
  │ 驗證權限：A.can_send_cross_agent == true
  │ 驗證目標：coder 存在且 status == active
  │
  ▼
Agent B (coder) 被喚醒
  │
  │ 執行完成後寫入回應
  │ { "type": "delegate_response", "source_agent": "coder", "result": "..." }
  │
  ▼
Agent A 的下一次心跳或輪詢中收到回應
```

### 2.6 動態 Agent 管理

主 Agent（`role = "main"`）擁有 `can_create_agents = true` 權限，可透過工具動態管理 Agent：

| MCP 工具 | 功能 | 權限要求 |
|----------|------|----------|
| `agent_list` | 列出所有 Agent 及狀態 | 任何 Agent |
| `agent_create` | 建立新 Agent（寫入資料夾 + agent.toml） | `can_create_agents` |
| `agent_pause` / `agent_resume` | 暫停/恢復 Agent | main 或 reports_to |
| `agent_delegate` | 委派任務給其他 Agent | `can_send_cross_agent` |
| `agent_status` | 查看特定 Agent 的狀態與預算 | 任何 Agent |

**安全護欄**：
- Agent 無法自行修改 `can_create_agents` 和 `can_modify_own_soul` 權限
- Agent 無法將自己設為 `role = "main"`
- 新建 Agent 的權限不能超過建立者的權限（權限繼承上限）

---

## 三、系統總架構

```
┌─────────────────────────────────────────────────────────────────┐
│                        DuDuClaw Server                          │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │                   Rust Core Layer                         │  │
│  │                                                           │  │
│  │  ┌─────────────┐  ┌──────────────┐  ┌─────────────────┐  │  │
│  │  │  Gateway     │  │ Message Bus  │  │  Agent Registry │  │  │
│  │  │  (Axum)      │  │ (tokio)      │  │  (索引+路由)    │  │  │
│  │  │             │  │              │  │                 │  │  │
│  │  │ • WS RPC    │  │ • broadcast  │  │ • 資料夾掃描    │  │  │
│  │  │ • HTTP API  │  │ • mpsc       │  │ • 熱重載       │  │  │
│  │  │ • OpenClaw  │  │ • 事件分發    │  │ • 觸發詞路由    │  │  │
│  │  │   相容      │  │              │  │ • 權限檢查     │  │  │
│  │  └──────┬──────┘  └──────┬───────┘  └────────┬────────┘  │  │
│  │         │                │                   │            │  │
│  │  ┌──────▼────────────────▼───────────────────▼────────┐   │  │
│  │  │                Security Layer                       │   │  │
│  │  │  ┌──────────┐ ┌───────────┐ ┌──────────┐ ┌──────┐ │   │  │
│  │  │  │Credential│ │  Mount    │ │   RBAC   │ │ Rate │ │   │  │
│  │  │  │  Proxy   │ │  Guard   │ │  Engine  │ │Limit │ │   │  │
│  │  │  └──────────┘ └───────────┘ └──────────┘ └──────┘ │   │  │
│  │  └────────────────────────────────────────────────────┘   │  │
│  │                                                           │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐  │  │
│  │  │Memory Engine │  │Container Mgr │  │  Heartbeat     │  │  │
│  │  │SQLite+Vec    │  │Docker/Apple  │  │  Scheduler     │  │  │
│  │  │+FTS5         │  │Container     │  │  (cron+interval│  │  │
│  │  │              │  │              │  │   +once)       │  │  │
│  │  └──────────────┘  └──────┬───────┘  └────────────────┘  │  │
│  │                           │                               │  │
│  └───────────────────────────┼───────────────────────────────┘  │
│                              │                                  │
│  ┌───────────────────────────▼───────────────────────────────┐  │
│  │              Python Extension Layer (PyO3)                │  │
│  │                                                           │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐  │  │
│  │  │  Channels    │  │  Evolution   │  │  Claude SDK    │  │  │
│  │  │  • LINE      │  │  Engine      │  │  Rotator       │  │  │
│  │  │  • Telegram  │  │  • Micro     │  │  • 多帳號輪替  │  │  │
│  │  │  • Discord   │  │  • Meso      │  │  • OAuth/Key   │  │  │
│  │  │  • (插件式)  │  │  • Macro     │  │  • 健康檢查    │  │  │
│  │  └──────────────┘  │  • Vetter    │  │  • 預算追蹤    │  │  │
│  │                    └──────────────┘  └────────────────┘  │  │
│  │                                                           │  │
│  │  ┌──────────────────────────────────────────────────────┐ │  │
│  │  │              Container Runtime (Agent)                │ │  │
│  │  │  ┌────────────┐ ┌─────────┐ ┌────────────────────┐  │ │  │
│  │  │  │Claude Code │ │  Tool   │ │  Skill Loader      │  │ │  │
│  │  │  │  SDK       │ │Executor │ │  + Self-Evolution   │  │ │  │
│  │  │  │(Agent Loop)│ │         │ │  + Skill Vetter     │  │ │  │
│  │  │  └────────────┘ └─────────┘ └────────────────────┘  │ │  │
│  │  └──────────────────────────────────────────────────────┘ │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 四、Rust 核心層

### 4.1 Crate 職責

| Crate | 職責 | 關鍵依賴 |
|-------|------|----------|
| `duduclaw-core` | 共用型別、trait 定義、錯誤型別 | serde, thiserror |
| `duduclaw-gateway` | WebSocket RPC + HTTP API (OpenClaw 相容) | axum, tokio-tungstenite |
| `duduclaw-bus` | 訊息路由、事件分發 | tokio (broadcast, mpsc) |
| `duduclaw-security` | CredentialProxy, MountGuard, RBAC, RateLimiter | rcgen, ring (crypto) |
| `duduclaw-memory` | 向量 + 全文混合搜尋記憶引擎 | rusqlite, sqlite-vec |
| `duduclaw-container` | Docker + Apple Container 生命週期管理 | bollard (Docker API) |
| `duduclaw-agent` | Agent 註冊、路由、心跳排程 | cron (Rust crate) |
| `duduclaw-bridge` | PyO3 橋接層 | pyo3 |
| `duduclaw-cli` | CLI 入口 (onboard, agent, gateway, status) | clap |

### 4.2 核心 Trait 定義

```rust
// duduclaw-core/src/traits.rs

/// 通道抽象 — 由 Python 層實作
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn connect(&self) -> Result<()>;
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
    fn is_connected(&self) -> bool;
    fn owns_chat_id(&self, chat_id: &str) -> bool;
}

/// 容器運行時抽象
#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId>;
    async fn start(&self, id: &ContainerId) -> Result<()>;
    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()>;
    async fn remove(&self, id: &ContainerId) -> Result<()>;
    async fn logs(&self, id: &ContainerId) -> Result<String>;
    async fn health_check(&self) -> Result<RuntimeHealth>;
}

/// 記憶體引擎抽象
#[async_trait]
pub trait MemoryEngine: Send + Sync {
    async fn store(&self, agent_id: &str, entry: MemoryEntry) -> Result<()>;
    async fn search(&self, agent_id: &str, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn summarize(&self, agent_id: &str, window: TimeWindow) -> Result<String>;
}
```

### 4.3 Container Runtime 抽象

```rust
// duduclaw-container/src/lib.rs

pub enum RuntimeBackend {
    Docker(DockerRuntime),
    AppleContainer(AppleContainerRuntime),
    Wsl2(Wsl2Runtime),
}

impl RuntimeBackend {
    pub fn detect() -> Self {
        if cfg!(target_os = "macos") && Self::apple_container_available() {
            RuntimeBackend::AppleContainer(AppleContainerRuntime::new())
        } else if cfg!(target_os = "windows") && Self::wsl2_available() {
            // Windows: 優先偵測 WSL2 直接執行，回退至 Docker Desktop
            RuntimeBackend::Wsl2(Wsl2Runtime::new())
        } else {
            RuntimeBackend::Docker(DockerRuntime::new())
        }
    }
}

// Docker 透過 bollard crate (Docker Engine API)
// 全平台支援：Linux 原生、macOS Docker Desktop、Windows Docker Desktop (WSL2 backend)
pub struct DockerRuntime {
    client: bollard::Docker,
}

// Apple Container 透過 `container` CLI 指令操作 (macOS 26+)
pub struct AppleContainerRuntime {
    binary_path: PathBuf,  // /usr/bin/container
}

// WSL2 Direct: 在 Windows 上透過 WSL2 直接執行 Linux 容器
// 不需要 Docker Desktop 授權，適合企業環境
pub struct Wsl2Runtime {
    distro: String,         // 預設 "Ubuntu-24.04"
    wsl_binary: PathBuf,    // C:\Windows\System32\wsl.exe
}

impl Wsl2Runtime {
    /// 透過 wsl.exe 在指定 distro 中執行容器化 Agent
    /// 流程：wsl -d Ubuntu-24.04 -- docker run ...
    /// 或使用 WSL2 原生 namespace 隔離（無需 Docker）
    pub fn new() -> Self {
        let distro = Self::detect_best_distro()
            .unwrap_or_else(|| "Ubuntu-24.04".to_string());
        Self {
            distro,
            wsl_binary: PathBuf::from(r"C:\Windows\System32\wsl.exe"),
        }
    }

    fn detect_best_distro() -> Option<String> {
        // 執行 `wsl -l -v` 解析已安裝的 distro
        // 優先選擇已安裝 Docker 的 distro
        todo!()
    }
}
```

---

## 五、Python 擴充層

### 5.1 PyO3 橋接

```python
# python/duduclaw/__init__.py
import duduclaw._native as _native  # PyO3 編譯的 Rust 模組

# Rust 暴露給 Python 的核心 API
_native.start_gateway(config_path: str) -> None
_native.send_to_bus(message: dict) -> None
_native.query_memory(agent_id: str, query: str, limit: int) -> list[dict]
_native.validate_mount(host_path: str, container_path: str) -> bool
_native.get_credential(account_id: str) -> str  # 僅限 Credential Proxy 內部使用
```

### 5.2 通道插件介面

```python
# python/duduclaw/channels/base.py
from abc import ABC, abstractmethod

class ChannelPlugin(ABC):
    """所有通道插件的基底類別"""

    @property
    @abstractmethod
    def name(self) -> str: ...

    @abstractmethod
    async def connect(self) -> None: ...

    @abstractmethod
    async def send_message(self, chat_id: str, text: str) -> None: ...

    @abstractmethod
    async def disconnect(self) -> None: ...

    def on_message_received(self, chat_id: str, sender: str, text: str):
        """收到訊息時呼叫，自動路由到 Rust Bus"""
        _native.send_to_bus({
            "type": "incoming",
            "channel": self.name,
            "chat_id": chat_id,
            "sender": sender,
            "text": text,
        })

# python/duduclaw/channels/line.py
class LineChannel(ChannelPlugin):
    name = "line"
    # LINE Messaging API 實作...

# python/duduclaw/channels/telegram.py
class TelegramChannel(ChannelPlugin):
    name = "telegram"
    # python-telegram-bot 實作...

# python/duduclaw/channels/discord.py
class DiscordChannel(ChannelPlugin):
    name = "discord"
    # discord.py 實作...
```

### 5.3 通道自動發現

```python
# python/duduclaw/channels/registry.py
import importlib
import pkgutil

def discover_channels() -> dict[str, type[ChannelPlugin]]:
    """自動掃描 channels/ 目錄，發現所有插件"""
    channels = {}
    package = importlib.import_module("duduclaw.channels")
    for _, module_name, _ in pkgutil.iter_modules(package.__path__):
        module = importlib.import_module(f"duduclaw.channels.{module_name}")
        for attr in dir(module):
            cls = getattr(module, attr)
            if isinstance(cls, type) and issubclass(cls, ChannelPlugin) and cls is not ChannelPlugin:
                channels[cls.name] = cls
    return channels
```

---

## 六、安全系統

### 6.1 安全架構（對標 NanoClaw）

```
┌─────────────────────────────────────────┐
│            Security Layer               │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │ 1. Credential Proxy (Rust)       │  │
│  │    • 真實金鑰永遠不進容器         │  │
│  │    • 常數時間比對（防計時攻擊）    │  │
│  │    • 支援 OAuth + API Key 混合    │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │ 2. Mount Guard (Rust)            │  │
│  │    • 白名單在容器外               │  │
│  │      (~/.config/duduclaw/)        │  │
│  │    • 自動阻擋 .ssh/.gnupg/.env   │  │
│  │    • Symlink 解析防穿越           │  │
│  │    • 非主 Agent 強制唯讀          │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │ 3. Agent Permission Engine       │  │
│  │    • 權限繼承上限（不可超越建立者）│  │
│  │    • SOUL.md 防自我修改           │  │
│  │    • IPC 跨 Agent 權限驗證        │  │
│  │    • 預算硬停機制                 │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │ 4. Skill Vetter (Python)         │  │
│  │    • Agent 自產技能安全掃描       │  │
│  │    • 檢測：命令注入、路徑穿越     │  │
│  │    •       prompt injection      │  │
│  │    •       資源濫用指令           │  │
│  │    • 掃描通過 → 自動啟用          │  │
│  │    • 掃描失敗 → 隔離 + 通知       │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │ 5. Rate Limiter (Rust)           │  │
│  │    • 每 Agent 獨立限流            │  │
│  │    • 每通道獨立限流               │  │
│  │    • 滑動視窗演算法               │  │
│  │    • 全域安全閥（防失控）         │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │ 6. Container Hardening           │  │
│  │    • 非 root 運行 (uid 1000)     │  │
│  │    • 唯讀掛載專案目錄             │  │
│  │    • .env 以 /dev/null 覆蓋      │  │
│  │    • 一次性容器 (--rm)            │  │
│  │    • 網路存取可配置限制           │  │
│  └───────────────────────────────────┘  │
└─────────────────────────────────────────┘
```

### 6.2 安全設定檔位置

```
~/.config/duduclaw/                    # 容器外！Agent 無法觸及
├── mount-allowlist.json               # 掛載白名單
├── sender-allowlist.json              # 發送者白名單
├── skill-quarantine/                  # 被隔離的可疑技能
└── audit.log                          # 安全稽核日誌
```

---

## 七、OpenClaw 相容 API

### 7.1 WebSocket RPC 協定

完全相容 OpenClaw 的三種訊息框架：

```rust
// duduclaw-gateway/src/protocol.rs

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsFrame {
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        params: serde_json::Value,
    },

    #[serde(rename = "res")]
    Response {
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<serde_json::Value>,
    },

    #[serde(rename = "event")]
    Event {
        event: String,
        payload: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        state_version: Option<u64>,
    },
}
```

### 7.2 相容的 Method 清單

| Method | 狀態 | 說明 |
|--------|------|------|
| `connect.challenge` | Phase 1 | Ed25519 Challenge |
| `connect` | Phase 1 | 認證握手 |
| `hello-ok` | Phase 1 | 握手完成 |
| `tools.catalog` | Phase 1 | 工具目錄 |
| `skills.bins` | Phase 2 | 技能清單 |
| `exec.approval.resolve` | Phase 2 | 執行核准 |
| `device.token.rotate` | Phase 2 | Token 輪換 |
| `device.token.revoke` | Phase 2 | Token 撤銷 |

### 7.3 DuDuClaw 擴充 Method

| Method | 說明 |
|--------|------|
| `agents.list` | 列出所有 Agent |
| `agents.status` | Agent 詳細狀態（含預算） |
| `agents.create` | 動態建立 Agent |
| `agents.delegate` | 委派任務 |
| `evolution.status` | 自主提升狀態 |
| `evolution.skills` | 自產技能清單 |

---

## 八、Claude Code SDK 多帳號輪替

### 8.1 帳號池設定

```toml
# config.toml

[[accounts]]
id = "main"
type = "api_key"                         # api_key | oauth
api_key_encrypted = "enc:aes256:..."     # 加密存儲
monthly_budget_cents = 5000              # $50
priority = 1                             # 越小越優先

[[accounts]]
id = "backup"
type = "oauth"
oauth_token_encrypted = "enc:aes256:..."
monthly_budget_cents = 3000
priority = 2

[[accounts]]
id = "heavy"
type = "api_key"
api_key_encrypted = "enc:aes256:..."
monthly_budget_cents = 20000
priority = 3
tags = ["opus-only"]                     # 僅用於需要 Opus 的任務

[rotation]
strategy = "least_cost"                  # round_robin | least_cost | failover | priority
health_check_interval_seconds = 60
cooldown_after_rate_limit_seconds = 120
```

### 8.2 輪替策略

```python
# python/duduclaw/sdk/rotator.py

class RotationStrategy(Enum):
    ROUND_ROBIN = "round_robin"       # 輪流使用
    LEAST_COST = "least_cost"         # 優先用花費最少的
    FAILOVER = "failover"             # 主帳號失敗才切換
    PRIORITY = "priority"             # 按優先級排序

class AccountRotator:
    def select_account(self, request_context: RequestContext) -> Account:
        available = [a for a in self.accounts if a.is_healthy and not a.budget_exceeded]

        if not available:
            raise AllAccountsExhausted()

        match self.strategy:
            case RotationStrategy.ROUND_ROBIN:
                return available[self._round_robin_index % len(available)]
            case RotationStrategy.LEAST_COST:
                return min(available, key=lambda a: a.spent_this_month)
            case RotationStrategy.FAILOVER:
                return sorted(available, key=lambda a: a.priority)[0]
            case RotationStrategy.PRIORITY:
                # 考慮 tags 匹配
                tagged = [a for a in available if request_context.matches_tags(a.tags)]
                pool = tagged if tagged else available
                return sorted(pool, key=lambda a: a.priority)[0]

    def on_rate_limited(self, account: Account, retry_after: int):
        account.cooldown_until = now() + timedelta(seconds=retry_after)
        logger.warning(f"Account {account.id} rate limited, cooldown {retry_after}s")

    def on_error(self, account: Account, error: Exception):
        account.consecutive_errors += 1
        if account.consecutive_errors >= 3:
            account.is_healthy = False
            logger.error(f"Account {account.id} marked unhealthy after 3 consecutive errors")
```

---

## 九、自主提升引擎

### 9.1 三層反思系統

```
┌────────────────────────────────────────────────────────┐
│                 Self-Evolution Engine                    │
│                                                        │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Layer 1: Micro Reflection (每次對話結束)         │  │
│  │                                                  │  │
│  │ 觸發：Agent Loop 完成一輪對話                    │  │
│  │ 動作：                                           │  │
│  │   1. 「這次對話我做得好的/做得不好的是什麼？」    │  │
│  │   2. 寫入 memory/daily/YYYYMMDD.md              │  │
│  │   3. 若發現新模式 → 候選技能佇列                 │  │
│  │ 成本：低（幾百 token 的 self-prompt）            │  │
│  └──────────────────────────────────────────────────┘  │
│                                                        │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Layer 2: Meso Reflection (心跳觸發，每小時)      │  │
│  │                                                  │  │
│  │ 觸發：Heartbeat Scheduler                        │  │
│  │ 動作：                                           │  │
│  │   1. 讀取最近的 daily notes                      │  │
│  │   2. 「過去幾次對話有什麼共同模式？」             │  │
│  │   3. 更新 MEMORY.md（長期知識萃取）              │  │
│  │   4. 若模式穩定 → 產生候選 SKILL.md             │  │
│  │ 成本：中                                         │  │
│  └──────────────────────────────────────────────────┘  │
│                                                        │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Layer 3: Macro Reflection (排程觸發，每日一次)   │  │
│  │                                                  │  │
│  │ 觸發：Cron (每日固定時間，如凌晨 3 點)          │  │
│  │ 動作：                                           │  │
│  │   1. 回顧所有 SKILLS/ 的使用頻率與效果           │  │
│  │   2. 「哪些技能需要改進？哪些該淘汰？」          │  │
│  │   3. 審視 SOUL.md 與實際行為的落差               │  │
│  │   4. 產出《每日進化報告》→ 通知使用者            │  │
│  │ 成本：高（完整的自我審計）                       │  │
│  └──────────────────────────────────────────────────┘  │
│                                                        │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Skill Vetter (安全掃描，技能啟用前必經)          │  │
│  │                                                  │  │
│  │ 掃描項目：                                       │  │
│  │   • 命令注入模式 (rm, curl | sh, eval)           │  │
│  │   • 路徑穿越 (../, /etc/, ~/)                   │  │
│  │   • Prompt injection 指標                        │  │
│  │   • 資源濫用 (無限迴圈、大量 API 呼叫)           │  │
│  │   • 敏感資料存取 (密碼、金鑰模式)                │  │
│  │                                                  │  │
│  │ 結果：                                           │  │
│  │   ✅ PASS → 自動啟用，寫入 SKILLS/               │  │
│  │   ⚠️ WARN → 啟用但標記，通知使用者               │  │
│  │   ❌ FAIL → 隔離至 skill-quarantine/，通知使用者  │  │
│  └──────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────┘
```

### 9.2 技能生命週期

```
候選技能（Agent 自產）
    │
    ▼
Skill Vetter 安全掃描
    │
    ├── ✅ PASS ──→ SKILLS/{name}.md（啟用）
    │                    │
    │                    ▼
    │              使用頻率追蹤
    │                    │
    │              ┌─────┴─────┐
    │              │           │
    │         高頻使用    長期未用
    │              │           │
    │         Macro 反思   Macro 反思
    │          改進技能     淘汰技能
    │              │           │
    │              ▼           ▼
    │         更新版本    移至 archive/
    │
    ├── ⚠️ WARN ──→ SKILLS/{name}.md（標記 ⚠️）
    │
    └── ❌ FAIL ──→ ~/.config/duduclaw/skill-quarantine/
```

---

## 十、專案結構

```
duduclaw/
├── Cargo.toml                         # Rust workspace root
├── Cargo.lock
├── pyproject.toml                     # Python 套件設定 (maturin)
├── config/
│   ├── duduclaw.example.toml          # 設定範例
│   └── mount-allowlist.example.json   # 掛載白名單範例
│
├── crates/                            # Rust crates
│   ├── duduclaw-core/                 # 共用型別、trait、錯誤
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs               # AgentConfig, Message, etc.
│   │       ├── traits.rs              # Channel, ContainerRuntime, MemoryEngine
│   │       └── error.rs
│   │
│   ├── duduclaw-gateway/              # WebSocket RPC Gateway
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── server.rs              # Axum server setup
│   │       ├── protocol.rs            # WsFrame, OpenClaw 相容
│   │       ├── auth.rs                # Ed25519 Challenge-Response
│   │       └── handlers.rs            # Method handlers
│   │
│   ├── duduclaw-bus/                  # Message Bus
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── bus.rs                 # tokio broadcast + mpsc
│   │       └── router.rs             # 觸發詞路由
│   │
│   ├── duduclaw-security/             # 安全層
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── credential_proxy.rs    # 憑證代理 HTTP server
│   │       ├── mount_guard.rs         # 掛載路徑驗證
│   │       ├── rbac.rs               # 權限引擎
│   │       ├── rate_limiter.rs        # 滑動視窗限流
│   │       └── crypto.rs             # AES-256 加密工具
│   │
│   ├── duduclaw-memory/               # 記憶引擎
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs             # SQLite + Vec + FTS5
│   │       ├── embedding.rs          # 向量嵌入
│   │       └── search.rs             # 混合搜尋
│   │
│   ├── duduclaw-container/            # 容器管理
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── docker.rs             # Docker (bollard) — 全平台
│   │       ├── apple.rs              # Apple Container CLI — macOS
│   │       ├── wsl2.rs               # WSL2 Direct — Windows
│   │       └── lifecycle.rs          # 建立、啟動、停止、清理
│   │
│   ├── duduclaw-agent/                # Agent 管理
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── registry.rs           # 資料夾掃描 + 熱重載
│   │       ├── resolver.rs           # 訊息 → Agent 路由
│   │       ├── heartbeat.rs          # 心跳排程器
│   │       └── ipc.rs               # Agent 間 IPC
│   │
│   ├── duduclaw-bridge/               # PyO3 橋接
│   │   └── src/
│   │       └── lib.rs                # #[pymodule] 定義
│   │
│   └── duduclaw-cli/                  # CLI 入口
│       └── src/
│           ├── main.rs               # clap: onboard, agent, gateway, status
│           └── service/
│               ├── mod.rs            # 平台偵測 + 統一介面
│               ├── systemd.rs        # Linux systemd service
│               ├── launchd.rs        # macOS launchd plist
│               └── windows.rs        # Windows Service (windows-service crate)
│
├── python/                            # Python 擴充層
│   └── duduclaw/
│       ├── __init__.py
│       ├── channels/                  # 通道插件
│       │   ├── __init__.py
│       │   ├── base.py               # ChannelPlugin ABC
│       │   ├── registry.py           # 自動發現
│       │   ├── line.py               # LINE Messaging API
│       │   ├── telegram.py           # python-telegram-bot
│       │   └── discord.py            # discord.py
│       ├── sdk/                       # Claude Code SDK 整合
│       │   ├── __init__.py
│       │   ├── rotator.py            # 多帳號輪替
│       │   ├── account.py            # 帳號模型
│       │   └── health.py             # 健康檢查
│       ├── evolution/                 # 自主提升引擎
│       │   ├── __init__.py
│       │   ├── micro.py              # Micro Reflection
│       │   ├── meso.py               # Meso Reflection
│       │   ├── macro_.py             # Macro Reflection (避免 Python 保留字)
│       │   └── vetter.py             # Skill Vetter 安全掃描
│       └── tools/                     # MCP 工具
│           ├── __init__.py
│           ├── agent_tools.py         # agent_list, agent_create, agent_delegate
│           ├── file_tools.py          # read, write, edit, append, list_dir
│           ├── exec_tools.py          # shell 執行
│           ├── search_tools.py        # web_search, web_fetch
│           ├── schedule_tools.py      # cron, interval, once
│           └── message_tools.py       # send_message
│
├── container/                         # 容器映像
│   ├── Dockerfile.agent               # Agent 執行環境
│   ├── Dockerfile.sandbox             # 沙箱（限制更多）
│   └── apple-container.yaml           # Apple Container 設定
│
├── tests/                             # 整合測試
│   ├── rust/                          # Rust 整合測試
│   └── python/                        # Python 測試
│
└── docs/                              # 文件
    ├── getting-started.md
    ├── configuration.md
    ├── multi-agent.md
    ├── security.md
    ├── api-reference.md
    └── self-evolution.md
```

---

## 十一、安裝與部署系統

### 設計原則

參考 Paperclip 的漸進式複雜度理念，DuDuClaw 提供三層安裝體驗：

```
零設定一行安裝                  互動式精靈                    進階手動設定
(duduclaw run --yes)     →    (duduclaw onboard)      →    (config.toml 手動編輯)
     5 秒上手                    2 分鐘引導                    完全掌控
```

---

### 11.1 一行安裝（三種方式）

#### Shell 一鍵安裝（推薦）

```bash
curl -fsSL https://install.duduclaw.dev | sh
```

安裝腳本行為：
1. 偵測平台（macOS ARM64 / Linux x86_64 / Linux ARM64 / Windows x86_64）
2. 下載預編譯二進位檔至 `~/.duduclaw/bin/duduclaw`（Windows: `%USERPROFILE%\.duduclaw\bin\duduclaw.exe`）
3. 加入 `$PATH`（寫入 `.bashrc` / `.zshrc`；Windows: 修改使用者 PATH 環境變數）
4. 安裝 Python 擴充套件（`pip install duduclaw`）
5. 提示使用者執行 `duduclaw onboard`

#### PowerShell 一鍵安裝（Windows）

```powershell
irm https://install.duduclaw.dev/win | iex
```

安裝腳本行為：
1. 偵測 Windows 架構（x86_64 / ARM64）
2. 下載 `duduclaw.exe` 至 `%USERPROFILE%\.duduclaw\bin\`
3. 將路徑加入使用者 PATH（`[Environment]::SetEnvironmentVariable`）
4. 偵測並安裝 Python 擴充（`pip install duduclaw`）
5. 偵測容器引擎（Docker Desktop / WSL2）
6. 提示使用者執行 `duduclaw onboard`

#### winget（Windows 套件管理器）

```powershell
winget install DuDuClaw.DuDuClaw
```

#### Scoop（Windows 進階使用者）

```powershell
scoop bucket add duduclaw https://github.com/duduclaw/scoop-bucket
scoop install duduclaw
```

#### Homebrew（macOS / Linux）

```bash
brew install duduclaw/tap/duduclaw
```

#### Cargo + pip（全平台開發者）

```bash
cargo install duduclaw-cli
pip install duduclaw
```

---

### 11.2 互動式設定精靈（`duduclaw onboard`）

使用 Rust TUI 框架 **ratatui + crossterm** 建構終端互動介面（對標 Paperclip 的 @clack/prompts）。

#### 精靈流程

```
duduclaw onboard
│
├── 1. 歡迎畫面
│      ┌─────────────────────────────────────┐
│      │  🐾 歡迎使用 DuDuClaw v0.1.0       │
│      │                                     │
│      │  選擇安裝模式：                      │
│      │  ● 快速啟動（推薦）— 使用預設值      │
│      │  ○ 進階設定 — 完整互動式設定          │
│      └─────────────────────────────────────┘
│
├── 2. Claude API 設定
│      ┌─────────────────────────────────────┐
│      │  設定 Claude 帳號：                   │
│      │                                     │
│      │  認證方式：                           │
│      │  ● API Key                          │
│      │  ○ OAuth Token                      │
│      │  ○ 稍後設定                          │
│      │                                     │
│      │  API Key: ····················       │
│      │  ✓ 金鑰驗證成功                      │
│      │                                     │
│      │  是否加入更多帳號（輪替用）？         │
│      │  ○ 是  ● 否                         │
│      └─────────────────────────────────────┘
│
├── 3. 通道設定
│      ┌─────────────────────────────────────┐
│      │  選擇要啟用的通訊通道：               │
│      │                                     │
│      │  ☑ LINE      — 需要 Channel Token   │
│      │  ☑ Telegram  — 需要 Bot Token       │
│      │  ☐ Discord   — 需要 Bot Token       │
│      │  ☐ 稍後再設定                        │
│      │                                     │
│      │  LINE Channel Access Token:          │
│      │  ····························        │
│      │  LINE Channel Secret:                │
│      │  ····························        │
│      │  ✓ LINE Webhook 驗證成功              │
│      │                                     │
│      │  Telegram Bot Token:                 │
│      │  ····························        │
│      │  ✓ @DuDuBot 連線成功                  │
│      └─────────────────────────────────────┘
│
├── 4. Agent 設定
│      ┌─────────────────────────────────────┐
│      │  設定你的 AI 助理：                   │
│      │                                     │
│      │  助理名稱: DuDu                      │
│      │  觸發詞: @DuDu                       │
│      │  人格描述（SOUL.md）：                │
│      │  ● 使用預設人格（溫暖友善的助理）     │
│      │  ○ 自訂人格                          │
│      │  ○ 從範本選擇                        │
│      └─────────────────────────────────────┘
│
├── 5. 容器環境（進階模式才顯示）
│      ┌─────────────────────────────────────┐
│      │  容器執行環境：                       │
│      │                                     │
│      │  偵測到的容器引擎：                   │
│      │  ✓ Docker Desktop 4.37.1             │
│      │  ✓ Apple Container (macOS 26)        │
│      │  ✓ WSL2 (Windows, Ubuntu 24.04)      │
│      │                                     │
│      │  偏好引擎：                           │
│      │  ● 自動選擇（推薦）                  │
│      │  ○ Docker                            │
│      │  ○ Apple Container (macOS)           │
│      │  ○ WSL2 Direct (Windows)             │
│      │                                     │
│      │  正在建構 Agent 容器映像...           │
│      │  ████████████████████░░░ 82%         │
│      └─────────────────────────────────────┘
│
├── 6. 安全設定（進階模式才顯示）
│      ┌─────────────────────────────────────┐
│      │  安全設定：                           │
│      │                                     │
│      │  Gateway 綁定：                      │
│      │  ● localhost (127.0.0.1) — 推薦      │
│      │  ○ LAN (0.0.0.0)                    │
│      │  ○ Tailscale                         │
│      │                                     │
│      │  Gateway Port: 18789                 │
│      │                                     │
│      │  認證模式：                           │
│      │  ● Token（自動產生）                 │
│      │  ○ 無認證（僅限 localhost）          │
│      │                                     │
│      │  掛載安全：                           │
│      │  允許 Agent 存取的目錄：              │
│      │  + ~/projects (讀寫)                 │
│      │  + ~/Documents (唯讀)                │
│      │  [新增目錄...]                       │
│      └─────────────────────────────────────┘
│
└── 7. 完成摘要
       ┌─────────────────────────────────────┐
       │  ✓ 設定完成！                        │
       │                                     │
       │  摘要：                              │
       │  • 助理名稱：DuDu                   │
       │  • 模型帳號：1 個 API Key            │
       │  • 通道：LINE, Telegram              │
       │  • 容器：Docker (自動偵測)           │
       │  • Gateway：localhost:18789          │
       │                                     │
       │  設定已儲存至：                       │
       │  ~/.duduclaw/config.toml             │
       │                                     │
       │  下一步：                            │
       │  $ duduclaw run        # 啟動服務     │
       │  $ duduclaw agent      # CLI 對話     │
       │  $ duduclaw status     # 檢查狀態     │
       │  $ duduclaw doctor     # 健康診斷     │
       └─────────────────────────────────────┘
```

#### 快速模式（`--yes`）

```bash
duduclaw onboard --yes
```

跳過所有互動，使用預設值：
- 無通道（僅 CLI 模式）
- 從環境變數讀取 `ANTHROPIC_API_KEY`
- Docker（自動偵測）
- localhost:18789
- 預設人格

#### 環境變數自動偵測

Onboard 過程自動偵測並填入以下環境變數：

| 環境變數 | 對應設定 |
|----------|----------|
| `ANTHROPIC_API_KEY` | 主帳號 API Key |
| `CLAUDE_CODE_OAUTH_TOKEN` | OAuth Token |
| `LINE_CHANNEL_TOKEN` | LINE Channel Access Token |
| `LINE_CHANNEL_SECRET` | LINE Channel Secret |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot Token |
| `DISCORD_BOT_TOKEN` | Discord Bot Token |
| `DUDUCLAW_PORT` | Gateway Port |
| `DUDUCLAW_BIND` | Gateway 綁定地址 |
| `DUDUCLAW_HOME` | 資料根目錄 |

---

### 11.3 CLI 指令體系

```
duduclaw <command> [options]

核心指令：
  onboard              互動式首次設定精靈
  run                  一鍵啟動（自動 onboard + doctor + gateway）
  agent                CLI 互動式對話
  gateway              僅啟動 Gateway 服務
  status               即時健康快照

管理指令：
  doctor               健康診斷（9 項自動檢查）
  doctor --repair      健康診斷 + 自動修復
  configure            重新設定特定區段
  configure --section channels    僅重新設定通道
  configure --section accounts    僅重新設定帳號
  configure --section agents      僅重新設定 Agent
  configure --section security    僅重新設定安全

Agent 指令：
  agent list           列出所有 Agent
  agent create <name>  互動式建立新 Agent
  agent inspect <name> 檢視 Agent 詳情（設定、預算、技能）
  agent pause <name>   暫停 Agent
  agent resume <name>  恢復 Agent

通道指令：
  channel status       各通道連線狀態
  channel add <type>   互動式新增通道
  channel test <type>  測試通道連線

帳號指令：
  account list         列出帳號與用量
  account add          互動式新增 Claude 帳號
  account rotate       手動觸發帳號輪替
  account health       帳號健康檢查

排程指令：
  cron list            列出排程任務
  cron add             新增排程任務
  cron pause <id>      暫停排程
  cron remove <id>     移除排程

其他：
  update               更新至最新版
  update --channel beta  切換更新通道
  logs                 查看即時日誌
  logs --agent dudu    僅顯示特定 Agent 日誌
  version              顯示版本資訊
  completions <shell>  產生 shell 自動補全腳本

全域選項：
  --config <path>      指定設定檔路徑
  --home <path>        指定資料根目錄
  --verbose            詳細輸出
  --json               JSON 格式輸出（適合腳本）
  --yes                非互動模式，使用預設值
```

---

### 11.4 `duduclaw run` — 一鍵啟動流程

```
duduclaw run
│
├── 1. 檢查設定檔是否存在
│      ├── 不存在 → 自動觸發 duduclaw onboard
│      └── 存在   → 繼續
│
├── 2. 執行 doctor 健康檢查（9 項）
│      ┌─────────────────────────────────────┐
│      │  🔍 DuDuClaw Doctor                  │
│      │                                     │
│      │  ✓ 設定檔         config.toml 有效   │
│      │  ✓ 容器引擎       Docker 運行中      │
│      │  ✓ Agent 映像     duduclaw-agent:ok  │
│      │  ✓ Claude 帳號    1/1 帳號健康       │
│      │  ✓ 通道憑證       LINE ✓ TG ✓       │
│      │  ✓ 記憶體引擎     SQLite 正常        │
│      │  ✓ 安全設定       憑證代理就緒       │
│      │  ✓ 磁碟空間       12.3 GB 可用       │
│      │  ✓ Port 可用      18789 未占用       │
│      │                                     │
│      │  9/9 檢查通過 ✓                      │
│      └─────────────────────────────────────┘
│      ├── 有 FAIL → 嘗試 --repair，失敗則提示手動修復
│      └── 全 PASS → 繼續
│
├── 3. 啟動服務
│      ├── Credential Proxy (port 3001)
│      ├── Gateway (ws://127.0.0.1:18789)
│      ├── Heartbeat Scheduler
│      ├── Channel Connections (LINE, Telegram...)
│      └── IPC Watcher
│
└── 4. 輸出啟動資訊
       ┌─────────────────────────────────────┐
       │  🐾 DuDuClaw 運行中                  │
       │                                     │
       │  Gateway:  ws://127.0.0.1:18789     │
       │  Agents:   dudu (active)            │
       │  Channels: LINE ✓  Telegram ✓       │
       │  Accounts: main (healthy)           │
       │                                     │
       │  按 Ctrl+C 優雅關閉                  │
       │  日誌：~/.duduclaw/logs/current.log  │
       └─────────────────────────────────────┘
```

---

### 11.5 Doctor 健康檢查系統

```rust
// crates/duduclaw-cli/src/doctor.rs

pub struct DoctorCheck {
    pub name: &'static str,
    pub status: CheckStatus,        // Pass / Warn / Fail
    pub message: String,
    pub can_repair: bool,
    pub repair_hint: Option<String>,
}

pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}
```

9 項檢查（按順序執行）：

| # | 檢查項目 | 可自動修復 | 修復動作 |
|---|----------|-----------|----------|
| 1 | **設定檔驗證** | ✓ | 重新觸發 onboard |
| 2 | **容器引擎** | ✗ | 提示安裝 Docker / 啟動 Docker Desktop |
| 3 | **Agent 映像** | ✓ | 自動 `docker build` 或拉取映像 |
| 4 | **Claude 帳號健康** | ✗ | 提示更新 API Key |
| 5 | **通道憑證** | ✗ | 提示執行 `duduclaw configure --section channels` |
| 6 | **記憶體引擎** | ✓ | 重新初始化 SQLite DB |
| 7 | **安全設定** | ✓ | 產生缺少的 secret.key、初始化白名單 |
| 8 | **磁碟空間** | ✗ | 提示清理 (< 1GB 時 WARN, < 100MB 時 FAIL) |
| 9 | **Port 可用** | ✗ | 提示更換 Port 或關閉佔用程序 |

---

### 11.6 Docker 部署方案

#### 方案 A：Docker Compose 快速啟動（單容器）

```yaml
# docker-compose.quickstart.yml
services:
  duduclaw:
    image: ghcr.io/duduclaw/duduclaw:latest
    container_name: duduclaw
    restart: unless-stopped
    ports:
      - "18789:18789"      # Gateway
    volumes:
      - duduclaw-data:/home/duduclaw/.duduclaw
      - /var/run/docker.sock:/var/run/docker.sock  # DinD 管理 Agent 容器
    environment:
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      - DUDUCLAW_BIND=0.0.0.0
      # 可選通道
      - LINE_CHANNEL_TOKEN=${LINE_CHANNEL_TOKEN:-}
      - LINE_CHANNEL_SECRET=${LINE_CHANNEL_SECRET:-}
      - TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN:-}
    healthcheck:
      test: ["CMD", "duduclaw", "status", "--json"]
      interval: 30s
      timeout: 10s
      retries: 3

volumes:
  duduclaw-data:
```

```bash
# 一行部署
ANTHROPIC_API_KEY=sk-ant-... docker compose -f docker-compose.quickstart.yml up -d
```

#### 方案 B：Docker Compose 完整部署（多服務）

```yaml
# docker-compose.yml
services:
  duduclaw:
    build:
      context: .
      dockerfile: container/Dockerfile.server
    container_name: duduclaw-server
    restart: unless-stopped
    ports:
      - "18789:18789"
    volumes:
      - duduclaw-data:/home/duduclaw/.duduclaw
      - /var/run/docker.sock:/var/run/docker.sock
    environment:
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      - DUDUCLAW_BIND=0.0.0.0
    depends_on:
      watchtower:
        condition: service_started
    healthcheck:
      test: ["CMD", "duduclaw", "status", "--json"]
      interval: 30s
      timeout: 10s
      retries: 3

  # 自動更新容器
  watchtower:
    image: containrrr/watchtower
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
    environment:
      - WATCHTOWER_CLEANUP=true
      - WATCHTOWER_POLL_INTERVAL=3600      # 每小時檢查更新
      - WATCHTOWER_SCOPE=duduclaw
    labels:
      - "com.centurylinklabs.watchtower.scope=duduclaw"

  # 可選：Tailscale sidecar（安全遠端存取）
  tailscale:
    image: tailscale/tailscale:latest
    hostname: duduclaw
    environment:
      - TS_AUTHKEY=${TS_AUTHKEY:-}
      - TS_STATE_DIR=/var/lib/tailscale
      - TS_SERVE_CONFIG=/config/serve.json
    volumes:
      - tailscale-state:/var/lib/tailscale
      - ./config/tailscale-serve.json:/config/serve.json:ro
    profiles:
      - tailscale                           # 僅 --profile tailscale 時啟用

volumes:
  duduclaw-data:
  tailscale-state:
```

#### Dockerfile（多階段建構）

```dockerfile
# container/Dockerfile.server
# Stage 1: Rust 編譯
FROM rust:1.82-slim AS rust-builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release --bin duduclaw

# Stage 2: Python 擴充
FROM python:3.12-slim AS python-builder
WORKDIR /build
COPY pyproject.toml ./
COPY python/ python/
RUN pip install --no-cache-dir --prefix=/install .

# Stage 3: Production
FROM python:3.12-slim
RUN groupadd -r duduclaw && useradd -r -g duduclaw -u 1000 duduclaw
COPY --from=rust-builder /build/target/release/duduclaw /usr/local/bin/
COPY --from=python-builder /install /usr/local
COPY container/entrypoint.sh /entrypoint.sh

# 安裝 Docker CLI（用於管理 Agent 容器）
RUN apt-get update && apt-get install -y --no-install-recommends \
    docker.io ca-certificates && rm -rf /var/lib/apt/lists/*

USER duduclaw
WORKDIR /home/duduclaw
VOLUME /home/duduclaw/.duduclaw
EXPOSE 18789

ENTRYPOINT ["/entrypoint.sh"]
CMD ["duduclaw", "run", "--yes"]
```

---

### 11.7 系統服務（systemd / launchd / Windows Service）

所有平台統一使用相同的 CLI 介面：

```bash
duduclaw service install    # 偵測平台，自動產生對應的服務設定
duduclaw service start
duduclaw service stop
duduclaw service status
duduclaw service logs
duduclaw service uninstall
```

#### Linux (systemd)

自動產生的 service 檔：

```ini
# /etc/systemd/system/duduclaw.service（由 duduclaw service install 產生）
[Unit]
Description=DuDuClaw AI Assistant
After=network.target docker.service
Requires=docker.service

[Service]
Type=simple
User=duduclaw
Group=duduclaw
ExecStart=/usr/local/bin/duduclaw run --yes
ExecStop=/bin/kill -SIGTERM $MAINPID
Restart=on-failure
RestartSec=10
Environment=DUDUCLAW_HOME=/home/duduclaw/.duduclaw

[Install]
WantedBy=multi-user.target
```

#### macOS (launchd)

自動產生的 plist：

```xml
<!-- ~/Library/LaunchAgents/dev.duduclaw.plist（由 duduclaw service install 產生）-->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.duduclaw</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/duduclaw</string>
        <string>run</string>
        <string>--yes</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/Users/USERNAME/.duduclaw/logs/stdout.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/USERNAME/.duduclaw/logs/stderr.log</string>
</dict>
</plist>
```

#### Windows (Windows Service)

使用 Rust crate **windows-service** 將 DuDuClaw 註冊為原生 Windows Service，
支援開機自動啟動、服務管理員控制、事件日誌整合。

```powershell
# 需要以系統管理員身份執行
duduclaw service install    # 註冊 Windows Service + 設定自動啟動
duduclaw service start      # 啟動服務（等同 sc start DuDuClaw）
duduclaw service stop       # 停止服務
duduclaw service logs       # 開啟事件檢視器的 DuDuClaw 日誌
duduclaw service uninstall  # 移除服務
```

內部實作：

```rust
// crates/duduclaw-cli/src/service/windows.rs

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode,
        ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler, service_dispatcher,
};

const SERVICE_NAME: &str = "DuDuClaw";
const SERVICE_DISPLAY: &str = "DuDuClaw AI Assistant";

define_windows_service!(ffi_service_main, duduclaw_service_main);

fn duduclaw_service_main(arguments: Vec<OsString>) {
    // 1. 註冊 Service Control Handler (接收 Stop/Pause/Continue)
    // 2. 回報 ServiceState::Running
    // 3. 啟動 duduclaw runtime (Gateway + Channels + Heartbeat)
    // 4. 收到 Stop 信號 → 優雅關閉
}

pub fn install_service() -> Result<()> {
    let manager = ServiceManager::local_computer(None, ServiceManagerAccess::ALL_ACCESS)?;
    let service = manager.create_service(&ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: vec!["service".into(), "run-as-service".into()],
        dependencies: vec![
            // 依賴 Docker Desktop Service（若已安裝）
            ServiceDependency::Service(OsString::from("com.docker.service")),
        ],
        account_name: None, // LocalSystem
        account_password: None,
    }, ServiceAccess::ALL_ACCESS)?;

    // 設定失敗重試策略：第一次 30 秒後重啟，第二次 60 秒，第三次 120 秒
    service.set_failure_actions(ServiceFailureActions {
        reset_period: Duration::from_secs(86400),
        actions: Some(vec![
            ServiceAction { action_type: SC_ACTION_RESTART, delay: Duration::from_secs(30) },
            ServiceAction { action_type: SC_ACTION_RESTART, delay: Duration::from_secs(60) },
            ServiceAction { action_type: SC_ACTION_RESTART, delay: Duration::from_secs(120) },
        ]),
        ..Default::default()
    })?;

    Ok(())
}
```

服務管理也可透過 Windows 原生工具操作：

```powershell
# Windows 服務管理員 GUI
services.msc                          # 可看到 "DuDuClaw AI Assistant"

# sc 指令
sc query DuDuClaw                     # 查詢狀態
sc stop DuDuClaw                      # 停止
sc start DuDuClaw                     # 啟動

# PowerShell cmdlet
Get-Service DuDuClaw                  # 查詢
Restart-Service DuDuClaw              # 重啟

# 事件日誌
Get-EventLog -LogName Application -Source DuDuClaw -Newest 20
```

---

### 11.8 更新系統

```bash
duduclaw update                    # 更新至 stable 最新版
duduclaw update --channel beta     # 切換至 beta 通道
duduclaw update --channel dev      # 切換至開發版
duduclaw update --check            # 僅檢查是否有新版本
```

更新流程：
1. 檢查目前版本 vs 遠端最新版本
2. 下載新二進位檔至暫存目錄
3. 驗證 SHA-256 校驗碼
4. 原子替換二進位檔（rename）
5. 更新 Python 擴充（`pip install --upgrade duduclaw`）
6. 執行 `duduclaw doctor --repair`（確保相容性）
7. 若為 service 模式 → 提示重新啟動

```
更新通道：
  stable  — 正式版（vYYYY.M.D 標籤）
  beta    — 預發布版（週更）
  dev     — main 分支最新（每日建構）
```

---

### 11.9 跨平台路徑對照

| 用途 | macOS / Linux | Windows |
|------|---------------|---------|
| 資料根目錄 | `~/.duduclaw/` | `%USERPROFILE%\.duduclaw\` |
| 設定檔 | `~/.duduclaw/config.toml` | `%USERPROFILE%\.duduclaw\config.toml` |
| 二進位檔 | `~/.duduclaw/bin/duduclaw` | `%USERPROFILE%\.duduclaw\bin\duduclaw.exe` |
| 安全設定 | `~/.config/duduclaw/` | `%APPDATA%\duduclaw\` |
| 掛載白名單 | `~/.config/duduclaw/mount-allowlist.json` | `%APPDATA%\duduclaw\mount-allowlist.json` |
| 日誌目錄 | `~/.duduclaw/logs/` | `%USERPROFILE%\.duduclaw\logs\` |
| 服務設定 | systemd / launchd（見 11.7） | Windows Service（見 11.7） |

所有路徑可透過 `DUDUCLAW_HOME` 環境變數覆蓋。

---

### 11.10 Windows 專屬注意事項

#### 容器引擎選擇

```
Windows 容器引擎偵測流程：

1. 偵測 Docker Desktop
   ├── 已安裝且 WSL2 backend ✓ → 使用 Docker（推薦）
   └── 未安裝 ↓

2. 偵測 WSL2
   ├── 已安裝 Linux distro ✓ → 使用 WSL2 Direct
   │   ├── distro 有 Docker ✓ → wsl -d <distro> -- docker run ...
   │   └── distro 無 Docker   → 自動安裝 Docker CE in WSL2
   └── 未安裝 ↓

3. 引導安裝
   └── 提示安裝 Docker Desktop 或啟用 WSL2
       ┌─────────────────────────────────────┐
       │  ⚠ 未偵測到容器引擎                  │
       │                                     │
       │  DuDuClaw 需要容器引擎來隔離 Agent    │
       │  請選擇安裝方式：                     │
       │                                     │
       │  ● 安裝 Docker Desktop（推薦）       │
       │    → 開啟下載頁面                    │
       │  ○ 啟用 WSL2 + Ubuntu               │
       │    → 自動執行 wsl --install          │
       │  ○ 跳過（僅 CLI 模式，無容器隔離）   │
       └─────────────────────────────────────┘
```

#### Shell 工具差異

容器內的 Agent 統一在 Linux 環境執行（Docker / WSL2），因此 `exec` 工具始終使用 `/bin/sh`。
但 CLI 模式下直接在主機執行時，DuDuClaw 自動偵測：

| 平台 | 預設 Shell | 路徑分隔 |
|------|-----------|----------|
| macOS / Linux | `/bin/sh` | `/` |
| Windows (容器化) | `/bin/sh`（容器內 Linux） | `/` |
| Windows (CLI 直接) | `powershell.exe` | `\` |

#### Windows 防火牆

`duduclaw service install` 在 Windows 上額外執行：
- 自動新增 Windows 防火牆規則（允許 Gateway Port 的入站連線）
- 僅在 Gateway 綁定非 localhost 時觸發
- 需要管理員權限確認

```powershell
# 自動執行的防火牆規則（使用者可在 onboard 中選擇跳過）
New-NetFirewallRule -DisplayName "DuDuClaw Gateway" `
  -Direction Inbound -Protocol TCP -LocalPort 18789 `
  -Action Allow -Profile Private
```

---

### 11.11 完整安裝流程示意

```
使用者
  │
  ├── 方式 A：零經驗使用者（macOS / Linux）
  │     curl -fsSL https://install.duduclaw.dev | sh
  │     duduclaw onboard              ← 互動式精靈引導
  │     duduclaw run                  ← 一鍵啟動
  │
  ├── 方式 B：零經驗使用者（Windows）
  │     irm https://install.duduclaw.dev/win | iex
  │     duduclaw onboard              ← 互動式精靈引導（含 WSL2/Docker 偵測）
  │     duduclaw run                  ← 一鍵啟動
  │
  ├── 方式 C：有經驗開發者（macOS）
  │     brew install duduclaw/tap/duduclaw
  │     duduclaw onboard --yes        ← 非互動，用環境變數
  │     duduclaw run                  ← 一鍵啟動
  │
  ├── 方式 D：有經驗開發者（Windows）
  │     winget install DuDuClaw.DuDuClaw
  │     duduclaw onboard --yes        ← 非互動，用環境變數
  │     duduclaw run                  ← 一鍵啟動
  │
  ├── 方式 E：Docker 部署（全平台）
  │     docker compose up -d          ← 容器化部署
  │
  ├── 方式 F：背景服務（全平台）
  │     duduclaw onboard
  │     duduclaw service install      ← 安裝為系統服務
  │     duduclaw service start        ←   Linux: systemd
  │                                       macOS: launchd
  │                                       Windows: Windows Service
  │
  └── 方式 G：開發貢獻者（全平台）
        git clone ... && cd duduclaw
        cargo build --release
        pip install -e ./python
        # macOS / Linux:
        ./target/release/duduclaw onboard
        # Windows:
        .\target\release\duduclaw.exe onboard
```

---

## 十二、實作路線圖

### Phase 1：核心骨架（2-3 週）

- [ ] Rust workspace 建立 + 跨平台 CI/CD（見下方建構矩陣）
- [ ] `duduclaw-core`：型別、trait 定義
- [ ] `duduclaw-cli`：onboard 指令（跨平台路徑處理）
- [ ] `duduclaw-agent`：Agent 資料夾掃描、agent.toml 解析
- [ ] `duduclaw-container`：Docker runtime 基本生命週期
- [ ] `duduclaw-bridge`：PyO3 骨架
- [ ] Python SDK rotator 骨架（單帳號先行）

### Phase 2：安全層 + 單 Agent 運行（2-3 週）

- [ ] `duduclaw-security`：CredentialProxy、MountGuard
- [ ] `duduclaw-container`：Apple Container 支援
- [ ] `duduclaw-memory`：SQLite + FTS5（向量搜尋 Phase 3）
- [ ] Container Agent Loop：Claude Code SDK 容器內執行
- [ ] 單 Agent CLI 模式可運行（macOS + Linux + Windows）

### Phase 3：Gateway + 通道（2-3 週）

- [ ] `duduclaw-gateway`：WebSocket RPC (OpenClaw 相容)
- [ ] `duduclaw-bus`：訊息路由
- [ ] Python channels：LINE、Telegram、Discord
- [ ] 心跳排程器
- [ ] 多帳號輪替完整實作

### Phase 4：多 Agent + 自主提升（2-3 週）

- [ ] Agent 路由（觸發詞匹配）
- [ ] Agent 間 IPC
- [ ] 自主提升三層反思
- [ ] Skill Vetter 安全掃描
- [ ] Agent 動態管理工具（agent_create/delegate）
- [ ] 預算控管

### Phase 5：跨平台打磨 + 文件（1-2 週）

- [ ] `duduclaw-container`：WSL2 Direct runtime 支援
- [ ] `duduclaw-cli`：Windows Service 整合
- [ ] 向量嵌入記憶搜尋
- [ ] 完整測試覆蓋 (80%+)
- [ ] 文件撰寫
- [ ] Docker Compose 部署配置
- [ ] winget / Scoop 套件發布
- [ ] 效能調優

---

### 跨平台建構矩陣

CI/CD 使用 GitHub Actions，每次發版自動建構所有平台：

```yaml
# .github/workflows/release.yml (概念)
strategy:
  matrix:
    include:
      # macOS
      - target: aarch64-apple-darwin
        os: macos-latest
        artifact: duduclaw-darwin-arm64
      - target: x86_64-apple-darwin
        os: macos-latest
        artifact: duduclaw-darwin-x64

      # Linux
      - target: x86_64-unknown-linux-gnu
        os: ubuntu-latest
        artifact: duduclaw-linux-x64
      - target: aarch64-unknown-linux-gnu
        os: ubuntu-latest
        artifact: duduclaw-linux-arm64

      # Windows
      - target: x86_64-pc-windows-msvc
        os: windows-latest
        artifact: duduclaw-windows-x64.exe
      - target: aarch64-pc-windows-msvc
        os: windows-latest
        artifact: duduclaw-windows-arm64.exe
```

每個建構產物包含：
- `duduclaw` 二進位檔（或 `.exe`）
- SHA-256 校驗檔
- Python wheel（`duduclaw-*.whl`）

### 條件編譯策略

```rust
// crates/duduclaw-container/src/lib.rs

pub fn detect_runtime() -> RuntimeBackend {
    #[cfg(target_os = "macos")]
    if apple_container_available() {
        return RuntimeBackend::AppleContainer(AppleContainerRuntime::new());
    }

    #[cfg(target_os = "windows")]
    if wsl2_available() {
        return RuntimeBackend::Wsl2(Wsl2Runtime::new());
    }

    // 全平台 fallback
    RuntimeBackend::Docker(DockerRuntime::new())
}

// crates/duduclaw-cli/src/service/mod.rs

pub fn install_service(config: &ServiceConfig) -> Result<()> {
    #[cfg(target_os = "linux")]
    return systemd::install(config);

    #[cfg(target_os = "macos")]
    return launchd::install(config);

    #[cfg(target_os = "windows")]
    return windows::install(config);
}
```
