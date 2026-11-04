# Linux 受保護服務的夜間自動升級

這組選用腳本每天凌晨檢查官方 gateway release，有較新版才交給已安裝的受保護升級指令下載、驗簽與安裝。執行中的 gateway 會重啟。完整操作與限制見[三語指南](../../../../docs/guides/zh-TW/linux-protected-service.md#自動升級選用)。

需要原生 Linux 受保護服務、systemd、GNU coreutils（含 `sort -V`）、curl、grep、sed、tr，以及既有升級流程需要的工具。固定路徑與 URL 不接受環境變數覆寫。03:30 採當地時間，最多延遲 20 分鐘；早上開機不補跑。

此目錄在 v1.79.3 之後的原始碼才加入，目前尚無已發布標籤包含它；發行 tarball 也不含 `scripts/`。第一個包含此目錄的版本發布前，請從固定的完整 commit SHA（下方以 `<commit>` 表示，請換成你審閱過的那一筆）取得並審閱，不要使用會移動的 `main`。它只呼叫既有的 `service upgrade --protected-linux --version`，可搭配 v1.79.3 受保護安裝。

審閱 `install.sh`、`duduclaw-auto-upgrade`、`.service`、`.timer` 後，將整個目錄複製到 `/root/duduclaw-auto-upgrade` 等只有 root 可寫入的位置，並確認檔案與所有上層目錄權限。不要從使用者可寫入目錄以 root 執行腳本。檔案可能在審閱後、複製前被替換，請再核對 root 副本；安裝程式會拒絕來源目錄與四個腳本／unit 檔案的符號連結、非一般檔案、非 root:root 擁有或群組／其他人可寫入的項目，也會檢查解析後目錄至 `/` 的每一層祖先；即使有 sticky bit 的 `/tmp` 也不接受。通過後，先複製至 `/run` 下的 root 專用 0700 暫存目錄，再檢查副本為一般檔案且大小為 1 byte 至 1 MiB，最後從副本安裝並清理暫存。這些權限檢查無法證明程式碼出處；仍須核對固定 commit，改成 root 所有不代表已驗證內容。

```sh
# Replace <commit> with the reviewed full commit SHA; obtain files from that SHA.
# Run from the reviewed checkout; the destination must not already exist.
sudo cp -r --no-preserve=ownership scripts/linux-protected/auto-upgrade /root/duduclaw-auto-upgrade
sudo chown -R root:root /root/duduclaw-auto-upgrade
sudo chmod -R go-w /root/duduclaw-auto-upgrade
sudo sh /root/duduclaw-auto-upgrade/install.sh
sudo /usr/local/sbin/duduclaw-auto-upgrade --dry-run
systemctl list-timers duduclaw-auto-upgrade.timer
sudo journalctl -u duduclaw-auto-upgrade.service
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

安裝後腳本為 root:root、0755，units 為 root:root、0644。解除安裝只移除這組排程與檔案；若當時已有升級執行中，會先停用 timer，再拒絕移除；請等升級完成後重跑解除安裝。

檢查只涵蓋最近 30 筆 release，接受純 `vX.Y.Z` tag，忽略 desktop 與帶後綴的 tag。解析支援單行壓縮與多行 JSON，只檢查外層陣列並擷取版本欄位，未完整驗證 JSON 語法，也不解析 `prerelease`：純版本 tag 即使標為 prerelease 仍納入；未登入 API 不回傳草稿。沒有較新版正常結束；API 失敗、沒有合格 tag、版本下限遺失或不符格式、存在待完成升級檔案則失敗且不升級。若 release 先發布、Linux 封存檔尚未上傳完成，當晚升級會失敗，隔晚重試；請查看 journal。journald 保存輸出。儀表板在 v1.80.0 前仍顯示手動指令。

API 回應上限為 8 MiB，保留 60 秒逾時；curl 的 `--max-filesize` 在舊版僅能檢查已知 Content-Length，因此回應先寫入私有 `/tmp` 下 0700 目錄中的暫存檔，解析前再檢查實際大小，退出時清理。systemd 的 `PrivateTmp=yes` 提供私有且可寫入的 `/tmp`；主機寫入只開放 `/opt/duduclaw` 與 `/var/lib/duduclaw-protected`，服務啟動逾時設為 1 小時以容納下載重試與安裝／重啟。純 `vX.Y.Z` 即使 GitHub 標為 prerelease，較新且驗簽通過仍會安裝。封存檔的 minisign 簽章是信任錨點，版本選擇本身沒有簽章。

## 驗證

```sh
sh -n scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade
sh -n scripts/linux-protected/auto-upgrade/install.sh
python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v
```

測試只改寫暫存副本中的固定常數並模擬 root、curl 與升級程式；不連線、不安裝。沒有 `systemd-analyze` 時，unit 驗證明確跳過，仍需在 Linux 主機驗收。
