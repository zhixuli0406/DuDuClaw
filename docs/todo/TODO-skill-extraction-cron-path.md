# TODO: 技能萃取的排程路徑（cron 場景永遠等不到使用者回饋）

> 狀態：Open（2026-08-13 介面盤點完成、方案已定向，待實作）· 類型：設計缺口（平台）· 優先：Medium-High
> 發現於：LWM 實驗 D4 零萃取調查——技能萃取四天零產出的其中一層根因。
> 相關：[TODO-dispatch-run-visibility.md](TODO-dispatch-run-visibility.md)（同一系列
> 「cron 路徑是二等公民」架構債；該篇已修執行紀錄與蒸餾，本篇處理技能）。

## 一句話

技能軌跡只在頻道對話路徑錄製，且 finalize 需要**使用者在 2 turns 內給出
正/負回饋**（`detect_user_sentiment`）——cron/派工場景既不錄軌跡、也永遠
沒有使用者回饋，技能萃取對排程驅動的 agent 完全不存在。

## 介面事實（2026-08-13 盤點）

- 錄製：`skill_extraction/recorder.rs` `TrajectoryRecorder`（in-memory，
  `start`/`record_turn`/`finalize(outcome, sentiment)`），只被
  `channel_reply.rs` 呼叫（回覆後錄 turn；下一則使用者訊息帶情緒才 finalize）。
- 萃取：`SkillExtractor::extract_heuristic(&Trajectory)`——零 LLM，輸入是
  `Trajectory { turns[{role, content, tools_used}], outcome, … }`。
- 落點：`ctx.skill_bank`（`SkillCache`，掛在 channel_reply 的 ReplyContext）
  → 週期性 distillation scan（每 ~20 對話）。**claude_runner／dispatch_engine
  拿不到這個 ctx——這是接線的主要障礙。**
- 可用素材：`invoke_recorded`（claude_runner）已擁有 prompt＋reply＋
  native tool events（工具序列＋成敗）——正是一段完整軌跡；goal-mode 任務
  在 `dispatch_engine` settle 處另有 MAV 判官結果。

## 方案（定向，未實作）

1. **skill_bank 共享化**：`SkillCache` 從 ReplyContext 私有改為
   `OnceLock`/`Arc` 全域（或掛 server state 傳入 claude_runner）——
   與 `run_steps::shared_store` 同款模式。
2. **成功訊號分級（取代使用者回饋，cron 場景）**：
   - **強**：goal-mode 輪被 MAV 判官 accept → `TrajectoryOutcome::Success`
     （dispatch_engine settle accept 分支構造 Trajectory 餵萃取）。
   - **弱**：一般 cron run `completed` 且 ≥3 個工具步驟且全部成功 →
     Success 但標弱訊號（metadata），供 distillation 降權。
   - 失敗步驟或 Err → 不餵（保守：壞軌跡不進 bank）。
   - 判官 reject → `Failure`（負樣本進 Bayesian update）。
3. **去重**：同一 cron 任務每天跑 N 次、軌跡高度重複——複用 novelty gate
   （0.92 n-gram）或以 (agent, 工具序列 hash) 去重，防 bank 洗版。
4. **與 gap_accumulator 的關係**：cron 軌跡的 domain gap 累積照舊走
   episodic 記憶（蒸餾管線已接，2026-08-13）——本篇只補「成功軌跡→技能」線。

## 驗收

- [ ] goal 任務判官 accept 後，skill_bank 出現該輪軌跡萃取的技能候選。
- [ ] 一般 cron 成功 run 以弱訊號進 bank，重複軌跡被去重。
- [ ] 頻道路徑行為 byte-identical（回饋 finalize 不變）。
