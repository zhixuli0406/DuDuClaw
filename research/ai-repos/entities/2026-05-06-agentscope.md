---
title: AgentScope — 可觀測、可信賴的生產級 Agent 框架
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-repos-researcher
tags: [repo, agent, llm, multi-agent, mcp, observability]
related: []
sources: [github]
layer: context
trust: 0.5
---

## Repo 資訊

- **URL**: https://github.com/agentscope-ai/agentscope
- **作者/組織**: agentscope-ai（Alibaba 孵化開源社群）
- **主要語言**: Python (100%)
- **技術棧**: ReAct、MCP、A2A 協議、OpenTelemetry、Trinity-RFT
- **授權**: Apache 2.0

## 核心功能

AgentScope 以「Build and run agents you can see, understand and trust」為設計理念，提供生產就緒的 Agent 開發框架。支援快速建構 ReAct agent 並整合工具/技能，部署選項涵蓋本地、Serverless、Kubernetes。透過 MsgHub 實現多 Agent 工作流協調，並以 OpenTelemetry 提供全面可觀測性。2026 年初整合 Trinity-RFT 庫，支援 Agentic 強化學習訓練。

## 亮點與創新點

- **A2A 協議支援**: 支援 Agent-to-Agent 標準協議，可與其他框架 Agent 互操作
- **Trinity-RFT 強化學習**: 內建 Agentic RL 訓練能力，Agent 可從環境互動中自我改進
- **Real-time Voice Agent**: 支援語音及即時語音 Agent 能力（2026 年初新增）
- **Human-in-the-Loop**: 支援即時中斷與人工介入引導 Agent 行為
- **記憶壓縮**: 短期與長期記憶管理，含自動壓縮機制

## Stars 數量與趨勢

- **Stars**: 24,600（截至 2026-05-06）
- **Forks**: 2,700
- **趨勢**: 穩定成長，企業生產部署導向的知名框架

## 適用場景

- 企業生產環境 Agent 系統建構與部署
- 多 Agent 工作流研究（MCP + A2A 整合）
- Agentic 強化學習研究（Trinity-RFT）
- 需要高可觀測性與審計追蹤的 AI 系統
