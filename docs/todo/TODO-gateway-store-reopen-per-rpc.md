# TODO: gateway 三個 SQLite store 每次 RPC 都重開（日誌噪音＋輕微浪費）

- **狀態**: Open
- **建立**: 2026-08-17
- **嚴重度**: Low（衛生問題，非洩漏、非正確性）

## 現象

dashboard 開著時，gateway log 以固定節奏重複：

```
INFO duduclaw_gateway::approval:      ApprovalStore initialized   （每輪 ×2）
INFO duduclaw_gateway::growth:        GrowthStore initialized
INFO duduclaw_gateway::custom_skills: CustomSkillStore initialized
```

來源：`growth.snapshot` 等 dashboard RPC 每次呼叫都 `Store::open()`（`handlers.rs`
的 `handle_growth_snapshot` / `gather_growth_facts` / `handle_growth_daily_report`，
以及多處 `ApprovalBroker::open`）。前端輪詢間隔多久，日誌就多久刷一組。

## 已排除的嫌疑（2026-08-17 實查，別再追）

曾懷疑這是慢性記憶體洩漏（LWM 實驗容器 guest-OOM 事故的調查線）。實查結論：
`open()` 只做 `Connection::open` + `init_schema`（CREATE IF NOT EXISTS），無背景
task spawn，drop 即釋放——**不是洩漏**。該事故的 OOM 兇手另有其人（未定案；
容器已加 cgroup mem_limit 防護）。

## 建議修法

- 把三個 store 掛進 `MethodHandler` 常駐欄位（比照 `task_store`/`cron_store` 的
  `RwLock<Option<Arc<…>>>` 慣例），RPC 改用共享實例；或
- 最低限度：`init_schema` 只在首次開啟時跑、`initialized` 日誌降為 `debug!`。

注意 `ApprovalBroker::open` 呼叫端遍布 goal loop / topology / skill approval /
mcp（`grep -rn "ApprovalBroker::open("`），收斂時要一起盤點，避免只改 dashboard 面。
