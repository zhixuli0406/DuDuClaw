---
title: InterleaveThinker：透過強化學習賦予圖像生成器交織生成能力
created: 2026-06-12T08:00:00Z
updated: 2026-06-12T08:00:00Z
author: ai-papers-researcher
tags: [AI, LLM, multimodal, agent, reinforcement-learning]
related: []
sources: [arxiv]
layer: context
trust: 0.5
---

## 核心貢獻

InterleaveThinker 是首個讓任意既有圖像生成器具備**交織文字—圖像序列生成**能力的多 agent 流水線。系統由 planner agent（組織輸入序列）與 critic agent（評估輸出並精煉指令）協作，結合監督微調資料集與強化學習（accuracy reward + step-wise reward）優化多步生成軌跡。

## 方法摘要

- **雙 agent 架構**：planner 負責序列規劃，critic 負責品質回饋與指令修正
- **RL 獎勵設計**：准確率獎勵（correctness）搭配逐步獎勵（step-wise），引導多步生成軌跡
- **SFT 資料集**：提供監督微調基礎，降低冷啟動難度
- **相容性**：可掛接於任意現有圖像生成器，無需重新訓練底座模型

## 實驗結果

- 在交織生成 benchmark 上與 Nano Banana 及 GPT-5 表現相當
- 同時提升 WISE 和 RISE 等推理導向任務的表現
- 對多種圖像生成器均呈現性能提升

## 重要性評估

現有多模態模型多以單輪圖文輸出為主，缺乏長程交織生成能力。InterleaveThinker 透過多 agent + RL 的架構填補此缺口，對多模態 agent、故事生成、視覺推理鏈等應用場景具有重要參考價值。其「不改動底座模型、外掛能力」的設計思路亦值得後續研究借鑑。

## 原文連結

- arXiv: https://arxiv.org/abs/2606.13679
- 作者：Dian Zheng, Harry Lee, Manyuan Zhang, Kaituo Feng, Zoey Guo, Ray Zhang, Hongsheng Li
- 發表日期：2026-06-11
