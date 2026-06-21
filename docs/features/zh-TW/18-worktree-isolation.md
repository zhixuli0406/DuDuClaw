# Git Worktree L0 隔離

> 輕量級沙箱——每個任務一個檔案系統、原子化合併、無需容器。

---

## 比喻：餐廳的備料站（Mise en Place）

開始營業前，主廚會為每個工作站做好擺設——各自有自己的砧板、刀具與備好的食材。兩位廚師處理不同的料理時，不會去搶同一把紅蔥頭，也不會把洋蔥刮到對方的砧板上。

當一道菜完成，盤子送到出餐口，工作站隨即清理乾淨迎接下一張訂單。如果某位廚師中途搞砸了一道菜，他的工作站會被清空——但廚房其餘部分照常運作。

容器沙箱就像為每位廚師蓋一整間*獨立的廚房*——昂貴，對簡單備料而言過頭了。Git worktree 隔離只是給每位廚師在同一間廚房裡屬於自己的工作站。便宜、快速，而且在關鍵之處依然隔離。

---

## 為什麼用 Worktree，而不只是分支

多個並行 Agent 編輯*同一個* checkout 會互相覆蓋彼此的檔案。顯而易見的解法是給每個 Agent 自己的分支。但單一分支仍然只是一個工作目錄——切換分支會踐踏未提交的工作。

`git worktree` 透過給每個分支**自己的檔案系統目錄**來解決這個問題：

```
Main repo:        /Users/you/project/
                  ↓
Branch main:      /Users/you/project/                  ← always here
Worktree 1:       /Users/you/project-wt/agent-a-swift-fox/
Worktree 2:       /Users/you/project-wt/agent-b-calm-pine/
Worktree 3:       /Users/you/project-wt/agent-c-eager-leaf/
```

三個目錄共享同一個 `.git/` 物件庫（因此節省儲存空間），但各自擁有自己的工作檔案與 HEAD。三個 Agent 可以同時編輯而不衝突。

DuDuClaw 稱此為 **L0 隔離**——比 L1 容器沙箱便宜，比單純「祈禱 Agent 不要衝突」更強健。

---

## WorktreeManager

`WorktreeManager` 提供完整的生命週期：

```
Agent task starts
     |
     v
create(agent_id, task_id)
     |
     v
├─ Generate branch name: wt/{agent_id}/{adjective}-{noun}
├─ Check resource limits (max 5/agent, 20 total)
├─ git worktree add <path> <branch>
├─ copy_env_files(.env, config.local, ...)
└─ Return WorktreeHandle
     |
     v
[Agent executes — reads/writes ONLY in its worktree]
     |
     v
inspect() — did the execution succeed?
     |
     v
merge() or cleanup() based on AgentExitCode
```

### 友善的分支名稱

分支名稱來自一個 50×50 的詞對產生器——形容詞 + 名詞：

```
wt/duduclaw-pm/swift-fox
wt/xianwen-coder/calm-pine
wt/agnes/bright-hawk
wt/sam/crisp-river
```

這些詞經過精選，務求簡短、好記且不冒犯。在 2,500 種組合 × Agent 範圍之下，碰撞極為罕見，清理也很簡單。

### 資源上限

```
MAX_WORKTREES_PER_AGENT: 5
MAX_TOTAL_WORKTREES:     20
```

如果某個 Agent 達到上限，建立新 worktree 會快速失敗，而不是讓 worktree 默默累積、耗盡磁碟空間。

---

## Snap 工作流程

Worktree 不只用於*建立*空間——它們也定義了一套**受控的合併協定**。受 agent-worktree 專案啟發，DuDuClaw 採用四階段工作流程：

```
┌─────────────────────────────────────────────────────┐
│  1. CREATE                                          │
│     └─ worktree + branch + env file copy            │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  2. EXECUTE                                         │
│     └─ Agent does work inside the worktree          │
│        (container sandbox nested here if needed)    │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  3. INSPECT                                         │
│     └─ Pure decision function (testable)            │
│        AgentExitCode + diff + test results          │
│        → Decide: merge / cleanup / keep-alive       │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  4. MERGE or CLEANUP                                │
│     └─ Atomic merge with dry-run pre-check          │
│        or remove worktree + branch                  │
└─────────────────────────────────────────────────────┘
```

純函數的決策邏輯（階段 3）與 I/O 分離，因此可以用 fixture 進行單元測試——測試中不需要任何 git 操作。

### 結構化的退出碼

`AgentExitCode` 是一個型別化的列舉，讓呼叫方不必檢視數字碼：

```
AgentExitCode {
  Success,       → auto-merge (if configured)
  Error,         → cleanup, don't merge
  Retry,         → keep worktree, re-run
  KeepAlive,     → leave for manual inspection
}
```

---

## 帶預檢的原子化合併

合併步驟是最危險的——兩個並行 Agent 同時嘗試合併進 `main` 會破壞 git 狀態。DuDuClaw 以**全域合併互斥鎖**與**dry-run 預檢**解決這個問題：

```
merge(worktree_handle):
     |
     v
acquire global_merge_lock()        ← serializes all merges
     |
     v
git merge --no-commit --no-ff <branch>   ← dry-run
     |
     v
if conflicts or errors:
    git merge --abort
    return MergeResult::Conflicts(info)
     |
     v
git merge --abort                  ← still dry-run, rollback
git merge <branch>                 ← now commit for real
     |
     v
release global_merge_lock()
```

`Mutex<()>` 存放在 `OnceLock<Mutex<()>>` 中，因此跨所有執行緒的**所有** `WorktreeManager` 實例共享同一把鎖。少了它，來自不同 async 任務的 `dispatch_in_worktree` 呼叫各自會建立自己的 manager 並產生競態。

---

## copy_env_files：安全地複製機密

建立 worktree 時，主 repo 中的某些檔案需要隨之帶過去——通常是 `.env`、`.env.local` 或設定檔。但天真的複製會開啟三個攻擊面：

1. **路徑穿越**——`.env/../../etc/passwd`
2. **符號連結**——`.env → /etc/shadow`
3. **超大檔案**——重達 500MB 的 `.env`

`copy_env_files` 對這三者都設下圍欄：

```
for each file in allowlist:
     |
     v
canonical_path = fs::canonicalize(main_repo_path + file)
     |
     v
if !canonical_path.starts_with(main_repo_path):
    reject (path traversal)
     |
     v
if fs::symlink_metadata(canonical_path).is_symlink():
    reject (symlink attack)
     |
     v
if fs::metadata(canonical_path).len() > 1 MB:
    reject (oversize)
     |
     v
copy to worktree_path/file
```

一旦在此出錯，代價可能是把主機憑證暴露給 Agent 的沙箱——因此圍欄設得很嚴格。

---

## Agent ID 的清洗

Agent ID 來自使用者輸入（`agent create my_agent!!!`）。分支名稱有嚴格的字元集規則。在組成分支名稱前：

```
sanitize("My_Agent!!!")
     |
     v
lowercase     → "my_agent!!!"
     |
     v
replace [^a-z0-9-] with '-'
              → "my-agent---"
     |
     v
collapse multiple -
              → "my-agent-"
     |
     v
strip leading/trailing -
              → "my-agent"
```

這可防止格式錯誤的分支名稱破壞 git。

---

## Worktree 在隔離層級中的定位

DuDuClaw 有多個隔離層——使用足夠的當中最便宜的那個：

| 層級 | 機制 | 成本 | 隔離對象 |
|-------|-----------|------|----------|
| **L0 Worktree** | `git worktree` | 極低 | 並行 Agent 之間的工作檔案 |
| **L1 Container** | Docker / Apple Container / WSL2 | 中等 | 檔案系統 + 網路 + 行程樹 |
| **L2 Capability Deny** | `agent.toml [capabilities]` | 無（策略） | 工具存取（bash / browser / computer use） |
| **L3 Hooks** | Claude Code PreToolUse hooks | 低 | 執行期指令層級的封鎖 |

純粹的「重構這個函式」任務只需要 L0——Agent 編輯檔案；沒有網路呼叫。「安裝依賴並執行測試」任務則需要 L0 + L1——檔案系統隔離*再加上*容器網路邊界。

---

## 設定

在 `agent.toml` 中以每個 Agent 為單位設定：

```toml
[container]
worktree_enabled       = true
worktree_auto_merge    = true   # auto-merge on Success exit code
worktree_cleanup_on_exit = true # remove on Error/Success
worktree_copy_files    = [".env", ".env.local", "config.local.json"]
```

若 `worktree_enabled = false`，任務會在主 checkout 目錄中執行（歷史行為，對單一 Agent 部署可接受）。

---

## 可觀測性

`WorktreeManager` 在每個生命週期階段發出 tracing span：

```
INFO  worktree::create{agent_id=dudu branch=wt/dudu/swift-fox}
DEBUG worktree::copy_env_files{files=[.env, .env.local]}
INFO  worktree::execute_start{task_id=t_abc123}
INFO  worktree::execute_end{exit_code=Success duration=12.3s}
INFO  worktree::merge{dry_run=true conflicts=0}
INFO  worktree::merge{committed=true hash=abc1234}
INFO  worktree::cleanup{removed_branch=true removed_dir=true}
```

這些訊息經由 BroadcastLayer → WebSocket → Dashboard Logs 頁面流動，因此你可以即時觀看 worktree 的啟動與合併。

---

## 為什麼這很重要

### 無需容器的並行

過去要同時執行 5 個 Agent，意味著只能二選一：
- 一個主 checkout，Agent 在檔案上互相衝突（快速、但會壞掉），或
- 五個容器沙箱（隔離良好，但啟動成本高）。

Worktree 讓你以幾乎等同於前者的成本，得到後者的隔離。

### 原子化、可稽核的合併

dry-run 預檢意味著壞掉的合併永遠不會半套套用。合併要嘛乾淨成功，要嘛完全中止。沒有混合狀態。沒有「有些檔案合併了、有些沒有」。

### 與其他層可組合

一個 Agent 可以在容器沙箱*內部*的 worktree 中執行——worktree 提供檔案系統隔離，容器提供網路／行程隔離。它們可以堆疊。

### 對人類友善的分支名稱

`wt/dudu-pm/swift-fox` 好記，也容易在日誌中 grep。勝過 `wt-3d8b2a...` 這類晦澀的雜湊。

---

## 總結

五個 Agent 同時在同一個程式碼庫上工作，需要*某種*隔離。容器對大多數任務而言太過頭。原始分支則不夠用。Git worktree 命中了甜蜜點——便宜、並行，並帶有一套不會破壞主 repo 的原子化合併協定。它是每個 DuDuClaw Agent 任務的預設隔離層。
