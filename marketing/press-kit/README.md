# press-kit

> Launch material for Hacker News (Show HN) + Product Hunt + 媒體 outreach.
>
> 一人公司的鐵律：上線前所有素材**全部錄完、剪完、上 CDN**。
> 上線當天只做一件事 —— 回留言。

---

## 目錄

| 檔案 | 用途 | 語言 |
|---|---|---|
| [`show-hn-post.md`](./show-hn-post.md) | HN Show HN 完整草稿 + 第一小時自我留言種子 | EN |
| [`product-hunt-launch.md`](./product-hunt-launch.md) | PH launch 完整方案 + tagline + gallery + 描述 + 留言種子 | EN |
| [`asset-checklist.md`](./asset-checklist.md) | GIF / 截圖 / 影片 / 架構圖規格 + 自己排的時程 | zh-TW |
| [`comment-reply-playbook.md`](./comment-reply-playbook.md) | 60 種預期留言的回覆腳本（敵意 / 懷疑 / 技術深入 / 策略題） | EN |

---

## 上線時間表（建議）

| 距離上線 | 任務 | 文件 |
|---|---|---|
| **T-4 週** | 開始錄製 A1-A4 GIF + B1-B8 截圖 | [`asset-checklist.md`](./asset-checklist.md) |
| **T-3 週** | 完成 D1 5 分鐘介紹影片 | 同上 |
| **T-2 週** | logos / boilerplate / portraits 補齊 | 同上 |
| **T-2 週** | 在 Product Hunt 排定 launch（PH 需 7+ 天前排程） | [`product-hunt-launch.md`](./product-hunt-launch.md) |
| **T-1 週** | 用 staging 環境模擬一次上線：草稿走過一遍、計時 + 回覆腳本走過 | [`comment-reply-playbook.md`](./comment-reply-playbook.md) |
| **T-3 天** | LINE OA + Discord + X 預告（不放 PH/HN 連結，只說「下週」） | — |
| **T-0** | PH 00:01 PT / 16:01 TPE → HN 06:30 PT / 21:30 TPE | [`show-hn-post.md`](./show-hn-post.md) |

> **不要**同時間發 HN + PH（會分散注意力，回不過來）。
> PH 在前、HN 在後 6 小時 — 一個冷啟動、一個高峰救援。

---

## 上線當天個人時程（範例 — Taipei 時區）

| 時間 | 事 | 動作 |
|---|---|---|
| 13:00 | 午餐 + 咖啡 | 確認手機網路 + 筆電電池滿 |
| 15:00 | 開電腦、開 PH dashboard + HN 草稿 tab | 確認 staging 環境一切正常 |
| 15:55 | 預備 | 5 分鐘呼吸 |
| **16:01** | **PH go-live** | 立刻轉發到 X / LINE OA |
| 16:30 | LINE OA newsletter 推送 | 模板：「我今天上 PH 了，幫我衝一波」 |
| 17:00 | X / LinkedIn 同步發 | 一條 10 推 thread + LinkedIn 長文 |
| 17:00-22:00 | 回 PH 留言 | 不離開電腦超過 5 分鐘 |
| 21:00 | 晚餐（叫外送）| 不離開電腦 |
| **21:30** | **HN Show HN go-live** | 立刻發兩個 seed comment（技術深入 + 一人公司） |
| 21:30-04:00 | 回 HN + PH 留言 | 輪流刷新 |
| 04:00 | 睡 4 小時 | 設鬧鐘 08:00 |
| 08:00 | 醒、看流量、回過夜留言 | 計算當下 KPI（star / 註冊 / trial）|
| 全天 | 繼續回 | 一個禮拜內每天都要看 |

---

## 已準備好的銷售素材引用

- [`marketing/blog/2026-06-05-why-pay-for-apache-2.md`](../blog/2026-06-05-why-pay-for-apache-2.md) — 為什麼開源還要付錢的核心論述（HN / PH / Landing 都引用）
- [`commercial/website/index.html`](../../commercial/website/index.html) — Landing page MVP（PayUni checkout 自助流程）
- [`commercial/docs/payuni-integration-notes.md`](../../commercial/docs/payuni-integration-notes.md) — 金流整合筆記（如果 HN 問「為什麼用 PayUni 不用 Stripe」可貼此連結 — **但只在重視的留言才貼**，因為它在 commercial 閉源目錄）

---

## 文件未涵蓋

- **媒體（記者）outreach**：本批不做。一人公司沒記者人脈，主動 outreach ROI 極低。
- **付費廣告投放**：不做。已在 [business-plan.md](../../commercial/docs/business-plan.md) §4.3 明列 Anti-strategy。
- **PR 公關公司聘用**：不做。一人公司的真實感是最強的 PR。

---

## 後續迭代

- 上線後 14 天內寫一篇「PH/HN 上線後我學到的事」（公開到 blog + 內部 retro）
- 上線後 30 天統計 PH / HN / LINE OA / X 帶來的付費轉化率，調整 Year 1 通路比重
- 上線後 90 天決定是否做第二次「DuDuClaw v1.20」的 HN 投放（重大版本才值得）
