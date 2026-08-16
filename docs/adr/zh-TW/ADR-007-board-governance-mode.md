# ADR-007:CEO/董事會治理模式

- Status: Accepted(opt-in,預設關閉)
- Date: 2026-07-09
- Related: WP17(`commercial/docs/TODO-client-demo-gaps-2026-07-08.md`)、ADR-004、RFC-21

## Context

目標客群轉向公司部署:demo 客戶是一個約 20 人的團隊。對於非工程背景的老闆而言,
「我是董事會,我的 AI 員工向 CEO 報告」比原始的 agent 設定要來得更自然的心智模型。
Paperclip 證明了這個形狀是可行的:一個能夠 pause/resume/terminate 並設定預算的董事會,
一個向董事會提出策略待核准的 CEO,以及層層下放的預算。單一使用者並不需要這一整套。

## Decision

新增一個 opt-in 的 `[governance] board_mode`(預設 `false`)。關閉時,每一條路徑都與
今天逐位元相同。開啟時,我們把這個比喻對映到既有的原語上,不發明新的實體型別:

- **Board(董事會)**= 持有董事會權限的人類使用者(對齊 WP15 最細粒度使用者 +
  `rbac.rs`)。硬不變式:**董事會永遠是人類,絕不是 agent。**
- **CEO** = `reports_to` 樹的樹根 agent(既有概念)。
- **Initiative(倡議)**= 標記 `kind = "initiative"` 的頂層 Task Board 任務(附加式)。
- 所有有實質後果的決策都經由既有的 `ApprovalBroker` + 稽核走。`action_kind` 字串收斂
  成一個型別化的 `ApprovalKind`(與已儲存的字串 serde 相容),讓自動化只能自動裁決安全
  的類型;`StrategicPlan` 與 `AgentHire` 僅限董事會人類。

重用優先於新建:凍結 = WP1 的 `agent freeze`;核准 = ApprovalBroker;預算 = `budget.rs` +
WP14 事件 UI;招募 = 既有的 `create_agent`;組織 = `reports_to`;任務 = Task Board。
真正新增的部分:策略提案流程、Board 面板、以及公司層級的預算層。

## Fail-closed 不變式(已強制執行、有 unit test,見 `governance.rs`)

- `can_decide(StrategicPlan|AgentHire, decider)` 只有在 decider 是持有董事會權限的人類
  時才回傳 true;agent 身分一律無條件拒絕 + 稽核記錄。
- `can_create_initiative` 僅限董事會人類;CEO agent 可以被*委派*一個 Initiative,但
  不能自行建立。
- 在 `board_mode` 開啟時,沒有任何 agent(包括 CEO)可以透過 MCP/tool 路徑編輯
  `[budget]` 值,只有 Board 面板 RPC 可以,以防止 agent 拉高自己的上限
  (self-promotion)。`agent_may_edit_budget(board_mode)` 在開啟時回傳 false。

## Consequences

- 單一使用者不受影響(opt-in、預設關閉;這是 WP17 測試套件裡的硬性測試項)。
- 型別化的 `ApprovalKind` 對 WP8(skill 啟用)與 WP16(通道按鈕)也有好處:一個
  enum,一個判斷自動化可以碰哪些類型的地方。
- 剩餘待整合項目(已列管):CEO 策略提案生成、Board dashboard 面板、公司→agent
  的預算層層下放接線、以及 `create_agent`/`spawn_agent` 上的 AgentHire 二次核准。
  這些都會建立在本 ADR 記錄的不變式之上。
