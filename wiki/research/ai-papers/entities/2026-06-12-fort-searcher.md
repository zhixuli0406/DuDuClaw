---
title: FORT-Searcher：合成捷徑抵抗型搜尋任務以訓練深度搜尋 Agent
created: 2026-06-12T08:00:00Z
updated: 2026-06-12T08:00:00Z
author: ai-papers-researcher
tags: [AI, LLM, agent, search, training-data]
related: []
sources: [arxiv]
layer: context
trust: 0.5
---

## 核心貢獻

FORT-Searcher 揭示現有深度搜尋 agent 訓練資料的根本缺陷：「結構複雜性並不保證真實搜尋難度」，因為 agent 可透過捷徑繞過預期的搜尋過程。論文識別四類捷徑風險，並提出 FORT 框架合成捷徑抵抗型訓練資料，僅透過 SFT 即可在深度搜尋 benchmark 上達到具競爭力的表現。

## 方法摘要

- **四類捷徑風險識別**：
  1. Evidence Co-Coverage（證據共覆蓋）
  2. Single-Clue Selectivity（單線索選擇性）
  3. Exposed Constants（外露常數）
  4. Prior-Knowledge Binding（先驗知識綁定）
- **FORT 框架四步驟**：實體選擇 → 證據圖構建 → 問題表述 → 對抗精煉
- **訓練策略**：SFT only（不依賴 RL），降低訓練複雜度

## 實驗結果

- FORT 資料集呈現「更長的預先搜尋路徑、更少的捷徑模式」
- FORT-Searcher 在深度搜尋 benchmark 上達到具競爭力表現（僅 SFT）
- 作者 12 人，含中國人民大學 Wayne Xin Zhao 團隊

## 重要性評估

深度搜尋 agent 是知識密集型問答、事實核查的關鍵技術。FORT 揭示了訓練資料品質而非模型規模才是深度搜尋的核心瓶頸，其「捷徑風險分類法」為後續資料集設計提供系統性框架。對構建可靠搜尋 agent 的 DuDuClaw 應用場景具直接參考價值。

## 原文連結

- arXiv: https://arxiv.org/abs/2606.12087
- 作者：Jia Deng, Yimeng Chen, Xiaoqing Xiang, Ziyang Zeng, Shuo Tang, Wayne Xin Zhao, Feng Chang, Chuan Hao, Yuan Wei, Ran Tao, Bryan Dai, Ji-Rong Wen
- 發表日期：2026-06-10
