# 在同一台機器上執行多個 DuDuClaw 實例（Plan A）

你可以在同一台機器上執行多個彼此獨立的 DuDuClaw 實例，共用同一支 binary，只要讓每個實例擁有各自的**狀態根目錄**、**埠號**與**實例名稱**即可。這是「Plan A」，也是最輕量的隔離模型。若需要更強的隔離（獨立作業系統使用者，或容器），請見文末的替代方案。

## 三個環境變數

| 環境變數 | 用途 | 每個實例是否必須不同 |
| --- | --- | --- |
| `DUDUCLAW_HOME` | 狀態根目錄：config、SQLite 資料庫、`bus_queue.jsonl`、`events.db`、models、shared wiki、secrets、cron。預設為 `~/.duduclaw`。 | **是** |
| `DUDUCLAW_PORT` | Gateway 的 HTTP/WS 埠號。預設為 `18789`。 | **是** |
| `DUDUCLAW_INSTANCE` | 短實例名稱（`[a-z0-9-]`）。用來替 `~/.claude/settings.json` 裡的**全域 MCP 註冊**鍵名加上命名空間（`duduclaw` → `duduclaw-<name>`），避免各實例互相覆蓋。 | 建議設定 |

每個子系統都透過同一個標準 helper（`duduclaw_core::duduclaw_home()`）來解析自己的狀態根目錄，因此設定 `DUDUCLAW_HOME` 會搬動*所有*屬於該實例的狀態，不會有任何路徑悄悄漏回 `~/.duduclaw`。

## 範例：兩個實例

```bash
# 實例 "work"
DUDUCLAW_HOME=~/dd-work  DUDUCLAW_PORT=18789 DUDUCLAW_INSTANCE=work \
  duduclaw run --yes

# 實例 "play"
DUDUCLAW_HOME=~/dd-play  DUDUCLAW_PORT=18790 DUDUCLAW_INSTANCE=play \
  duduclaw run --yes
```

每個實例在註冊自己的 MCP server 時，都會在共用的 `~/.claude/settings.json` 裡寫入一筆帶命名空間的項目，並把自己的環境變數一併帶進啟動規格，讓 Claude CLI 派生出的 `duduclaw mcp-server` 能連回正確的實例：

```jsonc
{
  "mcpServers": {
    "duduclaw-work": {
      "command": "/path/to/duduclaw",
      "args": ["mcp-server"],
      "env": { "DUDUCLAW_HOME": "/Users/you/dd-work", "DUDUCLAW_PORT": "18789", "DUDUCLAW_INSTANCE": "work" }
    },
    "duduclaw-play": {
      "command": "/path/to/duduclaw",
      "args": ["mcp-server"],
      "env": { "DUDUCLAW_HOME": "/Users/you/dd-play", "DUDUCLAW_PORT": "18790", "DUDUCLAW_INSTANCE": "play" }
    }
  }
}
```

`"command"` 應該填你 `duduclaw` binary 的絕對路徑，可用 `which duduclaw` 查出來（例如 npm 全域 bin 目錄，或桌面應用內建的 binary）。

## 必須各不相同的檢查清單

- [ ] `DUDUCLAW_HOME`：每個實例各自獨立的目錄
- [ ] `DUDUCLAW_PORT`：各自獨立的埠號（若有跑 `http-server --bind`，MCP HTTP 埠號也要各自獨立）
- [ ] `DUDUCLAW_INSTANCE`：各自獨立的名稱（用來替 MCP 註冊加上命名空間）
- [ ] launchd／systemd 的**服務標籤**：每個實例各自獨立
- [ ] **models 目錄**：讓所有 `DUDUCLAW_HOME/models` 指向同一個共用、唯讀的位置（用 symlink），避免重複存放好幾 GB 的 GGUF 模型檔

## 共用狀態 vs 隔離狀態

- **由 `DUDUCLAW_HOME` 隔離**：config、所有 SQLite 資料庫、bus queue、events、cron、shared wiki、JWT／keyfile、evolution 狀態。
- **在同一個作業系統使用者底下共用**：`~/.claude`（Claude CLI 的 OAuth session 與 MCP 設定）。各實例透過帶命名空間的 MCP 鍵名可以並存於此，但仍會用到**同一批 OAuth 訂閱帳號**，高併發使用可能造成輪替／rate-limit 互相搶佔。建議在各實例的 `config.toml` 裡設定各自專屬的帳號，或使用個別帳號 profile（`~/.claude/profiles/<name>`）來避免互相干擾。

## 何時該選更強的隔離模型

- **獨立作業系統使用者**：每個實例跑在自己的帳號底下，`~/.duduclaw` 與 `~/.claude`（OAuth）天然就有檔案系統層級的邊界隔離，完全不依賴環境變數，但仍需要各自獨立的埠號。
- **容器（Docker／Podman）**：完整的檔案系統與網路命名空間隔離，每個容器內部都可以沿用同一個埠號 `18789`，再各自對應到主機上不同的埠號。注意：在 macOS 上，Linux 容器沒有 Metal，本機 GGUF 推論會退回 CPU 執行（若需要 GPU，請把推論留在主機上執行）。
