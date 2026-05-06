---
title: 幻覺損害信任；元認知是前進之道
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-papers-researcher
tags: [AI, LLM, safety, hallucination, metacognition, alignment, ICML]
related: []
sources: [arxiv]
layer: context
trust: 0.5
---

# Hallucinations Undermine Trust; Metacognition is a Way Forward
**幻覺損害信任；元認知是前進之道**

## 基本資訊

- **作者：** Gal Yona, Mor Geva, Yossi Matias
- **機構：** Google（依作者背景推斷）
- **發表日期：** 2026-05-02
- **來源：** arXiv；已獲接受為 **ICML 2026 Position Track**
- **論文連結：** https://arxiv.org/abs/2605.01428

## 核心貢獻

本文認為，當前 LLM 改善幻覺問題的研究路線——擴充知識廣度——並未根本解決問題。作者提出**忠實不確定性**（faithful uncertainty）與**元認知**（metacognition）作為核心解決框架：模型應具備對自身知識邊界的感知能力，並據此透明地溝通不確定性或在自主系統中觸發外部驗證機制。

## 方法摘要

- **忠實不確定性（Faithful Uncertainty）**：超越「回答或拒絕」的二元選擇，讓模型能以不同置信度表達回應
- **元認知（Metacognition）**：模型感知自身不確定性的能力，分為兩種應用場景：
  - **直接互動場景**：向用戶透明溝通不確定程度
  - **自主系統場景**：以不確定性作為控制信號，決定何時查詢外部資源、信任何種資訊
- 識別現有研究中尚未解決的元認知挑戰，提出未來研究方向

## 實驗結果

本文為 Position Paper（立場論文），側重論證框架而非單一實驗。作者系統性分析現有幻覺緩解方法的局限，並提出元認知評估維度。

## 重要性評估

在 LLM 可信度議題日益重要的背景下，本文提出了一個框架性轉變：從「模型知道什麼」轉向「模型知道自己不知道什麼」。這對 AI Safety 與 Alignment 社群具有重要意義，尤其在 agentic 系統中，不確定性的錯誤傳播可能導致嚴重後果。ICML 2026 Position Track 的接受代表學術界對此問題框架的認可。
