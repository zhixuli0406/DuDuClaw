---
title: EurekAgent：Agent 環境工程是自主科學發現的關鍵
created: 2026-06-12T08:00:00Z
updated: 2026-06-12T08:00:00Z
author: ai-papers-researcher
tags: [AI, LLM, agent, scientific-discovery, environment-engineering]
related: []
sources: [arxiv]
layer: context
trust: 0.5
---

## 核心貢獻

EurekAgent 主張隨著 LLM 能力提升，自動化科學發現的瓶頸已從「設計 agent 工作流」轉移至「設計 agent 所處環境」。系統從**權限管理、產物管理、預算約束、人工監督**四個維度工程化 agent 環境，在數學、核心工程、機器學習 benchmark 上達到 SOTA，並以不到 11 美元 API 成本發現新的 26 圓填充問題解。

## 方法摘要

- **環境工程四維度**：
  1. Permissions（權限）：細粒度控制 agent 可執行的操作
  2. Artifact Management（產物管理）：追蹤、版本化實驗結果
  3. Budget Constraints（預算約束）：成本感知的資源分配
  4. Human Oversight（人工監督）：嵌入式人工審查節點
- **核心主張**：環境設計 > 工作流設計（隨 LLM 能力增強而愈加顯著）

## 實驗結果

- 數學、kernel engineering、ML benchmark 三項均達到 SOTA
- 26 圓填充問題：發現新最優解，API 成本 **< $11**
- 來自清華大學 KEG 實驗室

## 重要性評估

EurekAgent 提出「環境工程」作為自主科研 agent 的核心研究方向，挑戰了「更好的 prompt/workflow = 更好的 agent」的主流假設。其成本效益（< $11 達到 SOTA）顯示此路線具備規模化可行性，對構建 AI scientist agent 和 DuDuClaw 的任務執行環境設計均有重要啟示。

## 原文連結

- arXiv: https://arxiv.org/abs/2606.13662
- 作者：Amy Xin, Jiening Siow, Junjie Wang, Zijun Yao, Fanjin Zhang, Jian Song, Lei Hou, Juanzi Li
- 機構：清華大學 KEG 實驗室
- 發表日期：2026-06-11
