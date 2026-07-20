# 半自動拓撲演化（D5，human-gated）

DuDuClaw 的 GVU / SOUL.md 自我演化只優化「節點」——每個 agent 的 prompt。多 agent
之間的「邊」，也就是 `reports_to` 階層把某類任務路由給誰，一直是寫死的。D5 讓這條邊
變成可演化的對象，但每一次改動都必須經過人工核准，機器只負責提案與證據收集。

設計出處：GPTSwarm（arXiv:2402.16823，拓撲是可學習物件）、AFlow（2410.10762）、
ADAS（2408.08435，全自動改寫控制流是 runaway 風險最高的能力，因此 D5 刻意不做全自動）。

> 預設關閉。整套機制只有在 `config.toml` 設定 `[topology_evolution] enabled = true`
> 時才會啟動；關閉時派工路徑與純 `FixedHierarchy` byte-identical。

## 運作流程

1. **證據分析**（純函式，可單測）
   背景驅動器每 `tick_secs` 掃描 task store，聚合每個 `(agent, task_class)` 在近
   `lookback_days` 天內的品質訊號：MAV/review 拒絕率、needs_human 升級率、goal-loop
   無進展 oscillation 次數。`task_class` 取任務的第一個 tag（與 D4 RoundRobin 同口徑），
   無 tag 時退回 priority。樣本基底是「已定案的 goal-mode 任務」（狀態 done / needs_human
   / failed），其中 needs_human / failed 或 `retry_count > 0` 記為一次拒絕。

2. **提案**（不是直接改動）
   當某 agent 對某類任務的樣本數 ≥ `min_samples` 且拒絕率 ≥ `reject_rate_threshold`，
   而同一 `reports_to` 父層的 sibling 中有人把同類任務做得更好（拒絕率更低、樣本也
   足夠），驅動器產生一筆 `reroute` 提案，附上證據（樣本數、拒絕率、最多 10 筆 sample
   task id）。沒有合格 sibling 就不提案——空結果優於假結果。每個 tick 最多提一筆。

3. **人工閘門（不可繞過）**
   每筆提案都走 `ApprovalBroker`（`action_kind = "topology_reroute"`）。這是 ActionGuard
   語意上的 **always-human**：不經 LLM judge、不受 `autonomy_level` 放寬，程式上只呼叫
   `request` + `poll`，沒有任何自動核准路徑。TTL 過期 = 拒絕（broker fail-closed）。
   人工可透過 dashboard 的 `approvals.decide` 或通道按鈕核准/退回。

4. **生效與觀察期**
   核准後寫入 `~/.duduclaw/routing_overrides.json`（advisory lock + 原子 temp/rename）。
   `FixedHierarchy` 派工時先查 active override：命中 `(task_class, from_agent)` 就改派給
   `to_agent`。override 檔缺失或損毀一律視為無 override，路由回到現狀（fail-safe）。
   生效後進入 `observe_hours`（預設 24h）觀察期。

5. **自動回滾**
   觀察期內若 `to_agent` 對該類任務的拒絕率 ≥ `from_agent` 的歷史基準值，override 立即
   `rolled_back`，路由自動還原。觀察期過且確實優於基準 → `confirmed`。樣本不足 → 延長
   一次觀察期，再不足即 `rolled_back`（保守收斂）。

6. **防提案風暴**
   同一 `(task_class, from_agent)` 在 `proposal_cooldown_days`（預設 7 天）內最多一筆提案
   （含被拒者），記錄在 override 檔的 proposal log；已有 active override 或 pending 提案時
   也不重複提。`dispatch_guard` 滑窗照常適用，不因 D5 繞過。

所有提案／核准／回滾／確認都寫入 `events.db` 事件與 dashboard Activity Feed
（`topology.proposed` / `topology.approved` / `topology.rejected` / `topology.rolled_back`
/ `topology.confirmed` / `topology.extended`）。

## 設定

```toml
[topology_evolution]
enabled = false            # 主開關，預設關閉
lookback_days = 14         # 證據回看窗口（天）
min_samples = 5            # 一個 (agent, task_class) 格子的最少已定案樣本數
reject_rate_threshold = 0.6  # 觸發提案的拒絕率門檻
observe_hours = 24         # 核准後的觀察期（小時）
proposal_cooldown_days = 7 # 同一條邊的提案冷卻天數
tick_secs = 3600           # 驅動器 tick 週期（秒）
approval_ttl_secs = 86400  # reroute 核准的 TTL（秒），過期 = 拒絕
```

## Dashboard RPC

`topology.list`（require_manager）回傳目前的 routing overrides 與 pending reroute 提案，
供 dashboard 呈現 D5 狀態。核准/退回沿用既有的 `approvals.list` / `approvals.decide`。

## 風險與邊界

- **預設關閉**，且啟動需要 `ApprovalBroker` 可用，否則 D5 不啟動（提案沒有人工閘門就
  不允許存在）。
- 機器只做可逆的事（提案、觀察、回滾），不可逆的事（真正改路由）永遠留給人拍板。
- D5 只在預設的 `FixedHierarchy` 階層路由上疊加。當操作者已明確選用 `RoundRobin` /
  `LlmSelect`（`[dispatch] policy`），那是刻意的路由選擇，D5 override 只在其空 roster
  fallback 到 hierarchy 時才生效。
- override 是路由層的改動，不會回填已在執行中的任務；已改派的 in-flight 任務維持原樣，
  未來派工才套用新路由或回滾。

依 opus-playbook 的觀察期紀律，D5 應在 D1–D4 穩定、eval 樣本量足夠後再啟用。
