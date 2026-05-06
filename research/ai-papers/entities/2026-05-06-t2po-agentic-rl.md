---
title: T²PO：不確定性引導的探索控制，用於穩定多輪 Agentic 強化學習
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-papers-researcher
tags: [AI, LLM, reinforcement-learning, agent, reasoning, ICML]
related: []
sources: [arxiv]
layer: context
trust: 0.5
---

# T²PO: Uncertainty-Guided Exploration Control for Stable Multi-Turn Agentic Reinforcement Learning
**T²PO：不確定性引導的探索控制，用於穩定多輪 Agentic 強化學習**

## 基本資訊

- **作者：** Haixin Wang, Hejie Cui, Chenwei Zhang, Xin Liu, Shuowei Jin, Shijie Geng, Xinyang Zhang, Nasser Zalmout, Zhenyu Shi, Yizhou Sun
- **機構：** 未在摘要頁顯示，共 10 位作者
- **發表日期：** 2026-05-04
- **來源：** arXiv；已獲接受為 **ICML 2026 Spotlight Paper**
- **論文連結：** https://arxiv.org/abs/2605.02178

## 核心貢獻

T²PO 解決多輪 agentic 強化學習訓練中的不穩定性問題（training instability），提出雙層不確定性引導探索控制機制：token 層級與 turn 層級分別監控策略探索效率，在探索停滯時觸發干預（thinking intervention）或重新取樣（resampling），有效防止策略崩潰（policy collapse）。

## 方法摘要

- **診斷根因**：認定訓練不穩定的核心原因在於「低資訊量探索」—— 策略生成的行動缺乏資訊價值
- **Token 層級（T¹）**：即時監控 token 生成的不確定性改善程度；當改善停滯時觸發 thinking intervention
- **Turn 層級（T²）**：分析每輪對話的進展貢獻；識別並重新取樣低進展互動
- **雙層協同**：兩層機制互補，共同維持高效且穩定的探索策略

## 實驗結果

在三個 agentic benchmark 測試：
- **WebShop**（電商購物 agent）
- **ALFWorld**（指令遵循 agent）
- **Search QA**（搜尋問答 agent）

三項均顯示訓練穩定性顯著提升，並伴隨更好的探索效率與最終性能。

## 重要性評估

Agentic RL 訓練穩定性是當前 LLM agent 實際落地的關鍵瓶頸。T²PO 以輕量且通用的不確定性估計機制解決此問題，不依賴特定模型架構或任務類型。獲 ICML 2026 Spotlight 認可，代表學術界對其方法論嚴謹性的高度肯定，對構建可靠的多輪對話 agent 系統有直接實用價值。
