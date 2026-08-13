# Umbrel 上架

`duduclaw/` 內是 Umbrel app 的兩件套（`umbrel-app.yml` + `docker-compose.yml`）。

## 發佈路徑（先 community、後官方）

1. **Community App Store（零審核、即時）**：建一個公開 repo（例 `zhixuli0406/duduclaw-umbrel-store`），套用官方模板 [getumbrel/umbrel-community-app-store](https://github.com/getumbrel/umbrel-community-app-store) 的結構（store id 前綴要求，如 `duduclaw-duduclaw`），把 `duduclaw/` 目錄放進去。使用者在 Umbrel App Store 設定貼上 repo URL 即可安裝。
2. **官方商店**：fork [getumbrel/umbrel-apps](https://github.com/getumbrel/umbrel-apps) → 放入 `duduclaw/` → PR（人工審核、時程數週）。gallery 截圖（1920×1080 ×3–5 張）在送官方店前補。

## 發佈前檢查

- [ ] 對照官方模板確認 manifest 欄位仍相符（schema 會演進；特別是 `manifestVersion` 與 gallery 要求）
- [ ] 在真機或 [umbrel-dev](https://github.com/getumbrel/umbrel) VM 實裝一次：dashboard 可開、資料重啟不掉（`${APP_DATA_DIR}/data` 掛載）
- [ ] 確認 GHCR image tag 與 manifest `version` 同步（release.sh 出貨後更新）
- [ ] 容器內 home 路徑實測：image 以 `duduclaw` 使用者跑，home 若非 `/home/duduclaw` 需修 volume 目標

## 備註

- Claude CLI OAuth（訂閱帳號）在容器內的 `setup-token` 交接是最卡的一步——安裝說明要引導使用者用 API key 起步或照 `docs/guides/docker.md` 的 token 注入流程。
