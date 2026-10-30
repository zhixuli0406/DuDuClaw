# 關於頁的總經銷資訊

總經銷專用的 DuDuClaw build 可以在 dashboard 的關於頁（`/about`）多顯示一塊授權總經銷的資訊，位置在固定的原廠區塊上方。公開版不帶這塊內容，關於頁和 `about.get` 的回應都和以前一樣。

內容由總經銷自己在授權 console 的「總經銷資訊」頁維護（負責人角色），有兩條路到客戶的 dashboard：

- **執行期更新**：有授權的 gateway 開機約 1 分鐘後、之後每 6 小時，向控制面要「發這張授權的總經銷」目前的內容。回應用簽發授權的同一把金鑰簽章，並綁定這張授權，gateway 驗過簽章、再消毒一次才顯示，並把簽章文件存在 `<home>/distributor_notice.json`。客戶改這個檔案只會讓簽章失效、退回編譯進來的內容。所以總經銷存檔後，連得上網的客戶約 6 小時內會看到新內容，不用出新版本。
- **建置輸入**：發版時建置管線自動向控制面取內容、編譯進執行檔，給離線安裝或還沒連上控制面的客戶。只有一家啟用中的總經銷有內容時才編入；有多家時共用映像不編入任何一家（客戶仍會在執行期拿到自己總經銷的內容）。

兩者都在時以執行期的為準，連「總經銷清除了內容」也是簽章過的結果，會蓋過編譯進來的版本。已安裝的系統上沒有任何設定或 RPC 能改這塊內容，終端客戶換不掉。

獨立的[總經銷品牌簽章文件](white-label.md) 可以在 `Oem` 方案下更換產品名稱與 Logo，公告本身不會修改它們。品牌生效時，關於頁依序顯示品牌卡片、這塊公告、固定的「軟體開發」區塊。原廠標示、原廠網站、版本與授權方案都必須保留，總經銷不能隱藏或覆寫。主題色、客戶端品牌修改入口與自訂 About HTML 仍未恢復。

## 建置輸入

```bash
DUDUCLAW_DISTRIBUTOR_NOTICE_PATH=/absolute/path/to/notice.html \
  cargo build --release -p duduclaw-cli
```

這個變數由 `crates/duduclaw-gateway/build.rs` 讀取：

- 沒設定：沒有總經銷資訊（所有公開版都是這樣）。
- 有設定：必須是指向一般檔案的絕對路徑（符號連結會被拒絕），UTF-8 編碼（開頭的 BOM 會去掉），**含內嵌圖片在內**不超過 64 KB。變數設了卻是空字串、檔案不存在或違反其他規則，建置會失敗並說明原因。
- 檔案或變數改變時，cargo 會重新建置。
- HTML 會在建置時消毒（規則見下）。被消毒器拿掉的東西，會在 cargo 輸出裡列成 `warning: distributor notice: …`。消毒後沒有任何可顯示的內容，或消毒後超過 64 KB（每個連結都會多出 `rel` 和 `target`），建置失敗。

這個輸入和 `DUDUCLAW_INSTALLATION_POLICY_PATH` 分開。安裝政策只用來封鎖通道、runtime 與供應商，不能帶任何身分資訊。

## 會保留的 HTML

- 元素：`p`、`br`、`h1`–`h4`、`ul`、`ol`、`li`、`strong`、`em`、`u`、`s`、`a`、`blockquote`、`code`、`pre`、`hr`、`img`、`div`、`span`、`table`、`thead`、`tbody`、`tr`、`th`、`td`。`<script>`、`<style>` 連內容一起移除；其他元素移除標籤、保留文字。
- 屬性：只有 `a[href]`、`img[src]`、`img[alt]`。`style`、`class`、`id` 和所有 `on*` 事件屬性都會移除。註解也會移除。
- 連結：只接受絕對網址的 `http://`、`https://` 或 `mailto:`，網址裡不能帶 `帳號:密碼@`。每個連結都會加上 `rel="nofollow noopener noreferrer" target="_blank"`。
- 圖片：只接受 `data:image/png`、`data:image/jpeg`、`data:image/webp` 的 base64 URI，內容要和宣告的格式相符，每張上限 512 KB（通常是整份 64 KB 的上限先到）。SVG 和外部圖片會被丟掉。

gateway 第一次送出時會再消毒一次編譯進來的 HTML。如果第二次沒通過，`about.get` 就不帶這個欄位，gateway 記一則警告。

## dashboard 怎麼顯示

- 原廠區塊上方一張卡片，固定標題「總經銷資訊」和說明「由授權總經銷提供」（英文、日文介面有對應翻譯）。標題是產品文字，不屬於總經銷內容。
- 內容只在 `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"` 的 iframe 裡顯示：不能執行 script、不能送表單、不能換掉整個頁面。連結會開在新分頁。iframe 內另有內容安全政策擋掉所有網路請求，所以圖片要用 data URI 內嵌。
- 為什麼這裡可以給 `allow-same-origin`：沒有 `allow-scripts`，iframe 裡任何東西都執行不了（`<script>`、行內事件、`javascript:` 網址都不會跑），所以它沒辦法利用 dashboard 的來源；這個權限只是讓 dashboard 讀得到內容高度、把卡片調成剛好的大小。絕對不能再加 `allow-scripts`：兩者同時存在時，iframe 可以自己拿掉 sandbox。
- iframe 高度跟著內容走（載入時、內容尺寸變動時、圖片載入後都會重新量），介於 64 px 與 600 px 之間，超過 600 px 就在框內捲動。高度變化有短暫動畫，系統設定減少動態時不播。
- iframe 跟著 dashboard 的淺色或深色主題。總經銷內容自己的樣式會被移除，套用的是 dashboard 的基本樣式。

## 確認 build

用已登入的 dashboard 連線呼叫 `about.get`。總經銷專用 build 會回傳 `distributor_notice`（消毒後的 HTML）；公開版不會有這個欄位。

## 執行期更新的細節

- 端點：`POST /v1/license/distributor-notice`，送 `subscription_id`、`machine_fingerprint` 與每次新產生的 `nonce`；控制面原樣回傳 `nonce`，對不上就丟棄（防重放）。
- 回應的 `distributor_notice` 是 `null` 代表這張授權沒有總經銷，gateway 回到編譯進來的內容；否則是簽章文件（`kind`、`subscription_id`、`distributor_id`、`version`、`html` 或 `null`、`sha256`、`issued_at`、`public_key_id`、`signature`）。
- 會被拒絕的情況：簽章或金鑰不對、綁的是別張授權、`html` 與 `sha256` 不符或超過 64 KB、版本比目前顯示的舊、消毒器不接受。被拒時保留原本的內容並記一則警告。
- 換授權或授權被撤銷時，快取檔刪除，回到編譯進來的內容，下一次查詢再取新授權的。
- 總經銷停用時，控制面回傳簽章過的「不顯示」。
