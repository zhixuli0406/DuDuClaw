---
title: "Apache 2.0 開源核心，為什麼還要付 NT$1,490？"
slug: why-pay-for-apache-2
date: 2026-06-05
author: zhixuli
tags: [pricing, business-model, open-source, apache-2]
description: >
  DuDuClaw 的程式碼 100% Apache 2.0 開源。你可以下載、修改、商業使用，
  完全不必付錢。那 Self-Host Pro 訂閱在賣什麼？這篇文章把帳算給你看。
---

# Apache 2.0 開源核心，為什麼還要付 NT$1,490？

> 經常被問的問題：「我看到你們的程式碼是 Apache 2.0，我自己 clone 不就好了？
> 那 Self-Host Pro 一個月 NT$1,490 你到底在賣什麼？」
>
> 直接回答：我賣的是**你的時間**，外加你自己扛不太動的四件事。

---

## TL;DR — 我幫你扛的四件事

1. **Anthropic / OpenAI / Google 改版的追趕**
   v1.16.0 一年內推了 16 個小版本。Anthropic 每幾個月就改一次規則
   （封 `claude -p`、改 OAuth 行為、Rate Limit 重洗）。追是我的工作，不是你的。

2. **產業 SOUL.md + Evolution 參數**
   餐飲、律師、會計、貿易、製造五套產業模板，是百次以上真實對話迭代來的。
   你開源自己改，能調到差不多，但會花你 2-3 個月。

3. **優先安全補丁**
   開源版的安全補丁刻意延遲 **30 天**才發。Self-Host Pro 訂閱者拿到的是
   **8 小時內**的補丁，包含像 RFC-23 redaction、prompt injection 規則更新等
   風險邊界的修補。

4. **私人 Discord support 頻道**
   你發 issue，我看見；你 LINE OA 問問題，我看見。不是 8x5、不是 24x7，
   但「有一個人會接電話」這件事本身，就值 NT$1,490。

如果上面四件事你不需要——**真的，不要付錢**。Apache 2.0 拿去用就好。

---

## 先把帳算給你看

假設你是個人接案開發者，或一間 5 人小公司的技術老闆，你想要：

- LINE 客服機器人替你 24/7 顧客
- 一台 Mac Mini 跑本地推論，盡量壓低 Anthropic API 費用
- ERP 串接（Odoo / 自家系統）讓客服可以查訂單
- 不要外包客戶資料給 SaaS

### 方案 A：完全自己拼

你下載 DuDuClaw（或任何一個替代品），花時間：

| 工作 | 樂觀估計 | 實際估計 |
|------|---------|---------|
| 看文件、決定架構 | 1 週 | 2 週 |
| 部署 + LINE webhook 串接 | 3 天 | 1 週 |
| OAuth 帳號池管理 + 多帳號輪替 | 1 週 | 2-3 週 |
| 容器隔離 + 安全 hook | 1 週 | 2 週 |
| Memory + Evolution 參數調校 | 1 週 | 4-6 週 |
| Odoo 串接 | 1 週 | 2 週 |
| 第一個月維護 + 修 bug | 1 週 | 2 週 |
| **小計** | **5 週** | **15-18 週** |

按一個資深工程師 **時薪 NT$1,500** 算（這已經很便宜了）：

- 樂觀：5 週 × 40 小時 = 200 小時 = **NT$300,000**
- 實際：15-18 週 × 40 小時 = **NT$900,000-1,080,000**

而且這只是「建起來」的成本。**後續每個月** Anthropic / Claude CLI 改版你都要追，
每出一個安全補丁你都要評估，每個產業客戶你都要重新調 SOUL.md。

### 方案 B：付 Self-Host Pro 訂閱

NT$1,490 × 12 = **NT$17,880 / 年**。

換到的東西：

- 開源版裡所有東西，都還在
- 加上 `commercial/templates-premium/`（5 套產業模板）
- 加上 `commercial/evolution-params/`（調好的 GVU 閾值 + MetaCognition 參數）
- 加上 `commercial/dashboard-enterprise/`（audit log 匯出、ROI 報表）
- 加上 8 小時內的安全補丁
- 加上一個我可以親自接的 Discord 頻道

**用上面實際估計的成本比較：NT$17,880 vs NT$900,000+。**

> 補一句話：Self-Host Pro 跟 Cloud 不一樣。Cloud（Studio NT$2,990/月、
> Business NT$8,900/月）是「我幫你跑基礎設施」；Self-Host Pro 是
> 「你自己跑，但我繼續幫你維護產品和補丁」。資料 100% 不離開你的網路。

---

## 你可能會問：那為什麼不直接閉源？

公平的問題。答案有兩個。

**第一個是技術上的**。DuDuClaw 是 12 個 Rust crate + Python bridge + React
Dashboard，外加跟 Claude CLI / Codex / Gemini 三個會持續改版的上游糾纏。
任何閉源版本兩個月內就會落後。
**開源，是我自己的工作備份**——社群幫我抓 bug、社群幫我移植到他們的環境、
社群幫我發現邊緣場景。閉源等於把這些變成我自己的責任。

**第二個是商業上的**。一人公司最怕的事，不是「客戶 fork 後不付錢」——
那種人本來就不是你的客戶。一人公司最怕的事是
**「客戶今天付錢，明天我消失了，客戶被綁架。」**

Apache 2.0 是我給客戶的保險。**如果哪天我被車撞了，你今天付的錢不會白付。**
你的 license 失效之後 commercial 模組會停載，
但 Apache 2.0 那部分繼續可以跑下去、可以你自己改、可以你自己接手維護、可以你 fork 成你公司的內部專案。

**這個保險的代價，就是我必須承認「不付錢也能用」**。
我選擇承認這件事，因為這是我能給客戶的最大善意。

---

## 我刻意「不做」的事

- **不做 license 翻牆檢測**：你拔掉 phone-home 我不會去抓你。
- **不做核心功能 gate**：沒有 license 啟動 gateway 完全不會跳警告。
  你只是看不到 commercial 模組裡的東西而已。
- **不做「30 天 trial 結束硬鎖定」**：失效後降回 OpenSource 模式，
  你的客戶資料、agent、Memory 全部繼續可用。
- **不做 DRM**：拔掉 license 模組整段、自己改、拿去賣，我都不會告你。
  我只保留「DuDuClaw 名字 + 爪印 Logo」的商標權。

**為什麼這樣做？** 因為這四件事都需要我花時間維護 anti-tamper 的程式碼。
而我寧願花時間寫 Evolution 演化新規則、或者多寫一個產業模板。
Anti-tamper 防得了沒誠意的人，防不了想 crack 的人。
有誠意的人本來就會付錢，因為他知道我在這之上花的時間，比他自己搞要少得多。

---

## 那「不要付錢」是真的嗎？

是真的。以下幾種人**不需要**付 Self-Host Pro：

- **學生或自學者**：你需要的是搞清楚怎麼跑通，Apache 2.0 版本綽綽有餘。
- **完全只用一個 channel + 自己跟 AI 聊**：你不需要 Evolution，
  不需要產業模板，不需要 audit log。
- **公司有專職 DevOps 想自己掌握所有設定**：很好，你公司付你薪水的，
  不是付我的。Apache 2.0 給你。
- **NPO / 公益專案**：直接寫信來，我可以給你不要錢的 Self-Host Pro 鑰匙。

---

## 那「應該要付錢」的是哪些人？

- **個人接案工作室**：你的時間就是錢，省下 200 小時就值得了。
- **5-50 人台灣中小企業**：你需要的不是「另一個工具」，是「能解決問題的人」。
- **律師事務所 / 會計師事務所 / 製造業**：合規敏感，自架是硬需求，
  但你又不想花一年自己搞 audit 匯出 + redaction pipeline。
- **想知道「東西壞了有人接電話」的所有人**：說真的，這就值 NT$1,490 了。

---

## 我接下來會做的事

短期內 Self-Host Pro 訂閱者會看到的：

- **產業模板**：第二個產業（律師 / 會計師）2026 Q3 上線
- **Evolution 最佳參數**：跟著 GVU 演化引擎下個小版本一起釋出
- **Dashboard Enterprise**：audit log CSV + PDF 匯出 2026 Q3 上線
- **季度健康檢查**：年付客戶贈 30 分鐘 1-on-1（我可以遠端進你的 Dashboard 看狀況）

長期（明年）：

- OEM 白標版本（如果有合作夥伴問就做）
- 多人團隊版本（如果有客戶問就做）
- 海外英文 Self-Host Pro（如果 GitHub Issue 英文比 > 50% 就做）

---

## 怎麼開始

1. 先用 Apache 2.0 跑起來：
   ```sh
   brew install zhixuli0406/tap/duduclaw
   duduclaw onboard
   duduclaw start
   ```
2. 跑得順了再評估要不要訂閱。
3. 想訂閱請來 [duduclaw.tw](https://duduclaw.tw)，
   或直接 LINE OA [@duduclaw](https://line.me/R/ti/p/@duduclaw)。

問題、批評、想罵我都可以寄到 hello@duduclaw.tw。

—— zhixuli
2026-06-05 於台北

---

**相關連結**

- 開源 repo：[github.com/zhixuli0406/DuDuClaw](https://github.com/zhixuli0406/DuDuClaw)
- 完整定價：[duduclaw.tw#pricing](https://duduclaw.tw#pricing)
- 商業模式背後的思考：[Solo-Founder Business Plan v2.0](https://github.com/zhixuli0406/DuDuClaw/blob/main/commercial/docs/business-plan.md)（僅內部，公開版本 2026 Q3）
- PayUni 整合筆記：[PayUni reverse-engineered](https://github.com/zhixuli0406/DuDuClaw/blob/main/commercial/docs/payuni-integration-notes.md)（同上）
