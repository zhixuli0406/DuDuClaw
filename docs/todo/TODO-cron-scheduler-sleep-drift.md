# TODO: 背景任務層靜默全滅（boot 卡點；初判「睡眠漂移」已被推翻）

> 狀態：Open（2026-08-14 排查中——容器重啟**不能**恢復，睡眠漂移假說已推翻）
> · 類型：可靠性 bug（平台）· 優先：**High**
> 發現於：LWM 實驗 D5——v1.56-dev 部署後排程全停。

## 一句話（2026-08-14 10:00 修正版）

v1.56-dev（2026-08-13 晚 bake）部署後，**cron／heartbeat／tick sources 全部
從未自動執行過**，而 HTTP、dashboard RPC、Telegram long-polling 完全正常，
健康檢查全綠、log 靜默（檔案層固定 WARN、stdout 無 info）——初判的
「Mac 睡眠 monotonic 漂移」已被**容器重啟後仍不恢復**推翻；現行假說是
**boot 序列卡在 Telegram 啟動之後、heartbeat 之前的某個 channel await**
（該時段其他 session 的 channel WIP 被 working-tree bake 烤進 image）。

## 排查證據鏈第二輪（2026-08-14 10:00 後——嫌疑收斂到 pro 編譯差異）

7. 本機重現 ×4 全部**不卡**：乾淨 home／實驗 config.toml／實驗 agents+真 TG
   token／`DUDUCLAW_EDITION=enterprise`——開源 binary 每輪都完整 boot
   （heartbeat/cron/tick 全起）。
8. **決定性對照**：同容器、同 home、同批編譯的**開源** `duduclaw gateway`
   起第二實例——cron 載入 5 任務、heartbeat 立刻 firing、tick 迴圈活；
   生產 `duduclaw-pro` 行程三次重啟後 scheduler 仍死。
9. `EnterpriseExtension` 讀畢：純被動 RPC handler，無 boot hook——排除。
10. 剩餘唯一差異軸：**Dockerfile.experiment 對 pro 用獨立 manifest 編譯**
    （`--manifest-path commercial/duduclaw-pro-gateway/Cargo.toml`，獨立
    target/、feature 解析與 workspace build 不同；開源 binary 帶
    `duduclaw-gateway/dashboard`）。v1.55 同法無恙 ⇒ 觸發者=2026-08-13
    working-tree 某變更 × pro 的 feature 組合（feature-gated cfg 分支卡
    boot）。二分方向：對 pro manifest 逐 feature 對齊 workspace 重編。

## 止血（2026-08-14 10:05，生效中）

宿主 crontab 代打五條排程（標記 `DUDUCLAW-LWM-STOPGAP`，
`crontab -l | grep -v DUDUCLAW-LWM-STOPGAP | crontab -` 一鍵整組移除），經
容器內 `fire_cron.py`（volume 持久）→ MCP `run_cron_task` standalone 路徑
——該路徑經活體驗證完整（更新 last_run、dispatch-run 錄製、判官照走）。

## 排查證據鏈第一輪（2026-08-14 早）

1. intraday（`*/3 9-13`）自部署後零自動 fire；重啟後再過 4+ 邊界仍零。
2. `docker restart` 與 `--force-recreate` 皆無效 ⇒ 非時鐘漂移。
3. 24 秒 `/proc` 採樣：tick_quotes.py（15s command source）零 spawn ⇒
   tick sources 也死 ⇒ 背景任務層全滅，非 cron 單獨。
4. TG long-polling 收發正常（boot 序列 `start_telegram_bots` 在 818 行完成）；
   heartbeat（851）之後的一切無跡象 ⇒ 卡點在 821-851 的九個 channel await。
5. 手動 `run_cron_task`（MCP 行程內自建 store）正常 ⇒ 排程邏輯本身無恙。
6. RUST_LOG=info 不影響輸出（stdout 2 行、檔案層 WARN-only）——log 面需要
   一併修（boot 各階段至少一行可見標記）。

## 現場證據（2026-08-14 09:00–09:33 台北）

- `lwm-intraday`（`*/3 9-13`）連續錯過 11 個排程邊界，`last_run_at` 停在前一日；
  `lwm-premarket` 08:40 未跑。
- 同時段 **Telegram long-polling 正常**（09:51 收使用者訊息並完整回覆——
  網路驅動的 task 不依賴凍結期間的 timer）。
- `docker logs --since 4h` **零輸出**（背景任務全體靜默）。
- 容器 `Up 8 hours (healthy)`——healthcheck 是 HTTP，完全測不到這個失效。
- `docker restart` 後排程恢復（timer 重建）。

## 修法方向（未定案）

1. **Wall-clock 對齊的 tick**：排程迴圈每次醒來先比對 `SystemTime`——
   偵測「monotonic 睡過頭/wall clock 跳躍」（差值 > 2×tick 間隔）即立刻重排，
   不等 timer 自然到期。`tokio::time::sleep` 改為短間隔（≤60s）輪詢 wall clock
   判斷「哪些 cron 到期」，而非 sleep 到下一個排程時刻。
   （CronScheduler 若已是短輪詢，則問題在更上層的 interval 漂移——
   需讀 `cron_scheduler.rs` 實測定位；本篇僅記行為證據。）
2. **睡眠偵測哨兵**：一個每 30s 的 task 記錄 `(Instant, SystemTime)` 對，
   醒來發現兩者差值突增 → log warning + 對所有排程器發 re-arm 訊號 +
   Activity Feed 事件（讓靜默失效變得可見）。
3. **healthcheck 加強**：健康檢查納入「排程器最近 tick 時間」——排程器
   停擺超過 N 分鐘即回 unhealthy，讓 Docker restart policy 自癒。

## 影響範圍

所有 macOS／筆電宿主的部署（桌面版、實驗容器）；伺服器常開環境不受影響。
與 Discord Gateway 的 stall watchdog（v1.9.2）同類問題——那裡已經修過一次，
模式可以複用。

## 驗收

- [ ] 模擬 monotonic 跳躍（測試注入）後排程器在 1 個 tick 內自癒。
- [ ] 睡眠喚醒後 5 分鐘內錯過的排程被偵測並記錄（不靜默）。
- [ ] healthcheck 在排程器停擺時轉 unhealthy。
