# Prompt 預算強制

> 回覆路徑上的一條管線：估算 prompt，超過預算就依「最不失真 → 最激進」走三個階段，走不完就拒絕。

---

## 歷史說明

本頁原本描述的是「壓縮三刀流」：Meta-Token／LTSC（無損模式取代）、LLMLingua-2 橋接（有損 token 剪枝）、StreamingLLM（KV-cache 驅逐），各自掛一個 MCP 工具。

那套壓縮器**已於 v1.33 從 `duduclaw-inference` 移除**。它只能靠手動 MCP 工具觸及，回覆路徑上沒有任何一處會呼叫它，而且與下面這條管線功能重疊。它留下的兩個儀表板設定區段（`inference.toml` 的 `[llmlingua]`、`[streaming_llm]`，`inference.update` 仍照單全收）已於 2026-09 一併移除——那只是在寫沒人會讀的鍵。

現在存在的東西更窄，但一直在熱路徑上：`crates/duduclaw-gateway/src/prompt_compression.rs`。

---

## 它補上的洞

在這條管線之前，200K 價格懸崖是**事後**才被診斷出來的：`cost_telemetry::record` 拋一個 `cost_pressure` 事件，操作者收到警告，但下一個請求照樣原樣送出。預算強制把這個迴圈在請求邊界上閉合。

---

## 怎麼啟用

強制是 per-agent 的，而且**沒設就不啟用**：

```toml
[budget]
max_input_tokens = 150000       # 0 或缺鍵 ⇒ 不啟用強制
cache_guard_min_eff = 0.5       # 見下方「快取守衛」
cache_guard_max_overshoot = 0.15
```

`prompt_audit::read_max_input_tokens` 讀第一個鍵；檔案不存在、TOML 壞掉、鍵不存在，三者都回 0，也就是永遠不進管線。這刻意與快取守衛的慣例相反——守衛預設**開**。

---

## Token 估算

`estimate_tokens` 是 CJK 感知的啟發式，不是 tokenizer：**CJK 每個 codepoint 1.306 token，其餘每 3.6 字元 1 token**。這兩個常數來自 2026-08 的本機語料校準，取代了原本「每 token 1.5 字元」的統一猜測——後者在繁體中文負載上低估了約 22%。

`estimate_request_tokens` 把 system prompt、歷史與待送的使用者訊息加總。

---

## 三個階段

每個階段都是 `(system, history, user)` 上的純函式，要嘛回傳更小的版本，要嘛回 `None`（「我幫不上更多忙」）；呼叫端依序走，直到估算值符合預算。

**1. `turn_trim`** — 逐輪尾端裁剪。超過 800 字元的輪次保留前 300 與後 200 字元，中間放 `[trimmed N chars]` 標記；以字元層級切片，CJK 不會從 codepoint 中間斷開。處於成本壓力時門檻從 800 降到 200。短回覆什麼都不會失去。

**2. `drop_oldest_tool_echoes`** — 剝除舊的工具內容；若該歷史路徑沒有可靠的取回把柄，就把那段位元組標記為不可用。工具回聲是最便宜的損失：結果通常已經反映在它後面那一輪助理回覆裡。

**3. `bisect_and_summarize`** — 非同步階段。純函式階段都失敗後，gateway 只摘要**未受保護**的較舊輪次，再重新檢查預算。切分點由 `partition_turns_for_summary` 決定。

如果管線仍然塞不下，它**不會**默默送出超預算的 prompt：回傳 `BudgetExceeded` 並發出 `budget_exceeded` 事件。

### never-trim 區段

`split_never_trim_sections`／`is_never_trim_header`／`never_trim_tokens` 圈出任何階段都不得碰的 system prompt 區段——身分、工作狀態權威區塊、安全邊界。預算可以沒達成，這些區段不能為了達成預算而被悄悄丟掉。

---

## 快取守衛

在 prompt 快取健康時，壓縮並不免費。哪怕只改寫歷史的一小段尾巴，也會改變快取所依據的位元組，強迫整個 cache prefix 重建——arXiv:2607.12161 量到這個重建佔該情境開銷的約 87%。換句話說，省下的 token 可能比付出的還少。

`should_skip_for_cache` 是 `maybe_compress_history` 在**進入管線之前**就會查的決定性閘：

| 條件 | 預設 | 效果 |
|---|---|---|
| 近期快取效率 | > 50%（`cache_guard_min_eff`）| 且… |
| 預算超出幅度 | < 15%（`cache_guard_max_overshoot`）| …完全跳過壓縮 |

與 `max_input_tokens` 不同，這道守衛**預設開啟**——檔案不存在或缺鍵都退回論文的門檻值，只有顯式寫 `cache_guard_min_eff = 0` 才關掉。它是一項該預設保護 agent 的安全最佳化，不該要求逐 agent opt-in。

`CompressionInfo`（`compressed`／`compression_stages`）透過 task-local 從 `maybe_compress_history` 一路帶到好幾個 async frame 之後的 `cost_telemetry` 紀錄點。

---

## 刻意不做的事

寫出來，免得有人哪天又加回去：

- **LLMLingua-2 橋接**：Python 子行程啟動延遲讓它不適合逐請求的同步壓縮。真要回來，位置是非同步摘要器，不是熱路徑。
- **Meta-token／LTSC 取代**：它在 agent 端要付解碼時間，所以只能是顯式 opt-in 的旋鈕。延後，未排程。
- **KV-cache 驅逐（StreamingLLM）**：DuDuClaw 驅動的是 CLI 與廠商 API，它並不擁有模型的 KV-cache。

---

## token 實際上省在哪裡

這個領域最大的一次縮減其實不是壓縮。`[runtime] minimal_context`（預設開）收窄每次 CLI spawn 的 `--tools` 清單並帶上 `--setting-sources project,local`，實測固定開銷從每次 spawn 35,892 降到 10,974 token，約 69%。剩下最大的固定成本是 DuDuClaw 自己的 MCP 工具 schema。

---

## 與其他系統的互動

- **Session 記憶堆疊** — 壓縮摘要注入 system prompt，絕不當成對話輪次，見 [16-session-memory-stack.md](16-session-memory-stack.md)。
- **CostTelemetry** — 逐請求記錄 `compressed`／`compression_stages`；`cache_attribution_snapshot()` 回報是哪一個快取區塊一直在打斷 prefix。
- **Direct API** — 分層 `cache_control` 斷點以 `CACHE_SPLIT_MARKER` 切分 system prompt，那正是快取守衛的效率數字之所以有意義的原因。

---

## 總結

這一頁的誠實版本是：一條管線、三個階段、兩個設定鍵，外加一條顯式的拒絕路徑。它取代的三刀流，是三套聽起來很厲害、但回覆路徑上從來沒被呼叫過的策略。
