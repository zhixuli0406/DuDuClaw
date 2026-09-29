# 一次性 PTY 呼叫（以及被移除的連線池）

> 有些 CLI 不肯對著 pipe 講話。DuDuClaw 就給它一個真的終端機。

---

## 現在有什麼

每則 agent 回覆都是重新 spawn 一次 AI CLI：`claude -p "<prompt>"`、跑、讀答案、結束。這就是 `FreshSpawn` 路徑，而且是唯一的路徑。

大多數情況下，CLI 就是一個普通子行程。但有些 CLI 會檢查自己的輸出是不是接到真的終端機，不是就拒絕互動執行。對這些 CLI，DuDuClaw 會配置一個**偽終端**（`portable-pty`：Windows 10 1809+ 用 ConPTY，macOS 與 Linux 用 openpty），在裡面 spawn CLI，然後把 stdout 讀到行程結束為止。一次呼叫、一個行程、不留狀態。

```
gateway
   │
   ▼
invoke_oneshot(program, args, env, cwd, deadline, clear_env)
   │
   ├─ 配置 PTY（ConPTY / openpty）
   ├─ 在裡面 spawn CLI —— CLI 看到的是真 TTY
   ├─ 把 stdout 讀到 EOF（或由 deadline 殺掉子行程）
   ▼
擷取到的 stdout
```

整個功能就這樣。目前的使用者：Grok runtime（它的 CLI 堅持要 TTY），以及 `duduclaw` 自己的 CLI 登入輔助流程。

兩個行為值得知道：

- **`clear_env`** —— 開啟時，子行程從空環境開始，只看得到呼叫端明確列出的白名單（加上 `NO_COLOR` / `TERM`）。gateway 自己的廠商 API 金鑰就是這樣擋在被 spawn 的 agent CLI 之外的（credentials doctrine P3）。
- **`deadline`** —— 絕對的 wall-clock 上限。到期還沒結束的子行程會被殺掉，呼叫端收到 read-timeout 錯誤。

---

## 2026-09 移除：PTY session 連線池

到 v1.65 為止，本頁描述的是大得多的東西：一整池**長駐的互動式 `claude` REPL session**，每次回應用 in-band sentinel 框住，讓 runtime 知道答案從哪開始、到哪結束；逐 agent 的 `[runtime] pty_pool_enabled` 開關；一個行程外的 `duduclaw-cli-worker` 子行程，配 supervisor 與 SIGTERM→SIGKILL 的關機鏈；一個降級斷路器；一個 `GET /api/runtime/status` 端點；以及一整族 `pty_pool_*` Prometheus 計數器。大約 8,000 行。

全部移除。兩個理由：

**1. 它保的那個險從沒發生。** 連線池存在的理由是：萬一 Anthropic 封鎖 OAuth 訂閱帳號的 `claude -p`，翻一個旗標就能讓通道回覆繼續運作。Anthropic 確實把這個變更排在 2026-06-15——然後當天就暫停了。十五個月後 `claude -p` 對 OAuth 訂閱仍然可用，而這份保險的代價，是每一次跨過回覆路徑的重構都要維護它。

**2. 而且它本來就不能安全地打開。** 連線池的 session key 是 `(agent, cli_kind, bare_mode, account, model)`——**沒有對話維度**。一個 agent 服務兩個 WebChat 對話時共用同一個活 REPL，而那個 REPL 記得自己先前的回合，所以對話 B 看得到對話 A 的工作狀態。本頁自己就寫了這件事，標題是「啟用前請先讀這段」。一個沒人能負責任地打開的功能，不叫備援，叫掛著旗標的半成品。

另外還有一次實地事故：儀表板的 bug 在未經同意的情況下把 `pty_pool_enabled = true` 寫進 agent 設定，讓正式安裝跑上互動式路徑——而單一 OAuth 帳號在那條路徑上被搶用就會停滯。那需要一次性的開機遷移（`wp10-pty-default-reset`）來還原。該遷移也一併移除了：有這問題的安裝早就跑過它，而它修正的那些設定鍵現在根本不存在。

### 這對你有什麼影響

除非你當初明確開啟過，否則沒有影響。如果你的 `agent.toml` 還留著 `[runtime] pty_pool_enabled` / `worker_managed` / `pty_idle_timeout_secs` / `pty_interactive_timeout_secs`，這些鍵現在會被忽略——未知鍵一律容忍，所以不會壞掉；有空再刪即可。`DUDUCLAW_DISABLE_PTY_POOL` kill switch、`/api/runtime/status` 端點、`pty_pool_*` 與 `worker_*` 指標都已消失。`DUDUCLAW_PTY_DISABLE_RETRY` 對一次性路徑仍然有效。

### 如果 Anthropic 真的重啟拆分

那就重做一次——而且是刻意地做，從第一個 commit 就把對話維度放進 session 身分。設計筆記在 `commercial/docs/runtime-pty-pool-design.md`，被移除的實作留在 v1.65 tag 的 git 歷史裡。

---

## 總結

需要真終端機的 CLI，DuDuClaw 用偽終端驅動，一次呼叫一次 spawn。疊在上面那層長駐 REPL 連線池，保的是一個被暫停、從未恢復的政策變更，而且本身有跨對話洩漏的缺陷，安全性上打不開。要讓 8,000 行的備援保持誠實，成本比真有那天再重做還高。
