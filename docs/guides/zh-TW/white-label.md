# 白牌品牌設定與經銷商控制台

取得授權的經銷商可以透過 DuDuClaw 重新品牌化 dashboard，包括產品名稱、logo、副標題與公司資訊；但上游原廠的標示(嘟嘟數位科技有限公司 / DuDu Digital Technology Co., Ltd.)永遠會顯示在 About 頁面上。這個標示是每次回應時從編譯進二進位檔的常數組出來的：它不會從設定檔讀取，也無法透過任何 RPC 寫入。

## 給經銷商：白牌品牌設定

門檻：授權方案要包含 `white_label` 功能，且已經在該實例上啟用（`duduclaw license activate <blob>`）。

1. 以管理員身分打開 **設定 → 品牌設定 (Branding)**。沒有白牌授權的話，這個分頁會唯讀，並顯示升級提示。
2. 填入產品名稱、副標題、公司名稱、網站、支援信箱、描述。
3. 上傳 logo：支援 PNG、JPEG、WebP，上限 512 KB。SVG 會被拒絕（因為有 script 注入風險）。圖片會以 base64 data URI 的形式存進 `~/.duduclaw/branding.json`，不會經過任何外部託管。
4. 可以選填**主題色**（`#rrggbb`）：dashboard 會用這一個十六進位色碼推導出整組 primary／accent CSS 色階；留空就維持原本的琥珀色。
5. 可以選填**About HTML 區塊**：一段顯示在 About 頁面上、關於經銷商的完整介紹。這段內容會在伺服器端用保守的允許清單（`ammonia`）過濾：只留一小組排版標籤，`<a>` 連結一律強制加上 `rel="nofollow noopener noreferrer" target="_blank"`，`<img>` 也限制成跟 logo 一樣的 `data:image/png|jpeg|webp` 規則（上限 512 KB，並檢查 magic byte）。`style`／`class`／`id`／`on*` 以及 `<script>` 都會被剝除。編輯器透過 `branding.preview` RPC 精準預覽最終會存下來的內容。超過 64 KB 會被拒絕。
6. 儲存後，側邊欄標誌、登入頁、瀏覽器標題、favicon、主題色、About 區塊都會立即更新。**重設**會還原成 DuDuClaw 的預設值。

**About** 頁面（`/about`）上半部顯示你的品牌設定，下半部則是固定的「軟體開發｜嘟嘟數位科技有限公司」區塊，加上版本號與授權方案。

驗證邏輯是 fail-closed：未知欄位一律拒絕，文字欄位有 CJK-safe 的長度上限，logo 必須符合宣告的 magic byte，而且授權快照沒有 `white_label` 的話，`branding.set`／`branding.reset` 會直接被拒絕。

## 給原廠：經銷商控制台

只有管理員能看到的 **/manage/distributors** 頁面，用來登記經銷商，並簽發綁定機器指紋的 OEM 授權金鑰。

1. 設定簽發用的簽章金鑰（跟授權金鑰產生工具輸出的格式一樣：32-byte Ed25519 seed、base64 編碼、單行）：

   ```toml
   # ~/.duduclaw/config.toml
   [distributor]
   issuer_key_path = "/path/to/license-signing-v2.key"
   ```

   沒設定的話，簽發會直接被拒絕並回報明確的錯誤；控制台會改顯示設定卡片。金鑰內容不會出現在 log 或任何回應裡。

2. 新增一個經銷商，然後點**簽發金鑰**：貼上經銷商用 `duduclaw license fingerprint` 拿到的機器指紋，選一個授權期限（預設 365 天），複製產生出來的 blob。經銷商拿到後用 `duduclaw license activate <blob>` 啟用。

   每一份簽發出來的授權都會先拿二進位檔內建的 v2 公鑰自我驗證過一次才會記錄下來，金鑰對不上會立刻明顯報錯。

3. **撤銷**會在本機帳本（`distributor.db`）裡把該金鑰標記為已撤銷，並寫進安全稽核 log。要讓已經啟用的實例知道，得靠下面說的 phone-home 更新與簽章過的 CRL，UI 會誠實標示這個時間差，不會假裝撤銷是即時生效的。

### 讓已發出的金鑰保持有效（更新與撤銷）

設定好簽發金鑰之後，原廠的 gateway 也會為自己簽發的金鑰提供一套輕量的控制平面，讓這些金鑰不會踩到 60 天離線降級，撤銷狀態也能傳播出去。以下兩個公開端點都以 `[distributor] issuer_key_path` 自我把關（沒設定 ⇒ 回 `404`，一般 gateway 完全不會暴露這兩個端點）：

| 端點 | 用途 |
|---|---|
| `POST /v1/license/refresh` | 用 `last_phone_home = now` 重新簽署呼叫方的授權。**絕對不會延長期限**：過期的金鑰要靠重新簽發，不是靠更新。已撤銷的金鑰回 `revoked`；指紋不符或已過期回 `403`。 |
| `GET /v1/license/crl` | 一份簽章過的憑證撤銷清單（用跟 client 驗證時同一份標準化 payload 做 Ed25519 簽章），列出所有已撤銷的 `subscription_id`。TTL 7 天。 |

經銷商的實例**完全不用改任何程式碼**，只要用一個環境變數指向原廠的 gateway 即可：

```bash
# 在經銷商的 DuDuClaw 實例上
export DUDUCLAW_CONTROL_URL=https://your-gateway.example.com
```

設定好之後，經銷商的 gateway 會照著自己方案對應的排程去 phone-home，並且輪詢 CRL，於是：

- **更新**可以讓授權無限期保持有效（每次成功 phone-home 都會重新蓋 `last_phone_home` 時間戳），原廠控制台也會顯示每把金鑰的**最後更新時間**，當作還活著的訊號。
- **撤銷**會在 phone-home 的間隔內（OEM 方案大約一週）傳到經銷商那邊，另外也會在 CRL 輪詢週期內（24 小時）獨立傳到，兩者以先到的為準。

只要有設定簽發金鑰，經銷商控制台就會顯示 **Endpoint active** 徽章與這段設定範例。

> 誠實說明：如果經銷商**沒有**設定 `DUDUCLAW_CONTROL_URL`（也連不到任何雲端控制平面），這把金鑰在連續 60 天沒有成功 phone-home 之後，還是會降級成開源版。更新端點只對有指向它的實例才有效，能免除這個降級。

#### 把端點烤進金鑰裡（`[distributor] public_url`）

如果想讓經銷商完全不用設定 `DUDUCLAW_CONTROL_URL`，可以只宣告一次原廠 gateway 對外可連到的網址：

```toml
# ~/.duduclaw/config.toml（原廠端實例）
[distributor]
issuer_key_path = "/path/to/license-signing-v2.key"
public_url      = "https://your-gateway.example.com"
```

設定好之後，`distributor.issue` 就會把這個網址內嵌進金鑰，變成金鑰自帶的 `control_url`。經銷商的實例解析控制平面的順序是：`DUDUCLAW_CONTROL_URL` 環境變數 → 金鑰裡的 `control_url` → 內建預設值。所以只要簽發時有設定 `public_url`，這把金鑰就能**零客戶端設定**完成 phone-home 與更新，`duduclaw license refresh` 也不需要任何環境變數就能動。`control_url` 不是簽章 payload 的一部分（要竄改它需要本機對 0600 權限的 `license.json` 有寫入權，而且每次更新回應本身都會做簽章驗證，最壞情況就是「網址連不到」，跟現在一樣）。更新時會保留原本的 `control_url`。

### 把品牌散發給你的客戶

經銷商通常希望自己的品牌能出現在*客戶*的實例上，但客戶端並沒有白牌授權。**簽章品牌包**解決了這個問題：品牌設定由原廠的簽發金鑰簽章，任何實例只要在 `~/.duduclaw/branding.bundle.json` 找到一份驗證得過的品牌包就會自動套用，*顯示*不需要授權（要*編輯*還是得有白牌授權）。上游原廠的標示永遠疊在最上層，品牌包無法把它蓋掉。

**產生品牌包**

- *自助（線上）*：在經銷商自己那台白牌實例上，點**產生散發用品牌包**（RPC `branding.bundle.create`）。gateway 會把 `subscription_id`、機器指紋、目前的品牌設定送到原廠 gateway 的 `POST /v1/branding/sign`（同樣以簽發金鑰自我把關，並依 IP 做 10 次／分鐘的速率限制），對方會重新檢查訂閱狀態（要有效且指紋相符，跟更新端點用同一套把關規則；已撤銷或已過期一律拒絕），用權威版邏輯再消毒一次品牌內容、簽章，然後回傳可下載的品牌包。
- *原廠協同簽章（離線）*：當經銷商的實例連不到原廠時，原廠的 **/manage/distributors** 頁面有一個**協同簽章品牌包**對話框（RPC `distributor.bundle.sign`）：貼上經銷商的品牌設定 JSON，原廠就能在本機用簽發金鑰簽好。

**散發與套用**

把產生出來的 `branding.bundle.json` 放進客戶的 `~/.duduclaw/`（例如包進你自己的產品安裝程式裡）。gateway 啟動時會依下面的順序解析品牌設定：

1. 本機的 `branding.json`（透過授權過的編輯器設定）：如果存在就優先採用；
2. 簽章能對上內建簽發公鑰的 `branding.bundle.json`；
3. DuDuClaw 內建的預設值。

目前使用中的來源會透過頂層 `source` 欄位回報給 dashboard（`local`／`bundle`／`default`）。簽章、schema 或欄位驗證任何一項沒過的品牌包會被忽略並留一則警告（fail-closed，退回預設值），而且品牌包內容在讀取時還會再消毒一次，就算有人手動改過檔案，也夾帶不了不安全的 HTML 繞過原廠的簽章檢查。

## RPC 介面

| 方法 | 存取權限 |
|---|---|
| `branding.get`、`about.get` | 任何已登入使用者都能呼叫（回應內容帶 `source`、`about_html`、`accent_color`） |
| `branding.set`、`branding.reset`、`branding.preview`、`branding.bundle.create` | 需要管理員身分，且授權要有 `white_label` 功能（fail-closed） |
| `distributor.status/list/add/update/remove/issue/revoke`、`distributor.bundle.sign` | 需要管理員身分 |

## HTTP 控制平面介面（原廠 gateway，以簽發金鑰把關）

| 端點 | 用途 |
|---|---|
| `POST /v1/license/refresh` | 重新簽署授權（phone-home）。 |
| `GET /v1/license/crl` | 簽章過的撤銷清單。 |
| `POST /v1/branding/sign` | 為符合資格的訂閱簽署品牌包（速率限制 10 次／分鐘／IP）。 |
