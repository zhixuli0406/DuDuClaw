# Native Linux isolation security fixes — rounds 1–3, 2026-11-05

**狀態：PARTIAL／NO-GO。** Round 3 接續本 worktree 未提交的 rounds 1–2，保留在 `feat/iso-secfix-20261105`，沒有 commit。Connector 具體 executor 原始碼已覆蓋 **62／62**；四個先前缺少的 finite adapters 已有受限實作；Spawnd 已有 authorized-workspace hookup。正式 boot caller、Google 完整 requester／非 OAuth 路徑、所有 Service／背景入口及原生整合驗收仍未完成。缺件拒絕不是功能驗收。依交辦未在本機建置 gateway／CLI，不能宣稱整棵 workspace 編譯通過。

依據為 round-1 MCP boundary 與 egress/parse reviews（`20261105-131141-codex-75237-219.md`、`20261105-130643-codex-75237-217.md`）及 design §6。本文件保留於 docs，僅記錄安全邊界、修補狀態與驗證，不收錄操作性攻擊重現。逐工具狀態見 [tool matrix](native-linux-isolation-tools.md)。

## Round 3 已落地

### 剩餘 12 個 connector executors

- `db_sources`：有限多帳號清單。先通過 owner／finite grant／catalog／BindingStore，再選 deterministic anchor；每筆 member 各自走完整選帳與重查，不能因第一個候選是另一位 owner 或已撤銷而擴權／錯選。
- Channel 8：`group_history_read`、`send_message`、`send_photo`、`send_sticker`、`channel_config`、`channel_config_list`、`channel_status`、`pairing_manage`。綁定 gateway 安裝的帳號、通道及精確 chat／scope／subject；保留既有 retained-history routing、injection 與 product gates。設定讀寫只處理選定 scope，pairing 清單只含選定 subject。Google Chat／Teams 的 sender 修正為使用傳入的 home；需要 home 憑證的 subtype 只接受 SharedHome，不把 Personal route 偷換為 home。送出後的撤權／錯誤回覆不確定結果。
- Model 2：`model_search`、`model_recommend`。使用 gateway boot 的硬體快照；HF 搜尋固定公開端點，不讀取 run 的 HF token／環境，不執行硬體探測或共用快取。這不等於每次呼叫重新量測硬體，亦不提供 private model registry。
- `computer_login`：精確綁定 gateway credential ID、metadata 與 label，再經 `ComputerUseSessions::action` 的原有 session／origin／threat gates。run 只能提供 label／session selector；秘密解封與填入仍在 gateway 控制的瀏覽器工作階段，不複製到原生員工 run。

這是 executor **原始碼覆蓋**，不是每個通道 subtype、真 provider 或容器後端的成功驗收。選帳、原產品 gates 與不支援輸入仍可拒絕。

### Finite adapters

`goals_list` 的 SQL 同時限制已授權 task IDs 與員工；`responsibility_get` 清單限制 owner 與 signed responsibility IDs，單筆沿用原 summary／person-field gates；`list_reminders` 使用 bounded JSONL reader，限制 immutable reminder IDs 與員工，blocking 工作保留 authority。既有 tasks／activity／cron finite adapters 保留。

有限 `db_query` **只接受受限語法**：單表 SELECT、plain columns／`*`、AND 串接的 literal predicates、單欄 ORDER BY 與正整數 LIMIT。嚴格 lexer／parser 轉成 `SelectRequest`，由既有 builder 引用 identifier、綁定 literal；**不把原 SQL 傳到 DB**。精確 table grants 與大小寫物件名稱在執行及 metadata 查詢時檢查。JOIN／UNION／CTE／subquery／functions／aliases／qualified names／comments 等拒絕。EmployeeScope 的既有完整表授權 SQL 路徑維持原政策。這不是任意自由 SQL 的有限授權實作。

### Google 憑證橋接與 boot registration

`GoogleIsolationBridge` 接回既有 sealed Google owner 的 select → bind → claim → resolve／refresh → effect 流程，重用 Owner／consent／ACL／grant generations、一次性 claim、effect lease 與輸出遮罩。Gateway 只持有 authenticated credential reference 與 account routes；bridge 不取得 bearer getter，也不把 refresh token、client secret 或 access token放進 run 的環境／檔案／結果。

目前支援 healthy personal 與明確選定的 home OAuth／已允許的 personal→home OAuth fallback。每工具 source metadata 固定至本次 installed bridge，owner、consent 或 source generation 改變需重建 boot state。選帳／recheck 使用唯讀 Home capture，不進行隱式 migration；初次 metadata 準備仍屬明確的 host lifecycle。Google source lock 內的 checkpoint 只檢查 run／BindingStore／catalog，避免遞迴取得 source flock；產品權威由 sealed owner 的原始 pinned proof 檢查，owner 外仍執行完整 `SelectedConnector::recheck`。Refresh 的 lock、解封、送出、response 與 publication 邊界保留 live scope；隔離 transport 禁用 ambient proxy 與隱藏 retry。

**尚缺實際 requester bridge**：目前沿用 dedicated native owner 的 `Unknown`，不把 capability owner 冒充為 requester，因此預設 `bound_people`／`owner_only` 會安全拒絕。Delegate、service-account、Apps Script 等非 OAuth source owner 不由這個 adapter 假裝支援。

`InstalledConnectorProviders::assemble` 與 `IsolationDispatcher::boot` 提供正式、具型別的組裝入口；九組 provider 由 policy table 一次註冊，policy／executor 缺件或重疊即拒絕，Google 欄位只能接受 sealed bridge。Resolver、已佈建 BindingStore／catalog、A/E factories 與 employee／agent／home readback 在 admission 前組合；不從偶然找到的 credentials 建立新 grant。Worker feature 明確轉接為 `isolation-worker-integration`，search constructor 只在 checked worker 可安裝時提供。

**目前沒有 production binary／integration host caller 採用這兩個新入口**；這輪是完整組裝入口，不是已啟用的 gateway boot。既有安裝的隔離保證不因此改變。

### Service、background work 與 Spawnd

本輪把 TaskStore 的 review／needs-human／cancel／zombie CAS／iterations／steering／stop／assign-schedules／responsibility 等入口移至 checked transaction；schema／migration 也使用同一外層交易，mutex／SQLite busy 等待後與 commit 前重查。補強 channel settings、access control、website credential read／publication rollback，以及 computer session 的 control／metadata／unseal／fill／backend action 邊界，撤權不阻擋 host 必須執行的安全清理。

Memory 額外補上 `decay.rs`／`lifecycle.rs` 的窄範圍交易：取得 engine mutex、SQLite Immediate transaction 等待後、每個資料效果與 commit 前重查；RAII 早退回滾 archive／FTS／reassign，cross-DB 的 destination archive schema 也在同一交易，退出後執行 DETACH。**Decay 行為改變為兩階段一起提交：第二階段失敗會一起回滾第一階段。** Scoped cross-DB move 要求 destination 已存在，避免拒絕後留下新空 DB；未改 raw `conn_for_maintenance` API，也未加入 pathname→ATTACH 的 FD／inode pinning，該路徑仍仰賴 gateway-owned filesystem。其他 maintenance／forget／trust／layer 仍待逐一覆蓋。

持久 reminder 由 host 強制蓋上 `isolation_bound`，本 process 另保留 home／ID／不可變 execution fields 對應的 captured authority；status／error bookkeeping 不改變執行綁定。重啟、map 缺件、retarget、內容變更、capability 過期或撤銷時拒絕，不回退 host authority。Typing／blocking／callback／sender 工作顯式帶 scope，等待後與 invoke 前重查。這是 **fail-closed 延後執行**，不是可跨 gateway restart／原 request 到期的 durable reauthorization。

`SpawndBackend::with_workspace_bridge` 新增 `spawn_authorized` hookup。可信 `RootWorkspaceBridge` receipt 綁定 reservation／employee／runtime／workspace handle／relative cwd，保留實際 directory 與 root generation lease；token 對實際目錄驗證，input、IPC／TakeIo、status／wait 都重查，sticky revocation monitor 取消 run。**未改 A execution producers 或 IPC v1**。預設 `connect()` 沒有 bridge，仍拒絕 protected workspace plans；production root receipt producer／mount pin／cleanup receipt 仍由 B/H integration 提供。

一般獨立 code review 已發現並回修 source-lock recursion、recheck 隱式 migration、channel wrong-home／Personal substitution、DB directory anchor、LINE token grammar 與送出後結果等問題；追加檢查 finite SQL／reminder 後，已讀的新 patch 未留下 HIGH／CRITICAL。這不是 Daybreak clearance，也不是完整 Service 覆蓋證明。

## Round 3 實際驗證

全部使用 `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`；沒有 cargo fmt、gateway／CLI 本機 build、commit、merge、release 或部署。原 worktree 與 rounds 1–2 未提交變更保留；本 session 沒有新增 worktree。

| 驗證 | 實際結果與邊界 |
| --- | --- |
| MCP boundary，binding-store | 93 unit tests passed；Linux target `--tests` typecheck passed。 |
| Memory crate | 最終完整 510 unit tests passed／0 failed／0 ignored；包含 5 個新增 mutex／雙連線 busy wait／末端撤權 rollback／cross-DB detach 測試。 |
| Execution crate | macOS 34 lib tests passed；Linux `--tests` typecheck passed。另 2 Spawnd hookup 與 2 checked transport Linux tests 只有型別檢查，沒有原生執行。 |
| DB crate | 70 unit + 16 SQLite integration passed；2 既有測試 ignored。包括 finite parser、literal injection／不支援語法、精確大小寫與 schema 範圍。Postgres／MySQL 真 server 未測。 |
| Connector actual-source harness | 100 passed；實際 DB／channel／model／computer providers、CLI bound retained-history adapter，以及 channel settings／access／website credential／model registry source。Host sender 與 ComputerUseSessions backend 是不允許成功呼叫的 panic fixtures，不能當作網路／登入活驗。 |
| Service actual-source harness | 253 passed；實際完整 TaskStore production modules 與多數原 tests、CronStore、finite CLI adapters／renderers、channel settings／access、website credentials、reminder persistence／authority map、typing／HTTP。排除需 gateway integration 的 survival tests；部分 audience／config／host-policy／binding 是 fixtures。Approval operations actual source 與 consume path 型別檢查及 SQLite 等待撤權測試，不是完整 approval workflow 驗收。 |
| Google／boot glue actual-source harness | 26 passed；實際 connector selection／registry、Google bridge、gateway／CLI typed boot source，含 source 變更拒絕、owner await 撤權回不確定、binding checkpoint 不重入 product selector與identity錯配。Google sealed owner／credential store／resolver／provider execution 是 fixtures，**未編譯或執行完整 Google owner／OAuth refresh**。 |
| 全部已修改／新增 Rust 語法 | 174 files syn parse passed；僅語法，不是全 crate 型別檢查。 |
| 差異檢查 | `git diff --check` passed。 |

Google Home readonly selection 新增了真實 owner module 的 regression test，但依禁止本機 gateway build 的交辦，此測試本輪**未型別編譯／執行**。CLI readiness／parity source 亦未跑完整 CLI suite；不能用 fixture harness 取代完整 crate build。

原始紀錄：

- `/private/tmp/iso-sec-r3-boundary.log`、`iso-sec-r3-boundary-linux.log`
- `/private/tmp/iso-sec-r3-memory-all-tests.log`、`iso-sec-r3-memory-parent-final.log`
- `/private/tmp/iso-sec-r3-execution-tests.log`、`iso-sec-r3-execution-linux.log`
- `/private/tmp/iso-sec-r3-db-all.log`、`iso-sec-r3-connectors-tests.log`
- `/private/tmp/iso-sec-r3-service-tests.log`、`iso-sec-r3-service-handoff.md`
- `/private/tmp/iso-sec-r3-google-boot-harness.log`、`iso-sec-r3-syntax.log`

Linux acceptance 重新執行 SSH check，遭 sandbox 拒絕（`Operation not permitted`；`iso-sec-r3-linux-host-check.log`）。指定 `gpt-daybreak-blue-latest` 只讀審查再次因 in-process app-server client 初始化被拒絕，沒有可用審查結果（`iso-sec-r3-daybreak.log`）。未嘗試繞過環境限制。一般 code review 的回修／追加檢查不等於 Daybreak GO。

## Round 2 已落地（歷史紀錄）

### Connector 選帳與執行

所有 Connector 路由仍必須經 `isolation_connectors` 的選帳、catalog／binding 與 live recheck。新增不可變 provider registry：每個工具必須同時有相符的產品選帳政策和 executor，重複、不相符或缺件都拒絕 readiness。

具體 executor 原始碼覆蓋 **50／62**：既有 Google 19、Notion 4、GitHub 5，加上本輪 Odoo 18、DB 3、web search 1。這是程式碼覆蓋數，並非可部署或真實供應商通過數。

- DB 依原工具 `source` 選定帳號，保留原查詢驗證、唯讀與列數限制、回應格式。有限 table grant 先取交集，metadata SQL 使用參數化範圍；取得 pool 連線後、真正查詢前再檢查 live authority。
- Odoo 使用被選定的 connector，沿用原 18 個 handler 的 model／action／feature／company／approval gates；DNS、登入／重試、RPC 與 schema snapshot 讀取前重查。未自行啟動未授權的背景 schema 掃描。
- DB／Odoo／native provider 的員工目錄解析保留 ephemeral 員工語義；缺少實際目錄不回退至另一個帳號。
- Search 固定供應商來源、拒絕 redirect／proxy，限制回應大小；結果交由已安裝的 checked E parser。排隊中的解析工作保留 live selection 與本次呼叫的取消狀態。

### Service 撤銷與衍生寫入

保留 round-1 sticky revocation watch，並把整個 isolated request（包括 handler 前後的衍生工作）放入 `io_authority` scope。新增可顯式帶入 spawned／blocking work 的 captured authority。

本輪補強 memory profile／decision／aliases／衍生交易、wiki FTS、向量與 code-map 工作；gateway 的 events、working state、mail、beliefs、approval、task goals／plans／steps、部分任務狀態寫入、reminder、delegation、canvas／agent files 與 channel HTTP 路徑。特別補上 mail blocking work 的 context，以及已讀狀態寫入的拒絕回傳。已改用受檢查交易的路徑，在取得 SQLite 寫入權後與提交前重查，失效則 rollback。

CronStore 的日常寫入、routine 與 migration publish／disable 使用共同 Immediate transaction helper；mutex 與 SQLite busy 等待後、提交前都檢查。`list_cron_tasks` 新增有限集合 adapter，以員工與授權 IDs 同時限制 SQL，與 legacy 清單共用 renderer。既有 `tasks_list`／`activity_list` 有限集合 adapter 保留。

**以上不是所有 Service 的完整覆蓋證明。** 後續仍需全量檢查未移轉的儲存入口、啟動／migration schema 寫入、背景生命週期及每個 spawned work 的 authority。

### Workspace／File tokens 與 worker

`AuthorizedWorkspace`／`AuthorizedFile` 持續保留本次 call、選定資源與實際開啟物件身分。File route 先取得授權 token，再開檔；檔案每次讀取檢查 live authority。ExecutionPlan／WorkerPlan、CSV／XLSX parser 與 gateway worker adapter 傳遞 token，不再於這條路徑提早退化為一般 File。

排隊的 blocking 工作保留本次 invocation guard；原呼叫被丟棄，即使 connection 仍有效，該工作的後續讀取／傳輸仍會拒絕。Worker launcher／session 必須明確支援 live authority；checked transport 在等待與 FD 傳送／接收邊界重查，沒有 legacy exchange fallback。

Execution backend 新增 authorized-workspace 介面，須檢查實際 backend root 與 token 綁定並保留 live authority；預設拒絕。**`SpawndBackend` 尚未完成 root-derived workspace receipt 的正式接線，所以 protected A lane 目前不可用。** IPC v1 未改，不能以測試 backend 成功取代 production 實作。取消或丟棄 worker session 也不代表已取得 root teardown receipt。

### Parse runtime tree 安裝

`crates/duduclaw-cli/src/service/protected_linux_install.rs` 已接上 runtime-tree helper。在有 recorded updater state 的安裝路徑，驗證簽章 manifest 及 worker artifact 的 digest／size／mode 後，以隔離的 Python runtime 建立／驗證 tree。Tree 僅包含 worker、loader 與精確 dependency closure；檢查 ownership、mode、link 與非預期節點，fresh staging 完成後原子發布。既有 populated tree 必須符合預期；不自動破壞舊 tree。

**初次 bootstrap 尚缺 recorded-state 前的正式接點；升級至不同 closure 尚缺與 service stop／替換的協調。** 空目錄不算 readiness，內容不符會拒絕。這不等於 systemd／root sentinel 實機驗收。

## 必須完成的剩餘項目

| 項目 | 精確剩餘 |
| --- | --- |
| 正式 boot caller／帳號 lifecycle | 新 typed assembly／dispatcher entry 尚未被 production binary／integration host 呼叫。必須接入既有 registry lifetime、account provisioning／revoke 及完整 A/E factories，再檢查 admission。Notion／GitHub 仍需 host 提供真正的 live selection policy／credential material；generic vault 本身不是產品選帳證據。 |
| Google 完整產品 parity | 可信 requester 來源及 requester 綁定的 sealed claim；delegate／SA／Apps Script source-owner adapters；真 OAuth refresh、consent／owner 撤銷及 provider output tests。現在 Unknown／不支援 source 的拒絕不能算 parity。 |
| Service 全量覆蓋 | skills／agent 建立與 baseline publication；其餘 background／maintenance／spawn；Memory 的 raw conn_for_maintenance、其餘 maintenance／forget-source／trust／layer 及 schema／目錄初始化；未移轉的 autocommit／read-side 與 Approval operation 全面核對。外部已發出 I/O 無法撤回。 |
| Background durable authority | Reminder 現在重啟／原 capability 過期即拒絕；如產品要繼續執行，仍需 host 重新核准的 durable authority，不能序列化舊 request 當有效憑證。其他延後工作的 scope 必須逐一核對。 |
| Execution production receipt | Hookup／測試已落地；B/H 真正 root receipt producer、實際 work mount 與 generation pin／cleanup readback 未接。只加接點，未修改另一套件的 A producers；預設仍拒絕。 |
| Installer bootstrap／upgrade | 初次安裝 recorded-state 接點、跨版本 runtime tree 的停服與更新協調；Linux root-owned tree／systemd sentinel 驗收。 |
| 整合與安全驗收 | 在允許的 Linux host 完整 build gateway／CLI，執行真 provider、FD／openat2、撤銷／transport、root cleanup 與跨 run egress tests；完整 Service 審查及指定 Daybreak review。 |

Finite `goals_list`／`list_reminders`／`responsibility_get` 與受限 `db_query` 已不再列為缺 executor 的 startup blocker；每個 call 仍須實際資源解析與 adapter 的語法／範圍檢查。複雜有限 SQL 拒絕，EmployeeScope 不擴權。Round 2 的 50／62 數字及下方測試表是歷史紀錄，未列作 round-3 重跑證據。

## 契約變動

Round 1 的 capability canonical encoding **v2** 保留：tag 9 `EmployeeScope`、tag 10 `Confined` 都涵蓋於 MAC，v1 MAC 不沿用；IPC wire version 維持 v1。EmployeeScope 綁定已驗證員工，不能混用 task-limited run 或有限 Service grants；Confined 僅表達伴隨 EmployeeScope 的 File／A／E 限制，不改變 Service／Connector 產品權限。

Round 2 的 Rust API 變動包含 typed FileParser input、ExecutionBackend authorized-workspace opt-in、worker checked exchange 與可傳遞的 Service authority。跨 crate callers 已更新；完整 gateway／CLI 型別整合仍待允許的 build host 驗證。

## Round 2 實際驗證（歷史紀錄）

Cargo 使用 `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`；未執行 cargo fmt、gateway／CLI crate 本機建置、commit、merge、release 或部署。以下獨立 harness 直接包含指定實際原始碼，但以最小依賴或 fixture 取代其他 gateway 模組，不能替代完整 crate build。

| 驗證 | 結果與邊界 |
| --- | --- |
| MCP boundary（binding-store） | 93 unit tests + 1 bridge integration passed；Linux target `--tests` typecheck passed。 |
| memory crate | 505 tests passed；包含衍生資料與 FTS 的 live authority 回歸。 |
| DB crate | 65 unit + 16 integration passed；2 既有測試 ignored。 |
| Connector actual-source harness | 26 focused tests passed；涵蓋實際 selection／DB／Odoo 與 CLI Odoo source slice；並型別檢查 search adapter。未連接真實 provider。 |
| Odoo | 新增 2 authority tests passed。較早完整 suite 為 154 passed／1 failed；既有 localhost socket bind 被 sandbox 拒絕。另一整體 harness 的 4 pool tests 也因此失敗，不能報整套通過。 |
| Service actual-source harness | 167 tests passed；涵蓋實際 storage／HTTP／canvas／goals／plans 等選定模組，不是完整 gateway。 |
| CronStore actual-source harness | 14 tests passed；含有限 owner／grant 查詢、mutex 等待撤銷、兩個 SQLite 連線的 busy 等待撤銷與未發布寫入。 |
| Worker gateway adapter harness | macOS 3 tests passed；Linux `--tests` typecheck passed。 |
| Parse worker | macOS 9 tests passed；Linux `--tests` typecheck passed。新增 3 個 Linux transport 測試只有編譯，未執行。 |
| Runtime tree helper | 12 Python tests passed；不等於 root／systemd 驗收。 |
| Installer actual-source harness | Linux typecheck passed；使用 updater fixture，不等於完整 CLI 或真實簽章更新驗收。 |
| Execution crate | `--lib` check passed；不代表 production authorized backend 已完成。 |
| 最終語法／差異檢查 | 110 個修改／新增 Rust 檔 syn syntax parse 通過；`git diff --check` 通過。語法檢查不是型別檢查。 |

本輪主要原始記錄位於 `/private/tmp/`：

- `iso-sec-r2-boundary.log`、`iso-sec-r2-boundary-linux.log`
- `iso-round2-memory-all-tests.log`、`iso-round2-service-harness-tests.log`
- `iso-sec-r2-db-final.log`、`iso-sec-r2-connectors-final.log`、`iso-sec-r2-odoo-authority-tests.log`、`iso-sec-r2-cron-tests.log`
- `iso-sec-r2-worker-harness.log`、`iso-sec-r2-worker-linux.log`、`iso-sec-r2-execution-check.log`
- `iso-sec-r2-syntax-final.log`
- `iso-sec-parse-r2-tests.log`、`iso-sec-parse-r2-linux.log`、`iso-sec-runtime-tree-r2.log`、`iso-sec-installer-r2-check.log`

Round 1 的 capability／egress／engine 測試紀錄仍保留於原 `/private/tmp/iso-sec-*final*.log`，不列作本輪重跑證據。

Linux acceptance 的 SSH check 遭目前 sandbox 拒絕，本輪沒有繞過。指定 `gpt-daybreak-blue-latest` 審查的 app-server 初始化也遭 sandbox 拒絕（`iso-sec-r2-daybreak.log`）。一般 code reviewer 已複核並修正本輪發現的型別、scope、provider 與交易問題，但不是 Daybreak GO，也不取代原生驗收。

## Round 3 主要新增／修改檔群

- gateway `isolation_connector_{boot,channel,model,computer,database,registry}.rs` 與 tests，`isolation_connectors.rs`；CLI `mcp_isolation_boot.rs`、`mcp_isolation_connector_channel.rs`、collections／readiness／parity；DB `finite_select.rs`／`source.rs`，inference `model_registry/hf_api.rs`。
- gateway `google_identity/isolation_bridge.rs`／tests、sealed owner／home selection／refresh checkpoints、native provider HTTP builders；CLI worker feature forwarding。
- execution `workspace.rs`／tests、`spawnd.rs`／authorized tests、checked transport 與 README；不改 A producers。
- gateway TaskStore／Approval operations、channel settings／access／website credentials、computer control/login、`reminder_isolation.rs`／scheduler、typing；CLI reminders renderer／responsibility person-field helper；boundary captured-authority accessors；memory `decay.rs`／`lifecycle.rs` 的 Immediate transaction／rollback 與測試。

## Rounds 1–2 主要修改檔案群（歷史紀錄）

- `duduclaw-mcp-boundary/src/{resources,execution,worker_execution,parsing,io_authority}.rs`、factories 與測試；`duduclaw-isolation/src/execution.rs`。
- gateway `isolation_connector_{registry,database,odoo,search}.rs`、`isolation_connectors.rs`、worker adapter；CLI `mcp_isolation*.rs`、`mcp/{dispatch,files,odoo,cron_tasks,codrive_mail}.rs`。
- DB `source.rs`、Odoo `connector.rs`；memory engine／wiki／vector／code-map；gateway TaskStore、CronStore、events／mail／approval／reminder／canvas 等 Service modules。
- CLI `service/protected_linux_install.rs`；parse worker client／transport、runtime-tree scripts 與測試。
- 各 crate README、tool matrix、implementation note、CHANGELOG 與本報告。
