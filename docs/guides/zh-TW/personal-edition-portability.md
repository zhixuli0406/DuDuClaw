# 個人版資料可攜：自架 ↔ 代管互轉

> 適用：DuDuClaw 個人版（Personal Edition）。代管的個人版實例與你自架的個人版是**同一份產物**，
> 因此資料可在兩者間自由搬移，沒有 vendor lock-in。

## 為什麼可攜

個人版（`EditionProfile::Personal`）是一個**自足、單一擁有者**的部署單位。Cloud「代管」只是
「幫你在我們的基礎設施上跑同一份個人版」——容器內帶的就是 `DUDUCLAW_EDITION=personal`。
所以你的全部狀態都集中在一個目錄 `~/.duduclaw/`：

| 內容 | 路徑 |
|------|------|
| Agents（SOUL.md / CLAUDE.md / agent.toml / .claude） | `~/.duduclaw/agents/` |
| 記憶（episodic / semantic SQLite + FTS5） | `~/.duduclaw/memory*.sqlite` |
| 設定 | `~/.duduclaw/config.toml`、`~/.duduclaw/inference.toml` |
| 授權 | `~/.duduclaw/license.json` |
| 任務 / 自動化 / 事件 | `~/.duduclaw/*.jsonl`、`events.db` |
| 進行中的外送 lease（含 WAL／SHM） | `~/.duduclaw/egress_leases.db`、`egress_leases.db-wal`、`egress_leases.db-shm`、`egress_leases.lock` |

## 立即可用：手動搬移（tar）

今天就能用標準工具搬移整份個人版狀態：

```bash
# 1. 從來源（自架或代管匯出的目錄）打包
tar -C "$HOME" -czf duduclaw-export.tar.gz .duduclaw

# 2. 搬到目標機器後解開（先停掉 gateway）
tar -C "$HOME" -xzf duduclaw-export.tar.gz

# 3. 啟動，個人版會直接載入既有 agents / 記憶
duduclaw start
```

> 代管客戶可在 Dashboard 申請匯出，取得同格式的 `~/.duduclaw/` tarball，解開後即可自架；
> 反之亦然，因為兩端是同一份個人版產物，**不需要任何轉換**。

## 切換時的注意事項

- **授權**：`license.json` 綁定機器指紋（macOS 使用硬體 UUID；Linux／Windows 使用 hostname + MAC）。換機器後個人版核心照常運作（Apache 2.0），
  若有 Pro 加值模組，需要由發出授權的一方把授權重新綁定到新機器。
- **頻道 token**：channel bot token 在加密設定內，會一起搬過去；換 IP/網域時記得更新 webhook URL。
- **EditionProfile**：所有建置以有效授權決定版本上限（Business、OEM、Partner、Self-Host Pro 可用 `enterprise`），
  沒有有效授權時使用 `personal`。`DUDUCLAW_EDITION`（`personal` 或 `enterprise`）優先於嵌入主程式的
  `GatewayConfig.edition`，兩者都只能縮限授權允許的版本；要求升級時會忽略該值，每個來源每個行程警告一次。
  例如 Business 授權可用 `personal` 降級，個人授權不能用 `enterprise` 升級。
  啟用 Debug assertions 也不能升級；測試使用臨時簽署授權。獨立測試 API 必須同時啟用非預設的 `test-edition-override` feature 與 Debug assertions，release 建置拒絕該 feature。儀表板沒有切換開關，`agent.toml [edition] profile` 也沒有程式讀取；
  自架企業版需要具備該權益的有效授權。

## macOS 授權指紋與重新綁定

macOS 新指紋使用 `macos-uuid-v2:<32 位十六進位字元>`，只由 `IOPlatformUUID` 產生，不包含主機名稱。`duduclaw license fingerprint`、Dashboard 啟用卡與換綁申請都使用此值。啟用只安裝已簽章授權，不會自行要求簽發。舊版 hostname + UUID 候選仍可驗證並標示 legacy；過去的主機名稱無法從雜湊還原，若舊綁定不再相符，請聯絡簽發端支援新格式後執行 `duduclaw license rebind`。

MAC 衍生識別分為三種強度。**Hardware** 是全球管理的單播 MAC。**Instance** 是非已知共用占位值的本機管理單播 MAC，包含 GCP、AWS 與 Docker 網卡；狀態顯示「雲端／容器識別」。兩者都可驗證既有付費綁定，也可用於新啟用與經核准的換綁。主要 MAC 優先選 Hardware，其次 Instance。**Weak** 包含未取得或全零 MAC、共用 `02:00:00:00:00:00`、廣播 `FF:FF:FF:FF:FF:FF`、多播 MAC 與占位 UUID；即使相符，仍維持 OpenSource 直到核准換綁，也不會用於新綁定提案。Linux／Windows 保留舊版 hostname + MAC 雜湊，歷史 Instance 候選仍可驗證（非主要候選時 `legacy = true`）。作業系統管理員仍能複製這些識別；Instance 表示本機取得的識別證據，不代表硬體證明。macOS UUID v2 指紋與 UUID 讀取失敗時的限制維持不變。

重新綁定為新格式後，舊版程式無法驗證該授權。請先升級所有會讀取這份授權的程序；需要降版時，須先請簽發者安排相容的重新綁定。

UUID 最多讀取三次，每次逾時五秒。仍讀不到時，不會用 MAC 候選放行綁定 UUID 的授權；CLI 與儀表板也會拒絕提供新簽發用的指紋，待 UUID 可讀取後再試。本次不建立 UUID 磁碟快取：即使檔案僅擁有者可讀寫，同一帳號執行的其他程式仍可竄改，無法證明 UUID 確實來自本機。Linux／Windows 保留「主機名稱 + MAC」綁定：真實的全球管理單播 MAC 仍視為 Hardware，可繼續驗證綁定機器的付費授權，不需遷移。已安裝授權失效時，核心功能維持 OpenSource；管理員的儀表板顯示警示，`duduclaw license status` 與 `duduclaw doctor` 也會列出原因。

**控制平面相容性：** refresh 的 `machine_fingerprint` 保留已安裝授權的綁定；不同的 macOS 穩定 UUID 透過 `new_fingerprint` 提案，**只有帳號持有人在控制台核准換綁後才生效**。rebind 傳送 `old_fingerprint`、`new_fingerprint` 與新的 nonce。指紋是公開識別值，不能當成所有權證明。已驗簽的 `status: "pending"` 會保留原授權並提示等待控制台核准；HTTP 202 也必須附上此已簽章本文，空白或未通過驗證的回應一律拒絕。Linux／Windows 的 refresh 保留相符的 Hardware 或 Instance 綁定，否則提出本機優先的非 Weak 候選；未綁定 OEM 的 refresh 綁定不變；macOS 讀不到 UUID 時，已綁定授權的 refresh／rebind 停止，不提案改綁 MAC。

接受 active 回應前，Gateway 與 CLI 會驗證授權與回應兩份簽章、完全相同的 nonce 與原訂閱，以及等於原綁定或提案綁定的回傳指紋（明確 rebind 必須等於提案）；另檢查期限、候選強度、離線寬限及方案／部署型態。active、revoked 與 pending 都必須使用 v2 簽章封套。撤銷回應必須通過驗簽且 `effective_from` 已到期，才會持久保存撤銷標記並移除已安裝授權；偽造或過時的回應不會改動授權或標記。

**簽發端回應簽章契約：** 僅接受數字 `response_signature_version: 2`；缺少版本、舊版、格式錯誤或未知版本一律拒絕。本文包含 `status`、`reason`、RFC3339 格式的 `effective_from`、`nonce`、`subscription_id`、`original_fingerprint`、`proposed_fingerprint`、`license_digest`、`public_key_id` 與 base64 Ed25519 `response_signature`。簽章內容為緊密 UTF-8 JSON 陣列 `["duduclaw:license-response:v2",2,status,reason,effective_from,nonce,subscription,original,proposed,license_digest]`。沒有提案時，提案綁定填原綁定。active 的 `license_digest` 是緊密序列化 `License` 的小寫 SHA-256，依 Rust 欄位順序及可選欄位省略規則，包含授權簽章與 `control_url`；回應與授權的 `public_key_id` 必須相同且可信。pending／revoked 的摘要為空字串，由回應的 `public_key_id` 選擇信任清單中的公鑰。以 `duduclaw_license::refresh::response_payload` 為參考序列化器。簽發端須驗證帳號持有人，等待控制台核准，更新綁定，再簽署授權及回應。升級客戶端前，須先部署支援此契約的簽發端。

Gateway 與 CLI 的啟用、匯入、更新、換綁及撤銷共用跨程序 advisory lock。送出網路請求前，在鎖內取得完整授權檔及撤銷標記的摘要，再釋放鎖；收到回應後重新取鎖，比對任一摘要有變更就拒絕寫入。Gateway 另檢查記憶體 revision。寫入使用唯一暫存檔、檔案 fsync、原子 rename，Unix 另同步目錄。此鎖保護遵循協定的產品程序；具有直接寫入權限的程序仍可忽略 advisory lock。Windows 原生持久化、簽發端實際部署、控制台核准、郵件投遞與客戶遷移仍須另外驗收。授權檔本身的簽章仍排除 `control_url`，但必要的回應簽章會涵蓋該網址。外連另受[固定主機與 DNS 政策](../../features/zh-TW/05-security-defense.md#授權綁定與更新)保護；自訂簽發端即使已在授權檔中提供網址，客戶端仍須在本機明確設定 `DUDUCLAW_CONTROL_URL`。忽略 `new_fingerprint` 的服務會保留舊綁定；弱綁定會一直維持 OpenSource，直到核准遷移成功。

## 路線圖（規劃中）

- Dashboard「一鍵匯出我的資料」按鈕（產生 tarball）。
- 啟動時一鍵匯入代管匯出的 tarball。
- 代管 ↔ 自架 round-trip 一致性自動驗證。
