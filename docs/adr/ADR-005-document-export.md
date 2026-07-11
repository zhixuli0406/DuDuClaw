# ADR-005: Document export (md → Slide / Word / PPT / PDF)

- Status: Accepted
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

客戶在 demo 中問(7:35):agent 能不能生出 Google Slides 或 Microsoft
Office 檔案。對方自承這塊複雜、還在研究。

現況錨點:**完全沒有。** grep 整個 Rust source 找不到 pptx / docx / pandoc /
`docx-rs` / `rust_xlsxwriter` 的實作;唯一命中的 `pptx` / `docx` 字樣在編譯後的
dashboard bundle(`PartnerPortalPage`)裡,是合作夥伴素材的下載連結,與文件
生成無關。ReportPage 的 export 是前端 JS 函數,不產出可寄送的檔案。

agent 的輸出天生是 markdown。要把它變成客戶能收、能開、能改的
Slide / Word / PPT / PDF,需要一條轉檔管線。這是一個選型 spike——先定方向,
再開實作(對映 WP11-T11.2 的 `document_export` MCP tool)。

## Options considered

**(a) 純 Rust:`docx-rs` / `rust_xlsxwriter`**
零外部依賴、單一 binary 打包乾淨、跨平台一致。缺點是 pptx 生態弱——Rust
沒有成熟的 pptx 生成庫,自己拼 OOXML 成本高。docx 可行,pptx 是硬傷。

**(b) Pandoc subprocess**
md → docx 極成熟,md → pptx 可用(Pandoc 有 pptx writer)。缺點是需要外部
二進位依賴,使用者機器不一定有 Pandoc。可用 detect-then-enable 化解:偵測到
Pandoc 才啟用,沒偵測到就 fail-soft 降級為 md 附件。

**(c) HTML → PDF(headless browser)**
專案已有 Playwright MCP 作為瀏覽器層(L3),理論上 md → HTML → 印成 PDF。
PDF 品質高、排版可控。**誠實的現況警告**:gateway 的 `browser_router.rs`
目前是骨架,實際的瀏覽器自動化走 Playwright MCP 而非這個 router,PDF 出圖
的完整迴路尚未串通,不能當成「現成的」。

**(d) Google Slides API**
原生 Google Slides,對重度用 Google Workspace 的客戶最貼。缺點是需要 OAuth、
資料經雲端、實作與授權維護成本高。與地端優先的產品調性相衝。

## Decision

**md → docx / pptx 走 Pandoc(detect-then-enable,fail-soft 降級為 md 附件),
PDF 走既有瀏覽器層。純 Rust 路徑保留為後續選項。**

理由:Pandoc 一次拿下 docx 與 pptx 兩種最常被要的格式,而 pptx 正是純 Rust
的死穴。detect-then-enable 把「需要外部依賴」這個缺點降級成優雅退場——沒
Pandoc 就回 md 附件並附一句說明,不當掉、不假裝成功。PDF 借瀏覽器層是最短
路徑,不引新依賴。Google Slides 原生涉及 OAuth 與雲端資料流,與地端優先的
產品方向衝突,先不做。

實作要點(細節在 WP11-T11.2):
- MCP tool `document_export`:輸入 md 內容 + 目標格式(docx / pptx / pdf),
  產出檔案落 agent workspace,並以檔案訊息從 channel 送出。
- pptx 最小模板:標題頁 + bullet 頁,套 DuDuClaw 品牌色。
- Pandoc 缺席 → fail-soft 降級為 md 附件(fail-open 到最保守的可用輸出,
  不是靜默失敗)。

## Consequences

**得到的:** md → Office 兩大格式(docx / pptx)有明確、成熟的路徑;沒有
Pandoc 的環境不會壞,只是拿到 md;不引入 OAuth 與雲端資料流。

**付出的:** Pandoc 是執行期外部依賴,部署文件要寫清楚怎麼裝、裝了才有
Office 輸出。PDF 依賴的瀏覽器層目前是骨架,PDF 這條要等瀏覽器迴路串通才
算數——這點不能對客戶含糊。

**給客戶的誠實話術:** 「md → Office 已支援方向(docx / pptx),Google Slides
原生仍在評估中。」不承諾 Google Slides,不誇大 PDF 現況。

**後續選項:** 若外部依賴變成真痛點(例如客戶要單一 binary、禁裝 Pandoc),
再啟用純 Rust 的 `docx-rs`,pptx 那塊屆時單獨評估是否值得自拼 OOXML。以新
ADR 記錄該轉向。
