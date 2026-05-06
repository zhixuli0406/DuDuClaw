---
title: 遞迴多智能體系統：以潛在空間遞迴計算實現協作推理
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-papers-researcher
tags: [AI, LLM, multi-agent, reasoning, inference-efficiency]
related: []
sources: [arxiv]
layer: context
trust: 0.5
---

# Recursive Multi-Agent Systems
**遞迴多智能體系統：以潛在空間遞迴計算實現協作推理**

## 基本資訊

- **作者：** Xiyuan Yang, Jiaru Zou, Rui Pan, Ruizhong Qiu, Pan Lu, Shizhe Diao, Jindong Jiang, Hanghang Tong, Tong Zhang, Markus J. Buehler, Jingrui He, James Zou
- **機構：** Stanford University 等
- **發表日期：** 2026-04-28
- **來源：** arXiv cs.AI / cs.CL / cs.LG
- **論文連結：** https://arxiv.org/abs/2604.25917
- **程式碼：** https://recursivemas.github.io

## 核心貢獻

本文提出 **RecursiveMAS**，將整個多智能體系統轉化為一個統一的潛在空間遞迴計算框架。透過輕量級協調模組，讓多個智能體的推理過程在潛在空間中迭代疊加，而非在 token 層級進行串行通訊，從根本上改變了多智能體協作的計算範式。

## 方法摘要

- 將傳統 token 層級的智能體間溝通替換為**潛在空間遞迴計算**（latent-space recursive computation）
- 引入輕量級協調模組（coordination module）實現跨智能體信息整合
- 支援在數學、科學、醫學、搜尋、程式碼生成等多類型任務上的統一框架

## 實驗結果

| 指標 | 提升幅度 |
|------|---------|
| 平均準確率 | +8.3% |
| 端到端推理速度 | 1.2×–2.4× 加速 |
| Token 用量減少 | 34.6%–75.6% |

測試任務涵蓋：數學推理、科學問答、醫學診斷、網路搜尋、程式碼生成。

## 重要性評估

RecursiveMAS 同時實現了**準確率提升、速度加速與 token 節省**，三者同向改善在多智能體研究中極為罕見。此框架將遞迴縮放原則（recursive scaling）從單一模型延伸至多智能體系統，對 LLM 推理效率與 AGI 架構設計具有重要啟示。在 Hugging Face trending 排行榜以 254 upvotes 領先，顯示社群高度關注。
