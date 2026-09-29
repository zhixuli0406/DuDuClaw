# DuDuClaw 全功能盤點與去留決策表（Jev 式型別化決策）— 2026-09-29

> 目的：列出現有全部功能，用 dudu-exo「數」層的 Jev 模式（`choice`／`noul`／`score` 型別化問題附機率、證據等級 L0–L2、固定閘門）標出「過時／可被取代／無用／過度工程」候選，**去留由使用者決定**。本檔只盤點，不改程式碼。
> 方法：5 隻獨立盤點 agent 依領域唯讀掃描（A runtime／B 記憶演化／C 通道 UX OS／D 治理企業／E CLI MCP 文件表面），每個功能一個 state（入口、接線狀態、最後實質變更、測試、文件、重疊者、規模），對它問固定題：
> - `choice verdict ∈ {保留, 簡化, 取代, 淘汰}`（機率分布）
> - `noul obsolete`／`replaceable`／`unused`／`overengineered`（各 0–1）
> - `score value`／`cost`／`removal_risk`（1–5）
> - `level`：L2 程式碼實錘（零呼叫端、`#[cfg]`、文件明寫 removed/shelved）、L1 多訊號一致、L0 判斷；**低於 L1 為 advisory**。
> - 去偏：每項先想最強保留理由再想最強淘汰理由（等價於 Jev 循環位移去位置偏差）。
> 閘門（協調者統一過）：`irreversible`（移除是否一週內難回復——多數靠 git 可逆，但發行物／資料格式／付費層承諾不可逆）、`sunk_cost`（是否只因投入多而留）、`risk`（移除對付費客戶／實驗容器／CI 的風險 1–5）、`satisfied_1y`（一年後會慶幸做了這個決定）。
> 入選門檻：verdict argmax ≠ 保留且 confidence ≥ 0.5，或任一 noul ≥ 0.6 且 level ≥ L1。
> 分版原則（記憶 edition-split-moat）：免費核心不閹割、鎖 quota 不鎖能力——付費層功能不因「免費版用不到」而判無用。

## 0. 一眼結論

- 盤點 5 領域、約 200 個功能列（附錄含全部總表）。工作樹 `+87,521／−17,109`、110 個 untracked：其中約 **8.2 萬行是 2026-09 的三條實驗線**（CCR／因果證據圖／決策孿生）＋ **1.7 萬行 Team-as-Agent**，全部**未進 release、預設關**。這兩件是本次最大的去留決策（X1、X2）。
- **L2 實錘可直接刪的死碼約 1.1 萬行**（零呼叫端／從未編譯／從未出貨），風險 1，全部靠 git 可逆（T1）。
- **8 處「文件或 UI 宣稱的功能實際不存在或未接線」**（T0）：三階段安全 hook 已刪、Governance Layer 零 enforcer、Durability crate 已刪、Odoo 事件同步零接線但 UI 開關預設開、Identity Notion 不在執行路徑、Google Workspace 前後端閘門不一致、browser 五層路由器死碼、多處文件數字過時。這些不是去留題，是「補實作或修文件」，且依專案規範「過時文件比沒有文件更糟」應最優先。
- **standby／保險／預設關且無使用痕跡的大面積**（T3）約 4 萬行：PTY pool 整體（前提已暫停 15 個月且本身有跨對話洩漏）、RFC-26 Live Forking、GVU legacy SOUL 演化、`python/` PyPI 套件（與 Rust 脫鉤）、JitRL、Exo／MLX／mistral.rs／llamafile 四個本地推論後端、`duduclaw proxy`、docuseal、agentcompanies 匯出、App compat、`duduclaw-shell`＋`-comp` 8.9 萬行零 CI。
- **過度工程訊號**（T5，建議收斂不建議刪）：四套 failover、三套包格式、兩套 wiki API、四個建任務入口、四份同形通知模組、11 個超過 800 行上限的巨檔（`mcp.rs` 32,570）、245 個 MCP 工具 schema 固定 token 成本、11 個只寫不讀的演化旋鈕。
- 出廠板模寫死 `gvu_enabled = false`：**出廠 agent 一個都不會演化**（3.1 萬行演化子系統對出廠使用者不生效）——這與產品敘事是否一致，需你拍板（K4）。

### 讀表方式
每列：`verdict`＝{保留／簡化／取代／淘汰} 機率（總和 1）；`obs`／`rep`／`unu`／`ove`＝過時／可取代／無用／過度工程（0–1）；`val`／`cost`／`risk`＝價值／維護負擔／移除風險（1–5）；`level`＝證據等級；閘門：`irrev`（移除一週內難回復？多數靠 git 可逆，發行物與資料格式除外）、`sunk`（只因投入多而留？）、`1y`（一年後慶幸做了這決定？）。**建議欄是我的建議，去留由你決定**；回覆時用 ID＋選項字母即可（例：`X1 C`、`S1 B`、`T1 全淘汰`）。

## 0.5 使用者拍板（2026-09-29）與執行波

| 範圍 | 拍板 | 執行 |
|---|---|---|
| X1 CCR＋因果圖＋決策孿生 | **三線全留**；無真實資料 → 四條真資料方案全做：① 任務板 `tasks.db` → 決策孿生 ② 稽核紀錄 → 因果證據圖（零 LLM，AEE 為消費者）③ CCR 自家高輸出工具實測後定預設 ④ Odoo Helpdesk／Project 轉接器；另加示範模式標示與落日條款（2026-12-31 前無真實試點則封存）＋`[decision] enabled` kill switch | P5（示範標示、落日、kill switch）＋P7（四條資料線） |
| X2 Team-as-Agent | **預設開**（使用者：「這是新功能／替換舊架構，不應預設關」）；安全條件：角色缺→cascade→同家族→Solo 降級、預算不足→Solo | P6 |
| T0 幽靈（G1–G8） | 全照建議 | P1（G1／G2／G3／G7／G8）、P2（G4／G5／G6） |
| T1 死碼（D1–D20） | 全淘汰（D12 反向接上 budget） | P3a／P3b／P2 |
| T2 半死（H1–H13） | 全照建議（H1 移除、H2 修 bug、H3 接線、H4 收斂、H5 查證、H6 補文件、H7 CI、H8–H10 修 bug、H11 接 composer、H13 補測試） | P4／P2／P3c／P6 |
| T3 standby（S1–S20） | 全照建議（S1-B、S2 留、S3／S4／S5／S9／S11／S13／S14／S17 淘汰、S6／S8 留、S7-A、S10 隱藏、S12-B、S15／S16 移到 DuDuClaw-OS、S18 砍 Qwen、S19 待 S16、S20 補開關） | P3a／P3b／P3c／P4／P5／P6b |
| T4 | 見 X1／X2 | — |
| T5 過度工程（O1–O16） | **全部收斂**（含改對外名稱者）；棄用政策由協調者定：舊名保留兩個 minor 版本（v1.68.0 移除）、MCP 舊工具 description 標 deprecated 且仍列於 tools/list、CLI 舊名 hidden alias、dashboard 只用新名、對映表落 `docs/guides/deprecations.md` | P8a／P8b／P8c（內部）、P9a／P9b（對外）、P10a–c（巨檔拆分）、P11（tools/list 裁剪） |
| K1 | 程式維持 true，文件改 | P5 |
| K2 | 出廠開 AEE | P3c |
| K3 | 補全 example.toml（保留分層） | P5（P4 加 `[night]`、P6 改 `[team]`） |
| K4 | NER 明講 | P5 |

執行紀律沿 2026-09-28 審查波：多 agent 並行實作、協調者串行全量驗證；不 commit；CLAUDE.md 與 docs/features/03 單一寫入者（P1），其餘 agent 以 fragments 交接。

## 0.6 執行結果（2026-09-29，逐波；全部未 commit）

### 第一階段（P1–P6 並行，協調者串行全量驗證）

| 波 | 狀態 | 重點 | 執行中推翻的稽核前提／刻意偏離 |
|---|---|---|---|
| P1 幽靈與治理 UI | COMPLETE | G1 三語 `05-security-defense` 改寫成四道現役守衛、移除 4 處懸空 `DUDUCLAW_BROWSER_VIA_BASH=1`；G2 Governance 頁／RPC／i18n 約 50 鍵刪除；G3 docs 22 刪；G7 `browser_router.rs` 438 行刪；G8 約 20 處數字與敘述（三語 11／14 整篇改寫、三份已解決 TODO 移 `wiki/reports/resolved-todos/`）；另刪 `security.status` 的 `credential_proxy`／`mount_guard` 假資料 | `live-forking.md` 併入 28 SKIPPED（另一 agent 已改寫成姊妹篇） |
| P2 接線補完 | COMPLETE | G4 `odoo_events.rs`（輪詢＋`/webhook/odoo`，預設全關）；G5 `identity_provider.rs` 三呼叫點統一；G6 Google 旗標改讀後端；D12 cost anomaly 接進 `budget::check_agent_budget`（在 `is_inert` 之前）；H8 github 預設關；H10 `data-file-guard` Rust 化 | H9 前提錯：pharmacy-pro slug 本就相符，真缺陷是三種靜默丟棄（已改 warn）；G4 `OdooConnectorPool` 在 cli 層 gateway 拿不到，改全域 `[odoo]` 憑證 |
| P3a 死碼（llm／inference） | COMPLETE | 28 檔 6,978 行整檔刪＋檔內約 46 KB；`Cargo.lock` 掉 `llama-cpp-2`／`mistralrs-core`；S7-A 拔 post-hoc 後補回「無校準閘即接受本地答案」的 fail-safe（自己引入的 bug，附回歸測試） | `BackendType::LlamaCpp/MistralRs` 列舉保留供舊 `inference.toml` 解析（回 `BackendUnavailable`） |
| P3b PTY pool 縮編與雜項 | COMPLETE（D13 4/5、D19 部分） | 整檔刪 17,509 行＋檔內約 2,600 行；保留 `oneshot_pty_invoke`；S13／S14（匯入方向保留）／S17／S18（Copilot 保留）／S10／D17（8 個 flag 改「服務承諾」，**行為變更**：`FeatureGate::check()` 一律拒絕它們）／D18／D20 | `vetting.rs` 保留（`graduate_skill_to_disk` 的第二道閘）；`clear-holdout-rotation` 改隱藏（唯一能清 AEE 旗標）；`python/spikes/` 未進版控、不動；`channel_reply` API-key oneshot 分支因唯一入口是 PtyPool 一併刪 |
| P3c GVU legacy／K2／H3 | COMPLETE | gvu 淨刪 9,140 行；AEE 全套保留；`EvolutionCommands::Finalize`、`evolution.history/versions/consolidations` RPC、docs 02／06 刪；K2 三個 scaffold＋4 板模 `gvu_enabled = true` + `strategy = "balanced"`；H3 接 4 旋鈕、刪 8 個 | AEE 兩處行為變更：prompt 移除 `## Version lineage`、`novelty` 語料改 playbook `failure_history`；`skill_synthesis_enabled`／`skill_trial_ttl` 保留（P4 新的 `synthesis_runner.rs` 在讀） |
| P4 半死接線 | COMPLETE | H1 `credit.rs`＋CLI 命令群刪；H2 合成鏈三處斷裂修；H4 13 條路徑收斂 `memory_factory`；H6 docs 58＋`[night]`；H7 修 `--features otel` 在 macOS／Windows 從未編得起來（五個依賴在 linux-only target 區塊）；H13 9 個整合測試；S8 docs 59；S20 miniapp 開關 | H5 稽核表事實錯：finetune 11 個 RPC 前後端全接好，未改碼 |
| P5 文件設定與 Python | COMPLETE | K1 12 處文件；K3 example.toml 253→849 行、57 段 403 條註解預設值＋3 個測試（順手修三個真錯：`[evolution]` 六鍵只在 agent.toml 生效、`api_key_encrypted` 鍵名不存在、`[logging] level` 零讀取端）；K4 NER 限制固定顯示；S12 刪 `channels/`＋`sdk/`、memory_eval 移出 wheel；X1-⑤ `DemoModeNotice` 三頁；X1-⑥ `decision_gate.rs`＋落日條款 | `[decision] enabled = false` 只擋 HTTP 面，SPA 頁與導覽仍在（待拍板是否隱藏） |
| P6 Team 預設開／H11／CI | COMPLETE | X2 `is_enabled()` 預設 true、`cascade_unbound_roles`、`FreezeOutcome::SoloByDefault`（不寫稽核列）、`budget_forces_solo`；H11 `resolve_member_model` 優先序：明寫 → 矩陣先驗（同 runtime、resolved 格、同家族、平手拒絕）→ 員工 preferred；otel CI 步驟合併 | `gate = "auto"` 現況只有 `bulk`／`long_horizon` 可量測，灰帶不會真的成團（待拍板是否把矩陣接進 `capability_gap_pp`） |
| 協調者收尾 | COMPLETE | 併入 P3b 的 CLAUDE.md 片段；移除 P6 的 otel CI Linux-only 守衛（P4 已修根因，macOS 實跑通過）；`EditAgentPage` 本地後端下拉只留 `openai_compat`（三語 i18n、`handlers.rs` 驗證器同步）；`server.rs` 關機序列結構測試改以 drain handover 為界（worker supervisor 已刪） | — |

**第一階段串行全量驗證（協調者實跑，`scratchpad/pA_final.log`）**：`cargo check --workspace --all-targets` PASS；`cargo check -p duduclaw-gateway --no-default-features --features otel` 在 macOS PASS；lib：core 749、llm 269、cli-runtime 18、agent 250、security 306、memory 364、inference 81、odoo 47、identity 42、license 111、container 9、db 63、redaction 366；**gateway lib 6,855／0 失敗／12 ignored**（修正關機序列測試後）、gateway 整合測試 16 組全綠、doctest 綠；**cli lib 1,615**、整合測試 60、doctest 綠；web `tsc -b` 0、vitest **256 檔 2,064**；三語 i18n **6,491** 鍵一致。無活體驗證。

### P6b（S15／S16，跨 repo）

- **兩個前提被推翻**（執行中查證）：① S15 稽核表寫「runner 只回報不執行」——錯，OS 殼 `windows_vm.rs:250` 直接 spawn `duduclaw compat windows-vm app`，OS recipe `windows-vm.toml` 以它為 `entrypoint`；OS image 的二進位是主 repo 快照建的，直接刪＝OS 的 Windows RemoteApp 失效。**改為 Cargo feature `app-compat`**（平台預設不編、OS recipe 開、主 repo CI 加 feature check 防 bit-rot），文件三語搬 OS repo。② 三個殼 crate 本來就是 `[workspace] exclude`、各自 detached lockfile，主 `Cargo.lock` 不含它們；shell→native-gui 是同層 path 依賴且 bitbake recipe 就是照此快照——**整組搬到 `DuDuClaw-OS/crates/` 但不合併 workspace、不改版本、不動 lockfile**。native-gui 的桌面 workflow（tag `native-gui-v*`，從未打過）與打包腳本一起搬。
- **OS repo 端（未 commit）**：三 crate 搬入 `crates/`；三支 `refresh-src.sh` 判準改寫（shell／comp 讀本 repo、覆寫變數 `DUDUCLAW_OS_SRC_ROOT`；cli 錨定 `crates/duduclaw-cli/Cargo.toml` 避免誤判）；`sync-platform.sh` 拆平台 recipe／OS 自有 recipe 兩組；`platform-sync.md`、README、CLAUDE.md、CHANGELOG、`.gitignore`（`crates/*/target/`）同步；`app-compat.md` 三語搬入；cli recipe 的 `--features app-compat` **先註解掉**（vendored 快照還沒有這個 feature，開了 bake 會死），sync 指南寫明對齊時必須打開。已知殘留：native-gui `screens/governance.rs` 仍呼叫已刪的 `governance.list`、`screens/security.rs` 仍渲染已刪欄位；本機缺 Metal Toolchain 無法編譯驗證，列待辦。
- **主 repo 端（P6b-B，COMPLETE）**：core／cli 各加 feature `app-compat`（cli 轉發 core），`compat_runners`／`compat_cmd`／`compat_windows_vm`／`OpsCommands::Compat` 與兩個 enum 全加 `#[cfg(feature = "app-compat")]`；回歸測試 `compat_feature_gate_tests` 兩方向；**活體驗證**：預設 build 的真 binary `duduclaw compat list` 回 unrecognized subcommand、`--features app-compat` build 有 `compat {list,windows-vm}`。根 `Cargo.toml` `exclude` 三條刪；native-gui workflow 與打包腳本刪（OS repo 有副本）；`sign-notarize-macos.sh` 保留（Tauri 文件仍引用）；`release.sh` 註解更新；`app-compat.md` 三語刪、`docs/README.md`＋三份 README＋docs 52 三語＋兩處來源註解改指 OS repo；CHANGELOG Removed×2、Changed×1。`cargo check --workspace --all-targets` PASS、`--features app-compat` check／tests PASS、`cargo test --features app-compat -- compat` 38＋core 11。副作用：feature 關掉後 48 個 compat 測試不進預設測試迴圈 → 協調者把 CI 那步從 `check` 改成 `test`（core＋cli 各一條）。

### 第二階段（P7、P8a、P8b、P8c、P9a、P9b 並行；P6b-B 同時收尾）

| 波 | 狀態 | 重點 | 執行中推翻的稽核前提／刻意偏離 |
|---|---|---|---|
| P7 X1 四條真資料線 | COMPLETE（CCR 四臂實測與 Odoo 活體 SKIPPED） | ① `decision_task_board_export.rs`＋`decision_task_board_shadow.rs`（`[decision] task_board_shadow` 預設關，30 測試）② `causal_audit_ingest.rs` 零 LLM 從失敗 settle＋`tool_calls.jsonl` 產 candidate claim（`[causal] audit_ingest` 預設開、50/agent/日）＋AEE `causal_support` 維度**不進 commit 閘**、`[evolution] require_causal_evidence` 走 shadow-rule ③ `[ccr] builtin_sources` 預設開（11 個自家工具）④ `duduclaw-odoo::support_export`＋`decision_odoo_export.rs`（helpdesk.ticket／project.task，mapping 測試） | 本機零 `_API_KEY`、無帳號 → CCR 實測 SKIPPED，`[ccr] enabled` 維持 false，方法與判準在 `wiki/reports/ccr-real-measurement-2026-09-29.md`；`source_version_hashes` 必須等於 canonical sha（tasks.db 摘要改放 `source_lineage`）；shadow 計分不代人登記 policy（只寫 nowcast）；odoo→gateway 依賴方向（odoo 產同形 `SupportQueueExtract`）；CLI 為扁平 `decision-task-board-export`／`decision-odoo-export` |
| P8a 通知／failover／openai 去重 | COMPLETE（dispatcher 第四份 sender SKIPPED） | O5 `notify_push::push(card, dest)` 收四份 notify，授權邏輯一行未動，`send_with_markup`／`send_plain_text` 移 `channel_sender.rs`；O1 `failover.rs` 檔頭三層邊界表＋`failover::model`（原 `llm_fallback.rs`），測試 35→38；O11 inference 的 514 行 client 刪，`ChatRequest` 加 `top_p/stop/logprobs/top_logprobs`、`complete_with_logprobs()`，薄殼帶 300s 逾時＋`with_verbatim_model_id()` | `dispatcher::forward_to_channel` 是委派轉送（thread id／parse mode／分段預算），`push()` 承載不了，另立項；線路差異兩項寫進 CHANGELOG（`"stream": false` 明寫、`top_p` f32→f64） |
| P8b goal_loop 目錄／org guard／mcp_auth／tick preset | COMPLETE | O8 八個 `goal_*` 小模組收進 `goal_loop/{signals,state,plan}.rs`（`lib.rs` 8 個 `pub use` 別名，120 個呼叫端不動，130 測試前後相同）；O9 `org_field_guard/` 三份 `diff_*` 收成 `FROZEN_FIELDS` 表＋`matcher::diff_frozen`，82 golden 原樣＋2 新；O14 `mcp_auth/{scope,grants,strategy}.rs`，測試 22→14（刪的 8 個只斷言已刪的 P2 佔位策略）；O15 `[tick] preset = conservative\|aggressive`（raw TOML 判 presence，顯式鍵永遠贏） | `goal_loop.rs` 本體不搬成 `mod.rs`（P8a 同時在改它的呼叫點）；82 測試不改寫成表驅動（測試數不減優先） |
| P8c os_ops 單一權威 | COMPLETE（docs 51 刻意不動） | `os_ops.rs` 821 行；MCP 25 工具名／RPC 34 方法名 diff 皆空；CLI 21 葉變體名零差異；三面只剩 gate→parse→call→render；跨三門等價 golden 測試；權限閘全留各入口 | 稽核表「CLI 25 葉」是與 MCP 撞號的筆誤（實際 21）；`os_notify` 等感知工具、`os.*` 頁 RPC、`system.apply_update` vs `os_apply_update` 刻意不收斂（理由在模組 doc） |
| P9a 對外名稱收斂 | COMPLETE | O3 `wiki_*`＋`scope`（`shared_wiki_*` 六個別名同 handler，`mcp_alias.rs` fail-closed）；O4 `tasks_create kind/schedule`（`goal_create_core.rs` 抽出、C3 委派閘在合併入口強制一次）；O13 `skill_search source`；O12 `evaluator_only`／`human_only` 棄用（讀到 warn 一次、寫入審計、下拉只剩兩值）；O10 `migrate/export/acp` 收斂、舊名 hidden alias；`docs/guides/deprecations.md` 三語 | `create_task`（多步驟計畫）、`goals_create`（階層節點）、`shared_wiki_delete`（無 agent-local 孿生）刻意不標棄用；`docs/guides/mcp-tools.md`／`cli.md` 不存在、208 個 help snapshot 不存在（改 5 個 clap 回歸測試） |
| P9b pack 統一 | COMPLETE | 設計文件 `commercial/docs/DESIGN-pack-format-unification-2026-09.md`（L3）；`duduclaw-core/src/pack.rs` 四個 legacy loader（稽核表以為三種）＋22 測試；`duduclaw pack {list,install,inspect}` 隔離 home 活測；`expert install` 改走同一 `install_pack`；順手堵真安全缺口（`expert install ./templates-premium/…` 先前無 premium gate） | `templates.*` RPC 是引導流程不是裝包、`preset install-builtin` 批次語意，皆不收斂；`pack.rs` 992 行超 800 上限（併入 P10c 拆）；`experts/lawfirm-team/expert.toml` 是過期快取，commit 前重跑 `duduclaw expert convert-teams` |
| 協調者收尾 | COMPLETE | 併入 P7／P8a／P8b／P8c／P9a／P9b 六份 CLAUDE.md 片段（含棄用政策條目）；13 處 `goal_*` 過時註解路徑改 `goal_loop/…`；`deprecations.md` 三語補 pack 對映；清掉 `git mv` 殘留的 staged rename | — |

**第二階段串行全量驗證（協調者實跑，`scratchpad/pB_final.log`）**：`cargo check --workspace --all-targets` PASS；otel feature check PASS；lib：core 764、llm 274、cli-runtime 18、agent 250、security 306、memory 364、inference 82、odoo 63、identity 42、license 111、container 9、db 63、redaction 366；**gateway lib 6,960／0 失敗／12 ignored**、整合測試與 doctest 綠；**cli lib 1,615**、整合測試與 doctest 綠；web `tsc -b` 0、vitest **256 檔 2,068**；三語 i18n **6,497** 鍵一致。無活體驗證（P7 的 CCR／Odoo、P9b 的 pack CLI 有隔離 home 活測，P6b-B 有真 binary 活測）。

### 第三階段（P10a、P10b、P10c 並行拆巨檔；P11 接 P10a 之後）

| 波 | 狀態 | 重點 | 例外／判斷 |
|---|---|---|---|
| P10a `mcp.rs` | COMPLETE | 33,099 行 → `mcp/` 79 檔；cli lib 測試 1,615 = 1,615；字串字面值多重集比對 MISSING 0／EXTRA 0；`cron` 子模組改名 `cron_tasks` 避免遮蔽外部 crate | 5 個超長測試模組拆成目錄，測試路徑多一層 `partN`（數量不變）；`tools_def.rs` 4,252 行留給 P11 |
| P10b gateway 三檔 | COMPLETE | `dispatch_engine/` 16 檔、`decision_store/` 32 檔、`channel_reply/` 24 檔；測試 128／28／127 前後相同；`decision_sla_shadow_screen.rs` 的引擎指紋改對 32 個子檔逐一 hash（覆蓋面不變） | 4 檔仍超 800 行，各只裝一個單一函式（`inner.rs` 3,530／`tests/shadow_forecast.rs` 1,764／`review.rs` 1,020／`cli_env.rs` 993），不動函式體拆不了 |
| P10c 八檔 | COMPLETE | `pack/`、llm `ccr/`（7 檔＋tests）、`tool_loop/`（6 檔＋tests）、agent `account_rotator/`、gateway `task_store/`、`goal_loop/`（併 P8b 目錄）、`approval/`、`ephemeral/`；各模組測試數逐一相同（llm 274、agent 83、gateway 502、core pack 53）；去縮排用狀態掃描器只動程式碼行、跨行字串原樣 | `goal_loop/tick.rs` 910 行（單一 `tick_once`）；`account_rotator::has_pool_entries` 路徑改 `load::has_pool_entries`（`pub(crate)`、零呼叫端） |
| P11 tools/list 裁剪 | COMPLETE | scaffold agent **215 工具／120,092 bytes → 170／81,743（−31.9%）**，裁掉的 45 個＝`os_native` 6＋`recording` 5＋`system_operator` 19＋`codrive` 2＋`computer_use` 7＋`fork` 6，與派工閘同一組常數；`tools.listChanged`＋`notifications/tools/list_changed`（5s 比對可見集合，非 mtime，因 grants 在 WAL 裡）；description 工具／參數各 ≤200 bytes（工具說明 51,822→33,944、參數 31,604→27,120）；CCR 兩工具 schema 併入 `tool_catalog::ccr_tool_schemas()`；`tools_def/` 16 檔；`docs/guides/mcp-tools.md` 三語新建 | `team_handoff.packet` 1,024 bytes 列明例外；`scoped_tools` 裁剪的代價：員工無法從 tools/list 讀到名稱再申請授權，需由 SOUL／playbook／`grant:<tool>` 告知（已寫文件）；順手修 `select!` 內 `read_line` 非 cancel-safe 的 frame 截斷風險 |
| 協調者收尾 | COMPLETE | 併 P11 CLAUDE.md 片段；MCP 工具數統一為 **243**（CLAUDE.md／README 三語／pyproject，P1 當時寫 245 且 ja 版漏改仍是 200+）；清掉 `git mv` 殘留 index | — |

**第三階段串行全量驗證（協調者實跑，`scratchpad/pC_final.log`）**：`cargo check --workspace --all-targets` PASS；otel feature check PASS；lib：core 766、llm 274、cli-runtime 18、agent 250、security 306、memory 364、inference 82、odoo 63、identity 42、license 111、container 9、db 63、redaction 366；**gateway lib 6,959 通過／1 失敗／12 ignored** —— 失敗的 `server::decision_shadow_api_tests::shadow_http_binds_prospective_sources_and_review_states` 是 09-28 波次留下的未 commit 測試（HEAD 沒有），兩執行緒交錯綁來源、就緒等待 `recv_timeout(10s)` 在 ~7k 測試並行的 SQLite busy 下逾時；單跑兩次皆過，第一、二階段全量也過。已把兩處護欄放寬到 60s（只是防死鎖，不弱化斷言）並附註解，gateway 全量重跑結果見下一行；**cli lib 1,626**（P11 +11）、整合測試與 doctest 綠；web `tsc -b` 0、vitest **256 檔 2,068**；三語 i18n **6,497** 鍵一致。
**gateway 全量重跑（護欄放寬後）**：待補。

### 拍板後續波（P12a／P12b，2026-09-29）

| 波 | 狀態 | 重點 | 判斷 |
|---|---|---|---|
| P12a-A `[decision] enabled` 同步隱藏 | COMPLETE | `system.status` 帶 `decision_enabled`（每次現讀）；前端 `nav-visibility.ts` 加 `Gated.requiresFeature`（唯一刻意 fail-open：只有明確 `false` 才隱藏）；側欄、進階設定索引、⌘K、`/app/system` 卡四個入口同步隱藏；`/app/system/decision-lab` 路由保持可達、顯示「操作者已關閉（`config.toml [decision] enabled`）」空狀態；i18n 三語 +2 鍵；後端 2 測試、前端 4 檔測試 | 頁內空狀態而非重導（既有 `nav-visibility` 教義「隱藏是呈現、路由保持可達」）；個人版導覽實測不含 decision-lab |
| P12a-B 矩陣→`capability_gap_pp` | COMPLETE | `capability_gap_from_matrix`／`capability_gap_for_task`：`(role, runtime)` 下最佳 resolved 模型與今天實際模型的 n 加權差（pp），跨家族不算、兩邊都要有 resolved 格、永不為負；**同檔成對餵 `declared_mde_pp`**（gate 要 `(Some(gap), Some(mde))` 才 fire，只餵 gap 是死碼）；出貨矩陣全 `unresolved` ⇒ 預設逐位不變（測試鎖住）；6 新測試，team_composer 113 綠 | 基線用「接矩陣前的 cascade」而非 `resolve_member_model`（後者會拿勝出者跟自己比永遠 0）；`docs/guides/goal-loop.md` zh-TW／ja-JP 本無 team 章節（既有翻譯欠帳） |
| P12b `handlers.rs` 拆檔 | COMPLETE | 50,808 行 → `handlers/` **190 檔**（17 個 free-fn 前言檔、84 個 `impl MethodHandler` 家族檔、5 個 `dispatch_*`、`tests/` 62 檔）；`handlers` 測試 488＝488、`fn handle_` 385＝385；dispatch 大 match 拆五段串接（`_ => self.dispatch_<next>()`），413 條方法名字串逐字不變；字面值多重集 MISSING 5／EXTRA 19 全可解釋（`include_str!` 路徑加深、ACL 巨集因 `macro_rules!` 衛生性必須留在各 `dispatch_*` 函式體內逐份複製）；`cargo check --workspace --all-targets` 綠；拆分腳本與原檔備份在 scratchpad `p12b/` 可重現 | 仍超 800 的 2 檔各只裝一個單一函式（`agents_update.rs` 1,142、`system_update_config.rs` 916），與 ⑤ 同一類例外 |
| 協調者 | COMPLETE | 併 P12a CLAUDE.md 片段；`ISSUES.md` 補移除註記；OS `release-os.sh audit` 新增 OS-owned crates 區塊並活跑（三 crate 1.65.1、lock 一致 OK）；**`duduclaw expert convert-teams commercial/templates-premium/teams` 已重跑**：22 個團隊全成功，只有 `lawfirm-team` 有差（`expert.toml` +16 行、`team-sop.md`、新增 agent 目錄）——證實 P9b 的過期快取判斷，其餘 21 包逐位相同；輸出在 L3 私有 repo，走該 repo commit | — |

**最終串行全量驗證（P12 後，`scratchpad/pD_final.log`＋`pD_gateway_rerun_full.log`）**：`cargo check --workspace --all-targets` PASS；otel PASS；lib：core 766、llm 274、cli-runtime 18、agent 250、security 306、memory 364、inference 82、odoo 63、identity 42、license 111、container 9、db 63、redaction 366；gateway 第一輪 6,967／1（同一個 09-28 交錯測試，60s 護欄仍逾時——從 panic 行號判讀是 B 先搶到 SQLite 寫鎖、A 在 5s busy_timeout 內拿不到就默默出錯；改為 **A 就緒後再起 B**，與 09-28 姊妹測試同一修法、待測性質不變），**重跑 gateway lib 6,968／0 失敗／12 ignored**；cli lib **1,626**、整合測試與 doctest 綠；web `tsc -b` 0、vitest **256 檔 2,074**；三語 i18n **6,499** 鍵一致。無 gateway 活體驗證。

### 欠帳與待拍板（全波收尾，2026-09-29）

> **2026-09-29 使用者拍板「照建議」**，協調者決定與執行：① `handlers.rs` **拆**（P12b，沿 P10 純搬動紀律）② `[decision] enabled = false` **一併隱藏** SPA 頁與導覽（P12a-A，`system.status` 帶 `decision_enabled`）③ 矩陣**接進** `capability_gap_pp`（P12a-B，只在有 resolved 格時給值，否則 `None` 逐位不變）④ 三 crate 版本：**維持自己的版本線、手動 bump、`release-os.sh` 不 bump**（與 `VERSION` 檔同一教義），`release-os.sh audit` 新增 OS-owned crates 區塊檢查 lock／manifest 一致（已實作並活跑）⑤ 五個單一巨函式檔**不拆**，維持列明例外 ⑥ 測試路徑多一層**接受** ⑦ `ISSUES.md` **補一行**移除註記（已做）。

**原待拍板清單（保留紀錄）**
1. `handlers.rs` 50,752 行不在稽核表 O6 清單（漏列），未拆；是否另開 P12（風險：儀表板 RPC 總調度，數百個 handler）。
2. `[decision] enabled = false` 只擋 `/api/decision/*`，`/app/system/decision-lab` 頁與導覽仍在（關掉後顯示 API 錯誤）；是否一併隱藏。
3. Team `gate = "auto"` 現況只有 `bulk`／`long_horizon` 可量測，灰帶不會真的成團；是否把 H11 接上的矩陣同時餵 `capability_gap_pp`（目前出貨矩陣格全是 `unresolved`，餵了也不會 fire）。
4. 三個殼 crate 搬到 DuDuClaw-OS 後版本政策：目前凍結在 1.65.1，平台 `release.sh` 不再 bump；改由 OS `release-os.sh` 接手或維持凍結。
5. P10 後仍超 800 行的 5 檔（各只裝一個單一函式：`channel_reply/inner.rs` 3,530、`decision_store/tests/shadow_forecast.rs` 1,764、`dispatch_engine/review.rs` 1,020、`channel_reply/cli_env.rs` 993、`goal_loop/tick.rs` 910）要不要做函式體切分（行為風險）。
6. P10a 五個測試模組路徑多一層 `partN`（數量不變）可否接受。
7. `ISSUES.md`（2026-03 歸檔）三條指向已刪的 `sdk/`／`channels/` 檔案，要不要補一行移除註記。

**測試層欠帳**
- `wiki_ingest::tests::wp5c_second_paste_updates_the_same_page` 間歇性永久 hang（09-29 一次，四次正常），需查 channel sender 生命週期。
- 09-28 波次的 `decision_shadow_api_tests::shadow_http_binds_prospective_sources_and_review_states`：60s 護欄仍在全量並行下逾時（2／7 次），根因是兩 contender 同時起跑搶 SQLite 寫鎖、輸家 5s busy_timeout 後默默出錯；已改為 A 就緒後再起 B，之後全量一次通過。若再失敗要看 store 的 busy_timeout 與錯誤回傳（輸家錯誤目前被 thread join 吞掉）。

**需真環境才能做**
- CCR 四臂實測（需 provider 憑證；方法與判準 `wiki/reports/ccr-real-measurement-2026-09-29.md`），過了才改 `[ccr] enabled` 預設。
- Odoo Helpdesk／Project 轉接器活測（需 Odoo 實例）。
- 所有第一至三階段的改動都沒有 gateway 活體驗證（起 gateway、走真通道）；有的只是隔離 home 的 CLI 活測（pack、compat、tools/list golden）。
- native-gui（已在 OS repo）`screens/governance.rs`／`security.rs` 呼叫已刪 RPC／欄位，本機缺 Metal Toolchain 無法編譯驗證。

**commit 前必做**
- `duduclaw expert convert-teams` 重跑（`experts/lawfirm-team/expert.toml` 是過期快取，P9b golden 測試以 NOTE 提示）。
- `fixtures/` 是 `include_str!` 編譯期嵌入，必須隨程式碼 commit；`commercial/`（設計文件、INDEX.md、HISTORICAL 檔頭）走私有 repo。
- DuDuClaw-OS repo 另行 commit（`crates/` 搬入、三支 refresh-src、sync-platform、docs、CHANGELOG、`.gitignore`）；cli recipe 的 `--features app-compat` 替換行在對齊到含該 feature 的平台版本時才打開。
- repo 根目錄雜物（`codex-session-*.md` 36 MB、`cat-ref.*`、`appdb_test.html`、`private_scratch/`）未 ignore，`git add -A` 會全掃進去。
- 版本：這波含多項對外行為變更與棄用（feature gate、Team 預設開、AEE 出廠開、`FeatureGate::check()` 拒絕服務承諾旗標、judge 模式棄用、CLI 動詞收斂），建議 **v1.66.0（MINOR）**，棄用項 v1.68.0 移除。

**建議 commit 拆法（依線）**
① `refactor(audit): remove dead code and ghosts` — P1／P3a／P3b／P3c 的刪除（docs 三語、i18n、CHANGELOG Removed）
② `feat(audit): wire half-dead paths` — P2／P4（Odoo 事件、identity、cost anomaly、data-file-guard、合成鏈、memory_factory、otel 修、night／proxy 文件、miniapp）
③ `feat(team): default-on with safety cascade + matrix prior` — P6
④ `docs(config): example.toml full sections, NER limits, K1 docs, demo-mode banner, decision kill switch` — P5
⑤ `feat(x1): real-data feeds for decision twin / causal graph / CCR builtin sources / Odoo adapter` — P7
⑥ `refactor(t5): notify_push, failover layers, openai-compat dedup, goal_loop dir, org_field_guard table, mcp_auth merge, tick preset, os_ops` — P8a／P8b／P8c
⑦ `feat(t5)!: external name convergence + deprecation policy; unified pack format` — P9a／P9b（含 `docs/guides/deprecations.md`）
⑧ `refactor(t5): split 13 oversized files into directory modules` — P10a／P10b／P10c／P12b（純搬動，`--color-moved` 審；`handlers.rs` 50k 行的 dispatch 拆五段是唯一結構性改動）
⑨ `feat(mcp): capability-filtered tools/list + list_changed + description budget` — P11
⑩ `build(os)!: app-compat feature gate; move shell/comp/native-gui to DuDuClaw-OS` — P6b（主 repo 側）＋ CI
⑪ `feat(dashboard): hide Decision Lab with [decision] enabled; team gate reads matrix capability gap` — P12a
⑫ `docs: CLAUDE.md, wiki reports (audit + tl-daily), CHANGELOG assembly`
共用檔（`lib.rs`、`handlers.rs`、`types.rs`、i18n、CHANGELOG、`docs/README.md`）跨線糾纏，非互動無法乾淨切，建議 ①→⑫ 順序逐段 `git add -p` 或直接接受少量跨線混入。

## 1. 候選決策表（交使用者拍板）

### T0｜文件／UI 幽靈（程式碼不存在或未接線；不是去留題，是「補實作 or 修文件」）

| ID | 項目 | level | 事實 | 選項 | 建議 |
|---|---|---|---|---|---|
| G1 | `.claude/hooks/` 三階段安全防禦 | L2 | commit `ba015a48` 已刪全部 hook 腳本；CLAUDE.md、`docs/features/05`、`architecture/overview`×3 語、`feature-inventory`×3 語仍當現役；4 處 spawn 仍設 `DUDUCLAW_BROWSER_VIA_BASH=1` 而唯一讀它的 `bash-gate.sh` 已不存在 | A 修文件＋移除懸空旗標 ／ B 重建 hooks | **A**（實際防線是 `agent-file-guard`＋`data-file-guard`＋`input_guard`，敘事應改成這三個） |
| G2 | Governance Layer（`/manage/governance`、policies YAML） | L2 | `duduclaw-governance` crate 於 `b0639b96` 刪除；UI 仍讓操作者寫 policies、零 enforcer；rate／permission／quota 已由 `mcp_rate_limit`／`delegation_policy`／`license_runtime` 覆蓋 | A 移除 UI＋3 RPC＋`gov_*` helper＋`docs/features/21`×3 語 ／ B 補 evaluator | **A**（假安全感是負價值） |
| G3 | Durability Framework 文件 | L2 | crate 已刪、workspace 零引用；`feature-inventory.md` 09-24 才更新仍宣稱存在 | A 刪 `docs/features/22`×3 語＋索引 | **A** |
| G4 | Odoo 事件同步（輪詢＋webhook） | L2 | `PollTracker`／`parse_webhook` 零生產呼叫端、無 `/webhook/odoo`、無輪詢 task；`OdooPage` 開關**預設 true** 且設定照實寫入 config | A 補完（392 LOC＋一條路由＋一個背景 task）／ B 移除 UI 四控制項＋persist＋文件節 | **A**（成本低、拼圖關鍵） |
| G5 | Identity Notion／Chained provider | L2 | 兩個生產點硬編 `WikiCacheIdentityProvider`（註解自陳 migration Step 2）；文件敘述 Notion 為上游；`chained.rs` 321 LOC 零測試；RFC-21 兩個承諾工具未實作 | A 接上 `build_identity_provider`＋補測試 ／ B 誠實化 `docs/features/25` | **A** |
| G6 | Google Workspace 閘門 | L1 | 後端 `[integrations] google_workspace` 預設 false，前端 `GOOGLE_INTEGRATION_ENABLED` 硬編 true ⇒ 分頁看得到、按下去 403 | A 前端讀後端旗標＋啟用引導 ／ B 後端預設開 | **A** |
| G7 | Browser 五層自動路由敘事 | L2 | `browser_router.rs`（438 LOC，2026-04）零呼叫端、CHANGELOG 零提及；真實只有 L1／L2／L5 三個 MCP 工具；`docs/features/08` 宣傳不存在的 L4 | A 刪 `browser_router.rs`＋改 CLAUDE.md／docs 08 | **A** |
| G8 | 文件數字與敘述過時（一併修） | L2 | CLAUDE.md「~191 MCP tools」實測 245；`pyproject.toml`「80+」（PyPI 公開頁）；README「200+」；「Python subprocess bridge for skill vetting」Rust 零呼叫端；「LOCOMO cron 03:00」repo 零排程；`docs/features/11` 描述 v1.33 已移除的三策略壓縮器（`inference.update` 仍收兩個死 section）；`03` 把 llama.cpp／mistral.rs／MLX 寫成可用後端且與 `53` 矛盾；`14-voice-pipeline` grep 零命中；`live-forking.md`＋`28-live-forking.md` 兩份；`docs/todo/` 兩份 ✅fixed 仍在＋一份寫 not-started 但已修；`feature-inventory` 「23 Pages」清單過時；`user_code.rs:16` 自稱無生產路徑（假，`user_code_profile` 是活工具）；`skill_hub.rs:13` 說 `skills-sh` 是 stub（它在 `DEFAULT_HUB_IDS`）；CLAUDE.md Architecture 標 v1.15.0、OS-native 線（v1.42–1.65）整條缺席；「第三份手刻 sender 已移除」但 `dispatcher.rs:3007/3128/3166` 還有第四份 | A 一次修完（約 20 處） | **A**（投入產出比最高） |

### T1｜L2 死碼（零呼叫端／從未編譯／從未出貨）— 建議淘汰

閘門一致：`irrev` 0.05（git 可逆）、`sunk` 0.3、`risk` 1、`1y` 0.85。verdict 淘汰機率皆 ≥ 0.75。

| ID | 項目 | LOC | 證據 | 一併處理 |
|---|---|---|---|---|
| D1 | `duduclaw-llm::FallbackRouter`（`router.rs`） | 451 | 全部符號 crate 外零呼叫端；CLAUDE.md 自承 | `lib.rs` re-export、8 測試、CLAUDE.md 段 |
| D2 | `inference/mlx_bridge.rs` | 200 | `generate()` 零呼叫端；文件宣稱的「演化反思」路徑不存在 | config `mlx` 欄、`engine.rs` 分支、mcp／handlers pass-through、docs 03 |
| D3 | llama.cpp in-process backend（`llama_cpp.rs`） | 92 | `generate()` 回「not yet fully implemented」；`build-release.sh` 從不編譯此 feature | `llama-cpp-2` 依賴＋metal/cuda/vulkan features、docs 03、jitrl 支援表 |
| D4 | `duduclaw-cli-worker`＋`worker_supervisor.rs` | 2,973 | 出貨管線從不 build 此 binary；打開 `worker_managed` 只會 resolve 失敗 | crate＋workspace member、三個 config 鍵、三個 Prometheus 指標、docs 27 worker 段（與 S1 綁定） |
| D5 | `cli-runtime::Supervisor`／`RestartPolicy` | 96 | crate 外零呼叫端；檔頭自承 Phase 2 才接線，已一年 | `supervisor.rs`、`lib.rs` |
| D6 | `gateway/browser_router.rs` | 438 | 零呼叫端、CHANGELOG 零提及（＝G7） | docs 08、CLAUDE.md |
| D7 | `discord_voice.rs`＋songbird 系依賴 | 563 | 零呼叫端、不在 default features；**背 3 條 RUSTSEC 豁免**（其中 `RUSTSEC-2023-0071` 理由指向 `livekit-api`，Cargo.lock 0 次） | `.cargo/audit.toml` 三條、Cargo features |
| D8 | `gateway/webhook.rs` | 280 | `webhook_router`／`WebhookState` 零引用，`server.rs` 從未 mount `/webhook/{agent}` | — |
| D9 | `gateway/activation.rs` | 158 | 零 `mod` 宣告＝從未編譯，8 個跑不到的測試 | — |
| D10 | `duduclaw-security` 五孤兒模組（`filter_chain`／`template_sanitizer`／`os_reconcile`／`credential_proxy`／`mount_guard`）＋`src/mod.rs`／`src/unicode_tests.rs` 重複檔 | 1,440 | 五組型別名 workspace-wide 零外部命中；後兩檔與 `src/tests/unicode_tests.rs` 位元組相同且未宣告 | `lib.rs` 五行；`os_reconcile` 若 OS 線要用移到 `duduclaw-os` |
| D11 | `gateway/delegation_scope.rs` | 192 | 三個公開符號零外部引用 | `lib.rs`、7 測試 |
| D12 | `gateway/cost_anomaly.rs` | 133 | `detect()` 只有自己的測試呼叫；doc 自稱已接 notify 是願望 | **反向選項：接上 `budget.rs`（便宜）即轉保留** |
| D13 | `skill_lifecycle` 五死模組（含 `reconstruct_skill`、`vetting`） | 1,574 | 全零呼叫端；`vetting` 唯一路徑經 test-only 函式；`docs/features/15` Stage 4 對應死碼 | docs 15 改寫 |
| D14 | `gvu/shadow_mode.rs`＋`diversity.rs` | 509 | 8 個 pub 項目零引用、零測試、半年未動；`gvu/mod.rs:18` 仍宣傳 | `gvu/mod.rs` doc |
| D15 | `prediction/foresight_gate.rs` | 304 | 檔頭自寫「No consumer wired」；CLAUDE.md 卻寫成已生效 | CLAUDE.md 段（或反向：接進 HITL） |
| D16 | memory `search.rs`＋`VectorIndex` | 160 | 零呼叫端；全 crate 唯一無 `pub use` 模組（**保留 `cosine_similarity`，5 呼叫端**） | — |
| D17 | 假旗標與死欄位：`metrics.rs` 六個零遞增序列（`update_budgets()` 無測試）；license `features.toml` 8 個零行為 feature flag；三個零讀取端 quota 欄位（`max_local_models`／`max_messages_per_month`／`office_hour_hours_per_month`） | — | 全 L2 | 先確認既有 Grafana panel；flag 改標「服務承諾」而非 gating |
| D18 | 前端孤兒：`_WipPlaceholder.tsx`、`ApprovalsPage.tsx`（自己的測試寫 `(unrouted)`，卡片邏輯已被 `ApprovalRequestCard` 抄走） | — | L2 | — |
| D19 | 殘留物：`scripts/build-release.sh`（零引用、2026-03 起未動、被 `release.sh` 取代）、`scripts/build.sh`、`python/spikes/`（含進版控的 `.egg-info`）、`evals/.DS_Store`、`smoke-pty-pool`／`smoke-fork`／`smoke-decision-continuity` 五個零 CI 引用腳本 | — | L2／L1 | — |
| D20 | `templates/orchestrator/`＋`KILLSWITCH.toml` | — | 零程式引用（其他六個子樹皆 `include_str!`），只能手動複製 | 移到 `docs/`或`examples/` 並標明 |

### T2｜半死／接線不完整（建成但沒接上）— 補完 or 移除

| ID | 項目 | level | 事實 | 選項 | 建議 |
|---|---|---|---|---|---|
| H1 | LINE OA 點數計費 `credit.rs`（228 LOC） | L2 | `CreditLedger` 唯一呼叫端是 CLI；doc 承諾的 fail-closed 扣點閘在回覆路徑零呼叫端；多 OA 路由 `resolve_accounts()` 零呼叫端 | A 補完（接 LINE 回覆路徑＋RPC／UI）／ B 移除（含 `credits.db`、CLI） | 依商業時程；未排程就 **B** |
| H2 | skill 自動合成鏈 | L2 | 與 config 無關地斷三處：`confirm_synthesis`／`cancel_pending` 零呼叫端→永遠 pending；`SandboxStore::add` 零呼叫端→整段 no-op；`gap.rs` 寫 `signal_type`、`external_factors.rs` 讀 `type` | A 當 bug 修 ／ B 改文件承認未接線 ／ C 連同 D13 整條合成線淘汰 | **A**（CLAUDE.md 主打功能） |
| H3 | `[evolution]` 11 個技能旋鈕只寫不讀 | L2 | 四個實際值硬編在 `channel_reply.rs:631/633/4994`；dashboard 調了不生效 | A 接上讀取端 ／ B 整組移除 | **A**（或 B，不能維持現狀） |
| H4 | `memory_factory` 收斂未完成 | L2 | `[memory] novelty_gate`（預設開）在約 18 條路徑靜默失效，含 `server.rs:1239` 排程 decay | A 補完收斂 | **A** |
| H5 | `finetune.jobs.*` 只暴露一個 RPC（3,983 LOC 頁面齊全） | L1 | 漏做還是故意未查證 | A 補 RPC ／ B 維持 | 需你答「漏做還是故意」 |
| H6 | night engine（2,478 LOC） | L1 | 雙層 opt-in 全關、無功能文件、`example.toml` 無 `[night]` 段＝使用者不可能知道怎麼開 | A 補文件＋example ／ B 淘汰 | **A** 若仍是路線，否則 B |
| H7 | `otel.rs`（883 LOC，feature 預設 OFF） | L1 | `--features otel` 全 repo 零命中、CI 未建 | A 加一條 CI build ／ B 移除 | **A**（企業 Langfuse 需求現成） |
| H8 | `github_workspace` 缺 `integration_enabled` 閘門（Google 有） | L1 | 5 個工具無預設關閘 | A 補閘門對齊 Google | **A**（bug） |
| H9 | Expert pack `pharmacy-pro` 被靜默排除 | L2 | manifest name 與目錄 slug 不符，靜默丟棄 | A 改名＋slug 不符時 warn | **A**（bug） |
| H10 | `data-file-guard` shell hook | L2 | Windows 無 bash 靜默失效（自陳） | A 改寫成 Rust 子命令 | **A** |
| H11 | `role_model_matrix.toml` composer 尚未讀取（量表產出無消費者） | L1 | 文件自承 | A 接上 composer ／ B 標「人工參考」 | 與 X2 一起決 |
| H12 | Partner Portal（store 2026-04-20 後零變更、無導覽項） | L1 | 真正控制面在 gitignored `commercial/cloud-control-plane` | A 維持 ／ B 移出主 repo | 依雲端經銷時程 |
| H13 | Streamable-HTTP 2 測試、Remote OAuth 5 測試（高風險低覆蓋） | L1 | 非去留，補測試 | A 補測試 | **A** |

### T3｜standby／保險／預設關且無使用痕跡 — 拍板去留

| ID | 項目 | LOC | level | verdict（保留/簡化/取代/淘汰） | obs/unu | 閘門 irrev/sunk/risk/1y | 最強保留 | 最強淘汰 | 選項 | 建議 |
|---|---|---|---|---|---|---|---|---|---|---|
| S1 | PTY pool 整體（`duduclaw-cli-runtime`＋`pty_runtime`＋`oneshot`＋migration＋`runtime_status`） | ~8,000 | L1 | .35/.35/.05/.25 | .55/.70 | .10/.60/3/.60 | Anthropic 隨時可能重啟 OAuth `-p` 拆分，這是零改碼保險 | 前提暫停 15 個月；pool key 無對話維度＝跨對話 context 洩漏，真要用不能直接開；08-04 還得寫遷移把誤開設定關回去 | A 留但先修 conversation 鍵 ／ B 留 `oneshot_pty_invoke`、刪 pool＋worker（含 D4/D5）／ C 全刪，降為 ADR 一筆 | **B** |
| S2 | RFC-26 Live Forking（`duduclaw-fork`＋`mcp_fork*`＋`ForkPage`） | ~5,000 | L1 | .45/.25/0/.30 | .30/.65 | .05/.55/2/.55 | 並行分支＋AI judge 擇優是論文級能力，全套完工 | 每 agent 預設 false、06 月後零星維護、無使用痕跡；`duduclaw eval` 已承接「多次跑取優」的部分需求 | A 留 ／ B 留後端刪 UI＋MCP 面 ／ C 全刪 | **A 或 C**（半留最糟） |
| S3 | JitRL（`jitrl/` 五模組＋`jitrl_feedback`） | 1,249 | L1 | .45/.25/.05/.25 | .20/.70 | .05/.40/2/.55 | 「只有本地推論做得到」的護城河 | 預設關＋需人工呼叫＋只一層有真偏置面＋零文件零 dashboard | A 留 ／ B 淘汰 | **B** |
| S4 | Exo P2P 叢集 | 178 | L1 | .25/.15/.10/.50 | .55/.80 | .05/.20/1/.75 | 235B 級跨機唯一路徑 | 04 月起未動、無範例設定＝使用者不可達、`openai_compat` 指向 Exo 端點即可 | A 留 ／ B 淘汰 | **B** |
| S5 | mistral.rs backend | 331 | L2 | .30/.20/.05/.45 | .45/.90 | .05/.30/2/.65 | ISQ／PagedAttention 是真能力 | feature 預設關、release 不帶＝從未出貨 | A 留 ／ B 淘汰 | **B**（與 D3 同批） |
| S6 | llamafile 管理（3 MCP 工具） | 293 | L1 | .55/.25/.10/.10 | .35/.55 | .05/.30/2/.55 | 零安裝單檔推論門檻最低 | 值班機改走 `llama-server`；三個工具換不到多少 | A 留 ／ B 淘汰 | A（成本低） |
| S7 | 本地路由雙 gate：legacy post-hoc（α/β 未擬合、`assess_response()` 零呼叫端、擬合刻意不排程）＋UCCI（疊在其上） | 514＋370 | L2／L1 | post-hoc .30/.45/.20/.05；UCCI .80/.15/0/.05 | .60/.75；.30/.70 | .05/.35/2/.60 | 級聯路由省真錢；UCCI 有擬合腳本與論文依據 | 第二層校準器疊在一個承認未量測的第一層上 | A 刪 post-hoc 四鍵留 UCCI ／ B 全刪 ／ C 排程擬合 | **A** |
| S8 | `duduclaw proxy`（讓 Aider／Cline 借訂閱配額） | 1,591 | L1 | .70/.20/0/.10 | .15/.60 | .05/.30/2/.60 | 對外部工具是實打實價值 | 無 feature 文件、07 月起未動、無使用痕跡 | A 留＋補文件 ／ B 淘汰 | **A** |
| S9 | Code Mode 量測閘 `cost tool-loop`＋`tool_loop_probe` | 977 | L1 | .30/.25/0/.45 | .45/.65 | .05/.40/1/.70 | 資料多後可重跑 | 一次性工具、已得 INSUFFICIENT_DATA、Code Mode 未立案 | A 留 ／ B 淘汰 | **B** |
| S10 | 一次性／零文件 CLI：`memory bench`、`evolution clear-holdout-rotation`（為死旗標補的命令）、`rl export/stats/reward`（零文件）、`ops tunnel`／`credit`／`reforward`（v1.8.21 事故補救）、MCP `log_mood`（描述四個字） | — | L1／L0 | 淘汰 .40–.55 | — | .05/.20/1/.70 | 各有一次性用途 | 長駐產品 CLI／MCP 表面 | A 留 ／ B 移到 `--hidden` 或刪 | **B** |
| S11 | GVU legacy SOUL 演化（`gvu/*` 非 AEE 部分） | 6,900 | L2 | .30/.30/.05/.35 | .60/.60 | .10/.70/3/.55 | 唯一能把人格改回來的機制；`observation_finalizer` 真被呼叫 | 官方宣告 non-default legacy；SOUL.md 對 agent 已唯讀＝它守護的寫入路徑已封；**`soul_partition.rs` 被 `playbook migrate-soul` 依賴不可同刪** | A 留 ／ B 淘汰（保留 `soul_partition`）| **B** |
| S12 | `python/duduclaw` PyPI 套件 | 12,364 | L2 | .25/.40/0/.35 | .65/.70 | **.50**（PyPI 使用者）/.50/3/.60 | PyPI 通路曝光；200 筆 golden QA 是唯一記憶品質基準 | 與 Rust 完全脫鉤（`Command::new("python` 零命中、打的 `/memory/search` 路由不存在、`duduclaw-bridge` 不存在、缺三個依賴 import 不起來）；`channels/`（03-19）與 `sdk/`（04-15）重複；`memory_eval` 7,223 行每版包進 wheel 但含 CI 無人跑；golden QA 全 `manual` 零筆 LOCOMO | A 維持 ／ B 縮成 `mcp`＋`agents`（有 CI），刪 `channels`／`sdk`／`spikes`，`memory_eval` 移出 wheel ／ C 停止 PyPI 發行 | **B** |
| S13 | `duduclaw-docuseal-mcp` | 727 | L1 | .45/.20/.10/.25 | .40/.55 | .05/.20/2/.55 | 電子簽署是辦公協作延伸 | 單一 commit、不隨 release 出貨、workspace 零引用、官方自有 MCP | A 留 ／ B 淘汰改用官方 MCP | **B** |
| S14 | `export --format agentcompanies` | 1,328 | L1 | .30/.20/.10/.40 | .65/.55 | .05/.30/1/.70 | paperclip 生態互通 | paperclip 路線已兩度轉向、無已知消費者 | A 留 ／ B 淘汰 | **B** |
| S15 | App compat／Windows VM（`compat_*`） | 2,334 | L1 | .45/.20/.05/.30 | .35/.60 | .05/.40/2/.60 | 值班機 Windows 敘事 | 屬已拆的 OS 線；runner 只回報不執行；真機從未驗證 | A 留主 repo ／ B 移到 DuDuClaw-OS ／ C 淘汰 | **B** |
| S16 | OS 線 crate 留在主 workspace：`duduclaw-shell`（57k）＋`-comp`（32k）零主 workspace 依賴、零 CI；另 `-os`／`-native-gui`／`-pets`／`-sysd`／`-relay` | 89k＋ | L2 | shell/comp：.30/.10/.50(移出)/.10 | .20/.60 | .15/.50/3/.65 | 桌面 App 產品線 | 拆 repo 時「shell←native-gui」理由依現況不成立（`Cargo.toml` 反向依賴不存在）；壞了沒有任何自動流程會發現 | A 全留＋納 CI ／ B shell＋comp 移到 DuDuClaw-OS ／ C 全部 OS crate 移出 | **B**（至少 A） |
| S17 | Git worktree L0 隔離 | 1,151 | L1 | .35/.25/.05/.35 | .45/.70 | .05/.35/2/.60 | 比容器輕、dispatcher 已接好 | 預設關、零 template 啟用、三個月未動、goal loop 主線不經此路 | A 留 ／ B 淘汰 | **B** |
| S18 | `duduclaw auth device` Qwen 分支 | — | L1 | — | .40/— | .05/.20/1/.70 | 席次廣度 | 上游 2026-04-15 停掉免費 OAuth，自標 PENDING-LIVE | A 留 ／ B 砍 Qwen 留 Copilot | **B** |
| S19 | `computer_*`（7 工具，純座標點擊）vs `codrive_*`（三段式） | — | L1 | computer：.60/.25/0/.15 | .20/.40 | .05/.30/2/.55 | 兩者不同抽象層 | `codrive_run` 是同一件事的更好版本；`codrive` 命運又綁 S16 | A 留兩套 ／ B 以 codrive 取代 computer_* | 先決 S16 |
| S20 | miniapp（`[miniapp]`、`miniapp.rs`） | — | L1 | .55/.30/0/.15 | .20/.50 | .05/.30/2/.55 | LINE MINI App 是 Q4 窗口項（記憶） | 無 dashboard 開關、無使用痕跡 | A 先補開關觀察一版 ／ B 淘汰 | **A** |

### T4｜大型未 commit 實驗線（本次最大決策）

| ID | 項目 | 規模 | level | verdict | obs/unu/ove | 閘門 irrev/sunk/risk/1y |
|---|---|---|---|---|---|---|
| **X1** | CCR 可逆上下文取回＋因果證據圖＋決策孿生（Decision Lab）三線 | ≈62,800 LOC（gateway 39,865＋CLI 9,536＋web 8,191＋llm 5,185）、61 個 CLI 子命令（約全部葉命令 1/3）、245 個 MCP 工具中 2 個、3 份 spec、7 個 dashboard 頁群；gateway 29/35 檔未進版控 | L1 | 整包：.30/.45/.05/.20 | .15/.60/.75 | 不 commit＝**存 patch 即可逆**（.10）／sunk **.80**／risk 2（無客戶依賴）／1y：A .45、B .60、C .65、D .50 |

- 最強保留：repo 裡唯一把「AI 的宣稱」與「可重放的來源位元組」綁死的體系（policy 預宣告→forecast 凍結→score 分離→人工收據→來源撤銷連動）；spec 誠實度極高；CCR 的撤銷租約與跨使用者隔離是市面少見真本事；今天兩波修正已修掉審查抓到的 5 個 P1，全綠。
- 最強淘汰／簡化：三份 spec 全部自我否定生產價值（「exploratory」「does not yet establish a production causal effect」「remains pending / unverified」）；61 個子命令約 50 個是 demo／eval／replay／compare 類只服務合成 fixture；零真實客戶資料、零真實模型量測；已吃掉 6.6% 程式碼；決策孿生零 config 鍵＝零 kill switch；CCR 橫切 11 個通道的訊息落庫語意；`decision_store.rs` 12,476 行單檔。
- 分線看：**CCR**（11.4k）有直接的 token 成本價值與 MCP 工具面，最接近產品；**因果圖**（11k）與**決策孿生**（34k）是同一條「可稽核預測」敘事，價值取決於是否真有客服／營運客戶要用。
- 選項：A 全部 commit（現狀）／ B 全部 commit 但凍結新增面：預設關、61 個 CLI 降為 `hide=true`、標 experimental、決策孿生補 kill switch ／ C 只留 CCR，因果＋決策孿生存成 patch 檔（`git diff > commercial/patches/…`）退出工作樹，等有真實客戶再回來 ／ D 三線全存 patch 退出。
- **建議 C**（若你有明確客服客戶要跑 shadow 試點則 B）。

| ID | 項目 | 規模 | level | verdict | obs/unu/ove | 閘門 |
|---|---|---|---|---|---|---|
| **X2** | Team-as-Agent（composer、role_turns、fault_attribution、team_gate、task_packet、role_model_matrix、eval matrix／team-2x2） | ≈17,300 LOC，未 commit，`[team] enabled` 預設 false 且 gate 再預設 Solo | L1 | .80/.15/0/.05 | .05/.55/.55 | .10/.60/3/.70 |

- 最強保留：「一位員工四角色各綁不同廠商」是強差異化；14 輪活測跑通；`fault_attribution` 已有 3 個生產呼叫端。
- 最強淘汰：三個月工程押在雙層預設關功能上；量表產出的矩陣 composer 尚未讀取（H11）；未進 release。
- 選項：A commit＋訂「什麼條件下 `[team] enabled` 翻預設」的判準 ／ B commit 標 experimental ／ C 存 patch 退出。
- **建議 A**。

### T5｜過度工程／重複（建議收斂，不建議刪）

| ID | 訊號 | 證據 | 收斂建議 |
|---|---|---|---|
| O1 | 「失敗後換誰」有四套：`failover.rs`／`llm_fallback.rs`／`FallbackRouter`／rotator 冷卻 | 邊界靠註解維持 | 刪 D1 後把 A08／A09 收成一層 |
| O2 | 三套包格式：Preset／Expert packs／Premium templates | rep .40–.45 | 合併為一套帶 tier 的 pack 格式 |
| O3 | 兩套鏡像 wiki API：`wiki_*`（14）vs `shared_wiki_*`（6） | 逐一對映只差 namespace | 收成一個 `scope` 參數 |
| O4 | 四個建任務入口：`create_task`／`tasks_create`／`goals_create`／`schedule_task` | — | 收成一或兩個 |
| O5 | 四份同形通知模組（5,917 LOC）＋`dispatcher.rs` 第四份手刻 sender | CLAUDE.md 說第三份已移除 | 合併 |
| O6 | 超過 800 行上限的單檔：`mcp.rs` 32,570／`channel_reply.rs` 13,543／`decision_store.rs` 12,476／`dispatch_engine.rs` 7,169／`task_store.rs` 6,296／`goal_loop.rs` 5,938／`ccr.rs` 5,185／`account_rotator.rs` 4,459／`tool_loop.rs` 4,135／`approval.rs` 3,500／`ephemeral.rs` 3,457 | 專案自訂上限 800 | 按功能拆檔 |
| O7 | 245 個 MCP 工具 schema＝spawn 固定 token 成本最大單項（CLAUDE.md 記 191 時就標「curation DEFERRED」，現漲 28%）；`duduclaw_ccr_*` 兩個工具定義在第二套註冊面（llm crate） | — | 依呼叫者能力裁剪 tools/list；合併註冊面 |
| O8 | Goal loop 八小模組（`goal_bail_detect` 395 LOC／27 測試／1 呼叫端／只出 advisory） | ove .65 | 併成 2–3 個 |
| O9 | `org_field_guard.rs` 2,374 LOC／82 測試 vs 約 10 呼叫端 | — | 規則表資料化 |
| O10 | CLI 命名衝突：三個 `migrate`（`migrate`／`migrate-from`／`data-migrate`，doc 要寫免責）、四個 `export` 家族、`acp` vs `acp-server`；`Wizard` 無說明 | — | 重命名／補說明 |
| O11 | inference 與 llm 各一份 openai-compat reqwest 實作（548 vs 908） | — | UCCI logprobs 擷取上移 llm，刪 inference 那份 |
| O12 | 判官 seam 四模式三個沒人用；`judge_command` 刻意不開 RPC | unu .50 | 留 mav＋external，其餘降級 |
| O13 | `skill_*` 三套搜尋（GitHub API／bank／hub）模型無規則可選 | ove .55 | 合併入口 |
| O14 | MCP 認證九模組 5,443 LOC（`mcp_auth_strategy` 679 LOC 唯一消費者是 `mcp_auth`） | — | 收斂預留抽象 |
| O15 | Resident sensing 旋鈕數；`org_field_guard`；autopilot screen fail-open 預設 | ove .55 | 「保守／積極」兩個 preset |
| O16 | `os_*` 同一組能力存在於 MCP（25）／CLI（25 葉）／dashboard RPC 三份 | rep .45 | 單一權威＋薄轉接 |

### T6｜config／文件矛盾（需拍板方向的）

| ID | 項目 | 事實 | 選項 | 建議 |
|---|---|---|---|---|
| K1 | `[task_forward_model]` 預設值 | CLAUDE.md／inventory 寫 default off；`task_forward_store.rs` `impl Default` 是 true 且同檔註解寫「(still default false)」 | A 程式改 false ／ B 文件改 true | 你決定產品意圖（記憶 v1.54 記「已 release＋預設開」） |
| K2 | 出廠板模 `wizard.rs:704` 寫死 `gvu_enabled = false` | 出廠 agent 一個都不會演化（GVU＋AEE＋playbook 3.1 萬行對出廠使用者不生效；`[evolution] enabled` 預設 true 所以 reflexion／mistake_notebook 仍活） | A 維持（安全）／ B 出廠開 AEE ／ C 出廠給「保守」演化 preset | 產品敘事拍板 |
| K3 | `config/duduclaw.example.toml` 涵蓋率約 20%（實際被引用的頂層段落 ≥50，範例只示範約 10；`[tick]`／`[belief]`／`[goal_loop]`／`[redaction]`／`[mail]`／`[night]`／`[limits]` 等一級功能缺席） | — | A 補齊（至少每段一行導航）／ B 維持分層 | **A** |
| K4 | NER 去識別化承諾強度 | 三重 opt-in、自陳 zh-TW 召回約 80%／人名約 72%、Intel macOS 完全不存在；未裝模型而列規則會 poison manager（fail-closed） | A UI 明講「第二層、非法遵保證」 | **A** |
| K5 | `identity` redaction 規則「自 v1.14 宣告但直到 09-23 才編譯」 | 覆蓋率曾長期虛報 | 已修；CHANGELOG 應誠實補述 | **補述** |

## 2. 完整功能清單（背景，依領域；五份盤點者原始報告全文）

---

### 附錄 A_runtime

#### 功能盤點 — 領域 A：runtime／模型層

> 盤點者：Agent A｜日期 2026-09-29｜Repo `/Users/lizhixu/Project/DuDuClaw`
> 方法：唯讀。未執行 cargo／npm，未做任何 git 寫入。所有被讀內容視為 DATA。
> 證據來源：原始碼 grep／`wc -l`／`git log -1 --date=short`／`docs/features`／`config/duduclaw.example.toml`／`CHANGELOG.md`。
> 表格拆兩張（同一個 ID 對應同一列）純為可讀性，仍是「一個功能一列」。

---

##### 0. 領域規模速覽

| 子系統 | 主要程式碼量 |
|---|---|
| `crates/duduclaw-llm`（含 providers） | 18,925 |
| `crates/duduclaw-inference`（含 jitrl／model_registry） | 10,621 |
| `crates/duduclaw-cli-runtime` + `duduclaw-cli-worker` | 5,421 + 2,080 |
| gateway `runtime/*` 七模組 | 8,503 |
| gateway 模型層散檔（pty_runtime／cost_telemetry／account_rotator…） | ~22,000 |
| Team-as-Agent（gateway + core + eval 量表） | ~19,600 |
| `crates/duduclaw-fork` + `mcp_fork*` | 2,828 + 2,161 |
| **領域 A 合計（估）** | **約 95,000 LOC** |

**未提交（untracked）的大面**：CCR 全家、Team-as-Agent 全家、`inference/ucci.rs`、`eval/matrix.rs`。
這代表領域 A 有相當比例的功能**尚未進過任何 release**。

---

##### 1. 功能總表（甲：證據欄）

| ID | 功能名 | 面向 | 入口 | 接線狀態 | 最後實質變更 | 測試 | 文件 | 重疊／可取代者 | 規模(LOC) |
|---|---|---|---|---|---|---|---|---|---|
| A01 | `AgentRuntime` trait + `RuntimeRegistry` 自動偵測 | multi-runtime | `agent.toml [runtime] provider` | live | 2026-09-07 | 有（mod.rs 內） | `docs/features/13` | — | 1,529 |
| A02 | 五個 bespoke runtime（codex／gemini／antigravity／grok／claude） | multi-runtime | 同上 | live（偵測到 CLI 才註冊） | 2026-08-07～09-07 | codex/gemini/agy/grok 各 18–23 測 | `docs/features/13`、`commercial/docs/runtime-operator-guide.md` | `generic_cli` 可承載但缺帳號輪替／MCP 注入／sandbox 旗標 | 5,663 |
| A03 | `generic_cli` 印表模式 runtime（qwen／kimi／copilot／kiro／cursor／vibe／opencode 七家） | multi-runtime | 同上 + `runtime_catalog` | live（binary 存在才註冊） | 2026-09-23 | 22 | `docs/features/13`、`docs/todo/TODO-ai-runtimes-2026-09.md` WP-B | — | 1,211 |
| A04 | `runtime_catalog`（13 個 runtime 的單一權威表） | core | 被偵測／安裝／模型探索／`cli_auth` 共讀 | live | 未查（core 新檔） | 有 | 同上 | 先前三份手寫清單（已收斂） | 1,504 |
| A05 | runtime 上線工具鏈：`cli_auth`／`runtime_install`／`runtime_models`／`model_capabilities` | multi-runtime | dashboard RPC＋OOBE | live | 2026-09-07 | 有 | `docs/features/13`、`54` | — | 3,263 |
| A06 | `openai_compat` runtime（API 模式 agent） | multi-runtime | `[runtime] provider = "openai_compat"` | live | 2026-09-23 | 有 | `docs/features/13` | — | 1,470 |
| A07 | `runtime_dispatch`：`run_agent_prompt` / `run_utility_prompt` 供應商無關咽喉 | dispatch | 內部 API（22 個 gateway 檔＋6 個 cli 檔呼叫） | live（高頻） | 2026-08-13 | 有 | RFC-25 | — | 897 |
| A08 | `failover.rs` 跨供應商健康追蹤＋切換 | dispatch | 只經 `runtime_dispatch` | live | 2026-08-13 | 19 | `docs/features/07` | 與 A09／A11／帳號輪替 Failover 策略四層重疊 | 970 |
| A09 | `llm_fallback.rs` 模型級 timeout/503 降級 | dispatch | `claude_runner` | live | 2026-09-01 | 有（獨立 test 檔） | CLAUDE.md | 與 A08／A11 重疊 | 183 |
| A10 | `judge_mode.rs` 判官 seam（mav／evaluator_only／external／human_only） | dispatch | `config.toml [dispatch] judge`＋儀表板下拉 | live（預設 mav＝逐位相同） | 2026-09-01 | 24 | `docs/guides/goal-loop.md` | — | 1,357 |
| A11 | `duduclaw-llm::FallbackRouter`（冷卻＋context-window 候選過濾） | llm | 只在 `lib.rs` re-export | **dead（零生產呼叫端）** | 2026-07-05 | 8 | CLAUDE.md 已自承 "zero production call sites" | A08／A09／`account_rotator` 冷卻邏輯 | 451 |
| A12 | 四協定 provider（Anthropic／OpenAI Responses／Gemini／OpenAI-compat 8 presets）＋`sse.rs`／`types.rs`／`error.rs`／`http.rs` | llm | `claude_runner`／`channel_reply`／`local_llm`／runtime openai_compat | live | 2026-09-23 | 多 | CLAUDE.md | — | 4,704 |
| A13 | `ModelRegistry` + `models.toml`（~15 model、價格／price-cliff／capability） | llm | `~/.duduclaw/models.toml` 覆寫 | live（cost_telemetry／eval／team_composer／proxy） | 2026-07-13 | 15 | CLAUDE.md | — | 645 |
| A14 | MoA 虛擬模型（`moa:<name>`） | llm | `models.toml [moa.<name>]`＋`[model] preferred = "moa:x"` | live opt-in（無 spec 則功能隱形）；`stream_moa_model` 零消費者（`direct_api.rs:511` 自承） | 2026-07-12 | 12 | 無專篇 | — | 1,030 |
| A15 | `mcp_client.rs`（stdio JSON-RPC MCP client）＋`tool_loop.rs`（供應商無關工具迴圈）＋`provenance.rs` | llm | 所有 direct-API／本地推論／openai-compat runtime 路徑 | live | 2026-09-23 | 59＋21＋23 | CLAUDE.md | — | 6,768 |
| A16 | **CCR 可逆工具結果壓縮**（`llm/ccr.rs` + gateway `ccr_runtime`／`ccr_dashboard`／`ccr_replay`／`causal_ccr_outbox` + 4 個 CLI 子命令 + `/api/ccr/dashboard`） | llm／context | `config.toml [ccr] enabled`（預設關）；MCP `duduclaw_ccr_retrieve`／`duduclaw_ccr_find`；`duduclaw ccr *` | opt-in 預設關，但已橫切 11 個通道 adapter 的交付租約 | **uncommitted 2026-09** | 59（ccr.rs）＋ 各檔 | `docs/spec/reversible-context-ccr.md`、`docs/todo/TODO-reversible-context-causal-simulation.md` | 與 `prompt_compression`(A27) 解的是不同問題（可逆 vs 預算裁切），不重疊 | ccr.rs 5,185 ＋ gateway 3,804 ＋ cli 2,440 ＝ **11,429** |
| A17 | PTY pool 執行時（`cli-runtime` pool／session／pty／envelope／progress／platform + gateway `pty_runtime`） | PTY | `agent.toml [runtime] pty_pool_enabled`（預設 **false**） | **standby**，且有一次性遷移主動關掉誤開的設定 | 2026-09-07 | 58（pty_runtime）＋ crate 內 | `docs/features/27`（自承「大多數 agent 應關著」＋跨對話洩漏） | 預設 FreshSpawn `claude -p` 路徑 | 5,421 ＋ 2,560 |
| A18 | `oneshot_pty_invoke`（API key 帳號的一次性 PTY） | PTY | `channel_reply` PTY 分支 | live（僅在 A17 開啟時） | 2026-09-07 | 有 | `docs/features/27` | — | 300 |
| A19 | `duduclaw-cli-worker` 行程外 worker ＋ gateway `worker_supervisor` | PTY | `[runtime] worker_managed`（預設 false） | **dead in production**：`scripts/build-release.sh` 只 build `-p duduclaw-cli -p duduclaw-gateway`；`scripts/`／`.github/`／Dockerfile 全無此 binary | 2026-08-20／09-07 | 有 | `docs/features/27`、`commercial/docs/TODO-cli-pty-pool-worker.md` | 行程內 `PTY_POOL` | 2,080 ＋ 893 |
| A20 | `cli-runtime::Supervisor` / `RestartPolicy`（Phase 2 stub） | PTY | 只在 `lib.rs` re-export | **dead（crate 外零呼叫端）**；檔頭自承「Phase 2 wires it into the pool's eviction logic」 | 2026-06-22 附近 | 無 | 無 | pool 自身的 eviction | 96 |
| A21 | `pty_default_migration.rs`（一次性撤銷儀表板誤寫的 PTY 開關） | PTY | 開機自動、marker 檔防重跑 | live 但**一次性**，2026-08 事故的補丁 | 2026-08-04 | 有 | 檔頭 | — | 449 |
| A22 | `runtime_status.rs` `GET /api/runtime/status`（loopback-only） | PTY | HTTP | live | 2026-05-17（領域內最舊） | 少 | `docs/features/27` | 儀表板 RPC | 199 |
| A23 | `InferenceEngine` ＋ backend 分派 ＋ `manager.rs` 多模式狀態機 | local inference | `inference.toml`；MCP `inference_status`／`inference_mode`／`model_*`／`route_query`／`hardware_info` | live opt-in | 2026-09-25 | 19 | `docs/features/03`、`53` | — | 1,902 ＋ 231 |
| A24 | openai-compat 本地後端（inference 自家 reqwest client，含 UCCI top-logprobs 擷取） | local inference | `inference.toml [openai_compat]` | live（appliance 的主路徑） | 未查 | 有 | `docs/features/53` | **與 `duduclaw-llm/providers/openai_compat.rs`(908 LOC) 功能重疊**，但獨有 logprobs 擷取 | 548 |
| A25 | llama.cpp in-process backend | local inference | feature `metal`/`cuda`/`vulkan` | **dead**：`#[cfg]` 關閉且 release 未開；`generate()` 直接回 "not yet fully implemented" | 2026-04-03 | 無 | `docs/features/03` 仍宣稱可用（**過時**） | 映像內的 `llama-server` 子行程（A24 路徑） | 92 |
| A26 | mistral.rs in-process backend | local inference | feature `mistralrs*` | **未出貨**：`#[cfg(feature="mistralrs")]`，`build-release.sh` 不帶該 feature | 2026-07-04 | 無 | `docs/features/03` 仍宣稱可用（**過時**） | 同上 | 331 |
| A27 | `prompt_compression.rs` 預算裁切管線（TurnTrim→DropOldestToolEchoes→BisectAndSummarize，cache-aware guard） | context | 回覆路徑自動 | live | 2026-08-16 | 有 | CLAUDE.md；`docs/features/11` 講的是**已被移除**的另一套 | inference crate 的三策略壓縮器 v1.33 已移除 | 1,715 |
| A28 | `exo_cluster.rs` Exo P2P 分散式推論 client | local inference | `inference.toml [exo]`（預設 `None`，repo 內無範例 toml） | opt-in，領域內最舊檔之一 | 2026-04-12 | 少 | `docs/features/03` | `openai_compat` 直接指向 Exo 端點即可 | 178 |
| A29 | `llamafile.rs` 子行程生命週期管理 | local inference | MCP `llamafile_start`／`_stop`／`_list`；`inference.toml [llamafile]` | opt-in | 2026-06-20 | 少 | `docs/features/03` | 映像走 `llama-server`；一般機器走 `openai_compat` | 293 |
| A30 | `mlx_bridge.rs`（Apple Silicon `mlx_lm` Python 橋） | local inference | `inference.toml [mlx]` | **dead 生成路徑**：`generate()` 全 repo 零呼叫端；只有 `is_available()` 進狀態顯示 | 2026-07-04 | 少 | `docs/features/03` 宣稱「供演化反思用」——**無此程式路徑** | `openai_compat` | 200 |
| A31 | `ConfidenceRouter`（三層 LocalFast／LocalStrong／Cloud）＋ legacy post-hoc 校準閘 | local inference | `inference.toml [router]`，`post_hoc_enabled` 預設關 | opt-in；α/β 為**未擬合**預設（＝固定在 mean logprob ≥ ln 0.5） | 2026-09-25 | 15 | `docs/features/03` | 被 A32 UCCI 取代（同一個 accept-vs-escalate 決策，UCCI 有離線擬合腳本） | 514 |
| A32 | UCCI 校準式階梯路由（`ucci` git 相依 + `ucci.rs` + `scripts/ucci_fit.py`／`ucci_pair.py`） | local inference | `[router] ucci_fast_router`／`ucci_strong_router`／`ucci_observations` | opt-in 預設關 | **uncommitted 2026-09** | 1 | `docs/features/57` | 取代 A31 的 post-hoc 半邊 | 370 |
| A33 | JitRL 零梯度續學（logit 偏置） | local inference | `inference.toml [jitrl] enabled`（預設 false）；MCP `jitrl_feedback` | opt-in；需明確 feedback 呼叫；只有 openai-compat tier B 有真實偏置面 | 2026-07-12 | 有 | 無專篇（僅模組 doc） | — | 1,249 |
| A34 | 本地模型市集＋下載＋服務（`model_registry/market`／`curated`／`downloader`／`hf_api` + gateway `inference_local`／`local_models`） | local inference | 儀表板 `inference.local.*`／`LocalModelsPage` | live | 2026-08-24／09-07 | 有 | `docs/features/45`、`53` | — | 1,841 ＋ 1,113 |
| A35 | `appliance.rs` 值班機推論預設填值 | local inference | `DUDUCLAW_APPLIANCE=1` ＋ `/usr/bin/llama-server` 存在 | live（僅 OS image） | 2026-09-07 | 有 | `docs/features/50`、`53` | — | 268 |
| A36 | gateway `local_llm.rs`（把本地推論接上 `run_tool_loop`） | local inference | `agent.toml [model] local.prefer_local`／`use_router`；cost 自適應 | live opt-in | 2026-09-07 | 有 | `docs/features/53` | — | 746 |
| A37 | `account_rotator`（OAuth＋API key 四策略、健康探測、auth_dead 退避、budget） | accounts | `accounts.add` RPC／`config.toml`／`agent.toml [model] account_pool` | live（核心） | 2026-09-08 | 多 | `docs/features/07` | — | 4,459 |
| A38 | `direct_api.rs` Anthropic Messages 直呼＋分層 cache 斷點＋歸因 | accounts／cost | 所有 OAuth 冷卻時的降級路徑 | live | 2026-08-20 | 有 | CLAUDE.md | — | 836 |
| A39 | `cost_telemetry.rs`（SQLite token 帳、cache efficiency、200K 懸崖、自適應 prefer_local） | cost | MCP `cost_summary`／`cost_agents`／`cost_recent`；儀表板 | live | 2026-07-26 | 有 | CLAUDE.md | — | 2,718 |
| A40 | `tool_loop_probe.rs` ＋ `duduclaw cost tool-loop`（Code Mode Phase 0 量測閘） | cost | CLI 子命令 | live 但**一次性決策工具**；本機已得 INSUFFICIENT_DATA | 2026-08-15 | 有 | `commercial/docs/DESIGN-code-mode-2026-08.md` | — | 977 |
| A41 | `duduclaw proxy`（把帳號池變成本機 OpenAI-compat 端點給 Aider／Cline／Codex 借配額） | accounts | `duduclaw proxy --bind`；Bearer 金鑰強制 | live opt-in | 2026-07-12 | 有 | 未查（無 feature 篇） | — | 1,591 |
| A42 | **Team-as-Agent**（`team_composer`／`role_turns`／`fault_attribution` + core `team_gate`／`task_packet`／`role_model_matrix`／`effort`） | team | `config.toml [team] enabled`（預設 **false**）＋`agent.toml [team]`；MCP `team_handoff` | 已接 goal_loop，但預設關且 gate 再預設 Solo | **uncommitted 2026-09** | 91（team_composer）＋各檔 | `docs/features/56`、`commercial/docs/DESIGN-team-as-agent-2026-09.md` | — | 11,415 |
| A43 | 角色×模型量表（`eval/matrix`／`team_probe`／`verifier_cell`／`stats`） | team／eval | `duduclaw eval --matrix`／`--team-2x2` | live 工具；但產出的 `role_model_matrix.toml` **composer 尚未讀取**（doc 56 自承） | **uncommitted 2026-09** | 有 | `docs/features/56` | — | 5,898 |
| A44 | `duduclaw eval` 本體（case／runner／assertions／judge／transcript／scaffold） | eval | `duduclaw eval <path>`＋十餘旗標 | live | 2026-08-07 | 有 | `docs/guides/evals.md` | — | 5,148 |
| A45 | RFC-26 Live Forking（`duduclaw-fork` crate + `mcp_fork*` + `ForkPage` + Prometheus） | fork | `agent.toml [fork] enabled`（預設 false）；MCP `fork_run`／`fork_cost` 等；`fork.list/inspect/resolve` RPC | opt-in 預設關，全堆疊已完成 | 2026-06-21～08-16 | 7＋各檔 | `docs/features/28`、`docs/todo/TODO-rfc26-live-forking.md` | — | 2,828 ＋ 2,161 |
| A46 | 微調與後訓練（`finetune/dataset`／`jobs`／`import`） | finetune | `finetune.datasets.*`／`finetune.jobs.*`／`finetune.import` RPC；儀表板頁 | live | 2026-09-07 | 有 | `docs/features/54` | 外部 LLaMA-Factory／雲端微調 API（本功能即是它們的前後處理層） | 3,983 |
| A47 | `embedding.rs`（ONNX 嵌入，供 prediction engine 語意相似度） | local inference | feature `onnx`；`prediction/engine.rs` | live（有生產消費者） | 2026-09-24 | 有 | 無專篇 | `duduclaw-memory::embedding` 只提供 cosine，不重疊 | 350 |

---

##### 2. 功能總表（乙：型別化決策欄）

> `verdict` ＝ {保留, 簡化, 取代, 淘汰} 機率分布。`noul` 四欄為機率。level 低於 L1 者視為 advisory。

| ID | verdict（保留/簡化/取代/淘汰） | obsolete | replaceable | unused | overeng | value | cost | removal_risk | level | 一句話理由（最強保留 ／ 最強淘汰） |
|---|---|---|---|---|---|---|---|---|---|---|
| A01 | 0.97/0.03/0/0 | 0.02 | 0.05 | 0.02 | 0.15 | 5 | 3 | 5 | L2 | 多 runtime 是平台的骨架，拔掉就沒有「換腦」這件事 ／ 無 |
| A02 | 0.92/0.08/0/0 | 0.05 | 0.25 | 0.10 | 0.30 | 5 | 4 | 5 | L2 | 帳號輪替／MCP 注入／sandbox 旗標各家不同，泛用驅動承不住 ／ 五份各 1–2.4k LOC 的平行實作，維護面很寬 |
| A03 | 0.95/0.05/0/0 | 0.02 | 0.05 | 0.25 | 0.10 | 4 | 2 | 3 | L1 | 一份實作換七家 CLI，是正確的收斂方向 ／ 七家是否有人真的在用未經查證 |
| A04 | 0.98/0.02/0/0 | 0.02 | 0.02 | 0.02 | 0.15 | 5 | 2 | 5 | L2 | 它是 A02/A03/A05 的共同權威表，刪不得 ／ 無 |
| A05 | 0.90/0.10/0/0 | 0.05 | 0.10 | 0.20 | 0.25 | 4 | 3 | 4 | L1 | 沒有這層，非 Claude runtime 對一般使用者不可達 ／ 三千多行只為「登入＋安裝＋列模型」 |
| A06 | 0.95/0.05/0/0 | 0.02 | 0.10 | 0.10 | 0.15 | 5 | 3 | 4 | L2 | Grok/DeepSeek/MiniMax API 模式 agent 全靠它拿到工具面 ／ 無 |
| A07 | 0.98/0.02/0/0 | 0.02 | 0.02 | 0.02 | 0.10 | 5 | 2 | 5 | L2 | 28 個檔案呼叫的單一咽喉，是 RFC-25 的成果 ／ 無 |
| A08 | 0.60/0.35/0.05/0 | 0.15 | **0.55** | 0.20 | 0.45 | 3 | 3 | 3 | L1 | 供應商層健康追蹤是 A09 做不到的粒度 ／ 平台同時有四套降級機制（A08/A09/A11/rotator Failover），語意邊界靠註解維持 |
| A09 | 0.80/0.15/0.05/0 | 0.05 | 0.40 | 0.05 | 0.10 | 4 | 1 | 3 | L2 | 183 行純函式＋獨立測試，便宜且天天在跑 ／ 與 A08 概念重疊 |
| A10 | 0.90/0.10/0/0 | 0.05 | 0.10 | 0.35 | 0.35 | 4 | 3 | 3 | L1 | 「什麼算完成」是平台最重要的決策點，做成真 seam 是對的 ／ 四個模式中三個預設沒人用，`judge_command` 刻意不開放 RPC |
| A11 | 0.15/0.05/0.05/**0.75** | 0.45 | 0.75 | **0.95** | 0.55 | 1 | 2 | 1 | **L2** | 冷卻＋context-window 候選過濾的抽象本身是對的 ／ `FallbackRouter`／`cooldown_for`／三個 COOLDOWN 常數／`CandidateOutcome` 全部只在 `lib.rs` re-export，crate 外零呼叫端，CLAUDE.md 已自承 |
| A12 | 0.98/0.02/0/0 | 0.02 | 0.05 | 0.02 | 0.20 | 5 | 4 | 5 | L2 | 一個 `ChatRequest` 打四種原生協定是這個平台的核心資產 ／ 無 |
| A13 | 0.95/0.05/0/0 | 0.05 | 0.10 | 0.05 | 0.15 | 4 | 2 | 4 | L2 | 價格／price-cliff 數學是成本控管的唯一事實來源 ／ 內建 ~15 個 model 需要人工跟上廠商 |
| A14 | 0.65/0.25/0/0.10 | 0.10 | 0.35 | **0.60** | 0.50 | 3 | 2 | 2 | L1 | 一個 `moa:` id 就能讓多家模型合議，是便宜的品質槓桿 ／ 需使用者手寫 `models.toml [moa.x]` 才存在，串流孿生 `stream_moa_model` 零消費者已在程式碼自承 |
| A15 | 0.98/0.02/0/0 | 0.02 | 0.05 | 0.02 | 0.25 | 5 | 5 | 5 | L2 | 非 CLI 後端的全部工具面都靠它 ／ `tool_loop.rs` 4,135 行單檔已超過專案自訂 800 行上限 |
| A16 | 0.60/0.30/0/0.10 | 0.05 | 0.15 | 0.50 | **0.55** | 4 | **5** | 4 | L1 | 可逆壓縮＋撤銷租約是市面少見的真本事，且 Unreleased 正在密集強化 ／ 1.1 萬行、四個 CLI 子命令、一個 HTTP 端點、11 個通道 adapter 的交付租約耦合，全部為了一個預設關且尚未進任何 release 的功能 |
| A17 | 0.45/0.35/0.05/0.15 | **0.55** | 0.35 | **0.70** | 0.45 | 2 | **5** | 3 | **L1** | 若 Anthropic 重啟 2026-06-15 的程式化用量拆分，這是零改碼的保險 ／ 前提在拆分當天被暫停、至今未恢復；`docs/features/27` 自承跨對話洩漏，2026-08 還得寫一次性遷移把誤開的 agent 關回去 |
| A18 | 0.85/0.15/0/0 | 0.20 | 0.20 | 0.50 | 0.15 | 3 | 1 | 2 | L1 | API key 帳號在 PTY 模式下的唯一可行路徑 ／ 只在 A17 開啟時可達 |
| A19 | 0.15/0.15/0.05/**0.65** | 0.50 | 0.40 | **0.90** | 0.60 | 1 | 3 | 1 | **L2** | 行程外隔離能讓 PTY 崩潰不拖垮 gateway ／ `build-release.sh` 只 build cli 與 gateway 兩個 package，`scripts/`／`.github/`／Dockerfile 全無此 binary 名稱——任何出貨安裝把 `worker_managed=true` 打開都找不到執行檔 |
| A20 | 0.05/0.05/0/**0.90** | 0.70 | 0.30 | **0.98** | 0.40 | 1 | 1 | 1 | **L2** | 未來要做 eager 重啟時可以接回 ／ crate 外零呼叫端，檔頭自承是 Phase 2 才會接線的 stub，已放了一年 |
| A21 | 0.35/0.20/0/0.45 | **0.75** | 0.10 | 0.55 | 0.20 | 2 | 1 | 2 | L1 | marker 檔讓它幾乎零成本地永久留著 ／ 它修的是 2026-07 儀表板的一次性事故，所有現役安裝早已跑過 |
| A22 | 0.70/0.25/0/0.05 | 0.25 | 0.35 | 0.55 | 0.20 | 2 | 1 | 2 | L0 | loopback 診斷端點在排查 PTY 問題時有用 ／ 2026-05-17 起未動，是領域內最舊的檔；儀表板 RPC 已能顯示同樣資訊 |
| A23 | 0.90/0.10/0/0 | 0.05 | 0.15 | 0.20 | 0.35 | 4 | 3 | 4 | L1 | 離線可用是值班機與隱私客戶的賣點 ／ 1,902 行 engine 為了一條多數人沒開的路徑 |
| A24 | 0.55/0.35/0.10/0 | 0.10 | **0.60** | 0.05 | 0.30 | 3 | 2 | 3 | L1 | 它獨有 UCCI 需要的 top-logprobs 擷取，`duduclaw-llm` 那份沒有 ／ 與 `duduclaw-llm/providers/openai_compat.rs` 是同一件事的第二份 reqwest 實作（548 vs 908 行） |
| A25 | 0.05/0.10/0.05/**0.80** | **0.80** | 0.85 | **0.95** | 0.30 | 1 | 1 | 1 | **L2** | `InferenceBackend` 的介面留著，未來真要 in-process 時有殼 ／ `generate()` 直接回 "not yet fully implemented"，且 `#[cfg]` 在 release 完全不編譯——文件卻還在宣傳它 |
| A26 | 0.30/0.20/0.05/0.45 | 0.45 | 0.65 | **0.90** | 0.35 | 2 | 2 | 2 | **L2** | mistral.rs 的 ISQ／PagedAttention 是真能力，真要走 in-process 時它比 llama.cpp 那份完整 ／ feature 預設關、release 腳本不帶，等同從未出貨 |
| A27 | 0.95/0.05/0/0 | 0.05 | 0.10 | 0.05 | 0.25 | 5 | 3 | 5 | L2 | 200K 懸崖的唯一即時防線，回覆路徑天天在跑 ／ 無 |
| A28 | 0.25/0.15/0.10/0.50 | 0.55 | **0.70** | **0.80** | 0.25 | 1 | 1 | 1 | L1 | 235B 級模型跨機是本地推論的唯一大模型路徑 ／ 2026-04-12 起未動、repo 內無任何範例 `inference.toml`、`openai_compat` 指向 Exo 端點即可達成同事 |
| A29 | 0.55/0.25/0.10/0.10 | 0.35 | 0.55 | 0.55 | 0.20 | 2 | 1 | 2 | L1 | 零安裝單檔推論對非技術使用者仍是最低門檻 ／ 值班機已改走 `llama-server`，一般機器走 `openai_compat`，三個 MCP 工具的維護面換不到多少 |
| A30 | 0.05/0.05/0.05/**0.85** | **0.80** | 0.80 | **0.95** | 0.25 | 1 | 1 | 1 | **L2** | Apple Silicon 本機生成是有價值的方向 ／ `MlxBridge::generate()` 全 repo 零呼叫端，只有 `is_available()` 餵狀態列；`docs/features/03` 宣稱的「供演化反思」在程式碼裡不存在 |
| A31 | 0.30/0.45/0.20/0.05 | **0.60** | **0.70** | 0.55 | 0.35 | 2 | 2 | 2 | L1 | 三層路由的骨架 A32 仍在用 ／ post-hoc 半邊的 α/β 是未擬合常數（等於固定門檻），已被同檔內有離線擬合腳本的 UCCI 取代 |
| A32 | 0.80/0.15/0/0.05 | 0.05 | 0.20 | 0.55 | 0.35 | 3 | 3 | 2 | L1 | 有真實擬合腳本與論文依據，是 A31 的誠實版 ／ 只有 1 個測試、外部 git rev 相依、預設關、尚未提交 |
| A33 | 0.45/0.25/0.05/0.25 | 0.20 | 0.35 | **0.70** | **0.60** | 2 | 2 | 2 | L1 | 「只有本地推論能做」的能力，是護城河型功能 ／ 預設關＋需人工呼叫 `jitrl_feedback`＋只有 openai-compat 一層有真偏置面，1,249 行換一個沒人按的按鈕 |
| A34 | 0.92/0.08/0/0 | 0.05 | 0.15 | 0.15 | 0.20 | 4 | 3 | 4 | L1 | 把「選本地模型」從術語問題變成一鍵問題，是 45 號文件的核心主張 ／ 精選清單需人工跟上 HF |
| A35 | 0.95/0.05/0/0 | 0.05 | 0.10 | 0.30 | 0.10 | 4 | 1 | 3 | L2 | 值班機零設定可用，是 OS 線的前提 ／ 只在 image 上可達 |
| A36 | 0.90/0.10/0/0 | 0.05 | 0.15 | 0.30 | 0.20 | 4 | 2 | 4 | L2 | 本地模型沒有它就拿不到 MCP 工具面 ／ opt-in |
| A37 | 0.98/0.02/0/0 | 0.02 | 0.02 | 0.02 | 0.35 | 5 | 5 | 5 | L2 | 訂閱配額輪替是這個產品省錢的根本 ／ 4,459 行單檔，遠超專案自訂 800 行上限 |
| A38 | 0.95/0.05/0/0 | 0.05 | 0.10 | 0.05 | 0.20 | 5 | 3 | 5 | L2 | 全帳號冷卻時的最後一條路 ／ 無 |
| A39 | 0.92/0.08/0/0 | 0.05 | 0.10 | 0.05 | 0.30 | 4 | 4 | 4 | L2 | 成本可見性是付費層的賣點之一 ／ 2,718 行 |
| A40 | 0.30/0.25/0/0.45 | 0.45 | 0.20 | **0.65** | 0.40 | 2 | 2 | 1 | L1 | 量測閘留著可在資料變多後重跑，避免憑感覺決定 Code Mode ／ 一次性決策工具，本機已得 INSUFFICIENT_DATA，Code Mode 尚未立案 |
| A41 | 0.70/0.20/0/0.10 | 0.15 | 0.30 | **0.60** | 0.25 | 3 | 2 | 2 | L1 | 讓 Aider／Cline 借用訂閱配額，是很實際的外部價值 ／ 無 feature 文件、2026-07 起未動、沒有證據有人在跑 |
| A42 | 0.85/0.15/0/0 | 0.05 | 0.10 | 0.55 | **0.55** | 4 | **5** | 4 | L1 | 「一位員工內部分四個角色、各自綁不同廠商模型」是強差異化，且 Unreleased 正在高密度推進 ／ 1.14 萬行、預設關、gate 再預設 Solo，且尚未進過任何 release |
| A43 | 0.70/0.25/0/0.05 | 0.10 | 0.15 | **0.60** | 0.50 | 3 | 4 | 3 | L1 | 「哪個角色該花錢」若沒有量測就只能憑感覺，這是唯一的外部尺 ／ `docs/features/56` 自承產出的 `role_model_matrix.toml` **composer 尚未讀取**——量了但沒有消費者 |
| A44 | 0.85/0.15/0/0 | 0.05 | 0.10 | 0.15 | **0.55** | 4 | 4 | 4 | L1 | 自我演化平台需要一把獨立於自家 verifier 的尺 ／ `--paired-seeds`／`--temperature` 兩個旗標在說明裡自承「no runtime in this build can consume」＝惰性旋鈕 |
| A45 | 0.45/0.25/0/0.30 | 0.30 | 0.35 | **0.65** | **0.60** | 2 | 4 | 2 | L1 | 並行分支＋judge 擇優是論文級能力，crate／MCP／儀表板／metrics 全套已完成 ／ 每 agent 預設關、2026-06 起只有零星維護、無任何使用痕跡；5,000 行換一個沒人開的開關 |
| A46 | 0.80/0.15/0/0.05 | 0.05 | **0.55** | 0.35 | 0.40 | 3 | 4 | 3 | L1 | 「你的對話與審批紀錄」是別人沒有的訓練語料，這是值班機的獨特價值 ／ 3,983 行本質是 LLaMA-Factory／雲端微調 API 的前後處理層，與「AI 員工平台」定位距離最遠 |
| A47 | 0.95/0.05/0/0 | 0.02 | 0.15 | 0.05 | 0.10 | 4 | 1 | 4 | L2 | prediction engine 的語意相似度有真實消費者 ／ 無 |

---

##### 3. 候選清單（通過門檻：verdict argmax ≠ 保留 且 conf ≥ 0.5，或任一 noul ≥ 0.6 且 level ≥ L1）

###### 3.1 可實錘刪除（L2，證據明確）

**C1｜A11 `duduclaw-llm::FallbackRouter`（451 LOC，8 測）**
- 最強保留：冷卻＋context-window 候選過濾是正確抽象，將來要做「模型級 fallback 鏈」時現成。
- 最強淘汰：`FallbackRouter`／`cooldown_for`／`RATE_LIMIT_COOLDOWN`／`BILLING_COOLDOWN`／`GENERIC_COOLDOWN`／`CandidateOutcome` 在 crate 外**全部零呼叫端**（只在 `lib.rs` 的 `pub use` 出現）；CLAUDE.md 已自承 "zero production call sites"。平台實際跑的是 A08＋A09＋rotator 三套。
- 一併處理：`crates/duduclaw-llm/src/router.rs` 整檔、`lib.rs:22` doc 行與 `lib.rs:71` 的 `pub use`、8 個單元測試、CLAUDE.md 該段落。**無 config 鍵、無 MCP 工具、無依賴 crate**。

**C2｜A30 `mlx_bridge.rs`（200 LOC）**
- 最強保留：Apple Silicon 本機生成方向正確，`MlxConfig` 已在 `inference.toml` schema 內。
- 最強淘汰：`MlxBridge::generate()` 全 repo 零呼叫端；唯一活路徑是 `is_available()` → `inference_status` 的一行顯示。`docs/features/03` 宣稱的「MLX Bridge enables local evolution reflections」在程式碼裡沒有對應路徑。
- 一併處理：`mlx_bridge.rs`、`config.rs:51` 的 `mlx` 欄位、`engine.rs` 的 `mlx` 欄位／`mlx_available()`、`mcp.rs:9069/9088` 狀態顯示、`handlers.rs:3459` 的 `"mlx"` pass-through section、`docs/features/03` 的 MLX 段、`lib.rs` doc 的 MLX 行。

**C3｜A25 llama.cpp in-process backend（92 LOC）**
- 最強保留：`InferenceBackend` 的第二個實作留著可證明 trait 抽象站得住。
- 最強淘汰：`generate()` 直接回 `"llama.cpp backend ... is not yet fully implemented. Use 'openai_compat' or 'mistral_rs' backend instead."`；`#[cfg(any(feature="metal","cuda","vulkan"))]` 且 `scripts/build-release.sh` 只帶 `duduclaw-gateway/dashboard`＝**出貨 binary 從不編譯它**。
- 一併處理：`llama_cpp.rs`、`lib.rs` 的 cfg mod、`Cargo.toml` 的 `llama-cpp-2` 相依與 `metal`/`cuda`/`vulkan` features（連帶 gateway `Cargo.toml:179-181`）、`docs/features/03` 的「llama.cpp — The C++ workhorse」段、`jitrl/mod.rs` 支援表那一列。

**C4｜A19 `duduclaw-cli-worker` ＋ `worker_supervisor`（2,080 ＋ 893 LOC）**
- 最強保留：行程外隔離讓 PTY session 崩潰不拖垮 gateway，是正確的可靠性設計。
- 最強淘汰：`scripts/build-release.sh` 只 build `-p duduclaw-cli -p duduclaw-gateway`；`scripts/`、`.github/`、Dockerfile 三處 grep `duduclaw-cli-worker` **全無命中**。supervisor 以「gateway binary 的兄弟檔或 PATH」解析 worker，所以任何出貨安裝打開 `worker_managed = true` 都只會拿到 `could not resolve binary`。它只在開發 checkout 可達。
- 一併處理：`crates/duduclaw-cli-worker` 整個 crate、workspace member、`worker_supervisor.rs`、`server.rs:2067` 的 spawn 點、`[runtime] worker_managed`／`worker_bind`／`worker_binary` 三個 config 鍵、Prometheus `worker_health_misses_total`／`worker_restarts_total`／`pty_pool_managed_worker_active`、`docs/features/27` 的 worker 段、`commercial/docs/TODO-cli-pty-pool-worker.md`。**注意：與 C6 綁在一起決定。**

**C5｜A20 `cli-runtime::Supervisor` / `RestartPolicy`（96 LOC）**
- 最強保留：真要做 eager 重啟時 80% 的殼已經在。
- 最強淘汰：crate 外零呼叫端；檔頭自承「Phase 1 ships the minimal contract; Phase 2 wires it into the pool's eviction logic」，Phase 2 至今未來。
- 一併處理：`supervisor.rs`、`lib.rs` 的 `pub mod supervisor` 與 `pub use supervisor::{RestartPolicy, Supervisor}`。

**C6｜A17 PTY pool 整體（5,421 ＋ 2,560 LOC）— 需使用者拍板，不建議我單方判死**
- 最強保留：它是針對「Anthropic 封鎖 OAuth 帳號的 `claude -p`」這個**存在過的**威脅的零改碼保險；2026-06-15 的拆分只是暫停，不是取消。
- 最強淘汰：拆分暫停已 15 個月未恢復；`docs/features/27` 自承「大多數 agent 應該關著」且有**跨對話 context 洩漏**的已知缺陷（pool key 無對話維度）；2026-08-04 還必須寫一次性遷移（A21）把儀表板誤開的設定關回去，並記錄了「90 分鐘內 4 次停滯、2 次帳號耗盡」的實地事故。維護面約 8,000 行。
- 選項：(a) 全留（現狀）；(b) 留 `oneshot_pty_invoke`（A18）＋刪 pool／worker（C4＋C5 一起）；(c) 全刪，等 Anthropic 真的動手再寫。
- 一併處理（若走 b/c）：`crates/duduclaw-cli-runtime`、`pty_runtime.rs`、`runtime_status.rs`、`pty_default_migration.rs`、`[runtime] pty_pool_enabled`／`pty_interactive_timeout_secs` 等鍵、`DUDUCLAW_DISABLE_PTY_POOL` 環境變數、`pty_pool_*` 全套 Prometheus 指標、`scripts/smoke-pty-pool.{sh,ps1}`、`docs/features/27`、`commercial/docs/runtime-pty-pool-design.md`、`portable-pty`／`dashmap` 相依。

###### 3.2 文件層過時（doc rot，L2，應立即修或刪）

**C7｜`docs/features/11-token-compression.md`（「Token Compression Triad」）**
- 最強保留：寫得很完整，有 metaphor 有圖。
- 最強淘汰：它描述的 Meta-Token／LLMLingua-2／StreamingLLM 三策略壓縮器**已於 v1.33 從 inference crate 移除**（`ls crates/duduclaw-inference/src/ | grep -i compress` 零結果，`lib.rs` 無 compression mod），而 `docs/features/README.md:23` 與 `docs/README.md:24` 仍在索引它。依使用者的 readme-and-docs 規範，過時文件比沒有文件更糟。
- 一併處理：改寫成 A27 `prompt_compression` 的說明，或刪檔＋同步兩個索引。另外 `handlers.rs:3450` 的 `inference.update` 仍接受 `llmlingua`／`streaming_llm` 兩個**死 config section**，同批清掉。

**C8｜`docs/features/03-confidence-router.md` 的後端章節**
- 最強保留：ConfidenceRouter 本體（A31 骨架＋A32）仍然活著，文件主體有效。
- 最強淘汰：它把 llama.cpp（C3，stub 且不編譯）、mistral.rs（A26，不出貨）、MLX（C2，dead）都寫成可用後端，並列出「Priority 1 Exo → Priority 2 llamafile」的優先序，與 `docs/features/53-local-models.md`（2026-09-07，`llama-server` + openai_compat）互相矛盾。
- 一併處理：03 與 53 的邊界重畫（03 保留路由概念、後端現況交給 53），順帶移除 03 的 MLX 段。

###### 3.3 unused ≥0.6 但需使用者判斷價值（L1）

| ID | 功能 | 最強保留 | 最強淘汰／取代 | 若淘汰要一併處理 |
|---|---|---|---|---|
| **A45** | RFC-26 Live Forking | 並行分支＋AI judge 擇優是論文級能力，crate／4 個 MCP 工具／`ForkPage`／Prometheus／`docs/features/28` 全套已完工 | 每 agent `[fork] enabled` 預設 false，2026-06 之後只有零星維護，repo 內找不到任何啟用痕跡；~5,000 LOC | `crates/duduclaw-fork`、`mcp_fork.rs`／`mcp_fork_exec.rs`、`fork_run`/`fork_cost` 等 MCP 工具與 `mcp_auth` scope、`fork.list/inspect/resolve` RPC、`web/src/pages/ForkPage.tsx` ＋ `nav.forks` 三語、`duduclaw_fork_*` metrics、`<home>/fork_store.db`、`agent.toml [fork]` 九個鍵、`docs/features/28`＋`live-forking.md`＋`docs/todo/TODO-rfc26-live-forking.md`、`DUDUCLAW_FORK_NO_EXEC` |
| **A33** | JitRL 零梯度續學 | 「只有本地推論做得到」的護城河型能力，有論文依據 | `[jitrl] enabled` 預設 false＋要人工呼叫 `jitrl_feedback` 才有樣本＋只有 openai-compat tier B 有真實偏置面（llama.cpp 那層是 stub）；1,249 LOC | `jitrl/` 五個模組、`inference.toml [jitrl]`、MCP `jitrl_feedback` ＋ `mcp_auth` 條目、`engine.rs` 的 jitrl 欄位與兩條路徑、`<home>/jitrl_experience.jsonl` |
| **A28** | Exo P2P 叢集 | 235B+ 模型跨機是本地推論唯一的大模型路徑 | 2026-04-12 起未動；repo 內**沒有任何範例 `inference.toml`**，等於沒有使用者可達的設定入口；`openai_compat` 直接指向 Exo 端點可達成同事 | `exo_cluster.rs`、`config.rs` 的 `exo` 欄位、`manager.rs` 的 `ExoCluster` 模式分支、`inference_mode` MCP 工具說明、`handlers.rs` 的 `"exo"` pass-through、`docs/features/03` |
| **A40** | Code Mode Phase 0 量測閘 | 留著可在資料量變多後重跑，避免憑感覺決定要不要做 Code Mode | 一次性決策工具，本機已產出 INSUFFICIENT_DATA，Code Mode 本身尚未立案；977 LOC | `tool_loop_probe.rs`、`duduclaw cost tool-loop` 子命令、`local_llm.rs:457/481` 的 probe 注入點、`types.rs:268` 的註解 |
| **A41** | `duduclaw proxy` 本機 OpenAI-compat 反向代理 | 讓 Aider／Cline／Codex 借用已付費的訂閱配額，對外部使用者是實打實的價值 | 2026-07-12 起未動、**無 feature 文件**、無使用痕跡；1,591 LOC | `proxy.rs`、`Commands::Maintenance(Proxy)`、proxy key 產生／儲存、`lib.rs` 註解 |
| **A21** | PTY 預設值一次性遷移 | marker 檔讓它幾乎零成本地永久留著 | 修的是 2026-07 儀表板的一次性事故，所有現役安裝早已跑過並留下 marker | `pty_default_migration.rs`、gateway 開機呼叫點、`<home>/migrations/wp10-pty-default-reset.done` |
| **A31** | legacy post-hoc 校準閘（A31 的一半） | 三層路由骨架 A32 仍在用，不能整個拔 | `post_hoc_alpha`/`beta` 是**未擬合**常數（實質＝固定在 mean logprob ≥ ln 0.5 的門檻），A32 UCCI 是同一決策的有擬合腳本版本；兩套並存於 `engine.rs:269` | `[router] post_hoc_enabled`／`post_hoc_alpha`／`post_hoc_beta`／`post_hoc_accept_threshold` 四個鍵、`engine.rs` 的 `post_hoc_enabled()` 分支、CLAUDE.md 該段誠實註記 |
| **A26** | mistral.rs backend | ISQ／PagedAttention／Speculative Decoding 是真能力，比 C3 那份完整 | feature 預設關、release 腳本不帶＝從未出貨；331 LOC + `mistralrs-core`／`indexmap`／`either` 三個 optional 相依 | `mistral_rs.rs`、`lib.rs` cfg mod、`Cargo.toml` 四個 feature 與三個相依、gateway `Cargo.toml:182-183`、`handlers.rs` 的 `"mistralrs"` pass-through |

###### 3.4 overengineered 訊號（不建議刪，建議簡化／收斂）

| ID | 訊號 | 建議 |
|---|---|---|
| **A08+A09+A11+rotator** | 同一個「失敗後換誰」的問題有**四套**機制：runtime 級 `FailoverManager`、模型級 `llm_fallback`、`FallbackRouter`（死）、rotator 的 `Failover` 策略。邊界只靠註解維持。 | 先刪 A11（C1），再決定 A08/A09 是否收斂成一層。 |
| **A15** | `tool_loop.rs` 4,135 行單檔，超過專案自訂 800 行上限 5 倍 | 拆檔（CCR 分支、provenance 分支、interceptor 分支各自成檔） |
| **A37** | `account_rotator.rs` 4,459 行單檔 | 同上 |
| **A16** | CCR 5,185 行單檔 + 3,804 gateway + 2,440 CLI | 若決定保留，至少拆 `ccr.rs` |
| **A44** | `duduclaw eval` 的 `--paired-seeds`／`--temperature` 在自己的說明裡承認「no runtime in this build can actually consume a seed」／「otherwise inert」 | 兩個惰性旗標：改成明確拒絕，或移除直到有 runtime 支援 |
| **A24** | `duduclaw-inference/openai_compat.rs`(548) 與 `duduclaw-llm/providers/openai_compat.rs`(908) 是同一件事的兩份 reqwest 實作 | 把 UCCI 的 top-logprobs 擷取上移到 `duduclaw-llm`，刪掉 inference 那份 |
| **A43** | 角色×模型量表產出的 `role_model_matrix.toml`，`docs/features/56` 自承 **composer 尚未讀取** | 量了沒人用＝目前是純報告；接上 composer，或先標記為「人工參考」 |

###### 3.5 高成本但主觀（列出供拍板，我不下淘汰判斷）

| ID | 功能 | 事實 | 我的看法 |
|---|---|---|---|
| **A16** | CCR | **11,429 LOC**、預設關、**未提交**、卻已橫切 11 個通道 adapter 的交付租約與 session 撤銷語意；Unreleased 有 10+ 條 CCR 修補 | 這是領域 A 最大的單一未提交面。技術上很紮實（撤銷租約、fail-closed principal、防跨使用者取回），但「一個尚未進 release 的預設關功能」已經改了通道層的訊息落庫語意，風險與投入不成比例。建議拍板：是否在下個 release 把它預設開／或先凍結新增面。 |
| **A42+A43** | Team-as-Agent | **17,313 LOC**、預設關、gate 再預設 Solo、未提交；Unreleased 幾乎全是它 | 差異化很強，但三個月的工程量押在一個雙層預設關的功能上。建議拍板一個「什麼條件下 `[team] enabled` 預設翻真」的判準，否則會重演 PTY pool 的命運。 |
| **A46** | 微調與後訓練 | 3,983 LOC、live、RPC 齊全 | 對「AI 員工平台」定位最遠的一塊；但它是值班機（OS 線）的獨特賣點。是否屬於領域 A 的核心，請使用者判斷。 |

---

##### 4. 我沒盤到的範圍（誠實列出）

1. **未執行任何建置或測試**：所有「測試有無／數量」是 grep `#[test]`/`#[tokio::test]` 的計數，不代表測試會過。
2. **未查生產使用痕跡**：我沒有讀 `~/.duduclaw/` 下任何真實 agent 的 `agent.toml`／`config.toml`。所有「預設關、無人開」的判斷來自**程式碼預設值與範例設定**，不是實機盤點。若使用者的實機有開啟 A17／A45／A33，我的 `unused` 機率應下修。
3. **`crates/duduclaw-inference/src/whisper.rs`（135 LOC）與 gateway `stt.rs`** 屬語音管線（`docs/features/14`），我判斷落在通道／語音領域，只確認了它有真實呼叫端（telegram／line／mcp），未做型別化決策。
4. **`ToolInterceptor` 去識別化 hook** 我只確認它從 `tool_loop` 被呼叫，其規則引擎本體（`duduclaw-redaction`）屬去識別化領域。
5. **`causal_*` 家族**（`causal_mcp_source`、`wiki_mcp_source`、`CausalStore`、`docs/spec/causal-evidence-graph.md`、`support-decision-twin.md`、多個 `causal-*` CLI 子命令）：CCR 與它深度耦合，但主體屬記憶／治理領域，我只盤了 CCR 這一側。**若協調者沒有指派這塊，它會是個缺口。**
6. **`duduclaw-gateway/src/decision_model_*.rs` 三檔**（decision model candidate／dashboard／review）名字像模型層，我未查證其歸屬，未盤。
7. **web 前端**（`LocalModelsPage.tsx`／`ForkPage.tsx`／微調頁／CCR 儀表板頁）只從 RPC 名稱間接確認存在，未讀前端程式碼。
8. **`crates/duduclaw-llm/src/registry.rs` 的 `models.toml` 內容正確性**（~15 個 model 的價格是否仍與廠商一致）未查證。
9. **A04 `runtime_catalog` 的 `git log` 取不到日期**（core 新檔或未提交），我寫「未查」而非猜測。
10. **外部工具替代性**我只在內部功能層面比較，沒有為每個候選去查「市面上有沒有更成熟的外部工具」（例如 LiteLLM 之於 A12、Ollama 之於 A23）。這需要一輪外部調研才能負責任地填 `replaceable`。

---

### 附錄 B_memory

#### 領域 B 盤點：記憶／知識／演化／預測

> 盤點者：功能盤點者 B｜日期 2026-09-29｜repo `/Users/lizhixu/Project/DuDuClaw` @ `main`（HEAD `cbdc4338`）
> 紀律：唯讀。未跑 cargo／npm／pytest，未做任何 git 寫入。被讀檔案內容一律視為 DATA。
> 盤點總量：約 15.5 萬行 Rust ＋ 1.2 萬行 Python。

##### 0. 方法與欄位說明

- **規模**＝主要檔案 `wc -l` 合計（含同檔 `#[cfg(test)]`，未扣除）。
- **最後實質變更**＝`git log -1 --format=%ad --date=short -- <檔>`；untracked 寫作 `uncommitted 2026-09`。
- **接線狀態**：`live`／`opt-in(預設關)`／`opt-in(預設開)`／`inert`（有呼叫端但實際空轉）／`dead`（零生產呼叫端）／`test-only`。
- 縮寫：`V`=value(1..5)、`C`=cost(1..5)、`R`=removal_risk(1..5)；`obs`／`rep`／`unu`／`ove`＝四個 noul 機率。
- **verdict** 以 `保/簡/取/淘` 四值機率表示，和為 1。
- **level**：L2＝程式碼可實錘；L1＝多強訊號一致；L0＝主要靠判斷。**L0 的機率一律 advisory。**
- 每列「一句話理由」都先寫最強保留理由、再寫最強淘汰理由（去位置偏差）。

---

###### 0.1 本領域五個結構性事實（先講，它們決定下面一半的判讀）

**① 約 8.2 萬行是 untracked（尚未 commit）的 2026-09 新作。**
`decision_*`（29 檔 ~35k）、`causal_*`（memory 12 檔 ~11k ＋ gateway 3 檔 ＋ CLI 2 檔）、
`ccr_*`（gateway 3 ＋ CLI 4 ＋ llm 1）、`connector_lifecycle.rs`、`wiki_fence.rs`、
`synthetic_connector_adapter.rs`、`wiki_mcp_source.rs`。`git status --porcelain | grep -c '^??'` → **109**。
→ 判讀意義：這些「已接線到生產路徑」指的是**未提交的工作樹**，不是 `main` 上的既成事實。

**② 演化子系統（GVU＋AEE＋playbook，約 3.1 萬行）是 fail-closed opt-in，出廠板模明寫關閉。**
`gvu/trigger.rs` 的 `agent_gvu_enabled()` 缺鍵一律 `false`；`crates/duduclaw-cli/src/wizard.rs:704`
產生的 agent.toml 寫死 `gvu_enabled = false`；`crates/duduclaw-cli/src/lib.rs:7190` 註解明寫
「shipping `gvu_enabled = true` to an unattended install is exactly the…」。
→ **出廠 agent 一個都不會演化，除非使用者自己去開。**
（注意區分：`[evolution] enabled` 這個總 kill-switch 預設 **true**（`types.rs:2445`），
所以技能熱路徑、reflexion、mistake_notebook 都是活的；只有 GVU/AEE 那一支預設關。）

**③ 文件與程式碼在「預設值」上對不起來（doc rot，L2 可實錘）。**
`CLAUDE.md:28` 與 `docs/features/feature-inventory.md:64` 都寫 `[task_forward_model]` **default off**；
`CLAUDE.md:29` 寫三個子開關「default off」。但
`crates/duduclaw-gateway/src/prediction/task_forward_store.rs` 的 `impl Default` 實際是
`enabled: true / calibration_enabled: true / held_out_gate_enabled: true`（v1.54 改的），
**而同一個檔案的欄位 doc comment 仍寫「(still default `false`)」——同檔自我矛盾**。
`feature-inventory.md:16` 又寫「Default ON」。三處敘述互相打架。

**④ `memory_factory` 的收斂只做了一半 → `[memory] novelty_gate`（預設開）在約 18 條路徑上靜默失效。**
`memory_factory::build_memory_engine`（`gateway/src/memory_factory.rs:79`）會掛
`.with_embedder(Arc::new(NgramHashEmbedder::new()))`，但下列生產呼叫端仍直接用
`SqliteMemoryEngine::new` 繞過它、因此**完全沒有 embedder**：
`profile_distill.rs:672`、`goal_loop.rs:1615`、`handlers.rs:12939/12945/12948` 與 15100-15800 RPC 區塊約 10 處、
`playbook_export.rs:37`、`playbook_migrate.rs:258`、`wizard.rs:389`、`cli/lib.rs:9865`、`migrate_from/apply.rs:39`，
**以及 `server.rs:1239` 的排程 decay job**。
→ 這正是 `memory_factory.rs:1-24` 當初寫來要修的那一類 bug，尚未修完。

**⑤ `[evolution]` 的 11 個技能旋鈕是「只寫不讀」。**
`skill_synthesis_enabled` / `skill_synthesis_threshold` / `skill_synthesis_cooldown_hours` / `skill_trial_ttl` /
`skill_graduation_enabled` / `skill_graduation_min_lift` / `skill_recommendation_enabled` /
`curiosity_enabled` / `curiosity_threshold` / `curiosity_max_daily` / `skill_behavior_monitor_enabled`
——全部由 `handlers.rs:2086-2121`（dashboard `evolution_advanced`）驗證並寫入 agent.toml，
`duduclaw-core/src/types.rs:2187-2229` 有型別，**但 gateway 端零讀取端**。
其中四個的實際值被**硬編碼**在 `channel_reply.rs`：
`SkillActivationController::new(5)`（`:631`，忽略 `max_active_skills`）、
`GapAccumulator::new(3, 24)`（`:633`，忽略 `skill_synthesis_threshold`／`_cooldown_hours`）、
`GraduationCriteria::default()`（`:4994`，忽略 `skill_graduation_min_lift`）。
→ **使用者在 dashboard 調了會以為生效，其實不會。**

---

##### 1. 功能總表

###### 群組 B1 — 記憶核心（`crates/duduclaw-memory`，34,807 行，371 測試）

> 全 crate **無 `[features]` 段落**，零模組被 feature gate，全部 `pub mod` 無條件編譯。

| # | 功能名 | 入口 | 接線 | 最後變更 | 文件 | 重疊／可取代者 | LOC | verdict 保/簡/取/淘 | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B1.1 | SqliteMemoryEngine 認知記憶（episodic/semantic＋3D 加權檢索） | MCP `memory_store`/`_search`/`_read`/`_search_by_layer`；RPC `memory.*`（11 個） | live | 2026-08-13 | `docs/features/10`、`20`、`docs/guides/memory-and-knowledge.md` | 無內部替代 | 6071 | .95/.05/0/0 | .02 | .05 | .02 | .25 | 5/4/5 | L2 | 保：平台記憶唯一真身，11 條注入路徑靠它；淘：單檔 6071 行是維護熱點（簡化題不是淘汰題） |
| B1.2 | Temporal memory／SPO 三元組（F1） | `store_temporal`；MCP `memory_get_history`/`_get_at`/`_invalidate_by_origin` | live | 2026-08-13 | `docs/features/20` | 無 | 併入 engine | .95/.05/0/0 | .02 | .05 | .05 | .2 | 5/3/5 | L2 | 保：supersession 讓舊事實不污染新回答；淘：無 |
| B1.3 | HippoRAG-lite 圖檢索（PPR） | 自動併入 `search()` 重排 | live（僅 crate 內呼叫） | 2026-07-20 | `feature-inventory:313` | 無 seed 命中時逐位退回 FTS | 760 | .85/.12/0/.03 | .05 | .15 | .1 | .35 | 4/3/3 | L1 | 保：零 seed 命中時逐位退回 FTS，風險已隔離；淘：`w_graph=0.15` 的實際增益**從未量測** |
| B1.4 | Ebbinghaus 遺忘曲線衰減 `decay.rs` | `server.rs:1222-1249` 開機 5 分後每 24h；RPC `memory.decay_overview`/`memory.forget` | live（排程） | 2026-07-20 | `docs/features/10` | 與 janitor/lifecycle **不重疊**（三個不同對象，已證實） | 358 | .9/.08/0/.02 | .05 | .1 | .05 | .2 | 4/2/4 | L2 | 保：真的每天在跑，且有 dashboard 面；淘：政策（30/90 天）硬編在 `server.rs:1229-1232`，非設定 |
| B1.5 | novelty_gate 反假驚訝寫入閘 | `[memory] novelty_gate` **預設 true**；經 `memory_factory` 掛 embedder | opt-in(預設開)，**但 ~18 路徑繞過** | 2026-08-07 | `feature-inventory:66` | playbook/dedup（共用 0.92 同一把尺） | 411 | .85/.12/0/.03 | .05 | .1 | .25 | .2 | 4/2/3 | **L2** | 保：與 playbook dedup 共用門檻是好設計，且 `novelty_gate=false` 時是「不掛 embedder」＝結構性 no-op（刻意）；淘：§0.1④ 的 18 個繞過點讓它在排程 decay 等路徑上**靜默失效** |
| B1.6 | origin.rs 寫入來源信任綁定（TMA-NM） | 所有寫入路徑；`origin_trust = min(caller, ceiling, derived_from)` | live | 2026-07-22 | `feature-inventory:83` | 無 | 160 | .95/.05/0/0 | .02 | .05 | .02 | .1 | 5/1/5 | L2 | 保：160 行擋住 Sybil 自我背書，全領域投報率最高；淘：無 |
| B1.7 | trust_store.rs（wiki 即時信任權威存放） | RPC `wiki.trust_audit`/`_history`/`_override`；MCP `global_trust_store`；`server.rs:620` 開機初始化；`[wiki.trust_feedback]` | **live（熱路徑）** | 2026-08-15 | 未查（無 `docs/features/NN`） | 無 | 2971（23 測試） | .85/.12/0/.03 | .05 | .05 | .05 | .45 | 4/4/4 | L2 | 保：frontmatter `trust` 只是快照、真值在這裡，讓信任調整不必每次改寫 md；淘：2971 行＋4 個 config 鍵服務一個沒有專屬功能文件的機制 |
| B1.8 | agent wiki（L0–L3 信任層） | MCP `wiki_*` 14 個；RPC `wiki.*` 約 9 個 | live（熱路徑） | 2026-06-22 | `docs/features/17` | 記憶層（刻意分工） | 3568 | .9/.08/0/.02 | .05 | .1 | .05 | .3 | 5/4/5 | L1 | 保：知識／記憶分離是產品賣點；淘：3568 行單檔＋14 MCP 工具，工具表膨脹有成本 |
| B1.9 | `vector.rs` SQLite 向量檢索 | `engine.rs:771/2106` store、`:3420` `vector_knn` | live | 2026-07-20 | `feature-inventory:99` | 無 | 345 | .85/.12/0/.03 | .05 | .1 | .1 | .25 | 4/2/4 | L2 | 保：engine 的 store/search 直接用它；淘：`NgramHashEmbedder` 是零依賴 n-gram hash，語意能力本就有限 |
| B1.10 | `embedding::cosine_similarity` | `prediction/engine.rs:955`、`playbook/dedup.rs:86` ＋ crate 內 3 處 | live | 2026-03-15 | — | 無 | ~40/167 | .9/.08/0/.02 | .05 | .05 | .05 | .05 | 4/1/4 | L2 | 保：5 個呼叫端，是 novelty_gate 與 playbook dedup 的共用尺；淘：無 |
| B1.11 | **`embedding::VectorIndex`（記憶體內索引）** | 無 | **dead** | 2026-03-15 | — | **`vector::vector_knn`（SQLite 版）已取代** | ~60/167 | .05/.05/.15/**.75** | **.85** | **.9** | **1.0** | .3 | 1/1/1 | **L2** | 保：想不出來；淘：全 repo 只有自身定義、自身 4 個測試、與 `lib.rs:38` 的 re-export，被 SQLite 版取代 |
| B1.12 | `graph_embed_seed` 圖檢索向量播種 | `MemoryConfig.graph_embed_seed`，**預設 false**（`engine.rs:97`） | opt-in(預設關) | 2026-08-13 | `feature-inventory:313` | B1.3 的純 FTS seed | 併入 engine | .55/.2/0/.25 | .3 | .3 | **.6** | .4 | 2/2/2 | L1 | 保：與 B1.9 同一組，關掉只是退回 FTS-only；淘：預設關、無 dashboard 路徑，實質零使用者開啟 |
| B1.13 | GDPR export／erase | CLI `duduclaw ops gdpr export\|erase`（`lib.rs:4811-4821`）；**無 dashboard RPC** | live | 2026-07-20 | `feature-inventory:101` | 無 | 436 | .95/.05/0/0 | .05 | .05 | .15 | .15 | 4/2/5 | L2 | 保：合規題，不能用「沒人跑過」當淘汰理由；淘：無 |
| B1.14 | `memory bench`（PPR 延遲量表） | CLI `duduclaw ops memory bench`（`lib.rs:4823-4828`） | live | 2026-07-11 | `feature-inventory:102` | 無 | 151 | .8/.15/0/.05 | .1 | .2 | .25 | .1 | 3/1/2 | L2 | 保：LightRAG 量測閘的落地，151 行極便宜；淘：半年未動，使用頻率未知 |
| B1.15 | `code_map.rs`（Aider 式 RepoMap） | MCP **`code_map`**（`mcp.rs:772`/`13819`，scope `MemoryRead`） | live | 2026-07-11 | 未查 | 無（正是 MEMORY.md 說的 P0 缺口的填補） | 639 | .85/.12/0/.03 | .05 | .05 | .15 | .25 | 4/2/4 | L2 | 保：tree-sitter 符號抽取＋PPR，是記憶檔點名的「Aider RepoMap P0 缺口」的答案；淘：無專屬文件，7 個測試偏薄 |
| B1.16 | `user_code.rs`（User-as-Code 偏好編譯） | MCP **`user_code_profile`**（`mcp.rs:833`/`13789`） | live（**doc 說自己是死的，錯**） | 2026-07-12 | 無 | 無 | 1388（15 測試） | .8/.15/0/.05 | .1 | .1 | .1 | .35 | 4/3/3 | **L2** | 保：把時序事實編譯成型別化偏好/約束規則、有確定性衝突解；淘：`user_code.rs:16-18` 檔頭寫「READ-ONLY EXPERIMENT…No production path consumes it yet」——**這句已經是假的**，會害人誤刪活工具 |
| B1.17 | `user_profile.rs` | MCP `user_profile_record`/`user_profile_get` | live | 2026-08-04 | 未查 | — | 367 | .85/.1/0/.05 | .05 | .1 | .1 | .15 | 4/1/4 | L2 | 保：兩個 MCP 工具直達；淘：無 |
| B1.18 | `feedback.rs`（CitationTracker／TrustSignal，無 SQLite） | `dispatcher.rs:283/286/869/871`、`channel_reply.rs` 4 處、`metrics.rs` 2 處 | **live（熱路徑）** | 2026-05-03 | 未查 | **不是**被 `prediction/feedback_bus.rs` 取代——後者是它的**消費者**（`feedback_bus.rs:26` 直接 import） | 604（12 測試） | .9/.08/0/.02 | .05 | .05 | .02 | .15 | 4/2/4 | **L2** | 保：約 10 個外部呼叫端、每次 dispatch 都在用，是 crate 內最熱的模組之一；淘：無（我原本猜它與 feedback_bus 撞車，**證實猜錯**） |
| B1.19 | `lifecycle.rs`（agent 記憶改派） | RPC **`agents.handoff`**（`handlers.rs:6396`） | live（事件驅動） | 2026-07-20 | 未查 | **不是保留機制**（是 re-key，與 decay/janitor 無關） | 486 | .85/.1/0/.05 | .05 | .1 | .15 | .15 | 4/1/4 | L2 | 保：離職交接功能的後端；淘：無 |
| B1.20 | `janitor.rs`（wiki 頁面＋信任列清理） | `server.rs:680` restart-aware 每日；`[wiki.trust_feedback.janitor]` 6 個鍵 | live（排程） | 2026-05-03 | 未查 | 與 decay 不重疊（不同資料存放） | 479 | .85/.1/0/.05 | .05 | .1 | .1 | .3 | 4/2/4 | L2 | 保：真的每天在跑且會持久化 `last_janitor_run_at`；淘：6 個 config 鍵無文件 |
| B1.21 | `import.rs`（CSV/JSON/JSONL 匯入） | **只有** `duduclaw tooling wizard`（`wizard.rs:394/397/399`） | live（極窄） | 2026-07-22 | 無 | 與 `duduclaw migrate-from` **無關**（後者走 `migrate_from/apply.rs:39` 的 raw engine） | 368 | .6/.25/0/.15 | .2 | .3 | .35 | .15 | 2/1/2 | L1 | 保：wizard 首次設定的匯入路徑；淘：唯一入口是 wizard，且 `wizard.rs:382` 寫到 `home/memory/{agent}.db`——**與其他所有呼叫端用的 `home/memory.db` 不同路徑**，匯入的資料可能落在沒人開的 DB（未證實是否刻意） |
| B1.22 | `router.rs`（CoALA 層別分類，零 LLM） | `mcp_memory_handlers.rs:132`、`mcp.rs:5074` | live | 2026-04-08 | 無 | 與 `prediction/router.rs` **無重疊**（不同領域，同名易混） | 168 | .85/.1/0/.05 | .05 | .1 | .1 | .1 | 4/1/4 | L2 | 保：兩個生產呼叫端、零 LLM 分類；淘：`classify` 這個名字在同 crate 的 `origin.rs:77` 也有一個（不同領域），命名危險 |
| B1.23 | **`search.rs`** | 無。**連 `lib.rs` 都沒有 `pub use`** | **dead** | **2026-03-28** | 無 | `engine.rs` 的 FTS5＋PPR＋vector_knn 混合檢索 | 100（3 測試） | .02/.03/.1/**.85** | **.9** | **.95** | **1.0** | .1 | 1/1/1 | **L2** | 保：想不出來；淘：`rank_results`／`filter_by_tags` 全 repo 零呼叫端、未 re-export、是樸素子字串計數，早被 engine 的混合檢索取代 |
| B1.24 | `sensitivity.rs` | engine metadata 路徑 | live | 2026-07-23 | 無 | — | 94 | .85/.1/0/.05 | .05 | .1 | .1 | .05 | 3/1/3 | L1 | 保：94 行、engine 直接用；淘：無 |
| B1.25 | `night.rs`（N3 schema induction＋N4 recurrence consolidation，零 LLM） | `night_engine.rs:476/494`；`[night_engine] enabled` **預設 false** | opt-in(預設關) | 2026-07-22 | 無 | 與 gateway `night_engine.rs` **不重疊**（刻意分工：這裡是確定性半邊） | 853（14 測試） | .65/.2/0/.15 | .2 | .15 | **.6** | .35 | 3/2/3 | L1 | 保：零 LLM 的確定性整理，分工清楚；淘：上游雙層 opt-in 且預設全關（見 B6.1），實質零執行；另有 7 個 pub 匯出零外部呼叫端 |
| B1.26 | `wiki_fence.rs`（per-directory 交付柵欄） | `mcp.rs:15470/15621`、`wiki_mcp_source.rs:16/72-73/172/175`、`feedback_bus.rs:145/199` | live | **uncommitted 2026-09** | 無 | 無 | 532（8 測試） | .8/.15/0/.05 | .05 | .1 | .15 | .3 | 4/3/3 | L2 | 保：解掉「A agent 寫頁擋住 B agent 讀」的真併發問題，且 `FENCE_BUSY_PREFIX` 用錨定前綴符合本專案第 2 條編碼慣例；淘：`WikiMutationGuard` 已 `pub use`（`lib.rs:65`）但零外部呼叫端 |
| B1.27 | causal_* 因果證據圖（12 檔） | CLI 6 個 `causal-*`；HTTP 25 條 `/api/causal/*`（admin）；MCP `get_source`（server `causal-mcp`）；`[causal_extraction]` 整段註解掉＝預設關 | live（程式碼無條件編譯＋`server.rs:937` 開機 spawn outbox）／設定面預設關 | **uncommitted 2026-09** | `docs/spec/causal-evidence-graph.md`（Draft） | 與 engine SPO **不重疊**（22 張 `causal_*` 專屬表，同一個 `memory.db` 檔，`causal.rs:997` 已處理 `user_version` 共用風險） | ~11,000（73 測試，`causal_negative_control.rs` **0**） | .5/.2/.05/.25 | .15 | .1 | .45 | **.75** | 3/5/4 | L1 | 保：`causal_memory.rs` 用 SQLite trigger 讓上游 quarantine/forget/GDPR 刪除**原子地**撤銷複製文字——把「證據可撤銷」做進儲存層；淘：spec 自寫「does not yet establish a production causal effect」「Real extraction accuracy is not yet measured」，1.1 萬行換一個自承尚未成立的能力 |

###### 群組 B2 — 預測與校準（`prediction/`，17,807 行，300 測試）

| # | 功能名 | 入口 | 接線 | 最後變更 | 文件 | 重疊／可取代者 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B2.1 | PredictionEngine＋Dual Process Router | `channel_reply.rs:5089` 每輪 `prediction::router::route()` | live | 2026-08-13 | `docs/features/01` | 無 | 1105+436 | .95/.05/0/0 | .05 | .05 | .02 | .2 | 5/3/5 | L2 | 保：90% 對話零 LLM 成本那條路就是它；淘：無 |
| B2.2 | MetaCognition 自校準門檻 | `prediction/engine.rs`；`dispatch_engine.rs:3198` | live | 2026-08-07 | `feature-inventory:206` | 無 | 1307 | .8/.17/0/.03 | .05 | .1 | .05 | **.5** | 4/3/4 | L1 | 保：門檻單向漂移是實際修過的 bug；淘：1307 行做「每 100 次預測調兩個閾值」，複雜度明顯超過問題 |
| B2.3 | rule_lifecycle 規則生命週期（ACE/ExpeL＋Janus probation） | `channel_reply` 注入 `## Learned Rules`；settle 後結算 | live | 2026-08-13 | `docs/features/20`、`38` | playbook（刻意共用同一張表） | 2029 | .9/.08/0/.02 | .05 | .1 | .05 | .35 | 5/4/4 | L2 | 保：規則會自己死掉，否則 prompt 被陳年規則稀釋；淘：與 playbook 疊在同一個 metadata blob，兩套概念共用一張表，理解成本高 |
| B2.4 | rule_staleness 來源過期 | `playbook/sweep.rs:117`、`reflexion.rs:434`、`playbook/select.rs:23` | live | 2026-08-13 | 未查 | — | 494 | .85/.12/0/.03 | .05 | .1 | .1 | .3 | 4/2/3 | L2 | 保：三個真呼叫端；淘：無專屬文件 |
| B2.5 | calibration.rs（Brier/RPS＋Murphy 分解） | `[task_forward_model] calibration_enabled` **實際預設 true**；RPC `forward.calibration` | live | 2026-08-10 | `docs/features/39` | 無 | 839 | .9/.08/0/.02 | .05 | .05 | .1 | .3 | 5/3/4 | L2 | 保：proper scoring 是「誠實回報」的數學版，Foresight 頁直接吃；淘：無 |
| B2.6 | rule_gate held-out 晉升閘 | `[task_forward_model] held_out_gate_enabled` **實際預設 true** | live | 2026-08-10 | `docs/features/39` | 無 | 451 | .9/.08/0/.02 | .05 | .05 | .1 | .35 | 5/3/4 | L2 | 保：擋住 agent 自說自話的規則晉升；淘：無 |
| B2.7 | 任務前向模型（task_forward／task_observe／transition／task_forward_store／tool_class） | `[task_forward_model] enabled` **實際預設 true**（文件說 false）；`dispatch_engine.rs:3221` | live | 2026-08-14 | `docs/features/39`；`CLAUDE.md:28` 敘述已過期 | 無 | ~4,395 | .85/.12/0/.03 | .05 | .05 | .05 | .4 | 4/4/4 | L2 | 保：predict-act-verify 是 LWM 線地基；淘：四層降級鏈＋三個自承「parsed but not consumed」的欄位（`cold_start_llm`／`min_samples`／`mature_n`），旋鈕多過用途 |
| B2.8 | task_rule_induce（A4 確定性任務規則歸納） | `dispatch_engine.rs:3419`；`rule_induction` 預設 true | live | 2026-08-10 | `CLAUDE.md:28` | reflexion F2b（寫同一 store） | 705 | .8/.15/0/.05 | .05 | .2 | .1 | .4 | 4/3/3 | L1 | 保：零 LLM 成本的規則來源；淘：與 B3.12 是兩條寫同一張表的管線 |
| B2.9 | forward_view（Foresight 頁後端） | RPC `forward.summaries`/`.recent`/`.chain`/`.calibration`（`handlers.rs:23328+`） | live | 2026-08-15 | `feature-inventory:35` | 無 | 1041 | .9/.08/0/.02 | .05 | .05 | .05 | .25 | 4/3/4 | L2 | 保：把 LWM 迴圈變成人看得到的頁面；淘：無 |
| B2.10 | **foresight_gate.rs（WP-B4 選擇性前瞻閘）** | 無。只有兩個 `DEFAULT_*` 常數被 `task_forward_store.rs:150-151` 借用 | **dead** | 2026-08-07 | 只有 `CLAUDE.md:28` 一句宣稱它已生效 | — | 304（13 測試） | .1/.05/0/**.85** | **.85** | .3 | **.95** | .5 | 1/1/1 | **L2** | 保：primitive 已寫好，D 系列 HITL 介面接上就能用；淘：**檔頭 19-20 行自寫「No consumer wired in this change」**，`evaluate_foresight_gate`／`foresight_gate`／`ForesightGateReasons` 全 repo 零外部引用，而 `CLAUDE.md:28` 把它寫成已生效 |
| B2.11 | belief.rs 信念迴圈 | MCP `belief_submit`/`_settle`/`_stats`；RPC `belief.recent`/`.summary`；`[belief] flat_band_pct` | live | 2026-08-15 | `docs/features/46` | calibration.rs（刻意共用統計方言） | 1539 | .75/.15/0/.1 | .1 | .15 | .25 | .45 | 3/4/3 | L1 | 保：刻意做成領域無關、且 stats 注入被標成「可評估的實驗」而非既成事實（誠實）；淘：1539 行＋3 MCP 工具＋1 dashboard 分頁，而「注入歷史能改善校準」作者自承**沒有一手證據** |
| B2.12 | feedback_bus（TrustFeedbackBus） | `channel_reply.rs:4725` | live | 2026-07-04 | 未查 | **不是**取代 `memory/feedback.rs`——是它的**消費者** | 439 | .8/.15/0/.05 | .1 | .15 | .15 | .25 | 3/2/3 | L2 | 保：橋接記憶層信任訊號與 wiki trust_store；淘：只有單一呼叫端 |
| B2.13 | subagent_prediction（GvuTriggerCtx） | `dispatcher.rs:384/1088`、`server.rs:1202` | live | 2026-06-17 | 未查 | — | 408 | .8/.15/0/.05 | .1 | .1 | .15 | .2 | 3/2/3 | L1 | 保：三個真呼叫端；淘：下游 GVU 預設關→實際多半空轉 |
| B2.14 | forced_reflection（沉默破冰） | `server.rs:1211-1212` spawn | live | 2026-06-17 | `docs/guides/evolution-switches.md` | — | 327 | .75/.2/0/.05 | .15 | .1 | .2 | .25 | 3/2/3 | L1 | 保：開機就 spawn；淘：產出丟給預設關的 GVU |
| B2.15 | user_model／outcome／metrics（會話訊號抽取） | `channel_reply.rs:4625/4654`；`skill_lifecycle` 借 `RunningStats` | live | 2026-04~06 | 未查 | — | 390+430+586 | .85/.1/0/.05 | .1 | .1 | .05 | .15 | 4/2/4 | L2 | 保：每輪對話都跑，是 B2.1 的輸入；淘：`metrics.rs` 最後動於 2026-04-17 |

###### 群組 B3 — 演化（`gvu/*` 25,339 行＋`playbook/*` 5,971 行，367＋102 測試）

> **共同前提**：`agent.toml [evolution] gvu_enabled` 缺鍵 fail-closed 為 `false`，出廠 wizard 寫死 `false`。
> 下列「live」多數受此總閘制約——**程式碼活著，但出廠 agent 不會跑到它**。B3.11／B3.12／B3.13 是例外（不受此閘）。

| # | 功能名 | 入口 | 接線 | 最後變更 | 文件 | 重疊／可取代者 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B3.1 | AEE 主迴圈（`gvu/aee/*`） | `gvu/loop_.rs:478` → `aee::run_aee_round`；`[evolution] strategy`；`duduclaw eval` subprocess | opt-in(預設關) | 2026-09-23 | `docs/features/38`、`docs/architecture/evolution-engine.md` | 無 | 4,547 | .7/.2/0/.1 | .1 | .1 | **.55** | .55 | 4/5/4 | L1 | 保：v3 預設演化路徑，Gate/Measure 拆分＋champion 整份快照是真正的設計進步；淘：出廠全關＋每條 entry 需 ≥1 個 EvalCaseRef 才能寫入＝門檻極高，很可能零 agent 真跑過完整一輪 |
| B3.2 | playbook 基因化規則集（entry/store/select/delta/gene/dedup/signals/assertions/sweep） | RPC `playbook.list`/`.export`/`.retire`；CLI `duduclaw playbook export`/`migrate-soul`；Memory 頁「自主學習」分頁 | live（讀/注入路徑）／opt-in(寫路徑隨 AEE) | 2026-08-13 | `docs/features/38` | rule_lifecycle（共用同一張表） | 5,166 | .8/.15/0/.05 | .1 | .15 | .3 | .5 | 4/4/4 | L1 | 保：`select` 注入路徑不受 gvu_enabled 影響，是活的；淘：寫入路徑與 AEE 綁死＝五千行裡大半只在「有人開 AEE」時才有意義 |
| B3.3 | playbook/humanize.rs（零 LLM zh-TW 口語化層） | `handlers.rs:23738` → RPC 回 `humanized`；前端 `MemoryPage.tsx:738` 消費 | **live（端到端接通）** | 2026-08-12 | `docs/features/38` | 無 | 805 | .85/.1/0/.05 | .05 | .1 | .05 | .4 | 4/2/3 | L2 | 保：Rust→`api.ts:1813`→`MemoryPage.tsx` 完整接通，是「不當黑盒」的落地；淘：805 行純模板拼句，可簡化 |
| B3.4 | GVU legacy SOUL.md 演化（loop_/generator/verifier/updater/version_store/observation_finalizer/text_gradient/proposal/soul_partition） | `[evolution] legacy_soul_evolution = true` **且** `gvu_enabled = true`（雙層）；CLI `duduclaw evolution finalize` | opt-in(預設關，且被 AEE 取代) | 2026-08-11 | `docs/features/02`、`06`；`feature-inventory:185-192` 明寫 non-default legacy | **AEE（B3.1）是官方繼任者** | ~6,900 | .35/.2/**.35**/.1 | **.75** | **.65** | .6 | .5 | 2/5/3 | **L2** | 保：SOUL.md 版本化＋SHA-256 指紋＋24h 觀察窗＋自動回滾是唯一能把人格改回來的機制，且 `observation_finalizer` 被 `server.rs`（30 分）與 CLI 真呼叫；淘：官方已宣告 legacy、SOUL.md 對 agent **唯讀**——**它守護的那條寫入路徑本身已被封了** |
| B3.5 | **gvu/shadow_mode.rs** | 無。只有 `gvu/mod.rs:32` 的 `pub mod` | **dead** | **2026-04-04** | 無 | — | 212（**0 測試**） | .05/0/0/**.95** | .8 | .3 | **1.0** | .4 | 1/1/1 | **L2** | 保：想不出來；淘：5 個 pub 項目全 repo 零外部引用、零測試、半年未動 |
| B3.6 | **gvu/diversity.rs** | 無。`gvu::diversity` 在全 repo 只出現在 mod.rs | **dead** | **2026-07-04** | `gvu/mod.rs:18` 註解仍在宣傳它 | — | 297（**0 測試**） | .05/0/0/**.95** | .8 | .3 | **1.0** | .4 | 1/1/1 | **L2** | 保：想不出來；淘：3 個 pub 型別零外部引用、零測試，而模組文件還在宣傳 |
| B3.7 | consolidate.rs（SOUL.md 超額壓縮） | `loop_.rs`(10)、`updater.rs`(4)、`server.rs`(1) | live（在 B3.4 legacy 路徑下） | 2026-08-12 | `CLAUDE.md:26` | — | 1014 | .5/.2/.2/.1 | .5 | .35 | .4 | .5 | 2/3/3 | L1 | 保：修掉「SOUL.md 超標＝永久單向閥死結」的真 bug；淘：它保護的 legacy 路徑本身待汰 |
| B3.8 | stagnation.rs（停滯偵測） | `server.rs`（30 分掃描）、RPC `evolution.stagnation`、`channel_alerts.rs` | live | 2026-08-20 | `feature-inventory:209` | — | 1366 | .75/.2/0/.05 | .1 | .15 | .3 | .45 | 3/3/3 | L1 | 保：「死路變可見」是誠實回報原則的落地；淘：1366 行偵測一個預設關閉的子系統是否停滯 |
| B3.9 | reward_hack.rs（H1-H4 獎勵駭客稽核） | `aee/inner_loop.rs`（折進 G-Contract） | live（AEE 內） | 2026-08-07 | `CLAUDE.md:27` | — | 312 | .85/.1/0/.05 | .1 | .05 | .3 | .25 | 4/1/3 | L1 | 保：312 行防演化系統自我作弊，投報率高；淘：只在 AEE 跑起來時有作用 |
| B3.10 | telemetry／knob_snapshot／champion | `aee/run.rs`、`handlers.rs`、`verifier.rs`、`updater.rs`、CLI | live（AEE 內） | 2026-08-15 | `CLAUDE.md`「第六波」 | — | 401+190+275 | .8/.15/0/.05 | .1 | .1 | .3 | .35 | 3/2/3 | L1 | 保：champion 整份快照擋住「一條改善藏三條退步」；淘：同上 |
| B3.11 | mistake_notebook.rs（跨迴圈錯誤記憶＋TrajectoryEvidence） | `channel_reply.rs`（14 處）、`reflexion.rs`（3）、`server.rs`、`subagent_prediction.rs` | **live（不受 gvu_enabled 影響）** | 2026-08-07 | `feature-inventory:204` | — | 1533 | .9/.08/0/.02 | .05 | .05 | .05 | .3 | 5/3/5 | L2 | 保：14 個 channel_reply 呼叫端＝每輪對話都在用，且是 F2 反思的證據來源；淘：無 |
| B3.12 | reflexion.rs（F2a 注入＋F2b 整併） | `channel_reply.rs`（4 處）、`persona_induction.rs` | **live（不受 gvu_enabled 影響）** | 2026-08-13 | `docs/features/20`、`feature-inventory:308` | task_rule_induce（B2.8，寫同一 store） | 1404（18 測試） | .85/.12/0/.03 | .05 | .2 | .05 | .3 | 5/3/4 | L2 | 保：不必開 GVU 就能學，是「免費核心不閹割」的關鍵一塊；淘：與 B2.8 兩條管線寫同一個 rule store |
| B3.13 | evolution_events（黑盒記錄器：schema/emitter/query/logger/reliability） | RPC `evolution.*`；HTTP handlers；`duduclaw doctor` 讀 `audit_index.db` | **live** | 2026-08-15 | `docs/features/29` | — | 5,296（121 測試） | .85/.12/0/.03 | .05 | .1 | .05 | .35 | 4/3/4 | **L2** | 保：**19 個宣告的 `AuditEventType` 變體全部都有生產發射端**（grep 逐一比對一致），沒有死 schema；淘：5.3k 行、其中 `query.rs` 1939 行，可簡化 |

###### 群組 B4 — 知識蒸餾與 wiki 自動建檔

| # | 功能名 | 入口 | 接線 | 最後變更 | 測試 | 文件 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B4.1 | wiki_ingest 對話蒸餾（雙 sink） | `channel_reply.rs`、`claude_runner.rs`、`handlers.rs` | live | 2026-09-01 | 22 | `docs/guides/memory-and-knowledge.md` | 3050 | .9/.08/0/.02 | .05 | .05 | .05 | .4 | 5/4/4 | L2 | 保：三個真呼叫端、9 月還在動；淘：3050 行單檔 |
| B4.2 | auto_wiki_page 自動建檔（四道鎖＋每日配額） | `wiki_ingest`(12)、`handlers.rs`(8)、RPC `wiki.auto_pages`/`.promote`/`.archive` | live | 2026-08-04 | 26 | `CLAUDE.md`「Wiki ↔ memory boundary」 | 1149 | .85/.12/0/.03 | .05 | .05 | .05 | .45 | 4/3/4 | L2 | 保：v1.33 的三個反對意見各有結構性答案（單一 sink／四道鎖／確定性頁鍵）；淘：四道鎖＋兩種每日配額＋灰帶仲裁，旋鈕偏多 |
| B4.3 | knowledge_route 知識分流 | `wiki_ingest`(7)、`auto_wiki_page`(6)、`handlers.rs` | live | 2026-08-04 | 30 | 同上 | 1095 | .85/.12/0/.03 | .05 | .1 | .05 | .4 | 4/3/4 | L2 | 保：修掉「2000 字貼文被八字回覆判成 Skip」的真 bug；淘：65/30 分數門檻是拍腦袋值 |
| B4.4 | KnowledgeCuration 策展站 | RPC `wiki.auto_pages`/`promote`/`archive`/`share` | live（RPC 已註冊） | 未查（前端） | 未查 | `CLAUDE.md` | 未查 | .85/.12/0/.03 | .05 | .05 | .1 | .2 | 4/2/4 | **L0** | 保：讓自動寫入可被人撤銷；淘：**未實地走查 UI** |
| B4.5 | session_summarizer_task（背景摘要） | `server.rs:1952` `spawn_summarizer` | live | 2026-06-17 | 9 | 未查 | 520 | .7/.2/.05/.05 | .2 | .25 | .15 | .2 | 3/2/3 | L1 | 保：開機就 spawn；淘：僅 9 個測試，且與 `session_summarizer.rs`（353 行，2026-05-12）職責邊界未寫明 |

###### 群組 B5 — 跨喚醒狀態

| # | 功能名 | 入口 | 接線 | 最後變更 | 測試 | 文件 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B5.1 | working_state 權威工作狀態（CAS／TTL／32 鍵上限／handoff 契約） | MCP `working_state_set`/`_clear`/`_handoff`/`_get`；`[memory] working_state_enabled` **預設開** | live | 2026-08-27 | 28 | `docs/features/44` | 1486 | .9/.08/0/.02 | .05 | .05 | .05 | .4 | 5/3/5 | L2 | 保：D3「一天三條停損線」事故的直接修法，五個檔案真呼叫；淘：1486 行＋CAS＋TTL＋supersession chain，對「記住幾條規則」偏重 |
| B5.2 | recent_actions 稽核紀錄注入 | `[memory] recent_actions_enabled` **預設開**；`claude_runner.rs`＋`channel_reply.rs` 兩條 prompt 路徑 | live | 2026-08-12 | 9 | `docs/todo/TODO-agent-cross-invocation-continuity.md` | 418 | .9/.08/0/.02 | .05 | .05 | .05 | .1 | 5/1/5 | L2 | 保：418 行修掉「agent 否認自己做過的事」的 D2 事故，投報極高；淘：無 |

###### 群組 B6 — 技能生態（`skill_lifecycle/` 7,626 行 23 檔 ＋ `skill_synthesis_pipeline/` 2,100 行）

| # | 功能名 | 入口 | 接線 | 最後變更 | 文件 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B6.1 | **技能提示建構熱路徑**（compression／relevance／activation／diagnostician／lift） | `channel_reply.rs:630-634`、`:2508`、`:4829`、`:4926-4934`、`:11864-11866`；每則訊息都跑 | **live（最熱）** | 2026-03-28~04-27 | `docs/features/15`（部分不符，見 §2 C-13） | 705 | .9/.08/0/.02 | .05 | .05 | .02 | .25 | 5/2/5 | L2 | 保：每則 channel 訊息都在跑，是系統提示的技能層本體；淘：無——**但容量 5 硬編在 `channel_reply.rs:631`，`max_active_skills` 設定無效** |
| B6.2 | security_scanner（6 類靜態掃描） | `handlers.rs` 5 處（`skills.vet`/`install`/`install_request`）、`mcp.rs` 2 處、`mcp_scan.rs`、`custom_skills.rs`、`expert/mod.rs`、`install_notify.rs`、`hub_install.rs:48`、`sandbox_trial.rs:363` | **live（本目錄最常被呼叫）** | 2026-07-15 | `docs/features/15:228-250`（準確） | 573+164 | .95/.05/0/0 | .02 | .05 | .02 | .15 | 5/2/5 | L2 | 保：技能安裝的唯一安全閘，13+ 個呼叫端；淘：無 |
| B6.3 | curator（陳舊／封存／重啟技能巡檢） | `skill_synthesis_pipeline/scheduler.rs:151`（30 分輪詢＋24h 內部守衛）；MCP `skill_curator_status`/`skill_pin`；`[skill_curator] enabled` **預設 true** | **live（預設開）** | 2026-07-13 | 未查 | 1098 | .8/.15/0/.05 | .05 | .1 | .1 | .35 | 4/3/4 | L1 | 保：預設開且真的在跑，會寫 wiki 報告；淘：`[skill_curator]` 三個鍵**不在 `config/duduclaw.example.toml`**，使用者不知道它存在 |
| B6.4 | hub_install（技能中樞安裝，四道 fail-closed 閘） | MCP `skill_hub_install`（`mcp.rs:11492-11528`）→ `duduclaw-agent/skill_hub.rs` 真 HTTP：`clawhub.ai`／`chat-plugins.lobehub.com`／`www.skills.sh`／`api.github.com`／`raw.githubusercontent.com` | **live（真網路，非 stub）** | 2026-07-27 | 未查 | 346 | .85/.1/0/.05 | .05 | .05 | .15 | .25 | 4/3/4 | L2 | 保：unknown hub DENY／source verdict DENY／hash mismatch DENY／scan DENY 四層都真的存在；淘：`skill_hub.rs:13` 註解說 `skills-sh` 是「stub only, excluded from defaults」，但它**在 `DEFAULT_HUB_IDS`（`:57`）裡**——註解已過期 |
| B6.5 | GitHub Search 技能索引＋`skill_search` | `skill_registry.rs:140` 真 API、4 條固定查詢、24h 快取；MCP `skill_search`（`mcp.rs:10408`/`13861`） | live | 未查 | `docs/features/15:206-226`（準確） | 未查 | .85/.1/0/.05 | .05 | .1 | .1 | .2 | 4/2/4 | L1 | 保：真 API＋快取，文件敘述準確；淘：無 |
| B6.6 | extraction（技能 md → wiki 提案） | MCP `skill_extract`（`mcp.rs:2842`/`13965`）；**無排程呼叫端** | live（僅手動） | 2026-04-17 | `docs/features/15` | 728 | .7/.2/0/.1 | .15 | .1 | .3 | .3 | 3/2/3 | L2 | 保：10 個測試、MCP 工具直達；淘：`extract_and_apply`（`:237`）**全 repo 零呼叫端**，且只有手動入口 |
| B6.7 | synthesizer＋skill_synthesis_pipeline（對話→技能合成） | MCP `skill_synthesis_run`；`server.rs:1082` → `scheduler.rs:118` spawn，但 `auto_run=false`／`dry_run=true`（`scheduler.rs:60-67`） | opt-in(預設關＋預設乾跑) | 2026-07-04 / 2026-09 | `docs/features/15` | 509+2100 | .7/.2/0/.1 | .1 | .15 | .4 | .4 | 3/4/3 | L2 | 保：這是**唯一真的能合成技能的路徑**（吃 EvolutionEvents 軌跡）；淘：雙層預設關（`auto_run=false` 且 `dry_run=true`），且 `[skill_synthesis]` 四個鍵不在 example.toml |
| B6.8 | graduation（技能畢業到全域） | MCP `skill_graduate`；`pipeline.rs:535` | live（MCP/pipeline）／**channel 路徑只 log** | 2026-04-16 | `docs/features/15` | 322 | .7/.2/0/.1 | .15 | .1 | .3 | .3 | 3/2/3 | L2 | 保：MCP 工具可手動畢業；淘：`channel_reply.rs:4997` 的 `check_graduation` 命中後**只 `info!` 不晉升** |
| B6.9 | **gap.rs（技能缺口訊號）** | `channel_reply.rs:4863` 寫 `feedback.jsonl` | live 但**消費端鍵名不符** | 2026-03-28 | — | 43（**0 測試**） | .3/**.5**/0/.2 | .3 | .2 | .3 | .1 | 2/1/2 | **L2** | 保：43 行、有真呼叫端；淘：`gap.rs:18` 寫 `"signal_type"`，消費端 `external_factors.rs:253` 讀 `v["type"]` → **每一列都變成 `unknown`／「?」圖示**，模組註解宣稱的「餵進演化引擎的 candidate_skills」不成立 |
| B6.10 | **gap_accumulator（缺口累積→合成觸發）** | `channel_reply.rs:633/4872/4917` | **inert（觸發後是死路）** | 2026-04-16 | — | 390 | .25/.35/0/.4 | .4 | .2 | **.7** | .35 | 2/2/2 | **L2** | 保：8 個測試、計數邏輯本身正確；淘：`confirm_synthesis()`／`cancel_pending()` **全 repo 零生產呼叫端** → 觸發過的 topic 永遠卡在 `pending`、**永不再觸發**；`:4870` 的 `info!("queuing synthesis")` 沒有任何東西 dequeue |
| B6.11 | **sandbox_trial（沙盒試用＋TTL）** | 評估端 `channel_reply.rs:5042-5076` 已接；但 `SandboxStore::add`（`:238`）**零生產呼叫端** | **inert（評估器接好了，倉庫永遠是空的）** | 2026-07-04 | `docs/features/15`、`CLAUDE.md:47` | 583（11 測試） | .2/.3/0/.5 | .45 | .15 | **.8** | .4 | 2/3/2 | **L2** | 保：TTL 試用是 Voyager 路線的正確設計，評估側已寫好；淘：`active_names()` 恆空 → `channel_reply.rs:5012-5079` 整段是 no-op 迴圈；`from_synthesized`／`graduate_skill_to_disk` 只被自己的測試呼叫 |
| B6.12 | **distillation（技能蒸餾）** | `channel_reply.rs:4972` `scan_for_distillation` | **inert（結果被丟棄）** | 2026-05-12 | `docs/features/15` | 145 | .3/.3/0/.4 | .4 | .15 | **.65** | .2 | 2/1/2 | **L2** | 保：145 行、掃描邏輯有呼叫端；淘：候選迴圈只 `info!`，`channel_reply.rs:4988-4989` 自寫「Distillation via GVU would be triggered here in production … deferred to dedicated distillation task」 |
| B6.13 | **vetting.rs** | 唯一非測試呼叫端 `sandbox_trial.rs:381` 在 `graduate_skill_to_disk` 內——而那是 test-only | **dead** | 2026-04-16 | — | 409（9 測試） | .1/.05/.1/**.75** | .7 | **.6** | **.9** | .3 | 1/2/1 | **L2** | 保：`security_scanner` 之外的第二道審查；淘：唯一路徑經過一個 test-only 函式；`check_secret_patterns`（`:111`）零呼叫端 |
| B6.14 | **curiosity.rs（好奇心驅動探索）** | 無 | **dead** | 2026-04-16 | `config` 有三個鍵但**零讀取端** | 388（6 測試） | .05/.05/0/**.9** | .85 | .3 | **1.0** | .5 | 1/2/1 | **L2** | 保：想不出來；淘：`CuriosityEngine`／`TopicCoverageMap` 零呼叫端；`curiosity_enabled`／`_threshold`／`_max_daily` 三個鍵 dashboard 可寫但 gateway 零讀取；`playbook/entry.rs:75` 只留一句 `/// Reserved: curiosity-driven probe.` |
| B6.15 | **dependency_resolver.rs** | 無 | **dead** | 2026-04-16 | 無 | 299（8 測試） | .05/.05/0/**.9** | .8 | .3 | **1.0** | .35 | 1/1/1 | **L2** | 保：想不出來；淘：`DependencyGraph` 只被自己的 8 個測試引用 |
| B6.16 | **reconstruction.rs** | 無 | **dead** | 2026-07-04 | `docs/features/15:116-137` **把它寫成 7 階段之一** | 276（4 測試） | .05/.05/0/**.9** | .8 | .3 | **1.0** | .3 | 1/2/1 | **L2** | 保：想不出來；淘：`reconstruct_skill`（`:74`）**連自己的測試都沒呼叫**，而功能文件把「Stage 4 Reconstruction」當成已實作在賣 |
| B6.17 | **recommender.rs** | 無 | **dead** | 2026-04-16 | `config` 有兩個鍵但零讀取端 | 202（4 測試） | .05/.05/0/**.9** | .8 | .3 | **1.0** | .3 | 1/1/1 | **L2** | 保：想不出來；淘：`recommend_for_agent`／`filter_for_auto_activation` 零呼叫端（後者連測試都沒有） |

###### 群組 B7 — 夜間／微調／匯入

| # | 功能名 | 入口 | 接線 | 最後變更 | 測試 | 文件 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B7.1 | night engine（夜間整理編排器）＋night_llm | `config.toml [night] llm_enabled` **且** agent `[night_engine] enabled`（雙層預設關）；`server.rs:1160` spawn | opt-in(預設關) | 2026-08-13 | 37 | **無 `docs/features/NN`、example.toml 無 `[night]` 段** | 1625 | .55/.3/0/.15 | .2 | .2 | **.65** | .4 | 3/3/3 | L1 | 保：spawn 永遠安全（預設關），`autopilot_screen` 有引用，且與 memory/night.rs 分工清楚（LLM 半邊 vs 確定性半邊）；淘：雙層 opt-in 全關＋零文件＋example.toml 沒有這個段落＝使用者**不可能知道要開什麼**，實質使用率幾可確定為零 |
| B7.2 | finetune（資料集策展／jobs／匯入 GGUF-LoRA） | RPC **只有 `finetune.import`**；前端 `FineTunePage.tsx` | live（單一 RPC） | 2026-09-23 | 45 | `docs/features/54` | 3983 | .6/.25/0/.15 | .1 | .25 | .45 | **.65** | 3/4/3 | L1 | 保：9 月還在動、有專屬功能頁與文件，「本機不訓練」的產品主張清楚；淘：**3983 行後端只暴露一個 dashboard RPC**（`jobs.rs` 1980＋`dataset.rs` 1474 沒有對應可達面） |
| B7.3 | migrate-from claude-code | CLI `duduclaw migrate-from claude-code --agent <id> [--apply]` | live | 2026-08-16 | 70 | `docs/guides/migrate-from.md`、`feature-inventory:52` | 1597 | .85/.12/0/.03 | .05 | .1 | .15 | .3 | 4/2/4 | L2 | 保：`--apply` 已在 `apply.rs`（2026-08-16）落地——**記憶檔中「欠 --apply」的紀錄已過期**；淘：一次性遷移工具，長期是純維護負擔 |

###### 群組 B8 — 未提交新大族（2026-09）

| # | 功能名 | 入口 | 接線 | 最後變更 | 測試 | 文件 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B8.1 | Decision Lab／支援決策孿生（23 新檔） | CLI 51 個 `decision-*` leaf；HTTP **52 條** `/api/decision/*`（admin-only）；前端 `/decision-lab`（`minRole: admin`, `newIn 1.66.0`） | live，**零 config 鍵＝零 kill switch** | **uncommitted 2026-09** | ~194（7 個新模組各 0-1） | `docs/spec/support-decision-twin.md`（Draft, 96KB）；**無 `docs/features/NN`** | ~34,000 | .4/.25/.05/.3 | .1 | .15 | .35 | **.8** | 2/5/3 | L1 | 保：證據內容定址＋撤銷即清洗＋prospective forecast 必須在結果日前提交，是「誠實回報」最徹底的實作，且刻意**不動作**（不調班不晉升）；淘：spec 自寫「no real customer-support outcome has yet been validated」，一個尚未驗證的**單一垂直**佔本領域 1/4 程式碼、單檔 12,476 行、52 條路由一落地就全上線 |
| B8.2 | CCR 可逆工具結果上下文 | CLI 7 個 `ccr-*`；HTTP `/api/ccr/dashboard`、`/replay`；MCP `duduclaw_ccr_retrieve`/`_find`；`[ccr] enabled` **預設 false** | live（reply 主路徑已接）／設定面預設關 | **uncommitted 2026-09** | ~104 | `docs/spec/reversible-context-ccr.md`（Draft）；TODO 自承「Status: In progress」 | ~3,500+ | .65/.2/0/.15 | .15 | .1 | .4 | .55 | 3/4/3 | L1 | 保：接線深度是真的（五個通道 adapter 各有 principal launderer 的結構性迴歸守衛）、五道獨立 fail-closed；淘：`example.toml:27` 自寫「Disabled until per-task cost/quality evaluation is complete」——**成本效益還沒評完，3.5k 行已落地並接進 reply 主路徑** |
| B8.3 | connector_lifecycle／synthetic_connector_adapter／wiki_mcp_source | 無 CLI；`server.rs:945` 開機 spawn（60s）；`claude_runner.rs:1791` 註冊 wiki 路由 | live | **uncommitted 2026-09** | 19 | 無專屬 spec、無 `docs/features/NN` | 1350+~600 | .7/.2/0/.1 | .1 | .1 | .3 | .4 | 3/3/3 | L1 | 保：「MCP 結果與 caller `_meta` 永遠不是 event」的威脅模型正確，且三者在同一交易內提交；淘：`synthetic_connector_adapter` 唯一外部消費者是 `server.rs:114`，只服務一個合成 fixture |

###### 群組 B9 — Python 層（`python/duduclaw/`，約 11,900 行）

| # | 功能名 | 入口 | 接線 | 最後變更 | 測試 | 文件 | 重疊／可取代者 | LOC | verdict | obs | rep | unu | ove | V/C/R | level | 一句話理由 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B9.1 | LOCOMO 記憶評測（memory_eval） | 只有 `python -m` 手動；「daily 03:00 UTC」**只存在於 docstring** | **dead** | **2026-07-23（且是 ruff lint 修正）** | 230 個 test fn，**CI 完全不收集** | `CLAUDE.md:87`、`feature-inventory:461` 都描述成活的 | `duduclaw eval`（Rust 側真量表） | ~5,700 | .05/.05/.1/**.8** | **.9** | **.7** | **.95** | .6 | 1/3/1 | **L2** | 保：200 筆 golden QA 是唯一存在的記憶品質基準資料；淘：① Rust 側 `Command::new("python` **0 命中**（v1.22.1 明文移除 Python runtime 依賴）② `HttpMemoryClient` 打的 `/memory/search` 路由在 Rust 端**不存在** ③ `pyproject.toml` 未宣告 `asyncpg`/`aiohttp`/`datasets`，裝了也 import 不起來 ④ **200 筆 golden QA 全是 `"source": "manual"`，零筆來自 LOCOMO** |
| B9.2 | Python agents 能力路由 | 無。`registry.yaml` 在 Rust 側零引用 | **dead** | **2026-06-16（ruff 修正；此套件史上只有 2 個 commit）** | 併入 tests/python 406 | `feature-inventory:462` | Rust `delegation_policy.rs`＋`dispatcher.rs`（不同關注點，非嚴格重疊） | ~1,570 | .05/.05/.1/**.8** | **.85** | .5 | **.95** | .5 | 1/2/1 | **L2** | 保：概念上與 Rust 路由不重疊（它選「哪個 agent」，Rust 選「哪個 model tier」）；淘：兩個 commit、其中一個是 lint 修正，`HandoffPacket`／`capability_match` 全 repo 零引用 |
| B9.3 | Python MCP 記憶工具 | 無 MCP server 可掛（`FastMCP`/`stdio_server` 零命中） | **dead** | 2026-06-20 | 131 | `wiki/duduclaw-kb/w19/…` 標 ✅ | **Rust `mcp_memory_handlers.rs` 的 17 個工具是它 3 個的嚴格超集** | ~1,050 | .02/.03/.1/**.85** | **.9** | **.9** | **.95** | .3 | 1/2/1 | **L2** | 保：scope 強制的參考實作；淘：3 ⊂ 17，沒有 server 能 host 它，只有測試在建構那些 class |
| B9.4 | PyPI 發布 `duduclaw` 套件 | `.github/workflows/release.yml:205-244`，每次 release 都推 | live（但推的是 B9.1-9.3 的死碼） | 2026-09（`__version__` → 1.65.1） | — | — | — | — | .3/.3/0/.4 | .5 | .3 | .5 | .3 | 2/2/3 | L1 | 保：已佔名，撤下會讓既有安裝者壞掉；淘：每個 release 都在對外發布一包 import 不起來的程式碼 |

---

##### 2. 候選清單（門檻：verdict argmax ≠ 保留且 ≥0.5，或任一 noul p ≥ 0.6 且 level ≥ L1）

依「證據強度 × 規模 × 可執行性」排序。

###### C-1 `python/duduclaw/` 三個子套件（B9.1–B9.3，約 8,300 行）— **L2**
- **最強保留**：`data/golden_qa_set.jsonl` 的 200 筆是目前唯一存在的記憶品質基準資料集。
- **最強淘汰**：`CHANGELOG.md:4470-4495`（v1.22.1, 2026-06-21）**明文**移除 gateway 的 Python runtime 依賴；
  `Command::new("python` 在全部 `.rs` 中 **0 命中**；ARCHITECTURE.md §5.3 描述的 PyO3 橋 `crates/duduclaw-bridge`
  **不存在**，害 `tools/agent_tools.py:183`、`channels/base.py:21` 的 `from .. import _native` 指向不存在的模組；
  `memory_eval` 打的 HTTP 路由在 Rust 端 0 命中；`pyproject.toml` 缺三個必要依賴。
- **一併處理**：`CLAUDE.md:87-88`、`ARCHITECTURE.md:255-300` §5、`feature-inventory:461-463`（＋ja-JP／zh-TW 鏡像）、
  `docs/architecture/overview.md`、`python/README.md`（27 行全錯）、`SECURITY.md:56`、
  `wiki/impl/w21-locomo-eval-implementation.md`、`wiki/duduclaw-kb/w19/mcp-memory-endpoints-impl-status.md`；
  `.github/workflows/ci.yml:117,119-125`、`pyproject.toml` `testpaths`；`release.yml:205-244` pypi job ＋ `scripts/release.sh:228-235`。
- **中間路線**：保留 `golden_qa_set.jsonl` 與 `build_golden_qa.py` 當資料資產，刪 `mcp/`＋`agents/`＋`memory_eval/` 執行碼，PyPI 縮成 SDK only。

###### C-2 skill_lifecycle 五個死模組（B6.13–B6.17，**1,574 行 ≈ 目錄的 21%**）— **L2**
`vetting.rs`(409) ＋ `curiosity.rs`(388) ＋ `dependency_resolver.rs`(299) ＋ `reconstruction.rs`(276) ＋ `recommender.rs`(202)
- **最強保留**：`vetting` 是 `security_scanner` 之外的第二道審查；`curiosity` 有對應的 config 鍵與 dashboard UI，看起來像「還沒接完」而非「不要了」。
- **最強淘汰**：全部零生產呼叫端（grep 排除自身目錄後 exit 1）；`reconstruction::reconstruct_skill`
  **連自己的測試都沒呼叫**；`vetting` 的唯一路徑經過一個 test-only 函式；`recommender::filter_for_auto_activation` 零測試。
- **一併處理**：`skill_lifecycle/mod.rs` 的五行 `pub mod`；`[evolution]` 的 `curiosity_*`(3)／`skill_recommendation_*`(2) 五個
  **只寫不讀**的 config 鍵與 `handlers.rs:2086-2121` 的 dashboard 表單欄位；
  **`docs/features/15-skill-lifecycle.md:116-137` 的「Stage 4 Reconstruction」整段必須刪**（它在賣不存在的功能）。

###### C-3 `gvu/shadow_mode.rs` ＋ `gvu/diversity.rs`（509 行）— **L2，零爭議**
- **最強保留**：shadow A/B 比對與提案多樣性追蹤，是演化系統想做量化評估時的現成 primitive。
- **最強淘汰**：8 個 pub 項目全 repo 零外部引用、**零測試**，分別 2026-04-04／2026-07-04 未動；
  而 `gvu/mod.rs:18` 的模組文件仍在宣傳 diversity——讀者會以為它在跑。
- **一併處理**：`gvu/mod.rs:18,26,32`。無 config 鍵、無 MCP 工具、無測試、無文件需改。

###### C-4 `prediction/foresight_gate.rs`（304 行）— **L2**
- **最強保留**：D 系列 HITL 介面（approval 文案、三步軌跡預覽）要接時，這個「四條件缺一即完全不注入」的閘已寫好，有 13 個測試。
- **最強淘汰**：檔頭 19-20 行自寫 **"No consumer wired in this change"**；三個主要 pub 項目零外部引用；
  **`CLAUDE.md:28` 把它寫成已生效**（「flags predicted-failure dispatches」）——這是主動誤導。
- **一併處理**：`prediction/mod.rs:36`；`task_forward_store.rs:86-90,150-151` 的 `foresight_tau`／`foresight_recent_k` 兩個欄位。
- **無論留不留，`CLAUDE.md:28` 那句都必須改。**

###### C-5 GVU legacy SOUL.md 演化路徑（B3.4，約 6,900 行）— **L2，最大宗的「已被自己取代」**
- **最強保留**：SOUL.md 版本化＋SHA-256 指紋＋24h 觀察窗＋自動回滾是唯一能「把人格改壞再改回來」的機制；
  `observation_finalizer` 被 `server.rs`（30 分背景）與 `duduclaw evolution finalize` CLI 真呼叫，不是死碼。
- **最強淘汰／取代**：`feature-inventory:185-192` 官方宣告演化目標 2026-08-06 已移到 playbook、SOUL.md 對 agent **唯讀**；
  此路徑要 `legacy_soul_evolution=true` **且** `gvu_enabled=true` 雙層 opt-in，而出廠 wizard 寫死 `gvu_enabled=false`。
  **它守護的那條寫入路徑本身已經被封了。**
- **一併處理**：`docs/features/02`、`06` 兩份主功能文件；`docs/architecture/evolution-engine.md`；`CLAUDE.md:26`；
  `[evolution] legacy_soul_evolution`／`max_gvu_generations`／`observation_period_hours` 三個鍵；
  `duduclaw evolution finalize` CLI；MCP `evolution_toggle` 欄位表。
- ⚠️ **`soul_partition.rs` 被 `playbook_migrate.rs`（`duduclaw playbook migrate-soul`）依賴，不能一起刪。**
- **建議中間路線**：保留 `version_store`＋`observation_finalizer`＋`soul_partition`（版本化／回滾／分割是通用能力），
  淘汰 `generator`/`verifier`/`updater`/`text_gradient`/`proposal`/`loop_` 的 SOUL 改寫閉環。

###### C-6 Decision Lab／支援決策孿生（B8.1，約 34,000 行 untracked）— **L1，規模最大**
- **最強保留**：內容定址證據＋來源撤銷即連帶清洗＋prospective forecast 必須在結果日前提交，
  是平台「誠實回報」原則最徹底的實作；且它刻意**不動作**（不調班、不晉升模型），功能風險低。
- **最強淘汰**：`docs/spec/support-decision-twin.md:203` 自寫 **"no real customer-support outcome has yet been validated"**、
  `:209`、`:217`（"is **not** evidence of real forecast accuracy"）。它是**單一垂直（客服排班）的應用**，
  卻佔本領域 1/4 程式碼、單檔 12,476 行、**零 config 鍵＝零 kill switch**、52 條 REST 路由一落地全上線、
  7 個新模組各只有 0-1 個 in-module 測試、**沒有 `docs/features/NN`**。
- **一併處理**：`gateway/lib.rs:214-235`（22 行 `pub mod`）；`server.rs:2218-2421`（52 routes）＋約 47 處 `store.dashboard_*()`；
  `cli/lib.rs:350-366` 四個 flatten ＋ 51 leaf ＋ `decision_cmd.rs`；
  前端 `App.tsx:88,327`、`nav-model.ts:646`、8 個 `Decision*Panel/Page.tsx`；
  `docs/spec/support-decision-twin.md`／`support-pilot-data-contract.md`／`docs/guides/support-shadow-synthetic-validation.md`＋`docs/README.md:88,90`。
- ⚠️ **`decision_action`／`capture`／`card`／`message_store`／`notify`／`text` 六檔是既有的通知決策卡**
  （屬 `docs/features/40-notification-governance.md`，測試最密：36/23/15/13/12），**只是前綴撞名，絕不可一起刪。**
- **最該問使用者的**：要不要把它拆成獨立垂直包／外掛，而不是留在核心 gateway 裡。

###### C-7 `causal_*` 因果證據圖（B1.27，約 11,000 行 untracked）— **L1**
- **最強保留**：與記憶引擎**刻意不重疊**（22 張專屬表，SPO 留給 HippoRAG）；
  `causal_memory.rs` 用 SQLite trigger 讓上游 quarantine/forget/GDPR 刪除**原子地**撤銷複製文字——
  把「證據可撤銷」做進儲存層而非靠應用層記得清。`causal.rs:997` 還處理了共用 `user_version` 的隱患。
- **最強淘汰**：`docs/spec/causal-evidence-graph.md:3` 自寫 **"does not yet establish a production causal effect"**、
  `:18` "Real extraction accuracy is not yet measured"；`causal_negative_control.rs`（314 行）**零測試**。
- **一併處理**：`duduclaw-memory/lib.rs:3-14`；`gateway/lib.rs:62,204,205`；`cli/lib.rs:12,13`＋6 個 CLI leaf；
  `server.rs:2431-2507`（25 routes）＋`server.rs:937` outbox spawn；`claude_runner.rs:1783`；
  `docs/spec/causal-evidence-graph.md`；web 的 `CausalCurationPage`/`CausalDagView`/`CausalEffectPanel`＋`causal-api.ts`；
  `config/duduclaw.example.toml:41-50`。
- ⚠️ **B8.1／B8.2／B8.3 全都依賴 `EvidenceScope`／`CausalStore`，牽一髮動全身——這四項應一起拍板。**

###### C-8 skill 自動合成鏈的三個斷點（B6.9–B6.12，合計 1,161 行 inert）— **L2，這是 bug 不只是候選**
`CLAUDE.md:47` 宣稱：「Gap accumulator detects repeated domain gaps → synthesizes skills from episodic memory →
sandbox trial with TTL → cross-agent graduation」。**這條鏈有三處斷開，與 config 無關：**
1. `gap_accumulator` 觸發後 `confirm_synthesis()`／`cancel_pending()` 零呼叫端 → topic **永遠卡 pending、永不再觸發**。
2. `sandbox_trial` 的 `SandboxStore::add` 零呼叫端 → `active_names()` 恆空 → `channel_reply.rs:5012-5079` 整段 no-op。
3. `gap.rs:18` 寫 `"signal_type"`、`external_factors.rs:253` 讀 `"type"` → 每列都是 `unknown`。
   另外 `distillation` 的結果只被 `info!` 丟掉（`channel_reply.rs:4988-4989` 自承 deferred）。
- **建議**：這一項**優先當 bug 修或當文件更正**，而非直接淘汰——但如果決定不修，
  `CLAUDE.md:47`、`docs/features/15` 的 7 階段敘述、`skill_synthesis_status` 的工具描述（`mcp.rs:1243-1244`）都必須改。

###### C-9 `[evolution]` 11 個「只寫不讀」的技能旋鈕 — **L2，UX 誠信問題**
見 §0.1⑤。使用者在 dashboard 調 `max_active_skills`／`skill_synthesis_threshold`／`skill_graduation_min_lift`
**完全不會生效**（實際值硬編在 `channel_reply.rs:631/633/4994`）。
- **最強保留**：這些鍵已寫進 `agent.toml`，移除會讓既有檔案出現未知鍵（雖然 lenient 解析不會炸）。
- **最強淘汰**：一個會寫入、會顯示、但永遠不生效的設定，比沒有設定更糟。
- **一併處理**：`handlers.rs:2086-2121` 表單欄位、`types.rs:2187-2229` 型別、`example.toml:90-122` 的註解、
  前端 evolution_advanced 卡片。**二選一：接上讀取端，或整組移除。**

###### C-10 `search.rs`（100 行）＋ `embedding::VectorIndex`（~60 行）— **L2，最乾淨的刪除**
- **最強保留**：`search.rs` 的樸素排序可當 FTS 掛掉時的 fallback（但目前沒有任何 fallback 接線）。
- **最強淘汰**：`rank_results`／`filter_by_tags` 全 repo 零呼叫端，**且是全 crate 唯一沒有 `pub use` 的模組**；
  `VectorIndex` 被 `vector::vector_knn`（SQLite 版）取代，只剩自身 4 個測試與 `lib.rs:38` 的 re-export。
- **一併處理**：`duduclaw-memory/lib.rs:27`（`pub mod search`）、`lib.rs:38`（`pub use embedding::VectorIndex`）。無文件、無 config。
- **注意：保留 `embedding::cosine_similarity`**（5 個呼叫端，含 `playbook/dedup.rs` 與 `prediction/engine.rs`）。

###### C-11 `finetune/`（B7.2，3,983 行）— **L1，比較像「欠接線」**
- **最強保留**：`jobs.rs` 2026-09-23 還在動、有 `docs/features/54` 與 `FineTunePage.tsx`，產品主張清楚。
- **最強簡化理由**：3,983 行後端**只暴露一個** dashboard RPC（`finetune.import`）。
- **建議**：先問使用者——`finetune.jobs.*` 的 RPC 是漏做還是故意不做？答案決定這是 P1 接線還是 P2 瘦身。

###### C-12 night engine（B7.1，1,625＋853 行）— **L1**
- **最強保留**：`server.rs:1160` 無條件 spawn 但預設全關＝永遠安全；`memory/night.rs` 的 N3/N4 是零 LLM 的確定性整理，設計乾淨。
- **最強淘汰**：**雙層 opt-in 全關**（`[night] llm_enabled` ＋ agent `[night_engine] enabled`）、
  **無 `docs/features/NN`**、**`config/duduclaw.example.toml` 完全沒有 `[night]` 段落**——
  使用者不可能知道要開什麼、怎麼開，實質使用率幾可確定為零。
- **建議**：若保留，**至少補一段 example.toml 與一份功能文件**；否則它只是在燒維護預算。

###### C-13 `docs/features/15-skill-lifecycle.md` 的敘述失真 — **L2，文件更正**
297 行的 7 階段模型裡，**Stage 4「Reconstruction」對應的 `reconstruction.rs` 是死碼**；
Stage 2 描述的「merge overlapping skills」在 `compression.rs` 裡不存在（它做的是三層 token 壓縮）；
Stage 6「Diagnostician」描述的「觸發準確率％／衝突偵測」與 `diagnostician.rs`（診斷**預測誤差**）不符。
準確的只有 GitHub 索引（`:206-226`）、安全掃描（`:228-250`）、MCP 工具（`:252-259`）三節。
→ 依 CLAUDE.md「過時文件比沒有文件更糟」，這份**必須改**，不論 C-2 怎麼拍板。

###### C-14 `memory_factory` 收斂未完成 — **L2，這是 bug**
見 §0.1④。`[memory] novelty_gate = true`（預設）在約 18 條繞過 `memory_factory` 的路徑上靜默失效，
**包含 `server.rs:1239` 的排程 decay job**。
- **建議**：這不是去留題，是**補完題**——把那 18 個 `SqliteMemoryEngine::new` 收斂到 `build_memory_engine`。

###### C-15 三處會誤導後續維護者的過期敘述 — **L2，必改**
1. `user_code.rs:16-18`：「READ-ONLY EXPERIMENT … No production path consumes it yet」——
   **假的**，`user_code_profile` 是已註冊、已授權、已對模型宣告的 MCP 工具。照文件清死碼的人會刪掉活工具。
2. `skill_hub.rs:13`：說 `skills-sh` 是「stub only, excluded from defaults」——但它**在 `DEFAULT_HUB_IDS`（`:57`）裡**。
3. `CLAUDE.md:28`／`feature-inventory:64` 的 `[task_forward_model]` default off，與 `impl Default` 的 `true` 相反
   （且 `task_forward_store.rs` 欄位註解與同檔 `impl Default` 自相矛盾）。

###### C-16 `import.rs` 的 DB 路徑分歧 — **L1，待確認是否為 bug**
`wizard.rs:382` 寫入 `home/memory/{agent_name}.db`，而其他所有呼叫端用 `home/memory.db`。
**未證實是刻意還是 bug**——若是 bug，wizard 匯入的記憶會落在沒有任何人開啟的資料庫。
- **建議**：請使用者確認 agent DB 的實際佈局後再拍板。

---

##### 3. 我沒盤到的範圍（誠實列出）

1. **前端未走查**：`MemoryPage.tsx`／`KnowledgeCuration.tsx`／`FineTunePage.tsx`／Foresight 頁／
   `/decision-lab`／`/causal`／`/ccr` 我只確認了 RPC 名稱與路由註冊，**沒有實際開啟 UI 看是否可達、是否有內容**。
   B4.4 的 level 因此只有 L0。
2. **`commercial/docs/` 的設計文件只取檔名未深讀**（依 brief 對大檔的指示），
   所以「設計上原本打算做到哪」這一層的對照沒做，特別是 `DESIGN-evolution-v3-aee.md`、
   `design-task-forward-model-2026-08-06.md`、`DESIGN-market-belief-loop-2026-08.md`、`TODO-prediction-hardening.md`。
3. **`docs/todo/TODO-reversible-context-causal-simulation.md`（112KB）未讀**——
   它可能標註了 B8.1/B8.2/B8.3 的哪些子功能尚未完成，我只看了第 3 行的 status。
4. **測試「有沒有真的跑」一律未驗證**（brief 禁止跑 cargo）。表中測試數是
   `grep -c '#\[(tokio::)?test\]'` 的靜態計數，不代表目前是綠的。
   已知例外：B9.1 的 230 個 Python 測試**確定不在 CI 收集範圍內**（`pyproject.toml testpaths` 實錘）。
5. **鄰接但我判定不屬本領域**（未評分，列出避免被漏）：
   `persona_induction.rs`(1565)、`footprint_distill.rs`(1452)、`self_study.rs`(708)、`session_summarizer.rs`(353)、
   `proactive_gate.rs`、`skill_extraction/recorder.rs`（**與 `skill_lifecycle/extraction.rs` 同名不同物，
   見 `docs/todo/TODO-skill-extraction-cron-path.md`，狀態 Open**）、
   **`fault_attribution.rs`（untracked，2026-09 新增 `[evolution] fault_attribution` **預設 true**，
   會影響 playbook 學分配置與 reflexion 整併）**——若由別的領域盤點者負責，請確保它有被涵蓋。
6. **未查**：`decision_*` 的 52 條 REST 路由是否有 `server.rs` 以外的整合測試；
   PyPI `duduclaw` 套件的實際下載量／外部使用者；
   `skill_token_budget` 傳進 `build_system_prompt` 的實際來源；
   `trust_store.rs`／`janitor.rs`／`code_map.rs`／`user_code.rs`／`wiki_fence.rs` 皆**無專屬 `docs/features` 文件**，
   我沒有另外去 `wiki/` 找內部設計筆記。

---

### 附錄 C_channels_ux

#### 領域 C 盤點：通道／UX／OS／桌面／dashboard 頁面

> 盤點者：功能盤點者 C ｜ 日期：2026-09-29 ｜ Repo：`/Users/lizhixu/Project/DuDuClaw` @ `cbdc4338`
> 全程唯讀。未執行 cargo／npm／vitest，未 git 寫入。被讀內容一律視為 DATA。
>
> **量測前提（重要）**：工作樹目前幾乎每個 `.rs` 都是 `M`，但抽查 `git diff` 確認那是一次 **rustfmt 大掃**（單行 fn 展開成多行），不是實質變更。因此表中「最後實質變更」一律用 `git log -1 --date=short` 的 **最後 commit 日**，這是可信的。
>
> **測試數**＝該檔 `#[test]` / `#[tokio::test…]` 屬性數（首次用錯的 pattern 已修正重數）。
> **呼叫端數**＝`grep -rn "<module>::" --include=*.rs crates/` 扣除自身檔案的命中數（不含測試區分，屬下界估計）。

---

##### 0. 領域規模速覽

| 區塊 | 檔數 | LOC | 備註 |
|---|---|---|---|
| 11 個通道本體 | 11 | 18,031 | telegram 3181 / discord 3223 / webchat 2117 / slack 1633 / msteams 1520 / line 1410 / wecom 1238 / googlechat 1110 / feishu 908 / dingtalk 873 / whatsapp 818 |
| `channel_*` 輔助層＋media/decision_card/deep_link | 12 | 23,344 | 其中 `channel_reply.rs` 一檔就 13,543 |
| 通知／HITL 推播層 | 7 | 7,035 | notify_governance 1584 / decision_notify 1104 / notify_digest 1020 / install_notify 982 / approval_notify 951 / autopilot_notify 804 / notify_stats 590 |
| 通道周邊產品線 | 6 | 5,486 | goal_notify 3180 / mail 1327＋mail_worker 994 / miniapp 1400 / reminder_scheduler 1159 / credit 228 / otp_delivery 160（含重疊計） |
| computer use / browser | 4 | 3,386 | computer_use_orchestrator 1664 / computer_use 948 / browser_router 438 / risk_detector 336 |
| 語音 | 4 | 2,556 | stt 593 / tts 707 / discord_voice 563 / （audio_bridge 693 歸 OS 線） |

---

##### 1. 通道本體（11 條）

全部 11 條都在 `server.rs:1103-1131` 的開機路徑被真正啟動（telegram/slack/discord 為 per-agent bot 清單；line/whatsapp/feishu/googlechat/msteams/wecom/dingtalk 為全域 webhook router；webchat 為 WS）。CHANGELOG `[Unreleased]` 段落裡「十一通道 adapter」同批被 CCR principal 修正——**沒有任何一條是殭屍**。

| 功能 | 入口 | 接線 | 最後變更 | 測試 | 文件 | LOC | verdict | obsolete / replaceable / unused / overeng | value/cost/risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Telegram 通道 | `server.rs` `start_telegram_bots`；per-agent token | live | 2026-08-20 | 19 | CLAUDE.md 通道段、`43-telegram-miniapp` | 3181 | 保留 0.97／簡化 0.03 | .02/.02/.02/.15 | 5/3/5 | L2 | 能力矩陣全滿（檔案/圖片/按鈕/編輯/typing/markdown/引用），台灣使用者主通道；最強淘汰理由只有「單檔 3.2k LOC 偏大」。|
| Discord 通道 | `start_discord_bots`；Gateway WS＋op6 RESUME | live | 2026-09-01 | 14 | CLAUDE.md（v1.9.2 硬化段） | 3223 | 保留 0.97 | .02/.02/.02/.2 | 4/3/4 | L2 | per-agent bot 的參考實作＋唯一有 RESUME/看門狗硬化的長連線；淘汰理由只有「非台灣主場」。|
| Slack 通道 | `start_slack_bots`；Socket Mode | live | 2026-08-20 | 21 | CLAUDE.md | 1633 | 保留 0.95 | .02/.03/.05/.1 | 4/2/4 | L2 | 企業客戶必備、能力矩陣全滿；淘汰理由：台灣中小企滲透率低於 LINE。|
| LINE 通道 | `start_line_bot` webhook；`relay_client` 亦導回同一驗簽路徑 | live | 2026-08-20 | 10 | `line-oa-b2c.md`／`line-touch-nfc.md` | 1410 | 保留 0.98 | .02/.02/.02/.05 | 5/2/5 | L2 | 台灣 B2C 命脈、NFC/QR 實體觸點的唯一落點；淘汰理由：平台能力最弱（無編輯、純文字、無引用）。|
| WebChat 通道 | `/webchat` 頁＋WS；`compose_session_id` | live | 2026-08-20 | 46 | 無專屬 feature doc | 2117 | 保留 0.93／簡化 0.07 | .03/.1/.05/.1 | 4/2/4 | L1 | 零設定即可試用的第一方通道（onboarding 關鍵），測試最密（46）；淘汰理由：功能與 dashboard chat 有部分重疊。|
| Microsoft Teams | `start_teams_webhook`＋`teams_conversations.json` | live | 2026-08-16 | 21 | `channels-googlechat-teams.md` | 1520 | 保留 0.85／簡化 0.15 | .05/.1/.2/.15 | 3/3/3 | L1 | 企業版賣點、有 proactive conversation-reference 持久化與 JWT 驗證；淘汰理由：台灣中小企幾乎不用、無檔案/圖片/按鈕能力。|
| Google Chat | `start_googlechat_webhook`＋service-account | live | 2026-08-16 | 11 | `channels-googlechat-teams.md` | 1110 | 保留 0.8／簡化 0.2 | .05/.15/.25/.1 | 3/3/3 | L1 | 有 Workspace 客戶時是唯一入口、edit-in-place 已實作；淘汰理由：`file_upload`／`photo_upload`／按鈕全 false，等於只是個文字管子。|
| WeCom 企業微信 | `start_wecom_webhook`（HMAC-SHA1＋AES-256-CBC） | live | 2026-08-16 | 14 | CLAUDE.md（2026-08 audit 補正「九→十一」） | 1238 | 保留 0.6／簡化 0.25／淘汰 0.15 | .1/.15/**.45**/.15 | 2/3/3 | L1 | 中國市場唯一入口、加解密已寫完；淘汰理由：**台灣定位的產品裡沒有已知使用者**，且無按鈕／無編輯／無引用。|
| DingTalk 釘釘 | `start_dingtalk_webhook`（HMAC-SHA256＋時窗） | live | 2026-08-16 | 10 | CLAUDE.md | 873 | 保留 0.55／簡化 0.25／淘汰 0.2 | .1/.15/**.5**/.15 | 2/3/3 | L1 | 同上；能力矩陣最低（file/photo/按鈕/編輯/typing/引用全 false，只剩文字＋markdown）。|
| WhatsApp | `start_whatsapp_webhook`（簽章 fail-closed） | live | 2026-08-16 | 7 | CLAUDE.md | 818 | 保留 0.85 | .05/.1/.2/.05 | 3/2/3 | L1 | 東南亞／跨境客戶入口、檔案圖片都支援；淘汰理由：引用只有標註（Cloud API 不給原文）、測試最少（7）。|
| Feishu 飛書 | `start_feishu_webhook` | live | 2026-08-16 | 7 | CLAUDE.md | 908 | 保留 0.7／簡化 0.2／淘汰 0.1 | .08/.15/.35/.1 | 2/2/3 | L1 | Card 2.0 markdown 渲染已做；淘汰理由：無按鈕、無 typing、無引用，台灣使用者極少。|

**三個中國系通道（wecom／dingtalk／feishu）合計 3,019 LOC**。它們共用 `channel_sender` / `markdown_render` / `channel_capabilities` 抽象，單獨移除省不了多少共用層，但會省掉三份 webhook 驗簽（各自一套密碼學）與三份 CHANGELOG/CCR 同步負擔——最近那批 CCR principal 修正就明確點名了這三條。

---

##### 2. `channel_*` 輔助層

| 功能 | 入口 | 接線 | 最後變更 | 測試 | 呼叫端 | LOC | verdict | noul | value/cost/risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `channel_reply.rs` 回覆主管線 | 全通道共用進入點 | live | 2026-09-23 | 170 | — | **13,543** | 保留 0.55／**簡化 0.45** | .0/.0/.0/**.7** | 5/**5**/**5** | L1 | 平台最核心的一條路徑（CLI spawn／PTY／Direct API／壓縮／CCR 租約／computer-use 全在裡面）；但一檔 13.5k LOC 對照 CLAUDE.md「200-400 typical, 800 max」是全 repo 最大的結構債，拆檔風險也最高。|
| `channel_sender.rs` 統一送出 | `create_sender(channel, target)` | live | 2026-08-16 | 28 | 高 | 2289 | 保留 0.97 | .0/.0/.0/.05 | 5/2/5 | L2 | 第四波才把第三份手刻 sender 收斂進來（順手修好 slack 通知自始沒送出的活 bug）；無淘汰理由。|
| `channel_format.rs` 富訊息／引用區塊 | `decision_markup`／`format_quoted_context` | live | 2026-08-20 | 40 | 63 | 1834 | 保留 0.97 | .0/.0/.0/.05 | 5/2/5 | L2 | 63 個呼叫端、按鈕與引用格式的單一權威；無淘汰理由。|
| `markdown_render.rs` 逐平台渲染 | `to_telegram_html`／`to_slack_blocks`… | live | 2026-08-04 | 16 | 14 | 967 | 保留 0.95 | .0/.05/.0/.1 | 4/2/4 | L2 | CJK 寬度對齊的等寬表格是真差異化；淘汰理由：可考慮改用現成 markdown→platform 套件，但沒有一個套件涵蓋十一平台。|
| `channel_capabilities.rs` 能力矩陣 | `channels.capabilities` RPC ＋`log_unsupported` | live | 2026-08-16 | 12 | 多 | 589 | 保留 0.9／簡化 0.1 | .0/.0/.1/.15 | 4/1/2 | L2 | 把 10 處硬編節流字面值＋4 套散落判斷收斂成一張表，且明寫「值是行為推導不是願望」；淘汰理由：它本身不改變行為（只影響 log 與 RPC），是純文件性型別。|
| `channel_typing.rs` 打字指示 | RAII guard | live | 2026-08-16 | 3 | 18 | 250 | 保留 0.95 | .0/.0/.05/.05 | 3/1/2 | L2 | 250 LOC 拿到六平台的「正在輸入」；無淘汰理由。|
| `channel_link.rs` 反向 deep link | 任務/審批卡「在通道中開啟」 | live | 2026-08-14 | 31 | 多 | 775 | 保留 0.9 | .0/.05/.1/.15 | 3/2/3 | L1 | 純函式 URL builder＋Discord guild_id／Slack workspace domain 的持久化；淘汰理由：是 UX 潤飾，拿掉不影響功能。|
| `deep_link.rs` 正向 deep link | 通道推播裡的 dashboard 連結 | live | 2026-08-11 | 14 | 22 | 276 | 保留 0.92 | .0/.05/.05/.1 | 3/1/2 | L2 | 同上對偶；無淘汰理由。|
| `channel_settings.rs` 分層設定 | SQLite＋快取；mention-only／白名單／auto-thread | live | 2026-08-11 | 26 | **41** | 740 | 保留 0.97 | .0/.0/.0/.05 | 4/2/4 | L2 | 41 個呼叫端；無淘汰理由。|
| `channel_alerts.rs` 通道斷線告警 | 讀 `channel_failures.jsonl`，推到「還活著的另一條通道」 | live | 2026-08-11 | 24 | 10 | 994 | 保留 0.85 | .0/.1/.1/.2 | 4/2/3 | L1 | 補上「沒人主動讀 failures.jsonl」的洞，且刻意不推回壞掉的通道；淘汰理由：與 `notify_governance` 的告警路徑有部分職責重疊。|
| `decision_card.rs` 決策卡收合 | 按下按鈕後就地改寫並移除按鈕 | live | 2026-08-11 | 13 | **56** | 509 | 保留 0.95 | .0/.0/.0/.1 | 4/1/3 | L2 | 56 個呼叫端；防「已決議卻仍可點」的陳舊卡片。|
| `media.rs` 附件管線 | 圖片縮放／MIME／base64→Vision | live | 2026-09-23 | 14 | **81** | 578 | 保留 0.98 | .0/.0/.0/.0 | 5/1/5 | L2 | 81 個呼叫端，最近才動過；無淘汰理由。|
| 進度看板（`ProgressEvent::TodoUpdate`） | 解析 Claude `TodoWrite` → 📋 看板，edit-in-place | live | — | — | 26 處 | 分散 | 保留 0.9 | .0/.05/.05/.2 | 4/3/3 | L1 | 長任務可見性的唯一來源；淘汰理由：綁死 Claude CLI 的 `TodoWrite` 事件形狀，對 codex/gemini runtime 無效。|
| **`webhook.rs`（通用 `POST /webhook/{agent_id}` → bus_queue）** | 無 | **dead** | **2026-04-06** | 0 | **0** | 280 | **淘汰 0.7**／簡化 0.15／保留 0.15 | .55/.35/**0.98**/.2 | 1/1/**1** | **L2** | 帶 HMAC-SHA256 驗簽、可讓外部系統直接把任務丟給某個 agent，概念上是 MCP/ACP 之外的第三條外部入口；**`webhook_router` 與 `WebhookState` 全 repo 零引用，`server.rs` 從未 mount 過任何 `/webhook/{agent}` 路由**，六個月未動——是 http-server／ACP／Remote MCP 上線後留下的孤兒。|

---

##### 3. 通道周邊產品線

| 功能 | 入口 | 接線 | 最後變更 | 測試 | LOC | verdict | noul（obs/repl/unused/over） | v/c/r | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `goal_notify.rs` 目標迴圈通道推播＋按鈕裁決 | needs_human／kickoff 卡片；`duduclaw:goal_*` 按鈕 | live | 2026-08-15 | 62 | 3180 | 保留 0.9／簡化 0.1 | .0/.0/.0/.35 | 5/4/5 | L1 | HITL 的主要人機介面，授權走統一 `authorize_press`；淘汰理由：3.2k LOC 有不少是逐通道降級分支，與 `decision_notify`／`approval_notify`／`install_notify` 形狀高度重複（四份「free functions 從 home_dir 開 store」）。|
| `reminder_scheduler.rs` 提醒 | MCP `create_reminder`／`list_reminders`／`cancel_reminder`；`server.rs:1941` 啟動 | live | 2026-08-15 | 17 | 1159 | 保留 0.85 | .0/.15/.15/.2 | 3/2/3 | L1 | 時間輪＋disk-as-SoT＋U1 自然時機豁免（「9 點提醒就要 9 點響」）寫得很乾淨；淘汰理由：與 `CronScheduler` 功能重疊（cron 也能排一次性任務），且**沒有任何 dashboard 頁面**，只有 agent 能建。|
| Agent Mail（`mail.rs`＋`mail_worker.rs`） | MCP 三工具＋`/mail` 頁；`[mail] enabled` 預設 **false** | opt-in 預設關 | 2026-08-16／08-15 | 22＋13 | 2321 | 保留 0.7／簡化 0.2／淘汰 0.1 | .05/.2/**.5**/.3 | 3/3/3 | L1 | 「外發必經 ApprovalBroker」的設計很正確且零改動就接上三個既有決策入口；淘汰理由：預設關、無原生 IMAP、無附件、未整合 channel registry／autopilot（CLAUDE.md 自己列的四項欠帳），目前只支援 Gmail API＋drop folder。|
| Telegram Mini App（`miniapp.rs`） | `/miniapp/approval`＋2 個 POST；`[miniapp] enabled` 預設 **false** | opt-in 預設關 | 2026-08-12 | 22 | 1400 | 保留 0.45／簡化 0.15／**淘汰 0.4** | .1/.3/**.6**/.4 | 2/3/2 | **L1** | 設計嚴謹（initData 驗簽、共用 `route_press`、不另建授權），且 `docs/features/43`（三語）＋`deployment-guide.md` 都有教怎麼開；淘汰理由：預設關、**`config/duduclaw.example.toml` 沒有 `[miniapp]` 段、web dashboard 零 UI 開關**（`grep miniapp web/src` 零命中），deployment-guide 自己標成 “experimental”，它自稱是「D-S1 spike，刻意只做一個畫面」，且只服務十一通道中的一條。|
| LINE OA B2C 多帳號＋點數計費（`credit.rs`＋`LineAccount`） | `duduclaw ops credit grant/balance/history`；`[[channels.line.accounts]]` | **dead（半成品）** | 2026-07-11 | 2 | 228＋types 段 | 保留 0.15／簡化 0.1／**淘汰 0.5／取代 0.25** | .3/.2/**0.95**/.2 | 2/2/2 | **L2** | 帳本、費率換算、operator CLI 都寫好了；**`CreditLedger` 全 repo 只有 1 個呼叫端（就是那支 CLI），`LineAccount::resolve_accounts()` 呼叫端為 0，`line.rs` 裡 `destination` 字串零命中**——多 OA 路由與扣點從未接上，doc 自己的 Status 段也承認「是剩下的整合步驟」。|
| OTP 通道投遞（`otp_delivery.rs`） | 登入 OTP → 1:1 DM | live | 2026-08-14 | 4 | 160 | 保留 0.95 | .0/.0/.0/.0 | 4/1/4 | L2 | 160 LOC 的依賴反轉把 channel token 從 auth handler 抽走，fail-closed；無淘汰理由。|
| 通道配對／存取控制（`access_control.rs`） | 通道使用者 ↔ dashboard user 映射 | live | 2026-07-07 | 15 | 589 | 保留 0.95 | .0/.0/.0/.05 | 5/2/5 | L2 | 所有按鈕裁決授權的底座；無淘汰理由。|
| 人工接管（`takeover.rs`） | `/takeover`／自動偵測；`[takeover] enabled` 預設 false | live（自動路徑 opt-in） | — | 21 | — | 保留 0.9 | .0/.05/.1/.1 | 4/2/3 | L1 | 「主管開口＝接管」，預設關是踩過坑後的正確修正（個人版自聊會被誤靜音）；淘汰理由：只有團隊版情境用得到。|
| LINE 加好友 QR／海報 | `channels.line_add_friend` RPC → ChannelsPage 客戶端渲染 QR | live | — | — | — | 保留 0.9 | .0/.1/.05/.05 | 4/1/3 | L2 | 實體觸點（桌卡／NFC）的唯一技術落點，NFC 指南整篇依賴它；無淘汰理由。|
| LINE Touch／NFC 指南 | `docs/guides/line-touch-nfc.md`（純文件，零程式碼） | 文件 | 2026-08-16 | — | 49 行 | 保留 0.8／簡化 0.2 | .25/.05/.1/.0 | 3/1/1 | L1 | 給實施夥伴的落地 SOP、成本表具體；淘汰理由：內含「2026-09 底開賣」的時效性預測，現在已到期需重驗（doc rot 風險）。|
| LINE LIFF | — | **未實作** | — | — | 0 | （非既有功能） | — | — | L2 | `docs/features/43` 明寫「LINE LIFF／Teams Dialog／Feishu Web App 有對等能力但這次沒做」；全 repo `LIFF` 零程式碼命中。列此行是為了說明「範圍裡的這一項不存在」。|
| LINE Rich Menu | — | **未實作** | — | — | 0 | （非既有功能） | — | — | L2 | `grep -rn "richmenu\|rich_menu\|Rich Menu" --include=*.rs` 零命中，與記憶檔「LINE Rich Menu deferred」一致。|
| Shortcuts／穿戴（`shortcuts-and-wearables.md`） | 沿用 `duduclaw http-server` 的 `/mcp/v1/call` ＋ `POST /ingest/transcript` | live（透過既有端點） | — | — | 指南 | 保留 0.9 | .1/.05/.15/.0 | 3/1/2 | L2 | **零新增程式碼**，只是把既有 HTTP MCP 端點包成 iPhone/Watch Shortcut 與穿戴 webhook；`/ingest/transcript` 真實存在（`mcp_http_server.rs:255`）；淘汰理由：純文件，無法量測有沒有人照做。|

---

##### 4. Computer Use 與「瀏覽器五層」

| 功能 | 入口 | 接線 | 最後變更 | 測試 | 呼叫端 | LOC | verdict | noul | v/c/r | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **`browser_router.rs`（L1–L5 自動分層路由）** | 無 | **dead** | **2026-04-02** | 12 | **0** | 438 | **淘汰 0.65**／簡化 0.2／保留 0.15 | .6/.3/**0.98**/.4 | 2/2/**1** | **L2** | 5 層升級模型是 `docs/features/08` 的招牌敘事；**但 `grep -rn "browser_router"` 扣掉自身只剩 2 行註解引用，`BrowserTier`／`BrowserRestrictions`／`select_tier` 在 repo 其他地方零使用**，`pub mod browser_router;` 是唯一活口。與記憶檔「browser_router 仍是空殼」一致，且 5 個月沒人碰。|
| `computer_use.rs`（L5 動作型別／安全） | MCP＋`channel_reply` | live | 2026-06-20 | 20 | 多 | 948 | 保留 0.85 | .05/.1/.1/.15 | 3/3/4 | L2 | `[capabilities] computer_use` 預設 false（deny-by-default），container/native 雙模式；淘汰理由：是最貴也最少用的一層。|
| `computer_use_orchestrator.rs` | `list_sessions`／`get_session_control`；MCP `mcp.rs:17810` | live | 2026-07-06 | 19 | 多 | 1664 | 保留 0.85 | .05/.1/.1/.2 | 3/3/4 | L2 | 真有 session 註冊表與跨通道 control；同上。|
| `risk_detector.rs` | computer use 動作風險判定 | live | 2026-04-18 | 12 | 3 | 336 | 保留 0.9 | .05/.05/.05/.05 | 4/1/4 | L2 | computer use 的安全閘；無淘汰理由。|
| `screenshot_audit.rs` | 截圖稽核 | live | 2026-04-02 | 5 | 4 | 382 | 保留 0.85 | .1/.05/.1/.05 | 3/1/3 | L2 | 同上。|
| L3「Playwright MCP」 | 使用者自行在 `.mcp.json` 註冊外部 MCP server | 非本專案功能 | — | — | — | （敘事問題） | — | — | L2 | repo 裡沒有任何 DuDuClaw 自有的 L3 實作；`playwright` 命中都是「範例設定字串」或 `mcp_recording.rs`（錄製轉技能，另一條線）。|
| L4「Sandbox Browser」 | 無 | **不存在** | — | — | 0 | 0 | — | — | — | **L2** | `SandboxBrowser` 只在 dead 的 `browser_router.rs` 裡當 enum 變體出現；`crates/duduclaw-sandbox` 是**行程層 Seatbelt/Landlock 侷限**，不是瀏覽器容器。|
| `docs/features/08-browser-automation.md` | 公開文件 | **doc rot** | 2026-04-15 | — | — | **淘汰 0.5／簡化 0.4** | **.75**/.1/.3/.1 | 1/2/1 | **L2** | 整篇在講一個「不存在的路由器路由不存在的 L4」；比沒有文件更糟（會主動誤導）。|

> **結論**：目前真實存在的是 **L1（`web_fetch`）／L2（`web_extract`）／L5（computer use）三層 MCP 工具**，由模型自己選用；「五層自動路由」這個敘事在程式碼裡不存在。CLAUDE.md 的「Browser automation & computer use（5-layer auto-routing）」那條 bullet 同樣需要誠實化。

---

##### 5. 語音

| 功能 | 入口 | 接線 | 最後變更 | 測試 | 呼叫端 | LOC | verdict | noul | v/c/r | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `stt.rs`（OpenAI-compat／本地 command） | `POST /api/voice/*`；`config.toml [voice]` | live | 2026-07-11 | 11 | 3 | 593 | 保留 0.9 | .05/.1/.1/.05 | 3/2/3 | L2 | 兩個 provider，fail-closed；淘汰理由：`[voice]` 段在 `config/duduclaw.example.toml` 裡完全沒出現，發現度低。|
| `tts.rs`（MiniMax／Edge） | Telegram 語音回覆＋MCP＋`/api/voice` | live | 2026-04-20 | 3 | 5 | 707 | 保留 0.88 | .05/.15/.1/.1 | 3/2/3 | L2 | Telegram 真的會回語音（`telegram.rs:1257`）；淘汰理由：只有 3 個測試，且 5 個月沒動。|
| **`discord_voice.rs`（Songbird 語音房）** | 無 | **dead**（且 `discord-voice` feature 不在 default） | **2026-07-04** | 6 | **0** | 563 | **淘汰 0.6**／簡化 0.2／保留 0.2 | .4/.25/**0.95**/.3 | 2/2/**1** | **L2** | 完整的 Discord Voice 收音→ASR→回 TTS 管線；**`grep -rn "discord_voice"` 扣掉自身只剩 `lib.rs:160` 的 `pub mod`**，零呼叫端，且 `songbird` 是 optional dep、`discord-voice` 不在 `default = ["dashboard","desktop"]` 裡——預設建置根本不編譯它。|
| `docs/features/14-voice-pipeline.md` | 公開文件 | **doc rot** | 2026-04-15 | — | — | **簡化 0.55／淘汰 0.35** | **.8**/.05/.2/.05 | 1/2/1 | **L2** | 文件列出「SenseVoice(ONNX)／Whisper.cpp／LiveKit 語音房／VAD」四大件——`grep -rn "livekit\|SenseVoice"` 在整個 `crates/` **零命中**；實際只有 OpenAI-compat STT＋本地 command STT＋MiniMax/Edge TTS。|

---

##### 6. 通知／HITL 推播層（通道面）

| 功能 | 接線 | 最後變更 | 測試 | 呼叫端 | LOC | verdict | noul | v/c/r | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `notify_governance.rs` L1/L2/L3＋勿擾 | live | 2026-08-11 | 50 | **56** | 1584 | 保留 0.97 | .0/.0/.0/.15 | 5/3/5 | L2 | 所有主動推播的必經閘，`docs/features/40`；無淘汰理由。|
| `decision_notify.rs` 統一按鈕裁決 | live | 2026-08-13 | 23 | **71** | 1104 | 保留 0.97 | .0/.0/.0/.1 | 5/3/5 | L2 | 五個決策來源收斂成一套 action-id＋一套授權；無淘汰理由。|
| `notify_digest.rs` 每日摘要 | opt-in | 2026-08-12 | 26 | 9 | 1020 | 保留 0.8 | .0/.1/.2/.2 | 3/2/3 | L1 | 空日靜默是對的；淘汰理由：opt-in 且與 `notify_stats` 職責相鄰。|
| `notify_stats.rs` 行動率量測 | live | 2026-08-11 | 15 | 11 | 590 | 保留 0.8 | .0/.1/.2/.15 | 3/1/2 | L1 | 「<50% 精確度就標為壞掉」是難得的誠實量測；淘汰理由：需要有人去看才有價值。|
| `install_notify.rs` / `approval_notify.rs` / `autopilot_notify.rs` | live | 08-14／08-12／08-12 | 13／22／21 | 7／13／**2** | 982／951／804 | 保留 0.75／**簡化 0.25** | .0/**.35**/.1/.35 | 3/3/3 | L1 | 三者＋`goal_notify` 是**四份同形的「從 home_dir 開 store 的 free functions＋逐通道降級」**（各自的 module doc 互相引用承認這點）；`autopilot_notify` 只有 2 個呼叫端。合併成一個 `notify::push(card, dest)` 是最有價值的簡化。|

---

###### 6.1 殘留的手刻 sender（與 CLAUDE.md 的「收斂完成」敘述不符）

CLAUDE.md 第四波宣稱「autopilot 第三份手刻 sender 移除……通知同族終章」。實際 grep 平台 API 字面值：

| 檔案 | 直接打平台 API 的次數 | 是否正當 |
|---|---|---|
| `channel_sender.rs` | 7 | ✅ 正本 |
| `goal_notify.rs` | 4（TG sendMessage／Slack postMessage／Discord DM 開房＋送出） | ⚠️ 有理由（需要 inline keyboard，`channel_sender` 不支援按鈕），但可抽成 `channel_sender` 的 `send_with_markup` |
| **`dispatcher.rs`** | **3**（`:3007` TG／`:3128` Discord／`:3166` Slack） | ❌ **第四份手刻 sender**——委派回報轉發。同一函式的 googlechat／msteams／wecom／dingtalk 分支（`:3310`–`:3344`）卻是呼叫共用模組，所以它是**半收斂**狀態；手刻的理由是 `resolve_forward_token` 的 `reports_to` token 級聯，但那可以當參數傳給共用 sender |
| `decision_card.rs` (2)／`channel_typing.rs` (1) | — | ✅ 正當（editMessage／sendChatAction 不是 send） |

###### 6.2 結構規模警訊（跨領域，供協調者轉交）

`crates/duduclaw-gateway/src/*.rs` 合計 **384,395 LOC**（不含子目錄）。前三大：

- `handlers.rs` **51,802 LOC**（dashboard RPC 總機，約 445 個 `"<ns>.<method>"` 字串；前幾大命名空間：config 117／device 73／channels 62／agents 49／experts 47）
- `server.rs` 16,294 LOC
- `channel_reply.rs` 13,543 LOC

三者都遠超 CLAUDE.md 自訂的「800 行上限」。這不是功能去留問題，是結構債，但它直接影響「哪些 dashboard 頁面有後端」這件事的可稽核性。

###### 6.3 其他 `refs=0` 模組（不在我領域，原樣轉交）

我對 `gateway/lib.rs` 全部 `pub mod` 做了一次「模組名零引用」掃描（下界估計，可能漏計 trait 實作與別名）。我領域內的三個已列在上方表中；以下 refs=0 者屬其他盤點者領域，僅轉交線索、不做判定：
`cost_anomaly`、`decision_forecast_dashboard`、`decision_model_candidate_dashboard`、`decision_outcome_dashboard`、`decision_shadow_dashboard`、`delegation_scope`、`skill_approval`、`workforce_private`。

---

##### 7. Web Dashboard（92 頁／103 路由）

> 本節由子調查完成，關鍵三項（`_WipPlaceholder` 零呼叫端、`ApprovalsPage` 孤兒、`/approvals` 已重導）我本人複查過 grep，可視為 L2。

###### 7.1 整體規模

| 指標 | 數值 |
|---|---|
| `web/src` 的 `.ts`/`.tsx` 檔數 | 734 |
| 總 LOC | 151,857（pages 49,131 ／ components 51,062 ／ lib 15,340 ／ test 30,812） |
| test 檔數 | 256 |
| `components/` 元件檔（不含 test） | 276，分散 25 個子目錄 |
| i18n | **3 語**（zh-TW 預設／en／ja-JP），各 **6,589 keys / 6,591 行**；`jq keys` 三方 diff **完全一致、零缺漏** |
| `lib/api.ts` | 7,236 行的單一巨型 API facade |

i18n 對齊度是全案最健康的部分之一：`index.ts:11` 的 `{...en, ...jaJP}` fallback 目前是永不觸發的保險。

###### 7.2 殼／導覽的四套（不是三套）

| 元件 | 真實角色 | 是否重疊 |
|---|---|---|
| `ManageShell` | `/manage` 巢狀路由殼（左 rail＋`<Outlet>`），manager+ fail-closed，子路由各自再 gate | 唯一真正的導覽殼 |
| `LauncherPage` | `/launcher` 跨 app 格狀啟動器，**刻意不進 nav**（自述「shell 桌面的前身」，對應 N-1/N-2） | 與 SystemHomePage **不重疊**（跨 app vs. 單 app 內） |
| `SystemHomePage` | `/app/system` 這一個 app 內部的分區卡片首頁 | 同上 |
| `BillingShell`／`GovernanceShell`／`LicenseShell` | **不是導覽殼**，是「兩頁合併成 Tabs」的容器 | 命名誤導；`BillingShell` 更已退化成 19 行的「BillingPage＋一個 `?tab=accounts` 重導 shim」，檔頭自承 |
| `HomePage` | `/` 與 `/workspace` 共用同一元件（舊 workspace 模式已併入 Home） | 已收斂，非殘留 |

###### 7.3 路由重複盤點（103 條路由的真相）

| 類型 | 組數 | 說明 |
|---|---|---|
| **A 類：純重導**（`Navigate`／`LegacyRouteRedirect`） | 約 17 條 | device／license×2／accounts×2／security×2／settings×2／updates／channels／mcp／mcp-keys／odoo／partner／wiki-trust／approvals／knowledge／wizard／legacy-dashboard。已收斂，健康。 |
| **B 類：同一元件被兩條路由各自掛載** | **8 組** | billing／logs／reliability／inference／local-models／finetune／users／**governance**。App.tsx:469-472 註解承認是刻意的（D10-B：舊 alias 曾是「企業版頁面有一條沒把關的第二路徑」的洞，補 guard 比刪路由安全）；但 **`/governance` 掛的是裸 `GovernancePage`、少了 WikiTrust tab，與正規路徑 `/manage/governance` 內容不等價** — 這是真 bug 等級的不一致。 |
| `/manage/system` | 1 | path 段名是 `system`、目的地卻是 `/app/system/settings`，三條 settings 別名中唯一字面語意不符的。 |

###### 7.4 孤兒與死碼（實錘）

| 項目 | 證據 | verdict | noul | v/c/r | level |
|---|---|---|---|---|---|
| **`_WipPlaceholder.tsx`**（35 LOC, 2026-07-18） | `grep -rl "WipPlaceholder" web/src` **只命中自己**；檔頭自述是 v2 改版期間的「建置中」代打 | **淘汰 0.85** | .8/.1/**1.0**/.05 | 1/1/**1** | **L2** |
| **`ApprovalsPage.tsx`**（251 LOC, 2026-08-13, 有 test） | 未被 App.tsx 或任何頁面 import；`/approvals` → `Navigate to="/inbox"`；**其自己的測試 describe 字串就寫著 `(MDS, unrouted — …)`**；卡片邏輯已被 `components/console/ApprovalRequestCard.tsx` **明文「duplicated here (not imported)」** 複製走 | **淘汰 0.7**／簡化 0.2 | .6/**.55**/**0.95**/.1 | 2/2/**2** | **L2** |
| `/legacy-dashboard` | 純 `Navigate to="/"`，**對應頁面檔案早已刪除**，只剩書籤相容 | 保留 0.7／淘汰 0.3 | .5/.1/.4/.0 | 1/1/1 | L2 |

###### 7.5 `newIn` 標籤

`apps/registry.ts:182-191` 的 `isNewFeature()` **只比 major.minor**，超過目標 minor 就自動不顯示 → **使用者畫面不會有陳舊徽章**。但 `nav-model.ts:91-96` 明訂「不要刪舊 `newIn` 欄位，它們是 inert」。

| newIn | 對應頁 | 對 current=1.65 | 狀態 |
|---|---|---|---|
| 1.58.0 | `/goals`、`/foresight` | 失效 | inert 死資料 |
| 1.60.0 | `/mail`、`/gallery` | 失效 | inert |
| 1.61.0 | `/presets` | 失效 | inert |
| 1.62.0 | `/console`、`/app/system/device`、`/manage/secaudit` | 失效 | inert |
| **1.66.0** | `/app/system/causal`、`/decision-lab`、`/ccr` | **仍顯示 New** | 唯一生效批（對應尚未 commit 的新頁） |

判定：**自我過期機制已存在，不是待修 bug**；8 個舊值是刻意保留的 inert 資料。唯一可商榷的是「永不清理」讓 `nav-model.ts` 逐年累積死欄位。

###### 7.6 其他 web 側發現

| 發現 | 證據 | 建議判定 |
|---|---|---|
| **決策科學三頁自成一套 REST 慣例** | `lib/causal-api.ts`(198)／`decision-api.ts`(932)／`ccr-api.ts`(144) 全部 `fetch('/api/…')`＋Bearer JWT，而全站其餘一律走 `client.call('ns.method')` WS JSON-RPC；三頁＋10 個子面板全是 **untracked 未 commit**，`newIn 1.66.0` | 非死碼，是**進行中的雙資料存取慣例技術債**——需要在 commit 前拍板「REST 或 WS RPC 二選一」 |
| `FineTunePage.tsx`(906) 與 `LocalModelsPage.tsx`(490) **零測試** | 同量級的 DevicePage(1163)／UsersPage(1085) 都有 test | 測試缺口，非去留問題 |
| `handlers.rs` 51,802 LOC 承載約 445 個 RPC | 見 §6.2 | 結構債 |

---

##### 8. OS-native 線與 OS 相關 crate

> 本節由子調查完成；三項關鍵結論（拆分邊界文件、Yocto 層已物理移除、shell/comp 零 CI 零依賴）我本人複查過，可視為 L2。

###### 8.0 先回答核心問題：OS crate 該不該跟著搬去 DuDuClaw-OS repo？

**已拍板過，且答案是「一個都不搬」。** `wiki/pm/repo-split-runbook-2026-09.md:6-13` 原文：

> 拍板邊界（2026-09-04）：**完整拆的 Yocto 層版**——OS repo 拿 Yocto 層（`meta-duduclaw/` + `appliance/` + `scripts/release-os.sh`）。Rust workspace **整份留主 repo**（`crates/` 所有成員，含 `duduclaw-shell/comp/os/sysd`——桌面 App 與 OS 都靠它；`duduclaw-shell`←`native-gui`(桌面)、`duduclaw-pets`←`src-tauri`(桌面)，硬抽會斷桌面 App 產品線）。

**已驗證執行完畢**：`ls meta-duduclaw appliance scripts/release-os.sh` → 三個都 `No such file or directory`；`scripts/` 底下 grep `appliance|os|image|vm|yocto|qemu` **零命中**。唯一名字像的 `scripts/box-setup/` 是另一回事（把 DuDuClaw 裝在使用者自己的 Mac mini/NAS 上的 Docker 指南），與 Yocto 映像無關、拆分後仍有效。

**所以這一節不是「找出該搬走的」，而是「檢視 2026-09 那個決定現在是否仍成立」。**

###### 8.1 os_* 感知／操作四件套（跨平台，**不是** appliance 專屬）

| 功能 | 入口 | 接線 | 最後變更 | 測試 | LOC | 文件 | verdict | noul | v/c/r | level |
|---|---|---|---|---|---|---|---|---|---|---|
| `os_events.rs` 檔案監看 | per-agent `agent.toml [os_watch] paths`（經 `agents.update` RPC）＋MCP `os_watch_status`；`server.rs:1448/1455/1525` 三處啟動 | live（`[capabilities] os_native` 預設關） | 2026-08-15 | 26 | 1248 | `docs/features/33` | 保留 0.85 | .05/.1/.2/.15 | 4/3/4 | L2 |
| `os_frontmost.rs` 前景視窗輪詢 | `[os_watch] frontmost_poll_secs`；`server.rs:1462` | live | 2026-08-15 | 9 | 525 | 同上 | 保留 0.8 | .05/.1/.25/.1 | 3/2/3 | L2 |
| `os_intent.rs` 對話→OS 意圖分類 | `channel_reply.rs:2448` **每一輪對話**都呼叫 | live | 2026-08-26 | 32 | 1503 | **無公開 feature 文件** | 保留 0.9 | .0/.05/.05/.25 | 4/3/4 | L2 |
| `os_operator.rs` ShortCircuit/Guide/Continue | `channel_reply.rs` 多處 | live | 2026-09-01 | 60 | 1443 | **無公開 feature 文件** | 保留 0.9 | .0/.05/.05/.25 | 4/3/4 | L2 |
| `proactive_gate.rs` / `proactive_feedback.rs` / `posture_watch.rs` | 主動感知閘 | live | 07-23／07-23／09-01 | 13／25／9 | 849／1229／479 | `docs/features/33` | 保留 0.85 | .05/.05/.2/.15 | 4/3/3 | L1 |

⚠️ **命名陷阱（子調查的重要發現）**：`crates/duduclaw-os` 這個 crate 是「跨平台 OS 環境整合（檔案監看／通知）」，**不是** Yocto 版「DuDuClaw OS」發行版——只是撞名。討論去留時必須先分清楚這兩件事。

###### 8.2 device.* 系列（真正 appliance-only，但有雙重 gate）

| 功能 | 入口 | 接線 | 最後變更 | 測試 | LOC | verdict | noul | v/c/r | level |
|---|---|---|---|---|---|---|---|---|---|---|
| `device.rs` | `device.status` / `device.network` RPC | live，**`require_admin!()` + `require_appliance!()` 雙 gate**，非 appliance 一律回 `not_appliance` | 2026-09-23 | 16 | 495 | 保留 0.85 | .1/.05/.3/.1 | 3/2/3 | L2 |
| `device_about.rs` | 裝置資訊卡 | live | 2026-08-24 | 16 | 441 | 保留 0.8 | .1/.05/.3/.1 | 2/2/2 | L2 |
| `device_ops.rs` | `device.power` / `factory_reset` / `backup_*`，經 `duduclaw-sysd` UDS | live（同雙 gate） | 2026-09-23 | 18 | 1089 | 保留 0.85 | .1/.05/.3/.15 | 3/3/3 | L2 |

13 個 `device.*` RPC 已逐一在 `handlers.rs:8169-8248` grep 命中，**全部有真 handler，無死 RPC**。

###### 8.3 codrive／network／bridges（語意綁死 appliance，但物理上嵌在 gateway crate 內）

| 功能 | 入口 | 接線 | 最後變更 | 測試 | LOC | verdict | noul | v/c/r | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `codrive/`（19 檔） | MCP `codrive_run` / `codrive_status`（Admin scope，`mcp.rs:628/647/13777`）；`[capabilities] codrive` | **半live**：MCP 前門真的掛上，但另一半（`duduclaw-comp` 的 agent-injection socket）是 workspace **exclude 的 Linux-only crate**——一般 mac/Linux 安裝下呼叫必然連線失敗 | — | `live_tests.rs` 6 個全 `#[ignore]`（需 comp 容器/VM rig）；其餘 5 個 `tests_*.rs` 對 fake-comp 跑 | **9,385** | 保留 0.5／簡化 0.25／淘汰 0.25 | .15/.1/**.5**/**.5** | 3/**4**/3 | 最強保留：人機共駕是值班機的招牌能力，且 `mod.rs` 刻意不依賴 comp、手抄 wire types 以保持解耦；最強淘汰：**9.4k LOC 嵌在 gateway crate 裡，但在非值班機環境 100% 無法運作**，活測骨架預設不跑，`docs/features` 無專篇。|
| `network/`（8 檔） | RPC `network.wifi_scan/connect/forget/status`（`handlers.rs:49561-49637`）＋HTTP＋MCP＋CLI `duduclaw os network` | live，但 `iwd.rs` 在非 Linux **連編譯都不可能**（`zbus` 是 `target_os="linux"` gated），mac 上回 `BackendUnavailable` | — | ipinfo 16／tests_network 26／portal 11／tests_iwd 13／wired 25／sysfs 10 | — | 保留 0.7／簡化 0.2 | .1/.1/**.4**/.25 | 2/3/3 | L1 | 值班機 Wi-Fi 設定的唯一入口，且刻意把純函式下放到 `tests_*.rs` 讓 mac 開發機也能測；淘汰理由：一般安裝下四個 RPC 永遠無效。|
| `display_bridge.rs` | agent → comp `shell_control` socket | live（comp-only 語意） | 2026-08-26 | 7 | 424 | 保留 0.65／簡化 0.2 | .15/.1/**.45**/.15 | 2/2/2 | L1 | 同 codrive，綁死 comp。|
| `audio_bridge.rs` | agent → `wpctl` subprocess（**不經 comp**） | live | 2026-08-27 | 24 | 693 | 保留 0.7 | .1/.15/.35/.15 | 2/2/2 | L1 | 走 subprocess 而非 comp socket，耦合較鬆。|
| `uki_patch.rs` | 被 `os_update.rs` 呼叫，UKI root PARTUUID 二進位修補 | live | 2026-09-02 | 18 | 594 | 保留 0.6／簡化 0.15／淘汰 0.25 | .2/.05/**.5**/.15 | 2/2/**2** | L1 | **語意上 100% Yocto/systemd-boot A/B 分割專屬**——是本次最像「應該跟 Yocto 層一起搬去 OS repo」的一塊，但它在 gateway crate 內、被 `os_update.rs` 依賴。|
| `mdns.rs` | `[server] mdns_advertise` 預設關 | live | 2026-07-27 | 10 | 391 | 保留 0.85 | .05/.1/.25/.1 | 3/1/3 | L2 | **通用功能**（LAN 上被桌面 App 探索），appliance 只是使用情境之一。|
| `relay_client/config/device.rs` | `[relay] enabled` 預設 false，appliance 模式預設 true；目前**只支援 LINE** | opt-in／appliance 預設開 | 2026-08-20 | 10／18／9 | 1370 | 保留 0.8 | .05/.15/.3/.15 | 3/3/3 | L1 | NAT/CGNAT 後方的 gateway 收 webhook 的唯一解，且刻意復用 `line::handle_line_webhook` 同一條驗簽路徑；淘汰理由：只支援一條通道、需要一台雲端 relay 才有意義。|
| cli `os_drive/`（7 檔）＋`mcp_os_ops.rs`（1519 LOC） | `duduclaw os <group> <verb>` CLI ＋ agent 面 MCP 橋 | live | 2026-08-27 | — | — | 保留 0.85 | .05/.1/.2/.2 | 3/2/3 | L1 | 「同一能力、兩個前門、同一套 gate」的正確做法；無明顯淘汰理由。|

###### 8.4 八個 OS／桌面相關 crate 的存留

| Crate | 檔／LOC | 最後變更 | workspace | 主 workspace 內誰依賴 | CI | 平台 | verdict | noul | v/c/r | level |
|---|---|---|---|---|---|---|---|---|---|---|
| `duduclaw-os` | 8／1,750 | 2026-07-23 | members | **gateway + cli 真依賴** | `--workspace` 涵蓋 | 跨平台 | 保留 0.95 | .0/.05/.05/.05 | 4/1/4 | L2 |
| `duduclaw-sysd` | 7／3,797 | 2026-09-23 | members | **gateway 真依賴**（device_ops 用它下 reboot/poweroff） | `--workspace` 涵蓋 | appliance 語意，程式碼跨平台可編 | 保留 0.85 | .1/.05/.25/.1 | 3/2/3 | L2 |
| `duduclaw-desktop` | 4／702 | 2026-07-04 | members | gateway optional（`desktop` feature，**在 default 裡**，L5b computer-use） | 涵蓋 | 跨平台 enigo | 保留 0.85 | .05/.1/.2/.15 | 3/1/3 | L2 |
| `duduclaw-pets` | 9／2,145 | 2026-07-28 | members | **只有 `src-tauri`（桌面 App）**；gateway/cli 不依賴 | 涵蓋（但真消費者在 exclude 的 src-tauri） | 跨平台 | 保留 0.7／簡化 0.2 | .1/.15/.3/.2 | 2/2/3 | L1 |
| `duduclaw-relay` | 18／2,435 | 2026-08-20 | members | gateway **只在 `[dev-dependencies]`**（測試起真 relay router） | 涵蓋 | 跨平台，獨立 Cloud Run 部署 | 保留 0.75 | .05/.15/.3/.15 | 3/2/3 | L2 |
| **`duduclaw-shell`** | **119／57,302** | 2026-09-24 | **exclude** | **零**（主 workspace 內零 grep 命中；它自己依賴 `duduclaw-native-gui`） | **零 CI**（不在 `--workspace`，無專屬 workflow，只有 `BUILD-LINUX.md`） | gpui git dep | 保留 0.55／簡化 0.15／**取代 0.1／淘汰 0.2** | .2/.1/**.45**/.35 | 3/**5**/3 | **L1** |
| **`duduclaw-comp`** | **66／31,804** | 2026-09-24 | **exclude** | **零** | **零 CI**（只有 `BUILD.md` Docker 手動建置） | **Linux-only**（wayland-server/libinput/udev） | 保留 0.5／簡化 0.15／**淘汰 0.35** | .25/.05/**.5**/.35 | 3/**5**/3 | **L1** |
| `duduclaw-native-gui` | 174／67,491 | 2026-09-24 | exclude | 被 `duduclaw-shell` 依賴 | **有** `native-gui-desktop-release.yml`，但**只在 tag `native-gui-v*` 或手動 dispatch 觸發**，且 Windows/Linux leg 是 `if: false` 的 placeholder | gpui，**今日僅 macOS** | 保留 0.8 | .1/.1/.2/.3 | 4/**5**/4 | L1 |

`src-tauri/`（根目錄，workspace exclude）只依賴 `duduclaw-pets`，與 `duduclaw-desktop` 無依賴關係；有自己的 `desktop-release.yml`（tag `desktop-v*`）。

**三個 exclude 的 gpui/smithay crate 合計 156,597 LOC**，佔本次盤點範圍的最大單一體量，卻**全部不在日常 CI 裡**——它們壞了不會被任何自動流程抓到。

###### 8.5 OS／裝置 dashboard 頁面的後端核對

| 頁面 | LOC | RPC | handler 存在？ |
|---|---|---|---|
| `OSPage.tsx` | 695 | `os.status`／`settings.update`／`gate.recent`／`events.recent`／`events.subscribe`／`events.unsubscribe`／`doctor.run` | **全部命中**（`handlers.rs:7989-8019`） |
| `DevicePage.tsx` | 1163 | 13 個 `device.*` ＋ `maintenance.logAccess` | **全部命中**（`handlers.rs:8169-8248`） |
| `SystemHomePage.tsx` | 145 | 無 RPC（純卡片導覽） | N/A |
| `SystemUpdatePage.tsx` | 26 | 內嵌 `<UpdateTab/>` | 未查（元件內部） |
| `PetStudioPage.tsx` / `MascotOverlayPage.tsx` | 403／174 | **完全不經 gateway**，走 Tauri IPC → `duduclaw-pets` | N/A（桌面 App 線） |
| `WorldPage.tsx` | 47 | 只呼 `agents.list` | 與 OS 無關 |

**沒有任何 OS/device dashboard 頁面指向不存在的後端。**

###### 8.6 OS 線的文件缺口（子調查發現，我確認過 CLAUDE.md 版本標示）

1. **根 `CLAUDE.md` 的「Architecture Overview」標示 `(v1.15.0)`，實際版本 1.65.1**（`CLAUDE.md:3`，我本人複查）；全檔 `grep -iE "os_native|codrive|appliance|os-native"` **唯一命中是第 104 行的「macOS-native polish」**（美學段落的形容詞），也就是**整條 OS-native 線（v1.42–v1.65）、codrive、appliance/device 完全沒有進入專案自己的架構總覽**。這是 L2 實錘，且與 §4（瀏覽器五層敘事）、§6.1（sender 已收斂敘事）並列為 CLAUDE.md 需要誠實化的三處。
2. `os_intent.rs`／`os_operator.rs`／`codrive/`／`network/`／`relay_client.rs` 在 `docs/features/*.md` **皆無專屬公開篇**，只存在於 L3 `commercial/docs/` 與程式內註解。

---

##### 9. 候選清單（通過門檻：verdict argmax ≠ 保留 且 conf ≥ 0.5，或任一 noul p ≥ 0.6 且 level ≥ L1）

依「證據強度 × 節省成本」排序。每項附最強保留理由、最強淘汰理由、連帶處理清單。

###### C-1 `browser_router.rs`（438 LOC）＋ `docs/features/08-browser-automation.md` — **L2，淘汰 0.65**

- **最強保留**：「用最便宜的層級做完事」的五層升級模型是對的工程直覺，而且 `BrowserRestrictions`（`allow_form_submit`、`max_pages`、`max_minutes`、信任域升級）是一套已經寫好的 per-agent 瀏覽器政策模型——將來若要做真正的自動分層，這是現成骨架。
- **最強淘汰**：`grep -rn "browser_router"` 扣掉自身只剩 2 行**註解**引用；`BrowserTier`／`BrowserRestrictions`／`select_tier` 全 repo 零使用；`pub mod browser_router;` 是唯一活口；2026-04-02 起五個半月零變更；**CHANGELOG 全文（8,132 行）零次提及**——它從未作為功能出貨過。而它撐著的那篇公開文件正在對外宣傳一個不存在的 L4「Sandbox Browser」（`duduclaw-sandbox` 其實是 Seatbelt/Landlock 行程侷限，不是瀏覽器容器）。
- **若淘汰要一併處理**：① `lib.rs:202` 的 `pub mod`；② `docs/features/08-browser-automation.md` 與 `docs/features/README.md:20` 的索引列；③ CLAUDE.md「Browser automation & computer use（5-layer auto-routing）」那條 bullet 改寫成「L1 `web_fetch` / L2 `web_extract` / L5 computer use 三個 MCP 工具，由模型自選」；④ `approval.rs:17` 與 `codrive/registry.rs:26` 兩處引用它的註解；⑤ 12 個測試。**無依賴 crate 可移除**（純本地型別）。

###### C-2 `discord_voice.rs`（563 LOC）＋ `songbird` 依賴 — **L2，淘汰 0.6**

- **最強保留**：完整的「Discord 語音房收音 → ASR → agent → TTS 回話」管線是一條真正差異化的互動形態，重寫成本不低；且 `stt.rs`／`tts.rs` 已經活著，只差把它接上。
- **最強淘汰**：`grep -rn "discord_voice"` 扣掉自身只剩 `lib.rs:160` 的 `pub mod`（零呼叫端）；`discord-voice` **不在 `default = ["dashboard","desktop"]`**，預設建置根本不編譯；CHANGELOG 全文唯一一次提到它是 **`.cargo/audit.toml` 為了它而抑制 RUSTSEC-2026-0293**（`ringbuf` 0.4.8 double-free，songbird 是唯一消費者、上游仍 pin 0.4 無法升版）——也就是**一個零呼叫端的功能正在讓專案背一條安全公告豁免**。
- **安全負擔實錘**：`.cargo/audit.toml` 的 10 條 ignore 裡，**至少 3 條掛在 songbird/serenity 語音棧上**——`RUSTSEC-2024-0388`（derivative unmaintained，註解直寫「blocked by songbird 0.5.x」）、`RUSTSEC-2025-0134`（rustls-pemfile，「via serenity/songbird old rustls stack」）、`RUSTSEC-2026-0293`（ringbuf double-free，註解自述 songbird 是唯一消費者且上游 0.6.0 仍 pin 0.4）。**一個零呼叫端、預設不編譯的功能，正在讓專案背三條安全公告豁免。**
- **順帶發現（`audit.toml` 自身已腐）**：`RUSTSEC-2023-0071` 的註解寫「Via: jsonwebtoken → **livekit-api**」，但 `grep -rn livekit --include=Cargo.toml .` **零命中**、`Cargo.lock` 內 `livekit` 出現 **0 次**——這條 ignore 的理由已經指向一個不存在的依賴，應一併重驗（可能整條可刪，也可能真因改成別的路徑）。
- **若淘汰要一併處理**：① `lib.rs:160`；② `Cargo.toml:158` 的 `songbird` optional dep 與 `:184` 的 `discord-voice` feature；③ **`.cargo/audit.toml` 的三條 songbird 系 ignore 可一併拿掉**（實質收益），並順手重驗 `RUSTSEC-2023-0071`；④ 6 個測試；⑤ `docs/features/14-voice-pipeline.md` 一併修（見 C-3）。

###### C-3 `docs/features/14-voice-pipeline.md` 文件腐爛 — **L2，簡化 0.55／淘汰 0.35**

- **最強保留**：STT/TTS 本身是活的（`/api/voice/*`、Telegram 語音回覆、MCP tts 工具），使用者需要一篇文件。
- **最強淘汰／改寫**：文件主打「SenseVoice(ONNX)／Whisper.cpp／VAD／LiveKit 語音房」四大件，**`grep -rn "livekit\|SenseVoice" crates/` 零命中，`Cargo.lock` 內 `livekit` 出現 0 次**（LiveKit 曾經是依賴——`.cargo/audit.toml` 還留著一條指向 `livekit-api` 的 ignore 註解——但已被移除）；`docs/features/README.md:26` 的索引摘要也照抄了「LiveKit」。實際只有 OpenAI-compat STT＋本地 command STT＋MiniMax/Edge TTS。依 readme-and-docs 規範「過時文件比沒有文件更糟」，這是主動誤導。
- **連帶**：`docs/features/README.md:26`、`ja-JP/`＋`zh-TW/` 兩份鏡像、`config/duduclaw.example.toml` 應補上 `[voice]` 段（目前完全沒有，STT/TTS 發現度為零）。

###### C-4 LINE OA B2C 多帳號＋點數計費（`credit.rs` 228 LOC＋`LineAccount`＋`duduclaw ops credit`） — **L2，淘汰 0.5／取代 0.25**

- **最強保留**：這是 **B2C 轉售／雲端代管**商業模式的唯一技術落點（一台 gateway 托管多個客戶的 LINE 官方帳號，各自計點）；帳本、費率換算、operator CLI 都已寫完並有測試。若「經銷商托管 LINE OA」是要走的路，砍掉等於放棄那條線。
- **最強淘汰**：三個獨立 grep 都是零：`CreditLedger` 全 repo **只有 1 個呼叫端**（就是那支 CLI 自己）；`LineAccount::resolve_accounts()` **呼叫端 0**；`line.rs` 內 `destination` 字串 **零命中**（多 OA 路由的關鍵欄位從未被讀）。`handlers.rs` 也沒有任何 `credit.*` RPC。文件 `line-oa-b2c.md` 的 Status 段自己承認「路由＋扣點是剩下的整合步驟」。2026-07-11 起兩個半月零變更。**這不是「預設關的功能」，是一個從未接線的半成品。**
- **若淘汰要一併處理**：① `credit.rs`＋`credits.db`；② `duduclaw ops credit` 三個子命令（`lib.rs:9581-9610`）；③ `duduclaw_core::types::LineChannelConfig::accounts` / `LineAccount` / `resolve_accounts()`；④ `docs/guides/line-oa-b2c.md`（整篇）；⑤ 2 個測試。**替代方案**：若要保留商業模式，正確做法是把它接到既有的 `budget.rs`／`license` 配額層，而不是維護第二套點數帳本。

###### C-5 `webhook.rs` 通用 webhook 入口（280 LOC） — **L2，淘汰 0.7**

- **最強保留**：「外部系統 HMAC 簽名後直接丟任務給某個 agent」是 MCP／ACP 之外的第三條整合形態，Zapier/n8n/IFTTT 類整合會用到，重寫要花時間。
- **最強淘汰**：`webhook_router` 與 `WebhookState` **全 repo 零引用**，`server.rs` 從未 mount 任何 `/webhook/{agent}` 路由——這個端點**從來不曾存在於執行中的 gateway**。2026-04-06 起六個月零變更。同樣的需求現在由 `duduclaw http-server` 的 Bearer `/mcp/v1/call` 與 `/ingest/transcript`（`shortcuts-and-wearables.md` 就是用這條）以及 Remote MCP／ACP 覆蓋。
- **連帶**：`lib.rs` 的 `pub mod webhook;`；無文件、無測試、無 config 鍵可清。注意**不要**誤刪 `webhook_jwt.rs`（Google Chat／Teams 的 JWT 驗簽在用，是活的）。

###### C-6 `web/src/pages/_WipPlaceholder.tsx`（35 LOC） — **L2，淘汰 0.85**

- **最強保留**：下次做 v2 大改版時還會想要一個「建置中」代打元件。
- **最強淘汰**：`grep -rl "WipPlaceholder" web/src` **只命中自己**；v2 改版期間掛它的路由都已換成真頁面。留著讓「這頁是不是還沒做完」的問題每次都要重查一次。
- **連帶**：無（零 import、零 test、零路由）。

###### C-7 `web/src/pages/ApprovalsPage.tsx`（251 LOC＋test） — **L2，淘汰 0.7**

- **最強保留**：核准中心的「整頁清單＋逐項卡片」比 Inbox 的混合流更適合大量待審場景；後端 `approvals.decide/list` 仍是活的，重新掛回路由的成本接近零。
- **最強淘汰**：未被任何頁面 import；`/approvals` 已 `Navigate to="/inbox"`；**它自己的測試 describe 字串就寫著 `(MDS, unrouted — see App.routes.test.tsx)`**——開發者已自我標記；更關鍵的是 `components/console/ApprovalRequestCard.tsx:12-13` 明文寫「duplicated here (not imported)」把它的 kind→標籤 fallback 邏輯**複製**走了，所以現在是「死頁 + 一份手抄副本」的最糟組合（改一邊不會同步）。
- **連帶**：`ApprovalsPage.tsx`＋`ApprovalsPage.test.tsx`；`/approvals` 重導可保留；**必須確認 `ApprovalRequestCard.tsx` 的那份手抄副本是完整的**再刪。

###### C-8 Telegram Mini App（`miniapp.rs` 1400 LOC） — **L1，淘汰 0.4／保留 0.45（邊界案例，交使用者拍板）**

- **最強保留**：設計是全 repo 少見的嚴謹（initData 驗簽、`route_press` 共用、`authorize_press` 同一套授權、預設 404 fail-closed），且「在 Telegram 裡看完整審批詳情＋模擬後果＋倒數」確實解決按鈕卡片塞不下上下文的真問題；三語文件齊全。
- **最強淘汰**：預設關 + `config/duduclaw.example.toml` 沒有 `[miniapp]` 段 + **web dashboard 零 UI 開關**（`grep miniapp web/src` 零命中）+ `deployment-guide.md` 自己標 “experimental” + 它自述是「D-S1 spike，刻意只做一個畫面」+ 只服務十一通道中的一條。以「1,400 LOC 換一個實驗性單畫面」計價偏高。
- **中間選項（建議）**：不刪，但**補一個 dashboard 開關＋example.toml 段**把它從「實質不可達」變成「可達的 opt-in」，再用一個版本週期看有沒有人開。若仍無人用，下個版本再砍。

###### C-9 `/governance` legacy 路由與正規路徑不等價 — **L2，這是 bug 不是去留題**

- `/manage/governance` → `GovernanceShell`（Governance + **WikiTrust** 兩個 tab）；`/governance`（legacy，enterprise）→ 裸 `GovernancePage`（**無 WikiTrust**）。從舊書籤進來的企業用戶看不到 Wiki 信任頁。
- **處理**：把 `App.tsx:474` 改成 `<GovernanceShell />` 或改成重導，與其餘 17 條已收斂的 A 類別名一致。

###### C-10 四份同形通知模組合併（`goal_notify` 3180＋`approval_notify` 951＋`install_notify` 982＋`autopilot_notify` 804 = 5,917 LOC） — **L1，簡化 0.25**

- **最強保留**：四者的授權、卡片形狀、降級策略已經共用 `decision_notify::authorize_press`／`decision_card`／`channel_capabilities`，剩下的差異是真實的（goal 有三顆按鈕、install 有簽核、autopilot 有斷路器狀態）；動它風險高（HITL 是不可逆動作的守門）。
- **最強簡化理由**：四個模組的 doc comment **互相引用承認同形**（「mirroring `install_notify.rs`」「the same situation `autopilot_notify` is in」）；`autopilot_notify` 只有 2 個呼叫端；`goal_notify` 為了 inline keyboard 又自己手刻了 TG/Slack/Discord 三個 POST。合併成 `notify::push(card, dest)` 一個入口是本領域最高價值的重構。
- **連帶**：需同步 `docs/features/40-notification-governance.md`；不可在同一個 commit 內改授權邏輯。

###### C-11 `channel_reply.rs` 13,543 LOC — **L1，簡化 0.45**

- **最強保留**：它是平台最核心的一條路徑（十一通道共用），27 個 pub 項、170 個測試；任何拆分都有把 CCR 租約／壓縮／帳號輪替／computer-use 的互動順序改壞的風險。
- **最強簡化理由**：CLAUDE.md 自訂「200-400 typical, 800 max」，這一檔是上限的 17 倍；檔內已經有天然切點（`// ── Python SDK subprocess`、`// ── PTY-routed`、`// ── Direct API delegate`、`// ── Streaming progress types`）。建議**先抽 `streaming`／`pty` 兩段**（相對獨立），不要一次全拆。
- **同類**：`handlers.rs` 51,802／`server.rs` 16,294 也超標，但那是其他領域。

###### C-12 中國系通道三件組（wecom 1238／dingtalk 873／feishu 908 = 3,019 LOC） — **L1，保留為主但請使用者拍板**

- **最強保留**：**分版原則明訂「免費核心不閹割」**，且三者共用 `channel_sender`／`markdown_render`／`channel_capabilities` 抽象，刪掉省不了共用層；一旦有中國／跨境客戶，重建成本（含三套獨立密碼學驗簽）遠高於維護成本。
- **最強淘汰**：產品定位是台灣 zh-TW，三者能力矩陣最低（dingtalk 六項能力全 false，只剩純文字＋markdown），測試數也最少（7-14）；每一輪跨通道修正（最近一次是 CCR principal 的五個 webhook 通道 fail-closed）都要同步三份。
- **我的判定**：**不列為淘汰建議**，但列出來讓使用者知道這 3k LOC 的實際持有成本。**無使用數據可讀，此判斷為 L1（定位＋文件推論），不是實測。**

---

###### C-13 `duduclaw-comp`（66 檔／31,804 LOC）＋`duduclaw-shell`（119 檔／57,302 LOC） — **L1，保留 0.55／0.5（邊界，強烈建議拍板）**

- **最強保留**：這是 **DuDuClaw OS 的整個使用者層**——`duduclaw-comp` 是自建 smithay compositor（D11 拍板「自建，拒 cosmic-comp fork」），`duduclaw-shell` 是 gpui 殼；2026-08-20 的 S0–S2 收官有使用者親自放行的 E2E（開機即殼＋dark Home）。它們也是 `codrive`／`display_bridge` 這兩組 gateway 程式碼唯一的對手方——砍掉它們，那 9.8k LOC 立刻變成真死碼。
- **最強淘汰／搬離**：合計 **89,106 LOC，零主 workspace 依賴、零 CI**（不在 `--workspace`，無任何 `.github/workflows/*` 提及，只有各自的 `BUILD.md`/`BUILD-LINUX.md` 手動指南）；`duduclaw-comp` 是**純 Linux-only**（wayland-server/libinput/udev）。它們壞了沒有任何自動流程會發現。而 Yocto 層本身已經在 2026-09-04 搬去 `DuDuClaw-OS` repo 了——**OS 的使用者層留在平台 repo、OS 的建置層在另一個 repo，這個切法本身就違反「一個產品線一個 repo」**。
- **當時的拍板理由值得重新檢視**：runbook 寫「`duduclaw-shell`←`native-gui`(桌面)、`duduclaw-pets`←`src-tauri`(桌面)，硬抽會斷桌面 App 產品線」。`←` 的方向在該句裡是歧義的，所以我直接查了 Cargo.toml：
  - `crates/duduclaw-shell/Cargo.toml:76`：`duduclaw-native-gui = { path = "../duduclaw-native-gui" }` → **shell 依賴 native-gui**。
  - `crates/duduclaw-native-gui/Cargo.toml`：`[dependencies]` 內**沒有任何 `duduclaw-*` path 依賴**（只有註解提到別的 crate）→ **native-gui 不依賴 shell**。
  - `duduclaw-comp`：主 workspace 內零依賴方。
  → 也就是說 **pets 那半句成立**（`src-tauri` 真的依賴 `duduclaw-pets`），**但 shell 那半句不成立**：搬走 `duduclaw-shell`／`duduclaw-comp` **不會**斷桌面 App 產品線（`native-gui` 有自己的 macOS release pipeline，且它不 import shell）。**這是本次盤點最值得使用者重看一次的一條——那個「不能搬」的理由，至少對 shell/comp 這兩個而言，依現在的 Cargo.toml 是不成立的。**（我沒有讀 2026-09-04 當時的 Cargo.toml，所以不能斷定拍板當下就錯；只能說**現況**不支持那個理由。）
- **若搬走要一併處理**：① `crates/duduclaw-shell/`＋`crates/duduclaw-comp/`（含各自 lockfile/BUILD 文件）；② 根 `Cargo.toml:13-19` 的兩條 exclude 註解；③ **`codrive/`（9,385 LOC）與 `display_bridge.rs`（424）要一起決定**——它們是 comp 的 gateway 側對手方，comp 走了就該跟著走或改成跨 repo 協定；④ `crates/duduclaw-gateway/Cargo.toml:129-132` 的 zbus/comp 註解；⑤ `docs/features/50-duduclaw-os-appliance.md`／`52-desktop-edition.md` 的交叉引用。
- **保守替代**：不搬，但**至少把它們納入 CI**（一個 `cargo check` job 即可），否則 89k LOC 處於無人看管狀態。

###### C-14 `uki_patch.rs`（594 LOC） — **L1，保留 0.6／淘汰 0.25（拆分殘留）**

- **最強保留**：它是 appliance A/B 更新機制的核心（UKI root PARTUUID 二進位修補），被 `os_update.rs` 呼叫，有 18 個測試，2026-09-02 才動過。
- **最強淘汰／搬離**：語意上**100% 屬於 Yocto/systemd-boot A/B 分割**，與 `meta-duduclaw/` 是同一條產品線，但那一層已經搬到 OS repo 了。它留在主 repo 是因為物理上嵌在 gateway crate 內，不是因為概念上屬於這裡。
- **處理**：與 C-13 綁在一起拍板；單獨搬它價值不大。

###### C-15 `codrive/`（19 檔／9,385 LOC） — **L1，保留 0.5／簡化 0.25／淘汰 0.25**

- **最強保留**：人機共駕（agent 在真桌面上照腳本操作 GUI）是值班機最難複製的能力；`mod.rs` 刻意不依賴 `duduclaw-comp`、手抄 wire types 以保持 crate 解耦，是好設計；MCP 前門（`codrive_run`/`codrive_status`，Admin scope）確實掛上了。
- **最強淘汰**：**在非值班機環境（絕大多數安裝）100% 無法運作**——另一半是 workspace exclude 的 Linux-only compositor socket；6 個 `live_tests.rs` 全部 `#[ignore]`（需要 comp 容器/VM rig 才能跑）；`docs/features/*` 無專篇。9.4k LOC 是本領域第二大單一體量。
- **處理**：命運綁定 C-13。若 comp 留下 → codrive 保留但應補公開文件與一條真能跑的 CI 活測；若 comp 搬走 → codrive 一起搬。

###### C-16 `[voice]`／`[miniapp]`／`[mail]`／`[relay]`／`[takeover]`／`[tick]` 等 config 段在 `config/duduclaw.example.toml` 完全缺席 — **L2，簡化（文件缺口，非去留）**

`config/duduclaw.example.toml` 只有 253 行，grep 上述六個段名**零命中**。這直接造成 §3 的「miniapp 實質不可達」與 §5 的「STT/TTS 發現度為零」。建議在同一個 commit 內補齊（依 readme-and-docs 規範「行為變更要 grep 全文件掃舊敘述」）。

---

##### 10. 我沒盤到／交給其他盤點者的

###### 已盤到（本報告涵蓋）
11 通道本體、`channel_*` 全部輔助層、`goal_notify`、reminders、agent mail、OTP／pairing、LINE OA B2C／NFC／LIFF（後者證實未實作）、`miniapp`、shortcuts/wearables、computer use 與「瀏覽器五層」、語音三件、通知／HITL 七件、OS-native 全線、八個 OS/桌面 crate、appliance scripts、web dashboard 92 頁／103 路由／`newIn`／i18n。

###### 未盤到（誠實列出）

- **不在我領域**：決策實驗室（`decision_*` 20 餘檔）、因果／CCR、記憶與演化、技能生命週期、任務板後端、授權／白牌／經銷、ERP／Odoo、LLM 路由與推論。§6.3 列出的 8 個 `refs=0` 模組屬這些領域，僅轉交線索。
- **`SystemUpdatePage.tsx` 內嵌的 `<UpdateTab/>` 元件內部 RPC**：未展開追查。
- **`scripts/release.sh` 是否仍有 Yocto 版號同步殘留**：runbook §5/§7 說已移除，但我未逐行覆核原始碼。
- **`commercial/docs/` 的 L3 設計文件內容**：只確認路徑存在（`DESIGN-codrive-desktop-2026-08.md`、`DESIGN-network-settings-2026-08.md`、`DESIGN-os-self-drive-2026-08.md`、`TODO-per-agent-channels.md`），未逐篇讀。
- **2026-09-04 拍板當下的 `Cargo.toml` 內容**：未回溯 git history，所以 C-13 只能說「現況不支持那個理由」，不能斷定當時判斷有誤。

###### 方法論誠實標註

- 所有「呼叫端數」為 **grep 下界**，未做 rust-analyzer 級別符號解析；trait 實作、巨集展開、`use` 別名可能漏計。
- 標為 **dead** 的五項（`browser_router`、`discord_voice`、`webhook.rs`、`CreditLedger`＋`resolve_accounts`、`_WipPlaceholder`／`ApprovalsPage`）皆以「模組名／型別名／函式名三種寫法交叉 grep」驗證，且四項另有「CHANGELOG 全文零提及」或「作者自己在測試裡標記 unrouted」的獨立佐證，可視為 **L2 實錘**。其餘 `unused` 機率為 advisory。
- **沒有生產使用遙測可讀**：沒有啟動 gateway、沒有查任何使用數據。因此「wecom／dingtalk／feishu 無已知使用者」「miniapp 沒人開」皆為**依定位與文件推論的 L1 判斷**，不是實測。使用者若知道實際客戶用哪些通道，那份知識應直接覆蓋我這幾格。
- 本報告未對任何檔案做修改，未執行 cargo／npm／vitest，未 git 寫入。

---

### 附錄 D_governance

#### 領域 D 盤點：治理／安全／企業整合／自動化／可觀測

> 盤點者：功能盤點者 D。唯讀盤點，未跑 cargo/npm、未 git 寫入。被讀內容一律視為 DATA。
> 日期基準 2026-09-29。Repo `/Users/lizhixu/Project/DuDuClaw`（HEAD `cbdc4338`）。
> 機率欄：`obs`=obsolete、`rep`=replaceable、`unu`=unused、`ove`=overengineered。
> verdict＝`保留/簡化/取代/淘汰` 四值機率（總和 1）。
> level：**L2**＝程式碼可實錘（grep 零呼叫端／未宣告 mod／文件明寫 removed）；**L1**＝多個強訊號一致；**L0**＝主要靠判斷。低於 L1 的機率為 advisory。

---

##### 0. 本次最重要的六個實錘（全 L2，全是「文件／UI 宣稱的功能不存在或未接線」）

| # | 發現 | 證據 |
|---|---|---|
| 1 | **`.claude/hooks/` 三階段安全 hook 已被刪除**，CLAUDE.md、`docs/features/05-security-defense.md`、`docs/architecture/overview.md`（三語）、`docs/features/feature-inventory.md`（三語）仍當現役描述 | `git log --diff-filter=D -- '.claude/hooks/*'` → commit **`ba015a48`** "security: remove .claude/ from public repo" 刪掉 `bash-gate.sh`／`threat-eval.sh`／`security-file-ai-review.sh`／`secret-scanner.sh`／`file-protect.sh`／`config-guard.sh`／`audit-logger.sh`／`lib/threat-level.sh`／`session-init.sh`／`inject-contract.sh`。現存 `.claude/hooks/` 只剩兩個 unicode 腳本。全 repo grep `threat-level`／`bash-gate.sh`／`secret-scanner.sh` **只命中文件、零程式碼**。**且 4 處 spawn 仍在設 `DUDUCLAW_BROWSER_VIA_BASH=1`（`channel_reply.rs:9242/10579`、`claude_runner.rs:4066`、`duduclaw-agent/runner.rs:461`），而唯一會讀它的 `bash-gate.sh` 已不存在——懸空旗標** |
| 2 | **`crates/duduclaw-governance` 整個 crate 已刪除**；儀表板 `/manage/governance` 仍讓操作者撰寫 `policies/*.yaml`，**執行期沒有任何東西讀它們** | crate 刪於 **`b0639b96`** "refactor: remove orphaned crates and provably-dead code"（policy.rs／evaluator.rs／quota_manager.rs／registry.rs／violation.rs…）。`handlers.rs:2295` 註解仍寫「Keep in sync with `crates/duduclaw-governance/src/policy.rs`」。全 repo `join("policies")` 僅 2 處，都在 `handlers.rs` 的 CRUD handler（12258／12273）。`GovernancePage.tsx` → `governance.list/upsert/remove` → 寫 `<home>/policies/*.yaml` → **零 enforcer** |
| 3 | **`crates/duduclaw-durability` 整個 crate 已刪除**；`docs/features/22-durability-framework.md` 與 `docs/features/feature-inventory.md`（**2026-09-24 才更新過**）仍宣稱存在且「Used by gateway LLM fallback + durable cron」 | 同 commit `b0639b96` 刪除 idempotency／retry／circuit_breaker／checkpoint／dlq。全 workspace grep `duduclaw_durability`／`duduclaw-durability` → **零命中**。`llm_fallback.rs` 實際未用任何 durability 元件 |
| 4 | **`crates/duduclaw-gateway/src/activation.rs` 從未被編譯** | 全 repo **零個 `mod activation` 宣告**指向它（唯一的 `mod activation;` 在 `skill_lifecycle/mod.rs:7`，指的是另一個檔）。158 LOC、2026-04-02、8 個永遠跑不到的測試。系統性掃描確認它是 gateway crate **唯一**的真孤兒檔 |
| 5 | **Odoo 事件同步（輪詢＋webhook）從未接線，但儀表板開關預設是「開」** | `PollTracker`／`poll_model`／`parse_webhook` 在 `duduclaw-odoo/src/events.rs` 之外**零生產呼叫端**（`lib.rs:16` 只是 re-export）。全 gateway 的 `.route("/webhook/…")` 有 line／googlechat／wecom／whatsapp／dingtalk／teams／feishu／`{agent_id}`，**就是沒有 odoo**。但 `web/src/pages/OdooPage.tsx:71` 的 `pollEnabled` **預設 `useState(true)`**、`handlers.rs:27157` 照實把 `poll_models`／`webhook_secret` 寫進 `config.toml`，而 `docs/features/12-industry-templates.md` 把「Event Synchronization」寫成現役能力。**使用者打開一個開關、看到它是開的、設定被存起來，然後什麼都不會發生** |
| 6 | **Identity 的 Notion／Chained provider 不在執行路徑上** | 兩個真正的生產消費點——`channel_reply.rs::build_sender_block`（`<sender>` 區塊注入）與 `identity_resolve` MCP 工具——都**硬編 `WikiCacheIdentityProvider`**，程式碼自己的註解寫「Step 2 of the migration plan: only `WikiCacheIdentityProvider` is available」。`NotionIdentityProvider`／`ChainedProvider` 只在儀表板測試用 RPC `identity.resolve` 裡被建構。但 `docs/features/25-identity-resolution.md` 敘述 Notion 是上游、斷線時「fall back to the wiki cache」，暗示 `ChainedProvider` 在執行期。另：RFC-21 承諾的 `identity_list_project_members`／`identity_invalidate_cache` 兩個 MCP 工具**從未實作**；`chained.rs`（321 LOC）**零測試** |

> 依 readme-and-docs 規範第 10 條「過時文件比沒有文件更糟」，1–3、5–6 都是**主動誤導**：操作者以為設了治理政策、以為有 durability 保障、以為有三層 hook 防禦、以為 Odoo 事件在同步、以為 Notion 是人員目錄上游——實際都沒有。這比功能該不該留更急。

---

##### 1. 治理／組織／權限

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 重疊/可取代 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話（最強保留／最強淘汰） |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Delegation policy engine（6 處 enforcement choke＋可見度過濾） | `delegation.get/set` RPC、`[delegation] policy`（預設 `department`） | live | 2026-08-07 | 29 | `docs/features/37-delegation-isolation.md` | 已取代舊 `rbac`／`check_supervisor_relation` | 1,477 | 0.92/0.06/0.01/0.01 | .05 | .05 | .03 | .25 | 5 | 3 | 5 | L2 | 保留：多人團隊唯一授權權威、全 fail-closed／淘汰：單人部署完全用不到 |
| Org authority store（`org.toml`＋`duduclaw ops org show\|sync`＋doctor drift） | CLI、`org.toml` RPC、`/org` | live | 2026-08-07 | 19 | CLAUDE.md WP22 | 無 | 1,053 | 0.85/0.12/0.01/0.02 | .05 | .10 | .05 | .30 | 4 | 3 | 4 | L2 | 保留：擋 agent 自改 `reports_to` 繞過授權／淘汰：seed-once＋CLI-only 對單人操作成本重 |
| Identity token（HMAC caller token） | `DUDUCLAW_AGENT_TOKEN`、`[delegation] require_identity_token` | opt-in，**預設 soft（false）**；無 `identity.key` 時整個 inert | 2026-08-15 | 8 | `37-delegation-isolation.md` | 無 | 485 | 0.80/0.15/0.01/0.04 | .10 | .05 | **.35** | .25 | 4 | 2 | 3 | L2 | 保留：冒名 delegation 的唯一密碼學防線／淘汰：預設 soft＋需逐 agent 重發 token，實務多半沒開 |
| `org_field_guard`（受保護 TOML／identity surface 寫入閘） | PreToolUse hook＋MCP 前門（約 10 處） | live | 2026-08-07 | 82 | CLAUDE.md v1.52 | 無 | 2,374 | 0.75/0.22/0.01/0.02 | .05 | .05 | .05 | **.55** | 4 | 4 | 4 | L1 | 保留：唯一擋 agent 自改組織欄位的閘／簡化：2,374 LOC＋82 測試對約 10 呼叫端，規則表過胖 |
| `agent-file-guard`（Rust 子命令 PreToolUse hook） | `duduclaw hook agent-file-guard`，`agent_hook_installer` 自動裝（8 處） | live | 2026-09-23 | 19 | CLAUDE.md | 無 | 790（installer） | 0.95/0.04/0/0.01 | .02 | .03 | .02 | .10 | 5 | 2 | 5 | L2 | 保留：SOUL.md 唯讀化的實際執行點、跨平台／淘汰：無 |
| `data-file-guard`（shell hook，RFC-23 §14.4） | `<agent_dir>/.claude/hooks/data-file-guard.sh`＋`DUDUCLAW_DATA_FILE_GUARD`（預設 `on`） | live，**僅 redaction 啟用時生效**；**Windows 無 bash 時靜默失效（模組自陳）** | 2026-09-23 | — | `docs/features/55-data-sources.md` | 應移植為 `duduclaw hook data-file-guard` | 130 | 0.70/0.28/0.01/0.01 | .05 | **.60** | .10 | .05 | 4 | 2 | 3 | L2 | 保留：唯一擋 Read/Bash 繞過 redaction 的路／取代：改寫成 Rust 子命令可同時修掉 Windows 失效 |
| **`.claude/hooks/` 三階段安全防禦** | 無（腳本已刪） | **removed** | `ba015a48` | 0 | `05-security-defense.md`（2026-04-15）＋CLAUDE.md＋overview 三語＋inventory 三語 | 已被 agent-file-guard／data-file-guard／`input_guard` 取代 | 0 | 0/0/0.05/**0.95** | **1.0** | **.95** | **1.0** | — | 0 | 2 | 1 | **L2** | 保留：無可保留（程式碼不存在）／淘汰：文件描述不存在的三層防禦，並留下懸空 `DUDUCLAW_BROWSER_VIA_BASH` |
| **Governance Layer（policies/*.yaml）** | `/manage/governance`、`governance.list/upsert/remove` | **CRUD-only，enforcer 已刪** | handler 2026-07-11；crate 刪於 `b0639b96` | 5（僅 YAML round-trip） | `docs/features/21-governance-layer.md` | `delegation_policy`＋`capability_grants`＋`budget.rs`＋`mcp_auth` scope 已各自覆蓋 rate/permission/quota | 187＋~300（handlers YAML helper）＋web page | 0.10/0.15/0.15/**0.60** | **.90** | **.85** | **.80** | .40 | 1 | 3 | 2 | **L2** | 保留：UI 已做好，補一個 evaluator 就能活／淘汰：操作者以為設了政策、實際零執行，是**比沒有更危險的假安全感** |
| **Durability Framework** | 無 | **removed** | `b0639b96` | 0 | `22-durability-framework.md`＋`feature-inventory.md`（2026-09-24 仍宣稱存在） | `duduclaw-security::circuit_breaker`、autopilot 三態斷路器、`dispatch_guard` 已各自覆蓋 | 0 | 0/0/0.05/**0.95** | **1.0** | **.90** | **1.0** | — | 0 | 1 | 1 | **L2** | 保留：無／淘汰：純文件幽靈 |
| **`gateway/activation.rs`** | 無（未宣告 mod） | **dead（從未編譯）** | 2026-04-02 | 8（跑不到） | 無 | `channel_reply` 內已有 mention 過濾 | 158 | 0.02/0.03/0.05/**0.90** | **.90** | .70 | **1.0** | .10 | 0 | 1 | 1 | **L2** | 保留：無／淘汰：孤兒檔，gateway 唯一一個 |
| `delegation_scope.rs` | 無 | **dead（零生產呼叫端）** | 2026-07-11 | 7 | 無 | `delegation_policy` | 192 | 0.03/0.05/0.05/**0.87** | .75 | .80 | **1.0** | .30 | 1 | 1 | 1 | **L2** | 保留：無／淘汰：`PermissionSnapshot`／`intersect`／`depth_within_limit` 三個公開符號全 repo 零外部引用 |
| `duduclaw-security` 五孤兒模組（見 §1.1） | 無 | **dead** | 2026-03-25 ~ 2026-07-06 | 有（跑得到但無生產意義） | 無 | — | 1,250 | 0.03/0.05/0.05/**0.87** | .85 | .50 | **1.0** | .30 | 1 | 2 | 1 | **L2** | 保留：無／淘汰：五個型別名 workspace-wide 零外部命中 |
| `duduclaw-security/src/mod.rs`＋`src/unicode_tests.rs` | 無 | **dead（未宣告，且與 `src/tests/unicode_tests.rs` 位元組完全相同）** | — | 189 | 無 | — | 190 | 0/0/0/**1.0** | 1.0 | 1.0 | **1.0** | — | 0 | 1 | 1 | **L2** | 保留：無／淘汰：`diff -q` 實錘重複檔，Rust 2018 下 `src/mod.rs` 本身就無意義 |
| `delegation.rs`（Envelope／Context） | 內部型別（`DelegationContext` 33 refs） | live | **2026-04-06** | 3 | — | — | 273 | 0.80/0.17/0.01/0.02 | .15 | .10 | .15 | .10 | 3 | 1 | 3 | L1 | 保留：delegation prompt 的資料結構／簡化：`TaskPayload` 零外部引用可刪 |
| `departments.rs` | `departments.*` RPC、`/manage/departments` | live | 2026-07-15 | 4 | — | — | 277 | 0.90/0.08/0.01/0.01 | .05 | .05 | .05 | .05 | 4 | 1 | 4 | L2 | 保留：delegation `department` policy 的資料來源／淘汰：無 |
| Preset（職務組合） | `duduclaw preset`、`agent create --preset`、`preset.toml` RPC、`/presets` | live（premium 9 部門包） | 2026-08-20 | 26 | — | 與 expert packs／premium templates 三套並存 | 2,015 | 0.65/0.20/0.12/0.03 | .10 | **.45** | .20 | .35 | 3 | 3 | 3 | L1 | 保留：解析物化到 `agent_resolved/` 防 agent 自改／取代：三套「預先配置好的 agent」機制職責重疊 |
| `team_gate.rs`（可分解性閘） | `team_composer` → goal loop | live（**未 commit**） | uncommitted 2026-09 | 11 | `docs/features/56-team-as-agent.md` | — | 862 | 0.80/0.15/0.02/0.03 | .05 | .10 | .10 | .30 | 4 | 3 | 3 | L1 | 保留：團隊即員工線核心閘／淘汰：未 commit、未活體驗證 |
| `spawn_env` allowlist＋`git_credentials` per-agent 授權 | `[capabilities] git_credentials`（預設 false） | live | 2026-09-23 | 13 | CLAUDE.md 第八/十波 | 無 | 753 | 0.95/0.04/0/0.01 | .02 | .02 | .02 | .15 | 5 | 2 | 5 | L2 | 保留：v1.61.0 CRITICAL 事故修法本體／淘汰：無 |

###### 1.1 `duduclaw-security` 模組別接線（外部呼叫端計數）

| 模組 | 外部呼叫端 | LOC | 判定 |
|---|---|---|---|
| `audit` | 253 | 1,742 | live（稽核主幹） |
| `input_guard` | 36 | 518 | live（注入掃描） |
| `secret_ref` | 28 | 996 | live（憑證單一化收官） |
| `perception` | 22 | 368 | live |
| `secret_manager`（env/local/file/keychain/vault/1password/infisical） | 18 | 1,921 | live |
| `crypto` 14／`safety_word` 13／`soul_guard` 10／`circuit_breaker` 8／`policy_kernel` 7／`action_claim_verifier` 7／`failsafe` 6／`stability_index` 6／`killswitch` 6／`rate_limiter` 6 | — | — | live |
| `audit_chain` 2／`keyfile` 2／`security_posture` 2／`soul_scanner` 1／`unicode_normalizer` 1 | — | — | live（薄） |
| **`filter_chain`** | **0** | 300（2026-04-04） | **dead**：`FilterChain`／`ThreatLevel` 零外部引用 |
| **`template_sanitizer`** | **0** | 324（2026-04-17） | **dead**：`TemplateSanitizer`／`sanitize_template` 零引用 |
| **`os_reconcile`（eslogger＋ebpf）** | **0** | 282（2026-07-06） | **dead**：`EsloggerObserver`／`parse_eslogger_line`／`EbpfObserver` 零引用 |
| **`credential_proxy`** | **0** | 173（2026-03-25） | **dead**：`CredentialProxy` 零引用 |
| **`mount_guard`** | **0** | 171（2026-03-25） | **dead**：`MountGuard` 零引用 |

> 死碼小計 **1,250 LOC**，全 L2。

---

##### 2. 去識別化（RFC-23 全族）

> **總開關**：`[redaction] enabled = false` 是預設（`redaction/config.rs:75`）。缺 `[redaction]` ⇒ `BootOutcome::Disabled` ⇒ 不建 `RedactionManager` ⇒ egress／NER／data-file-guard／mcp-proxy／ToolInterceptor **全部變 no-op**。整個子系統在頂層是 opt-in-default-off，即使個別路徑「live」。

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 核心規則引擎（engine／pipeline／regex／keyword／identity） | `RedactionManager::open`、`[redaction.rules.*]` | live（總開關下） | 2026-09-24 | 84 | `55-data-sources.md`、`commercial/docs/RFC-23` | 3,572 | 0.92/0.06/0.01/0.01 | .05 | .05 | .10 | .25 | 5 | 4 | 5 | L2 | 保留：法遵賣點的本體／淘汰：`identity` 規則「自 v1.14 宣告但從未編譯」直到 2026-09-23（commit `434f4aa9`）才接上，代表覆蓋率曾長期虛報 |
| JsonPath／db_field 欄位規則＋`data_source` 登錄（odoo／duduclaw_db／duduclaw_files 三內建） | `[redaction.data_sources.*]`、儀表板欄位規則卡 | live | 2026-09-24 | 75 | `55-data-sources.md` | 2,937 | 0.90/0.08/0.01/0.01 | .03 | .05 | .10 | .35 | 5 | 4 | 4 | L2 | 保留：v1.64.0 出貨的主力賣點／淘汰：登錄表「只描述形狀不描述語意」，打錯字只會靜默不比對，靠 dry-run 才看得到 |
| NER（OpenAI Privacy Filter，ort load-dynamic） | `redaction.model.install/status/remove` RPC、`type="ner"`／`ai_pii` profile | **standby**：cargo feature `ner` 預設 ON 且 gateway 硬依賴，但模型（~945MB）**永不自動下載**；未裝而列了 ner 規則會 **poison 整個 manager（fail-closed，mcp-server 拒啟動）** | 2026-09-24 | 81 | `55-data-sources.md` | 3,592 | 0.70/0.22/0.03/0.05 | .10 | .25 | **.45** | **.50** | 4 | 4 | 3 | L1 | 保留：「AI 智慧偵測」是 v1.65.0 的行銷主軸／淘汰：自陳 zh-TW 召回率約 80%／人名約 72%、1.1–1.7GB RAM、60–160ms/句、**Intel macOS 完全不存在**，三重 opt-in 後真實啟用率存疑 |
| 我的規則（dashboard custom rules＋profiles 匯入） | `redaction.custom_rules.*`、`redaction.suggest_pattern`、`redaction.profiles.import/remove` | live | 2026-09-24 | 79 | `55-data-sources.md` | 3,278 | 0.85/0.12/0.02/0.01 | .05 | .10 | .15 | .40 | 4 | 4 | 4 | L2 | 保留：讓非工程客戶自己加規則／簡化：`redaction_custom_rules.rs` 2,437 LOC＋46 測試僅服務一組 CRUD RPC |
| Vault（token↔原文加密對照，AES-256-GCM，TTL 168h） | 內部 | live but **crate 內部限定**（跨 crate 零呼叫端） | 2026-06-20 | 23 | — | 1,133 | 0.75/0.20/0.03/0.02 | .10 | .15 | .30 | .35 | 4 | 3 | 4 | L1 | 保留：可還原去識別化是對話場景的必要條件／淘汰：唯一可能的外部消費者（transcript 匯入）明文說它**刻意繞過** vault |
| `duduclaw mcp-proxy`（外部 MCP server 去識別化） | 隱藏 CLI 子命令，由 `.mcp.json` 改寫 spawn | live（4 處：`channel_reply` ×2、`claude_runner` ×2） | 2026-09-24 | 16 | `55-data-sources.md` | 928 | 0.88/0.10/0.01/0.01 | .05 | .05 | .10 | .30 | 4 | 3 | 4 | L2 | 保留：redaction 第一次伸出 DuDuClaw 自家 MCP 之外／淘汰：文件明列**四個缺口**（HTTP/SSE MCP、PTY session pool、codex/gemini/antigravity、地端推論 tool loop） |
| `ToolInterceptor`（`duduclaw-llm` in-process 等價物） | `RedactionToolInterceptor` | live（5 檔）；**`local_llm.rs` 的接線是未 commit 的新碼** | 2026-09-24 | — | `55-data-sources.md` | — | 0.88/0.10/0.01/0.01 | .05 | .05 | .10 | .25 | 4 | 3 | 4 | L1 | 保留：openai-compat tool loop 唯一的去識別化路／淘汰：文件仍把它列為未覆蓋缺口，工作樹已修但未 commit＝狀態不一致 |
| `secret_redact.rs`／`mcp_redact.rs`（兩個與 RFC-23 無關的遮罩工具） | 無 config gate，永遠開 | live（13＋2 呼叫端） | 2026-08-04／2026-05-01 | 21 | — | 784 | 0.85/0.13/0.01/0.01 | .05 | .15 | .05 | .10 | 4 | 2 | 4 | L2 | 保留：通道診斷／CLI log 的憑證遮罩，與總開關無關／淘汰：命名與 RFC-23 撞名，易被誤認為同一套 |

---

##### 3. 資料連接

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `duduclaw-db` 唯讀連接器（PG／MySQL／SQLite，三層唯讀防護） | MCP `db_sources`/`db_tables`/`db_select`/`db_query`、`Scope::DbRead`、`db_sources.*` RPC | live | 2026-09-24 | — | `55-data-sources.md` | 3,100 | 0.92/0.07/0/0.01 | .02 | .05 | .03 | .20 | 5 | 3 | 5 | L2 | 保留：企業資料整合門面、deny-by-default 授權／淘汰：無 |
| `db_sources` 授權三路（精靈／AI員工頁／聊天 `agent_update`） | 三入口 | live | 2026-09-24 | 30 | `55-data-sources.md` | 2,168 | 0.85/0.13/0.01/0.01 | .02 | .10 | .03 | .35 | 4 | 3 | 4 | L2 | 保留：免手改 agent.toml 是真實 UX 改善／簡化：三條入口做同一件事，有收斂空間 |
| 地端檔案 MCP（`file_read`／`csv_read`／`xlsx_read`） | 三個 MCP 工具＋`duduclaw_files` 內建 source | live | 2026-09-23 | 28 | `55-data-sources.md` | 1,214 | 0.88/0.10/0.01/0.01 | .03 | .05 | .05 | .20 | 4 | 2 | 4 | L2 | 保留：補上 Claude CLI 內建 Read/Bash 繞過 redaction 的洞／淘汰：無 |

---

##### 4. 自動化（autopilot／resident sensing／approval／task board／goal loop）

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Autopilot 規則引擎（事件匯流排＋三態斷路器） | `autopilot.*` RPC、MCP `autopilot_list`、`/routines` | live | 2026-09-01 | 45 | `docs/features/23-autopilot-engine.md` | 5,323 | 0.90/0.08/0.01/0.01 | .05 | .05 | .05 | .30 | 5 | 4 | 5 | L2 | 保留：事件驅動自動化主幹／淘汰：規則 DSL 對非技術使用者門檻高 |
| CEP 時序比對（`sequence` 規則） | autopilot rule `sequence` 欄位 | live（規則層 opt-in） | 2026-09-01 | 9 | `23-autopilot-engine.md` | 861 | 0.60/0.25/0.03/0.12 | .10 | .25 | **.45** | **.50** | 2 | 3 | 2 | L1 | 保留：「A 之後 N 秒內沒出現 B」無替代寫法／淘汰：需手寫 `{first,then,within_secs}` JSON，實際使用率存疑 |
| Resident sensing（`[tick]` http_poll／command／file_tail／websocket） | `[tick]`＋`[[tick.sources]]`（**預設 off**）、`ticks.*` RPC、唯讀卡 | opt-in 預設關 | 2026-09-23 | 89 | `docs/features/41-resident-sensing.md`（589 行） | 6,251 | 0.70/0.24/0.02/0.04 | .05 | .15 | **.35** | **.55** | 4 | 4 | 3 | L1 | 保留：五輪真 Kraken 活測修出的管線，是 belief loop 資料源／簡化：旋鈕（DNS TTL／idle watchdog／ping／baseline 壽命／round6／rate cap／persist_every_n）遠超一般使用者可理解 |
| Autopilot 地端模型預篩（`action.screen`） | autopilot rule `screen` 欄位 | opt-in，**fail-open** | 2026-08-11 | 23 | `41-resident-sensing.md` | 1,115 | 0.65/0.25/0.05/0.05 | .10 | .25 | **.40** | .35 | 3 | 3 | 2 | L1 | 保留：唯一能不上雲過濾雜訊的層／淘汰：預設 fail-open＝壞掉時等於沒裝 |
| ApprovalBroker（HITL 單一原語＋simulate-before-act） | `approvals.list/decide`、`/approvals`＋`/inbox`、MCP `capability_request`、通道按鈕 | live | 2026-09-23 | 67 | CLAUDE.md v1.33 | 4,451 | 0.95/0.04/0/0.01 | .02 | .03 | .02 | .35 | 5 | 4 | 5 | L2 | 保留：三套 ad-hoc 審批收斂成一個可稽核 store，是不可逆動作唯一人閘／淘汰：無 |
| Task Board（含 artifacts ledger／changes／iterations／role_turns／flow_metrics） | `tasks.*` RPC（20+）、MCP `tasks_*` 8 工具、`/tasks` | live | 2026-08-16 | 135 | `docs/features/24-task-board.md` | 9,580 | 0.85/0.13/0.01/0.01 | .05 | .05 | .03 | **.50** | 5 | 5 | 5 | L1 | 保留：agent-as-teammate 的落地面／簡化：`task_store.rs` 6,296 LOC 單檔遠超專案自訂 800 行上限 |
| Goal Loop 主幹（driver＋dispatch engine＋MAV 判官） | `/goal`、`tasks.goal_create`、`/goals`、`[dispatch] enabled`（**v1.59 起預設 ON**） | live | 2026-09-07 | 210 | `docs/features/34-goal-loop.md` | 13,107 | 0.90/0.09/0/0.01 | .03 | .03 | .02 | **.55** | 5 | 5 | 5 | L1 | 保留：「給目標→跑到完成→卡住找人」是產品核心敘事／簡化：`dispatch_engine.rs` 7,169 LOC／126 測試是最大維護熱點 |
| Goal Loop 週邊八小模組（`pause_reason` 42 refs／`goal_state` 33／`goal_budget_best_round` 6／`goal_gap_fingerprint` 5／`goal_visit_graph` 3／`goal_tool_streak` 3／`goal_bail_detect` **1**／`goal_plan`） | goal_loop／dispatch_engine 內部 | live | 2026-08-07~15 | 各 10–30 | `34-goal-loop.md` | 3,564 | 0.60/0.32/0.03/0.05 | .15 | .20 | .25 | **.65** | 3 | 4 | 3 | L1 | 保留：每個都是一次真實卡死事故的修法，刪了會回歸／簡化：八個獨立檔各帶旋鈕；`goal_bail_detect`（395 LOC／27 測試）只有 1 處呼叫端且只產生 advisory |
| Judge seam（`[dispatch] judge` = mav／evaluator_only／external／human_only） | `[dispatch] judge`、設定→自動化下拉 | live（預設 `mav`，逐位同舊） | 2026-09-01 | 24 | `34-goal-loop.md` | 1,357 | 0.55/0.30/0.05/0.10 | .10 | .25 | **.50** | **.55** | 3 | 3 | 2 | L1 | 保留：「一切皆插件」第一個真 seam，`external` 讓客戶接自家驗收／淘汰：四模式中三個是降級品，`judge_command` 刻意不開放 RPC＝實際沒人用 |
| Spawn admission queue（有界 FIFO） | `[dispatch] admission`（預設 `queue`） | live | 2026-08-15 | 23 | CLAUDE.md 第二梯 | 1,231 | 0.75/0.20/0.02/0.03 | .05 | .10 | .15 | **.50** | 3 | 3 | 3 | L1 | 保留：超限從硬拒絕改排隊是真 UX 改善／簡化：1,231 LOC／23 測試對一個 FIFO 佇列偏重 |
| `dispatch_guard`（跨行程滑動視窗斷路器） | `[dispatch_guard]`、`dispatch_guard.json` | live（4 處） | 2026-07-18 | 11 | `34-goal-loop.md` | 520 | 0.85/0.12/0.02/0.01 | .05 | .10 | .10 | .20 | 4 | 2 | 4 | L2 | 保留：runaway 防線／淘汰：無 |
| Ephemeral agent spawn | MCP `spawn_agent`、`DUDUCLAW_HOP_DEPTH` | live（74 處） | 2026-08-15 | 51 | — | 3,457 | 0.90/0.08/0.01/0.01 | .03 | .05 | .03 | .40 | 5 | 4 | 5 | L1 | 保留：sub-agent 編排執行面／簡化：3,457 LOC 單檔 |
| Activity Feed | `activity.list/subscribe`、MCP `activity_list/post`、`/timeline` | live | — | — | `24-task-board.md` | — | 0.92/0.07/0/0.01 | .02 | .05 | .03 | .10 | 4 | 2 | 4 | L1 | 保留：即時可觀測唯一使用者可見面／淘汰：無 |
| Foresight 頁（信念／校準三態） | `/foresight`、`belief.*` RPC | live | 2026-08-11 | 20 | `docs/features/46-belief-loop.md` | 989 | 0.70/0.22/0.03/0.05 | .10 | .10 | **.30** | .40 | 3 | 3 | 3 | L1 | 保留：誠實三態校準是難得的反 AI 吹牛設計／淘汰：僅 3 處呼叫端，需 ≥30 筆結算才有意義 |

---

##### 5. 企業整合

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Odoo ERP bridge 主幹（**17 MCP 工具**＋`OdooConnectorPool` per-agent 憑證隔離＋EditionGate＋`Scope::OdooRead/Write/Execute`） | `odoo.*` RPC（10 方法）、`OdooPage`（在 `/manage/integrations` 的 odoo 分頁）、17 MCP 工具 | live | 2026-08-15／pool 2026-08-16 | 50（其中 4 個 live 測試 `#[ignore]`，需 `ODOO_LIVE_*` env） | `docs/features/12-industry-templates.md`、`docs/rfc/RFC-21-operator-guide.md`、`erp-support-matrix.md` | 2,835 | 0.80/0.15/0.03/0.02 | .10 | .10 | .20 | .35 | 4 | 3 | 4 | L1 | 保留：台灣中小企業 ERP 是真實需求、三元 scope＋`allowed_models`／`allowed_actions` 防護完整／淘汰：17 個工具對單一廠商耦合極深，2026-08 後未動 |
| **Odoo 事件同步（輪詢＋webhook）** | `OdooPage` 的 `pollEnabled` 開關（**預設 true**）／`poll_models`／`webhook_secret`，寫進 `config.toml` | **dead**：`PollTracker`／`poll_model`／`parse_webhook` 零生產呼叫端；gateway 無 `/webhook/odoo` 路由；無背景輪詢 task；無 `duduclaw odoo` CLI | 2026-08-15 | 7（僅單元測試） | `12-industry-templates.md` 的「Event Synchronization」節寫成現役 | 392 | 0.15/0.15/0.10/**0.60** | **.80** | .30 | **.95** | .25 | 2 | 2 | 2 | **L2** | 保留：事件驅動的 ERP 同步是自動化敘事的關鍵拼圖／淘汰：**UI 開關預設開、設定被存、什麼都不發生**——比沒有這個開關更糟 |
| Identity Resolution（`duduclaw-identity`） | MCP `identity_resolve`、`Scope::IdentityRead`、`channel_reply::build_sender_block` `<sender>` 注入、`identity.config_get/set` RPC | **WikiCache live；Notion／Chained 只在儀表板測試 RPC 可達**（兩個生產點硬編 WikiCache，程式碼註解自陳「Step 2 of the migration plan」） | 2026-09-23 | 21（`chained.rs` **0**） | `docs/features/25-identity-resolution.md`（敘述與程式碼不符）、RFC-21 | 1,736 | 0.55/**0.30**/0.10/0.05 | **.35** | .20 | **.40** | .35 | 3 | 2 | 3 | **L2** | 保留：讓 SOUL.md「拒絕非專案成員」從模糊推理變成可評估資料，WikiCache 這條真的在跑／簡化：三個 provider 只有一個上線、`ChainedProvider` 321 LOC 零測試、RFC-21 承諾的另兩個 MCP 工具從未實作 |
| Google Workspace（native REST，**19 MCP 工具**，涵蓋 Gmail／Calendar／Sheets／Forms／Tasks／Drive／Docs／Slides）＋Apps Script 橋＋service account DWD | 三條憑證路；`GoogleIntegrationPage` | **opt-in 預設關**：`[integrations] google_workspace` 預設 `false` fail-closed（待 Google OAuth app 驗證）。**前端 `GOOGLE_INTEGRATION_ENABLED` 硬編 `true`**，所以分頁看得到、但後端工具仍 403 | 2026-08-03~15 | 70 | `google-workspace.md`／`google-no-oauth-client.md`／`google-mcp.md`（三語） | 4,201 | 0.78/0.18/0.02/0.02 | .10 | .15 | **.35** | .30 | 4 | 4 | 4 | L1 | 保留：免自建 OAuth client 的三條路是實測驗證過的、19 工具覆蓋面最廣／淘汰：**前後端閘門不一致**（UI 開、後端關）＝使用者看得到卻用不了；Google 政策變動風險高 |
| `notion_workspace.rs`（native Notion REST，4 工具；與 `duduclaw-identity` 是兩回事） | MCP `notion_status`/`notion_search`/`notion_page_read`/`notion_page_append`，經 `mcp_oauth` vault | live（無專屬 RPC／頁面，已併入通用「工具伺服器」分頁） | 2026-07-26 | 11 | `docs/guides/notion.md`（三語） | 709 | 0.75/0.20/0.03/0.02 | .10 | **.35** | .20 | .15 | 3 | 2 | 3 | L1 | 保留：Notion 是台灣團隊常用知識庫／取代：Notion 官方有 remote MCP，`mcp_external` bridge 可直接掛 |
| `github_workspace.rs`（5 工具） | MCP `github_status`/`github_search_issues`/`github_issue_read`/`github_pr_read`/`github_issue_comment` | live（**無 `integration_enabled` 閘**，與 Google 不同：vault 有 token 即視為可用） | 2026-07-26 | 6 | `docs/guides/github.md`（三語） | 763 | 0.70/0.22/0.05/0.03 | .10 | **.40** | .20 | .15 | 3 | 2 | 3 | L1 | 保留：開發者客群剛需／取代：GitHub 官方 MCP server 成熟，`mcp_external` 可直接掛；且它缺 Google 那種預設關閘門 |
| `duduclaw-docuseal-mcp`（簽署工作流，10 工具） | 獨立 binary（workspace member） | live 但**不隨 release 出貨**（`scripts/release.sh` 零命中 `docuseal`），需客戶自行 `cargo build` | **2026-07-30（單一 commit，此後未動）** | 16 | `docs/guides/docuseal.md`（三語） | 727 | 0.45/0.20/0.10/**0.25** | **.40** | **.55** | **.55** | .15 | 2 | 1 | 2 | **L1** | 保留：電子簽署是辦公協作定位的自然延伸、程式碼小／淘汰：兩個月零變更、不出貨、workspace 零引用、DocuSeal 官方自己就有 MCP |
| Premium 產業包（**22 包** `<industry>-pro/`，`commercial/templates-premium/`，gitignored） | `templates.*` RPC（`WelcomePage` 首登精靈＋`CreateAgentPage`）、`premium_templates` feature gate；`install_eval_suite` 於 `handlers.rs:9726` 真有生產呼叫 | live（付費層） | 2026-08-15 | 20 | `docs/features/12-industry-templates.md`（免費層敘述）；**premium 本身無專屬 `docs/features` 條目** | 1,390 | 0.88/0.10/0.01/0.01 | .05 | .10 | .10 | .25 | 5 | 3 | 4 | L1 | 保留：五柱護城河之一、鎖 quota 不鎖能力的正確示範／淘汰：內容維護（法規勘誤）成本隨包數線性成長 |
| Expert packs（`expert/*`＋9 個 `experts.*` RPC＋`/experts`＋`expert_admin`／`expert_generate`） | `duduclaw expert install\|pack\|list\|remove\|export\|publish` | live；**24 個內建**＝22 個 team（由 `templates-premium/teams/*-team/team.toml` 自動衍生）＋2 個 standalone expert（`cad-drafter`／`marketing-designer`） | 2026-08-15 | 79 | `docs/features/32-expert-packs.md`、`build-your-own-pack.md`（三語） | 8,387 | 0.75/0.20/0.03/0.02 | .10 | **.40** | .10 | .40 | 4 | 4 | 4 | L1 | 保留：「一個 zip 一整個 AI 團隊」＋`safe_zip` 安全管線／取代：與 premium templates／preset 三套機制職責重疊。**另有一個真 bug：`experts/pharmacy-pro/` 因 manifest name（`pharmacy-assistant`）與目錄 slug 不符被靜默排除在目錄外** |
| `/gallery` 靈感畫廊 | `gallery.list` RPC（admin-only），由 `expert_generate::gallery_cards()` **即時生成**（非靜態檔） | live（newIn 1.60.0）；因無 `team.toml` 設 `examples`，退回取每隊前 3 個 worker `summary` ⇒ **約 66 張卡** | 2026-08-15 | — | **無專屬文件**（僅 zh-TW feature-inventory 一句帶過） | — | 0.75/0.20/0.03/0.02 | .10 | .15 | .25 | .15 | 3 | 1 | 3 | L1 | 保留：22 組產業劇本一鍵預填交辦，降低冷啟動／淘汰：admin-gated、卡片內容其實是 worker summary 的 fallback 而非策劃過的範例 |
| `templates/` 倉庫根目錄八個子樹 | 見右 | `evaluator`（`include_str!` ×5）／`manufacturing`＋`restaurant`＋`trading`（wizard `FREE_INDUSTRIES`）／`presets/system-operator`（`preset.rs` `include_str!`）／`redteam`（`duduclaw ops redteam`）／`wiki`（`memory/wiki.rs` `include_str!`）皆 **live**；**`orchestrator/` 零程式引用（純手動 `cp -r`）**；`KILLSWITCH.toml` **未 `include_str!`**，需操作者手動複製到 `~/.duduclaw/` | — | — | — | — | 0.70/0.20/0.03/0.07 | .20 | .15 | **.40** | .10 | 3 | 1 | 2 | **L2** | 保留：六個子樹真的被編進 binary／淘汰：`orchestrator/` 與 `KILLSWITCH.toml` 是「文件式模板」，與其他六個的接線方式不一致，容易誤以為會自動生效 |

---

##### 6. 授權／商業化／白牌

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `license_runtime.rs`（真正的執行期樞紐） | 開機單例＋`license.status/activate/redeem` RPC | live（~20 處呼叫端） | 2026-07-23 | 28 | 1,703 | 0.92/0.07/0/0.01 | .02 | .03 | .02 | .30 | 5 | 4 | 5 | L2 | 保留：agent/channel/memory quota 的唯一強制點、phone-home 與 CRL 都 fail-open／淘汰：無 |
| `duduclaw-license` crate（v2 簽章，v1 已於 2026-07-09 正式退役且刻意不再入 registry） | library | live | 2026-07-18 | 106 | 2,944 | 0.90/0.08/0.01/0.01 | .05 | .05 | .05 | .30 | 5 | 3 | 5 | L2 | 保留：五柱護城河的密碼學基礎／淘汰：無 |
| **Feature flag 實際生效範圍** | `features.toml` | **只有 3 個 boolean 真的有 `check_feature()` 呼叫端**：`premium_templates`／`white_label`／`industry_evolution_params`。其餘 8 個（`dashboard_enterprise`／`priority_security_patch`／`private_discord_support`／`odoo_integration_supported`／`redistribution`／`dedicated_engineer`／`cloud_only`／`self_host_only`）**純顯示，無行為** | — | — | — | 0.30/**0.55**/0.10/0.05 | .30 | .35 | **.70** | .40 | 2 | 2 | 2 | **L2** | 保留：`duduclaw license status` 的分層說明有行銷價值／簡化：8 個假旗標讓人誤以為有實際 gating |
| **死 quota 欄位** | `features.toml`／`gate.rs` | `max_local_models`／`max_messages_per_month`／`office_hour_hours_per_month` **零讀取端**（RFC-27 自己已點名 `max_messages_per_month` "zero readers"） | — | — | — | 0.10/0.15/0.05/**0.70** | **.80** | .40 | **1.0** | .20 | 1 | 1 | 1 | **L2** | 保留：無／淘汰：三個從未被讀的計價欄位 |
| Distributor／白牌（9 RPC＋`/manage/distributors`，nav 有，`enterprise:true, minRole:admin`） | `distributor.*` | live | 2026-07-18 | 21 | 2,414 | 0.85/0.12/0.02/0.01 | .05 | .05 | .10 | .30 | 4 | 3 | 4 | L2 | 保留：OEM 簽章散發包自動套用是已驗證能力／淘汰：無 |
| `branding.rs`（白牌產品名替換） | `branding.*` RPC | live（5 個通道 adapter 都用 `effective_product_name`） | 2026-07-13 | 40 | 1,755 | 0.90/0.08/0.01/0.01 | .03 | .05 | .03 | .25 | 4 | 3 | 4 | L2 | 保留：白牌的實際兌現面／淘汰：無 |
| Partner Portal（雲端夥伴） | `partner.*`（7 RPC）；**無獨立導覽項**，掛在 `/app/system/license?tab=partner`；裸 `/partner` 路由 redirect | live 但**低可發現性** | store **2026-04-20** / page 2026-08-13 | 6 | 1,506 | 0.50/0.25/0.05/0.20 | .30 | .20 | **.45** | .25 | 2 | 2 | 2 | **L1** | 保留：雲端經銷分潤是既定商業路線／淘汰：store 五個月零變更、藏在授權頁分頁裡、真正的控制面在 gitignored `commercial/cloud-control-plane` |
| `license_serve.rs`（子簽發者 HTTP） | `/v1/license/refresh`／`/crl`／`/branding/sign` | live 但**非白牌簽發者時全部 404**（opt-in-default-off） | 2026-09-01 | 7 | 652 | 0.80/0.15/0.03/0.02 | .10 | .05 | **.40** | .25 | 3 | 2 | 3 | L1 | 保留：owner gateway 當子簽發者的機制／淘汰：一般單租戶安裝完全用不到 |
| **`credit.rs`（LINE OA B2C 點數）** | CLI `duduclaw credit grant/balance/history` | **半死**：`CreditLedger` 全 repo 唯一呼叫端是 `cli/lib.rs:9583/9585`；模組 doc 自陳的「fail-closed gate（balance ≤ 0 ⇒ 呼叫 LLM 前拒絕）」**在回覆路徑零呼叫端**；無 RPC、無 UI、無通道整合 | 2026-07-11 | 2 | 228 | 0.20/0.25/0.05/**0.50** | **.60** | .30 | **.85** | .20 | 1 | 1 | 2 | **L2** | 保留：LINE OA 轉售計價是已規劃商業模式／淘汰：計量的一半（扣點與阻擋）從未接線，等於一個手動記帳本 |
| `duduclaw license` CLI（10 子命令） | 頂層 `duduclaw license` | live | 2026-07-12 | **0** | 718 | 0.70/0.25/0.03/0.02 | .15 | .10 | .15 | .15 | 3 | 2 | 3 | L2 | 保留：離線啟用／匯出匯入的操作面／淘汰：718 LOC **零單元測試**，且 `refresh` 的「control-plane 未完成」註解已過時 |
| `duduclaw-auth`（OTP／JWT／ACL 多人帳號） | 儀表板登入 | live（20+ 處） | 2026-08-20 | 37 | 2,383 | 0.92/0.07/0/0.01 | .02 | .05 | .02 | .25 | 5 | 3 | 5 | L2 | 保留：與 license 正交——它管「誰能登入、什麼角色」／淘汰：無 |
| `duduclaw auth device`（Copilot／Qwen 席次） | CLI `auth device --provider` | live；**Copilot 已活測、Qwen 標 PENDING-LIVE（Qwen 2026-04-15 停掉免費 OAuth，端點無法驗證）** | 2026-07-12 | 15 | 852 | 0.55/0.28/0.12/0.05 | **.40** | .20 | .35 | .20 | 3 | 2 | 3 | **L1** | 保留：訂閱席次廣度對標 Hermes 是護城河／淘汰：兩 provider 中一個的上游已消失，兩個半月未動 |

---

##### 7. 可觀測性

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `otel.rs`（OpenTelemetry GenAI spans） | cargo feature `otel`（**預設 OFF**）、`[telemetry] otlp_endpoint` | opt-in 預設關；**`--features otel` 在 Cargo.toml／otel.rs／文件以外零命中，CI 未建** | 2026-07-05 | 15 | `docs/guides/observability.md`（三語） | 883 | 0.60/0.25/0.10/0.05 | .15 | .25 | **.55** | .30 | 3 | 2 | 3 | **L1** | 保留：企業客戶要 Langfuse/Grafana 時是現成答案、關閉時零成本／淘汰：從未被任何 build 開啟，`gen_ai.*` span 呼叫永遠是 no-op |
| `metrics.rs`（Prometheus，44 個 metric family） | `GET /metrics`（localhost-only） | live，**但有一串死序列** | 2026-08-20 | 29 | 無專屬文件 | 1,536 | 0.70/**0.26**/0.02/0.02 | .10 | .10 | .25 | .35 | 4 | 3 | 4 | **L2** | 保留：PTY pool／tick／goal-intent／relay 等新計數器都在活躍遞增／簡化：**6 個最舊的序列（`duduclaw_requests_total`、`duduclaw_tokens_total`、`duduclaw_request_duration_seconds`、`duduclaw_active_sessions`、`duduclaw_channel_connected`、`duduclaw_budget_remaining_cents`）永遠渲染但從未被生產程式碼遞增**——`record_request()` 只有自己的測試在呼叫，`update_budgets()` 連測試都沒有 |
| Evolution Events（JSONL→SQLite index→2 RPC＋1 HTTP） | `audit.evolution_query`／`audit.reliability_summary`／`GET /api/reliability/summary` | live | 2026-08-15 | 121 | `docs/features/29-evolution-events.md`（含資料流圖，三語） | 5,296 | 0.85/0.12/0.02/0.01 | .05 | .05 | .05 | .40 | 4 | 4 | 4 | L2 | 保留：單一真相來源→索引→三條存取路→一個頁面，是整個領域接線最乾淨的一塊／簡化：schema 1,103 LOC／30+ 事件型別，實際 emit 端只有 7 個檔 |
| `ReliabilityPage`（`/manage/reliability`） | 在導覽（`minRole:'admin'`、`personalHidden:true`） | live | 2026-08-13 | 2 | `29-evolution-events.md` | 566 | 0.85/0.12/0.02/0.01 | .05 | .05 | .10 | .15 | 4 | 2 | 4 | L2 | 保留：evolution events 的唯一可視化出口／淘汰：無 |
| `channel_failures.jsonl` | `audit.unified_log` RPC | live（5 個寫入端、4 個讀取端，含 native GUI） | — | — | `07-account-rotation.md`、`27-pty-pool-runtime.md`、`40-notification-governance.md` | — | 0.92/0.07/0/0.01 | .02 | .05 | .03 | .10 | 5 | 2 | 5 | L2 | 保留：「已讀不回」事故的唯一 post-mortem 來源／淘汰：無 |
| `notify_stats.rs`＋`notify_governance.rs`（通知治理四級階梯） | `notify.stats`、安靜時段、每日摘要 | live（6＋56 處） | 2026-08-11 | 15 | `docs/features/40-notification-governance.md` | 590＋— | 0.90/0.08/0.01/0.01 | .03 | .05 | .05 | .30 | 4 | 3 | 4 | L2 | 保留：「行動率 <50% 即判定該類通知壞掉」是罕見的自我否證設計／淘汰：無 |
| `fault_attribution.rs`（模型／評分者／環境／harness 歸因） | `[evolution] fault_attribution`（預設 true） | live（3 處）但**檔案未 commit（untracked）** | uncommitted | 14 | `docs/guides/evolution-switches.md` | 895 | 0.80/0.15/0.03/0.02 | .05 | .10 | .10 | .35 | 4 | 3 | 3 | L1 | 保留：把「模型錯」和「環境錯」分開是誠實回報的前提／淘汰：已有 3 個生產呼叫端卻從未進 git，狀態不一致 |
| `mast.rs`（MAST 多 agent 失效分類） | `eval/mod.rs`、`trajectory_guard.rs`、`channel_reply.rs` | live（3 處） | 2026-07-22 | 8 | **無任何 feature/guide 文件** | 559 | 0.70/0.22/0.05/0.03 | .10 | .20 | .20 | .30 | 3 | 2 | 3 | L1 | 保留：FM-3.3 等分類直接餵給 grounding／judge／淘汰：零使用者可見面、零文件 |
| **`cost_anomaly.rs`（燒錢速率異常偵測）** | 無 | **dead**：`detect()`／`AnomalyVerdict` 只有自己的 `#[cfg(test)]` 引用；`budget.rs`／notify 路徑皆無呼叫 | 2026-07-11 | 5 | `feature-inventory.md:105`（一行提及） | 133 | 0.10/0.10/0.15/**0.65** | **.70** | .45 | **1.0** | .15 | 1 | 1 | 1 | **L2** | 保留：成本失控告警是真需求、133 LOC 接上很便宜／淘汰：模組 doc 自稱「routed to logs / the notify path」是**願望不是事實** |
| `cost_telemetry.rs` | MCP `cost_summary/agents/recent`、`duduclaw cost` | live（27 檔） | 2026-07-26 | 39 | — | 2,718 | 0.92/0.07/0/0.01 | .02 | .03 | .03 | .30 | 5 | 4 | 5 | L2 | 保留：快取效率／200K 價格懸崖是真實省錢機制／淘汰：無 |
| `audit_export.rs`（SIEM NDJSON/JSON＋webhook） | CLI `duduclaw audit export` | live（**僅 CLI，無 RPC/UI**） | 2026-07-11 | 6 | 無 | 357 | 0.60/0.25/0.10/0.05 | .15 | .20 | **.45** | .15 | 3 | 1 | 2 | L1 | 保留：企業要 SIEM 匯出時是現成答案／淘汰：單一 CLI 呼叫端、儀表板完全沒接、兩個半月未動 |
| `secaudit`（掃描器→AI 深審→對抗覆核） | CLI `maintenance secaudit`、`secaudit.*` RPC、`/manage/secaudit` | live | 2026-09-01 | — | `docs/features/49-code-security-audit.md` | 6,760 | 0.80/0.15/0.02/0.03 | .05 | .20 | .10 | **.50** | 4 | 4 | 4 | L1 | 保留：三段式（scanner→AI→adversarial）有差異化、可對外賣／簡化：6,067 LOC 主要是 gitleaks／semgrep／cargo-audit 三個外部工具的包裝 |

---

##### 8. MCP 傳輸／ACP／A2A

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| stdio MCP server（**245 個工具**） | `duduclaw mcp-server` | live | 2026-09-24 | 224 | 多份 guides | 32,570（單檔） | 0.88/0.11/0/0.01 | .02 | .03 | .02 | **.60** | 5 | 5 | 5 | L1 | 保留：整個平台的工具面／簡化：`mcp.rs` **32,570 LOC 單檔**、245 個工具 schema 是 minimal_context 之後最大的固定 token 成本 |
| HTTP/SSE 傳輸（`duduclaw http-server`） | `POST /mcp/v1/call`、`GET /mcp/v1/stream`、`POST /mcp/v1/stream/call`、`/healthz` | live | 2026-08-13 | 33 | `docs/features/26-mcp-http-sse.md`（三語） | 1,528 | 0.85/0.12/0.02/0.01 | .05 | .10 | .15 | .30 | 4 | 3 | 4 | L2 | 保留：外部 HTTP 客戶端唯一入口／淘汰：無 |
| Streamable-HTTP 傳輸（MCP spec 2025-06-18） | `/mcp`（掛在 http-server 內，無獨立 CLI 旗標） | live | 2026-08-13 | **2** | `docs/guides/remote-mcp.md` | 378 | 0.75/0.20/0.03/0.02 | .10 | .15 | .25 | .20 | 4 | 2 | 3 | L1 | 保留：新版 MCP 規格的標準傳輸，不跟上會被生態淘汰／淘汰：只有 2 個測試 |
| Remote MCP OAuth 2.1（6 條路由＋DCR） | `/oauth/*`＋`.well-known/oauth-*` | live | 2026-08-13 | 5 | `docs/guides/remote-mcp.md` | 805 | 0.80/0.16/0.02/0.02 | .10 | .10 | .25 | .35 | 4 | 3 | 3 | L1 | 保留：Claude.ai 等遠端客戶端接入的必要條件／淘汰：805 LOC／5 測試，OAuth 是高風險低測試覆蓋 |
| MCP 認證與治理層（`mcp_auth` 2,113／`mcp_auth_strategy` 679／`mcp_capability` 341／`mcp_headers` 693／`mcp_rate_limit` 325／`mcp_http_errors` 174／`mcp_refresh` 453／`mcp_namespace` 192／`mcp_internal_key` 473） | 各自 | **全部 live，無一零呼叫端** | 2026-05~09 | 154 | ADR-002 等 | 5,443 | 0.82/0.15/0.02/0.01 | .05 | .10 | .05 | .40 | 4 | 4 | 4 | L2 | 保留：23 個 scope＋per-agent grant＋mtime 感知重驗是安全骨幹／簡化：九個小模組的分層對一個 API key 驗證偏厚 |
| ACP（編輯器協定，`duduclaw acp`） | Zed／JetBrains／nvim | live | 2026-08-13 | 24（共用 tests.rs） | `docs/features/19-agent-client-protocol.md`（三語） | 3,371 | 0.75/0.20/0.03/0.02 | .10 | .15 | **.35** | .30 | 3 | 3 | 3 | L1 | 保留：IDE 接入是開發者客群入口／淘汰：`handlers`／`message_send`／`server`／`types` 四檔零 inline 測試 |
| A2A（`duduclaw acp-server`＋agent card） | stdio JSON-RPC、`/.well-known/agent-card.json`（**由 gateway `server.rs:2610` 提供，非 CLI**） | live；**`[acp] trusted` 預設 `false` fail-closed（已驗證）** | 2026-08-13 | — | `19-agent-client-protocol.md` | — | 0.70/**0.25**/0.03/0.02 | .10 | .15 | .30 | .30 | 3 | 3 | 3 | **L2** | 保留：`message/send` 真的寫進 `bus_queue.jsonl`、誠實回 `submitted` 不假裝 `completed`／簡化：**agent card 有兩份平行實作**——CLI 的 `WELL_KNOWN_AGENT_CARD_PATH` 標 `#[allow(dead_code)]` 只有測試在用，真正上線的是 gateway 內另一份 inline 卡（依賴方向所致，已文件化但仍是技術債） |
| `mcp_external.rs`（第三方 MCP bridge） | `agent.toml [[mcp.external]]` | live | 2026-08-15 | 26 | `docs/guides/mcp-bridge.md` | 902 | 0.88/0.10/0.01/0.01 | .03 | .05 | .05 | .25 | 4 | 3 | 4 | L2 | 保留：接任意 SaaS 的通用口／淘汰：無 |
| `mcp_oauth.rs`（對外部 provider 的 OAuth 2.1+PKCE） | Google／GitHub／Slack／custom | live（6 檔） | 2026-08-12 | 15 | `google-mcp.md`／`mcp-bridge.md` | 1,034 | 0.88/0.10/0.01/0.01 | .03 | .05 | .05 | .25 | 4 | 3 | 4 | L2 | 保留：免客戶自建 OAuth client／淘汰：無 |
| `mcp_scan.rs`（寫入 `.mcp.json` 前的靜態安全掃描） | `handlers.rs` 多處＋`install_notify` | live | 2026-07-15 | 14 | **無文件** | 357 | 0.85/0.12/0.02/0.01 | .05 | .05 | .05 | .15 | 4 | 2 | 4 | L2 | 保留：擋 command injection／download-and-execute 的安裝前閘／淘汰：無文件，使用者不知道有這層保護 |

---

##### 9. 沙箱／文件產物／遷移／相容層

| 功能 | 入口 | 接線狀態 | 最後變更 | 測試 | 文件 | 規模 | verdict | obs | rep | unu | ove | val | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Container sandbox（Docker／Apple Container／WSL2） | `[container] sandbox_enabled`（**預設 false**） | opt-in；**全 repo 只有 `templates/evaluator/agent.toml` 一個開啟** | 2026-09-07 | — | `docs/guides/docker.md` | 1,537 | 0.70/0.22/0.03/0.05 | .10 | .20 | **.50** | .35 | 3 | 3 | 3 | L1 | 保留：`--network=none`＋tmpfs＋read-only rootfs 是唯一真隔離／淘汰：預設關、shipped template 只有 1 個用 |
| `duduclaw-sandbox`（OS 原生 confinement） | `condition_eval`、`runtime/mod.rs`（僅 2 處） | live 但呼叫端極少 | 2026-07-06 | — | — | 960 | 0.60/0.30/0.05/0.05 | .15 | .25 | **.40** | .30 | 3 | 2 | 3 | L1 | 保留：跨平台 confinement 抽象／淘汰：960 LOC 只服務 2 個呼叫端 |
| PTC 腳本沙箱（MCP `execute_program`） | MCP 工具 | live | 2026-07-04 | — | — | 615 | 0.85/0.12/0.02/0.01 | .05 | .10 | .10 | .15 | 4 | 2 | 4 | L2 | 保留：API-mode agent 無 Bash 時唯一執行路徑／淘汰：無 |
| Office 文件產出（MCP `office_script`＋`📎DELIVER:` 協定） | MCP 工具 | live | 2026-08-15 | 18 | `docs/features/31-office-document-suite.md` | 1,270 | 0.90/0.09/0/0.01 | .02 | .05 | .03 | .15 | 5 | 2 | 5 | L2 | 保留：「交給你一個真的 .docx」是辦公協作定位核心／淘汰：無 |
| `artifact_gate`（交付前結構檢查） | `[office] delivery_gate`（預設 true） | live | 2026-09-01 | 15 | `31-office-document-suite.md` | 579 | 0.88/0.10/0.01/0.01 | .02 | .05 | .05 | .20 | 4 | 2 | 4 | L2 | 保留：零位元組／magic 不符的假交付直接被擋／淘汰：無 |
| `document_limits`（解壓炸彈／XML 深度） | `[limits]`（`0`＝預設非無上限） | live（4 處） | 2026-08-15 | 16 | — | 877 | 0.90/0.08/0.01/0.01 | .02 | .05 | .05 | .25 | 4 | 2 | 4 | L2 | 保留：守 soffice／office_script／expert 解壓三個下游解析器／淘汰：無 |
| **Git worktree L0 隔離** | `[container] worktree_enabled`（**預設 false**）；唯一呼叫端 `dispatcher.rs:1192` | opt-in；**零個 shipped template 啟用** | **2026-06-22** | 11 | `docs/features/18-worktree-isolation.md`（2026-04-21） | 1,151 | 0.35/0.25/0.05/**0.35** | .45 | **.55** | **.70** | .40 | 2 | 2 | 2 | **L1** | 保留：比容器輕的並行隔離、dispatcher 已接好／淘汰：預設關、零 template 啟用、三個月沒人動，且 goal loop 主線走 `message_queue` 不走 dispatcher worktree 路徑 |
| App compat 層（`duduclaw ops compat list` / `windows-vm`） | CLI | live 但 **runner 只「回報就緒」不執行**（模組自陳整合是後續波次） | 2026-08-30／2026-09-07 | 37 | `docs/guides/app-compat.md` | 2,334 | 0.45/0.20/0.05/0.30 | .35 | .20 | **.60** | .45 | 2 | 3 | 2 | **L1** | 保留：DuDuClaw OS 值班機的 Windows 應用敘事／淘汰：屬已拆出的 OS 線、`discover_runners` 只掃不執行、Windows VM 真機輪從未驗證 |
| `duduclaw export --format agentcompanies` | CLI `export` | live | 2026-08-15 | 13 | 無 | 1,328 | 0.30/0.20/0.10/**0.40** | **.65** | .25 | **.55** | .30 | 1 | 2 | 1 | **L1** | 保留：對 paperclip 生態的互通匯出／淘汰：paperclip 路線 2026-07 已轉辦公協作、再轉 Agent-Native OS，這個 v1-draft 單向匯出沒有已知消費者 |
| `duduclaw data-migrate`（`/data` 前向遷移） | CLI＋systemd unit | live（值班機專用） | 2026-08-24 | **0** | 無 | 197 | 0.70/0.15/0.05/0.10 | .25 | .10 | .35 | .10 | 3 | 1 | 3 | L1 | 保留：A/B root 回滾無法還原的格式變更只有它能補／淘汰：屬已拆出的 OS 線、零測試 |
| `duduclaw migrate-from claude-code`（含逐字稿） | CLI | live；**`--apply` 未完成（設計文件自陳）** | 2026-08-16 | — | `commercial/docs/DESIGN-runtime-state-sync-2026-08.md` | 1,597 | 0.78/0.18/0.02/0.02 | .10 | .10 | .30 | .25 | 4 | 3 | 3 | L1 | 保留：從 Claude Code 搬家是最強獲客入口／淘汰：dry-run 實測有效訊號僅約 1.5%、`--apply` 尚未做 |

---

##### 10. 候選清單（通過門檻：verdict argmax ≠ 保留 且 ≥0.5，或任一 noul p ≥ 0.6 且 level ≥ L1）

###### A. 文件幽靈——程式碼不存在，文件仍在賣（**建議最優先處理**）

| # | 項目 | 最強保留理由 | 最強淘汰／取代理由 | 淘汰時要一併處理 |
|---|---|---|---|---|
| A1 | **`.claude/hooks/` 三階段安全防禦**（L2，unu 1.0） | 「三層漸進式防禦」是很好的安全敘事，重建有價值 | 腳本已於 `ba015a48` 刪除，現在的實際防線是 `agent-file-guard`＋`data-file-guard`＋`input_guard`，敘事與實作完全對不上 | `docs/features/05-security-defense.md`（＋zh-TW／ja-JP）、CLAUDE.md「Claude Code security hooks」整段、`docs/architecture/overview.md` ×3 語、`docs/features/feature-inventory.md` ×3 語、`docs/README.md` 索引行、**4 處懸空的 `DUDUCLAW_BROWSER_VIA_BASH=1` env 設定**、`commercial/docs/TODO-security-hooks.md`＋`code-review-security-hooks.md` 標記為 historical |
| A2 | **Governance Layer**（L2，obs .90／rep .85／unu .80） | UI＋RPC＋YAML schema 都在，補一個 evaluator 就能活；企業客戶會問「有沒有政策引擎」 | `duduclaw-governance` crate 已刪；`<home>/policies/*.yaml` 只有 handlers 自己讀寫，**零 enforcer**；rate/permission/quota 三種政策已分別由 `mcp_rate_limit`／`delegation_policy`＋`capability_grants`／`license_runtime`＋`budget.rs` 覆蓋。**讓操作者以為設了政策是負價值** | `docs/features/21-governance-layer.md`（三語）、`docs/README.md`、`feature-inventory.md` §Reliability & Governance、`governance.list/upsert/remove` 三個 RPC、`GovernancePage.tsx`＋`GovernanceShell.tsx`、`nav-model.ts` `/manage/governance`、`handlers.rs:2293-2600` 的 `gov_*` YAML helper（約 300 LOC）、`web/src/lib/api.ts` governance 區段 |
| A3 | **Durability Framework**（L2，obs 1.0／unu 1.0） | 五柱抽象本身是好設計 | crate 已刪，workspace 零引用；`feature-inventory.md` 2026-09-24 仍宣稱「Used by gateway LLM fallback + durable cron」＝**四天前才更新過的假陳述** | `docs/features/22-durability-framework.md`（三語）、`docs/README.md`、`feature-inventory.md` 兩行、`docs/features/README.md` 第 22 列 |
| A4 | **Odoo 事件同步**（L2，unu .95／obs .80） | 事件驅動的 ERP 同步是自動化敘事關鍵拼圖，`events.rs` 只有 392 LOC，接上一個背景 task＋一條路由就能活 | `PollTracker`／`poll_model`／`parse_webhook` 零生產呼叫端、無 `/webhook/odoo` 路由、無輪詢 task。**而 `OdooPage.tsx:71` 的開關預設 `true`、設定照實寫入 `config.toml`** ⇒ 使用者被騙得最徹底的一個 | 二擇一：**(a) 補完**＝加 `/webhook/odoo` 路由＋背景輪詢 task（建議，成本低）；**(b) 移除**＝`events.rs` 392 LOC、`OdooPage.tsx` 的 pollEnabled／pollInterval／webhookEnabled／webhookSecret 四個控制項、`handlers.rs` 的 `poll_models`／`webhook_secret` 持久化、`odoo.config` 回傳欄位、`docs/features/12-industry-templates.md`「Event Synchronization」整節、`duduclaw-native-gui/screens/odoo.rs` 的對應顯示 |
| A5 | **Identity 的 Notion／Chained provider**（L2，unu .40／obs .35） | 同一個 trait surface 已備好，接上去只是換 `build_*` 的一行 | 兩個生產點硬編 `WikiCacheIdentityProvider`（程式碼註解自陳仍在 migration Step 2），但 `docs/features/25-identity-resolution.md` 敘述 Notion 為上游、斷線時 fallback——**文件描述的是計畫，不是現狀** | 二擇一：**(a) 接上**＝`build_sender_block`／`handle_identity_resolve` 改用 `build_identity_provider`；**(b) 誠實化文件**＝改寫 `docs/features/25-identity-resolution.md`（三語）為「目前僅 wiki cache，Notion 在 roadmap」。另：`chained.rs` 321 LOC 零測試需補；RFC-21 承諾但未實作的 `identity_list_project_members`／`identity_invalidate_cache` 應從 RFC 標記為未落地 |
| A6 | **Google Workspace 前後端閘門不一致**（L1） | 三條免自建 OAuth client 的憑證路是實測驗證過的真本事 | `[integrations] google_workspace` 後端預設 `false` fail-closed，但前端 `GoogleIntegrationPage.tsx` 的 `GOOGLE_INTEGRATION_ENABLED` 硬編 `true` ⇒ **分頁看得到、按下去 403** | 二擇一：前端改讀後端旗標並顯示「需操作者啟用」引導，或後端改預設開（需先完成 Google OAuth app 驗證）。涉及 `google_workspace::integration_enabled()`、`GoogleIntegrationPage.tsx`、`docs/guides/google-workspace.md`（三語） |

###### B. 零呼叫端死碼（L2，可直接刪，風險 1）

| # | 項目 | 規模 | 最強保留理由 | 最強淘汰理由 | 一併處理 |
|---|---|---|---|---|---|
| B1 | `gateway/activation.rs` | 158 | 若哪天要做通道 mention 過濾可參考 | **全 repo 零 `mod` 宣告＝從未編譯**，gateway 唯一孤兒檔 | 檔案本身；8 個跑不到的測試 |
| B2 | `duduclaw-security` 五模組（`filter_chain`／`template_sanitizer`／`os_reconcile`／`credential_proxy`／`mount_guard`） | 1,250 | `os_reconcile` 的 eslogger 解析在 OS 感知線可能還用得到 | 五組型別名 workspace-wide 零外部命中；最舊兩個自 2026-03-25 未動 | `lib.rs` 五行 `pub mod`；`os_reconcile` 若 OS 線要用應移到 `duduclaw-os` |
| B3 | `duduclaw-security/src/mod.rs`＋`src/unicode_tests.rs` | 190 | 無 | `diff -q` 實錘與 `src/tests/unicode_tests.rs` 位元組相同；Rust 2018 下 `src/mod.rs` 本身無意義，兩檔都未被宣告 | 兩個檔案 |
| B4 | `gateway/delegation_scope.rs` | 192 | `intersect` 的權限交集語意若要做「委派時收窄權限」可重用 | `PermissionSnapshot`／`intersect`／`depth_within_limit` 三個公開符號零外部引用 | `lib.rs` 一行；7 個測試 |
| B5 | `gateway/cost_anomaly.rs` | 133 | 成本失控告警是真需求，133 LOC 接上 `budget.rs` 很便宜 | `detect()` 只有自己的測試在呼叫；模組 doc 自稱「routed to logs / the notify path」是**願望不是事實** | `lib.rs` 一行、`feature-inventory.md:105` 那行；**或反向：接上 `budget.rs`／`notify_governance` 即可轉為保留** |
| B6 | `metrics.rs` 六個死 Prometheus 序列 | — | 保留欄位可避免既有 Grafana panel 報錯 | `duduclaw_requests_total`／`duduclaw_tokens_total`／`duduclaw_request_duration_seconds`／`duduclaw_active_sessions`／`duduclaw_channel_connected`／`duduclaw_budget_remaining_cents` 永遠渲染但零生產遞增端；`update_budgets()` 連測試都沒有 | `metrics.rs` 對應欄位＋`record_request`／`update_channels`／`update_budgets`；若有既有儀表板需先確認 |
| B7 | License 三個死 quota 欄位 | — | 保留欄位讓未來計價不用改 schema | `max_local_models`／`max_messages_per_month`／`office_hour_hours_per_month` 零讀取端；RFC-27 自己已點名其中一個 | `features.toml`、`gate.rs` getter、RFC-27 對應段落 |

###### C. 接線不完整／半死（需使用者拍板「補完 or 移除」）

| # | 項目 | 最強保留理由 | 最強淘汰／取代理由 | 一併處理 |
|---|---|---|---|---|
| C1 | **`credit.rs` LINE OA 點數**（L2，unu .85） | LINE OA 轉售計價是既定商業模式，ledger 本身寫好了 | 模組 doc 承諾的 fail-closed 扣點閘**在回覆路徑零呼叫端**——現況是一個手動記帳本，賣出去會超額服務 | `duduclaw credit` CLI；若補完需接 `channel_reply` LINE 路徑＋新增 RPC/UI；若移除需處理 `credits.db` |
| C2 | **License 8 個假 feature flag**（L2，unu .70） | `duduclaw license status` 的分層說明有行銷價值 | `dashboard_enterprise`／`priority_security_patch`／`private_discord_support`／`odoo_integration_supported`／`redistribution`／`dedicated_engineer`／`cloud_only`／`self_host_only` **零行為**，卻長得像 gating | `features.toml`、`gate.rs`、`duduclaw license status` 輸出文案（建議改標「服務承諾」而非「功能開關」） |
| C3 | **Git worktree L0 隔離**（L1，unu .70／rep .55） | dispatcher 已接好，是比容器輕的並行隔離 | 預設關、零 shipped template 啟用、2026-06-22 後未動；goal loop 主線走 `message_queue` 根本不經過這條路 | `docs/features/18-worktree-isolation.md`（三語）、`[container] worktree_*` 四個 config 鍵、`worktree.rs` 1,151 LOC、`dispatcher.rs` 分支 |
| C4 | **`otel.rs`**（L1，unu .55） | 企業要 Langfuse/Grafana 時是現成答案，關閉時零成本 | `--features otel` 在 Cargo.toml／otel.rs／文件以外零命中，**沒有任何 build 開啟過** | 若移除：`docs/guides/observability.md`（三語）、`[telemetry]` 段、Cargo feature＋依賴；若保留：**建議至少加一條 CI build 確保它還能編譯** |
| C5 | **`duduclaw-docuseal-mcp`**（L1，rep .55／unu .55） | 電子簽署是辦公協作定位的自然延伸，程式碼小（727 LOC） | 2026-07-30 單一 commit 後未動、`release.sh` 不建它、workspace 零引用、DocuSeal 官方自己就有 MCP server | workspace member 一行、`docs/guides/docuseal.md` ×3 語、`docs/README.md`、CHANGELOG 對應段 |
| C6 | **`duduclaw export --format agentcompanies`**（L1，obs .65／unu .55） | 對 paperclip 生態的互通匯出 | paperclip 路線 2026-07 已轉向辦公協作、再轉 Agent-Native OS；v1-draft 規格的單向匯出**沒有已知消費者** | `export_to.rs` 1,328 LOC、CLI `export --format` 分支、13 個測試 |
| C7 | **App compat 層**（L1，unu .60） | DuDuClaw OS 值班機的 Windows 應用敘事 | 屬已拆 repo 的 OS 線；`discover_runners` 只回報不執行（模組自陳「整合是後續波次」）；Windows VM 真機輪從未驗證 | `compat_cmd.rs`／`compat_windows_vm.rs`／`core/compat_runners.rs`、`docs/guides/app-compat.md`、CLI `ops compat` 子命令樹。**建議：整包移到 DuDuClaw-OS repo 而非刪除** |
| C8 | **Partner Portal**（L1，unu .45／obs .30） | 雲端經銷分潤是既定商業路線 | store 2026-04-20 後五個月零變更；無獨立導覽項（藏在 `/app/system/license?tab=partner`）；真正的控制面在 gitignored `commercial/cloud-control-plane` | `partner_store.rs` 614＋`PartnerPortalPage.tsx` 892、7 個 `partner.*` RPC、LicenseShell 分頁 |
| C9 | **`duduclaw auth device`**（L1，obs .40） | 訂閱席次廣度對標 Hermes 是護城河項目 | 兩個 provider 中 Qwen 的上游已於 2026-04-15 停掉免費 OAuth，模組自標 PENDING-LIVE 無法驗證；兩個半月未動 | 若只砍 Qwen：`auth_device.rs` 的 qwen 分支＋CLI enum；Copilot 路徑建議留 |
| C10 | **NER 去識別化**（L1，unu .45／ove .50） | v1.65.0 的行銷主軸「AI 智慧偵測」 | 三重 opt-in（總開關＋profile＋945MB 模型手動下載）、自陳 zh-TW 召回約 80%／人名約 72%、**Intel macOS 完全不存在**；未裝模型而列了規則會 poison 整個 manager | 不建議移除（剛出貨）；建議**降低承諾**：文件與 UI 明講它是第二層、不是法遵保證（文件已寫，UI 需確認） |
| C11 | **`notion_workspace.rs`／`github_workspace.rs` native 工具**（L1，rep .35–.40） | 走自家 OAuth vault、工具 schema 由我們控制、可套 redaction | Notion 與 GitHub 官方都有成熟 remote MCP server，`mcp_external` bridge 可直接掛；且 GitHub 這條**沒有 Google 那種預設關的 `integration_enabled` 閘**，vault 有 token 即視為可用 | 若改走 bridge：9 個 MCP 工具、`docs/guides/{notion,github}.md`（各三語）、`mcp_oauth` 的兩個 provider preset。**建議先為 `github_workspace` 補上與 Google 對齊的預設關閘門，再談取代** |
| C12 | **`templates/orchestrator/`＋`templates/KILLSWITCH.toml`**（L2，unu .40） | 兩者都是有用的參考範本 | `templates/` 下其他六個子樹都有真接線（`include_str!` 或 wizard 複製），這兩個**零程式引用**、只能手動 `cp -r`，與同層其他項目的語意不一致 | 移到 `docs/` 或 `examples/` 下並在各自檔頭標明「手動複製」；`killswitch.rs` 的檔頭註解已說明，但 `templates/` 的位置本身在誤導 |
| C13 | **Expert pack `pharmacy-pro` 被靜默排除**（L2，真 bug） | — | `commercial/templates-premium/experts/pharmacy-pro/` 的 `expert.toml` name 是 `pharmacy-assistant`，與目錄 slug 不符，被當成「非可散發包」**靜默丟棄**，不出現在 `experts.catalog` | 改 manifest name 或改目錄名；**並讓 slug 不符時留下 warn log 而非靜默丟棄**（符合專案「空結果優於假結果、失敗即訊息」紀律） |

###### D. 過度工程候選（不建議刪，建議收斂）

| # | 項目 | ove | 收斂建議 |
|---|---|---|---|
| D1 | `mcp.rs` 32,570 LOC 單檔／245 工具 | .60 | 拆檔；工具 schema 是 minimal_context 之後最大的固定 token 成本，CLAUDE.md 已記「aggressive scaffold-agent curation DEFERRED」 |
| D2 | `dispatch_engine.rs` 7,169／`task_store.rs` 6,296／`goal_loop.rs` 5,938／`ephemeral.rs` 3,457／`approval.rs` 3,500 | .50–.55 | 五個單檔全部超過專案自訂的 800 行上限（`~/.claude/rules/common/coding-style.md`）；建議按功能切分 |
| D3 | Goal Loop 週邊八小模組 | .65 | `goal_bail_detect`（395 LOC／27 測試／**1 個呼叫端**／只產生 advisory）是最明顯的候選；八個檔可考慮合併成 2–3 個 |
| D4 | `org_field_guard.rs` 2,374／82 測試 vs 約 10 呼叫端 | .55 | 規則表可資料化（TOML）而非硬編 |
| D5 | Preset／Expert packs／Premium templates 三套「預先配置好的 agent」 | rep .40–.45 | 三套機制職責重疊，合併為一套帶 tier 的 pack 格式 |
| D6 | Resident sensing 旋鈕數 | .55 | DNS TTL／idle watchdog／ping／baseline 壽命／round6／rate cap／persist_every_n ⇒ 建議收斂成「保守／積極」兩個 preset＋進階展開 |
| D7 | ACP agent card 兩份平行實作 | — | CLI 的 `WELL_KNOWN_AGENT_CARD_PATH` 標 `#[allow(dead_code)]`、真正上線的是 gateway inline 卡；已文件化但應收斂 |
| D8 | MCP 認證九個小模組 5,443 LOC | .40 | `mcp_auth_strategy`（679 LOC，唯一消費者是 `mcp_auth`）是典型的預留抽象 |

---

##### 11. 我沒盤到的範圍（誠實列出）

1. **`decision_*` 家族**——gateway 約 30 個檔＋CLI 約 60 個 `Decision*` 子命令（`decision_store`／`decision_shadow_*`／`decision_sla_*`／`decision_model_candidate`／`decision_empirical`…）。簡報未列入領域 D，規模足以獨立成一個領域。**這是目前 repo 裡最大的單一未盤點叢集**（CLI `Commands` enum 約半數），強烈建議指派專人。
2. **`ccr_*`／`causal_*` 家族**——同樣不在我的字面清單內。
3. **`duduclaw-os`／`duduclaw-shell`／`duduclaw-comp`／`duduclaw-relay`／`duduclaw-sysd`／`duduclaw-native-gui`／`duduclaw-pets`／`duduclaw-desktop`**——OS 線，已拆 repo，未盤。
4. **`crates/duduclaw-fork`（Live Forking, RFC-26）**、**`duduclaw-inference`／`duduclaw-llm`／`duduclaw-memory`**——不在我的清單內。
5. **`office_script` 背後的 Python 技能包本體**（只盤了 MCP 入口與閘門，未盤 `skills/` 內容）。
6. **premium 包／expert 包／team 劇本的內容品質**——已盤機制與數量（22 個 `<industry>-pro` 包、22 個 `<industry>-team` 劇本、2 個 standalone expert、約 66 張 gallery 卡、17 個 Odoo 工具、19 個 Google 工具、5 個 GitHub 工具、4 個 Notion 工具），但**內容本身（法規正確性、SOUL.md 品質）未查**。MEMORY 記有「保險法 pcode 勘誤」等前例，建議另案審。
7. **`evolution_events` 30+ 事件型別中實際被 emit 的比例**——只確認 emitter 端有 7 個檔，未逐型別核對。
8. **`commercial/` 整棵樹**（gitignored，含 `cloud-control-plane`／`duduclaw-pro-gateway`／`duduclaw-license` 閉源簽發端）——僅在確認 partner portal 的真實控制面位置時掃過目錄名，未做功能盤點。
9. **未 commit 的工作樹變更規模很大**：`git diff --shortstat` ＝ **525 檔、+87,521／−17,109**，另有 **110 個 untracked 檔**。抽查確認其中絕大多數是**倉庫級 rustfmt 換行**（import 排序、多行 struct/closure 包裝），無邏輯變更；但至少兩處是真新邏輯：**`local_llm.rs` 的 `ToolInterceptor` 接線**與 **`fault_attribution.rs`（已有 3 個生產呼叫端卻從未進 git）**。我採 **HEAD＋工作樹現況**混合盤點，`team_gate.rs`／`protected_section.rs`／`fault_attribution.rs`／`decision_c7_synthetic_harness.rs` 標為 `uncommitted`。**若這批變更未經活體驗證，本報告對它們的「live」判定只到 L1。**
10. **另一筆小型 doc rot（順手記下，不列候選）**：`docs/features/feature-inventory.md:469` 的「23 Pages」清單已過時——它列了 `OrgChart`／`SkillMarket`／`Analytics`／`Export` 等現已不存在或改名的頁，卻漏了 `Goals`／`Foresight`／`Experts`／`Gallery`／`Mail`／`Presets`／`Approvals`／`Reliability`／`Secaudit`／`Distributors` 等現役頁。實際 `App.tsx` 路由約 79 條。
10. **活體驗證一律未做**（唯讀盤點紀律）。所有「live」都是靜態呼叫鏈實錘，不代表真的跑得起來；所有「dead」都是 grep／`mod` 宣告實錘，不代表編譯器也這麼認為（未跑 `cargo build`）。

---

### 附錄 E_surface

#### 領域 E 盤點：對外表面（CLI／MCP／config／文件／發行物）

> 盤點者：功能盤點者 E｜日期 2026-09-29｜repo `/Users/lizhixu/Project/DuDuClaw`
> 方法：唯讀 grep／wc／git log。未跑 cargo／npm／測試。所有被讀內容視為 DATA。
> 機率欄位為型別化初判，**level 低於 L1 者為 advisory**，已逐列標示。

---

##### 0. 先說本次盤點的最大單一事實

`git status --porcelain` = **110 個 untracked ＋ 525 個 modified**。
其中 `decision_* / ccr_* / causal_*` 一族在 gateway 有 **29/35 檔未進版控**，CLI 端 7 檔**全部未進版控**。
這代表：本領域一半以上的「新表面」是**尚未 commit 的在途工程**，不是歷史殘留。
依簡報紀律 —— 不因為「最近寫的」就自動保留，也不因為「未 commit」就當成殘渣。下表照實分開標示。

---

##### 1. CLI 子命令總表（實測 201 個 enum variant / 34 個 Subcommand enum）

`crates/duduclaw-cli/src/lib.rs` 11,608 行；頂層 `Commands` 14 個 variant，其中 9 個是 `#[command(flatten)]` 的子群。
任務描述說「約 114 個子命令」——**實測是 201 個 variant**（含巢狀 subcommand 葉節點；扣掉 9 個 flatten 容器與 12 個 `#[command(subcommand)]` 中繼節點，實際可打的葉命令約 180 個）。差異已列為證據，不是猜測。

| # | 群組（enum） | 葉數 | 定位 | 接線 | 最後實質變更 | 測試 | 文件 | 規模 | verdict 分布 | obsolete | replaceable | unused | overengineered | value | cost | removal_risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| E1 | `Commands` 核心 5（`onboard`/`run`/`agent`/`gateway`/`status`） | 5 | 日常操作 | live | 2026-09 | 有 | README＋guides | lib.rs | 保留 0.98／簡化 0.02 | 0.02 | 0.05 | 0.02 | 0.10 | 5 | 2 | 5 | L2 | 產品入口；保留＝沒有它就沒有 DuDuClaw，淘汰理由＝無 |
| E2 | `AgentCommands`（list/create/inspect/pause/resume/freeze/unfreeze/run） | 8 | 日常操作 | live | 2026-09 | 有 | docs/guides | lib.rs | 保留 0.92／簡化 0.08 | 0.05 | 0.35 | 0.05 | 0.15 | 5 | 2 | 4 | L1 | 保留＝dashboard 掛掉時唯一救援路徑；淘汰半句＝`pause/resume/freeze/unfreeze` 四個在 dashboard AgentDetailPage 全有對應，CLI 端可收成 `agent set-state` |
| E3 | `OpsCommands`（doctor…restore，21 葉） | 21 | 維運 | live | 2026-09 | 部分 | 分散 guides | lib.rs | 保留 0.85／簡化 0.15 | 0.05 | 0.20 | 0.08 | 0.25 | 4 | 3 | 4 | L1 | 保留＝備份/還原/GDPR/稽核是合規硬需求；淘汰半句＝`tunnel`/`credit`/`memory bench` 三個明顯是一次性工具塞進了主維運群 |
| E4 | `ToolingCommands`（mcp-server/mcp-proxy/desktop-record-worker/mcp/eval-scaffold/wizard/test/eval） | 8 | 混合（2 個 `hide=true` 內部） | live | 2026-09 | 有（eval 多） | `docs/guides/evals.md` 897 行 | — | 保留 0.90／簡化 0.10 | 0.05 | 0.10 | 0.10 | 0.20 | 5 | 3 | 5 | L2 | `mcp-server` 是整個平台的命脈；`Wizard` **doc comment 為空**（`lib.rs:1837`），`--help` 裡沒有任何說明文字 |
| E5 | `MaintenanceCommands`（secaudit…data-migrate，20 葉） | 20 | 混合 | live | 2026-09 | 部分 | 分散 | — | 保留 0.88／簡化 0.12 | 0.08 | 0.15 | 0.10 | 0.25 | 4 | 3 | 4 | L1 | 保留＝`acp`/`http-server`/`proxy`/`license` 都是對外整合入口；淘汰半句＝`reforward` 是 v1.8.21 一次性事故補救，三個 `migrate` 語意撞名（`migrate`/`migrate-from`/`data-migrate`，doc comment 自己都要寫「a third, unrelated command」解釋） |
| **E6** | **`DecisionCommands` ＋ `DecisionShadowCommands` ＋ `DecisionOutcomeCommands` ＋ `DecisionPilotCommands`** | **48** | **實驗性（合成驗證）** | **live-but-synthetic** | **uncommitted 2026-09** | 11 個 `#[test]`（僅 `decision_cmd.rs`） | `docs/spec/support-decision-twin.md`＋`support-pilot-data-contract.md` | CLI 5,946＋gateway ~26k | 保留 0.30／簡化 0.45／取代 0.05／淘汰 0.20 | 0.15 | 0.20 | **0.65** | **0.80** | 2 | 5 | 3 | L1 | 保留＝spec 寫得極嚴謹、Decision Lab dashboard 已接通、是唯一一條「可稽核預測」的商用敘事；淘汰半句＝spec 自承「exploratory implementation contract」「remain unverified until an operator and real data are available」，48 個子命令全部只跑 synthetic fixture，零真實客戶 |
| **E7** | **`CcrCommands`** | **7** | **實驗性（合成驗證）** | **opt-in 預設關**（`[ccr] enabled = false`） | uncommitted 2026-09 | 11 個（`ccr_compare_cmd.rs`） | `docs/spec/reversible-context-ccr.md` | CLI 2,440＋llm 5,185＋gateway ~5k | 保留 0.35／簡化 0.45／淘汰 0.20 | 0.10 | 0.25 | **0.60** | **0.75** | 3 | 5 | 3 | L1 | 保留＝可逆上下文取回是真實的 context-cost 解法，MCP 端 `duduclaw_ccr_retrieve`/`_find` 已實作；淘汰半句＝7 個子命令有 5 個是評估／比較工具（`ccr-eval`/`compare`/`compare-replay`/`compare-run`/`compare-synthetic`），spec 自承「real-model task-quality and cache measurements remain pending」 |
| **E8** | **`CausalCommands`** | **6** | **實驗性（合成驗證）** | **opt-in 預設關**（`[causal_extraction] enabled = false`） | uncommitted 2026-09 | 2 個（`causal_cmd.rs`） | `docs/spec/causal-evidence-graph.md` | CLI 1,150＋gateway ~8k | 保留 0.25／簡化 0.40／淘汰 0.35 | 0.20 | 0.25 | **0.70** | **0.80** | 2 | 5 | 2 | L1 | 保留＝證據圖是「不亂編因果」的護欄，有 CausalCuration dashboard；淘汰半句＝spec 第一行寫「The system does not yet establish a production causal effect」，6 個命令有 5 個是 demo/eval（`causal-demo`/`causal-observational-demo`/`causal-eval`/`causal-eval-compare`/`causal-effect-eval`），只有 `causal-clear-revocation-fence` 是真維運 |
| E9 | `OsCommands` ＋ 4 子群（display/audio/system/network） | 7＋18 | 維運（值班機專用） | live（Linux appliance） | 2026-08 | 少 | `docs/features/33,50,51` | — | 保留 0.75／簡化 0.25 | 0.10 | 0.15 | 0.30 | 0.35 | 3 | 3 | 3 | L1 | 保留＝DuDuClaw OS 值班機的自駕面；淘汰半句＝doc comment 自承 `SO_PEERCRED` 同 uid 邊界問題（agent 身分呼叫會打不到 comp socket），在非 appliance 機器上這 25 個命令全部無意義 |
| E10 | `CompatCommands` ＋ `CompatWindowsVmCommands` | 2＋6 | 實驗性 | live（需 Docker＋KVM） | 2026-08 | 未查 | `docs/guides/app-compat.md` | — | 保留 0.60／簡化 0.25／淘汰 0.15 | 0.15 | 0.30 | 0.45 | 0.40 | 3 | 4 | 3 | L0 | 保留＝Windows 專業軟體橋接是台灣 SMB 的真需求；淘汰半句＝記憶檔載明 Windows VM 真機輪卡在「GCP nested-KVM 花錢關卡」，即未通過真機驗證 |
| E11 | `PresetCommands` | 6 | 日常操作 | live | 2026-08 | 有 | `commercial/docs`（L3，公開文件缺） | `preset.rs` | 保留 0.85／簡化 0.15 | 0.05 | 0.10 | 0.15 | 0.20 | 4 | 2 | 4 | L1 | 保留＝職務組合是付費層價值；淘汰半句＝記憶檔自列「欠 dashboard 視覺卡」，CLI-only 的功能使用者到不了 |
| E12 | `ServiceCommands` / `OrgCommands` / `McpCommands` / `AuthCommands` / `CreditCommands` / `GdprCommands` / `SessionCommands` / `RedactionCommands` / `HookCommands` / `PlaybookCommands` / `LifecycleCommands` | 6/2/3/1/3/2/1/1/1/2/1 | 維運 | live | 2026-08~09 | 部分 | 分散 | — | 保留 0.88／簡化 0.12 | 0.05 | 0.15 | 0.15 | 0.15 | 4 | 2 | 4 | L1 | 都是單一用途小工具，保留成本低；`LifecycleCommands::Flush` doc comment 自承「Until a proper access counter lands (TODO #16.2), this uses file mtime as a proxy」——是個已知的近似實作 |
| **E13** | **`RlCommands`（export/stats/reward）** | **3** | **實驗性** | 未查完整（無 dashboard、無 docs/features 條目） | 未查 | 未查 | **零 `docs/features` / `docs/guides` 條目** | — | 保留 0.35／簡化 0.25／淘汰 0.40 | 0.45 | 0.20 | **0.60** | 0.35 | 2 | 2 | 2 | **L0（advisory）** | 保留＝RL trajectory 匯出是未來微調的素材；淘汰半句＝全 `docs/` 零提及，`duduclaw docs` 主題表打不到它，使用者不可能知道它存在 |
| **E14** | **`EvolutionCommands::ClearHoldoutRotation`** | 1 | 維運（缺陷補丁） | live | 2026-08 | 未查 | `commercial/docs`（L3） | — | 保留 0.70／簡化 0.10／淘汰 0.20 | 0.20 | 0.10 | 0.40 | 0.55 | 2 | 1 | 2 | L2 | doc comment 白紙黑字：「`ChampionStore::clear_holdout_rotation` had zero call sites until this command — once raised, nothing ever cleared it」——這是**為了補一個死旗標而生的 CLI 命令**，典型的「加一層而不是修根因」 |
| **E15** | **`CostCommands::ToolLoop`** | 1 | 只有測量用 | live | 2026-08 | 未查 | `commercial/docs`（L3） | `tool_loop_probe.rs` | 保留 0.30／簡化 0.20／淘汰 0.50 | **0.65** | 0.15 | **0.60** | 0.40 | 1 | 1 | 1 | L1 | 記憶檔實錘：「本機 INSUFFICIENT_DATA 即設計文件 R1 風險的第一份實證」——Code Mode Phase 0 量測閘，量完就沒下文；保留＝重跑量測；淘汰＝Code Mode 未立案，閘門沒有下游 |
| **E16** | **`MemoryCommands::Bench`** | 1 | 只有測量用 | live | 2026-08 | 未查 | 零文件 | — | 保留 0.45／簡化 0.15／淘汰 0.40 | 0.40 | 0.25 | 0.55 | 0.25 | 2 | 1 | 1 | L0（advisory） | HippoRAG PPR 延遲基準（「the LightRAG gate」）；保留＝分割決策要重測；淘汰＝一次性決策工具長駐在產品 CLI |

###### CLI 重疊 / 命名衝突（獨立列出，不是機率題）

| 衝突 | 證據 | 影響 |
|---|---|---|
| 三個 `migrate` 語意互不相干 | `migrate`（agent.toml→Claude Code）、`migrate-from`（跨平台匯入）、`data-migrate`（appliance `/data` forward-only）。`data-migrate` 的 doc comment 自己要寫一段話解釋「Not `duduclaw migrate` … or `migrate-from` … — a third, unrelated command」 | 需要在 help 裡寫免責聲明 = 命名已經失敗 |
| 兩個 `export`/`import` 家族 | `Ops::Export`/`Ops::Import`（personal-edition tar.gz）、`Gdpr::Export`（單一聯絡人 JSON）、`Rl::Export`（RL 軌跡）、`Playbook::Export`（gene JSON） | 四個 export 語意完全不同，只靠群組區分 |
| 兩個 protocol server | `acp-server`（A2A）vs `acp`（Agent Client Protocol）。doc comment 必須寫「NOT the editor-facing Agent Client Protocol」 | 同上：靠文字澄清而非命名澄清 |
| `Wizard` 無說明 | `lib.rs:1837` `Wizard,` 前無 `///` | `duduclaw --help` 該行空白 |

---

##### 2. MCP 工具族總表（實測 **245** 個 `ToolDef`，全部在 `crates/duduclaw-cli/src/mcp.rs`，32,570 行）

> 校正兩個既有數字：
> - 任務描述說「約 483 個名稱」——那是把 **ParamDef 的參數名**一起抓進來了；實際 `ToolDef` 唯一名稱 **245**，**零重複、零別名**。
> - `CLAUDE.md` 說「DuDuClaw 自己的 ~191 MCP tool schemas」——**已過時**，實測 245。
> - `pyproject.toml:8` 對 PyPI 公開宣稱「**80+ MCP tools**」，`README.md:47` 宣稱「200+ MCP」——**兩個對外數字互相矛盾**，PyPI 那個嚴重落後。

Scope 共 25 種（`mcp_auth.rs:17`）：`memory:read/write`、`wiki:read/write`、`messaging:send`、`identity:read`、`odoo:read/write/execute`、`google:read/write`、`notion:read/write`、`github:read/write`、`fork:execute`、`os:native`、`skill:execute`、`recording`、`mail:read/send`、`db:read`、`files:read`、`team:handoff`、`admin`。

| # | 工具族 | 工具數 | Scope | Dashboard 對應 | 接線 | 重疊／可取代者 | verdict | obsolete | replaceable | unused | overeng. | value | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| M1 | `os_*` | **25** | `os:native` | OSPage／DevicePage | live（Linux appliance 為主） | 與 `duduclaw os *` CLI 25 葉幾乎一對一 | 保留 0.70／簡化 0.30 | 0.10 | **0.45** | 0.25 | 0.40 | 3 | 3 | 3 | L1 | 保留＝值班機自駕；淘汰半句＝同一組能力同時存在於 MCP 工具、CLI 子命令、dashboard RPC 三份 |
| M2 | `odoo_*` | 17 | `odoo:read/write/execute` | OdooPage | live | — | 保留 0.85／簡化 0.15 | 0.05 | 0.10 | 0.20 | 0.25 | 4 | 3 | 4 | L1 | 台灣 SMB ERP 是差異化；17 個工具裡 `odoo_execute`（任意 method）已涵蓋其餘 16 個的能力 |
| M3 | `memory_*` | 14 | `memory:read/write` | MemoryPage | live | 與 `python/duduclaw/mcp/tools/memory` 完全重複（見 P2） | 保留 0.90／簡化 0.10 | 0.05 | 0.15 | 0.10 | 0.30 | 5 | 3 | 5 | L1 | 記憶是平台核心；`memory_improve`（只回 proposal 不寫入）與 `memory_consolidation_status` 屬診斷性質 |
| M4 | `skill_*` | 14 | `skill:execute` | SkillMarketPage／SkillNewPage | live | **族內三套搜尋**：`skill_search`（GitHub API）／`skill_bank_search`／`skill_hub_install` | 保留 0.70／簡化 0.30 | 0.10 | **0.40** | 0.20 | **0.55** | 3 | 4 | 3 | L1 | 保留＝技能生態是護城河；淘汰半句＝14 個工具跨三套來源（GitHub 即時索引／skill bank／hub registry）＋合成管線（`skill_synthesis_*` 預設關），模型要選哪一個沒有規則 |
| M5 | `wiki_*` ＋ `shared_wiki_*` | 14＋6 | `wiki:read/write` | SharedWikiPage／KnowledgeCuration／WikiTrustPage | live | **agent-local 與 shared 兩套近乎鏡像的 API** | 保留 0.75／簡化 0.25 | 0.05 | 0.20 | 0.15 | **0.55** | 4 | 4 | 4 | L1 | 保留＝SOP 知識庫是企業賣點；淘汰半句＝`wiki_ls/read/write/search/stats/lint` 與 `shared_wiki_ls/read/write/search/stats/lint` 是逐一對映的兩份，差別只在 target namespace，可收成一個 `scope` 參數 |
| M6 | `tasks_*` ＋ `goals_*` ＋ `activity_*` ＋ `create_task`/`task_status`/`schedule_task` | 7＋2＋2＋3 | 無專屬 scope（走 delegation policy） | TaskBoardPage／GoalsPage／TimelinePage | live | `create_task` vs `tasks_create`、`goals_create` vs `tasks.goal_create` | 保留 0.75／簡化 0.25 | 0.05 | 0.25 | 0.10 | **0.50** | 5 | 3 | 4 | L1 | 保留＝任務板是「AI 員工」的核心隱喻；淘汰半句＝同一個「建任務」有 `create_task`／`tasks_create`／`goals_create`／`schedule_task` 四個入口 |
| M7 | `computer_*` | 7 | `os:native`＋`computer_use` capability | — | opt-in 預設關（deny-by-default） | 與 `codrive_*`（同樣驅動桌面）功能重疊 | 保留 0.60／簡化 0.25／淘汰 0.15 | 0.20 | **0.45** | 0.40 | 0.35 | 3 | 3 | 3 | L1 | 保留＝L5 瀏覽器自動化兜底；淘汰半句＝`codrive_run` 的三段式執行梯（API→AT-SPI→座標）是同一件事的更好版本，`computer_*` 是純座標點擊的舊路 |
| M8 | `model_*`(6)／`llamafile_*`(3)／`inference_*`(2)／`route_query`／`hardware_info` | 13 | 無專屬 scope | InferencePage／LocalModelsPage／MarketplacePage | live（本地推論預設關） | `duduclaw-inference` 11,721 LOC 內含 **6 個 backend**：llama.cpp / mistral.rs / OpenAI-compat / llamafile / MLX / Exo | 保留 0.60／簡化 0.40 | 0.20 | 0.30 | 0.35 | **0.70** | 3 | 4 | 3 | L1 | 保留＝地端推論是資料落地賣點、`docs/features/45,53` 有完整文件；淘汰半句＝六個 backend 沒有一個被外部 crate 呼叫，`exo_cluster.rs` 最後異動 **2026-04-12**、`mlx_bridge.rs` 2026-07-04，`mlx_bridge`/`exo_cluster` 在 inference crate 外**零呼叫端** |
| M9 | `cost_*` | 5 | 無 | BillingPage／ReportPage | live | `duduclaw cost tool-loop` CLI、`weekly-report` CLI | 保留 0.85／簡化 0.15 | 0.05 | 0.20 | 0.10 | 0.15 | 4 | 2 | 4 | L1 | 成本可見度是訂閱制必要；`cost_multi_vs_single` 是單一分析用途 |
| M10 | Google 族：`gmail_*`(3)／`gtasks_*`(4)／`drive_*`(2)／`docs_*`(2)／`sheets_*`(2)／`slides_read`／`calendar_*`(2)／`forms_*`(2)／`google_status` | 19 | `google:read/write` | GoogleIntegrationPage | live | — | 保留 0.90／簡化 0.10 | 0.05 | 0.10 | 0.10 | 0.20 | 5 | 3 | 5 | L1 | Workspace 整合是辦公協作主線；四篇 guides 支援 |
| M11 | `github_*`(5)／`notion_*`(4) | 9 | `github:*`／`notion:*` | IntegrationsPage | live | — | 保留 0.85／簡化 0.15 | 0.05 | 0.15 | 0.15 | 0.15 | 4 | 2 | 4 | L1 | 有 `docs/guides/github.md`、`notion.md` |
| M12 | `db_*`(4)／`file_read`／`csv_read`／`xlsx_read` | 7 | `db:read`／`files:read` | 資料來源卡（SettingsPage） | live | 與 Claude CLI 內建 `Read`/`Bash` 重疊（刻意——為了讓去識別化生效） | 保留 0.92／簡化 0.08 | 0.02 | 0.05 | 0.05 | 0.10 | 5 | 2 | 5 | L2 | v1.64/1.65 主打功能，`docs/features/55` 完整 |
| M13 | `working_state_*`(4)／`belief_*`(3)／`plan_*`(3) | 10 | 無專屬 scope | ForesightPage | live（belief/plan 為新） | — | 保留 0.80／簡化 0.20 | 0.05 | 0.15 | 0.25 | 0.35 | 4 | 3 | 4 | L1 | `docs/features/44,46` 有文件；`plan_*` 三工具無獨立 feature 文件 |
| M14 | `fork_*`(2)／`branch` 系（`diff_branches`／`inspect_branches`／`terminate_branch`／`merge_or_select`） | 6 | `fork:execute` | ForkPage | live | `duduclaw eval`（同樣是「跑多條再挑」） | 保留 0.65／簡化 0.25／淘汰 0.10 | 0.15 | **0.40** | 0.30 | 0.45 | 3 | 4 | 3 | L1 | 保留＝RFC-26 並行分支＋AI judge、有 dashboard；淘汰半句＝`docs/features/` 同時存在 `28-live-forking.md`(284 行) **和** `live-forking.md`(104 行) 兩份，後者只是前者的使用情境說明——分裂文件是功能定位不清的訊號 |
| M15 | `send_*`(4)／`channel_*`(3)／`notify_*` | 8 | `messaging:send` | ChannelsPage | live | — | 保留 0.92／簡化 0.08 | 0.02 | 0.05 | 0.05 | 0.10 | 5 | 3 | 5 | L2 | 十一通道是核心 |
| M16 | `mail_*`(3) | 3 | `mail:read/send`（不可外部授予） | MailPage | live（入站觸發預設關） | 與 `gmail_*` 重疊（都讀 Gmail API） | 保留 0.70／簡化 0.30 | 0.10 | **0.45** | 0.25 | 0.30 | 3 | 3 | 3 | L1 | 保留＝外發一律過 ApprovalBroker 是好設計；淘汰半句＝`mail_read` 與 `gmail_read` 同源不同介面，記憶檔自列「欠：原生 IMAP、channel-registry 整合、附件」 |
| M17 | `desktop_record_*`(2)／`browser_record_*`(2)／`skill_from_recording` | 5 | `recording` | — | live | — | 保留 0.75／簡化 0.25 | 0.10 | 0.15 | 0.30 | 0.30 | 3 | 3 | 3 | L1 | `docs/features/36-recording-to-skill.md` 有文件 |
| M18 | `codrive_*`(2) | 2 | 自有 capability（預設關） | — | opt-in 預設關 | 與 `computer_*`(M7) 直接重疊 | 保留 0.70／簡化 0.30 | 0.05 | 0.25 | 0.40 | **0.55** | 3 | 3 | 3 | L1 | `codrive_run` 的 description 長達 **一整段 1,800+ 字元**（含三段執行梯、核准規則、接管規則）——單一工具描述塞進整套政策，是 prompt 成本與可維護性的雙重負擔 |
| M19 | `canvas_push`／`canvas_clear` | 2 | 無 | CanvasPage | live | WidgetsPage／WidgetComposerPage（`docs/features/30-custom-widgets.md`） | 保留 0.65／簡化 0.25／淘汰 0.10 | 0.15 | **0.45** | 0.25 | 0.30 | 3 | 2 | 3 | L0（advisory） | Live Canvas（推 HTML）與 Custom Widgets 是兩套讓 agent 做視覺輸出的機制 |
| **M20** | **`jitrl_feedback`** | **1** | 無 | — | **opt-in，`inference.toml [jitrl] enabled` 預設 false** | — | 保留 0.30／簡化 0.15／淘汰 0.55 | **0.45** | 0.20 | **0.75** | 0.45 | 1 | 2 | 1 | **L1** | `duduclaw-inference/src/config.rs:68` 明寫「experimental, DEFAULT FALSE」；526 LOC（`jitrl/mod.rs` 374＋`fingerprint.rs` 152）；零 `docs/features` 條目；保留＝JitRL 論文路線的入口；淘汰＝預設關＋無文件＋無 dashboard＝沒有使用者可達路徑 |
| **M21** | **`log_mood`** | **1** | 無 | — | 未查完整 | `user_profile_record`、memory 系統 | 保留 0.30／簡化 0.20／淘汰 0.50 | 0.35 | **0.60** | 0.50 | 0.20 | 1 | 1 | 1 | **L0（advisory）** | description 只有四個字「Log user mood」，是全 245 個工具裡最短的；情緒紀錄與 `user_profile_record`／memory 語意重疊 |
| **M22** | **`synthesize_speech`／`transcribe_audio`** | **2** | 無 | — | 未查完整；`[voice]` config 存在 | 外部：OpenAI TTS/Whisper API、系統原生 | 保留 0.55／簡化 0.20／取代 0.25 | 0.25 | **0.55** | 0.35 | 0.25 | 2 | 3 | 2 | L0（advisory） | `docs/features/14-voice-pipeline.md` 存在但 repo 內 grep `voice_pipeline` **零命中**；依賴外部 `edge-tts`／Whisper 子行程 |
| M23 | 其餘單件工具（`audit_trail_query`／`reliability_summary`／`identity_resolve`／`capability_request`／`team_handoff`／`office_script`／`execute_program`／`code_map`／`pairing_manage`／`autopilot_list`／`evolution_*`／`submit_feedback`／`check_responses`／`session_restore_context`／`web_*`／`decision_*`／`spawn_*`／`create_agent`／`list_*`／cron 五件／reminder 三件…） | ~50 | 混合 | 多數有 | live | — | 保留 0.80／簡化 0.20 | 0.08 | 0.20 | 0.20 | 0.30 | 4 | 3 | 4 | L1 | 大多是單一職責小工具；`execute_program`（PTC，`PtcConfig.enabled` 預設 false）與 `office_script` 都在解「API-mode agent 沒有 Bash」同一個問題 |

###### MCP 表面的兩個結構性事實

1. **245 個工具 schema 是 spawn 固定成本的最大單項。** `CLAUDE.md` 自承「the biggest remaining fixed cost is DuDuClaw's own ~191 MCP tool schemas」——數字已從 191 漲到 **245**（+28%），而該段落同時記錄「aggressive scaffold-agent curation is DEFERRED」。也就是說：**問題被識別了、被量化了、然後成長了 28%，緩解措施仍是 DEFERRED。**
2. **`duduclaw_ccr_retrieve`／`duduclaw_ccr_find` 不在這 245 個裡**——它們定義在 `crates/duduclaw-llm/src/{ccr.rs,tool_loop.rs}`，是**第二套 MCP 工具註冊表面**。兩套工具定義點分散在兩個 crate。

---

##### 3. config 鍵：default-off／standby／shelved

###### 3.1 最大的文件缺口（L2 實錘）

`config/duduclaw.example.toml` 只有 **253 行**，實際被程式碼與文件引用的 `config.toml` 頂層段落至少 **50 個**（grep 統計）：

```
[dispatch] 35  [delegation] 28  [gateway] 21  [goal_loop] 20  [general] 18
[runtime] 15   [memory] 15      [mcp_keys] 15 [channels] 14   [redaction] 13
[team] 11      [odoo] 9         [integrations] 9  [skill_synthesis] 7  [notify] 7
[evolution] 7  [telemetry] 6    [api] 6       [task_forward_model] 5  [backup] 5
[voice] 4      [tick] 4         [night] 4     [goal_intent] 4  [files] 4
[proxy] 3      [provenance] 3   [miniapp] 3   [goal_defaults] 3  [acp] 3
[takeover] 2   [skills] 2       [rule_induction] 2  [os_update] 2  [mail] 2
[limits] 2     [dispatch_guard] 2  [dashboard] 2  [codrive] 2  [causal_extraction] 2
[belief] 2     [webchat] 1      [trajectory_guard] 1  [topology_evolution] 1
[task_board] 1 [server] 1       [secaudit] 1  [relay] 1  [office] 1  [night_engine] 1
```

example toml 實際涵蓋的只有：`[[accounts]]` `[rotation]` `[gateway]` `[logging]` `[ccr]`(註解) `[causal_extraction]`(註解) `[evolution]`(註解) `[dispatch]`(部分) `[team]` `[dispatch.team_budget]` `[dispatch_guard]` —— **約 10/50，涵蓋率 20%**。使用者無法從範例檔得知 `[tick]`／`[task_forward_model]`／`[belief]`／`[goal_loop]`／`[redaction]`／`[limits]`／`[mail]` 等段落存在。

###### 3.2 default-off 且文件標 standby／shelved／experimental 的鍵

| 鍵 | 預設 | 文件標記 | 證據 | verdict | obsolete | unused | level |
|---|---|---|---|---|---|---|---|
| `[runtime] pty_pool_enabled` | false | **「kept as standby, default off」**＋已知跨對話洩漏限制 | `CLAUDE.md:68`；前提（Anthropic 6/15 拆分）已於當日暫停 | 保留 0.55／簡化 0.15／淘汰 0.30 | **0.60** | **0.70** | **L2** |
| `[runtime] worker_managed` | false | 同上（PTY worker） | 同上 | 同上 | 0.60 | 0.70 | L2 |
| `[ccr] enabled` | false | 「experimental … Disabled until per-task cost/quality evaluation is complete」 | example toml 註解 | 見 E7 | 0.10 | 0.60 | L1 |
| `[causal_extraction] enabled` | false | 「experimental … Disabled by default」 | example toml 註解 | 見 E8 | 0.20 | 0.70 | L1 |
| `[jitrl] enabled` | false | 「experimental, DEFAULT FALSE」 | `duduclaw-inference/src/config.rs:68` | 見 M20 | 0.45 | 0.75 | L1 |
| `inference.toml [router] ucci_*` (3 鍵) | None/false | 「experimental, opt-in」＋**疊在一個已承認未擬合的 gate 之上** | `CLAUDE.md:74`；`docs/features/57` untracked | 保留 0.40／簡化 0.25／淘汰 0.35 | 0.30 | **0.70** | **L1** |
| `[router] post_hoc_enabled` | false | **「α/β are unfitted defaults … `assess_response()` calibration-logging hook has zero gateway callers … Fitting is deliberately not scheduled」** | `CLAUDE.md`；最新 commit `cbdc4338` 就是把這件事誠實化 | 保留 0.35／簡化 0.25／淘汰 0.40 | **0.60** | **0.75** | **L2** |
| `[task_forward_model] enabled` / `calibration_enabled` / `held_out_gate_enabled` | 全 false | 「default off」 | `CLAUDE.md`；但記憶檔說 v1.54 「已 release＋預設開＋dashboard 開關」 | **文件與記憶互相矛盾——標為未查** | — | — | — |
| `[team] enabled` | false | 「Disabled by default — this is the only master switch」 | example toml | 保留 0.85（新功能在途） | 0.05 | 0.40 | L1 |
| `[topology_evolution] enabled` | 未查預設 | 零 `docs/features` 條目 | `handlers.rs:5982` | 保留 0.50／簡化 0.2／淘汰 0.3 | 0.30 | 0.55 | L0 |
| `[trajectory_guard]` | 未查 | 零 `docs/features` 條目，全 repo 僅 1 處引用 | grep | 未查 | — | — | — |
| `[provenance]` | 「absent/off ⇒ default」 | 零 `docs/features` 條目 | `claude_runner.rs:2083` | 未查 | — | — | — |
| `[night_engine] enabled` | false | 「Disabled by default — opt in per agent」 | `types.rs:3764` | 保留 0.6 | 0.20 | 0.50 | L1 |
| `[codrive]` | 需明確啟用 | 工具 description 自述「Requires the codrive capability to be explicitly enabled」 | `mcp.rs` | 見 M18 | 0.05 | 0.40 | L1 |

---

##### 4. 文件

###### 4.1 規模與翻譯同步

| 項目 | 數 | 狀態 |
|---|---|---|
| `docs/features/*.md`（編號 01–57） | 57＋4 附屬 | — |
| `docs/features/ja-JP/` | 59 | **缺 56、57**（兩份皆 untracked，尚未翻譯——可接受） |
| `docs/features/zh-TW/` | 59 | 同上 |
| `docs/guides/` | 41＋2 目錄 | ja-JP 39／zh-TW 39（缺 2 份） |
| `docs/README.md` 索引列 | 137 | — |
| 抽驗翻譯時效（55／53／46） | — | **src 與 ja/zh 同日 commit，同步良好** |

翻譯同步狀況**優於預期**，不是問題點。

###### 4.2 文件層面的候選

| # | 項目 | 證據 | verdict | obsolete | level | 一句話 |
|---|---|---|---|---|---|---|
| D1 | `docs/features/live-forking.md` 與 `28-live-forking.md` 並存 | 104 行 vs 284 行，前者第一行就說「For the mechanism … read 28-live-forking.md」 | 保留 0.35／簡化 0.55／淘汰 0.10 | 0.30 | L2 | 同一功能兩份文件、命名規則不一致（無編號 vs 有編號） |
| D2 | `docs/todo/TODO-rate-limit-warning-misread-as-failure.md`、`TODO-spawn-env-allowlist-fallout.md` | 兩份 header 都寫 **「✅ fixed 2026-08-17」** 仍留在 todo/ | 保留 0.15／淘汰 0.85 | **0.85** | **L2** | 已修的 TODO 留在待辦目錄＝doc rot |
| D3 | `docs/todo/TODO-bootstrap-admin-ws-deadlock.md` | 文件寫「confirmed, **not started**」；記憶檔 `project_bootstrap_admin_ws_deadlock` 記載「已修復(bc14b96e 08-20)」 | 保留 0.15／淘汰 0.85 | **0.85** | L1 | 文件與實際狀態直接矛盾 |
| D4 | `docs/guides/support-shadow-synthetic-validation.md` | **17 行、untracked**，內容是「跑一個 cargo test」，最後一段自承「This fixture is synthetic engineering evidence. It does not measure skill on real support data」 | 保留 0.40／簡化 0.40／淘汰 0.20 | 0.20 | L1 | 一條 cargo test 指令值不值得佔一個公開 guide 檔位 |
| D5 | `CLAUDE.md` 的 MCP 工具數 | 寫「~191 MCP tool schemas」，實測 245 | 淘汰（修正） 1.0 | **1.0** | **L2** | 過時敘述直接誤導 context 成本判斷 |
| D6 | `pyproject.toml:8`（PyPI 公開描述） | 寫「80+ MCP tools」；README 寫「200+」；實測 245 | 淘汰（修正） 1.0 | **1.0** | **L2** | **對外發行物的描述落後三倍**，且與同 repo README 互相矛盾 |
| D7 | `CLAUDE.md`「Python subprocess bridge for skill vetting」 | grep `vetter`／`skill_vet` 在 `crates/` **零命中**；`Command::new("python")` 在 crates 零命中 | 淘汰（修正） 0.9 | **0.80** | **L2** | 架構總覽宣稱的橋接，程式碼裡找不到呼叫端 |
| D8 | `CLAUDE.md`「LOCOMO … `cron_runner` triggered daily at 03:00 UTC」 | repo 內 grep `cron_runner` 在 `.rs/.sh/.yml/.toml` **零命中**（只有 python 檔自身） | 淘汰（修正） 0.85 | **0.80** | **L2** | 宣稱有排程觸發，repo 裡沒有任何排程指到它 |
| D9 | `docs/features/14-voice-pipeline.md` | grep `voice_pipeline` 全 crates 零命中；只有 `synthesize_speech`／`transcribe_audio` 兩個 MCP 工具 | 保留 0.5／簡化 0.3／淘汰 0.2 | 0.30 | L0 | 文件名與實作命名脫節 |
| D10 | `RlCommands`(3 葉)、`[topology_evolution]`、`[trajectory_guard]`、`[provenance]`、`[rule_induction]` | 全部**零 `docs/features` 與 `docs/guides` 條目** | 保留 0.5／簡化 0.5 | 0.25 | L1 | 有程式碼、有 config 鍵、零文件＝使用者不可達 |

---

##### 5. 發行物

| # | 項目 | 內容 | 接線 | 最後變更 | verdict | obsolete | replaceable | unused | overeng. | value | cost | risk | level | 一句話 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **P1** | `npm/`（6 包：duduclaw ＋ 5 平台 ＋ scripts/publish.sh） | 版本全部 1.65.1，齊一 | **活的發行管線** | 2026-09 | 保留 0.97 | 0.02 | 0.05 | 0.02 | 0.10 | 5 | 2 | 5 | L2 | 版本一致、`npm/scripts/publish.sh` 在位；無問題 |
| **P2** | **`python/duduclaw`（PyPI 套件 `duduclaw` 1.65.1，12,364 LOC）** | 7 個子模組 | **與 Rust binary 完全脫鉤**：`crates/` 內 grep `Command::new("python")`／`-m duduclaw`／`python/duduclaw` **全部零命中** | agents 06-16／channels **03-19**／evolution 06-20／mcp 06-20／sdk **04-15**／tools 06-20／memory_eval 07-23 | 保留 0.25／簡化 0.40／淘汰 0.35 | **0.65** | **0.70** | **0.70** | 0.45 | 2 | 4 | 3 | **L1** | 保留＝PyPI 通路本身是曝光（release.yml 有 `pypi-publish` job，每版都發）；淘汰半句＝**Rust 端零呼叫**，`channels/`（telegram/discord/line）與 Rust 十一通道功能重複、最後異動 2026-03-19，`sdk/`（account/rotator/chat/health）與 Rust account_rotator 重複、2026-04-15 |
| **P3** | **`python/duduclaw/memory_eval`（LOCOMO，7,223 LOC，占 PyPI 套件 58%）** | 14 個 .py ＋ data/ ＋ db/ ＋ tests/ | **無任何 repo 內觸發點**（grep `memory_eval` 於 `.rs/.sh/.yml` 零命中；CI `ci.yml` 只跑 `ruff check python/` ＋ `pytest tests/python/`，而 `tests/python/` 只有 `agents/` 與 `mcp/` 兩個子目錄，**不含 memory_eval**） | 2026-07-23 | 保留 0.20／簡化 0.25／淘汰 0.55 | **0.65** | 0.35 | **0.80** | 0.50 | 2 | 3 | 2 | **L1** | 保留＝記憶品質的外部量尺，是「自我演化平台」的誠實性基礎；淘汰半句＝**每一版都隨 PyPI wheel 出貨給使用者，但沒有任何人（含 CI）跑它**，`golden_qa_set.jsonl` 200 筆基準無排程消費者 |
| P4 | `python/spikes/w22-managed-agents-wiki` | 6 個 .py ＋自己的 pyproject.toml ＋ `.egg-info` | **spike，零整合** | 未查 | 保留 0.15／淘汰 0.85 | **0.75** | 0.30 | **0.85** | 0.20 | 1 | 1 | 1 | **L1** | 目錄名就叫 spikes；`.egg-info` 是 build 殘留物進了版控 |
| **P5** | **`scripts/build-release.sh`（49 行）** | 建 `dist/v<ver>` 多平台 | **零引用**（全 repo grep `build-release.sh` 只命中自己） | **2026-03-15** | 保留 0.10／淘汰 0.90 | **0.85** | **0.85** | **0.90** | 0.10 | 1 | 1 | 1 | **L2** | 被 `scripts/release.sh`(924 行) ＋ `.github/workflows/release.yml` 完全取代；最後異動半年前 |
| P6 | `scripts/build.sh`（17 行） | web build ＋ cargo build | 零 CI 引用；可能仍被人手動用 | 2026-04-02 | 保留 0.45／簡化 0.2／淘汰 0.35 | 0.40 | **0.60** | 0.45 | 0.10 | 2 | 1 | 2 | L1 | 17 行的包裝，`docs/guides/development-guide.md` 未查是否引用 |
| P7 | `scripts/smoke-{pty-pool,fork,decision-continuity}.{sh,ps1}` | 5 檔 | **零 CI 引用**，只在 CHANGELOG／TODO 歷史文件中被提到 | 2026-05-17／2026-08-04／2026-06-22 | 保留 0.55／簡化 0.25／淘汰 0.20 | 0.35 | 0.25 | **0.60** | 0.20 | 2 | 1 | 2 | L1 | 保留＝手動冒煙腳本有價值；淘汰半句＝`smoke-pty-pool`(2026-05-17) 守的是一個已標 standby 的功能 |
| P8 | `scripts/ucci_{fit,pair}.py`（158 行） | UCCI 離線擬合 | **untracked**；由 `docs/features/57`（亦 untracked）指引 | uncommitted 2026-09 | 保留 0.50／簡化 0.20／淘汰 0.30 | 0.25 | 0.20 | **0.65** | 0.35 | 2 | 2 | 2 | L1 | 保留＝要讓 UCCI 真的被擬合就需要它；淘汰半句＝它服務的 `[router] ucci_*` 三個鍵全部預設關，而它們疊在一個「deliberately not scheduled 擬合」的 post-hoc gate 之上 |
| P9 | `scripts/{install.sh,install.ps1,release.sh,dev-replace-binary.sh}` | 405／319／924／222 行 | **活的** | 2026-09-24／09-24／09-09／06-10 | 保留 0.95 | 0.03 | 0.05 | 0.03 | 0.20 | 5 | 3 | 5 | L2 | 安裝與發版主幹 |
| P10 | `scripts/box-setup/`(4) ＋ `scripts/desktop/`(6) | NAS compose／硬體檢查／macOS 簽章公證／Windows 簽章／DMG 背景 | 活的（desktop-release.yml 引用未查） | 未查 | 保留 0.85 | 0.05 | 0.10 | 0.15 | 0.15 | 4 | 2 | 4 | L0 | 桌面版發行支援 |
| P11 | `templates/`（10 目錄） | KILLSWITCH.toml／apps-script／evaluator／manufacturing／orchestrator／presets／redteam／restaurant／trading／wiki | 全部 tracked、乾淨 | wiki **2026-04-20**（最舊）、presets 2026-08-20 | 保留 0.80／簡化 0.20 | 0.15 | 0.15 | 0.25 | 0.20 | 4 | 2 | 4 | L1 | `templates/trading/` 對應的是一個已結束的 LWM 實驗（記憶檔：OEM license 10-08 到期） |
| P12 | `evals/` | README ＋ `examples/`(3 case ＋2 transcript) ＋ `_privacy/README.md` ＋ **`.DS_Store`** | 活的（`duduclaw eval` 預設讀 `./evals`） | 未查 | 保留 0.85／簡化 0.15 | 0.05 | 0.10 | 0.20 | 0.15 | 4 | 1 | 4 | L1 | `.DS_Store` 進了版控 |
| P13 | `fixtures/`（4 檔，全 untracked） | causal-extraction ×2、ccr ×2 | 服務 E6/E7/E8 | uncommitted 2026-09 | 隨 E6–E8 處置 | — | — | — | 2 | 1 | 2 | L1 | 合成 fixture；與 E6–E8 同進退 |

---

##### 6. 候選清單（通過簡報門檻者）

> 門檻：verdict 的 argmax ≠ 保留 且 confidence ≥ 0.5，**或**任一 noul p ≥ 0.6 且 level ≥ L1。
> 按「淘汰後可回收的表面／LOC」降冪。

###### C1 — `decision-*` / `ccr-*` / `causal-*` 三線合成驗證體系（E6＋E7＋E8＋P13＋D4）

- **規模**：CLI 9,536 ＋ gateway 39,865 ＋ web 8,191 ＋ `duduclaw-llm/ccr.rs` 5,185 ≈ **62,800 LOC**（占 repo Rust 946k 的 6.6%），**61 個 CLI 子命令**（占全部葉命令約 1/3），3 份 `docs/spec`、1 份 guide、4 個 fixture、7 個 dashboard 頁面群。
- **接線**：CLI live；`[ccr]`／`[causal_extraction]` 預設關；decision 線走 Decision Lab（admin-only）。
- **最強保留理由**：這是整個 repo 裡唯一一條把「AI 的宣稱」與「可重放的來源位元組」綁死的體系——policy 預先宣告、forecast 凍結、score 分離、human review 收據、來源撤銷連動。對一個賣「自我演化 AI 員工」的平台，**能證明自己沒有事後改分數**是最難買到的信任。spec 品質極高（每一段都主動標示自己證明不了什麼），這種誠實度在 repo 裡是稀有品。
- **最強淘汰／取代理由**：**三份 spec 全部自我否定其生產價值**——`support-decision-twin.md` 開頭「exploratory implementation contract … must confirm … before production recommendations」、`causal-evidence-graph.md` 開頭「The system does not yet establish a production causal effect」、`reversible-context-ccr.md` 反覆出現「remains pending / unverified / cannot attest」。61 個子命令中約 **50 個**是 demo／eval／replay／compare／screen 類，只服務合成 fixture；`ccr-compare` 的 doc 明寫「The bundled fixture validates the calculation only」。**零真實客戶資料、零真實模型量測**，而它已經吃掉 6.6% 的程式碼與 1/3 的 CLI 表面，且全部未 commit。
- **若淘汰／縮編要一併處理**：
  - CLI：`DecisionCommands`(4)／`DecisionShadowCommands`(20)／`DecisionOutcomeCommands`(22)／`DecisionPilotCommands`(2)／`CcrCommands`(7)／`CausalCommands`(6) 共 61 葉 ＋ `lib.rs` 的 6 個 flatten 掛點
  - crates：`duduclaw-cli/src/{decision_cmd,ccr_cmd,ccr_compare_cmd,ccr_eval_cmd,ccr_run_cmd,causal_cmd,causal_observational_demo}.rs`；`duduclaw-gateway/src/` 29 個 untracked ＋ 6 個 tracked（`decision_action/capture/card/message_store/notify/text` 屬**舊的決策卡功能，不可誤刪**）；`duduclaw-llm/src/ccr.rs`＋`tool_loop.rs` 的 CCR 分支
  - MCP：`duduclaw_ccr_retrieve`／`duduclaw_ccr_find`（第二套工具註冊表面）
  - config：`[ccr]`（5 鍵）、`[causal_extraction]`（4 鍵）＋ example toml 兩段註解
  - 文件：`docs/spec/{support-decision-twin,support-pilot-data-contract,reversible-context-ccr,causal-evidence-graph}.md`、`docs/guides/support-shadow-synthetic-validation.md`、`docs/todo/TODO-reversible-context-causal-simulation.md`、`docs/README.md` 13 處索引列
  - web：`{Decision*,Ccr*,Causal*}.tsx` 及其 test（8,191 行）
  - fixtures：`fixtures/` 全目錄（4 檔）
  - 測試：`c7_synthetic_shadow_harness`（gateway lib test，~90s）
  - **提醒**：整包未 commit，「淘汰」對它而言等於「不 commit」，回收成本最低的時間點就是現在。

###### C2 — `python/` PyPI 套件與 Rust 的脫鉤（P2＋P3＋P4＋D7＋D8）

- **規模**：12,364 LOC，其中 `memory_eval` 7,223（58%）、`spikes` 另計。
- **接線**：`.github/workflows/release.yml` 有 `pypi-publish` job，**每版都發**；但 `crates/` 對它**零呼叫**（三種 grep 全空）；CI 只 `ruff check python/` ＋ `pytest tests/python/`（不含 memory_eval）。
- **最強保留理由**：PyPI 是第二條發行通路，`pip install duduclaw` 的曝光與品牌價值獨立於功能；`memory_eval` 是「自我演化平台」唯一的外部記憶品質量尺，砍掉等於放棄一條誠實性證據鏈。
- **最強淘汰／取代理由**：`channels/`（2026-03-19）與 Rust 十一通道功能重複、`sdk/`（2026-04-15）與 `account_rotator` 重複、`evolution/vetter.py` 被 `CLAUDE.md` 宣稱為「Python subprocess bridge for skill vetting」但 **Rust 端零呼叫端**；`memory_eval` 被宣稱「cron_runner triggered daily at 03:00 UTC」但 **repo 內零排程指向它**。等於**每版把 12k 行沒人跑的程式碼包進 wheel 發給使用者**。
- **若淘汰要一併處理**：`pyproject.toml`（含 `80+ MCP tools` 的過時描述）、`.github/workflows/release.yml` 的 `pypi-publish` job、`ci.yml` 的 python job、`python/README.md`、`tests/python/`、`CLAUDE.md` 三處宣稱（skill vetting bridge／LOCOMO cron／python agents routing）、`README*.md` 的 PyPI 徽章與安裝段。
- **建議切法（給使用者拍板用）**：保留 `python/duduclaw/{mcp,agents}`（有 CI 測試覆蓋）＋刪 `channels/`／`sdk/`／`spikes/`＋把 `memory_eval` 移出 wheel（改成 repo-only 的評估工具，不隨 PyPI 出貨）。

###### C3 — 本地推論的六 backend 堆疊（M8）

- **規模**：`duduclaw-inference` 11,721 LOC；六 backend：llama.cpp／mistral.rs(331)／OpenAI-compat／llamafile(293)／MLX(200)／Exo(178) ＋ `ucci.rs`(370) ＋ `jitrl/`(526)。
- **接線**：`exo_cluster` 與 `mlx_bridge` 在 inference crate **外零呼叫端**；`exo_cluster.rs` 最後異動 **2026-04-12**（半年前）。
- **最強保留理由**：地端推論是「資料不出門」的商業賣點，`docs/features/45-local-model-marketplace.md`＋`53-local-models.md` 有完整使用者路徑，LocalModelsPage／MarketplacePage 已上線；六 backend 是「跨硬體都能跑」的承諾。
- **最強淘汰／取代理由**：Exo（分散式 P2P，178 行、半年未動、零外部呼叫）與 MLX（Apple 專用 Python 子行程、零外部呼叫）解的是同一件事的兩個極端角落，而主線 llama.cpp＋OpenAI-compat 已覆蓋 95% 使用情境；六 backend × 兩套路由器（post-hoc 與 UCCI）× JitRL 的組合爆炸，沒有任何一條被量測過。
- **若淘汰要一併處理**：`exo_cluster.rs`＋`manager.rs` 的 Exo 分支＋`InferenceConfig.exo`；`mlx_bridge.rs`＋`engine.rs` 的 MLX 分支＋`InferenceConfig.mlx`；`CLAUDE.md` 兩條 bullet（Exo P2P cluster／MLX bridge）；`inference.toml` 對應段落。

###### C4 — 兩層未擬合的本地路由 gate（`[router] post_hoc_enabled` ＋ `ucci_*` ＋ P8）

- **證據等級 L2**：`CLAUDE.md` 自述 post-hoc「α/β are **unfitted** defaults, so the gate is a fixed cutoff at mean logprob ≥ ln 0.5 … `assess_response()` calibration-logging hook has **zero gateway callers** … Fitting … is **deliberately not scheduled**」。最新一個 commit（`cbdc4338`）正是把這件事寫進文件。
- **最強保留理由**：級聯路由省下的是真金白銀的雲端 token；UCCI（等滲校準）是比固定門檻更正確的做法，留著它等於留著一條正確路線的入口。
- **最強淘汰／取代理由**：**UCCI 疊在一個承認自己沒擬合、且擬合工作「刻意不排程」的 gate 之上**——第二層校準器的前提是第一層有被量測，而第一層的量測 hook 零呼叫端。三個 UCCI config 鍵全預設關、擬合腳本 untracked、`docs/features/57` untracked。這是典型的「用新抽象繞過舊問題」而非修舊問題。
- **若淘汰要一併處理**：`duduclaw-inference/src/ucci.rs`(370)、`scripts/ucci_{fit,pair}.py`、`docs/features/57-ucci-calibrated-cascade.md`、`inference.toml [router]` 三鍵、`InferenceEngine::flush_shadow_observations()`、CHANGELOG 三條、`CLAUDE.md:74`。

###### C5 — PTY Pool ＋ Worker standby（`[runtime] pty_pool_enabled` / `worker_managed`＋P7 的 smoke 腳本）

- **證據等級 L2**：`CLAUDE.md` 自述「**Status (2026-07) — kept as standby, default off**: Anthropic's 2026-06-15 programmatic-usage split … was **paused on the day**; `claude -p` still works … PTY pool is **not required**」，並且「**Known limitation**: pool sessions … **no conversation dimension** … bleeds context」。
- **最強保留理由**：這是一份**對已知外部風險的保險**——Anthropic 隨時可能重啟拆分，屆時 PTY pool 是唯一能讓 OAuth 訂閱帳號繼續跑的路徑，重寫成本遠高於維護成本。兩個 crate（`duduclaw-cli-runtime`＋`duduclaw-cli-worker`）是完整的、已通過 smoke 的。
- **最強淘汰／取代理由**：保險已經**過期 15 個月未更新**（`smoke-pty-pool.sh` 最後異動 2026-05-17），而且**這份保險本身是壞的**——跨對話洩漏是資料外洩等級的缺陷，真要啟用時不能直接開。等於「留著一個不能用的備案」。
- **若處置要一併處理**：兩個 crate、`gateway/{pty_runtime,worker_supervisor,runtime_status}.rs`、`GET /api/runtime/status`、`pty_pool_*` 七個 Prometheus 指標、`scripts/smoke-pty-pool.{sh,ps1}`、`docs/features/27-pty-pool-runtime.md`＋兩份翻譯、`CLAUDE.md` 該 bullet、`[runtime]` 兩鍵、`channel_reply`／`claude_runner` 兩處分支。
- **建議（不越權，僅列選項）**：A) 維持 standby 但**先修跨對話鍵**（加 conversation 維度），讓保險真的可用；B) 明確降級為 `docs/adr` 的一筆決策紀錄並移除程式碼；C) 現狀不動但在 `CLAUDE.md` 標註「啟用前必須先修 conversation 鍵」。

###### C6 — 已修卻仍在待辦的文件 ＋ 三處與程式碼矛盾的架構敘述（D2＋D3＋D5＋D6＋D7＋D8）

- **L2 實錘清單**：
  1. `docs/todo/TODO-rate-limit-warning-misread-as-failure.md` header「✅ fixed 2026-08-17」
  2. `docs/todo/TODO-spawn-env-allowlist-fallout.md` header「✅ fixed 2026-08-17」
  3. `docs/todo/TODO-bootstrap-admin-ws-deadlock.md` 寫「not started」，實際已於 `bc14b96e`(08-20) 修復
  4. `CLAUDE.md`「~191 MCP tool schemas」→ 實際 245
  5. `pyproject.toml`「80+ MCP tools」→ 實際 245（且與同 repo README 的「200+」矛盾，**這是對外發行物**）
  6. `CLAUDE.md`「Python subprocess bridge for skill vetting」→ Rust 零呼叫端
  7. `CLAUDE.md`「LOCOMO cron_runner triggered daily at 03:00 UTC」→ repo 內零排程
- **最強保留理由**：無。這些不是功能，是敘述錯誤。
- **最強淘汰理由**：專案自己的 `rules/readme-and-docs.md` 第 10 條寫「**過時文件比沒有文件更糟**——它主動誤導，且讀者一旦發現文件不可信就永遠不再讀文件」。第 5 項還會出現在 PyPI 公開頁面上。
- **處理成本**：極低（7 處字串／2 個檔案搬到 `docs/adr` 或刪除）。**這是本次盤點投入產出比最高的一項。**

###### C7 — `config/duduclaw.example.toml` 涵蓋率 20%（§3.1）

- **證據**：範例檔 253 行涵蓋約 10 個段落；程式碼與文件實際引用 50+ 個頂層段落。
- **最強保留理由**：範例檔刻意只放「一般使用者會動的」，把進階段落留給文件，是合理的資訊分層設計。
- **最強淘汰／取代理由**：`[tick]`（常駐感知）、`[task_forward_model]`（前向模型）、`[belief]`、`[goal_loop]`、`[redaction]`、`[mail]`、`[limits]` 這些**有專屬 `docs/features` 文件的一級功能**，使用者從範例檔完全看不到它們存在——資訊分層若成立，範例檔至少要有一段「其他可用段落見 docs/…」的導航，目前沒有。
- **若處理要一併涵蓋**：範例檔補註解式導航；或在 `docs/README.md` 加一張 config 段落總表。

###### C8 — 低價值／零文件的單件表面（E13 RlCommands、E14 ClearHoldoutRotation、E15 cost tool-loop、E16 memory bench、M20 jitrl、M21 log_mood、P4 spikes、P5 build-release.sh）

八項合計規模小（~1,500 LOC ＋ 9 個 CLI 葉 ＋ 2 個 MCP 工具），但都是**零文件或自承一次性**的表面：

| 項目 | 最強保留 | 最強淘汰 | level |
|---|---|---|---|
| `duduclaw rl *`(3) | RL 軌跡是微調素材 | 零 docs、`duduclaw docs` 打不到 | L0 |
| `evolution clear-holdout-rotation` | 補一個真的會卡住的旗標 | doc 自承「had zero call sites until this command」——為死碼補 CLI | **L2** |
| `cost tool-loop` | 重跑 Code Mode 量測閘 | 量完 INSUFFICIENT_DATA，Code Mode 未立案 | L1 |
| `memory bench` | 分割決策重測 | 一次性決策工具長駐產品 CLI | L0 |
| `jitrl_feedback` ＋ `jitrl/`(526 LOC) | JitRL 論文路線入口 | 預設關＋零文件＋零 dashboard | **L1** |
| `log_mood` | — | 描述四個字；與 `user_profile_record` 重疊 | L0 |
| `python/spikes/`(6 檔＋egg-info) | — | 目錄名叫 spikes；build 殘留進版控 | **L1** |
| `scripts/build-release.sh` | — | 零引用、半年未動、被 release.sh 完全取代 | **L2** |

---

##### 7. 我沒盤到的範圍（誠實列出）

1. **未跑任何 build／test／CLI**，全部結論來自靜態讀取。「零呼叫端」是 grep 結果，不是連結器實錘——動態分派（trait object、字串查表分派）可能漏抓。`mcp_dispatch.rs` 的分派表我只抽樣檢查，未逐一對 245 個工具核對是否都有分派臂。
2. **`web/` dashboard RPC 表面未盤**（120,945 行 TS/TSX、163 個 page 檔）。「是否有 dashboard 對應」欄位是靠 page 檔名推斷，**未驗證 RPC 是否真的接到後端**。此欄一律應視為 L0。
3. **`agent.toml` 段落未逐欄盤點**。`agent_toml.rs`(1,076 行) ＋ `types.rs`(5,351 行、53 個 struct) 我只抽驗了 `PtcConfig`／`NightEngineConfig`／`TeamConfig`／`CapabilitiesConfig` 的預設值。記憶檔提到「影子 reader 全貌 62 處/16 檔」——我沒去核對現況。
4. **`[task_forward_model]` 的預設值有矛盾**：`CLAUDE.md` 說 default off，記憶檔 `project_v154_calibrated_forward_model` 說「已 release＋**預設開**＋dashboard 開關」。我沒有讀 `Default` impl 實錘，**標為未查**，不下判斷。
5. **未盤 `commercial/` 與 `research/`**（L3 gitignored）。C1/C4/C5 的設計文件都在那裡，我只能從公開側推斷其狀態，可能錯過「這條線已經被內部叫停／或剛被拍板加碼」的關鍵資訊。
6. **未盤 `docs/rfc`(6)／`docs/adr`(7) 的逐份時效**，只列了檔名。RFC-21/22/24/26/27 與 ADR-002~007 是否仍與程式碼一致，未查。
7. **未盤 Yocto／OS 線的對外表面**（`meta-*` layer、`openembedded-core/`），該線已於 2026-09 拆到 `DuDuClaw-OS` repo，但 workspace 仍留有目錄。
8. **CLI 葉命令的「零測試」判定不完整**：我只抓了 `decision_cmd.rs`／`ccr_compare_cmd.rs`／`causal_cmd.rs` 的 `#[test]` 數，其餘 ~140 個葉命令的測試覆蓋未逐一查。
9. **CLI 葉數 201 vs 任務描述的 114**：差異已列，但我沒有實跑 `duduclaw --help` 核對 clap 實際產出的命令樹，所以「約 180 個可打葉命令」是推算值，不是實測值。
10. **翻譯內容一致性未查**，只比對了檔案清單與 git commit 日期；ja-JP／zh-TW 的**內文**是否與 src 同步（而非只是同時 commit）未驗證。

---

## 3. 盤點者未涵蓋範圍（彙整）

- 全部為靜態閱讀：未跑 build／test／CLI，「零呼叫端」是 grep 下界（動態分派可能漏抓），「live」不代表跑得起來。
- 未查生產使用遙測：所有「無人開」來自程式預設值與範例設定，不是實機盤點；**你知道實際客戶用哪些通道／功能，那份知識應直接覆蓋 unused 機率**。
- 未盤：`commercial/`／`research/`（L3）內容、web dashboard RPC 是否真接後端（L0）、`agent.toml` 逐欄、premium／expert 劇本內容品質、`evolution_events` 30+ 事件實際 emit 比例、ja-JP／zh-TW 翻譯內文一致性、外部工具替代性（LiteLLM／Ollama 之類）未做調研。
- 領域邊界交接：`decision_*`／`ccr_*`／`causal_*` 由 B、E 兩份涵蓋（D 未盤）；OS 線 crate 由 C 涵蓋；`fault_attribution.rs`（untracked、預設 true、影響 playbook 學分）由 D 涵蓋。
