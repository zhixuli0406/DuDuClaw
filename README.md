# DuDuClaw 🐾

<div align="center">

**繁體中文** · [English](README.en.md) · [日本語](README.ja.md)

</div>

> **Multi-Runtime AI Agent Platform** — 統一 Claude / Codex / Gemini 三大 CLI，打造你的多通道 AI 助理

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.9+-blue?logo=python)](https://www.python.org/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-1.29.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)

---

## 🔒 信任與安全（Trust & Security）

這是開源專案 — 完整透明地說明你安裝的是什麼。

### 為什麼一個「新」的 npm 套件版本號已經 1.21+？

DuDuClaw 在公開之前，已在私有 repository 經歷數個月密集開發（400+ commits）。
完整歷史見 [git log](https://github.com/zhixuli0406/DuDuClaw/commits/main)。

### npm 套件裡有什麼？

- 一個小型 JS wrapper，僅負責呼叫對應平台的 Rust 二進位
- 平台二進位透過 npm `optionalDependencies`（`@duduclaw/<platform>`）安裝 —
  **沒有 postinstall 從任意 URL 下載並執行外部程式碼**
- `postinstall` 只檢查平台套件是否就位（見 [`npm/duduclaw/scripts/install.js`](npm/duduclaw/scripts/install.js)），不下載、不執行任何東西
- GitHub Releases 的二進位皆附 SHA-256 checksum

### 不信任預編譯二進位？從原始碼建置

```bash
git clone https://github.com/zhixuli0406/DuDuClaw
cd DuDuClaw
cargo build --release
```

### 二進位驗證

每個 Release 都附帶 SHA-256 checksum，並透過 [cosign](https://github.com/sigstore/cosign) keyless 簽章：

```bash
# 從 Releases 下載
wget https://github.com/zhixuli0406/DuDuClaw/releases/download/v1.21.1/duduclaw-darwin-arm64.tar.gz

# 驗證 SHA-256（對照 release 內的 .sha256 檔）
shasum -a 256 -c duduclaw-darwin-arm64.tar.gz.sha256

# 驗證 cosign 簽章
cosign verify-blob \
  --certificate duduclaw-darwin-arm64.tar.gz.pem \
  --signature duduclaw-darwin-arm64.tar.gz.sig \
  --certificate-identity-regexp "https://github.com/zhixuli0406/DuDuClaw" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  duduclaw-darwin-arm64.tar.gz
```

### 供應鏈透明度

- **授權**：Apache 2.0
- **維護者**：嘟嘟數位科技有限公司（台灣登記公司，統編 94139082）
- **公開 commit 歷史**：github.com/zhixuli0406/DuDuClaw
- **CI/CD**：所有 Release 皆由 GitHub Actions 建置
- **無遙測（No telemetry）**：零 phone-home 連線
- **不蒐集 API 金鑰**：所有密鑰以 AES-256-GCM 留在你自己的機器上
- **不需特權提升**：完全在 user space 執行

漏洞回報請見 [SECURITY.md](SECURITY.md)。

---

## 為什麼選 DuDuClaw，而不是直接用原生 Claude / GPT / Gemini CLI？

如果你只是偶爾以個人身分使用單一 LLM，原生 CLI 已經很好用。
但一旦你要把東西推上生產環境，很快就會發現自己在重造 DuDuClaw 早就提供好的東西：

| 需求 | 原生 CLI | DuDuClaw |
|---|---|---|
| 多 LLM 自動容錯切換 | 手動重啟 | 內建（4 種策略） |
| 切換 LLM 時保留上下文 | 遺失 | 完整保留 |
| 工具跨 LLM 共用 | 每個 LLM 重寫 | 寫一次，到處共用 |
| 生產級韌性（DLQ／重試／可觀測性） | 自己造 | 內建 |
| 多通道（Telegram／LINE／Discord／…） | 僅限 CLI | 7 種通道 |
| 機密管理／稽核／PII 遮蔽 | 自己造 | 內建 |

如果你只是偶爾單獨用 `claude` 或 `gemini` — 用原生就好。
如果你要打造**生產級的多 LLM Agent 系統** — DuDuClaw 幫你省下 3 個月的基礎設施工作。

---

> 🎉 **v1.29.0 — Cloud 方案 agent/通道上限強制（自架不受限）**（[Release](https://github.com/zhixuli0406/DuDuClaw/releases/tag/v1.29.0)）
>
> 把 `features.toml` 裡「宣告了卻從未生效」的每方案 `max_agents` / `max_channels` 真正接上（Hobby 1 agent/1 通道、Solo 1/2、Studio 3/5）。**自架永不受限**（Apache 2.0 承諾），限制只套用在代管 Cloud 租戶。
>
> - **Cloud 方案資源上限** — `agents.create` / `channels.add` 達上限即擋並提示升級；以 `DUDUCLAW_DEPLOYMENT=cloud`（僅注入代管租戶容器）為閘，自架（預設）一律放行、`max_*=0` 亦代表無限
> - **軟上限提示** — 個人 Cloud 租戶接近上限時，dashboard 顯示具體用量（`Agent X/Y · 通道 A/B`）+ 升級 CTA，非阻擋、可關閉
> - `license_runtime::cap_exceeded()` 純函式 + `is_self_host_deployment()` 對 gateway 公開



https://github.com/user-attachments/assets/9f18408a-cf46-4db2-9ab0-dcc8db2486fc



<details>
<summary><strong>v1.9.4 → v1.28.x 累積亮點</strong></summary>

- **v1.28.0** — 合作夥伴（NFR）免費授權 + License 自助服務：免付費 Partner tier（不可轉售、解鎖等同 Self-Host Pro 模組）+ 夥伴碼自助兌換（`POST /v1/partner/redeem`）+ CLI `redeem/rebind/subscriptions` + 簽發自動寄 Key + 換機重簽 + 部署模式綁定（M51，`DUDUCLAW_DEPLOYMENT` cloud/自架，fail-closed）
- **v1.27.0** — 進階產業模板（Pro）：電商 / 醫美·牙醫 / 房仲 / 補教 4 個法規查證版模（SOUL + 法遵 CONTRACT + agent.toml + FAQ + 術語/SOP/合規 wiki，閉源）+ `premium_templates` 授權解鎖載入（fail-closed，公開 OSS binary 拿不到）+ wizard 進階產業選單與 upsell 引導
- **v1.26.0** — 個人版 / 企業版形態（`EditionProfile`，與 license tier 正交、不 gate 核心功能）+ Dashboard 一鍵 CLI 登入（PTY 驅動 Claude/Codex/Gemini/Antigravity 原生登入、貼碼回填、`remote_safe` 分類）+ Antigravity CLI 內建 server image + 個人版資料可攜（`export`/`import`）+ `PersonalProSelfHost` 自架個人版授權層（NT$490/月）
- **v1.25.0** — 瀏覽器優先的首次設定引導：`WelcomePage`（3 步，5 條 AI 後端路徑）+ `FirstRunGate` 零 Agent 導向 + 全頁面導覽 `GuidedTour`（零相依 spotlight）+ `runtime.detect` RPC 零設定開機
- **v1.24.0** — Antigravity CLI（`agy`）runtime 正式支援 · PtyPool 解除 Claude 綁定：新增 `RuntimeType::Antigravity`（oneshot `agy -p`、二進位自動解析、system prompt + 歷史內嵌、CJK-safe 截斷、預植 `trustedWorkspaces`）；`CliKind::Antigravity` 接上 PtyPool / worker spawn、`cli_kind_for_provider()` 依 `[runtime] provider` 推導；互動 REPL 仍 Claude-only（刻意）；舊 `gemini` 後端保留給付費 `GEMINI_API_KEY` / 企業版
- **v1.23.0** — Decision Continuity（RFC-24）：當 agent 向使用者提出列舉式選項（方案 A/B/C），每個選項固化進 Temporal Memory 的 semantic 層（獨立於對話壓縮），待決事項回合間重新注入；稍後「用方案 C」（跨回合 / session / 程序）從持久狀態解析而非猜測。偵測確定性、零 LLM；per-agent opt-in `[memory] decision_continuity = true`
- **v1.22.0** — RFC-26 Live Forking（round 1–4）：把進行中任務分叉成 N 個並行競爭分支、各自在 copy-on-write 隔離工作區嘗試不同策略、AI judge 挑勝者（`duduclaw-fork` + 6 MCP 工具 + 跨程序 `ForkStore` + `RotatingBranchExecutor` + `LiveAggregate` 預算搶占）；Skill 合成排程器（W19-P1）；Calm Glass dashboard 共用元件庫重構。皆預設關閉
- **v1.21.0** — RFC-25 §5 收尾：非 Claude（Codex / Gemini / OpenAI-compat）路徑補齊全部 11 項缺口成為一等公民（多輪上下文、成本遙測、keepalive、per-(home,provider) failover 退避）；`release.sh` 多平台版本同步 + 漂移偵測 + bump 後 assert + `verify` 查 registry，修好 PyPI 被 `skip-existing` 靜默凍版的問題
- **v1.20.0** — RFC-25 多模型解鎖 + A2A：「Multi-Runtime 四後端」過去是未編譯的孤兒程式碼，每條執行路徑都寫死 Claude。v1.20.0 把它接上電，所有呼叫 LLM 的子系統走單一 provider-agnostic choke-point（`runtime_dispatch::run_agent_prompt` + lazy 自動偵測的 `RuntimeRegistry`）；channel reply / GVU / 子 agent 派工在非 Claude provider 時走 choke-point（Claude 維持 OAuth 輪替 / PTY 路徑，零回歸）；ACP `tasks/send` 改為實際執行目標 agent 並正確回報 Failed / Completed；Phase 0 拆掉 GVU 演化模型硬鎖（reject → warn）

- **v1.19.0** — Memory Intelligence：把 W18/W19 設計但未實作的記憶層非侵入式落地於現行 Rust `SqliteMemoryEngine`。**Temporal Memory**（`memories` 加時態 / 知識圖譜欄位 + `store_temporal` 自動取代鏈 + `get_history`/`get_at`，搜尋預設只回有效記憶）；**Reflexion Loop**（橋接既有 `MistakeNotebook`：召回注入答題 prompt + 同 category ≥3 固化成 semantic 規則）；**`memory_fetch_batch`** MCP 工具（依 ID 批次讀取 ≤100，namespace/ownership 隔離）。`MemoryEntry` 不動，零破壞
- **v1.18.0** — Dashboard 預算／用量正確化：改讀持久化 `CostTelemetry` 帳本（取代一重建就歸零的記憶體計數），修 `cost_millicents` 單位誤稱、`marketplace.install` 補實作、設定持久化補洞、前端一輪 runtime bug 清掃 + 88 個 i18n key
- **v1.17.0** — RFC-24 License v2.0（Open Core 基礎）：新 crate `duduclaw-license`（verification-only 客戶端，簽章金鑰留在 `commercial/duduclaw-license`），7 個 tier 繼承鏈 `OpenSource` / `Hobby` / `Solo` / `Studio` / `Business` / `SelfHostPro` / `Oem`，Ed25519 trust registry 由 `DUDUCLAW_LICENSE_PUBKEY_<ID>` env 種子化（空 registry fail-safe 退回 OpenSource）。Apache 2.0 核心**無限制可用**，付費訂閱解鎖 `commercial/*` 加值模組
- **v1.16.0** — MCP Refresh Tokens + GVU `SoulPatchOp::Consolidate`：新模組 `mcp_refresh` 以 `~/.duduclaw/mcp_tokens.db` 後盾的長壽憑證（`ddc_refresh_<env>_<64hex>`、90 天、可撤銷、僅儲 hash），解決 Claude Desktop auth-fail 後靜默斷線不重試；GVU 新增 `SoulPatchOp::Consolidate` 變體帶「縮減不變式」，讓 SOUL.md 接近 150 行／8KB 硬上限時可自我觸發整合
- **v1.15.2** — `agent_update_soul` 信賴後門封補：原本寫 SOUL.md 後沒呼叫 `soul_guard::accept_soul_change` 更新完整性 hash，每次合法呼叫都會留下永久 stored-vs-current drift；且整條呼叫鏈不寫 `tool_calls.jsonl`，後門對事後分析完全隱形。v1.15.2 補齊 audit row（成功 + 四種拒絕路徑都記，hash 前綴 16 字元）並在每次寫入後同步 fingerprint
- **v1.15.1** — GVU SOUL.md 無界成長修補：agnes/SOUL.md 5 個 GVU cycle 從 61 行膨脹到 592 行。三層防禦：(1) `strip_proposal_meta` 在 legacy 路徑剝除 `## 診斷` / `## rationale` / `## expected_improvement` 等 meta 段；(2) `SOUL_MAX_LINES = 150` / `SOUL_MAX_BYTES = 8KB` 硬上限獨立於 ASI 內容權重門檻；(3) 新增 structured `SoulPatch { section, op, content }` 與 `apply_patch_to_soul`，Generator→Verifier→Updater 全鏈路打通
- **v1.15.0** — Cross-Platform PTY Pool + Worker：Anthropic 封鎖 OAuth 訂閱帳號的 `claude -p` 後的官方替代路徑。新 crate `duduclaw-cli-runtime`（`portable-pty` ConPTY/openpty 跨平台 + sentinel-framed in-band 協定 + `PtyPool` semaphore + idle eviction + supervisor + restart policy）與 `duduclaw-cli-worker`（localhost JSON-RPC + Bearer + `/healthz`，gateway 可選 in-process 或 out-of-process）；`channel_reply` OAuth 走 REPL / API-key 走 `oneshot_pty_invoke + claude -p`；Phase 8 `pty_pool_*` Prometheus 指標；所有失敗都 fallback 回 legacy `tokio::process::Command`。預設關閉，`agent.toml [runtime] pty_pool_enabled = true` 啟用
- **v1.14.0** — RFC-23 Sensitive Data Redaction：新 crate `duduclaw-redaction`，內部資料（Odoo / shared wiki / file tools）以 `<REDACT:CATEGORY:hash8>` token 取代後才送 LLM，受信邊界（user channel reply、whitelist 工具 egress）自動還原；AES-256-GCM 加密 SQLite vault（per-agent 32-byte key，0o600 權限）+ TTL 7d 兩階段 GC（mark→30 天 purge）+ 5 個內建 profile + 五層 enable/disable resolver + JSONL audit 10MB rotation
- **v1.13.1** — Odoo Test-Before-Save：`odoo.test` RPC 接受 inline params，Dashboard「測試連線」直接用表單目前的值打 Odoo 不必先儲存；inline credential 留空 fallback 已儲存的金鑰；同樣的 SSRF / HTTPS / db-name 驗證鏈、`scrub_odoo_error()` 截 240 字防 HTML 錯誤頁外洩
- **v1.13.0** — Runtime-health overhaul（16 個 issue / 兩輪修補）：恢復 GVU/SOUL 自我演化、新增 `[prompt] mode = "minimal"` Anthropic Skills 風格系統提示、`[budget] max_input_tokens` 壓縮管線、async session summarizer、TF-IDF wiki 相關性排序、`duduclaw lifecycle flush` 季度冷熱分離 CLI
- **v1.12.x** — W22-P0 ADR-002 `x-duduclaw` capability negotiation（HTTP 422 早期失敗）+ ADR-004 Secret Manager + RFC-22 多 agent 協調修補（agnes 偽造子 agent 回應 / autopilot 大量誤觸發 / channel 路徑 token 未紀錄）+ `duduclaw weekly-report` 子命令
- **v1.11.0** — RFC-21（[Issue #21](https://github.com/zhixuli0406/DuDuClaw/issues/21)）：`duduclaw-identity` crate（IdentityProvider trait + Wiki/Notion/Chained 三實作）+ Odoo per-agent 認證隔離（`OdooConnectorPool` 取代全域 admin 單例）+ shared wiki `.scope.toml` SoT 命名空間政策
- **v1.10.0** — Wiki RL Trust Feedback：`WikiTrustStore` per-agent SQLite trust、`CitationTracker` 雙級 LRU + bounded-time eviction 防 DoS、`WikiJanitor` 每日 pass（自動標 corrected / archive / frontmatter 同步）+ sub-agent turn_id 貫通 + multi-process flock + atomic batch upsert
- **v1.9.4** — `duduclaw-durability` 五大持久性機制（idempotency / retry / circuit breaker / checkpoint / DLQ）+ `duduclaw-governance` PolicyRegistry + MCP HTTP/SSE Transport + LOCOMO 記憶評測系統（每日 03:00 UTC 評測 + 200 筆 golden QA）+ LLM Fallback + Discord RESUME + Web ReliabilityPage

</details>

---

## 目錄

- [什麼是 DuDuClaw？](#what)
- [核心特色](#features)
- [競品對比](#comparison)
- [Agent 目錄結構](#directory)
- [Security Hooks 安全防禦系統](#security)
- [安裝](#install)
- [CLI 指令](#cli)
- [專案結構](#structure)
- [技術決策](#tech)
- [測試](#testing)
- [文件](#docs)
- [授權](#license)

---

<a id="what"></a>

## 什麼是 DuDuClaw？

DuDuClaw 是一個 **Multi-Runtime AI Agent 平台**——同時支援 **Claude Code / Codex / Gemini** 三大 CLI 作為 AI 後端，並透過統一的 `AgentRuntime` trait 實現無縫切換與自動偵測。

它不綁定任何單一 AI 供應商，而是為你的 AI Agent 接上通訊通道、記憶、自我進化、本地推論與帳號管理等完整基礎設施。

核心概念：

- **Multi-Runtime** — `AgentRuntime` trait 統一 Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，per-agent 設定
- **Plumbing = DuDuClaw** — 負責通道路由、session 管理、記憶搜尋、帳號輪替、本地推論等基礎設施
- **橋接 = MCP Protocol** — `duduclaw mcp-server` 作為 MCP Server，將通道與記憶工具暴露給 AI Runtime

```
AI Runtime (brain) — Claude CLI / Codex CLI / Gemini CLI / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat
  ├─ Multi-Runtime — Claude / Codex / Gemini / OpenAI-compat 自動偵測 + per-agent 設定
  ├─ Session Memory Stack — 原生 --resume + Instruction Pinning + Snowball Recap + Key-Fact Accumulator
  ├─ MCP Server — 80+ 工具（通訊、記憶、Agent、Skill、推論、任務、知識庫、ERP），per-agent 註冊
  ├─ Evolution Engine — GVU² 雙迴圈進化 + 預測驅動 + MistakeNotebook
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX / ONNX
  ├─ Voice Pipeline — ASR (SenseVoice / Whisper) + TTS (Piper / MiniMax) + VAD (Silero)
  ├─ Account Rotator — 多 OAuth + API Key 輪替、預算追蹤、健康檢查、Cross-Provider Failover
  ├─ Browser Automation — 5 層自動路由（API Fetch → Scrape → Headless → Sandbox → Computer Use）
  ├─ Worktree Isolation — Git worktree L0 沙箱、原子合併、每 Agent 5 個上限
  ├─ Wiki Knowledge Layer — L0-L3 四層知識架構 + 信任權重 + FTS5 + 自動注入
  ├─ ACP/A2A Server — `duduclaw acp-server` stdio JSON-RPC 2.0，Zed/JetBrains/Neovim 整合
  └─ Web Dashboard — React 19 SPA（23 頁面），透過 rust-embed 嵌入 binary
```

---

<a id="features"></a>

## 核心特色

### 通訊與通道

| 特色 | 說明 |
|------|------|
| **七通道支援** | Telegram（long polling）、LINE（webhook）、Discord（Gateway WebSocket，op 6 RESUME + stall watchdog + 1-5s jitter）、Slack（Socket Mode）、WhatsApp（Cloud API）、Feishu（Open Platform v2）、WebChat（WebSocket）|
| **Per-Agent Bot** | 每個 Agent 可擁有獨立的 Bot Token，同平台多 Agent 並行 |
| **通道熱啟停** | Dashboard 新增/移除通道即時生效，無需重啟 gateway |
| **WebChat** | 內建 `/ws/chat` WebSocket 端點，React 前端即時對話 |
| **Generic Webhook** | `POST /webhook/{agent_id}` + HMAC-SHA256 簽章驗證 |
| **Media Pipeline** | 圖片自動縮放（max 1568px）+ MIME 偵測 + Vision 整合 |
| **Sticker 系統** | LINE 貼圖目錄 + 情緒偵測 + Discord emoji 等價映射 |

### AI 執行與推論

| 特色 | 說明 |
|------|------|
| **MCP Server 架構** | `duduclaw mcp-server` 提供 80+ 工具，涵蓋通訊、記憶、Agent 管理、推論、排程、Skill 市場、任務看板、共享知識庫、Odoo ERP。註冊於每個 agent 目錄的 `.mcp.json`（Claude CLI `-p --dangerously-skip-permissions` 僅讀取專案級設定），gateway 啟動時自動建立/修復 |
| **MCP Refresh Tokens**（v1.16.0）| `~/.duduclaw/mcp_tokens.db` 後盾的長壽憑證 — token 形式 `ddc_refresh_<env>_<64hex>`、90 天壽命、可個別撤銷、僅儲 hash（原 token 永遠不落地）；`authenticate_from_env` 依 prefix 路由憑證，舊版 `ddc_<env>_<32hex>` 完整保留；新 CLI `duduclaw mcp { issue-refresh-token \| revoke-token \| list-tokens }` 解決 Claude Desktop 拿到 auth-fail 後靜默斷線不重試的痛點 |
| **Multi-Runtime** | `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，per-agent 設定 |
| **本地推論引擎** | 統一 `InferenceBackend` trait — llama.cpp（Metal/CUDA/Vulkan）/ mistral.rs（ISQ + PagedAttention）/ Exo P2P 叢集 / llamafile / MLX（Apple Silicon）/ OpenAI-compat HTTP |
| **三層信心路由** | LocalFast → LocalStrong → CloudAPI，基於啟發式信心評分自動分流，CJK-aware token estimation |
| **InferenceManager** | 多模式自動切換：Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API，週期性健康檢查 + 自動 failover |
| **原生多輪 Session** | Claude CLI `--resume` 搭配 SHA-256 確定性 session ID + history-in-prompt fallback（帳號輪替/stale session 自動重試）；Hermes 風格 turn trimming（>800 chars, CJK-safe）；Direct API "system_and_3" 斷點快取策略 |
| **Session 記憶堆疊** | Instruction Pinning（首訊 Haiku 擷取核心任務 → session prompt 尾端注入）+ Snowball Recap（每輪 `<task_recap>` 前置零成本回顧）+ P2 Key-Fact Accumulator（每輪 2-4 則事實 → FTS5 索引 → 注入 top-3，僅 100-150 tokens vs MemGPT 6,500 tokens，−87%）|
| **Claude CLI 輕量路徑** | `call_claude_cli_lightweight()` 以 `--effort medium --max-turns 1 --no-session-persistence --tools ""` 處理 metadata 任務（壓縮、instruction/key-fact 擷取），25-40% 成本節省 |
| **Claude CLI 穩定化旗標** | `--strict-mcp-config`（MCP 隔離）+ `--exclude-dynamic-system-prompt-sections`（跨輪 prompt 穩定，10-15% token 節省），`--bare` 因破壞 OAuth 鑰匙圈於 v1.8.11 移除 |
| **Direct API** | 繞過 CLI 直接呼叫 Anthropic Messages API，`cache_control: ephemeral` 達 95%+ 快取命中率 |
| **Token 壓縮** | Meta-Token（BPE-like 27-47%）、LLMLingua-2（2-5x 有損）、StreamingLLM（無限長對話）|
| **Cross-Provider Failover** | `FailoverManager` 健康追蹤、冷卻、不可重試錯誤偵測 |
| **Cross-Platform PTY Pool**（v1.15.0）| OAuth 帳號專用互動式 REPL 通道 — 跨平台 `portable-pty`（ConPTY on Win 10 1809+、openpty on Unix）+ sentinel-framed in-band 回應協定（無 scrollback scraping / 無 sidecar）+ per-agent semaphore + idle eviction + health-check supervisor + restart policy。預設關閉，per-agent `agent.toml [runtime] pty_pool_enabled = true` 開啟；可選 out-of-process 模式（`worker_managed = true`）把 pool 移到 `duduclaw-cli-worker` 子進程透過 localhost JSON-RPC 通訊 |
| **PTY Pool Observability** | Phase 8 production-rollout 指標 — `pty_pool_*` Prometheus counters（acquires / cache-hit / spawn / 三種驅逐原因 / 4 種 invoke outcome / duration histogram）+ `worker_health_misses_total` + `worker_restarts_total` + `pty_pool_managed_worker_active` 模式 gauge + `GET /api/runtime/status` JSON 端點（loopback-only） |
| **Browser 自動化** | 5 層路由（API Fetch → Static Scrape → Headless Playwright → Sandbox Container → Computer Use），deny-by-default |

### 語音與多媒體

| 特色 | 說明 |
|------|------|
| **ASR 語音辨識** | ONNX SenseVoice（本地）+ Whisper.cpp（本地）+ OpenAI Whisper API |
| **TTS 語音合成** | ONNX Piper（本地）+ MiniMax T2A |
| **VAD 語音活動偵測** | ONNX Silero VAD |
| **Discord 語音頻道** | Songbird 整合，Discord 語音對話 |
| **LiveKit 語音房** | WebRTC 多 Agent 語音會議 |
| **ONNX 嵌入** | BERT WordPiece tokenizer + ONNX Runtime 向量嵌入 |

### Agent 編排與進化

| 特色 | 說明 |
|------|------|
| **Sub-Agent 編排** | `create_agent` / `spawn_agent` / `list_agents` MCP 工具 + `reports_to` 組織層級 + D3.js 架構圖；system prompt 自動注入 "## Your Team" 子 Agent 名冊 + 長回報訊息自動分頁（Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900 byte budget，標籤 `📨 **agent** 的回報 (1/N)`）|
| **跨系統 prompt 注入** | CLAUDE.md + CONTRACT.toml（must_not/must_always）+ SOUL.md + Wiki L0+L1 + key_facts top-3 + pinned_instructions 於 CLI/channel/dispatcher 三條路徑一致注入，Claude/Codex/Gemini/OpenAI 四 runtime 行為對齊 |
| **孤兒回應恢復** | dispatcher 啟動時 `reconcile_orphan_responses` 掃描 `bus_queue.jsonl`，原子重播 crash/Ctrl+C/hotswap 後殘留的 `agent_response` callback |
| **GVU² 雙迴圈進化** | 外迴圈（Behavioral GVU — SOUL.md 進化）+ 內迴圈（Task GVU — 即時任務重試），MistakeNotebook 跨迴圈記憶 |
| **預測驅動進化** | Active Inference + Dual Process Theory，~90% 對話零 LLM 成本；MetaCognition 每 100 預測自校準閾值 |
| **4+2 層驗證** | L1-Format / L2-Metrics / **L2.5-MistakeRegression** / L3-LLMJudge / **L3.5-SandboxCanary** / L4-Safety，前 4 層零成本 |
| **Adaptive Depth** | MetaCognition 驅動 GVU 迭代深度（3-7 輪），根據歷史成功率自動調整 |
| **Deferred GVU** | gradient 累積 + 延遲重試（最多 3 次 deferral，72h 跨度 9-21 輪有效迭代）|
| **ConversationOutcome** | 零 LLM 對話結果偵測（TaskType / Satisfaction / Completion），zh-TW + en 雙語 |
| **SOUL.md 版控** | 24h 觀察期 + 自動回滾，atomic write（SHA-256 fingerprint）|
| **`SoulPatchOp::Consolidate`**（v1.16.0）| structured patch path 新增「縮減不變式」變體 — 語意等同 `Replace` 但 `apply_patch_to_soul` 在新內容沒比現有正文短時拒絕，LLM 可在 SOUL.md 接近 150 行 / 8KB 硬上限時自我觸發整合 |
| **`agent_update_soul` 信賴鏈**（v1.15.2）| 寫入後自動 `soul_guard::accept_soul_change` 同步完整性 fingerprint + 成功/四種拒絕路徑皆寫入 `tool_calls.jsonl`（hash 前綴 16 字元），封補 stored-vs-current drift 與後門隱形問題 |
| **Agent-as-Evaluator** | 獨立 Evaluator Agent（Haiku 成本控制）進行對抗式驗證，結構化 JSON verdict |
| **DelegationEnvelope** | 結構化交接協議 — context / constraints / task_chain / expected_output，向後相容 Raw payload |
| **TaskSpec 工作流** | 多步驟任務規劃 — dependency-aware scheduling / auto-retry（3x）/ replan（最多 2 次）/ persistence |
| **Orchestrator 模板** | 5 步規劃策略（Analyze → Decompose → Delegate → Evaluate → Synthesize）+ 複雜度路由 |
| **Skill 生命週期** | 7 階段管理 — Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| **Skill 自動合成** | 偵測重複領域缺口 → 從情境記憶合成新 Skill → 沙箱試用（TTL 管理）→ 跨 Agent 畢業升級 |
| **Task Board** | SQLite 任務管理 — 狀態/優先級/指派追蹤 + 即時 Activity Feed（WebSocket 推播）|
| **Autopilot 規則引擎** | 自動化任務委派、通知、Skill 觸發 — 支援任務建立/狀態變更/頻道訊息/閒置偵測/Cron 排程 |
| **共享知識庫** | `~/.duduclaw/shared/wiki/` 跨 Agent 共享知識（SOP、政策、產品規格）+ 作者歸屬 |
| **Wiki 知識分層** | Vault-for-LLM 啟發 — L0 Identity / L1 Core（自動注入每次對話）/ L2 Context（每日更新）/ L3 Deep（按需搜尋），每頁附 `trust` (0.0-1.0) 權重；FTS5 unicode61 tokenizer 支援 CJK 全文搜尋；`wiki_dedup` 偵測重複頁、`wiki_graph` 輸出 Mermaid 知識圖 |
| **Wiki 自動注入** | `build_system_prompt()` 自動將 L0+L1 頁面注入 WIKI_CONTEXT；涵蓋 CLI 互動、頻道回覆、dispatcher/cron 三條系統 prompt 組裝路徑，Claude/Codex/Gemini/OpenAI 四 runtime 一致 |
| **Git Worktree L0 隔離** | 每任務獨立 worktree 工作區（比容器沙箱便宜），atomic merge（dry-run pre-check + global `Mutex`），`wt/{agent_id}/{adjective}-{noun}` 友善分支名；每 agent 上限 5 個、全域 20 個；Snap workflow：create → execute → inspect → merge/cleanup |
| **ACP/A2A Protocol Server** | `duduclaw acp-server` 提供 stdio JSON-RPC 2.0 伺服器（`agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`），相容 Agent Client Protocol，支援 Zed / JetBrains / Neovim IDE 整合；輸出 `.well-known/agent.json` AgentCard |
| **Reminder 排程** | 一次性提醒（相對時間 `5m`/`2h`/`1d` 或 ISO 8601 絕對時間），`direct` 靜態訊息或 `agent_callback` 喚醒模式 |

### 可靠性與治理（v1.9.x 新增）

| 特色 | 說明 |
|------|------|
| **`duduclaw-durability` crate** | 五大持久性機制 — idempotency key 管理、指數退避重試（jitter）、三態斷路器（Closed/Open/HalfOpen）、checkpoint 斷點續傳、Dead Letter Queue 終態失敗訊息處理 |
| **`duduclaw-governance` crate** | PolicyRegistry + 4 種 PolicyType（Rate/Permission/Quota/Lifecycle）+ quota_manager（soft/hard 配額）+ error_codes（QUOTA_EXCEEDED / POLICY_DENIED 標準化）+ YAML 熱重載 + audit log |
| **LLM Fallback** | 主模型 timeout/503/429/overloaded 時自動切換 fallback 模型，`is_llm_fallback_error` / `should_attempt_model_fallback` 純函式，hard deadline 統一回傳 hard timeout 錯誤觸發 fallback |
| **Evolution Events 系統** | 30+ event schema、async emitter（batch + retry）、query 介面、reliability 機制；HTTP endpoint 暴露於 gateway，Web ReliabilityPage 視覺化 |
| **MCP HTTP/SSE Transport**（W20-P1/P2）| `duduclaw http-server --bind 127.0.0.1:8765` — `POST /mcp/v1/call`（單次 JSON-RPC 工具呼叫）+ `GET /mcp/v1/stream`（SSE 長連接事件流）+ `POST /mcp/v1/stream/call`（async + SSE push）+ Bearer 認證 + token bucket rate limit |
| **記憶 MCP scope 強制驗證** | `memory:read` / `memory:write` scope 在 `store/read/search` execute() 進入點檢查，修補 v1.9.3 之前任意有效 API Key 可繞過 scope 的認證缺口 |
| **LOCOMO 記憶評測** | `memory_eval/` — retrieval_accuracy / retention_rate / locomo_integrity_check + cron_runner（每日 03:00 UTC）+ 5 分鐘 smoke_test P0 + 200 筆 golden QA 黃金集 |

### 安全防護

| 特色 | 說明 |
|------|------|
| **Claude Code Security Hooks** | 三層漸進式防禦 — Layer 1 黑名單（<50ms）→ Layer 2 混淆偵測（YELLOW+）→ Layer 3 Haiku AI 研判（RED only）|
| **威脅等級狀態機** | GREEN → YELLOW → RED 自動升降級，24h 無事件降一級 |
| **SOUL.md 漂移檢測** | SHA-256 fingerprint 即時比對 |
| **Prompt Injection 掃描** | 6 類規則，XML 分隔標籤防注入 |
| **Secret 洩漏掃描** | 20+ 模式（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL 等）|
| **敏感檔案保護** | Read/Write/Edit 三方向保護 `secret.key`、`.env*`、`SOUL.md`、`CONTRACT.toml` |
| **行為契約** | `CONTRACT.toml` 定義 `must_not` / `must_always` 邊界 + `duduclaw test` 紅隊測試（9 項場景）|
| **統一多源審計日誌** | `audit.unified_log` 合併 4 條 JSONL（`security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl`）為統一信封（timestamp / source / event_type / agent_id / severity / summary / details），Logs 頁面支援來源篩選、嚴重度下拉、即時與歷史分頁 |
| **JSONL 審計日誌** | async 寫入，格式相容 Rust `AuditEvent` schema |
| **CJK-Safe 字串切片** | `truncate_bytes` / `truncate_chars` 新模組取代 31 處 `s[..s.len().min(N)]` byte-index 切片（修復 v1.8.11 多位元組 codepoint panic）|
| **Per-Agent 密鑰隔離** | AES-256-GCM 加密儲存，agent 間密鑰互不可見 |
| **容器沙箱** | Docker / Apple Container（`--network=none`、tmpfs、read-only rootfs、512MB limit）|
| **Browser 自動化** | 5 層路由（API Fetch → Static Scrape → Headless → Sandbox → Computer Use），deny-by-default |

### 帳號與成本

| 特色 | 說明 |
|------|------|
| **雙模式帳號輪替** | OAuth 訂閱（Pro/Team/Max）+ API Key 混合 — 4 策略（Priority/LeastCost/Failover/RoundRobin）|
| **健康追蹤** | Rate limit 冷卻（2min）、帳單耗盡冷卻（24h）、Token 過期追蹤（30d/7d 預警）|
| **成本遙測** | SQLite token 追蹤、快取效率分析、200K 價格懸崖警告、自適應路由（快取效率 <30% 自動切本地）|
| **Claude CLI 二進位探測** | `which_claude()` / `which_claude_in_home()` 掃描 Homebrew（Intel + Apple Silicon）/ Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf shims / NVM 版本目錄，修復 launchd 啟動時找不到 binary 的問題 |
| **結構化失敗分類** | `FailureReason` 枚舉（RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown）+ 分類 zh-TW 訊息 + `channel_failures.jsonl` 審計紀錄 |

### 整合與擴充

| 特色 | 說明 |
|------|------|
| **Odoo ERP 整合** | `duduclaw-odoo` 中間層 — 15 個 MCP 工具（CRM/銷售/庫存/會計/通用搜尋報表），支援 CE/EE，EditionGate 自動偵測。Dashboard 設定頁支援**測試後再儲存**（v1.13.1，credential 留空時 fallback 已儲存金鑰）+ **per-agent 認證隔離**（v1.11.0，`OdooConnectorPool` 取代全域 admin 單例）|
| **Skill 市場** | GitHub Search API 即時索引 + 24h 本地快取 + 安全掃描 + Dashboard 市場頁面 |
| **Prometheus 指標** | `GET /metrics` — requests、tokens、duration histogram、channel status |
| **CronScheduler** | `cron_tasks.jsonl` + cron 表達式，定時任務自動觸發 |
| **ONNX 嵌入** | BERT WordPiece tokenizer + ONNX Runtime 向量嵌入，語意搜尋支援 |
| **Experiment Logger** | Trajectory recording，支援 RL/RLHF 離線分析 |
| **Memory Decay 排程** | 每 24h 背景執行 `run_decay`：低重要度 + 30 天以上歸檔 → 封存 90 天以上永久刪除 |
| **RL Trajectory Collector** | 頻道互動期間寫入 `~/.duduclaw/rl_trajectories.jsonl`，`duduclaw rl` CLI 提供 export/stats/reward 功能，複合獎勵（outcome×0.7 + efficiency×0.2 + overlong×0.1）|
| **Marketplace RPC** | `marketplace.list` 服務真實 MCP 目錄（Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres, SQLite, Memory, Fetch, Brave Search），可透過 `~/.duduclaw/marketplace.json` 合併使用者自訂 |
| **Partner Portal** | SQLite `PartnerStore`（`~/.duduclaw/partner.db`）+ 7 RPCs（profile/stats/customers CRUD）+ 銷售統計 |

### Web Dashboard

| 特色 | 說明 |
|------|------|
| **技術棧** | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui，溫暖 amber 色系 |
| **24 個頁面** | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export / **Reliability**（v1.9.4 新增）|
| **Reliability 儀表板** | circuit breaker 狀態 / retry 統計 / DLQ 佇列深度 / evolution events 即時資料；`/reliability` 路由，整合 `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems` API |
| **即時日誌** | BroadcastLayer tracing → WebSocket 推播，WS 心跳 ping/pong（server 30s / client 25s）+ 60s 空閒關閉 |
| **Logs 歷史頁重寫** | 來源篩選 chips（全部 / 安全 / 工具呼叫 / 通道失敗 / 回饋）+ 即時人次計數 + 嚴重度下拉 + 嚴重度著色左框（emerald/amber/rose）+ 點擊展開 JSON 細節 |
| **Memory 頁 Key Insights** | 第四分頁呈現 P2 Key-Fact Accumulator 累積的結構化洞察（`key_facts` 表）+ `access_count` badge + 時間戳 + 來源 metadata |
| **Memory 頁演化歷史** | SOUL.md 版本歷史 + 前/後度量差異（positive feedback / prediction error / user corrections）+ 狀態徽章（Confirmed / RolledBack / Observing）|
| **Toast 通知系統** | 模組作用域事件匯流排、max-5 queue、自動關閉、暖色系 stone/amber/emerald/rose 變體、尊重 `prefers-reduced-motion` |
| **組織架構圖** | D3.js 互動式 Agent 層級視覺化 |
| **深淺色切換** | 跟隨系統偏好，支援手動切換 |
| **國際化** | zh-TW / en / ja-JP 三語支援（600+ 翻譯鍵）|
| **Skill Market 三分頁** | Marketplace / Shared Skills / My Skills 三分頁架構 + Skill 採用流程 |
| **Autopilot 設定** | 自動化規則建立/管理/監控 + 歷史紀錄檢視 |
| **Session Replay** | 對話回放元件，支援時間軸檢視 |

---

<a id="comparison"></a>

## 競品對比

| | **DuDuClaw** | **OpenClaw** | **IronClaw** | **Moltis** | **Dify** |
|---|---|---|---|---|---|
| 語言 | Rust | TypeScript | Rust | Rust | Python |
| 頻道 | 7 | 25+ | 8 | 5 | 0 (API) |
| Multi-Runtime | **4 後端（Claude/Codex/Gemini/OpenAI）** | - | - | - | 多 LLM |
| MCP Server | **80+ 工具** | - | - | - | - |
| 自我演化引擎 | **GVU² 雙迴圈** | - | - | - | - |
| 本地推論 | **6 後端 + 三層信心路由** | - | - | - | - |
| 語音 (ASR/TTS) | **4 ASR + 4 TTS provider** | - | - | - | - |
| Token 壓縮 | **3 種策略** | - | - | - | - |
| Browser 自動化 | **5 層路由** | - | - | - | - |
| 成本遙測 | **快取效率分析** | - | 基礎 | 基礎 | 基礎 |
| 行為合約 | **CONTRACT.toml + 紅隊** | - | WASM 沙箱 | - | - |
| ERP 整合 | **Odoo 15 工具** | - | - | - | - |
| 安全稽核 | **三層防禦 + Hooks** | CVE-2026-25253 | WASM | 基礎 | 中等 |
| 授權 | **Apache 2.0 (Open Core)** | MIT | 開源 | 開源 | $59+/月 |

---

<a id="directory"></a>

## Agent 目錄結構

每個 Agent 是一個資料夾，結構與 Claude Code 完全相容：

```
~/.duduclaw/agents/
├── dudu/                    # 主 Agent
│   ├── .claude/             # Claude Code 設定
│   │   └── settings.local.json
│   ├── .mcp.json            # MCP Server 設定（DuDuClaw platform tools + agent 專屬 MCP 如 Playwright）
│   │                        # gateway 啟動時自動建立/修復；Claude CLI `-p` 模式僅讀此檔
│   ├── SOUL.md              # 人格定義（SHA-256 保護）
│   ├── CLAUDE.md            # Claude Code 指引（含 CLAUDE_WIKI 模板）
│   ├── CONTRACT.toml        # 行為契約（must_not / must_always），自動注入 system prompt
│   ├── agent.toml           # DuDuClaw 設定（模型、預算、心跳、runtime、capabilities）
│   ├── SKILLS/              # 技能集（可由進化引擎自動產出）
│   ├── wiki/                # Wiki 知識庫（L0-L3 分層 + trust 權重 + FTS5）
│   ├── memory/              # 每日筆記 + memory.db（預測偏差）+ key_facts 表
│   ├── tasks/               # TaskSpec 工作流持久化（JSON）
│   └── state/               # 運行時狀態（SQLite：sessions.pinned_instructions 等）
│
└── coder/                   # 另一個 Agent
    └── ...
```

使用 `duduclaw migrate` 可將舊版 `agent.toml` 自動轉換為 Claude Code 相容格式。

---

<a id="security"></a>

## Security Hooks 安全防禦系統

DuDuClaw 在 Claude Code 的 Hook 系統上建構了三層漸進式防禦：

```
                    ┌─────────────────────────────────────┐
  SessionStart ──→  │ session-init.sh                     │  密鑰權限驗證 + 環境初始化
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  UserPrompt   ──→  │ inject-contract.sh                  │  CONTRACT.toml 規則注入
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ bash-gate.sh (Bash)                 │  Layer 1: 黑名單 (<50ms)
     (Bash)         │   ├─ Layer 2: 混淆偵測 (YELLOW+)    │  Layer 2: base64/eval/外滲
                    │   └─ Layer 3: Haiku AI (RED only)   │  Layer 3: AI 安全研判
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ file-protect.sh → ai-review.sh     │  敏感檔案保護 + AI 審查
  (Write|Edit|Read) └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PostToolUse  ──→  │ secret-scanner.sh → audit-logger.sh │  Secret 掃描 → async 審計
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  Stop         ──→  │ threat-eval.sh                      │  威脅等級重新評估
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  ConfigChange ──→  │ config-guard.sh                     │  配置篡改偵測
                    └─────────────────────────────────────┘
```

### 威脅等級狀態機

| 等級 | 觸發條件 | 防禦行為 |
|------|---------|---------|
| **GREEN** (預設) | 正常操作 | Layer 1 黑名單 + 檔案保護 + Secret 掃描 |
| **YELLOW** | 1 小時內 ≥ 2 次攔截 | +Layer 2 混淆偵測 + 外部網路限制 |
| **RED** | 偵測到注入/eval 攻擊 | +Layer 3 Haiku AI 研判所有命令 + AI 檔案審查 |

降級：24 小時無事件自動降一級（RED→YELLOW→GREEN）。

---

<a id="install"></a>

## 安裝

### npm（推薦，所有平台含 Windows）

```bash
npm install -g duduclaw
```

安裝完成後會自動下載對應平台的**預編譯 binary**（支援 macOS ARM64/x64、Linux x64/ARM64、Windows x64），**無需編譯器、無需 Rust、無需 MSVC Build Tools**。Windows 使用者只要先裝 [Node.js](https://nodejs.org/) 即可，這是唯一前置需求。

> **⚠️ 如果安裝過程要求你安裝 Rust / MSVC Build Tools（~2GB）並編譯（約 1.5 小時），代表你走錯路徑了。**
> 那是「[從原始碼建構](#從原始碼建構)」路徑，只有想改程式碼的貢獻者才需要。一般使用請務必用上面的 `npm install -g duduclaw`（或下方 Homebrew / 一行安裝），會直接下載官方預編譯 binary。

### Homebrew（macOS / Linux）

```bash
brew install zhixuli0406/tap/duduclaw
```

### 一行安裝

**macOS / Linux：**

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

**Windows（PowerShell）：**

```powershell
irm https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.ps1 | iex
```

> 一行安裝腳本會自動偵測**最新 release** 並下載對應平台的預編譯 binary，同樣免編譯。若 GitHub 下載失敗才會詢問是否改用原始碼建構（此時建議改用 `npm install -g duduclaw`）。可用環境變數 `DUDUCLAW_VERSION` 釘選特定版本。

### Python SDK（選用函式庫，非 CLI）

> **重要**：核心 gateway / CLI（`duduclaw` 指令）是 **Rust 二進位**，透過 **npm** 或 **Homebrew** 安裝即可獲得**完整功能**——Skill 安全掃描與通道回覆全部由 Rust-native 路徑處理，**不再需要任何 Python 依賴**。
> PyPI 上的 `duduclaw` 是一個**純 Python 函式庫**（供 `import duduclaw` 使用），**不含任何命令列工具**；因此 `pipx install duduclaw` 會失敗（No apps associated with package），這是預期行為。

`pip install duduclaw` **對核心功能而言是選用的**，只在下列情境才需要：

- 你想在自己的 Python 程式中 `import duduclaw`（agents / channels / mcp / memory_eval 模組）。
- 你要跑獨立的記憶評測工具（LOCOMO）。

> **進階本地推論（MLX 反思 / LLMLingua-2 壓縮）** 是另一組獨立 opt-in 功能，依賴的是 `mlx_lm`、`llmlingua` 這類 ML 套件，**而非** `duduclaw` 這個 PyPI 套件。需要時請依 `inference.toml` 個別安裝。

若要安裝這個選用函式庫：

```bash
pip install duduclaw
```

此命令會安裝以下依賴：

| 套件 | 最低版本 | 用途 |
|------|---------|------|
| `anthropic` | ≥ 0.40 | 在自有 Python 程式中直接呼叫 Claude API |
| `httpx` | ≥ 0.27 | 非同步 HTTP 客戶端（帳號輪替、健康檢查）|
| `pyyaml` | ≥ 6.0 | 設定檔解析 |

#### macOS（Homebrew Python）／其他 externally-managed 環境

系統若回報 `error: externally-managed-environment`（[PEP 668](https://peps.python.org/pep-0668/)），代表禁止直接裝進系統 Python。請改用虛擬環境：

```bash
# venv
python3 -m venv .venv && source .venv/bin/activate
pip install --upgrade duduclaw

# 或使用 uv（本專案已採用，速度更快）
uv venv && uv pip install --upgrade duduclaw
```

驗證安裝版本：

```python
import duduclaw
print(duduclaw.__version__)   # 反映實際安裝的 PyPI 版本
```

> `__version__` 透過 `importlib.metadata` 從已安裝套件的 metadata（`pyproject.toml`）動態讀取，原始碼樹（未 pip 安裝）則回退到內建字串，並由 `scripts/release.sh` 的漂移守衛與其他平台版號一同同步。

開發環境額外安裝：

```bash
pip install duduclaw[dev]
# 包含：pytest>=8, pytest-asyncio>=0.24, ruff>=0.8
```

### 從原始碼建構

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# （選用）僅在需要 import duduclaw 函式庫或記憶評測工具時安裝；核心建構不需要
# pip install duduclaw

# 建構 Dashboard
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# 建構 Rust binary（含 Dashboard）
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# 首次設定
./target/release/duduclaw onboard

# 啟動
./target/release/duduclaw run
```

> **前置需求**：[Rust](https://rustup.rs/) 1.85+、[Python](https://www.python.org/) 3.9+、[Node.js](https://nodejs.org/) 20+，以及至少一個 AI CLI：[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Gemini CLI](https://github.com/google-gemini/gemini-cli)（擇一或多個）

---

<a id="cli"></a>

## CLI 指令

```
duduclaw onboard             # 互動式首次設定
duduclaw run                 # 一鍵啟動（gateway + channels + heartbeat + cron + dispatcher）
duduclaw migrate             # 將 agent.toml 轉換為 Claude Code 格式
duduclaw mcp-server          # 啟動 MCP Server（供 AI Runtime 使用，stdio JSON-RPC 2.0）
duduclaw http-server         # 啟動 MCP HTTP/SSE Transport（Bearer 認證，預設 127.0.0.1:8765）
duduclaw acp-server          # 啟動 ACP/A2A Server（IDE 整合：Zed/JetBrains/Neovim）
duduclaw gateway             # 僅啟動 WebSocket gateway server

duduclaw agent               # CLI 互動式對話
duduclaw agent list          # 列出所有 Agent
duduclaw agent create        # 建立新 Agent（可指定產業模板）
duduclaw agent inspect       # 檢視 Agent 詳情
duduclaw agent pause         # 暫停 Agent
duduclaw agent resume        # 恢復 Agent
duduclaw agent edit          # 編輯 Agent 設定
duduclaw agent remove        # 移除 Agent

duduclaw test <agent>        # 紅隊安全測試（9 項內建場景 + JSON 報告）
duduclaw status              # 系統健康快照
duduclaw doctor              # 健康診斷
duduclaw wizard              # 產業模板互動式設定
duduclaw evolution finalize  # 一次性回收逾期 SOUL.md 觀察視窗（--dry-run / --agent <id>）

duduclaw rl export           # 匯出 RL trajectory（~/.duduclaw/rl_trajectories.jsonl）
duduclaw rl stats            # 每 Agent trajectory 統計
duduclaw rl reward           # 計算複合獎勵（outcome×0.7 + efficiency×0.2 + overlong×0.1）

duduclaw service install     # 安裝為系統服務
duduclaw service start/stop  # 啟停系統服務
duduclaw service status      # 服務狀態
duduclaw service logs        # 服務日誌
duduclaw service uninstall   # 移除系統服務

duduclaw license activate    # 啟用授權
duduclaw license status      # 授權狀態
duduclaw license verify      # 驗證授權
duduclaw update              # 檢查並安裝更新
duduclaw version             # 版本資訊
```

---

<a id="structure"></a>

## 專案結構

```
DuDuClaw/
├── crates/                         # Rust crates (20 個)
│   ├── duduclaw-core/              # 共用型別、traits (Channel, MemoryEngine)、錯誤定義
│   ├── duduclaw-agent/             # Agent 註冊、心跳、預算、契約、skill loader/registry
│   ├── duduclaw-auth/              # 多用戶認證（Argon2 密碼、JWT、ACL 角色權限）
│   ├── duduclaw-security/          # AES-256-GCM、SOUL guard、input guard、audit、key vault
│   ├── duduclaw-container/         # Docker / Apple Container / WSL2 沙箱執行
│   ├── duduclaw-memory/            # SQLite + FTS5 全文搜尋 + 向量嵌入 + 評測 batch query API
│   ├── duduclaw-inference/         # 本地推論引擎（llama.cpp / mistral.rs / ONNX / Exo / llamafile）
│   ├── duduclaw-gateway/           # Axum 伺服器、7 通道、session、GVU²、prediction、cron、dispatcher、LLM fallback、evolution events、PTY pool 整合
│   ├── duduclaw-bus/               # tokio broadcast + mpsc 訊息路由
│   ├── duduclaw-bridge/            # PyO3 Rust↔Python 橋接層
│   ├── duduclaw-odoo/              # Odoo ERP 中間層 (JSON-RPC, CE/EE, 15 MCP tools)
│   ├── duduclaw-cli/               # clap CLI 入口 + MCP server (stdio + HTTP/SSE) + migrate + test
│   ├── duduclaw-dashboard/         # rust-embed 嵌入 React SPA
│   ├── duduclaw-desktop/           # 桌面端 wrapper（macOS/Windows/Linux）
│   ├── duduclaw-durability/        # 持久性框架（idempotency / retry / circuit breaker / checkpoint / DLQ）— v1.9.4 新增
│   ├── duduclaw-governance/        # PolicyRegistry / quota_manager / error_codes / audit / approval — v1.9.4 新增
│   ├── duduclaw-identity/          # IdentityProvider trait + Wiki/Notion/Chained 三實作 — v1.11.0 新增
│   ├── duduclaw-redaction/         # 來源感知 redaction + 可還原 vault（AES-256-GCM）+ 5 profile + JSONL audit — v1.14.0 新增
│   ├── duduclaw-cli-runtime/       # 跨平台 PTY pool runtime（portable-pty / sentinel-framed）— v1.15.0 新增
│   └── duduclaw-cli-worker/        # standalone PTY pool worker subprocess（localhost JSON-RPC + Bearer token）— v1.15.0 新增
│
├── python/duduclaw/                # Python 擴充層
│   ├── channels/                   # LINE / Telegram / Discord 通道插件
│   ├── sdk/                        # Claude Code SDK chat + 多帳號輪替
│   ├── evolution/                  # Skill Vetter 安全掃描
│   ├── tools/                      # Agent 動態管理工具
│   ├── agents/                     # capability manifest + capability-based router + memory_resolver（v1.9.4）
│   ├── mcp/                        # MCP API Key auth（含 key masking）+ memory tools（store/read/search/namespace/quota）
│   └── memory_eval/                # LOCOMO 記憶評測（retrieval/retention + cron + 200 筆 golden QA）— v1.9.4 新增
│
├── npm/                            # npm 發布套件
│   ├── duduclaw/                   # 主套件（平台無關 wrapper + postinstall binary 下載）
│   ├── darwin-arm64/               # macOS Apple Silicon 預編譯 binary
│   ├── darwin-x64/                 # macOS Intel 預編譯 binary
│   ├── linux-x64/                  # Linux x86-64 預編譯 binary
│   ├── linux-arm64/                # Linux ARM64 預編譯 binary
│   └── win32-x64/                  # Windows x64 預編譯 binary
│
├── web/                            # React Dashboard
│   └── src/
│       ├── components/             # UI 元件 (OrgChart, ApprovalModal, SessionReplay)
│       ├── pages/                  # 24 個頁面（含 ReliabilityPage v1.9.4 新增）
│       ├── stores/                 # Zustand 狀態管理 (8 stores)
│       ├── lib/                    # API client (WebSocket JSON-RPC + evolution events / reliability HTTP)
│       └── i18n/                   # zh-TW / en / ja-JP
│
├── templates/                      # 產業模板 + Agent 角色模板
│   ├── restaurant/                 # 餐飲業（客服、訂位、FAQ、主動推播）
│   ├── manufacturing/              # 製造業（設備監控、SOP、異常告警）
│   ├── trading/                    # 貿易業（報價、訂單、庫存、價目表）
│   ├── evaluator/                  # Evaluator Agent（對抗式驗證）
│   ├── orchestrator/               # Orchestrator Agent（任務編排）
│   └── wiki/                       # Wiki 知識庫模板
│
├── .claude/                        # Claude Code Hook 安全系統
│   ├── settings.local.json         # Hook 設定（6 事件 × 10 腳本）
│   └── hooks/                      # 三層漸進式防禦腳本
│
├── docs/                           # 公開文件
│   ├── spec/                       # 格式規範（SOUL.md / CONTRACT.toml）
│   ├── api/                        # WebSocket RPC + OpenAPI spec
│   ├── guides/                     # 開發指南（自訂 MCP 工具等）
│   └── *.md                        # 架構、部署、進化引擎等
│
├── ARCHITECTURE.md                 # 完整架構設計文件
└── CLAUDE.md                       # AI 協作設計上下文
```

---

<a id="tech"></a>

## 技術決策

| 項目 | 選擇 | 理由 |
|------|------|------|
| AI 對話 | **Multi-Runtime（Claude / Codex / Gemini CLI）** | 不綁定單一供應商、自動偵測 + per-agent 設定 |
| 核心語言 | **Rust** | 記憶體安全、高效能、單 binary 部署 |
| 擴充語言 | **Python (PyO3)** | Claude Code SDK 整合、通道插件彈性 |
| 前端框架 | **React 19 + TypeScript** | 即時資料更新、生態成熟 |
| UI 風格 | **shadcn/ui + Tailwind CSS 4** | 溫暖可自訂、效能佳 |
| 資料庫 | **SQLite + FTS5** | 零依賴、嵌入式、全文搜尋 |
| 工具協議 | **MCP (Model Context Protocol)** | Claude Code 原生支援、stdin/stdout JSON-RPC |
| 本地推論 | **ONNX Runtime + llama.cpp** | 跨平台、Metal/CUDA/Vulkan GPU 加速 |
| 語音辨識 | **SenseVoice + Whisper.cpp** | 多語言、本地離線、零 API 成本 |
| 即時通訊 | **WebRTC (LiveKit)** | 低延遲語音、多人會議 |

---

<a id="testing"></a>

## 測試

```bash
# Rust 測試
cargo test --workspace --exclude duduclaw-bridge

# Python 測試
pip install pytest pytest-asyncio ruff
ruff check python/
pytest tests/python/ -v

# 前端型別檢查
cd web && npx tsc --noEmit
```

---

<a id="docs"></a>

## 文件

- [ARCHITECTURE.md](ARCHITECTURE.md) — 完整系統架構設計
- [CLAUDE.md](CLAUDE.md) — AI 協作設計上下文與原則
- [CHANGELOG.md](CHANGELOG.md) — 版本變更紀錄
- [docs/features/README.md](docs/features/README.md) — 特色功能詳解（19 篇，含 zh-TW / ja-JP 翻譯）
- [docs/features/feature-inventory.md](docs/features/feature-inventory.md) — 完整功能清單
- [docs/spec/soul-md-spec.md](docs/spec/soul-md-spec.md) — SOUL.md 格式規範 v1.0
- [docs/spec/contract-toml-spec.md](docs/spec/contract-toml-spec.md) — CONTRACT.toml 格式規範 v1.0
- [docs/api/README.md](docs/api/README.md) — WebSocket RPC 協議 + JSON-RPC 2.0 介面
- [docs/architecture/evolution-engine.md](docs/architecture/evolution-engine.md) — Evolution Engine v2 設計文件
- [docs/guides/deployment-guide.md](docs/guides/deployment-guide.md) — 生產環境部署指南
- [docs/guides/development-guide.md](docs/guides/development-guide.md) — 開發者設定與 Agent 開發
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md) — 自訂 MCP 工具教學

---

<a id="license"></a>

## 授權

**Open Core 模式** — 核心程式碼採用 [Apache License 2.0](LICENSE)，完全自由使用、修改、分發。

商業加值模組（`commercial/` 目錄）為閉源付費，包含：產業模板、演化參數集、企業儀表板、授權驗證。

詳見 [LICENSING.md](LICENSING.md)。

---

<p align="center">
  🐾 Built with louis.li
</p>
