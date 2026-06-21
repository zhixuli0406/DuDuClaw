# Live Run Forking（即時執行分叉）

> 將一個進行中的執行拆成 N 個競爭分支，讓 AI 評審保留最佳者，只把贏家合併回去。

---

## 比喻：在迷宮的每個岔路複製自己

想像你站在迷宮的一個岔路口。你不知道哪條路通往出口。與其挑一條路然後祈禱，不如**複製自己**——一個分身走左邊的隧道、一個走右邊、一個直走。每個分身獨立探索，用自己的雙腿、在自己的走廊裡，互不干擾。

當分身們回來時，**入口處的評審**會詢問每一個發現了什麼：你到達出口了嗎？路線多乾淨？你有把握嗎？評審保留那個真正找到出路的分身，捨棄其餘的——彷彿那些死路從未發生過。

DuDuClaw 的 Live Run Forking 就是把這個概念套用到 agent 執行上。一項任務被拆成多個平行**分支（branch）**，每個分支在自己隔離的工作區裡執行，擁有自己的帳號和自己的預算。當它們完成後，**AI 評審**為它們評分，贏家的成果會被合併回 parent——落敗的分支則被丟棄。

這個模式的靈感來自 [`pydantic-deepagents`](https://github.com/vstorm-co/pydantic-deepagents)，它的招牌能力正是如此：將進行中的執行分叉成競爭分支，讓評審合併贏家。RFC-26 把這個概念對映到 DuDuClaw 既有的 `AccountRotator`、容器沙箱與 GVU 評審原語上。

---

## 為什麼是分叉，而非單純重試

DuDuClaw 的 GVU 自我對弈迴圈已經能改善一個答案——但它是**循序的**：生成、驗證、更新、重複。那是單一條推理線，一步一步精煉自己。

分叉則不同。它**真正地平行執行獨立的嘗試**，每個都可能被導向不同的策略：

```
循序（GVU）：    attempt → critique → attempt' → critique → attempt''
                （單一條推理線，隨時間精煉）

平行（Fork）：   ┌─ branch A：「用狀態機重構」
       fork_run ─┼─ branch B：「用 early return 重構」
                 └─ branch C：「用查表重構」
                 （三個獨立嘗試，最後由評審裁定）
```

當一項任務有多種看似合理的做法、而你又負擔得起同時嘗試數種時，分叉以廣度而非深度探索解空間。

---

## 流程

```
                      fork_run(prompt, n, strategies[], budget, merge_mode)
                                   |
                                   v
        ┌──────────────────────────────────────────────────────────┐
        │  每個分支取得：                                            │
        │   • copy-on-write 工作區 overlay（read-through parent）    │
        │   • 來自 AccountRotator 的獨立帳號                         │
        │   • 自己的 per-branch budget_usd 上限                      │
        │   • 選用的 steering 訊息                                   │
        └──────────────────────────────────────────────────────────┘
                                   |
              ┌────────────────────┼────────────────────┐
              v                    v                    v
        ┌──────────┐         ┌──────────┐         ┌──────────┐
        │ branch A │         │ branch B │         │ branch C │   ← tokio::spawn，
        │ (overlay)│         │ (overlay)│         │ (overlay)│     平行執行
        └────┬─────┘         └────┬─────┘         └────┬─────┘
             │  （每個分支選用性執行 test_command）    │
             └────────────────────┼────────────────────┘
                                   v
                            ┌─────────────┐
                            │  AI  評審   │   confidence =
                            │ JudgeAgent  │     quality_spread·0.4
                            └──────┬──────┘   + test_pass_ratio·0.4
                                   │          + internal_consistency·0.2
                                   v
                            ┌─────────────┐
                            │  MergeMode  │   Auto / AutoWithFallback /
                            │   resolve   │   Vote / Manual
                            └──────┬──────┘
                                   v
                  winner.overlay.promote() → parent 工作區
                  （落敗者丟棄；其餘一切不變）
```

一切都是**預設關閉**。除非 agent 在 `agent.toml` 設定 `[fork] enabled = true`，否則不會發生任何分叉。

---

## 跨行程的真相來源：ForkStore

分叉是在 MCP-server 行程裡**執行**的（`claude` 子行程在那裡被 spawn），但**負責提供 `/metrics` 與 dashboard 的 `fork.list / inspect / resolve` RPC** 的是 gateway。行程內的 registry 無法跨越這兩個行程。

因此分支與分叉的狀態存放在一個共用的 WAL SQLite 資料庫——`ForkStore`，位於 `<home>/fork_store.db`——讓兩個行程都能開啟：

```
        ┌────────────────────────┐        ┌────────────────────────┐
        │   MCP-server 行程        │        │      gateway 行程        │
        │  （spawn 分支執行）       │        │  （/metrics + dashboard）│
        │                         │        │                         │
        │  mcp_fork /             │        │  render_fork_metrics_from│
        │  mcp_fork_exec          │        │  handle_fork_list/...    │
        └───────────┬────────────┘        └───────────┬─────────────┘
                    │  寫入                            │  讀取
                    │            ┌──────────────┐      │
                    └───────────▶│  ForkStore   │◀─────┘
                                 │  (WAL SQLite │
                                 │  fork_store. │
                                 │     db)      │
                                 └──────────────┘
                       WAL + busy_timeout ⇒ 並行讀寫安全
```

`mcp_fork` + `mcp_fork_exec` 處理器已從舊的行程內 registry **重構到** `ForkStore` 上。它有兩張表——`forks` 與 `fork_branches`——並提供 `insert_fork`、`update_branch`、`set_resolution`、`set_all_branch_states`、`get_fork`、`list_branches`、`list_forks` 與 `metrics`。它沿用 `SqliteMemoryEngine` 相同的模式（WAL + busy timeout），讓並行讀寫存取安全。

---

## 分支生命週期狀態

分支透過一個型別化的 `BranchState` enum 流轉（呼叫端不必猜測數字代碼）：

| 狀態 | 意義 | 可評審？ |
|------|------|---------|
| `Pending` | 已建立，尚未開始 | 否 |
| `Running` | 子行程執行中 | 否 |
| `Finished` | 乾淨完成——符合評審資格 | **是** |
| `BudgetKilled` | 因超過 per-branch 或 aggregate 預算上限而被終止 | 否 |
| `Terminated` | 透過 `terminate_branch` 外部終止 | 否 |
| `Failed` | 無帳號、spawn 失敗或 executor 錯誤 | 否 |

只有 `Finished` 分支 `is_judgeable()`。若**零**個分支可評審，評審會回傳錯誤，呼叫端將該分叉交由 operator 處理——絕不從無中自動挑選（fail-closed）。

---

## 預算執行

`budget.rs` 裡有兩層預算保護，皆 fail-closed：

**`Pool`——完成後計帳。** 每個分支以 per-branch 上限 `register`；`try_charge` 同時執行 per-branch 上限與 aggregate 上限。任何拒絕（`BranchExceeded` / `AggregateExceeded`）都不會提交——呼叫端必須停止該分支。未註冊的分支上限為零，因此任何正向扣費都會被拒絕。

```
Pool::try_charge(branch, amount)：
     branch_spent + amount > branch_cap   ⇒ BranchExceeded   （不提交）
     aggregate_spent + amount > agg_cap    ⇒ AggregateExceeded （不提交）
     否則                                  ⇒ Allowed          （提交兩個計數器）
```

**`LiveAggregate`——串流時刻的搶先終止。** 當分支串流 stream-json 時，它們**正在累積中**的 `total_cost_usd` 會被即時監看。當進行中的合計花費一跨越 aggregate 上限，`observe` 就會點名**單一個最昂貴的進行中分支**（以 id 做確定性平手判斷）讓呼叫端在串流中途終止它——犧牲最少的分支，而非等每個分支各自撞到上限。NaN／負值花費會被淨化為 0.0；`finish` 在某分支結束後把它的預算釋放給倖存者。

沒有靜默上限：當分支數被縮減到可用帳號數量時，這個縮減會被記錄下來。

---

## 評審

`JudgeAgent` 產生一個 `JudgeVerdict { winner, confidence, per_branch_scores, rationale }`。confidence 遵循 `JudgeScores` 中的 deep-agents 公式：

```
confidence = quality_spread       · 0.4
           + test_pass_ratio      · 0.4
           + internal_consistency · 0.2
```

每個子分數都被夾限到 `[0,1]`（超範圍 ⇒ 夾限 + 警告；NaN ⇒ 0.0）。提供兩種實作：

- **`LlmJudge<C: LlmCaller>`**——建構一個多候選、XML 分隔的 prompt（`build_judge_prompt`），對候選標籤做跳脫以抵抗注入；以 JSON 優先（含 fence 剝除）解析回應。後端是注入式的，因此 gateway 可以接上 Confidence Router（local 優先，信心低時升級到 Claude）。無法解析的裁定或越界索引會回傳錯誤（fail-closed）。
- **`HeuristicJudge`**——確定性、零 LLM 的後備評審，在沒有 LLM 可用時使用。

`test_pass_ratio` 由各分支的 `test_exit_code` 在可評審分支間計算而得，當沒有分支被測試時取中性值（0.5）。

---

## 合併模式

`merge::resolve` 把一個裁定轉成 `MergeDecision { winner, needs_confirmation, reason }`：

| 模式 | 行為 |
|------|------|
| `Auto` | 挑選裁定贏家、立即 promote、無人介入 |
| `AutoWithFallback` | **預設**——挑選贏家但在 promote 前先浮出確認 |
| `Vote` | 取樣評審 `VOTE_ROUNDS`（3）次取多數；平手或平均信心過低 ⇒ 延後 |
| `Manual` | 永遠交由 operator（`winner = None`） |

低於 `DEFAULT_CONFIDENCE_THRESHOLD` 的贏家**無論何種模式**都會被延後。贏家的 overlay **只有**在決策為最終且不需確認時才會被 `promote()` 進 parent 工作區——否則 parent 維持原狀，直到 operator 透過 `merge_or_select` 解決。

---

## Copy-on-Write Overlay

每個分支在一個 `BranchOverlay` 裡工作，它 read-through parent 工作區，但把寫入保留在本地直到 promote：

- **`Snapshot`**——可攜的 MVP 後端：對 parent 做遞迴 `copy_tree`。
- **`NativeCow`**——macOS/APFS 上透過 `cp -c` 走 `clonefile(2)`，Linux btrfs/XFS 上走 `cp --reflink=always`。

`detect_backend()` 對主機探測一次（快取）並在原生 clone 失敗時退回 `Snapshot`——**隔離永不被妥協，只犧牲速度/空間最佳化**。落敗的 overlay 被丟棄；只有贏家的寫入被合併過去。

---

## MCP 工具

全部六個工具都受 `Scope::ForkExecute` 把關（明確列舉；任何未知工具預設需要 Admin scope——既有的 fail-closed 規則），且只有在 agent 設定 `[fork] enabled = true` 時才可用：

| 工具 | 用途 |
|------|------|
| `fork_run` | 將當前任務拆成 N 個分支 |
| `inspect_branches` | 列出存活分支 + 狀態 + 花費 |
| `diff_branches` | 顯示兩個分支間的檔案/輸出 diff（每側 `truncate_bytes`） |
| `merge_or_select` | 解決一個分叉——評審裁定或明確挑選 |
| `terminate_branch` | 終止失控分支（取消尚未開始的；對進行中的子行程在串流中途 SIGKILL） |
| `fork_cost` | 合計 + per-branch 花費 |

`fork_run`、`merge_or_select` 與 `terminate_branch` 被標記為 `is_state_changing` 以留下稽核軌跡。

---

## 設定

於 `agent.toml` 中按 agent 設定：

```toml
[fork]
enabled              = false              # 預設關閉
max_branches         = 4                  # 硬上限（避免帳號/額度爆掉）
default_budget_usd   = 0.50               # per-branch 上限
aggregate_budget_usd = 1.50               # 跨所有分支
merge_mode           = "auto_with_fallback"
test_command         = ""                 # 選用；空 ⇒ test_pass_ratio 中性化
test_timeout_s       = 120
```

缺漏或格式錯誤的 `[fork]` 區塊是停用的 fail-safe——不 panic。無效的子值（如 `max_branches = 0`、負預算）退回預設；未知的 `merge_mode` 字串退回 `AutoWithFallback` 並附警告。

---

## 可觀測性

gateway 的 `/metrics` 端點在 scrape 時讀取跨行程的 `ForkStore`，並輸出 Prometheus 行：

| 指標 | 型別 | 意義 |
|------|------|------|
| `duduclaw_fork_runs_total` | counter | 已建立的分叉總數 |
| `duduclaw_fork_resolved_total` | counter | 已解決出贏家的分叉 |
| `duduclaw_fork_promoted_total` | counter | 贏家已被 promote 的分叉 |
| `duduclaw_fork_branches_total` | counter | 跨所有分叉的分支總數 |
| `duduclaw_fork_branch_outcome{outcome="finished\|budget_killed\|failed"}` | counter | 依終端結果分類的分支 |
| `duduclaw_fork_spend_usd_total` | counter | 跨所有分叉的合計美元花費 |

每次分叉解決也會透過 `with_file_lock`（跨行程安全）附加到 `<home>/fork_history.jsonl`，並在 gateway 的 `activity` 表插入一筆 `fork_resolved` 列，使其出現在 dashboard 的 Activity Feed。dashboard 的 `ForkPage` 列出近期分叉、並排顯示分支、標出評審贏家，並提供手動解決（`fork.resolve` 受 manager 檢查保護）。

---

## 已實作 vs 規劃中

依 RFC-26 §5，全部六個階段（P1–P6）皆已落地：

| 階段 | 範圍 | 狀態 |
|------|------|------|
| P1 | `duduclaw-fork` crate：`Branch`、`BranchOverlay`、`budget::Pool`、controller | 完成 |
| P2 | `JudgeAgent` + `JudgeVerdict`、test runner、merge modes | 完成 |
| P3 | 6 個 MCP 工具 + `Scope::ForkExecute` + dispatch 接線 | 完成 |
| P4 | 平行執行、aggregate 預算池、native CoW、串流預算終止、外部 SIGKILL | 完成 |
| P5 | 透過共用 `ForkStore` 的 Prometheus `/metrics` + `fork_history.jsonl` + Activity Feed + dashboard `ForkPage` | 完成 |
| P6 | 次要對等項（Plan Mode、checkpoint fork/rewind + SQLite、built-in skills、memory `/improve`、Task Board claim + 循環偵測） | 完成 |

**唯一的設計性排除**：`terminate_branch` 只能終止由其所屬行程擁有的子行程。跨*不同*行程終止分支子行程（例如 gateway 伸手進 MCP-server 的子行程）不在範圍內——終止必須源自 spawn 該子行程之處。

---

## 為什麼這很重要

### 廣度優先的問題解決

有些任務有數種看似合理的策略、且事前看不出哪個最好。分叉同時嘗試它們，讓證據（測試 + 評審）來裁定——而非押注一種做法、一小時後才發現它是錯的。

### 不會撞到速率限制

因為每個分支從 `AccountRotator` 取得**獨立帳號**，N 個平行執行不會爭搶單一帳號的速率限制。分支數會被縮減到可用帳號數量，使獨立性永遠成立。

### 成本維持有界

兩層預算——per-branch 與即時 aggregate——意味著失控的分支會在串流中途被終止，而非燒光整筆預算才停。沒有任何東西被靜默上限；縮減會被記錄。

### 全程 Fail-Closed

缺少評審、無法解析的裁定、沙箱 spawn 失敗、或零個可評審分支，全都交由 operator——絕不靜默自動挑選。信心低於門檻也會延後。預設關閉的姿態意味著在明確啟用前這一切都不會運作。

---

## 總結

當你迷失在迷宮裡、又負擔得起複製自己時，你不會挑一條隧道然後祈禱——你會送一個分身走每一條，讓入口處的評審只保留那個找到出口的分身。Live Run Forking 賦予 DuDuClaw agent 這項能力：平行競爭分支、隔離工作區、獨立帳號、有界預算、AI 評審，以及一個跨行程的 `ForkStore`，讓 gateway 與 dashboard 都能看著它發生。預設關閉、fail-closed、無靜默上限——以廣度探索，只合併贏家。
