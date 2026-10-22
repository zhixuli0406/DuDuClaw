# 經銷商金鑰控制台（白牌功能已移除）

> **白牌功能已在 1.72.2 移除。** 產品名稱、logo、副標題、主題色、公司欄位、About HTML 區塊、簽章品牌包與 `branding.*` RPC 都不存在了，`branding.json`／`branding.bundle.json` 會被忽略。每個 dashboard 一律顯示 DuDuClaw 的產品識別與固定的上游原廠標示（嘟嘟數位科技有限公司 / Dudu Technology Ltd.），這個標示編譯在執行檔裡，設定檔和 RPC 都改不了。
>
> 總經銷專用 build 可以在關於頁多一塊總經銷資訊。內容由總經銷在授權 console 維護，不是 dashboard 設定，請見 [distributor-notice.md](distributor-notice.md)。

這一頁現在只說明原廠端的經銷商金鑰控制台。

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

   **指紋要用 1.66.1（含）以後的版本取得。**
   - **macOS**：1.66.1 起改以 `IOPlatformUUID` 綁定硬體。macOS 26（Darwin 27）對一般未授權行程遮蔽所有介面的 MAC（一律回報 `02:00:00:00:00:00`），挑介面救不回來，所以換了識別來源。**既有 macOS 安裝的指紋都會因此改變**。
   - **Linux／Windows**：指紋沒有變動。挑選邏輯只是多跳過全零／佔位／multicast 位址；原本第一個介面就有效的主機，算出來的值與 1.66.0 完全相同。

   **換機或升級之後**：在新機（或升級後的機器）跑 `duduclaw license fingerprint`，把新指紋回報給簽發方重簽；或直接跑 `duduclaw license rebind`（打控制平面 `/v1/license/rebind`，由它重簽）。舊指紋簽出的授權**仍然有效**（相容判定），gateway 只會在 log 留一則 WARN 建議重簽；`duduclaw license status` 的 `Fingerprint binding:` 那行會顯示 `strong` 或 `legacy (re-issue recommended)`。想看這台機器接受哪些指紋，跑 `duduclaw license fingerprint --all`——第一行永遠是強指紋（腳本照抓第一行即可），其餘是相容值。

   若 `fingerprint`／`status` 警告「No hardware identity is readable」，代表這台機器既沒有平台 UUID 也沒有可用網卡位址（容器或受限環境），指紋只綁主機名稱——仍可穩定發卡，但不提供硬體層級的防複製。

   **換機或升級 OS 之後**：在新機（或升級後的機器）跑 `duduclaw license fingerprint`，把新指紋回報給簽發方重簽一次即可。舊指紋簽出的授權在 1.66.1 上**仍然有效**（相容判定），gateway 只會在 log 留一則 WARN 建議重簽；`duduclaw license status` 的 `Fingerprint binding:` 那行會顯示 `strong` 或 `legacy (re-issue recommended)`。想看這台機器接受哪些指紋，跑 `duduclaw license fingerprint --all`——第一行永遠是強指紋（腳本照抓第一行即可），其餘是相容值。

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

## RPC 介面

| 方法 | 存取權限 |
|---|---|
| `about.get` | 任何已登入使用者都能呼叫（固定的產品與原廠欄位、版本、授權方案；只有總經銷專用 build，或總經銷發的授權取得簽章內容時，才帶 `distributor_notice`） |
| `distributor.status/list/add/update/remove/issue/revoke/upgrade` | 需要管理員身分 |

## HTTP 控制平面介面（原廠 gateway，以簽發金鑰把關）

| 端點 | 用途 |
|---|---|
| `POST /v1/license/refresh` | 重新簽署授權（phone-home）。 |
| `GET /v1/license/crl` | 簽章過的撤銷清單。 |
