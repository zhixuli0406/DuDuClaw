# DuDuClaw Knowledge Base (imported)

DuDuClaw 相關的設計／架構文件，自共享 wiki（`~/.duduclaw/shared/wiki/`）匯入，
經敏感度審查後判定為**可公開**（內容多已反映於專案 README / CLAUDE.md）。

匯入日期：2026-06-11

## 目錄

| 子目錄 | 內容 |
|--------|------|
| `specs/` | 技術規格（MCP Server、A2A Bridge、EvolutionEvents、HandoffPacket、Checkpoint Schema） |
| `architecture/` | 架構設計（Reflexion Loop） |
| `decisions/` | 架構決策紀錄（ADR） |
| `w19/` | W19 sprint 技術設計與實作狀態（MCP memory 端點、skill synthesis、軌跡品質評分） |
| `sprint-n/` | Sprint N EvolutionEvents schema ADR 與事件發射點盤點 |
| `reports/` | 交付／驗收／調研報告（公開技術主題） |
| `drafts/` | 對外技術部落格草稿（DuDuClaw 演化日誌架構） |

## 敏感度分流

匯入時依敏感度拆分：
- **可公開** → 本目錄（主 repo `wiki/`，會進公開 open-source repo）。
- **內部機密** → `commercial/docs/wiki/`（gitignore，閉源）：競品分析、PM 路線圖、內部團隊政策。

非 DuDuClaw 的內容（`xianwen/` 仙問 Online 專案、`core.md`、泛用 `research/`）未匯入。
