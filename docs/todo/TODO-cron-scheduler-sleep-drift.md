# TODO: 背景任務層靜默全滅（根因已定位：pro 依賴解析漂移＋log 全啞）

> 狀態：**修復已落地，待重烤容器活體驗證**（2026-08-14 晚）
> · 類型：可靠性 bug（平台）· 優先：**High**
> 發現於：LWM 實驗 D5——v1.56-dev 部署後排程全停。

## 根因（2026-08-14 第三輪，兩個獨立缺陷疊加）

1. **pro binary 沒有安裝 tracing subscriber**（診斷性根因）：
   `duduclaw-pro.rs` 的 `tracing_subscriber_init()` 只印一行 stderr，註解宣稱
   「開源 gateway 會自己裝 log layer」——不實（`init_log_broadcaster()` 只建
   channel；真正的 subscriber 由開源 CLI 的 `entry_point` 安裝，pro 完全繞過）。
   全 gateway 的 `info!`/`warn!` 在 pro 下被靜默丟棄——「log 靜默」不是卡死
   證據，是 log 系統本身死的。先前「卡點在 818-851 channel await」的定位由
   log 缺席反推，**不成立**（且 dashboard RPC 正常 ⇒ boot 其實有走完）。
2. **pro 獨立 workspace 的 Cargo.lock 從未版控**（行為性根因，最強嫌疑）：
   每次 bake 就地重新解析，與 workspace lock 相比 **124 個共同套件版本不同**
   （tokio 1.52.0→1.52.3、mio 1.2.0→1.2.1、hyper 1.9.0→1.10.1、rustls
   provider 由 aws-lc-rs 變 ring…）。cron／heartbeat／tick 三個排程迴圈全靠
   `tokio::time::sleep`，網路 I/O 不靠 timer——「排程死、HTTP/TG 活」與
   「同容器同 home 的開源 binary（workspace lock）正常、pro 三次重啟照死」
   完全吻合。「v1.55 同法無恙」＝程式碼沒變、bake 時抓到的依賴變了。
   （feature 差異軸已排除：兩邊 gateway feature set 同為 `{dashboard}`。）

## 已落地的修復（2026-08-14）

- **pro 裝上真 subscriber**：新增共用 `duduclaw_gateway::log::init_tracing_stack(home)`
  （stderr + 每日輪替檔案 + BroadcastLayer + OTel，等價開源 CLI 疊層），
  `duduclaw-pro.rs` 改為呼叫之。任何繞過 CLI 直接 boot gateway 的嵌入者都必須呼叫。
- **依賴對齊**：兩個 pro Dockerfile（enterprise + experiment）在 build 前
  `cp Cargo.lock commercial/duduclaw-pro-gateway/Cargo.lock`——同一 image 內
  兩個 binary 共享依賴一律解析到 workspace-locked 版本；本機 pro lock 也已
  種鎖並納入 commercial repo 版控（gitignore 解禁）。
- **boot 路徑無界 await 補洞**：googlechat／msteams 的 token client 是 boot
  序列上僅有的無 timeout 網路 await，補上 30s（與其他七通道對齊）。
- **boot 階段可見標記**：channel 啟動段前後、HTTP bind 前補 `info!` 標記。
- **healthcheck 真材實料**：cron／heartbeat 迴圈每 tick 寫 `LAST_TICK_UNIX`；
  `/healthz` 於任一迴圈停擺逾 300s（或 boot 後從未啟動）回 503 附診斷欄位；
  實驗部署與四個產品 compose 的 healthcheck 全部由 `/health`（恆 ok）改指
  `/healthz`。

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
    → **第三輪已否定 feature 假說**（兩邊 feature set 同為 `{dashboard}`，
    `server.rs` 全檔僅一處 `#[cfg(feature)]` 且兩邊皆開）；真差異＝未版控的
    pro Cargo.lock 每次 bake 重新解析（124 套件版本漂移，見頂部根因）。

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

## 原「修法方向」核對（第三輪讀碼結論）

1. ~~Wall-clock 對齊的 tick~~ **不需要**：CronScheduler 與 HeartbeatScheduler
   本來就是 30s 短輪詢＋`Utc::now()` wall-clock 到期判斷（`cron_scheduler.rs`
   `TICK_INTERVAL_SECS=30`、`heartbeat.rs` 同款），設計上不存在「sleep 到下一
   個排程時刻」的漂移面；睡眠假說本就已被容器重啟不恢復推翻。
2. ~~睡眠偵測哨兵~~ **以 healthcheck 取代**：失效模式是「timer 整層死」而非
   「跳躍後漏排」，per-tick 時戳＋`/healthz` 停擺判斷已覆蓋可見性需求。
3. **healthcheck 加強** ✅ 已落地（見上方修復清單）。

## 影響範圍

pro binary 的所有 Docker bake（enterprise image、實驗容器）；開源 binary
（workspace lock 編譯）不受影響。log 啞巴問題則影響所有 pro 部署的可診斷性。

## 驗收

- [x] healthcheck 在排程器停擺時轉 unhealthy（`/healthz` 503＋`schedulers`
      診斷欄位；compose healthcheck 已改指 `/healthz`）。
- [x] boot 各階段至少一行可見標記（channel 段前後、HTTP bind 前）＋ pro
      binary log 復活（subscriber 修復）。
- [ ] **活體驗證**：重烤實驗容器（依賴已種鎖）→ 部署 → 原生 cron 於下一個
      排程邊界自動 fire、tick sources spawn、`docker logs` 有 info 輸出
      → 移除宿主 crontab 止血（`DUDUCLAW-LWM-STOPGAP` 整組）。
- [ ] 若重烤後排程仍死：log 已復活，直接讀 boot 標記定位——屆時再開下一輪。
