# PL-0 安全修正補充（2026-11-05）

## 現行決策（2026-11-05，取代下方歷史全面拒絕方案）

**Codex 外部設定檢查（尚未發佈）：** 外部設定不存在或未定義 MCP 時，一般員工、Explore、utility、Task Sandbox 與 Discovery 可執行；偵測到 MCP 或既有設定無法讀取／解析時，回報來源並拒絕。明確指定的 Codex 判官遭拒時，驗收失敗，不改用預設判官。這是啟動前的本機／快取快照檢查，無法保證後續雲端更新的完整隔離；[來源範圍與限制](../architecture/extension-gates.md)。

目前檢查：Unix `/etc/codex/{config,requirements,managed_config}.toml`；Windows `%ProgramData%/OpenAI/Codex/{config,requirements}.toml` 與原 Codex home 的 managed 層；macOS 以 forced CFPreferences 取得 `com.openai.codex` 的 `config_toml_base64`、`requirements_toml_base64`；profile 檔、cwd 各祖先的 `.codex/config.toml`（成功產生的 gateway proxy 設定除外）；原 Codex home 的 `cloud-config-bundle-cache.json`、`managed-config-*.json` 內嵌 TOML。

限制：只檢查當時存在的本機／快取快照，不凍結檔案或偏好，也不能阻止 Codex 後續遠端雲端更新；這不是完整隔離保證，也未宣稱雲端政策活驗通過。私有 Codex home 透過 Unix symlink／非 Unix hardlink 共用原 `auth.json`，保留 upstream 0.166 原地截斷寫入的 token refresh 行為。


下方全面拒絕 Codex 的修補與測試結果是歷史紀錄，已由此決策取代；不得再當作目前支援範圍。新增／更新的測試仍須由遠端 builder 執行，本機未編譯 gateway／CLI。

狀態：修正實作已落地，驗收 **PARTIAL**。五項 finding 均已處理；H2 採用交辦允許的 fail-closed 替代方案，正常 Codex 員工與 utility 目前均不可用。只修改指定 worktree，沿用 `fix/extension-gates-pl0-20261105`，保留原本未提交變更；沒有 commit、merge、部署、`cargo fmt` 或本機 gateway／CLI 建置。

## 本輪 utility High 與 Grok regression 修補

針對 `20261105-121330-codex-75237-199.md` 的 High，本輪採取 fail closed：刪除 Codex utility command builder，所有 `CodexRuntime::execute` 呼叫在準備檔案、版本探測與 spawn 前回傳同一 MCP spawn-gate 錯誤。`execute_streaming` 沿用此入口。utility 支援清單只保留 Claude 與 OpenAI-compatible API；員工選 Codex 時可沿既有機制使用全域後端與模型（未設定預設 Claude／`claude-haiku-4-5`），不沿用 Codex 的模型。明確 Codex hint、全域 Codex，以及其他員工 fallback 指向全域 Codex，全部拒絕並提示更換設定。底層直接 utility dispatch 也拒絕 Codex。

Grok 保留「unknown transport ⇒ refuse spawn」產品行為，不把缺少 command 的項目當成可安全 shadow。`no_command` 改為獨立拒絕測試；原 shadow 測試使用有效的 HTTP transport 與含空白的名稱，驗證 `enabled=false`、無 URL/env，並保留正常 proxy 對照。拒絕前 host gate 可能修復內建 duduclaw 項目，因此 Grok 測試驗證第三方項目未改與不產生 `.grok`，不宣稱整個原始檔逐位不變。

同類搜尋另找到並修正 Codex `ungateable_entries_are_dropped_never_passed_through` 的 `no_command` fixture。保留兩種不同契約的測試：`redaction_proxy::a_server_without_a_command_is_skipped` 僅測 `tool_gate=false` 的 redaction rewrite；`mcp_external::missing_command_skipped` 測外部 API 設定解析器跳過項目，兩者均未放寬 managed native MCP transport gate。core 的未知 transport 測試已有空物件與缺 URL 等拒絕案例。

新增／調整測試：

- `utility_codex_refuses_before_files_or_spawn`：utility 直接呼叫、空白／明確模型，拒絕且未碰員工檔案或 collector。
- `hinted_team_verifier_and_judge_refuse_codex`：有／無員工的明確 Codex hint 均拒絕。
- `codex_utility_falls_back_but_explicit_or_global_codex_is_refused`：全域 Claude／API fallback 使用正確模型；明確 hint、全域 Codex、其他 runtime fallback 指向 Codex 均拒絕。
- `codex_refused_utility_and_employee_book_no_usage`：直接 dispatch 拒絕且無模型用量。
- Grok `commandless_entry_refuses_spawn_without_runtime_config`、Codex `commandless_entry_refuses_spawn_without_changing_files`；各自保留合法但不可代理項目的 shadow／drop 測試。
- 通用 `partial_timeout_is_refused_by_utility_and_every_non_reply_entry` 使用 OpenAI-compatible mock；原 Codex collector 的 partial timeout 測試保留。

本輪實際驗證：`git diff --check` 通過；獨立唯讀實作審查檢查了 gate、fallback、測試假設與移除 builder 後的引用，修正 Grok fixture 假設後未報告其他 blocker。這不代表 Daybreak 安全複審通過。依交辦未在本機 build 或執行 gateway／CLI Rust 測試，沒有 commit 或 `cargo fmt`。遠端測試與 Daybreak 複審仍待 caller，狀態 **PARTIAL**。

本輪修改的 Rust 檔案（均位於 `crates/duduclaw-gateway/src/`）：

- `runtime/codex.rs`
- `runtime_config.rs`
- `runtime_dispatch.rs`
- `runtime/codex_timeout_usage_tests.rs`
- `runtime/codex/third_party.rs`
- `runtime/codex/third_party/tests.rs`
- `runtime/grok/third_party/tests.rs`

文件：`CHANGELOG.md`、本文件、`docs/architecture/extension-gates.md`，以及下列文件的英文／zh-TW／ja-JP 版本：`features/05-security-defense.md`、`features/13-multi-runtime.md`、`features/16-session-memory-stack.md`、`features/58-night-engine.md`、`features/feature-inventory.md`、`architecture/overview.md`、`guides/mcp-ecosystem.md`、`guides/dashboard-settings.md`。同步移除「Codex utility 可使用受限模式或 CLI 預設模型」的舊敘述。

交給 caller 的本輪遠端測試命令（本輪未執行）：

```sh
cargo test -p duduclaw-gateway runtime::codex::
cargo test -p duduclaw-gateway codex_timeout_usage_tests::
cargo test -p duduclaw-gateway runtime_config::tests::
cargo test -p duduclaw-gateway runtime_dispatch::utility_policy_tests::
cargo test -p duduclaw-gateway runtime::grok::third_party::tests::
```

## 修正與新增測試

| Finding | 最終行為 | 回歸測試 |
|---|---|---|
| H1 | 移除 `RouteAccess` 與 Public；所有擴充 HTTP 一律認證。沒有新增免登入 webhook。 | Admin／Employee 路由無 token 皆 401；無 Origin 也不豁免；認證後仍檢查 Origin 和 2 MiB 本文上限。 |
| M1 | `ExtensionRoute.minimum_role` 為必要欄位，`new()` 預設 Admin；server 外層重讀 DB 角色並強制最低角色。 | 預設 Admin、Admin→Manager→Employee 降權、Manager／Employee 明確允許的路由、舊 JWT 不保留權限、強制改密碼與停權。 |
| H2 | 無可驗證的完整 native MCP 隔離能力，任何 normal／Explore／utility 皆在寫檔、probe、spawn 前拒絕；utility 可轉用全域 Claude／API，明確或全域 Codex 則拒絕。 | command builder flags/base/auth/model 順序；各能力／版本／Explore 皆拒絕且無檔案；被拒絕不記模型用量；詳下節。 |
| H3 | 上游 annotations 最低 Modify，destructive 可升 Delete；舊 trust list 保留解析但不得降級。沒有新增 host-owned 唯讀例外。 | trusted read claim 仍走 Modify 動作規則、Explore 拒絕、工具清單隱藏、approval、snapshot、sink lease。 |
| M2 | 開啟 tool gate 時，未知、混合或 malformed transport 使整個 spawn 拒絕；已知 direct remote 仍移除，需改 managed bridge。 | 兩種 rewrite API 的未知形狀／非字串 command／錯誤 args/env／unknown type；有效 stdio 正常代理；gateway 上層錯誤傳遞且原檔不變。 |

stdio 僅支援 `command`、字串陣列 `args`、字串 map `env`、可選 `type="stdio"`。包含 `cwd`／`shell`／未知欄位的設定需先移除不支援欄位；不再交給下游 CLI 自行解讀。

## 前一輪實際檢查與待驗收

以下為本文件原先記錄的前一輪結果，本輪未重跑，不作為 utility 修補的測試證據：

- `CARGO_BUILD_JOBS=2 cargo test -p duduclaw-core third_party_ --offline`：3 passed。
- `CARGO_BUILD_JOBS=2 cargo test -p duduclaw-core mcp_proxy_rewrite:: --offline`：12 passed。
- `CARGO_BUILD_JOBS=2 cargo test -p duduclaw-core agent_toml:: --offline`：26 passed。
- 合計 41 passed；只有既有 `installation_policy_release` unused import warning。
- `git diff --check` 通過。
- 架構文件來源位置檢查 62 筆、文件連結檢查 7 筆通過；只驗證檔案存在與行號範圍，不代表來源語意或執行驗收。
- 獨立唯讀 implementation review 已檢查實際修正，曾找到 Codex timeout 測試預期與新拒絕行為不符，已修正；最終唯讀 implementation review 未發現尚未修正的具體 blocker。這不代表 Daybreak 安全複審通過。

尚未完成：遠端 gateway／CLI build/test、指定 Daybreak 複審、真實 provider／out-of-tree extension 遷移驗收。未啟動付費模型或讀出憑證。沒有可宣稱成功的正常 Codex 員工隔離啟動。

交給 caller 的遠端測試命令（本機未執行）：

```sh
cargo test -p duduclaw-gateway extension_http_
cargo test -p duduclaw-gateway runtime::codex::
cargo test -p duduclaw-gateway codex_timeout_usage_tests::
cargo test -p duduclaw-gateway egress_registry::tests::
cargo test -p duduclaw-gateway third_party_tools::tests::
cargo test -p duduclaw-gateway redaction_proxy::mcp_config_proxy_tests::
cargo test -p duduclaw-cli mcp_proxy::tests::
```

並保留原交接所列 Rust suites 與正常 gateway／CLI build；此表只補充本輪直接影響的 suites。

## 前一輪修改檔案

以下只列本輪實際修改，未將 worktree 既有的其他變更當成本輪成果。

共 36 份檔案（18 份 Rust、18 份文件）。

| 檔案 | 修改 |
|---|---|
| `crates/duduclaw-agent/src/heartbeat.rs` | 同步 Explore／annotations 註解。 |
| `crates/duduclaw-cli/src/mcp_proxy.rs` | 唯讀 claim 仍需 Modify approval 的測試。 |
| `crates/duduclaw-core/src/agent_toml.rs` | 舊 trust 設定不降級的註解與測試。 |
| `crates/duduclaw-core/src/types.rs` | 舊 trust 設定相容性註解。 |
| `crates/duduclaw-core/src/tool_effect.rs` | 只升不降分類與測試。 |
| `crates/duduclaw-core/src/mcp_proxy_rewrite.rs` | 精確 transport 分類、fail closed 與測試。 |
| `crates/duduclaw-gateway/src/extension.rs` | 移除 RouteAccess re-export。 |
| `crates/duduclaw-gateway/src/extension/http.rs` | 移除 Public、必要最低角色與安全建構子。 |
| `crates/duduclaw-gateway/src/extension/http/tests.rs` | 登入／預設角色／來源／本文上限測試。 |
| `crates/duduclaw-gateway/src/server.rs` | 以 live DB 角色強制路由最低角色。 |
| `crates/duduclaw-gateway/src/server/extension_http_tests.rs` | 真 JWT 降權及明確最低角色測試。 |
| `crates/duduclaw-gateway/src/egress_registry.rs` | 分類、Explore、sink lease 測試。 |
| `crates/duduclaw-gateway/src/third_party_tools.rs` | 分類政策註解與 approval／snapshot／Explore 測試。 |
| `crates/duduclaw-gateway/src/handlers/capabilities_apply.rs` | 舊 trust 設定註解。 |
| `crates/duduclaw-gateway/src/redaction_proxy.rs` | 未知 transport 的 spawn 拒絕與原檔保留測試。 |
| `crates/duduclaw-gateway/src/runtime/codex.rs` | 認證共用 helper、命令 base、early refusal 與測試。 |
| `crates/duduclaw-gateway/src/runtime/codex/third_party/tests.rs` | 手動 shim 測試改驗證正常員工拒絕。 |
| `crates/duduclaw-gateway/src/runtime/codex_timeout_usage_tests.rs` | 被拒絕 normal 不記 usage，保留 utility timeout 測試。 |
| `CHANGELOG.md` | 安全與相容性影響。 |
| `docs/architecture/extension-gates.md` | 路由／分類／transport／Codex 遷移契約。 |
| `docs/features/05-security-defense.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/features/13-multi-runtime.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/features/69-egress-guard.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/guides/mcp-bridge.md` | 同步唯讀宣稱、transport 或 Codex 遷移說明。 |
| `docs/guides/mcp-ecosystem.md` | 同步唯讀宣稱、transport 或 Codex 遷移說明。 |
| `docs/features/zh-TW/05-security-defense.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/features/zh-TW/13-multi-runtime.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/features/zh-TW/69-egress-guard.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/guides/zh-TW/mcp-bridge.md` | 同步唯讀宣稱、transport 或 Codex 遷移說明。 |
| `docs/guides/zh-TW/mcp-ecosystem.md` | 同步唯讀宣稱、transport 或 Codex 遷移說明。 |
| `docs/features/ja-JP/05-security-defense.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/features/ja-JP/13-multi-runtime.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/features/ja-JP/69-egress-guard.md` | 同步安全分類、HTTP 或 Codex 可用性說明。 |
| `docs/guides/ja-JP/mcp-bridge.md` | 同步唯讀宣稱、transport 或 Codex 遷移說明。 |
| `docs/guides/ja-JP/mcp-ecosystem.md` | 同步唯讀宣稱、transport 或 Codex 遷移說明。 |
| `docs/plans/pl0-security-followup.md` | 本輪交接、驗證與 H2 官方來源證據。 |

## H2：Codex 員工啟動已阻斷，完整隔離尚不可用

一般 Codex 員工啟動現在一律回傳 MCP spawn-gate 拒絕，位置在讀寫員工設定、寫入 `AGENTS.md`、版本探測與任何實際 child spawn 之前。Explore 與 utility 也拒絕；不安全的 utility command builder 已移除。這是使用者明示的「無法隔離就拒絕」替代方案，**不能宣稱 Codex 一般員工現在有可用的完整隔離**。

保留的 command builder 會先加 `--ignore-user-config`、`-c mcp_servers={}`，再加入受管理 server；明確的 `-m`、`CODEX_HOME`、`OPENAI_API_KEY` 仍保留，只有被阻斷的一般員工 builder 保留認證 helper。不複製憑證，也不改指向暫存 `CODEX_HOME`。但目前沒有任何版本可通過 production 隔離閘門，builder 本身不構成隔離證據。

實際唯讀查證：本機 `codex --version` 為 `codex-cli 0.166.0`；`codex exec --help` 的 `--ignore-user-config` 說明明確寫出 auth 仍使用 `CODEX_HOME`。沒有執行付費模型、登入或讀出憑證。CLI help 同時有 PATH aliases 建立受限警告，命令 exit 0；此處只採用 help/version 輸出。

官方原始碼證據（既有 utility 文件引用的固定 revision `970111f6cb62cc02fe36a82ea7d71b37a2889c97`）：

- [exec/src/lib.rs](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/exec/src/lib.rs#L366-L375)：一般 exec 的 loader override 只設 `ignore_user_config`；沒有設 `ignore_project_config`。worktree 特定分支另設後者，不能當成一般 exec 的保證。
- [config loader](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/config/src/loader/mod.rs#L277-L326)：user layer 可略過，project layer 仍由獨立開關控制；system 與 managed layers 亦存在。
- [config merge](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/config/src/merge.rs#L90-L143)：table 以 key 遞迴合併，空 `mcp_servers` 不會移除其他層的 server。

新增／調整測試：command builder 的旗標、model/auth 環境與 managed overrides 順序；0.164／0.165／0.166／未知版本、所有 sandbox capability 與 Explore 的啟動拒絕且零員工檔案副作用；原 employee permissions 測試改直接測其 helper，避免被更早的隔離拒絕遮蔽；既有手動 shim 測試改期待拒絕；normal 與 utility accounting fixture 均改驗證拒絕且零 usage，低層 collector 覆蓋保留；通用 partial-timeout 測試改用支援的 API mock，避免被更早的 Codex 拒絕遮蔽。依交辦限制，這些 gateway 測試沒有在本機編譯或執行，待遠端 builder。


## H2 follow-up：容器內 Codex 同樣拒絕（2026-11-05）

來源：`/Users/lizhixu/.baton/runs/20261105-125046-codex-75237-210.md` 的 High。Task Sandbox／Discovery 原本直接使用 `RuntimeFamily::argv`，避開一般 `CodexRuntime::execute` 的隔離拒絕；私有 HOME、空 MCP table 與事件串流監看無法證明 image 的 system／managed MCP 設定沒有在副作用前被載入。

修補：Task Sandbox `preflight` 回傳固定的 `codex_mcp_isolation` 拒絕；dispatch 在預算、路由、帳號與容器準備前終止，`run_unsandboxed` 不能略過；`settle` 對相同拒絕也不走退路。Discovery 的 `RuntimeFamily::parse` 拒絕 Codex，因此 runtime admission、catalog、公開 create 與 factory 共用拒絕；直接構造的 container runner 在讀取 budget／工作區之前重新檢查。共用 `argv` 改為 `Result`，Codex 分支只回傳既有 MCP spawn-gate 拒絕，不再產生 `codex exec`。Claude／Grok／OpenAI-compatible 保留原 argv。

本次程式檔案：

- `crates/duduclaw-gateway/src/runtime/codex.rs`：共用拒絕文字補入 Task Sandbox／Discovery。
- `crates/duduclaw-gateway/src/task_sandbox.rs`：預檢、dispatch、settle 與 argv 錯誤傳遞；加入 dispatch 零檔案副作用測試。
- `crates/duduclaw-gateway/src/discovery/attempt_adapter.rs`：parse／argv 拒絕。
- `crates/duduclaw-gateway/src/discovery/agent_spawn.rs`：runner 最前端防禦及 argv 錯誤傳遞。
- `crates/duduclaw-gateway/src/task_sandbox/tests.rs`：Codex 優先拒絕與兩種 unavailable policy；通用測試改用 Claude。
- `crates/duduclaw-gateway/src/task_sandbox/tests_runner.rs`：scripted Docker 記錄所有呼叫，Codex 拒絕時零 Docker 呼叫、零 sandbox／憑證檔；舊 Codex step-limit 執行測試改為拒絕，auth redaction 改用 Grok。
- `crates/duduclaw-gateway/src/task_sandbox/tests_docker.rs`：移除已無效的 Codex 真供應商啟動測試；示例改用 Grok。
- `crates/duduclaw-gateway/src/discovery/tests_attempt_adapter.rs`：parse／argv 拒絕，合法家族 argv 斷言保留。
- `crates/duduclaw-gateway/src/discovery/tests_agent_spawn.rs`：runtime parity 排除 Codex、直接 runner 在缺少 home／workspace／帳號／budget 時仍優先拒絕且零檔案；帳號輪替、OAuth seat、unparsable-stream 測試改用 Grok；既有純串流解析／guard 測試保留。
- `crates/duduclaw-gateway/src/discovery/tests_service.rs`：已設定 Codex image 也不在 catalog，create 拒絕且不新增 task；Grok 正常建立的 control 保留。

文件：`CHANGELOG.md`；本檔；英／繁中／日文的 `docs/features/{05-security-defense,13-multi-runtime,69-egress-guard,60-discovery}.md` 與 `docs/guides/task-sandbox.md`（語言副本在 `zh-TW/`、`ja-JP/`）。修正 Discovery 四家族／Codex credential／step-limit 舊敘述與可用 image 範例，Task Sandbox 的 Codex 活驗改明列歷史結果。

### gateway／CLI 同類 argv 掃描結果

掃描整個 `crates/duduclaw-gateway`、`crates/duduclaw-cli` 的 `codex`／`Codex`、`exec`、`Command::new`、`RuntimeFamily`、`which_codex`，並追查下列路徑。這是來源盤點，不是活體驗證。

| 路徑 | 結果／界線 |
|---|---|
| `runtime/codex.rs::employee_base_command` | 仍保留 `codex exec` builder，但 `CodexRuntime::execute` 第一個動作即拒絕；streaming 共用 execute。utility 無例外。 |
| Task Sandbox／Discovery `RuntimeFamily::argv` | 本次移除 Codex argv；parse／preflight／direct runner／argv 均拒絕。`task_sandbox/container.rs` 僅接收 argv，不另建 Codex 指令。 |
| `runtime_config.rs`、`runtime_dispatch.rs`、`dispatch_engine/judge.rs`、team／ephemeral | 一般執行到 CodexRuntime 拒絕；utility 只允許 Claude／OpenAI-compatible。明確 Codex judge/verifier 拒絕；既有 Goal Acceptance 上層可記 warning/audit 後用預設 utility 重試，本次未改此降級策略。 |
| CLI `eval/runner.rs`、`eval/verifier_cell.rs`、`eval/matrix.rs` | Codex case／verifier 透過 `run_agent_prompt`／共用 runtime；直接 subprocess builder 是 Claude，不另建 Codex exec。benchmark／matrix 未找到另一個內建 Codex argv builder。 |
| `ccr_runtime.rs`、CLI `ccr_run_cmd.rs`／`ccr_eval_cmd.rs`／`ccr_compare_cmd.rs` | CCR 為 provider/tool-loop／HTTP API 路徑，未找到 Codex CLI argv。 |
| `runtime/codex_sandbox.rs` | 保留 Linux 診斷 `codex sandbox … -- /bin/true`，不是模型 exec。 |
| `runtime/codex.rs` version probe、`runtime_models.rs`、`handlers/runtime_models_rpc.rs`、`handlers/doctor_util.rs`、`server.rs` | 保留版本／help／模型與沙箱診斷；不送委派 prompt。 |
| `cli_auth.rs`、`runtime_install.rs` | 保留 operator 登入／登出／安裝／版本操作，不是員工 exec。 |
| `runtime/generic_cli.rs`、`runtime/mod.rs` | Codex 是 bespoke runtime，內建 Codex 不走通用 print-mode builder。 |
| `judge_mode.rs::ExternalJudge`、CLI `tooling ns-guard-exec` | 保留操作者指定的任意命令；操作者可自行指定 Codex 或 shell wrapper，這些不是內建 Codex argv，未新增字串黑名單，也不宣稱此任意命令面有 MCP 隔離。 |
| `codrive/registry.rs`、其他 `Command::new` 相符項 | codrive 為固定 GUI 命令模板；其他相符項為 Claude／Grok、測試 fixture 或一般程序，未找到另一個內建 Codex 模型啟動 builder。 |

### 驗證狀態

- `git diff --check`：通過。Python 靜態斷言亦通過：拒絕先於 budget／workspace、容器 Codex argv 已移除、其他家族 argv 與 HEAD 一致、三語 Discovery 不再提供 Codex image 範例。這些不是 Rust 測試。
- 已逐一檢查共用 `argv()` 的兩個 production callers 與測試 callers，均處理 `Result`；獨立唯讀 code review 發現的支援家族測試及 budget gate 順序問題已修正。
- 依本次交辦限制：未 commit、未執行 `cargo fmt`、未在本機 build gateway／CLI，也未執行需要編譯這兩個 crate 的 Rust 測試。新增／調整測試尚待授權 builder 執行，不能宣稱 High 已經動態驗證解除或 release GO。
- 建議 builder 執行：`cargo test -p duduclaw-gateway --lib --no-default-features task_sandbox::`、`cargo test -p duduclaw-gateway --lib --no-default-features discovery::`（不含 ignored 真供應商測試）。Daybreak Blue 安全複核尚待派工，這次沒有代替其結論。
