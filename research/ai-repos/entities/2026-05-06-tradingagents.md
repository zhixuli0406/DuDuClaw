---
title: TradingAgents — 多 Agent LLM 金融交易框架
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-repos-researcher
tags: [repo, agent, llm, finance, multi-agent]
related: []
sources: [github]
layer: context
trust: 0.5
---

## Repo 資訊

- **URL**: https://github.com/TauricResearch/TradingAgents
- **作者/組織**: TauricResearch
- **主要語言**: Python (99.8%)
- **框架**: LangGraph
- **授權**: 未標示（公開 repo）

## 核心功能

TradingAgents 模擬真實世界的交易公司架構，部署多個專責 AI Agent 協同完成市場分析與交易決策。框架內含基本面分析師、情緒專家、技術分析師、交易員、風控管理等角色，每個 Agent 各司其職。透過 LangGraph 實現模組化可拆解的工作流，支援辯論輪次配置與跨提供商模型選擇。具備決策日誌追蹤功能，可將過去交易教訓注入未來分析，形成持續學習迴圈。

## 亮點與創新點

- **多 LLM 提供商支援**: 相容 OpenAI、Gemini、Claude、Grok、DeepSeek、Qwen、GLM、OpenRouter、Ollama、Azure OpenAI 等 10 家提供商
- **持久化學習**: Decision logging 系統追蹤已實現報酬，並將教訓反注入未來分析
- **結構化輸出**: v0.2.4（2026-04）引入 structured-output agents 與跨提供商標準化
- **Docker 支援**: 可容器化部署，並選配 Ollama 本地推論

## Stars 數量與趨勢

- **Stars**: 69,301（截至 2026-05-06）
- **本週增長**: +14,697 stars（GitHub Trending 週榜 #3）
- **趨勢**: 急速上升，進入週榜前三名

## 適用場景

- 量化交易研究與回測
- 多 Agent 金融分析系統原型
- LangGraph 多 Agent 工作流學習範例
- 跨 LLM 提供商切換驗證測試
