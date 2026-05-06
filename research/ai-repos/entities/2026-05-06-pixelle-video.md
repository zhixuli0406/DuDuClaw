---
title: Pixelle-Video — AI 全自動短影片生成引擎
created: 2026-05-06T08:00:00Z
updated: 2026-05-06T08:00:00Z
author: ai-repos-researcher
tags: [repo, diffusion, multimodal, video-generation, aigc]
related: []
sources: [github]
layer: context
trust: 0.5
---

## Repo 資訊

- **URL**: https://github.com/AIDC-AI/Pixelle-Video
- **作者/組織**: AIDC-AI（阿里巴巴國際數字商業集團 AI 部門）
- **主要語言**: Python (76.1%)
- **框架**: ComfyUI、多 LLM 整合
- **授權**: Apache 2.0

## 核心功能

Pixelle-Video 實現「輸入主題即可自動生成完整短影片」的端到端流程。系統依序執行：AI 文案撰寫 → AI 配圖/影片生成 → 語音合成解說 → 背景音樂添加 → 最終影片合成。支援多模型 LLM（GPT、Qwen、DeepSeek、Ollama）切換，並以 ComfyUI workflow 作為影像/影片生成後端，提供高度自訂空間。

## 亮點與創新點

- **完整端到端流水線**: 一個主題輸入，自動串接文案→圖像→語音→音樂→合成全流程
- **數位人頭像模組**: 支援數位人與動作遷移（motion transfer），可製作虛擬主播影片
- **語音克隆 TTS**: 多種 TTS 引擎選項，含聲音克隆功能
- **ComfyUI 靈活後端**: 可替換任意 ComfyUI workflow，對接社群最新圖像/影片模型
- **響應式 Web UI**: 配置與生成流程均可透過 Web 介面操作

## Stars 數量與趨勢

- **Stars**: 11,629（截至 2026-05-06）
- **本週增長**: +4,201 stars（GitHub Trending 週榜前十）
- **趨勢**: 近期爆發性成長，短影片 AI 自動化賽道代表作

## 適用場景

- 自媒體創作者批量生產短影片內容
- 電商、行銷素材自動化生產
- AI 影片生成技術研究與 ComfyUI workflow 開發
- 數位人虛擬主播原型搭建
