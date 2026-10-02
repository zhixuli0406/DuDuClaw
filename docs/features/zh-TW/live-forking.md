# Live Forking：同一個任務，並行跑幾條路，擇優收斂

> 本文講**使用情境**（什麼時候該用、什麼時候別用），面向對內/對外溝通。想看機制原理與比喻，讀技術特寫 [28-live-forking.md](./28-live-forking.md)；完整設計見 [RFC-26](../../rfc/RFC-26-deep-agents-alignment.md)。

Live Forking 讓一個正在執行的任務當場分裂成 N 條競爭分支，每條用不同的帳號、獨立的工作區副本、各自的預算跑，跑完由一位 AI judge（或你自己）挑出最好的一條合併回來。它預設關閉，per-agent 用 `agent.toml [fork] enabled = true` 開啟。

一句話講差異：平常一個任務走一條路，錯了重來；開了 fork，一個任務同時走幾條路，直接比出哪條好。

---

## 一條分支到底是什麼

DuDuClaw 的 agent 是以 `claude` CLI 子行程跑的，所以一條「分支」就是一次隔離的子行程執行，帶著：

- 自己的**工作區疊層**（copy-on-write：從經篩選的父工作區副本開始，變更寫進自己的副本）
- 自己的**帳號**（由 AccountRotator 分派不同帳號，分支之間不會撞到同一個 rate limit）
- 自己的**預算上限**與可選的**引導語**（steering：告訴這條分支往哪個方向試）
- 父工作區**在建立分支時的副本**，內容經 fork 複製規則篩選

跑完每條分支可選跑一個 `test_command`（例如 `pytest -q`），用 exit code 餵給 judge 評分。

---

## 採用保留的分支

尚未採用的 fork 會把已完成分支保留在 `<home>/fork_ws/<fork_id>/<branch_id>/`。預設保留 24 小時，可由 `[fork] retained_workspace_ttl_hours` 調整；使用 fork 工具時會清理過期副本。呼叫 `merge_or_select` 時需帶 `fork_id` 與 `branch_id`，MCP 工具只接受建立該 fork 的 agent 採用；Dashboard 的 manager 也能挑選。人工採用與保留副本清理共用跨行程鎖。執行時自動採用尚未在不同 fork 之間共用此鎖，因此使用相同父工作區的自動 fork 應依序執行。工具會先把實際檔案複製到紀錄中的父工作區，再標記採用成功並清除保留副本。副本遺失或過期、父工作區不存在、複製失敗都會回報錯誤。省略 `branch_id` 不會重跑評審；自動評審在執行分支時完成。

建立、保留與採用分支都使用同一套複製規則，排除 `.env`、`.env.*`、`*.pem`、`*.key`、SSH 私鑰名稱、`.npmrc`、`.pypirc` 與 `.netrc`。符號連結不會被跟隨：指向外部或目標不存在的連結會丟棄，Unix 上確認留在來源樹內的連結會重建。硬連結會變成獨立檔案，特殊檔案會丟棄；採用時會替換目的檔案，不會透過目的連結寫入。這是依名稱排除的規則，無法辨識所有可能的機密檔名。採用目標是 agent 目錄時，採用也絕不會把 agent 結構檔複製回去：`SOUL.md`、`CONTRACT.toml`、`agent.toml`、`.mcp.json`、`.claude/` 與其他 agent 結構檔維持上層原本的內容。分支仍然可以讀取它們。

`test_command` 會在分支副本內執行。逾時時，Unix 會終止該行程群組，Windows 使用 `taskkill /T /F`；輸出擷取有大小上限。Unix 子行程若自行脫離群組，便不在此清理邊界內，輸出讀取也有有限的收尾等待時間。

---

## 三種該用的情境

### 情境一：多方案並行，擇優

一個問題有好幾種合理解法，你不確定哪種好。與其一種一種試、試錯了再退回來，不如一次開三條分支各走一種策略，跑完比結果。

典型：「這段慢查詢，一條分支試加索引、一條試改寫 JOIN、一條試加快取，哪個實測最快用哪個。」每條分支拿到不同的 steering，judge 依測試通過率與品質評分，`merge_mode = "auto_with_fallback"` 會自動挑贏家但保留一個確認關卡。

### 情境二：高風險變更，先在分支裡試

要動的東西一旦錯了很麻煩（大規模重構、改動核心設定、跑破壞性遷移），你想先看它到底會改成什麼樣，再決定要不要落地。

分支的 copy-on-write 疊層天生適合這件事：分支的所有寫入落在自己的副本，父工作區在合併前一個字都不動。試爆了就丟掉那條疊層，父工作區完好如初；試對了才把贏家的寫入合併回來。這比「先 commit 再 revert」乾淨，因為失敗的那條根本沒碰到主線。

### 情境三：AI judge 評審

你要的是「幾個結果裡挑一個最好的，並且說得出為什麼挑它」，光「跑出結果」還不夠。

judge 產出一個 `JudgeVerdict`，信心分數按 RFC-26 的公式算：`quality_spread·0.4 + test_pass_ratio·0.4 + internal_consistency·0.2`。這正是會議 §15 問的那個情境：一個 agent 產出多個結果後，派另一個 agent 當評審。fail-closed 是刻意的：judge 不存在、判決 parse 不出來、sandbox 起不來，一律退回 `manual`（把所有分支攤給你看），絕不靜默亂挑一個。

---

## 何時不該用

Fork 不是預設就該開的東西，以下情況別用：

- **確定性的單一任務**：答案只有一個、沒有取捨空間（格式轉換、單純查詢），開分支只是白燒 N 倍成本。
- **成本敏感**：N 條分支 = N 個帳號、N 份預算。訂閱額度或 API 費用吃緊時，fork 會放大消耗。`[fork] max_branches` 與 `aggregate_budget_usd` 是硬上限，但根本問題是這個任務值不值得並行。
- **沒有可評分的依據**：`test_command` 為空時，judge 公式裡的 `test_pass_ratio` 會被中性化，評分只剩品質與一致性，擇優的客觀性打折。沒有測試能跑的任務，fork 的價值主要在情境二（隔離試錯），不在情境一（客觀擇優）。
- **需要跨行程強制中止某條分支**：`terminate_branch` 只能中止與它同一個行程的子行程。跨不同行程殺分支子行程是 RFC-26 明列的 by-design 排除項。

---

## 跟 `duduclaw eval` 的關係

兩者都會用到「AI judge」，但用途相反，別混淆：

| | Live Forking | `duduclaw eval` |
|---|---|---|
| 時機 | 執行期，任務正在跑 | 事後 / CI，離線回歸 |
| 目的 | 在幾條路裡當場挑一條最好的往下走 | 用固定的黃金任務驗 agent 有沒有退步 |
| 輸入 | 一個活的任務 + N 種策略 | `evals/<suite>/<case>.toml` 預錄案例 |
| 產出 | 合併贏家分支到工作區 | JSON 報告 + 非零 exit 擋 PR |

實作上 `duduclaw eval` 的 LLM judge 復用了 `duduclaw-fork` 的 judge 原語，所以評審邏輯是同一套。可以這樣記：**fork 是往前探路，eval 是回頭驗收**。eval 的用法見 [evals 指南](../../guides/evals.md)。

---

## 設定與工具

`agent.toml`：

```toml
[fork]
enabled = false              # 預設關,per-agent 開啟
max_branches = 4             # 分支數硬上限(避免帳號/額度爆掉)
default_budget_usd = 0.50    # 每條分支上限
aggregate_budget_usd = 1.50  # 所有分支合計上限
merge_mode = "auto_with_fallback"
test_command = ""            # 可選;空 ⇒ judge 的 test_pass_ratio 中性化
test_timeout_s = 120
retained_workspace_ttl_hours = 24          # 尚未採用的分支副本
```

MCP 工具（agent 開了 `[fork] enabled = true` 才註冊，統一 gated 在 `Scope::ForkExecute`，fail-closed）：

| 工具 | 做什麼 |
|---|---|
| `fork_run` | 把當前任務分成 N 條分支 |
| `inspect_branches` | 列出活著的分支 + 狀態 + 花費 |
| `diff_branches` | 兩條分支的檔案/輸出差異 |
| `merge_or_select` | 建立該 fork 的 agent 明確挑選並採用保留分支 |
| `terminate_branch` | 殺掉一條失控的分支 |
| `fork_cost` | 合計與各分支花費 |

每次 fork 收斂都寫進 `~/.duduclaw/fork_history.jsonl` 與 Activity Feed，dashboard 有 ForkPage 可視化。

---

## 一句總結

Live Forking 適合有取捨、有風險、需要比較的任務，不適合確定性的、成本敏感的、無從評分的任務。開之前先問一句：這個任務，並行幾條路換來的品質，值不值那幾倍的花費。
