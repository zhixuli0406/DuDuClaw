# 演化開關總覽：每個開關控制什麼

DuDuClaw 的 agent 能隨時間自我改進：反思預測誤差、改寫自己的 `SOUL.md`、合成新技能、探索利用不足的領域。這些路徑全部是選擇性加入（opt-in），且各自獨立可切換。本指南是唯一一份地圖，說明哪個開關管哪件事，以及如何完全凍結一個 agent。

## 總開關

`agent.toml`：

```toml
[evolution]
enabled = true   # master kill-switch (default: true)
```

`enabled = false` 會讓**該 agent 上所有自主演化路徑失效**，不管下面個別開關怎麼設。想讓一個 agent 停止自我改變時，這是唯一要切換的開關。預設為 `true`，所以在這個欄位出現之前建立的 agent 會維持原本的行為。

具體來說，當 `enabled = false` 時：

| 路徑 | 會停止什麼 |
|---|---|
| GVU 自我對弈迴圈 | 不再產生 `SOUL.md` 提案，不再開啟觀察期 |
| Heartbeat 沉默破窗機制 | **不會**在沉默後觸發強制反思 |
| 通道預測路徑 | 技能診斷／啟用／合成／畢業與 GVU 觸發全部跳過 |
| 子 agent 派工反思 | `maybe_run_gvu` 直接短路返回 |
| 技能合成自動排程器 | 即使全域開啟，仍會跳過已凍結的目標 agent |

預測誤差**記錄**仍會繼續執行，那是被動觀測（telemetry），不是自我修改，所以儀表板上的數字仍然準確。

## 各功能獨立開關

在總開關之下，每項能力都有自己的旗標。只要下列任一項開著，`is_any_evolution_enabled()` 就會是 true：

| 開關 | 預設值 | 控制什麼 |
|---|---|---|
| `gvu_enabled` | `false` | GVU generator→verifier→updater 迴圈（改寫 SOUL.md） |
| `skill_synthesis_enabled` | `false` | 從重複出現的領域缺口合成新技能 |
| `skill_graduation_enabled` | `false` | 把驗證過的技能升格到全域範圍 |
| `skill_recommendation_enabled` | `false` | 為新 agent 自動啟用推薦技能 |
| `curiosity_enabled` | `false` | 主動探索利用不足的領域 |
| `skill_auto_activate` | `false` | 在對話中途啟用建議的技能 |
| `skill_behavior_monitor_enabled` | `false` | 啟用後的行為偏移偵測 |

**`gvu_enabled` 預設為 `false`（fail-closed opt-in，2026-08-06 變更，詳見 `TODO-evolution-v3-2026-08.md` WP0.1）。** 每一份寫入 `agent.toml` 的 scaffold／模板都會明確寫出這個鍵，即使值是 `false`，這樣開關永遠可見，不會變成一個「不存在的鍵默默代表關閉」的狀態。要讓某個 agent 選擇加入，設定 `gvu_enabled = true`。

### GVU 冷卻時間

跟上面那個開關無關，每一條 GVU 觸發路徑（通道回覆的 ε-exploration、沉默計時器、子 agent 派工的強制反思）都共用同一個 per-agent 冷卻時間，避免一波觸發連鎖疊出好幾個耗時數分鐘的 GVU 循環：

```toml
[evolution]
gvu_cooldown_minutes = 60   # default 60; 0 disables the cooldown
```

冷卻時間從觸發被放行的那一刻開始計算（不是循環結束時），而且不論結果為何都會套用（applied／abandoned／deferred／timed_out／skipped），因為要節流的成本是*嘗試*的 LLM 呼叫次數，不只是成功的呼叫。狀態存在記憶體中，gateway 重啟就會重置。

### 實際跑的是哪個引擎：AEE（預設）還是舊版 SOUL 路徑

當 `gvu_enabled = true` 時，實際運作的演化引擎是 **AEE**（Agentic Evolution Engine）。AEE 演化的是 agent 的 *playbook*：一條條小型、可獨立退場的行為規則，每一條都連結至少一個 eval case，而且從不改寫 `SOUL.md`。人格檔案的所有權屬於操作者。

改寫 `SOUL.md` 的舊版 Generator→Verifier→Updater 循環仍保留作為逃生艙：

```toml
[evolution]
legacy_soul_evolution = true   # default false → AEE
```

`agent.toml` 缺漏或格式錯誤時會得到 `false`（走 AEE），這是刻意跟本頁其他鍵相反的 fail-safe 方向，因為 AEE 這條路徑本來就*不可能*寫入 `SOUL.md`，設定檔打錯字絕不能因此悄悄重新打開那個寫入面。

有兩件事是兩個引擎共用的：上面提到的冷卻時間，以及 `SOUL.md` 大小上限的整併斷路器（人格檔案超過上限時，無論是哪個引擎在跑，agent 的 prompt 都會被凍結）。

AEE 一輪提交之後，新增的條目會先經過觀察，才會定案：

```toml
[evolution]
aee_settle_hours = 24   # default 24; the agent runs no new AEE round until it elapses
```

### 策略組合（AEE 每輪怎麼選意圖）

AEE 每一輪都會用決定性的方式，從 per-agent 的組合中挑出一個意圖：`repair`（消化 `MistakeNotebook` 積壓）、`optimize`（優化既有 `success_streak` 偏低的條目），或 `innovate`（提出新條目），取代先前原始的 ε-exploration：

```toml
[evolution]
strategy = "balanced"   # balanced (default) | innovate | harden | repair_only
```

| `strategy` | 修復（Repair） | 優化（Optimize） | 創新（Innovate） |
|---|---|---|---|
| `balanced` | 5 | 3 | 2 |
| `innovate` | 2 | 3 | 5 |
| `harden` | 4 | 5 | 1 |
| `repair_only` | 10 | 0 | 0 |

無法辨識的值會觸發 `warn!` 並退回 `balanced`，打錯字不能悄悄改變演化行為。當錯誤積壓是空的時候，`repair` 會被降級為 `optimize`，不管設定的組合是什麼。

commit 閘門逐維度的雜訊帶（跟冠軍多接近才算平手，也就是 matches-or-improves 中的「matches」）同樣可以設定，這些預設值只是等待實證校準的起始值，不是已經調校好的數字：

```toml
[evolution.noise_band]
cases = 0.05     # eval-case pass-rate dimension; hard-clamped to ≤ 0.10
                 # (a wider band means the cases are noisy, not that the band should widen)
judge = 0.15     # LLM judge score dimension (judges vary run to run)
anti_sycophancy = 0.0   # deterministic — zero band
novelty = 0.05
relevance = 0.10
```

### Eval 語料庫位置（AEE 的量測依據）

AEE 透過重播 agent 的 eval suite 來為候選項評分。一個 agent 對應的 suite，就是 suites root 底下以該 agent 命名的目錄：

```toml
# ~/.duduclaw/config.toml
[evolution]
eval_suites_root = "evals"        # default: <home>/evals; relative paths resolve against the home dir
# eval_binary    = "/usr/local/bin/duduclaw"   # optional: which binary to spawn for `duduclaw eval`
```

`DUDUCLAW_EVAL_SUITES_ROOT` 可以針對單一行程覆寫 `eval_suites_root`。開發者的 checkout 通常會把它指向 repo 裡的 `commercial/evals`。

**分數要有意義，語料庫得先錄過一次。** AEE 是以重播模式量測（離線、零 LLM 成本），每個案例都要讀取一份已錄製的 transcript。旁邊沒有 `<stem>.transcript.jsonl` 的案例無法重播：

```bash
duduclaw eval ~/.duduclaw/evals/<agent-id> --record   # one live pass, then replay is free
```

在那一次錄製跑完之前，整個 suite 都會被當成*尚未量測*，不會被判定為失敗。未錄製的案例是基礎設施上的缺口，不是品質訊號，把它算成 0.0 分只會固化出一個全零的冠軍，讓後面永遠沒有東西能改進它。

**沒有 suite 時，量測會優雅降級。** 語料庫尚未錄製（或 eval binary 連不上）的 agent 仍然會被量測，`cases` 這個維度會回報為*缺席*，絕不是零分，commit 閘門只比對確實存在的維度。這個降級會顯示在該輪的稽核紀錄裡（`case_dimension_available: false`）並留下一行 `warn!`，不是靜默發生。

**但新增條目確實需要至少一個 eval case（v1.53，G6/E1）。** 每一個 playbook `Add` 都必須連結至少一個 eval case，並帶有可機器檢查的斷言（`must_use_tools` / `output_contains` / …），零 eval case 的 agent 無法累積*新*規則。要從 agent 的 SOUL 行為規則起步建立語料庫：

```bash
duduclaw eval-scaffold --agent <agent-id>   # drafts into evals-drafts/
```

審查這些草稿，把好的搬進 `evals/<agent-id>/`，再照上面的方式錄製。草稿被刻意寫進獨立的 `evals-drafts/` 目錄，這樣未審查的案例就永遠不會滲入正式語料庫。對沒有錄製 transcript 的案例重播斷言，會回報*未驗證*（僅供參考），絕不會靜默視為通過。

自 v1.53 起，錄製本身沒有副作用：`--record` 會把 agent 的 `.mcp.json` 改寫成一份暫存副本，其 `DUDUCLAW_HOME` 指向 eval home（並帶一個佔位用的 MCP key），所以錄製過程碰不到正式環境的狀態，也不會把真的金鑰洩漏進 transcript。

## Autopilot 刻意不受總開關管轄

Autopilot 規則（`autopilot.*`）是**使用者明確設定的自動化**，是你自己寫的規則，所以 DuDuClaw 把它當成一道指令，不是 agent 自主演化出來的東西。演化總開關不會動到 autopilot。想停用某條特定的 autopilot 規則，請到 dashboard 的 Autopilot 頁面停用它。

唯一的例外是下面的緊急凍結，那是設計成粗暴的「全部停下來」，並且會提醒你另外去停用 autopilot。

## 一次性凍結／解凍（企業版逃生艙）

當狀況看起來不對勁，需要一個 agent *立刻*停止自我改變時：

```bash
duduclaw agent freeze <agent-id>
```

這會在一次編輯中同時把 `[evolution] enabled = false` 與 `[heartbeat] enabled = false` 設好，並寫入一筆 `security_audit.jsonl` 紀錄（`event_type = agent_freeze`）。什麼都不會被刪除，要復原的話：

```bash
duduclaw agent unfreeze <agent-id>
```

會把 `[evolution] enabled = true` 與 `[heartbeat] enabled = true` 復原。Autopilot 規則不會被自動修改，這個指令會印出提醒，告訴你如果需要的話請自行到 dashboard 停用。

## 驗證凍結真的生效了

總開關的重點在於，你切下去之後可以證明真的沒有任何東西還在演化。檢查方式：

1. 在該 agent 上設定 `[evolution] enabled = false`。
2. 觀察 `prediction.db`（`evolution_events` / `gvu_experiment_log`）：不應該出現新的 GVU 紀錄列。
3. `SOUL.md` 的 SHA-256 指紋不應該改變。
4. 不應該開啟任何觀察期（version store 中沒有待定版本）。

這對應到本專案針對這個功能所跑的自動化驗證。

## 其他頁面上的相關開關（v1.53）

不是演化開關，但屬於同一套「學習與驗證」機制的一部分：

| 鍵 | 預設值 | 頁面 |
|---|---|---|
| `config.toml [memory] novelty_gate` | `true` | [memory-and-knowledge.md](../memory-and-knowledge.md) — 拒絕近乎重複的語意記憶 |
| `config.toml [dispatch] grounding_precheck_enabled` | `true` | [goal-loop.md](./goal-loop.md) — 在驗收判官之前做零 LLM 成本的證據檢查 |
| `config.toml [dispatch] two_stage_judge` | `true` | [goal-loop.md](./goal-loop.md) — 在 MAV 驗收判官團之前先跑一個低成本的第一階段評估器 |
| `config.toml [goal_loop] resume_on_restart` | `"pause"` | [goal-loop.md](./goal-loop.md) — gateway 重啟時把進行中的目標任務升級為 `needs_human`；設成 `"auto"` 則改為自動恢復。Dashboard：設定 → 自動化 |
| `config.toml [task_forward_model] enabled` | `false` | [goal-loop.md](./goal-loop.md) — 任務層級的預測、行動、驗證世界模型 |
| `config.toml [goal_loop] progress_report_minutes` | `10` | [goal-loop.md](./goal-loop.md) — 已認領的目標任務連續這麼多分鐘沒有進度訊號時發出通知（絕不介入）；`0` 表示停用 |
| `config.toml [goal_loop] tool_streak_advisory` | `true` | [goal-loop.md](./goal-loop.md) — 同一輪內連續 3/5/8 次相同工具呼叫時，注入一則逐步升級的提醒；零 LLM 成本，絕不阻擋 |
| `config.toml [dispatch] admission` | `"queue"` | [goal-loop.md](./goal-loop.md) — 超過容量的臨時子 agent 產生會改為持久 FIFO 排隊，不再立即失敗；設成 `"fail"` 則恢復 pre-H19 的直接拒絕行為 |
