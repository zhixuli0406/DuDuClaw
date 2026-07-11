# ERP / CRM 支援矩陣

> 對業務與客戶對談用的一頁速查。技術決策見 [ADR-004](../adr/ADR-004-erp-connector-abstraction.md)。
> 最後更新:2026-07-09。

DuDuClaw 讓 AI 員工直接對你的 ERP / CRM 讀寫資料。下表是目前的覆蓋範圍,
分三個狀態:**已支援**(現在就能上線)、**抽象層就緒**(接新系統的骨架已完成,
排程接入)、**規劃中**(在 backlog,尚未動工)。

| 系統 | 版本 / 型態 | 狀態 | 能做什麼 | 隔離與稽核 |
|------|------------|------|----------|-----------|
| **Odoo** | CE(社群版)/ EE(企業版) | ✅ 已支援 | CRM 名單、報價與銷售單、庫存查詢、發票與收款狀態,共 15 個工具 | per-agent 憑證、動作 / 模型白名單、每筆操作進稽核 |
| **ERPNext** | v14+ | 🔜 規劃中 | 抽象層完成後的第一個驗證實作 | 與 Odoo 共用同一套隔離機制 |
| **Twenty** | 開源 CRM | 📋 規劃中 | CRM 場景(名單 / 商機 / 聯絡人) | 同上 |
| 其他 REST/JSON-RPC ERP | — | 📋 評估中 | 依 `ErpConnector` 合約可擴充 | 合約內建,新實作免費得到 |

## 給業務的話術

**面對「Odoo 只適合中小公司,我們規模更大」的疑慮:**

Odoo 確實在 15-50 人的公司最順手,這是它的甜蜜點。但 DuDuClaw 對 ERP 的
接法不綁死 Odoo。底層是一個叫 `ErpConnector` 的抽象合約(見 ADR-004),
Odoo 只是第一個實作。合約一旦定案,接 ERPNext、Twenty 或客戶自家的
REST/JSON-RPC 系統,走的是同一條路,而且新系統一接進來就自動繼承
per-agent 憑證隔離、動作白名單、全操作稽核。這些是合約本身的一部分,
不是每接一家重寫一次的東西。

所以對大型企業客戶,誠實的說法是:**「Odoo 現在就能跑,你用的系統我們有
一層標準化的接法能擴充,ERPNext 是排程中的第一個驗證案例。」** 不誇大成
「什麼都支援」,也不把客戶推走。

## 什麼是「抽象層就緒」

ADR-004 定案後,`duduclaw-erp` 骨架 crate 提供 trait、連線池、scope 檢查與
稽核,Odoo 改為實作者。此時接新 ERP 的工作量從「複製整包 Odoo 邏輯」降到
「實作一個 trait」。矩陣裡的「抽象層就緒」指的就是這個骨架到位、Odoo 回歸
測試全綠的狀態。

## 相關文件

- [ADR-004: ERP connector abstraction](../adr/ADR-004-erp-connector-abstraction.md) — 為什麼抽 trait、取捨
- [features/12-industry-templates.md](12-industry-templates.md) — Odoo ERP 橋接深入介紹
