---
title: Future AGI — 開源 LLM Agent 評估與可觀測平台
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-repos-researcher
tags: [repo, llm, evals, observability, agent, ai-safety]
related: []
sources: [github]
layer: context
trust: 0.5
---

## Repo 資訊

- **URL**: https://github.com/future-agi/future-agi
- **作者/組織**: Future AGI
- **主要語言**: Python (48.8%)
- **技術棧**: OpenTelemetry、Docker/Kubernetes、OpenAI-compatible API
- **授權**: Apache 2.0

## 核心功能

Future AGI 是一個端到端的 LLM 與 AI Agent 評估、觀測、改進平台，將多個 AI 開發工作流整合於單一系統。核心六大功能模組：Simulate（場景模擬）、Evaluate（50+ 指標評估）、Protect（18 個安全掃描器）、Monitor（OpenTelemetry 追蹤）、Gateway（LLM 路由代理）、Optimize（提示詞最佳化）。強調資料主權：「你擁有你的評估邏輯和資料」。

## 亮點與創新點

- **50+ 評估指標**: 涵蓋幻覺偵測、一致性、相關性等，並支援自訂評估標準
- **18 個內建安全掃描器**: 整合多家供應商的 AI 安全防護，不依賴單一黑箱
- **六種提示詞最佳化演算法**: 系統化改進 LLM 應用效能
- **OpenTelemetry 原生支援**: 跨 50+ 框架的統一追蹤，可接入現有可觀測性基礎設施
- **100+ LLM 提供商 Gateway**: OpenAI 相容代理，統一管理成本與路由

## Stars 數量與趨勢

- **Stars**: 836（截至 2026-05-06）
- **趨勢**: 新興但功能完整，AI Safety 與 LLMOps 方向的實用工具

## 適用場景

- LLM 應用生產環境品質監控與評估
- AI Agent 安全測試與幻覺偵測
- 多 LLM 提供商成本管控與路由最佳化
- MLOps/LLMOps 可觀測性基礎設施建設
