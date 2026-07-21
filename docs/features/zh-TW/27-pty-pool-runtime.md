# 跨平台 PTY Pool + Worker

> 當 Anthropic 封鎖 OAuth 帳號的 `claude -p` 時，我們不再寄信，而是改成保持一條電話線暢通。

---

## 比喻：寄信 vs. 開著的電話線

過去與 Claude CLI 對話的方式，就像**每問一個問題就寄一封信**。你寫下 `claude -p "你的提示"`，封好信封，寄出去，由一位全新的信差（一個全新的 process）送達。每封信都是一次完整、獨立的往返。很簡單——直到郵局不再投遞。

2026 年中，Anthropic 封鎖了 OAuth 訂閱帳號的 `claude -p`。信被退回了。

解法是**保持一條與同事連線的電話線，即時對話**。不再每問一題就寄一封信，而是撥號一次，線路維持連通，你在同一通電話裡逐一說出問題。那位同事就是真正的互動式 `claude` REPL——和人類手動操作的是同一支程式。

但電話通話有一個信件沒有的問題：你怎麼知道對方是**說完了**，還是只是停頓？你們約定一個**暗號**。「完畢。」當你聽到「完畢」，就知道這一輪結束、輪到你了。DuDuClaw 把這個暗號稱為 **sentinel**——模型在每個回答外層包上的標記，讓 runtime 精準知道一個回應從哪裡開始、到哪裡結束。不必猜測，也不必聽完整段對話歷史才能找到答案。

這就是 PTY Pool 的核心：一個真正的終端機（暢通的線路）、一套 sentinel 協定（暗號），以及一池預熱的 session（已在線上待命、隨時可講話的同事）。

---

## 現況（2026-07）：你現在真的需要它嗎？

**多數 agent 應維持關閉——它預設就是關的。**

Anthropic 原本排定 2026-06-15 的變更，會把程式化用量（`claude -p`、Agent SDK、GitHub Actions）拆到獨立的 Agent SDK credit，這將導致 OAuth 訂閱帳號的 channel reply 失效。**但該變更已於 2026-06-15 當天暫停。** 截至本文撰寫時，`claude -p` 對 OAuth 訂閱帳號照舊可用，因此預設的 fresh-spawn（`FreshSpawn`）路徑完全正常，PTY pool **並非必要**。

所以 PTY pool 被保留為**備援**：若 Anthropic 重新啟動程式化用量拆分，把 `pty_pool_enabled = true` 打開即可在無需改碼的情況下恢復 OAuth channel reply。在那之前，只有在你有明確理由、且已讀過下方限制時才開啟。

---

## 已知限制：Pool Session 不含對話維度

**開啟 `pty_pool_enabled` 前務必讀這段。** Pool 以 `(agent, cli_kind, bare_mode, account, model)` 為 key 管理長生命週期的 REPL session——**沒有對話維度**。同一個 agent 的 WebChat 對話 A 與對話 B 共用**同一條**活的 `claude` REPL，而該 REPL 記得它自己先前的輪次。結果就是**跨對話脈絡洩漏**：對話 B 會看到對話 A 啟動的 workflow 狀態（例如 B 問待辦卻拿到 A 的；兩個不同對話收到同一份週報）。

這**不影響**預設的 `FreshSpawn`（`claude -p`）路徑。Fresh-spawn 不帶任何 CLI 端 session 狀態——每一輪的脈絡完全來自 `SessionManager::get_messages(session_id)`，而 session id 是逐對話的（WebChat 組成 `webchat:<conn>#agent:<id>#conv:<nonce>`；每個外部通道以其 chat/thread id 為 key）。所以預設路徑正確隔離對話；只有 opt-in 的 PTY pool 會跨對話共用 REPL。

若你為單一對話工作負載開啟此池（每個 agent 一次只跑一個長任務、沒有並行的不同對話），這不成問題。但對多對話 agent（同時服務多位使用者／多個 thread 的 WebChat bot），在逐對話 pool key 落地前，**請勿開啟**。

---

## 為什麼用真正的 PTY，而非刮取 scrollback

以程式方式驅動互動式 REPL，有兩種天真的失敗模式：

1. **像一般 subprocess 那樣用 pipe** ——但 `claude` 會偵測到它沒有接上真正的終端機，於是拒絕進入互動模式。許多 CLI 都這樣。
2. **刮取 scrollback 畫面** ——擷取終端機印出的所有內容，試圖從雜訊（橫幅、轉圈動畫、ANSI 色碼、提示字元裝飾）中解析出答案。既脆弱又慢。

DuDuClaw 兩者皆不採用。它配置一個**真正的偽終端機（PTY）**，讓 `claude` 相信有人類正在打字，再使用**帶內（in-band）的 sentinel framing 協定**，使答案抵達時已預先界定好邊界——不刮取 scrollback，也不需要 sidecar process。

```
   天真做法                          PTY Pool 做法
   ────────                          ────────────
   claude（拒絕 pipe）               真正的 PTY（claude 看見 TTY）
        │                                  │
   刮取 scrollback                   read_until(sentinel)
        │                                  │
   從 ANSI 雜訊中正則擷取            payload 抵達時已預先 framing
        │                                  │
   ❌ 脆弱                            ✅ 決定性
```

PTY 後端是跨平台的——這正是讓單一程式碼路徑能橫跨 macOS、Linux 與 Windows 的關鍵：

| 平台 | PTY 後端 | 由誰提供 |
|------|----------|----------|
| Windows 10 (1809+) / 11 | ConPTY | `portable-pty` |
| macOS | openpty | `portable-pty` |
| Linux | openpty | `portable-pty` |

先前的前人之作（`dorkitude/maude` 透過 tmux、`runtorque/torque` 透過 Unix domain socket 的 PTY supervisor）都是**僅限 Unix**。`portable-pty` 正是讓單一 runtime 涵蓋這三種作業系統的那塊拼圖。

---

## Sentinel 協定

當 `PtySession` spawn 時，它會注入一段 `--append-system-prompt` 指令，教導模型遵守 sentinel 包裹協定：先印一行 sentinel、接著答案文字、再印一行完全相同的 sentinel——且閉合 sentinel 之後不再有任何內容。

```
Gateway                          PTY                         claude REPL
   │                              │                              │
   │  invoke("proxy 設定是         │                              │
   │          什麼？")            │                              │
   ├─────────────────────────────►  將提示寫入 PTY ────────────►│
   │                              │                              │ 思考中…
   │                              │  ◄─── <SENTINEL> ────────────┤
   │                              │  ◄─── 答案文字 ──────────────┤
   │                              │  ◄─── <SENTINEL> ────────────┤
   │  read_until(閉合             │                              │
   │            sentinel)        │                              │
   │  ◄── sentinel 配對之間的 ────┤                              │
   │      payload                │                              │
```

Runtime 持續讀取直到看見閉合 sentinel，然後擷取**sentinel 配對之間**的 payload。由於閉合 sentinel 就是 read-until 的探測標記，runtime 完全不必去解讀周圍的終端機裝飾——它只要切出被 framing 的答案即可。實作刻意取**最後一對** sentinel 出現位置，以應付終端機把開頭 sentinel 與 assistant 裝飾渲染在同一行的情況。

另有一條獨立的 **one-shot** 路徑（`oneshot_pty_invoke`），供不需要長壽 session 的情況使用。它一樣透過真正的 PTY 執行（讓 CLI 看見 TTY），但**不**注入 sentinel framing——它對應的是傳統單次呼叫的生命週期。

---

## RuntimeMode：兩條路徑，一個預設

每個 agent 的回覆都依其 `agent.toml` 選定的 `RuntimeMode` 路由。此功能**預設關閉**——你需逐 agent 主動開啟。

| RuntimeMode | 路徑 | 何時使用 |
|-------------|------|----------|
| `FreshSpawn` | 經由 `call_claude_cli_rotated` 的舊版 `tokio::process::Command` | 預設；只要 `agent.toml` 缺失、格式錯誤，或旗標未設定 |
| `PtyPool` | 本 crate 的池化、sentinel framing 的 PTY session | 僅當 `[runtime] pty_pool_enabled = true` |

`runtime_mode_for_agent()` 讀取 agent 目錄，並**失敗安全地回退到 `FreshSpawn`**——檔案缺失、解析錯誤或旗標未設定，全都預設走舊版路徑。Gateway 的公開介面為 `acquire_and_invoke` / `acquire_and_invoke_with`，會從池中取出一個 session、執行一次 sentinel 往返，再歸還。

```
agent X 的 channel reply
        │
        ▼
runtime_mode_for_agent(agent_dir)
        │
   ┌────┴─────────────────────────┐
   │                              │
FreshSpawn                     PtyPool
   │                              │
tokio::process::Command        acquire_and_invoke()
claude -p（舊版）              池化的 sentinel session
```

---

## OAuth vs. API-Key 路由

在 `PtyPool` 分支內，`channel_reply` 會依帳號類型分流——因為 `claude -p` 的封鎖只打到 OAuth 訂閱帳號：

| 帳號類型 | 路徑 | 原因 |
|----------|------|------|
| OAuth 訂閱 | 長壽互動式 REPL（sentinel framing） | 這類帳號的 `claude -p` 已被封鎖；REPL 是唯一路徑 |
| API key | `oneshot_pty_invoke` + `claude -p` | `-p` 對 API-key 認證仍可用；不需持有 session |

於是 OAuth 帳號取得那條暢通的電話線，API-key 帳號則繼續寄信——但兩者都是透過真正的 PTY。`claude_runner` dispatcher 套用相同的短路邏輯，使子 agent 派發與 channel reply 保持一致：當 `pty_pool_enabled = true` 時，兩者都跳過 local-offload 與 hybrid routing。

---

## Phase 7：受管 Worker

為了更強的隔離，這個池可以**移到 process 之外**，存在於獨立的 `duduclaw-cli-worker` 子 process 中，由 `[runtime] worker_managed = true` 控制。Gateway 的 `worker_supervisor` 掌管其生命週期——而最關鍵的是，把它的關閉動作排入 gateway 的優雅關閉流程：

```
Gateway 優雅關閉
        │
        ▼
flush 預測引擎
        │
        ▼
worker_supervisor: SIGTERM ──► duduclaw-cli-worker
        │  （等待）                  │ 排空 in-flight
        ▼                            │
worker_supervisor: SIGKILL ──► （若仍存活）
        │
        ▼
axum 排空 HTTP 連線
```

Worker 在預測引擎 flush **之後**、axum 排空**之前**被關閉——因此沒有工作遺失，也不會有殭屍 process 在 gateway 結束後存活。

---

## Fallback 鏈：可復原，而非致命

整個 runtime 最重要的性質：**每一條 PTY 路徑在出錯時都會回退到舊版的 `tokio::process::Command + claude -p`。** worker 缺失、池不健康或 spawn 失敗，皆為可復原——而非致命。

```
acquire_and_invoke()
     │
     ├─ 池健康？      ──否──► 回退到舊版 spawn ──┐
     │                                          │
     ├─ session spawn 成功？否─► 回退到舊版 spawn ──┤
     │                                          │
     ├─ sentinel 抵達？  否──► 回退到舊版 spawn ──┤
     │                                          │
     ▼ 是                                       ▼
   回傳 framing 過的 payload              claude -p 結果
```

這意味著開啟 `pty_pool_enabled` 絕不會讓 agent 比舊版路徑**更不可靠**。最差情況也只是悄悄退化成原本的樣子。

---

## Phase 8.5：Runtime 狀態端點

`runtime_status.rs` 提供 `GET /api/runtime/status`——一個**僅限 loopback** 的 JSON 端點（非 loopback 的對端會得到 403；loopback 邊界**本身**即是認證）。它回報即時的池計數器，以及全域 kill switch 是否啟用。

```
$ curl http://127.0.0.1:<port>/api/runtime/status
{
  "kill_switch_active": false,
  "pool": {
    "acquires_cache_hit_total": 412,
    "acquires_spawn_total": 9,
    "evicted_idle_total": 3,
    "evicted_unhealthy_total": 0,
    "invokes_ok_total": 421,
    "invokes_empty_total": 0
  }
}
```

---

## Phase 8：Prometheus 可觀測性

此 runtime 匯出一系列 `pty_pool_*` 計數器外加 worker 健康度量表，讓你能觀察快取效率與 failover 行為：

| 指標 | 意義 |
|------|------|
| `pty_pool_acquires_cache_hit_total` | 從池中重用的 session（熱） |
| `pty_pool_acquires_spawn_total` | 全新 spawn 的 session（冷） |
| `pty_pool_evicted_idle_total` / `_unhealthy_total` / `_shutdown_total` | 三種驅逐原因 |
| `pty_pool_invokes_ok_total` / `_empty_total` | invoke 結果 |
| `pty_pool_invoke_duration_*` | 往返時間 histogram |
| `worker_health_misses_total` / `worker_restarts_total` | 受管 worker 健康度 |
| `pty_pool_managed_worker_active` | 模式量表（worker 開/關） |

`cache_hit` 對 `spawn` 的高比值代表池發揮了作用——多數輪次都重用熱 session，而非付出冷啟動成本。

---

## 設定

一切皆逐 agent 設於 `agent.toml`，預設關閉：

```toml
[runtime]
pty_pool_enabled = true   # 開啟互動式 PTY pool（預設 false）
worker_managed   = true   # 在 process 外的 duduclaw-cli-worker 中執行池

# 互動 REPL 逾時（停滯偵測 + 硬上限），皆為選填
pty_idle_timeout_secs        = 120   # 連續無實質進度達此秒數即快速失敗（預設 120）
pty_interactive_timeout_secs = 1800  # 絕對牆鐘硬上限／安全網（預設 1800）
```

兩個 `pty_*` runtime 旗標皆未設定時，agent 完全照舊走 `FreshSpawn` 舊版路徑。僅設 `pty_pool_enabled` 會在 process 內執行池；再加上 `worker_managed` 則把池移入受監督的子 process。

**互動 REPL 逾時。** 一個 turn 會在兩者先到者觸發時失敗：**停滯偵測**（`pty_idle_timeout_secs`）——當 REPL 在 idle 視窗內沒有**實質進度**（token 計數上升或新的回覆文字；spinner 動畫與經過計時器刻意不算）；或**絕對硬上限**（`pty_interactive_timeout_secs`）。停滯偵測讓「長但仍在工作」的任務（多分鐘工具呼叫、agentic 工作）不再被誤殺，真正卡死的 session 則仍會快速失敗、降級到 fresh-spawn `claude -p`（並寫入 `channel_failures.jsonl`，附 `reason`＝`stall`／`hard_cap`／`boot` 與 `mid_task` 旗標）。環境變數覆寫：`DUDUCLAW_PTY_IDLE_TIMEOUT_SECS`、`DUDUCLAW_PTY_INTERACTIVE_TIMEOUT_SECS`。

---

## 為什麼這很重要

### 它解封了 OAuth 帳號

當 2026 年中 Anthropic 封鎖 OAuth 訂閱的 `claude -p` 時，所有由 Pro/Team/Max 帳號支撐的 channel reply 都會失敗。互動式 REPL 路徑以人類驅動 `claude` 的方式恢復了這些帳號——沒有繞過任何政策，只是讓程式以它預期被執行的方式執行。

### 一條程式碼路徑，三種作業系統

`portable-pty`（Windows 上的 ConPTY、Unix 上的 openpty）讓同一個 runtime 能在 macOS、Linux 與 Windows 上運作。它借鑑的前人之作僅限 Unix；這是跨平台的版本。

### 決定性的解析

sentinel 協定意味著 runtime 永遠不必猜測答案在哪裡結束。不刮取 scrollback、不依賴脆弱的 ANSI 正則——答案抵達時已被兩個標記預先 framing 好。

### 開啟很安全——但有一個但書

預設關閉、失敗安全回退到 `FreshSpawn`，且每個 PTY 錯誤都退化到舊版 `claude -p` 路徑。在**可靠性**這條軸上，開啟此池只可能與舊行為持平或更好。唯一的但書是**隔離性，而非可靠性**：pool session 的 key 不含對話維度，所以多對話 agent 會跨對話洩漏脈絡（見〈已知限制〉）。由於截至 2026-07 `claude -p` 仍可用（程式化用量拆分已於 2026-06-15 暫停），多數部署應維持關閉、留在完全隔離的 `FreshSpawn` 預設路徑。

### 可觀測

loopback 狀態端點與 `pty_pool_*` Prometheus 指標讓池的熱度、驅逐與 worker 健康度都可見，因此你能確認池確實省下了冷啟動。

---

## 總結

Anthropic 收走了「每問一題寄一封信」的能力。DuDuClaw 的回應是保持一條電話線暢通——一個讓 `claude` 以為有人類在鍵盤前的真正 PTY、一個讓 runtime 精準知道每個答案何時結束的 sentinel 暗號，以及一池熱 session 讓多數輪次跳過冷啟動。它以單一程式碼路徑橫跨 macOS、Linux 與 Windows，預設關閉，且每次失敗都靜靜回退到原本可行的方式。郵局改了規則；對話卻沒有中斷。
