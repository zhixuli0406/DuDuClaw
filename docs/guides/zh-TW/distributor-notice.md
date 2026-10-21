# 關於頁的總經銷資訊

總經銷專用的 DuDuClaw build 可以在 dashboard 的關於頁（`/about`）多顯示一塊授權總經銷的資訊，位置在固定的原廠區塊上方。公開版不帶這塊內容，關於頁和 `about.get` 的回應都和以前一樣。

這塊內容是**建置輸入**：製作總經銷專用 build 時，HTML 會編譯進 gateway 執行檔。已安裝的系統上沒有任何設定、RPC 或檔案能改它，所以終端客戶沒辦法換成別的內容。代價是：內容要改，就得重新出 build、重新交付。

白牌功能（自訂產品名稱、logo、顏色）仍然是移除的狀態，見 [white-label.md](white-label.md)。這塊內容不會改動頁面上的產品名稱、原廠標示、版本或授權方案。

## 建置輸入

```bash
DUDUCLAW_DISTRIBUTOR_NOTICE_PATH=/absolute/path/to/notice.html \
  cargo build --release -p duduclaw-cli
```

這個變數由 `crates/duduclaw-gateway/build.rs` 讀取：

- 沒設定：沒有總經銷資訊（所有公開版都是這樣）。
- 有設定：必須是指向一般檔案的絕對路徑（符號連結會被拒絕），UTF-8 編碼（開頭的 BOM 會去掉），**含內嵌圖片在內**不超過 64 KB。變數設了卻是空字串、檔案不存在或違反其他規則，建置會失敗並說明原因。
- 檔案或變數改變時，cargo 會重新建置。
- HTML 會在建置時消毒（規則見下）。被消毒器拿掉的東西，會在 cargo 輸出裡列成 `warning: distributor notice: …`。消毒後沒有任何可顯示的內容時，建置失敗。

這個輸入和 `DUDUCLAW_INSTALLATION_POLICY_PATH` 分開。安裝政策只用來封鎖通道、runtime 與供應商，不能帶任何身分資訊。

## 會保留的 HTML

- 元素：`p`、`br`、`h1`–`h4`、`ul`、`ol`、`li`、`strong`、`em`、`u`、`s`、`a`、`blockquote`、`code`、`pre`、`hr`、`img`、`div`、`span`、`table`、`thead`、`tbody`、`tr`、`th`、`td`。`<script>`、`<style>` 連內容一起移除；其他元素移除標籤、保留文字。
- 屬性：只有 `a[href]`、`img[src]`、`img[alt]`。`style`、`class`、`id` 和所有 `on*` 事件屬性都會移除。註解也會移除。
- 連結：只接受絕對網址的 `http://`、`https://` 或 `mailto:`。每個連結都會加上 `rel="nofollow noopener noreferrer" target="_blank"`。
- 圖片：只接受 `data:image/png`、`data:image/jpeg`、`data:image/webp` 的 base64 URI，內容要和宣告的格式相符，每張上限 512 KB（通常是整份 64 KB 的上限先到）。SVG 和外部圖片會被丟掉。

gateway 第一次送出時會再消毒一次編譯進來的 HTML。如果第二次沒通過，`about.get` 就不帶這個欄位，gateway 記一則警告。

## dashboard 怎麼顯示

- 原廠區塊上方一張卡片，固定標題「總經銷資訊」和說明「由授權總經銷提供」（英文、日文介面有對應翻譯）。標題是產品文字，不屬於總經銷內容。
- 內容只在 `sandbox="allow-popups allow-popups-to-escape-sandbox"` 的 iframe 裡顯示：不能執行 script、來源是不透明的、不能送表單、不能換掉整個頁面。連結會開在新分頁。iframe 內另有內容安全政策擋掉所有網路請求，所以圖片要用 data URI 內嵌。
- iframe 高度固定 280 px，內容比較長時在裡面捲動。沒有 script 的 iframe 回報不了內容高度，所以高度只能固定。
- iframe 跟著 dashboard 的淺色或深色主題。總經銷內容自己的樣式會被移除，套用的是 dashboard 的基本樣式。

## 確認 build

用已登入的 dashboard 連線呼叫 `about.get`。總經銷專用 build 會回傳 `distributor_notice`（消毒後的 HTML）；公開版不會有這個欄位。
