# 一般成員 AI 員工可見性修正

分支：`fix/v180-member-agent-visibility-20261107`，起點 `39cf55b30`。本次僅修改 dashboard、測試與文件，沒有修改 gateway、commit、push、安裝套件或執行 `cargo fmt`。既有 worktree 由交辦方建立，本 session 未另開 worktree。

## 調查結果（程式碼證據，未連線實際部署）

### RPC 的現有回傳範圍

- 帳號角色只有 `Admin`、`Manager`、`Employee`，沒有獨立的 `Member` enum（`crates/duduclaw-auth/src/models.rs:6`）；本報告的一般成員指後兩者。
- `agents.list` 已經走過濾 handler（`crates/duduclaw-gateway/src/handlers/dispatch_core.rs:55`），不能把現況描述成「每個非管理員一律收到全員」。`visible_agents()` 對 Admin 回傳不限範圍，其他角色回傳直接綁定的 agent 名稱（`crates/duduclaw-auth/src/acl.rs:167`）。Manager 的團隊授權不在這份名單。
- 清單排除 soft-deleted，預設排除 archived；`include_archived: true` 可列出綁定範圍內的 archived（`crates/duduclaw-gateway/src/handlers/agents_list_rpc.rs:95`）。回傳包含名稱、顯示名稱、role/status、部門、trigger、icon/avatar/outfit、reports_to、runtime、model/account_pool、budget、heartbeat、skills、permissions、evolution。這個 handler 沒有依 main role 再排除，因此直接綁定 main 的非管理員仍可能收到它的資料。特殊 `system-operator` 限制另在 live context 移除，不等於一般 main role 的限制。
- RPC dispatch 會從 users.db 重新建立有效身分，讀取失敗、停用或不存在的帳號不能沿用舊 JWT 權限；system principal／本機無帳號模式另有既存例外（`crates/duduclaw-gateway/src/handlers/live_context.rs:1`、`:256`，`handlers/mod.rs:515`）。因此使用者實際遇到「全員」的部署，仍需確認執行版本、登入 principal、edition 以及資料庫綁定，不能由本分支推定根因。
- `agents.callable`（`handlers/agents_callable_rpc.rs:125`）使用相同呼叫授權來源，回傳 own binding、Manager 的 team grant，以及公開／通道預設員工。Admin、binding、team 是 `selectable: true`；default、public 是 `false`（同檔 `:84`）。另有 `via`、`level`、team 的 `via_user`／顯示名稱／depth、`assignable`、`manager_may_read`、精確 `call_text` 與顯示欄位。`session_id` 只影響 default 的解析，回應另含 `default_agent`。
- `agents.callable` 已透過 `for_webchat` 排除非管理員的 main-role 與 gateway-default 員工；無法讀取 default 設定時不放行（`crates/duduclaw-gateway/src/trigger_route/access.rs:92`）。特殊 machine employee 另受 Admin gate。`callable_grants` 僅從可呼叫目標計算 grant（`trigger_route/mod.rs:155`）。`manager_may_read` 是監督提示，不是擴權依據。

### 路由與導覽

`web/src/App.tsx:215` 的 AuthGuard 下，`/`、`/workspace`、`/inbox`、`/chat`、`/meeting-transcripts`、`/conversations`、`/tasks`、`/tasks/:id`、`/goals`、`/responsibilities`、`/foresight`、`/plans`、`/runs`、`/canvas`、`/files`、`/routines`、`/agents`、新增／編輯／明細、`/world`、`/computer/:agentId`、`/memory`、`/growth`、技能相關、widgets、knowledge-base、about、mail、launcher、個人帳號、`/org`、`/experts` 以及舊路由 `/marketplace`、`/wiki`、`/shared-wiki` 都沒有 Admin 路由閘。`/assign` 在目前 entry policy 下導向 `/chat`；保留的交辦元件也一併處理。

Manager 額外可到 timeline、reports、employee-health、forks、license、billing、migrate、secaudit、advanced，以及 Enterprise 的 team／team conversations（`App.tsx:344`、`:359`、`:402`、`:487`）。系統設定、安全、整合、channels、logs、presets、templates、OS、console 等有 Admin guard。RoleGuard 未達權限會回首頁（`components/AuthGuard.tsx:144`）。

導覽另受 edition、operator binding、資料存在與 feature 等條件過濾（`lib/nav-visibility.ts:110`）；側欄隱藏不等於路由不可達。這也是本次必須處理任務／技能等直接網址頁面的原因。

### 員工名單與選人入口盤點

| 入口 | 程式碼位置 | 處理 |
|---|---|---|
| 側欄 LIVE staff、主要員工選單、首頁卡片、世界／員工列表 | `components/layout/AppSidebar.tsx:241`、`:551`；`pages/HomePage.tsx:119`；`pages/WorldPage.tsx`；`pages/AgentsPage.tsx:105` | 既有 `useVisibleAgents` 改為共享 callable 授權，不再僅依 bindings／舊 team cache |
| 新對話（側欄、palette、對話頁、FirstLoginCard） | `stores/conversations-store.ts:223`；`components/home/FirstLoginCard.tsx:66` | 新對話清除非管理員舊指定 agent，仍交給既有 server resolver；FirstLoginCard 只送草稿、沒有選人名單 |
| 對話列表員工篩選與預設 main | `pages/ConversationsPage.tsx:67`、`:82`、`:98` | 共用名單，無效參數清除，使用可見的 selected／resolved 員工，不以不可見 main 作為 fallback |
| 指令面板員工命令、員工檔案命令、內容搜尋範圍 | `components/CommandPalette.tsx:235`、`:245`、`:409`、`:425` | 共用名單；檔案命令保留 direct-only，不把 team grant 變成讀檔權 |
| WebChat `@`、交辦 `@`、URL／舊選項 | `pages/WebChatPage.tsx:112`、`:214`、`:359` | session-aware 共用 callable；非管理員只建議 selectable；EmployeeRow 仍隱藏；不可見 partner 不顯示為目前選取 |
| 交辦選人與預填 | `components/assign/ComplexAssignTab.tsx:75`；`components/workspace/AgentModelPicker.tsx:28` | 過濾並清除失效預填 |
| 任務、目標、預測、計畫負責人與步驟指派、例行工作 | `pages/TaskBoardPage.tsx:759`；`GoalsPage.tsx:345`；`ForesightPage.tsx:750`；`PlansPage.tsx:88`、`:141`、`:600`；`RoutinesPage.tsx:81` | 全部使用共用來源；保留較窄的工作／direct 存取限制 |
| 持續任務的綁定名單 | `pages/ResponsibilitiesPage.tsx:161` | binding 與可見名單取交集 |
| Files、Canvas、Runs、Timeline | 各頁的 `useVisibleAgents` 與 `requestedAgent` | 名單與 URL 篩選一致，失效參數不顯示為選取 |
| Memory、KnowledgeHub、演進員工列表、Marketplace 安裝 | `pages/MemoryPage.tsx:102`、`:347`；`KnowledgeHubPage.tsx:77`；`MarketplacePage.tsx:143` | 移除獨立未過濾的 `agents.list` 選擇器來源 |
| 技能安裝／已有技能／採用／建立技能 | `pages/SkillMarketPage.tsx:540`；`components/skills/MySkillsTab.tsx:126`、`SharedSkillsTab.tsx:34`、`SkillWizard.tsx:87` | 共用名單，失效本地選項清除或回到 aggregate |
| 組織圖、我的團隊、團隊歷史、會議轉錄 | `pages/OrgChartPage.tsx`、`TeamPage.tsx`、`TeamHistoryPage.tsx`、`MeetingTranscriptsPage.tsx` | 專用 RPC 名單與共用可見名單取交集，原有更窄的歷史／轉錄限制仍適用；Admin／Personal 保留原有展示 |
| 員工明細／編輯／建立、上級選擇 | `pages/AgentDetailPage.tsx`、`pages/agent-form/{CreateAgentPage,EditAgentPage}.tsx`、`components/experts/AttachUnderSelect.tsx` | 選擇器過濾；不可見明細不掛載私人 loader／聊天操作；team-only 保留 read-only 提示 |
| 匯出對話框 | `components/DataExportButton.tsx` | 共用 direct-only 名單，失效 selection 清空 |
| 自訂首頁／gallery widget 的員工摘要 | `lib/widget-bridge.ts:41` | 使用同一純 selector；每次重算 projection，避免 15 秒摘要快取跨帳號或跨授權刷新沿用 |

其他 `useAgentsStore` 使用點包括紀錄中已有 agent 的名稱、avatar/outfit、任務既有負責人、approval/inbox 項目、桌寵表情／數量與 FirstRunGate。這些沒有提供全員選擇器或任意指定新聊天對象，本次保留既有資料／歷史授權流程。`WebsiteCredentialsPanel`、channels 與 settings 的獨立 roster 只在 Admin 路由／元件中使用。

## 實作與測試

共享來源為 `stores/callable-agents-store.ts`，以帳號、role/status、bindings、primary、JWT、edition 與 chat session 隔離回應。換帳號／權限時清空；舊請求不得回填新身分；focus／連線與員工事件刷新。載入中與失敗皆是 `null` grants。`lib/data-scope.ts` 的 `useVisibleAgents`／`selectVisibleAgents` 僅採 `selectable === true`，main 沒有從原始 roster 取得豁免；明確 selectable 的 main grant 才可能顯示。Admin／Personal 不受 callable 失敗影響。

`includeTeam: false` 保留原有資料範圍：只有 callable 的 direct grant 與詳細 roster 的交集；team display entries 不帶 model／budget。它不能用來推導任何額外 RPC 權限。

新增介面矩陣覆蓋側欄、對話頁、palette、交辦選人、匯出、記憶、知識庫、員工列表、檔案、foresight、goals、canvas、runs、timeline、plans、routines、task filter、skills filter、skill builder，逐一驗證 Employee、Manager、Admin、callable 失敗；另測 `@`、新對話 stale main。Hook 測試含載入中、拒絕、個人版、明確 main grant、team display-only、帳號切換、並行 consumer 與 refresh 失敗。Widget 測試含 role、拒絕與切換帳號。既有測試補明確登入身分／callable fixture，移除預期成員以 main 作為 fallback 的斷言。

最終驗證結果：

- `cd web && npx tsc --noEmit -p .`：exit 0。
- `npx vitest run` 指定本次修改／新增的 22 個測試檔：22 files passed，391 tests passed（29.57 秒）。
- `npx vitest run 2>&1 | tail -6`（另以 `tee` 保存完整輸出）：461 files passed，4224 tests passed（99.78 秒）。
- 介面矩陣本身：80 tests passed。完整套件另包含既有的首頁、FirstLoginCard、會議轉錄、持續任務、角色路由與管理員功能測試。
- 最終輸出仍有 jsdom 的 canvas／navigation 未實作訊息，沒有失敗測試。
- 沒有執行 `npm ci`、Rust build／test、真實瀏覽器或原生平台驗收；本次指定的 TypeScript／Vitest 驗證全部通過。


## 交給後端後續工作的界線

1. **main 的資料讀取與工作准入不一致。** `agents.list` 的直接綁定範圍仍可能列出 main 及詳細 model/budget 等欄位；`agents.inspect` 只檢查 Viewer binding（`handlers/dispatch_core.rs:170`），此處沒有 main-role deny。UI 隱藏不會撤銷綁定或阻止手寫 RPC。需決定 main/default 是否應在所有資料讀取路徑統一拒絕。
2. **結構名單可比可用名單廣。** `agents.org_chart` 除直接綁定外，還列同部門與祖先節點（`handlers/agents_list_rpc.rs:9`、`:41`、`:60`）。這次 UI 取交集；RPC 本身仍可讀到這些結構資料。這是現行設計，不等於可讀它們的設定或可派工。
3. **公開／預設呼叫權仍存在。** `agents.callable` 仍傳 public/default 的 display metadata，並標 `selectable: false`。本次 UI 的選取與 `@` 建議省略它們；手動輸入或自行呼叫協定仍由既有 routing／公開權政策決定，沒有在此分支撤銷（`trigger_route/access.rs:114`）。若產品需求是禁止成員呼叫公開員工，必須另改 server policy。
4. **可選取不等於可做所有工作。** Viewer direct grant 也能得到 `selectable: true`，而 `assignable` 另外由 `can_assign_to` 決定；檔案／memory／設定仍要求直接授權。需要後端回合比對每個實際呼叫／委派／資料讀取端點，不能以這次 UI 測試宣稱全部 RPC 已收斂。
5. **已確認存在的 main 工作防線，不重複報成未修漏洞。** WebChat 的 `authorize_target`／`webchat_allows` 拒絕非管理員 main/default（`webchat/access.rs:140`、`trigger_route/access.rs:100`）；任務 create／goal create／update 已呼叫 `deny_main_for_non_admin`（`handlers/tasks_detail_rpc.rs:362`、`handlers/tasks_rpc.rs:222`、`:419`；helper 在 `handlers/main_agent_access.rs:5`）。該 helper 明示 MCP delegation 有自己的 policy。本次未驗證其他通道、MCP delegation 或已部署 binary 的活體行為。

本報告的後端判斷來自指定起點的程式碼閱讀；沒有執行 Rust 測試、啟動 gateway 或用真實 member 帳號發 RPC，也沒有做 Claude／Daybreak 的獨立安全審查。

## 修改檔案

以下清單由本次 working diff 產生；包含原始碼、回歸測試與三語文件。
- `CHANGELOG.md`
- `docs/features/05-security-defense.md`
- `docs/features/62-calling-ai-staff.md`
- `docs/features/ja-JP/05-security-defense.md`
- `docs/features/ja-JP/62-calling-ai-staff.md`
- `docs/features/zh-TW/05-security-defense.md`
- `docs/features/zh-TW/62-calling-ai-staff.md`
- `docs/guides/dashboard-settings.md`
- `docs/guides/ja-JP/dashboard-settings.md`
- `docs/guides/zh-TW/dashboard-settings.md`
- `web/src/components/CommandPalette.test.tsx`
- `web/src/components/CommandPalette.tsx`
- `web/src/components/DataExportButton.tsx`
- `web/src/components/agent-visibility.test.tsx`
- `web/src/components/assign/ComplexAssignTab.tsx`
- `web/src/components/experts/AttachUnderSelect.tsx`
- `web/src/components/layout/AppSidebar.test.tsx`
- `web/src/components/shared/RequiredEnvFields.test.tsx`
- `web/src/components/skills/MySkillsTab.tsx`
- `web/src/components/skills/SharedSkillsTab.tsx`
- `web/src/components/skills/SkillWizard.tsx`
- `web/src/components/workspace/AgentModelPicker.test.tsx`
- `web/src/components/workspace/AgentModelPicker.tsx`
- `web/src/lib/data-scope.test.ts`
- `web/src/lib/data-scope.ts`
- `web/src/lib/widget-bridge.test.ts`
- `web/src/lib/widget-bridge.ts`
- `web/src/pages/AgentDetailPage.test.tsx`
- `web/src/pages/AgentDetailPage.tsx`
- `web/src/pages/AgentsPage.test.tsx`
- `web/src/pages/CanvasPage.tsx`
- `web/src/pages/ConversationsPage.test.tsx`
- `web/src/pages/ConversationsPage.tsx`
- `web/src/pages/FilesPage.test.tsx`
- `web/src/pages/FilesPage.tsx`
- `web/src/pages/ForesightPage.tsx`
- `web/src/pages/GoalsPage.test.tsx`
- `web/src/pages/GoalsPage.tsx`
- `web/src/pages/KnowledgeHubPage.test.tsx`
- `web/src/pages/KnowledgeHubPage.tsx`
- `web/src/pages/MarketplacePage.tsx`
- `web/src/pages/MeetingTranscriptsPage.tsx`
- `web/src/pages/MemoryPage.test.tsx`
- `web/src/pages/MemoryPage.tsx`
- `web/src/pages/OrgChartPage.test.tsx`
- `web/src/pages/OrgChartPage.tsx`
- `web/src/pages/PlansPage.tsx`
- `web/src/pages/ResponsibilitiesPage.tsx`
- `web/src/pages/RoutinesPage.tsx`
- `web/src/pages/RunsPage.tsx`
- `web/src/pages/SkillMarketPage.test.tsx`
- `web/src/pages/SkillMarketPage.tsx`
- `web/src/pages/TaskBoardPage.test.tsx`
- `web/src/pages/TaskBoardPage.tsx`
- `web/src/pages/TeamHistoryPage.test.tsx`
- `web/src/pages/TeamHistoryPage.tsx`
- `web/src/pages/TeamPage.test.tsx`
- `web/src/pages/TeamPage.tsx`
- `web/src/pages/TimelinePage.tsx`
- `web/src/pages/WebChatPage.test.tsx`
- `web/src/pages/WebChatPage.tsx`
- `web/src/pages/agent-form/CreateAgentPage.tsx`
- `web/src/pages/agent-form/EditAgentPage.test.tsx`
- `web/src/pages/agent-form/EditAgentPage.tsx`
- `web/src/pages/agent-form/EditAgentPage.v168.test.tsx`
- `web/src/stores/callable-agents-store.ts`
- `web/src/stores/conversations-store.ts`
- `wiki/impl/2026-11-07-member-agent-visibility.md`
