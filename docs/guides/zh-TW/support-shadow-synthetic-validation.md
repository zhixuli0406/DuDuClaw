# 合成客服 shadow 驗證

在儲存庫根目錄用一行指令執行 C7 工程 fixture：

```bash
CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway c7_synthetic_shadow_harness --lib -- --nocapture
```

預設的測試執行緒堆疊大小已足夠（2026-09 驗證：不設定 `RUST_MIN_STACK` 時，harness 約 90 秒完成，CI 執行 `duduclaw-gateway` 測試套件也是這樣跑）。如果日後對這個 fixture 的修改讓堆疊用量超過預設值，可用 `RUST_MIN_STACK=33554432` 加大工作執行緒的堆疊，這是選用項目，目前不需要。

測試會印出一行 `C7_SYNTHETIC_SHADOW_SUMMARY=` JSON。內容只有案例數量、審查決定與失敗碼，不含工單 ID、來源文字或租戶資料。測試通過代表目前的待處理量（backlog）與工單 SLA shadow 引擎，能處理一段確定性的 21 天序列，且每天的預測都在該日結果來源出現之前就已提交。接著它會檢查缺少分數、對最後一天的彙總與工單結果做過審查的更正，以及來源移除這幾種情況。缺失、過期與已撤銷的證據必須抑制完整視窗的審查；已儲存的歷史畫面保留其原本的意義，直到所連結的來源被撤銷為止。

歷史日期與匯入時間只在 `#[cfg(test)]` fixture 中以人為方式設定。正式環境的 policy、預測與計分方法仍使用系統時鐘，並拒絕過晚的提交。這個 fixture 是合成的工程證據，不衡量真實客服資料上的技巧（skill），不校準不確定度，也不授權模型升級或人力調度行動。Decision Lab 的合成工程驗證仍是目前本地 fixture 的儀表板檢視，這個測試的輸出刻意與營運證據分開。
