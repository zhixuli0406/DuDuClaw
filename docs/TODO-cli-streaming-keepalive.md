# TODO: CLI Streaming Keepalive — 定時串流進度回報

> 解決 Claude CLI 長時間任務因 idle timeout 被 kill 的問題
> Created: 2026-03-29
> Status: Implemented (pending cargo build verification)

---

## 背景

現行架構中 `call_claude_cli()` 使用 idle timeout（原 120s）：若 stream-json 事件之間
沉默超過閾值，進程直接被 kill，返回 partial result。

**問題場景**：使用者要求「寫一篇安全性技術文章」→ Claude 讀完檔案後進入長時間思考/撰寫
→ stream-json 事件之間沉默 > 120s → 進程被 kill → 使用者只收到「分析完成！現在來寫文章」
→ 文章從未被寫出。

**根因**：`stream-json` 格式輸出完整事件（非 token-by-token delta），複雜 agentic 任務
（多輪 tool use、extended thinking）在事件之間可能長時間沉默。

---

## 設計：Keepalive + 進度串流

### 核心原則

```
❌ 沉默 > N 秒 → kill 進程 → 丟掉正在做的工作
✅ 沉默 > N 秒 → 發「仍在處理中…」給用戶 → 進程繼續跑
```

### 架構

```
┌─────────────────────────────────────────────────┐
│  call_claude_cli()                              │
│                                                 │
│  tokio::select! {                               │
│    line = reader.next_line() => {               │
│      parse stream-json event                    │
│      if tool_use → callback("⏳ 讀取 X...")     │
│      reset keepalive timer                      │
│    }                                            │
│    _ = keepalive_interval.tick() => {           │
│      callback("⏳ 仍在處理中...")               │
│      // 不 kill，繼續等                          │
│    }                                            │
│    _ = hard_max_timeout => {                    │
│      kill process  // 安全網：30 分鐘絕對上限    │
│      break                                      │
│    }                                            │
│  }                                              │
└─────────────────────────────────────────────────┘
```

### 參數設計

| 參數 | 預設值 | 說明 |
|---|---|---|
| `keepalive_interval` | 90s | 每隔 N 秒無新事件時發一次 keepalive |
| `hard_max_timeout` | 30min | 絕對上限，防止真正掛死的進程 |
| `progress_events` | `tool_use` | 要解析並回報進度的 stream-json 事件類型 |

### 進度訊息類型

1. **Keepalive**（無事件時定時發送）：`⏳ 仍在處理中…`
2. **Tool progress**（解析 stream-json `tool_use` 事件）：
   - `⏳ 正在讀取 docs/TODO-security-hooks.md...`
   - `⏳ 正在搜尋程式碼...`
   - `⏳ 正在撰寫檔案...`
3. **重複壓縮**：連續相同工具類型不重複發送

---

## 安全性分析

| 風險 | 嚴重度 | 緩解措施 |
|---|---|---|
| 進程掛死不退 | 中 | `hard_max_timeout = 30min` 絕對安全網 |
| 成本失控 | 中 | `--max-turns 50` 限制 turn 數；CostTelemetry 監控 |
| 進程堆積 | 低 | per-session semaphore（一個 session 同時只跑一個 CLI） |
| Keepalive 洩漏資訊 | 無 | 訊息發給發起請求的同一用戶 |
| 中間結果洩漏 | 低 | 只發工具名稱/檔名，不發推理內容 |

---

## 實作清單

### Phase 1: 核心重構（channel_reply.rs） ✅

- [x] P1-1: 定義 `ProgressCallback` 類型 — `Box<dyn Fn(ProgressEvent) + Send + Sync>`
- [x] P1-2: 定義 `ProgressEvent` enum — `Keepalive`, `ToolUse { tool, detail }`
- [x] P1-3: 重構 `call_claude_cli()` — idle timeout → `tokio::select!` keepalive loop
- [x] P1-4: 解析 `tool_use` stream-json 事件，提取工具名稱和檔案路徑
- [x] P1-5: 重複壓縮 — 連續相同工具類型不重複發送
- [x] P1-6: `build_reply_with_session` 傳入 channel-specific callback

### Phase 2: 同步 claude_runner.rs ✅

- [x] P2-1: `call_claude_streaming` 同步支援 keepalive（接受 `Option<&ProgressCallback>`）
- [x] P2-2: cron/dispatch 場景傳 `None`（無 channel 可回報）

### Phase 3: Channel 端適配 ✅

- [x] P3-1: Telegram — `send_message` keepalive（debounce 30s）
- [x] P3-2: LINE — `push_message` keepalive（debounce 60s，注意月訊息配額）
- [x] P3-3: Discord — `channel.send_message` keepalive（debounce 30s）
- [x] P3-4: 各 channel 的 keepalive 限流（debounce via `std::sync::Mutex<Instant>`）

### Phase 4: 移除舊 idle timeout ✅

- [x] P4-1: 移除 channel_reply.rs idle timeout（改用 hard_max_timeout 30min）
- [x] P4-2: 移除 claude_runner.rs idle timeout（改用 hard_max_timeout 30min）
- [x] P4-3: 保留 hard_max_timeout 作為安全網

---

## 相關檔案

- `crates/duduclaw-gateway/src/channel_reply.rs` — 主要的 `call_claude_cli()`
- `crates/duduclaw-gateway/src/claude_runner.rs` — cron/dispatch 用的 `call_claude_streaming()`
- `crates/duduclaw-gateway/src/telegram.rs` — Telegram channel handler
- `crates/duduclaw-gateway/src/line.rs` — LINE channel handler
- `crates/duduclaw-gateway/src/discord.rs` — Discord channel handler
