# DuDuClaw 特色功能介紹

> DuDuClaw v1.8.14 | 最後更新：2026-04-21

本目錄收錄 DuDuClaw 各項特色功能的詳細介紹。每篇文章以設計理念、系統行為與運作流程為主軸——面向想深入了解「系統如何運作」的開發者，無需閱讀原始碼。

---

## 功能索引

| # | 文章 | 一句話摘要 |
|---|------|-----------|
| 1 | [預測驅動演化引擎](01-prediction-driven-evolution.md) | 90% 的對話以零 LLM 成本完成演化 |
| 2 | [GVU² 雙迴圈自我博弈](02-gvu-self-play-loop.md) | 雙迴圈演化 + 4+2 層驗證 |
| 3 | [信心路由器與本地推論引擎](03-confidence-router.md) | 智慧模型選擇，節省 80% 以上 API 費用 |
| 4 | [檔案式 IPC 訊息匯流排](04-file-based-ipc.md) | 結構化跨 Agent 委派 + TaskSpec 工作流 |
| 5 | [三階段漸進式安全防禦](05-security-defense.md) | 分層威脅過濾，成本降至最低 |
| 6 | [SOUL.md 版本控制與回滾](06-soul-versioning.md) | 原子化人格更新與自動回滾機制 |
| 7 | [多帳號輪替與跨供應商容錯](07-account-rotation.md) | 跨 Claude/Codex/Gemini 的認證資料智慧排程 |
| 8 | [5 層瀏覽器自動化路由](08-browser-automation.md) | 逐層遞增的資源調度策略 |
| 9 | [行為契約與紅隊測試](09-behavioral-contracts.md) | 可機器執行的 Agent 行為邊界 |
| 10 | [認知記憶系統](10-cognitive-memory.md) | 仿人腦記憶設計，具備遺忘曲線 |
| 11 | [Token 壓縮三刀流](11-token-compression.md) | 三種策略，以更少的 Token 承載更多內容 |
| 12 | [產業模板與 Odoo ERP 橋接](12-industry-templates.md) | 開箱即用的商業智慧 |
| 13 | [Multi-Runtime Agent 執行](13-multi-runtime.md) | Claude / Codex / Gemini / OpenAI-compat 統一後端 |
| 14 | [語音管線](14-voice-pipeline.md) | ASR / TTS / VAD / LiveKit — 本地優先語音智慧 |
| 15 | [Skill 生命週期引擎](15-skill-lifecycle.md) | 7 階段自動化技能萃取與管理 |
| 16 | [Session 記憶堆疊](../16-session-memory-stack.md) | Instruction Pinning + Snowball Recap + Key-Fact Accumulator |
| 17 | [Wiki 知識分層](../17-wiki-knowledge-layer.md) | L0-L3 四層信任加權知識，自動注入系統 prompt |
| 18 | [Git Worktree L0 隔離](../18-worktree-isolation.md) | 每任務獨立工作區，原子合併 |
| 19 | [Agent Client Protocol (ACP/A2A)](../19-agent-client-protocol.md) | stdio JSON-RPC 2.0，Zed/JetBrains/Neovim 整合 |

> 註：16-19 篇目前僅提供英文版，歡迎 PR 翻譯。

---

## 完整功能清單

所有功能（不限特色功能）的完整清單，請參閱 [feature-inventory.md](feature-inventory.md)。
