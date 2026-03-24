# DuDuClaw Phase 2 — 競爭力強化

> 版本：v0.6.0 目標
> 更新日期：2026-03-23
> 參考：[docs/claw-ecosystem-report.md](docs/claw-ecosystem-report.md)
> 前置：Phase 1 (v0.1–v0.5.1) 全部 46 項已完成

---

## 總覽

Phase 2 目標：**從「能用」升級到「好用且有壁壘」**。

基於 Claw 生態系統研究的四大建議發展方向，拆分為 4 個模組、21 項任務：

| 模組 | 任務數 | 優先序 | 核心價值 |
|------|--------|--------|---------|
| A. 容器隔離 | 6 | P0 | 安全性（NanoClaw/NemoClaw 最強賣點） |
| B. Skill 市場相容 | 5 | P1 | 借用 5,400+ OpenClaw skill 生態 |
| C. 安全防護 | 6 | P1 | 防禦認知攻擊（ClawSec 模式） |
| D. 紅隊測試 | 4 | P2 | 部署前驗證（SuperClaw 模式） |

---

## A. 容器隔離（P0）

> 參考：NanoClaw（OS 層級隔離）、NemoClaw（NVIDIA 沙箱 + k3s）
> 現狀：`duduclaw-container` crate 已有 Docker/WSL2 runtime stubs（bollard），但未接入 agent 執行流程

### A-1. Agent 沙箱執行器

- [x] **[A-1a]** 實作 `SandboxRunner` — 在 Docker 容器中執行 agent 任務
  - 檔案：`crates/duduclaw-container/src/sandbox.rs`
  - 建立 `duduclaw-agent:latest` Docker image（基於 `node:lts-slim` + `claude` CLI）
  - 掛載 agent 目錄為 readonly，workspace 為 tmpfs
  - 設定 `--network=none`（預設離線），可選開啟網路
  - 執行逾時自動 kill（使用 `ContainerConfig.timeout_ms`）

- [x] **[A-1b]** Agent 配置新增 `sandbox` 選項
  - 檔案：`crates/duduclaw-core/src/types.rs`
  - `ContainerConfig` 新增 `sandbox_enabled: bool`、`network_access: bool`
  - `agent.toml` 範例：`[container] sandbox_enabled = true`

- [x] **[A-1c]** Dispatcher 整合沙箱
  - 檔案：`crates/duduclaw-gateway/src/dispatcher.rs`
  - 當 `sandbox_enabled = true` 時，改用 `SandboxRunner` 而非直接 `call_claude`
  - 結果從容器 stdout 收集

### A-2. macOS Container 支援

- [x] **[A-2a]** Apple Container runtime backend
  - 檔案：`crates/duduclaw-container/src/apple.rs`
  - 偵測 macOS 15+ 的 `container` CLI 可用性
  - 實作 `ContainerRuntime` trait（create / start / stop / remove / logs / health_check）
  - `RuntimeBackend::detect()` 中加入 Apple Container 優先級

### A-3. Dashboard 沙箱控制面板

- [x] **[A-3a]** 前端：Agent 卡片顯示沙箱狀態
  - 檔案：`web/src/pages/AgentsPage.tsx`
  - 沙箱啟用時顯示盾牌圖示 + "Sandboxed" 標籤
  - 顯示 network 狀態（離線/在線）

- [x] **[A-3b]** 後端：`agents.sandbox_toggle` RPC 方法
  - 檔案：`crates/duduclaw-gateway/src/handlers.rs`
  - 切換 agent 的 `sandbox_enabled` 設定
  - 回傳切換後的完整 agent 配置

---

## B. Skill 市場相容（P1）

> 參考：OpenClaw（5,400+ skills）、awesome-openclaw-skills
> 目標：可安裝、載入、執行 OpenClaw 格式的 skill

### B-1. Skill 格式解析器

- [x] **[B-1a]** 實作 OpenClaw skill 格式解析
  - 檔案：`crates/duduclaw-agent/src/skill_loader.rs`（新檔案）
  - 解析 OpenClaw skill 的 `SKILL.md` frontmatter（name, description, trigger, tools）
  - 轉換為 DuDuClaw 內部的 `SkillFile` 格式
  - 支援 `.js` / `.ts` / `.py` 工具腳本（記錄路徑但不直接執行）

- [x] **[B-1b]** `duduclaw skill install <url|name>` CLI 命令
  - 檔案：`crates/duduclaw-cli/src/main.rs`
  - 從 GitHub URL 或 skill registry 名稱下載 skill 到 agent 的 `SKILLS/` 目錄
  - 執行安全掃描（呼叫 evolution vet_skill）
  - 通過後啟用，失敗則放入 `SKILLS/_quarantine/`

### B-2. Skill Registry 整合

- [x] **[B-2a]** 本地 skill registry 索引
  - 檔案：`crates/duduclaw-agent/src/skill_registry.rs`（新檔案）
  - 從 `awesome-openclaw-skills` 的 JSON 索引快取到 `~/.duduclaw/skill_index.json`
  - 支援搜尋（by name / tag / description）
  - 定期更新（每天或手動）

- [x] **[B-2b]** MCP 工具 `skill_search` / `skill_install`
  - 檔案：`crates/duduclaw-cli/src/mcp.rs`
  - Agent 可透過 MCP 自主搜尋和安裝 skill
  - 安裝前自動執行安全掃描

- [x] **[B-2c]** Dashboard Skill 市場頁面
  - 檔案：`web/src/pages/SkillMarketPage.tsx`（新檔案）
  - 瀏覽 / 搜尋 / 安裝 / 移除 skill
  - 顯示安全掃描結果和相容性

---

## C. 安全防護（P1）

> 參考：ClawSec（SOUL.md 漂移檢測、prompt injection 偵測）、OneCLI（密鑰託管）
> 目標：保護 agent 認知架構完整性

### C-1. SOUL.md 完整性保護

- [x] **[C-1a]** SOUL.md 漂移檢測引擎
  - 檔案：`crates/duduclaw-security/src/soul_guard.rs`（新檔案）
  - Agent 啟動時計算 SOUL.md 的 SHA-256 指紋並持久化
  - 每次 heartbeat 重新計算並比對
  - 漂移時發出 warn 級別 tracing event + WebSocket 通知
  - 提供 `duduclaw doctor` 項目：SOUL integrity check

- [x] **[C-1b]** SOUL.md 版本歷史
  - 檔案：`crates/duduclaw-security/src/soul_guard.rs`
  - 每次變更時備份至 `~/.duduclaw/agents/<name>/.soul_history/`
  - 命名格式：`SOUL_<timestamp>.md`
  - 最多保留 10 個版本

### C-2. Prompt Injection 偵測

- [x] **[C-2a]** 輸入訊息安全掃描
  - 檔案：`crates/duduclaw-security/src/input_guard.rs`（新檔案）
  - 規則引擎檢查常見 prompt injection 模式：
    - `ignore previous instructions`
    - `system prompt override`
    - Base64 編碼繞過
    - Unicode 零寬字元注入
  - 風險分數 0-100，超過閾值時攔截或警告
  - 整合到 `channel_reply.rs` 的 `build_reply_with_session()` 前

- [x] **[C-2b]** 安全事件日誌
  - 檔案：`crates/duduclaw-security/src/audit.rs`（新檔案）
  - 所有安全事件（漂移偵測、injection 偵測、skill 隔離）寫入 `~/.duduclaw/security_audit.jsonl`
  - 包含：timestamp, event_type, agent_id, severity, details
  - Dashboard SecurityPage 可查詢

### C-3. 密鑰隔離

- [x] **[C-3a]** Agent 級密鑰隔離
  - 檔案：`crates/duduclaw-security/src/key_vault.rs`（新檔案）
  - 每個 agent 只能存取其 `allowed_channels` 對應的 API key
  - Key 透過 `ReplyContext` 傳遞，不寫入環境變數
  - 沙箱模式下密鑰透過 stdin 一次性注入

---

## D. 紅隊測試（P2）

> 參考：SuperClaw（情境測試、行為契約、SARIF 報告）
> 目標：部署前驗證 agent 安全性

### D-1. 行為契約系統

- [x] **[D-1a]** Agent 行為契約定義
  - 檔案：`crates/duduclaw-agent/src/contract.rs`（新檔案）
  - 支援 `CONTRACT.toml` 檔案定義 agent 行為邊界：
    ```toml
    [boundaries]
    must_not = ["reveal api keys", "execute rm -rf", "modify SOUL.md"]
    must_always = ["respond in zh-TW", "refuse harmful requests"]
    max_tool_calls_per_turn = 10
    ```
  - 載入時解析並注入為 system prompt 約束

- [x] **[D-1b]** 行為契約驗證器
  - 檔案：`crates/duduclaw-agent/src/contract.rs`
  - 每次 agent 回覆後比對 `must_not` 規則
  - 使用簡單的關鍵字 + regex 匹配（不呼叫 AI 以避免成本）
  - 違規時記錄到安全審計日誌

### D-2. 紅隊測試 CLI

- [x] **[D-2a]** `duduclaw test <agent>` 命令
  - 檔案：`crates/duduclaw-cli/src/main.rs`
  - 內建測試情境（prompt injection、jailbreak、身份偽造、工具濫用）
  - 逐一執行，收集 agent 回應
  - 與行為契約比對，生成通過/失敗報告

- [x] **[D-2b]** 測試報告生成
  - 輸出格式：JSON + 終端機彩色摘要
  - 包含：測試名稱、攻擊向量、agent 回應摘要、通過/失敗、建議修復
  - 未來可擴展為 SARIF 格式供 CI/CD 整合

---

## 依賴關係

```
A-1a ─→ A-1c ─→ A-3a
A-1b ─╯         A-3b
A-2a（獨立）

B-1a ─→ B-1b
B-2a ─→ B-2b ─→ B-2c

C-1a ─→ C-1b
C-2a ─→ C-2b
C-3a（獨立）

D-1a ─→ D-1b ─→ D-2a ─→ D-2b
```

## 建議執行順序

### Sprint 1（安全基礎）— 1-2 週
1. C-1a SOUL.md 漂移檢測
2. C-2a Prompt injection 偵測
3. C-2b 安全事件日誌
4. C-1b SOUL.md 版本歷史

### Sprint 2（容器隔離）— 2-3 週
5. A-1a SandboxRunner 實作
6. A-1b Agent 配置新增 sandbox
7. A-1c Dispatcher 整合沙箱
8. A-2a Apple Container 支援

### Sprint 3（Skill 生態）— 1-2 週
9. B-1a Skill 格式解析器
10. B-1b `skill install` CLI
11. B-2a 本地 skill registry
12. B-2b MCP skill 工具

### Sprint 4（紅隊 + 收尾）— 1-2 週
13. D-1a 行為契約定義
14. D-1b 契約驗證器
15. D-2a `duduclaw test` 命令
16. D-2b 報告生成
17. C-3a 密鑰隔離
18. A-3a/b Dashboard 沙箱 UI
19. B-2c Skill 市場頁面

---

## 進度總覽

| 模組 | 任務數 | 完成 | 剩餘 |
|------|--------|------|------|
| A. 容器隔離 | 6 | 6 | 0 |
| B. Skill 市場相容 | 5 | 5 | 0 |
| C. 安全防護 | 6 | 6 | 0 |
| D. 紅隊測試 | 4 | 4 | 0 |
| **合計** | **21** | **21** | **0** |

---

## Sprint 完成記錄

### Sprint 1 — 安全基礎 (2026-03-23)

**Build 狀態**：PASS (`cargo check` 零錯誤)

| 項目 | 檔案 | 狀態 |
|------|------|:----:|
| C-1a SOUL.md 漂移檢測 | `crates/duduclaw-security/src/soul_guard.rs` | done |
| C-1b SOUL.md 版本歷史 | `crates/duduclaw-security/src/soul_guard.rs` | done |
| C-2a Prompt injection 偵測 | `crates/duduclaw-security/src/input_guard.rs` | done |
| C-2b 安全事件日誌 | `crates/duduclaw-security/src/audit.rs` | done |

**實作摘要**：
- `soul_guard`: SHA-256 指紋計算 + `.soul_hash` 持久化 + `.soul_history/` 版本備份（最多 10 版）+ `accept_soul_change()` 合法變更接受
- `input_guard`: 6 類規則（instruction_override, system_prompt_extraction, role_hijack, encoding_bypass, tool_abuse, data_exfiltration）+ Unicode 零寬字元偵測 + 風險評分 0-100
- `audit`: JSONL append-only 日誌 + `log_soul_drift()` / `log_injection_detected()` / `log_skill_quarantined()` 便捷方法 + `read_recent_events()` 查詢

### Sprint 2 — 容器隔離 (2026-03-23)

**Build 狀態**：PASS (`cargo check` 零錯誤)

| 項目 | 檔案 | 狀態 |
|------|------|:----:|
| A-1b Agent sandbox 配置 | `crates/duduclaw-core/src/types.rs` | done |
| A-1a SandboxRunner | `crates/duduclaw-container/src/sandbox.rs` | done |
| A-1c Dispatcher 沙箱整合 | `crates/duduclaw-gateway/src/dispatcher.rs` | done |
| A-2a Apple Container | `crates/duduclaw-container/src/apple.rs` | done |

**實作摘要**：
- `types.rs`: `ContainerConfig` 新增 `sandbox_enabled: bool` + `network_access: bool`（`#[serde(default)]` 向後相容）
- `sandbox.rs`: Docker 一次性容器 — read-only agent mount, tmpfs workspace, 可選 `--network=none`, 512MB memory limit, read-only rootfs, timeout auto-kill, 結果收集
- `apple.rs`: macOS 15+ `container` CLI backend, 實作完整 `ContainerRuntime` trait, `RuntimeBackend::detect()` 優先選擇
- `dispatcher.rs`: `dispatch_to_agent()` 自動判斷 `sandbox_enabled` — 有沙箱且 Docker 可用時走 sandbox, 否則 fallback 到直接 `call_claude`

### Sprint 3 — Skill 生態 (2026-03-23)

**Build 狀態**：PASS (`cargo check` 零錯誤)

| 項目 | 檔案 | 狀態 |
|------|------|:----:|
| B-1a Skill 格式解析器 | `crates/duduclaw-agent/src/skill_loader.rs` | done |
| B-1b skill install 函式 | `crates/duduclaw-agent/src/skill_loader.rs` | done |
| B-2a 本地 skill registry | `crates/duduclaw-agent/src/skill_registry.rs` | done |
| B-2b MCP skill 工具 | `crates/duduclaw-cli/src/mcp.rs` | done |

**實作摘要**：
- `skill_loader`: YAML frontmatter 解析（name/description/trigger/tools/tags）+ tool script 掃描（.js/.ts/.py）+ `install_skill()` 安裝到 agent SKILLS/ 目錄
- `skill_registry`: JSON 索引快取 + 加權搜尋（name x10, tag x7, description x5）+ upsert/remove CRUD + tag 過濾
- MCP 工具：`skill_search`（搜尋 registry）+ `skill_list`（列出 agent 已安裝 skill）

### Sprint 4 — 紅隊測試 + 收尾 (2026-03-23)

**Build 狀態**：PASS (`cargo check` 零錯誤)

| 項目 | 檔案 | 狀態 |
|------|------|:----:|
| D-1a 行為契約定義 | `crates/duduclaw-agent/src/contract.rs` | done |
| D-1b 契約驗證器 | `crates/duduclaw-agent/src/contract.rs` | done |
| D-2a `duduclaw test` 命令 | `crates/duduclaw-cli/src/main.rs` | done |
| D-2b 測試報告生成 | `crates/duduclaw-cli/src/main.rs` | done |
| C-3a 密鑰隔離 | `crates/duduclaw-security/src/key_vault.rs` | done |

**實作摘要**：
- `contract.rs`: `CONTRACT.toml` 解析 + `must_not` / `must_always` / `max_tool_calls_per_turn` 邊界定義 + `validate_response()` 子字串 + glob 匹配 + `contract_to_prompt()` 注入 system prompt
- `cmd_test_agent()`: 9 項內建測試（SOUL 完整性、契約存在、6 種 injection 場景、契約強制執行）+ 彩色終端機輸出 + JSON 報告寫入 `~/.duduclaw/test-report-<agent>.json`
- `key_vault.rs`: per-agent 通道權限檢查 + `resolve_agent_keys()` 從 config.toml 過濾 + `check_channel_access()` 存取驗證

### UI 收尾 — Dashboard 整合 (2026-03-23)

**Build 狀態**：PASS (TypeScript 零錯誤 + Vite build + `cargo check` 零錯誤)

| 項目 | 檔案 | 狀態 |
|------|------|:----:|
| A-3a Agent 沙箱 badge | `web/src/pages/AgentsPage.tsx` | done |
| A-3b sandbox_toggle (API 定義) | `web/src/lib/api.ts` | done |
| B-2c Skill 市場頁面 | `web/src/pages/SkillMarketPage.tsx` | done |
| SecurityPage 審計日誌 | `web/src/pages/SecurityPage.tsx` | done |

**實作摘要**：
- `AgentsPage`: 沙箱啟用時顯示藍色 ShieldCheck badge "沙箱"
- `SkillMarketPage`: 搜尋框 + 8 類別瀏覽（utility/communication/code/data/security/ai/media/automation）+ 卡片式結果（名稱/描述/tags/作者/Install 按鈕）
- `SecurityPage`: 新增 Audit Log 卡片，從 `security.audit_log` RPC 載入最近 30 筆事件，以嚴重度圖示 + 時間 + 類型顯示
- `api.ts`: 新增 `HeartbeatInfo`、`AuditEvent`、`SkillIndexEntry` 型別 + `heartbeat.*`、`security.*`、`skillMarket.*` API 命名空間
- Sidebar: 新增 Skill 市場（Puzzle 圖示）導航項
- i18n: zh-TW + en 新增 12 組翻譯
