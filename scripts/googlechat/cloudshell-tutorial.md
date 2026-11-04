# 建立 DuDuClaw Google Chat 應用程式

<walkthrough-tutorial-duration duration="10"></walkthrough-tutorial-duration>

## 確認使用自己的帳號

需要 Workspace Business／Enterprise、管理員允許 Chat 應用程式，以及建立專案、API、服務帳戶與金鑰的權限。Cloud Shell 已備有 Bash、Python 3 和 gcloud。先確認登入者：

```bash
gcloud auth list --filter=status:ACTIVE
```

請使用自己的 Workspace 使用者帳號。腳本不改變 gcloud 的預設專案，不會自動綁定帳務或調整組織政策。

## 建立 HTTP 連線資源

請換成您自己的公開 HTTPS 端點：

```bash
bash scripts/googlechat/bootstrap.sh --endpoint-url https://chat.example.com/webhook/googlechat
```

員工專屬應用程式加 `--agent alice`，HTTP 端點改為 `/webhook/googlechat/agents/alice`。每位員工要有自己的專案。加 `--dry-run` 可先預覽，完全不執行 gcloud 或寫檔；`--project-id` 可指定專案。`--out` 指定私密輸出目錄。金鑰是最後一個雲端建立步驟；建立後若後續步驟失敗，腳本會嘗試撤銷新金鑰並刪除本機檔案，撤銷失敗會明確提示。金鑰政策阻擋時，將印出的指令交給組織政策管理員審核；不要把政策拒絕當成登入問題重試。

## 完成 Chat 設定頁

**建立時請取消勾選「以 Google Workspace 外掛程式建構這個 Chat 應用程式」（Build this Chat app as a Google Workspace add-on）。** DuDuClaw 接收一般 Chat 事件，不支援外掛程式事件格式。

開啟腳本印出的 Configuration 連結，填名稱（最多 25 字）、公開頭像網址與說明，開啟 Interactive features、一對一及聊天室。

- HTTP：貼上端點，Authentication Audience 選 Project number。
- Visibility：填自己的 Workspace email，儲存。

這個設定頁沒有可用的自動設定 API，所以仍需手動儲存。

## 安全下載並匯入 DuDuClaw

使用 Cloud Shell 的下載功能，下載摘要中列出的 `service-account.json`，妥善保管；不要貼到聊天室、版本庫或工單。Cloud Shell 不是 DuDuClaw 主機，因此在 DuDuClaw 主機執行摘要中的 `duduclaw channels googlechat import …`，將 `--key-file` 改成下載後的本機路徑，並先執行 `chmod 600 /path/to/service-account.json`。檔案必須由目前使用者擁有，且不可為符號連結或硬連結。保留原本的 `--agent` 參數。Windows 請將金鑰放在目前使用者自己的設定檔目錄內（例如 `%USERPROFILE%`）；只接受一般檔案，拒絕重新解析點。

匯入需要正在執行的 DuDuClaw gateway 與管理員登入（有啟用 MFA 時也須驗證）。完成匯入後，在 Chat 加入應用程式並送出訊息。收到回覆才算完成收發驗證；腳本成功只代表資源準備完成。

## 開發者離線檢查

這些檢查不使用真實 gcloud 或帳務資源：

```bash
bash -n scripts/googlechat/bootstrap.sh
python3 -m unittest discover -s scripts/googlechat -p 'test_*.py' -v
# If installed:
shellcheck scripts/googlechat/bootstrap.sh
```

手動驗收需另用測試專案，分別測試 HTTP、重跑沿用資源、組織金鑰政策與帳務拒絕，以及從 Cloud Shell 下載後匯入的完整流程。不要把離線模擬測試當成真實 Google 連線驗收。

### CLI 匯入失敗時的手動處理

腳本一律寫入 `bootstrap-result.json`，只包含專案、金鑰 ID、檔案路徑等中繼資料，不含私鑰。CLI 讀取結果檔後，驗證金鑰檔的擁有者、權限、一般檔案與連結狀態；Unix 保留 `O_NOFOLLOW`／檔案 handle 檢查。驗證後的金鑰內容保留在記憶體中供匯入，不會重新開啟金鑰路徑。

腳本內部建立金鑰後若失敗，仍會嘗試撤銷該次新建金鑰並清理檔案。腳本成功後，CLI 若匯入失敗，會保留金鑰檔並以非零狀態結束；新建的服務帳戶金鑰仍然有效。CLI 會顯示檔案路徑，以及可直接複製的重試匯入與手動撤銷指令，不會自動執行撤銷或刪檔。撤銷指令的專案與服務帳戶取自 CLI 執行參數，金鑰 ID 取自已驗證的金鑰內容。員工專屬設定的重試指令會保留 `--agent`。

Windows 原生驗收須另行執行，離線測試不代表已通過。
