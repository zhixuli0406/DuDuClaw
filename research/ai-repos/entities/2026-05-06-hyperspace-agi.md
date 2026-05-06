---
title: Hyperspace AGI — 首個分散式 P2P AGI 系統
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-repos-researcher
tags: [repo, agi, agent, distributed, p2p, llm]
related: []
sources: [github]
layer: context
trust: 0.5
---

## Repo 資訊

- **URL**: https://github.com/hyperspaceai/agi
- **作者/組織**: Hyperspace AI
- **主要語言**: Python
- **技術棧**: libp2p、DiLoCo 分散式訓練、SparseLoCo 梯度壓縮、CRDT leaderboard
- **授權**: MIT

## 核心功能

Hyperspace AGI 是一個完全點對點的分散式 AGI 系統，數千個自主 AI Agent 透過 P2P gossip 協議協同訓練模型並分享實驗結果。系統內置 6 個全球 bootstrap 節點，Agent 可從瀏覽器或 CLI 加入網路，貢獻算力並獲得積分。截至 2026 年 4 月，網路內 660+ 個 Agent 已完成 27,247 次實驗，涵蓋 5 個研究領域，產出超過 101,000 個 block。

## 亮點與創新點

- **DiLoCo 分散式訓練**: 32 個節點在 24 小時內完成語言模型訓練，SparseLoCo 實現 195× 梯度壓縮
- **Pods（私人 AI 叢集）**: 小型團體可將機器池化為共享 AI 叢集，支援分散式推論與 API 共用
- **CRDT Leaderboard**: 每小時將網路狀態 JSON 快照發布到 repo，完全透明可驗證
- **Hyperspace A1 鏈**: 使用 Mysticeti 共識的專屬區塊鏈，支援 Agent 間微支付與交易
- **五大研究領域**: 機器學習、SEO、金融分析、技能/工具、公益事業

## Stars 數量與趨勢

- **Stars**: 1,600（截至 2026-05-06）
- **Forks**: 172
- **趨勢**: 新興專案，技術概念激進，P2P 分散式 AI 訓練方向的代表性實驗

## 適用場景

- 去中心化 AI 訓練與協作研究
- P2P 算力共享網路建設
- AGI 系統架構實驗與探索
- 分散式機器學習（DiLoCo）技術驗證
