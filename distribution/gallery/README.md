# 板模畫廊（WP1.5 分享 URL 管線）

`data/templates.json` 驅動的零依賴靜態站生成器——每個板模一頁 SEO landing page（n8n 模式），`node generate.mjs` 輸出到 `out/`。

## 新增一個板模頁

在 `data/templates.json` 的 `templates` 加一個 entry 即可：
- `kind: "free"`＝內建 starter 板模（頁面顯示 onboard 精靈指令）
- `kind: "pack"`＝expert pack（`install.pack_url` 指到 zip，頁面顯示 `duduclaw expert install <url>` 一鍵匯入指令）

降級包（premium 劇本 → 免費版，拍板挑 5–8 個高搜尋量產業）產完後照 `_notes` 加 entry 即上線。

## 部署（👤）

`out/` 是純靜態檔：GitHub Pages、Cloud Run 靜態容器或任何 CDN 皆可；正式網域拍板 `duduclaw.app`（改 `site.base_url` 後重生成）。

## 待辦

- `duduclaw://import` desktop deep-link scheme（Tauri deep-link plugin 註冊）——目前一鍵匯入以 CLI/儀表板貼 URL 為主，scheme 屬加分項
- sitemap.xml 生成（entry 多於 10 個後加）
