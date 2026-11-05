# Linux 受保護服務的夜間更新要求

這組腳本保留每日 03:30、最多延遲 20 分鐘且不補跑的排程，改向獨立更新服務提出要求。

**目前僅供開發，請勿啟用 timer。** 新更新服務已實作世代切換、固定服務重啟及健康讀回；跨套件的排空、遷移與就緒端點仍須串接，缺少端點時會回報維護，不會提交新的版本下限。遷移 bootstrap 雙簽、真實 FIDO2 與 Linux 實機更新尚未完成。只把這組檔案複製到 v1.79.3 不會安裝服務、帳號或信任根；既有夜間部署應保留先前審閱過的固定版本 helper，等待完整遷移。

目前 helper 只接受無參數或 `--dry-run`，並呼叫固定的 root 管理程式 `/usr/libexec/duduclaw-updated-launch --check-latest`。`--dry-run` 只查候選版本；正式要求經 Apply RPC 排隊，排隊成功不代表安裝成功。沒有更新服務時拒絕，不退回舊的 CLI 安裝器。

候選查詢由 `duduclaw-dl` 帳號執行 curl，清除繼承環境；API 回應上限 1 MiB，完整 JSON 必須有 `draft=false`、`prerelease=false` 與正式版本。未簽章索引僅選候選版本，官方簽章清單與 root 管理的版本／序號才決定能否接受。要求端 unit 逾時為六分鐘，不寫入發行檔案。固定路徑與網址不能用環境變數覆寫。

## 日後部署的來源要求

完整隔離遷移提供前，不要執行安裝指令。屆時須依已發布的遷移指引佈建更新服務，再審閱精確 commit 的四個腳本／unit 檔。不要使用會移動的 `main`，也不要從使用者可寫入的 checkout 以 root 執行 `install.sh`。

安裝器仍要求 root:root 來源、一般檔案與所有上層目錄不可被其他人寫入，拒絕符號連結及 sticky `/tmp`。通過後複製至 `/run` 的 root 專用 0700 快照，重驗型別與 1 byte 至 1 MiB 大小後安裝。權限檢查無法證明程式碼出處；必須核對固定 commit 與 root 副本。

若要移除已安裝的排程，使用先前審閱過的安裝器副本：

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

解除安裝只移除這組排程與檔案；若升級正在執行，先停用 timer，再拒絕移除。請等作業結束後重試。它不清理新隔離安裝的服務或資料。

## 驗證

```sh
sh -n scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade
sh -n scripts/linux-protected/auto-upgrade/install.sh
python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v
```

測試只改寫暫存副本並模擬 root 與更新程式，不連線、不安裝。沒有 `systemd-analyze` 時，unit 驗證明確跳過，仍需 Linux 實機驗收。完整狀態見[繁體中文指南](../../../docs/guides/zh-TW/linux-protected-service.md#每晚自動更新)。

安裝器在寫入前確認凍結更新啟動器是 root:root、不可由其他帳號修改的單一連結可執行檔，並逐層檢查其祖先；寫入前與啟用 timer 前均要求更新服務狀態精確為 `active`。這是排程的安裝前提，不是完整隔離或更新成功的證據；安裝器不會為了檢查而送出更新要求。
