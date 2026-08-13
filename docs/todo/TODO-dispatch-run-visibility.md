# TODO: 排程／派工執行紀錄的可觀測性（cron 跑了 202 次、儀表板一筆都看不到）

> 狀態：**Resolved（2026-08-13 方案 A 落地，欠活體驗證）** · 類型：可觀測性缺口（平台）· 優先：High
>
> **修復摘要**：`run_steps.db` 新增 `dispatch_runs` 表；`claude_runner` 的兩個派工
> 入口（`call_claude_for_agent_with_type` / `_preloaded`）以 `invoke_recorded` 包裝
> ——Cron/Dispatch 呼叫結束時落一筆 run（首尾時間/狀態/遮罩後 in-out 摘要）＋把
> WP-A4 原生工具事件寫成 `dispatch:<run_id>` 步驟列（外層 goal-loop collector 存在
> 時唯讀共用、不遮蔽；無則自建 scope）。`runs.list` 合併 dispatch runs（`channel`
> 欄位帶 "cron"/"dispatch"），`runs.get` 新增 `dispatch:<id>` 分支回放步驟。
> 保留期沿用 run_steps 7 天修剪。Chat/Evolution 不錄（既有面已覆蓋，防重複）。
> 發現於：LWM 交易實驗 D3-D4——使用者反映 dashboard 看不到 cron 執行紀錄／goal-loop 狀態／對話紀錄。
> 相關：[TODO-agent-cross-invocation-continuity.md](TODO-agent-cross-invocation-continuity.md)（同一實驗的
> 記憶連續性線）、`docs/features/44-working-state.md`（工作狀態）。

## 一句話

`runs.list` 的資料來源是 sessions.db 的頻道對話訊息、`run_steps.db` 只由
channel_reply 寫入——**cron／派工（claude_runner）路徑的每一次執行完全不落
執行紀錄**，於是「執行紀錄」頁只看得到聊天回合，看不到排程真正做的事。

## 盤點證據（2026-08-13，對實驗容器 18899 實測 RPC）

| 使用者反映「看不到」 | 實測結果 | 判定 |
|---|---|---|
| 對話紀錄 | `chat.sessions.list` 回 1 條（TG，53 turns）、`chat.sessions.history` 可取逐字稿 | **後端正常**（`/conversations` 頁自 v1.50 就在側邊欄）；需確認使用者看的是容器 18899 還是主環境 18789 |
| cron 執行紀錄 | `cron.list` 回 5 條含 last_run_at／run_count（intraday run_count=202）；但 **per-run 內容零筆**（run_steps 只有 telegram session） | **真缺口**：cron 路徑不落 run 紀錄 |
| 執行紀錄（runs） | `runs.list` 只回頻道對話折出的 runs | 同上——runs 只涵蓋 channel 路徑 |
| goal-loop 任務內狀態 | 任務詳情頁（v1.55 起）有迭代輪次＋判官回饋；但 `<state>` 區塊（confirmed_facts／hypotheses）無 UI；實驗日常是 cron 驅動、任務板只有 2 筆歷史 | 半缺口＋認知落差 |
| LLM→LWM 實驗資訊 | 持倉／損益／forecast／校準在容器檔案與 masterlink MCP，平台本來就無此頁 | 實驗層需求（custom widget 或實驗頁） |

## 根因

1. `run_steps.rs` 的寫入端只有 `channel_reply`（G12 run inspector 當初的範圍）；
   `claude_runner` 的 dispatch/cron/heartbeat 串流迴圈雖然已逐事件解析 `tool_use`
   （WP-A4 `ingest_stream_json_event_for_native_tools`），但不寫 run_steps。
2. `handle_runs_list` 由 sessions.db 訊息折疊 runs；cron 派工不建 session → 折不出東西。
3. cron_tasks 只有聚合欄位（last_run_at／last_status／run_count），無 per-run 表。

## 修法方向（建議，未拍板）

- **A（核心，零前端改動）**：`run_steps.db` 加 `dispatch_runs` 表
  （agent、source=cron 名稱／request_type、started_at、ended_at、status、
  in/out preview 各 ≤500 chars 秘密遮罩）；`claude_runner` 在 invocation 首尾
  落 start/finish，串流迴圈把 `tool_use` 步驟以 `dispatch:<run_id>` session_key
  寫進 run_steps（複用 WP-A4 已解析的事件，成本近零）；`runs.list`／`runs.get`
  合併 dispatch runs → RunsPage 直接看到每次 cron 跑了什麼。保留 run_steps
  既有的 7 天／per-agent 上限修剪。
- **B（前端小改）**：RoutinesPage 每條 cron 加「最近執行」抽屜（讀合併後的
  runs.list，filter by source）；TaskDetailPage 補 `<state>` 區塊顯示。
- **C（實驗層）**：LWM 容器用 custom widget 呈現持倉／損益／最近 forecast。

## 測試計畫

- [ ] cron 觸發一次派工 → runs.list 出現一筆 source=cron 的 run（含步驟數）。
- [ ] runs.get 能回放該 run 的 tool 步驟。
- [ ] 頻道路徑行為不變（step 計數不重複計）。
- [ ] 修剪：滿 7 天／超量自動刪。
