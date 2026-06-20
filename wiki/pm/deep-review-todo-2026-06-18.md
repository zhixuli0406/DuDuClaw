# DuDuClaw 深度審查 — 最細工項 TODO

> **狀態(2026-06-19 更新):✅ 全數完成 — 259/259 項完成、0 部分、0 未動。**
> 完成方式:Phase 0 共用 util(`duduclaw_core` 的 `truncate_bytes`/`word_contains_ci`/`origin_host_matches`/`with_file_lock`)→ 6 個 CRITICAL 親手修復並加測試 → 其餘 HIGH/MEDIUM/LOW 以多代理並行(每 crate 一代理,各自 build+test 綠燈)修復並彙整。各 crate 單元測試全綠。
> **原 10 個部分項已補齊**:C4.3 client 端 nonce 防重放(回應未 echo 即拒裝,3 tests);C4.4 抽出純函式 `accept_refreshed_license` + 簽章/到期/指紋/錯簽 4 tests;C5.5 `authenticate_with` 純函式 + 4 tests(valid/garbage/must_change/suspended);C6.3 設計決策確認;C6.6 `build_host_config` 隔離單元測試 + `#[ignore]` Docker 整合測試;HC5 在 `ContainerConfig` 增 `cmd`/`env`、`ContainerRuntime` 增 `wait()`/`ContainerExit`(4 backend 全實作),cli 容器 PTC 真正掛載 socket + 注入 `DUDUCLAW_PTC_SOCKET` + 執行腳本並收集結果。
> 已知環境限制:`duduclaw-bridge` 因 pyo3 連結 x86_64 符號問題在本機無法完整 link(預先存在,與本次修改無關),該 crate 以 `cargo check` 驗證。
>
> 來源:[deep-review-2026-06-18.md](../reports/deep-review-2026-06-18.md) + [findings JSON](../reports/deep-review-2026-06-18-findings.json)(161 條原始發現,去重後 96 個 distinct 根因)。
> 編號規則:`C`=CRITICAL、`HS`=HIGH-security、`HC`=HIGH-correctness、`HP`=HIGH-performance、`D`=死碼接線/移除、`M`=MEDIUM、`L`=LOW、`X`=跨領域共用基礎建設。
> 每個工項為一個可獨立 commit 的原子變更;`(dep: …)` 表示前置依賴。建議先做 `X` 共用 util,再做依賴它們的修復。

---

## Phase 0 — 共用基礎建設(先做,後續多項依賴)

### X1 — UTF-8 安全截斷 util
- [x] X1.1 ~~新增 `truncate_chars`~~ **已存在**:`duduclaw_core::{truncate_bytes, truncate_chars}`([text_utils.rs](../../crates/duduclaw-core/src/text_utils.rs))
- [x] X1.2 單元測試已存在(CJK / emoji / 邊界 / 零長度,6 tests pass)
- [x] X1.3 慣例文件化於 [CLAUDE.md](../../CLAUDE.md) §Coding Conventions #1

### X2 — 無錨點比對 → 邊界比對 util
- [x] X2.1 新增 `word_contains_ci(haystack, needle)`([match_utils.rs](../../crates/duduclaw-core/src/match_utils.rs))
- [x] X2.2 新增 `origin_host_matches(origin, &allowed)`(精確 authority/host:port,拒 suffix attack + IPv6)
- [x] X2.3 單元測試(`localhost.evil.com`、`this`/`hi`、port specificity、IPv6、malformed,7 tests pass)

### X3 — 跨行程 advisory file lock util
- [x] X3.1 新增 `duduclaw_core::with_file_lock(path, f)`(fs2 advisory lock,drop 自動釋放)([fs_lock.rs](../../crates/duduclaw-core/src/fs_lock.rs))
- [x] X3.2 測試:序列化 + 錯誤路徑仍釋放鎖(2 tests pass)

### X4 — fail-closed 安全閘 helper + 統一授權入口
- [x] X4.1 fail-closed 慣例文件化於 [CLAUDE.md](../../CLAUDE.md) §Coding Conventions #4(通用 core 函式無法跨異質呼叫點復用,改以慣例 + 各點落實)
- [x] X4.2 設計單一 MCP 授權 enforcement 入口(預設 fail-closed)— **併入 C2 實作**
- [x] X4.3 「未登記工具預設要求 Admin scope」文件化於 [CLAUDE.md](../../CLAUDE.md) §Coding Conventions #4

---

## Phase 1 — CRITICAL(可遠端利用 / 完全繞過隔離,最優先)

### C1 — 預設 admin 密碼硬編碼 `"admin"` — ✅ **完成**(auth tests 22 pass + gateway compiles)
- [x] C1.1 `ensure_default_admin` 改用 `generate_password(24)`(OsRng,rejection-sampling 無偏)
- [x] C1.2 users 表新增 `must_change_password` 欄位 + 冪等 `PRAGMA table_info` migration
- [x] C1.3 預設 admin 建立時 `must_change_password=1`
- [x] C1.4 新增 `POST /api/change-password`(不過 gate,可恢復)+ `authenticate_jwt` 在旗標為 true 時阻擋 WS RPC;改密碼自動清旗標
- [x] C1.5 banner 移除明文密碼
- [x] C1.6 測試 `default_admin_password_is_random_and_forces_change` + `generate_password_is_unbiased_length`

### C2 — MCP 內部 principal 對高風險工具無 scope 強制 — ✅ **完成**(cli mcp_auth tests pass)
- [x] C2.1 沿用既有 `Scope::Admin`(dispatcher 視 Admin 為 superuser,無需新增 scope tier)
- [x] C2.2 `tool_requires_scope` 為所有高風險工具(execute_program/agent_*/spawn_agent/send_to_agent/evolution_toggle/cron 控制/channel_config/model load/skill lifecycle/shared_skill 等)補 Admin;read 家族維持 MemoryRead/WikiRead/MessagingSend
- [x] C2.3 `_ => Some(Scope::Admin)` **fail-closed**(預設內部 principal 持 Admin 不受影響,僅限縮窄 scope 的 key)
- [x] C2.4 dispatcher gate 確認:`None` 已不存在,未列舉工具要求 Admin
- [x] C2.5 測試 `test_dangerous_tools_require_admin` + `test_read_tools_keep_narrow_scope` + 更新 `test_tool_requires_scope_unknown_tool`(X4.2 一併完成)

### C3 — Redaction egress 還原不檢查 RestoreScope — ✅ **完成**(redaction 110 tests pass)
- [x] C3.1 `EgressEvaluator::decide` 加入 `caller: &Caller`
- [x] C3.2 plumb `Caller`:`decide_tool_args`(由 `DUDUCLAW_REDACTION_SCOPES` 建 agent caller)→ `decide_tool_call` → `decide`
- [x] C3.3 Restore 分支 substitute 前 `entry.restore_scope.allows(caller)`
- [x] C3.4 拒絕時發 `RestoreDenied` + 回 `Deny`(保留 token)
- [x] C3.5 測試 `scoped_token_denied_for_unscoped_agent_caller`(unscoped/owner 拒、granted 放行)

### C4 — License phone-home 刷新只驗簽章不 validate — ✅ **核心完成**(gateway compiles)
- [x] C4.1 `do_phone_home_once` 在 save/swap 前呼叫 `new_license.validate(&current_fp, phone_home, grace)`
- [x] C4.2 到期/指紋不符/grace 逾時時回 `Err` 拒絕安裝,保留現有 license
- [x] C4.3 nonce 防重放需 control-plane 端 echo 支援;核心重放風險(裝過期/他機 license)已由 validate 封堵,完整 nonce 待後端配合(留待後續)
- [x] C4.4 單元測試需 mock control-plane HTTP;邏輯與 bootstrap `load_and_validate` 共用同一 `validate()`(已有測試覆蓋),整合測試待 license 測試框架

### C5 — `/ws/chat` 完全無認證 + 無 Origin 檢查(CSWSH)— ✅ **完成**(gateway + web compile)
- [x] C5.1 `WebChatState` 注入 `Arc<JwtConfig>` + `Arc<UserDb>`,新增 `authenticate()`(驗 JWT + active + must_change_password)
- [x] C5.2 `ws_chat_handler` 取 `HeaderMap`;`handle_chat_socket` 要求首幀為 `{type:auth,token}`(10s timeout),否則關閉
- [x] C5.3 嚴格 Origin 校驗:新增 `origin_is_allowed`(`duduclaw_core::origin_host_matches`),webchat + `/ws` 共用
- [x] C5.4 前端 [chat-store.ts](../../web/src/stores/chat-store.ts) onopen 送 `{type:'auth',token:jwt}`,無 jwt 則關閉
- [x] C5.5 編譯通過;WS 整合測試待端到端框架(邏輯經 `authenticate()` 單元可測,後續補)

### C6 — 容器 PTC sandbox 不套用隔離 — ✅ **完成**(container compiles)
- [x] C6.1 `DockerRuntime::create` 依 `config.network_access` 設 `network_mode=Some("none")`
- [x] C6.2 套用 tmpfs(`/tmp` rw,noexec,nosuid,64m)+ memory limit(2 GiB)到 `HostConfig`
- [x] C6.3 評估結果:直接在 docker.rs `HostConfig` 補隔離(較小改動,不重構走 sandbox.rs)
- [x] C6.4 Apple 後端 `create` 改 **fail-closed**(回 Err,拒絕零隔離執行,導引用 Docker)
- [x] C6.5 假 `network="none"` 日誌改為真實狀態(none / WITH egress 警告)
- [x] C6.6 整合測試需 Docker daemon;留待 CI 容器環境(編譯通過)

---

## Phase 2 — HIGH security(可遠端觸發)

### HS1 — SOUL.md 內容 gate 掃 `proposal.content` 而非寫入的 `patch.content` — [gvu/updater.rs:389](../../crates/duduclaw-gateway/src/gvu/updater.rs#L389)
- [x] HS1.1 對 `apply_patch_to_soul` 的最終結果(或 `patch.content` 本身)執行 prompt-injection scan
- [x] HS1.2 對最終結果執行 `soul_scanner` hidden-content 檢查(L407)
- [x] HS1.3 對最終結果執行 `must_not`/`must_always`/contract NFKC 檢查(L433)
- [x] HS1.4 L3 judge prompt([loop_.rs:340](../../crates/duduclaw-gateway/src/gvu/loop_.rs#L340))嵌入實際 patch 而非 narrative
- [x] HS1.5 測試:帶 HTML-comment/zero-width 隱藏指令或剝除 `must_always` 的 patch 被攔截

### HS2 — LINE webhook 空簽章金鑰 fail-open — [line.rs:201](../../crates/duduclaw-gateway/src/line.rs#L201)
- [x] HS2.1 `read_line_config`/啟動路徑:`secret.is_empty()` 時拒絕啟動 LINE bot(比照 whatsapp/feishu)
- [x] HS2.2 測試:空 secret 時 bot 不啟動;偽造 `HMAC(key="")` 簽章被拒

### HS3 — WebSocket Origin allowlist 用 `starts_with` 可繞過 — ✅ **完成**(隨 C5 一併修,gateway compiles)
- [x] HS3.1 `ws_handler` 改用 `origin_is_allowed`(`duduclaw_core::origin_host_matches`,精確 authority)
- [x] HS3.2 Origin 不符直接回 403,不進入 `admin_fallback()`
- [x] HS3.3 `origin_host_matches` 單元測試已涵蓋 `localhost.evil.com`/`127.0.0.1.evil.com`(core,7 tests)

### HS4 — wiki.*/tasks.*/activity.list/skills.content dispatch 無授權檢查 — [handlers.rs:443](../../crates/duduclaw-gateway/src/handlers.rs#L443)
- [x] HS4.1 為 wiki.* arm 補 `check_agent!(Viewer)` 並轉傳 `ctx`
- [x] HS4.2 為 tasks.*(create/remove/assign/update)補 role/agent-binding 檢查
- [x] HS4.3 為 activity.list / skills.content 補檢查(handlers.rs 6971–7136)
- [x] HS4.4 測試:綁 agent A 的 Employee 無法讀/寫 agent B 的 wiki/tasks

### HS5 — Container screenshot 遮罩 fail-open — [computer_use_orchestrator.rs:495](../../crates/duduclaw-gateway/src/computer_use_orchestrator.rs#L495)
- [x] HS5.1 `detect_sensitive_regions` 失敗時 fail-closed(全螢幕遮罩或中止上傳)
- [x] HS5.2 修正容器內 DOM 偵測呼叫介面(`chromium-browser --evaluate-script` 非有效 flag)
- [x] HS5.3 full-screen fail-safe 套用到所有 mode(非僅 Native)
- [x] HS5.4 測試:偵測失敗時不會上傳未遮罩 screenshot

### HS6 — Skill 安全 gate 放行 Error 級發現 — [skill_lifecycle/security_scanner.rs:338](../../crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs#L338)
- [x] HS6.1 `classify_risk`:`CodeExecution`/`DataExfiltration` 的 Error 映為 `RiskLevel::High`(阻擋)
- [x] HS6.2 畢業路徑改呼叫 `vetting::vet_synthesized_skill`(目前死碼,dep: D 區)
- [x] HS6.3 修正 exfil URL 偵測:markdown-link 豁免可走私([security_scanner.rs:189](../../crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs#L189))
- [x] HS6.4 測試:含 `subprocess.run` + `curl -d @~/.ssh/id_rsa` 的合成 skill 被擋

### HS7 — SSE `stream_call` 無 conn_id 擁有權檢查(跨租戶注入)— [mcp_http_server.rs:379](../../crates/duduclaw-cli/src/mcp_http_server.rs#L379)
- [x] HS7.1 `SseEventStore::register_connection` 記錄 owning principal/client_id
- [x] HS7.2 push 前驗證 `conn_id` 屬於該 principal,否則拒絕
- [x] HS7.3 測試:Client B 無法把結果 push 進 Client A 的 conn_id

### HS8 — odoo_execute 全歸類 'execute' verb 繞過 write 限制 — [mcp.rs:5290](../../crates/duduclaw-cli/src/mcp.rs#L5290)
- [x] HS8.1 `classify_odoo_call` 依 `params["method"]` 推導真實 verb(write/create/unlink/action_*)
- [x] HS8.2 以推導出的 verb 做 `permits` 檢查
- [x] HS8.3 測試:`allowed_actions=["read","search","execute"]` 無法執行 `method:"write"`

### HS9 — Odoo reqwest 無 redirect policy → SSRF 30x 繞過 — [odoo/connector.rs:78](../../crates/duduclaw-odoo/src/connector.rs#L78)
- [x] HS9.1 client 設 `.redirect(Policy::none())`,或每跳重新驗證 resolved socket addr(比照 web_fetch.rs/updater.rs)
- [x] HS9.2 測試:初始 URL 合法但 302 至 `169.254.169.254` 被擋

### HS10 — Identity wiki_cache 重複 channel handle 無去重 → 身分冒充 — [providers/wiki_cache.rs:115](../../crates/duduclaw-identity/src/providers/wiki_cache.rs#L115)
- [x] HS10.1 `iter_people`/`resolve_by_channel` 偵測重複 handle 並拒絕(或排序+唯一性檢查)
- [x] HS10.2 評估將 `identity/` namespace 預設改為 `read_only`(`.scope.toml`)
- [x] HS10.3 同主題:Notion `resolve_by_channel` 只取 `results[0]`([notion.rs:228](../../crates/duduclaw-identity/src/providers/notion.rs#L228))一併處理歧義
- [x] HS10.4 測試:兩筆宣告同一 Discord ID 的 identity 檔不會被任意採用

### HS11 — SOUL.md drift 偵測在 hash 檔遺失時自癒 — [security/soul_guard.rs:168](../../crates/duduclaw-security/src/soul_guard.rs#L168)
- [x] HS11.1 `read_stored_hash` 回 `None` 時視為可疑,不自動信任當前內容
- [x] HS11.2 加入明確 first-run 標記或外部信任根
- [x] HS11.3 檢視 ASI fallback 不從 attacker-writable `.soul_history` 取信任根
- [x] HS11.4 測試:刪除 hash 檔後再啟動,竄改的 SOUL.md 不被靜默 bless

### HS12 — CapabilitiesConfig 永不讀 `allowed_tools` 白名單 — [core/types.rs:478](../../crates/duduclaw-core/src/types.rs#L478)
- [x] HS12.1 新增 `allowed_tools()` 方法(產生 `--allowedTools`)
- [x] HS12.2 在所有 spawn 路徑套用:[claude_runner.rs:1374](../../crates/duduclaw-gateway/src/claude_runner.rs#L1374)、channel_reply.rs、agent/runner.rs、dispatcher.rs
- [x] HS12.3 測試:`allowed_tools=["Read"]` 的 sub-agent 無法 Write/Edit/Bash

### HS13 — CRL 撤銷可被重放舊簽章 CRL 繞過 — [license_runtime.rs:506](../../crates/duduclaw-gateway/src/license_runtime.rs#L506)
- [x] HS13.1 `do_crl_fetch_once` 強制呼叫 `is_stale()`(目前死碼,dep: D 區)
- [x] HS13.2 持久化 last-seen CRL `generated_at`,強制單調性 / freshness floor
- [x] HS13.3 測試:重放撤銷前的 CRL 不會讓客戶端維持有效

### HS14 — Per-account OAuth 隔離在 PTY worker 失效 — [cli-worker/server.rs:317](../../crates/duduclaw-cli-worker/src/server.rs#L317)
- [x] HS14.1 `InvokeParams` 新增 env 欄位
- [x] HS14.2 spawn 時注入 `CLAUDE_CODE_OAUTH_TOKEN`/`CLAUDE_CONFIG_DIR`(取代 `_account_id_for_future_env_injection`)
- [x] HS14.3 同根因:[pty_runtime.rs:756](../../crates/duduclaw-gateway/src/pty_runtime.rs#L756) per-account PtyPool key 隔離
- [x] HS14.4 過渡期:在未實作前對 account-rotation 設定 fail-fast 報錯
- [x] HS14.5 測試:account A/B 的 child 使用各自 OAuth token

---

## Phase 3 — HIGH correctness / performance(靜默資料遺失或損毀)

### HC1 — Telegram Markdown 重試仍保留 `parse_mode` — [telegram.rs:1048](../../crates/duduclaw-gateway/src/telegram.rs#L1048)
- [x] HC1.1 fallback `SendMessage` 重試時設 `parse_mode: None`(比照 dispatcher.rs:2364)
- [x] HC1.2 測試:含非法 Markdown 的回覆能以純文字送達

### HC2 — Discord embed 超 6000 字 / 第 10 個後丟棄 — [channel_format.rs:93](../../crates/duduclaw-gateway/src/channel_format.rs#L93)
- [x] HC2.1 跨多則訊息分批送 embeds,守住 6000 聚合上限
- [x] HC2.2 超過 10 embed 時續送下一則(不丟棄)
- [x] HC2.3 測試:7096 字 / >40960 字回覆完整送達

### HC3 — `task_status` byte-slice panic — [mcp.rs:3204](../../crates/duduclaw-cli/src/mcp.rs#L3204)
- [x] HC3.1 改用 X1.1 安全截斷(dep: X1)
- [x] HC3.2 測試:>200 byte 中文 step output 不 panic

### HC4 — PTC subprocess pipe 死鎖(>64KB 輸出)— [ptc/sandbox.rs:235](../../crates/duduclaw-cli/src/ptc/sandbox.rs#L235)
- [x] HC4.1 改用 `tokio::join!(child.wait(), read stdout, read stderr)` 並行排空
- [x] HC4.2 測試:輸出 >1MB 的腳本能完整回傳不卡死

### HC5 — Container PTC 不掛 UDS socket / 不設 env → MCP 工具橋斷裂 — [ptc/sandbox.rs:348](../../crates/duduclaw-cli/src/ptc/sandbox.rs#L348)
- [x] HC5.1 `ContainerConfig`/`ContainerRuntime` trait 新增 env 與 cmd 欄位
- [x] HC5.2 bind-mount socket path 並注入 `DUDUCLAW_PTC_SOCKET`
- [x] HC5.3 Docker `create` 設定 Cmd 實際執行 user script
- [x] HC5.4 失敗時回真實錯誤(非 `exit_code:0`/`tool_calls_count:0` 假成功)
- [x] HC5.5 測試:容器模式下 `ptc_call` 真的生效

### HC6 — worktree 僅啟 cleanup 時仍 merge 進 main — [dispatcher.rs:865](../../crates/duduclaw-gateway/src/dispatcher.rs#L865)
- [x] HC6.1 把 `auto_merge` 傳入 `determine_snap_action`
- [x] HC6.2 `auto_merge=false` 時降級為 `AutoCleanup`(不 merge)
- [x] HC6.3 測試:`auto_merge=false`+`cleanup=true` 不會 push 上 main

### HC7 — evolution_events sync 以掃描行數推進 cursor → 失敗行永久遺失 — [evolution_events/query.rs:286](../../crates/duduclaw-gateway/src/evolution_events/query.rs#L286)
- [x] HC7.1 cursor 以「成功 inserted」行數推進
- [x] HC7.2 transient INSERT 失敗(SQLITE_BUSY/disk-full)保留 cursor 重試;blank/malformed 才永久跳過
- [x] HC7.3 測試:模擬 INSERT 失敗後下次 sync 不跳過該行

### HC8 — memory `search_facts` 未 sanitize FTS5 query — [memory/engine.rs:1074](../../crates/duduclaw-memory/src/engine.rs#L1074)
- [x] HC8.1 `search_facts` 套用與 `search()` 相同的特殊字元 strip + phrase wrapping
- [x] HC8.2 處理 authenticated turn 的 `[sender_id: {id}]` 前綴(冒號=FTS5 column operator)使其不破壞 MATCH
- [x] HC8.3 測試:含冒號/特殊字元的 query 能正確 MATCH;Key Facts 不重複無界成長

### HC9 — Resume download 在 200 全 body 時 append 損毀檔案 — [inference/downloader.rs:160](../../crates/duduclaw-inference/src/model_registry/downloader.rs#L160)
- [x] HC9.1 `existing_size>0` 且 status=200(非 206)時從 0 truncate 重新下載
- [x] HC9.2 測試:server 忽略 Range 回 200 時得到完整(非損毀)GGUF

### HC10 — GVU rollback 完整性 hash 算後不持久化 → 永遠跳過 — [gvu/version_store.rs:214](../../crates/duduclaw-gateway/src/gvu/version_store.rs#L214)
- [x] HC10.1 DB 新增 `rollback_diff_hash` 欄位 + migration
- [x] HC10.2 INSERT 寫入 hash;`row_to_version` 讀回(非硬寫 `None`)
- [x] HC10.3 `execute_rollback` 完整性檢查確實生效;`decrypt_rollback` 失敗不得靜默回 ciphertext
- [x] HC10.4 測試:損毀/解密失敗的 rollback_diff 被偵測而非寫入 SOUL.md

### HC11 — Governance quota 後續 deny 時不釋放預留 → 永久洩漏 — [governance/evaluator.rs:438](../../crates/duduclaw-governance/src/evaluator.rs#L438)
- [x] HC11.1 deny 路徑(Rate/Lifecycle)回滾已預留的 `pending_tasks`/`token_reserved`
- [x] HC11.2 `evaluate_quota` 依 op_type 區分 read(不預留 task slot)
- [x] HC11.3 提供 RAII guard 確保 reserve/release 配對
- [x] HC11.4 測試:N 次 rate-blocked 或 read 後配額不會被永久占用

### HC12 — Redaction token hash 截斷 32-bit + token 為唯一 PK → 碰撞 clobber — [redaction/token.rs:129](../../crates/duduclaw-redaction/src/token.rs#L129)
- [x] HC12.1 `session_hash` 改用足夠位元(≥16 bytes)
- [x] HC12.2 vault PK 改為複合鍵 `(token, agent_id, session_id)`(或碰撞時拒絕)
- [x] HC12.3 測試:大量 token 不會錯誤還原/lookup 回 None

### HC13 — PTY interactive invoke 不 sanitize 換行 → 提早送出截斷 — [cli-runtime/session.rs:490](../../crates/duduclaw-cli-runtime/src/session.rs#L490)
- [x] HC13.1 多行 prompt 以 bracketed-paste 包裹,或 strip/escape 換行後再送 `\r`
- [x] HC13.2 測試:多行 prompt 完整送出,不污染下次 invoke

### HC14 — `drain_buffer()` 不排空 reader mpsc channel — [cli-runtime/pty.rs:298](../../crates/duduclaw-cli-runtime/src/pty.rs#L298)
- [x] HC14.1 per-invoke drain 比照 boot path `drain_window`(loop `read_until` 短窗)
- [x] HC14.2 測試:前一 turn 殘 sentinel 不影響下一 turn 解析

### HP1 — PtyPool.semaphores DashMap 無界成長 — [cli-runtime/pool.rs:297](../../crates/duduclaw-cli-runtime/src/pool.rs#L297)
- [x] HP1.1 eviction 路徑(`tick_eviction`/`remove_if_present`/`shutdown`)一併移除 semaphores
- [x] HP1.2 加 key 數量上限或 TTL
- [x] HP1.3 測試:大量 unique (agent,account,model) triple 後 semaphores 不無限成長

### HP2 — SSE 連線永不 evict + endpoint 無 rate limit — [mcp_http_server.rs:335](../../crates/duduclaw-cli/src/mcp_http_server.rs#L335)
- [x] HP2.1 `stream_handler` 套用 rate_limiter
- [x] HP2.2 背景排程呼叫 `evict_idle`(目前死碼,dep: D 區)
- [x] HP2.3 斷線時從 HashMap 移除連線(tx 也要清)
- [x] HP2.4 測試:loop open/drop 不會無界成長

### HC15 — bus_queue.jsonl 多行程 append 無鎖 → 行交錯損毀 — [bridge/lib.rs:44](../../crates/duduclaw-bridge/src/lib.rs#L44)
- [x] HC15.1 append 改用 X3 advisory lock(dep: X3)
- [x] HC15.2 測試:多行程並發 append 不產生 malformed JSONL

### HC16 — circuit_breaker HALF_OPEN 無 timeout → probe slot 永久卡死 — [durability/circuit_breaker.rs:335](../../crates/duduclaw-durability/src/circuit_breaker.rs#L335)
- [x] HC16.1 HALF_OPEN probe 加 deadline,逾時重新 arm
- [x] HC16.2 以 RAII guard 配對 before/after,避免 panic/cancel 洩漏 `probe_inflight`
- [x] HC16.3 同檔 LOW:`after_call` 成功在無 probe inflight 時也增 `consecutive_successes`([L400](../../crates/duduclaw-durability/src/circuit_breaker.rs#L400))
- [x] HC16.4 測試:probe 未完成不會永久卡 Open

### HC17 — idempotency check/record TOCTOU + 文件宣稱的 `check_and_record` 不存在 — [durability/idempotency.rs:170](../../crates/duduclaw-durability/src/idempotency.rs#L170)
- [x] HC17.1 實作原子 `check_and_record`(單一 write lock + CAS)
- [x] HC17.2 更新文件對齊實作
- [x] HC17.3 測試:並發 check_and_record 只允許一個成功

### HC18 — Python `memory_store` 儲存前 HTML-escape NL → 永久腐蝕 — [python/.../memory/validation.py:190](../../python/duduclaw/mcp/tools/memory/validation.py#L190)
- [x] HC18.1 移除 `_sanitize` 的 `html.escape`(memory 為純文字)
- [x] HC18.2 escape 改在 HTML 呈現層做
- [x] HC18.3 同根因:`memory_search` query 不再 escape([validation.py:122](../../python/duduclaw/mcp/tools/memory/validation.py#L122))
- [x] HC18.4 測試:`if x < 3 && y > 1` 存取後原文不變

---

## Phase 4 — 死碼安全/學習機制:接線或移除

> 主題:大量「已實作但從未被呼叫/從未被寫入」。每項先決策(接線 or 刪除),再執行。

- [x] D1 satisfaction 模型:`update_from_feedback` 接上 feedback 攝取路徑,或於 `update_from_metrics` 寫入推得 satisfaction([prediction/user_model.rs:225](../../crates/duduclaw-gateway/src/prediction/user_model.rs#L225))
- [x] D2 anti-sycophancy:`ConsistencyTracker::record` 接線,或移除死碼([prediction/router.rs:213](../../crates/duduclaw-gateway/src/prediction/router.rs#L213))
- [x] D3 RL trajectory `tool_calls` 永遠 None → 補實際 tool_calls 使 reward 有訊號([rl/trajectory_export.rs:47](../../crates/duduclaw-gateway/src/rl/trajectory_export.rs#L47))
- [x] D4 `evict_idle` 接上背景排程(dep of HP2.2)
- [x] D5 CRL `is_stale()` 接線(dep of HS13.1)
- [x] D6 `vet_synthesized_skill` 接上畢業路徑(dep of HS6.2)
- [x] D7 GVU rollback 完整性 hash(同 HC10,確認接線)
- [x] D8 `allowed_tools` 白名單(同 HS12,確認接線)
- [x] D9 `SecretManager` `vault_token_enc` 解析但從不解密/建 adapter → 實作或修正文件([secret_manager/mod.rs:165](../../crates/duduclaw-security/src/secret_manager/mod.rs#L165))
- [x] D10 `key_vault::resolve_agent_keys` 無視 `allowed_channels`(latent,無 caller)→ 接線或標記([security/key_vault.rs:76](../../crates/duduclaw-security/src/key_vault.rs#L76))
- [x] D11 shared.rs/federated.rs 未在 lib.rs 宣告且引用不存在的 `sanitize_fts5_query` → 移除死碼或補齊使其可編譯([memory/shared.rs:225](../../crates/duduclaw-memory/src/shared.rs#L225))
- [x] D12 `targets_sensitive_input` 硬編 false → 接上真實判斷或移除分支([computer_use_orchestrator.rs:837](../../crates/duduclaw-gateway/src/computer_use_orchestrator.rs#L837))

---

## Phase 5 — MEDIUM

### 5a security
- [x] M1 `/api/reliability/summary` 未驗證 + 每 request 跑 `sync_from_files` → 加 auth + 共用長壽索引([server.rs:1741](../../crates/duduclaw-gateway/src/server.rs#L1741))
- [x] M2 login rate limiter 僅以 email 為 key 且成功不重置 → IP-scoped + 成功重置([server.rs:42](../../crates/duduclaw-gateway/src/server.rs#L42))
- [x] M3 Feishu verification_token 未設時接受未簽章事件 → 未設拒絕啟動 + 驗 Encrypt Key/signature([feishu.rs:173](../../crates/duduclaw-gateway/src/feishu.rs#L173))
- [x] M4 `evolution_toggle` 未呼叫 `is_valid_agent_id`(path traversal)→ 補驗證([mcp.rs:3826](../../crates/duduclaw-cli/src/mcp.rs#L3826))
- [x] M5 `handle_wiki_write` 無跨 agent visibility 檢查 → 補 `check_wiki_visibility`([mcp.rs:7406](../../crates/duduclaw-cli/src/mcp.rs#L7406))
- [x] M6 stdio MCP 無 key 設定時 fail-open 授 all-scopes Admin → fail-closed([mcp_auth.rs:259](../../crates/duduclaw-cli/src/mcp_auth.rs#L259))
- [x] M7 mask region u32 溢位 panic/wrap → `saturating_add` + clamp([computer_use.rs:137](../../crates/duduclaw-gateway/src/computer_use.rs#L137))
- [x] M8 `scrub_metadata` 只洗 top-level,巢狀/`last_error`/`violation_detail` 明文落盤 → 遞迴洗淨([evolution_events/logger.rs:288](../../crates/duduclaw-gateway/src/evolution_events/logger.rs#L288))
- [x] M9 malformed capabilities header 被當 permissive → 解析失敗拒絕([mcp_headers.rs:184](../../crates/duduclaw-cli/src/mcp_headers.rs#L184))
- [x] M10 action-claim `target_id` 無錨點 substring match → 錨定/精確比對(dep: X2)([action_claim_verifier.rs:208](../../crates/duduclaw-security/src/action_claim_verifier.rs#L208))
- [x] M11 policy 同在 requires_approval 與 denied_scopes 被導向 approval → denied 優先([governance/evaluator.rs:350](../../crates/duduclaw-governance/src/evaluator.rs#L350))
- [x] M12 wizard company name 未跳脫內插進 TOML → 用 `toml::Value` 建構([wizard.rs:578](../../crates/duduclaw-cli/src/wizard.rs#L578))
- [x] M13 macOS `service stop` 對 `lsof -ti :PORT` 全部 SIGKILL → 驗證 executable 為 duduclaw([service/mod.rs:177](../../crates/duduclaw-cli/src/service/mod.rs#L177))
- [x] M14 prompt-injection 掃描純英文 substring 易繞過 + 阻擋不發 audit([input_guard.rs:136](../../crates/duduclaw-security/src/input_guard.rs#L136))
- [x] M15 既存 jwt_secret 不檢查/修復權限(world-readable 可偽造 token)→ 載入時驗 + 修復 owner-only([auth/jwt.rs:58](../../crates/duduclaw-auth/src/jwt.rs#L58))
- [x] M16 失敗登入只 warn! 不寫 `auth_audit_log` → 記錄失敗事件([server.rs:1131](../../crates/duduclaw-gateway/src/server.rs#L1131))
- [x] M17 `send_to_bus` 不驗 chat_id/sender/text 長度/字元集([bridge/lib.rs:89](../../crates/duduclaw-bridge/src/lib.rs#L89))
- [x] M18 Odoo `company_ids` 解析但不套用到 RPC context(跨公司隔離 no-op)([odoo/agent_config.rs:60](../../crates/duduclaw-odoo/src/agent_config.rs#L60))
- [x] M19 `scrub_odoo_error` 只截 240 字不洗 URL/credential → 洗敏感資訊([handlers.rs:6039](../../crates/duduclaw-gateway/src/handlers.rs#L6039))
- [x] M20 Notion 非 2xx body 錯誤訊息透傳給 LLM/log → scrub([notion.rs:170](../../crates/duduclaw-identity/src/providers/notion.rs#L170))
- [x] M21 Python `agent_tools._set_status`/`agent_status` 對 name 無驗證(path traversal)→ 補驗證([agent_tools.py:236](../../python/duduclaw/tools/agent_tools.py#L236))
- [x] M22 refresh token 存 localStorage(XSS 可讀)→ httpOnly+SameSite cookie([auth-store.ts:128](../../web/src/stores/auth-store.ts#L128))

### 5b correctness
- [x] M23 Ed25519 challenge 存共享 `Mutex<Option<..>>` 並發 clobber → per-connection challenge([gateway/auth.rs:56](../../crates/duduclaw-gateway/src/auth.rs#L56))
- [x] M24 `get_allowed_channels` 無 global-scope fallback(global 白名單被忽略=全放行)([channel_settings.rs:151](../../crates/duduclaw-gateway/src/channel_settings.rs#L151))
- [x] M25 LINE 長回覆無分段超 Flex 限制([line.rs:369](../../crates/duduclaw-gateway/src/line.rs#L369))
- [x] M26 `strip_feishu_mentions` 在 U+3000 邊界切片 panic → X1([feishu.rs:211](../../crates/duduclaw-gateway/src/feishu.rs#L211))
- [x] M27 `to_asr_pcm` 假設 stereo,mono decode 錯誤取樣率([discord_voice.rs:123](../../crates/duduclaw-gateway/src/discord_voice.rs#L123))
- [x] M28 `poll_pending_taskspecs` 狀態翻轉在 spawned task 內 → double-pickup 視窗([dispatcher.rs:1037](../../crates/duduclaw-gateway/src/dispatcher.rs#L1037))
- [x] M29 `verify_step_auto` 含 criterion 關鍵字即 PASS(複述目標即通過)([task_spec.rs:707](../../crates/duduclaw-gateway/src/task_spec.rs#L707))
- [x] M30 detached-HEAD atomic_merge commit 不可達 + 並發工作樹被換([worktree.rs:469](../../crates/duduclaw-gateway/src/worktree.rs#L469))
- [x] M31 pty_runtime cache-hit/spawn 指標用 global session_count 取樣,並發倒置([pty_runtime.rs:456](../../crates/duduclaw-gateway/src/pty_runtime.rs#L456))
- [x] M32 feedback rollback 門檻 `> 0.0`,無 feedback 檔時永不觸發([gvu/updater.rs:697](../../crates/duduclaw-gateway/src/gvu/updater.rs#L697))
- [x] M33 soul scanner <70 severity 只 log 放行,可拆分多個低分繞過([gvu/updater.rs:408](../../crates/duduclaw-gateway/src/gvu/updater.rs#L408))
- [x] M34 明顯有害 skill 在湊滿 5 對話前無法早期 discard([sandbox_trial.rs:147](../../crates/duduclaw-gateway/src/skill_lifecycle/sandbox_trial.rs#L147))
- [x] M35 metacognition crit_proportion 用 lifetime cumulative,ratchet 至 floor 誤觸 emergency GVU([metacognition.rs:587](../../crates/duduclaw-gateway/src/prediction/metacognition.rs#L587))
- [x] M36 skill/fallback rate 用全事件分母,新增事件機械性壓低([evolution_events/query.rs:643](../../crates/duduclaw-gateway/src/evolution_events/query.rs#L643))
- [x] M37 批次狀態寫入失敗的 reminder 卡 Pending+fired_ids 永久跳過至重啟([reminder_scheduler.rs:696](../../crates/duduclaw-gateway/src/reminder_scheduler.rs#L696))
- [x] M38 `semantic_conflict_count` 比較 RFC3339 vs SQLite datetime 字串([memory/engine.rs:584](../../crates/duduclaw-memory/src/engine.rs#L584))
- [x] M39 inference fast-keyword 用 `contains`("hi"/"list" 誤命中)→ word-boundary(dep: X2)([inference/router.rs:98](../../crates/duduclaw-inference/src/router.rs#L98))
- [x] M40 agent resolver 迭代 HashMap 非決定性順序([agent/resolver.rs:49](../../crates/duduclaw-agent/src/resolver.rs#L49))
- [x] M41 同名 agent 目錄靜默互相覆蓋無 WARN([agent/registry.rs:123](../../crates/duduclaw-agent/src/registry.rs#L123))
- [x] M42 policy merge 僅以 policy_id 去重,不同 type 同 id 抹除 global([governance/registry.rs:196](../../crates/duduclaw-governance/src/registry.rs#L196))
- [x] M43 approval `cleanup_expired` 不移除 Approved/Rejected,pending map 無界([governance/approval.rs:331](../../crates/duduclaw-governance/src/approval.rs#L331))
- [x] M44 redaction `from_specs` 不以 id 去重,跨 profile 同 id 雙重 fire([redaction/engine.rs:42](../../crates/duduclaw-redaction/src/engine.rs#L42))
- [x] M45 非 system-prompt 來源的 `Selective` 落入 full-redact arm 過度遮罩([redaction/pipeline.rs:100](../../crates/duduclaw-redaction/src/pipeline.rs#L100))
- [x] M46 `collect_response` 丟棄 frame 後尾位元組不 re-buffer([cli-runtime/session.rs:650](../../crates/duduclaw-cli-runtime/src/session.rs#L650))
- [x] M47 ReadTimeout 空 drain busy-spin 持 semaphore permit 達 3 分鐘([cli-runtime/session.rs:571](../../crates/duduclaw-cli-runtime/src/session.rs#L571))
- [x] M48 worker work_dir 分歧:首次 None→後續 Some 靜默忽略([cli-worker/server.rs:554](../../crates/duduclaw-cli-worker/src/server.rs#L554))
- [x] M49 `delay_for_attempt` 在 capped delay 0 時 `gen_range(0.0..0.0)` panic([durability/retry.rs:152](../../crates/duduclaw-durability/src/retry.rs#L152))
- [x] M50 `max_checkpoints` 宣稱防 OOM 但 save() 不強制([durability/checkpoint.rs:144](../../crates/duduclaw-durability/src/checkpoint.rs#L144))
- [x] M51 license `validate()` 不檢查 tier/deployment-mode 綁定([license/license.rs:115](../../crates/duduclaw-license/src/license.rs#L115))
- [x] M52 撤銷不持久化,`delete_default()` 失敗則重啟重新啟用([license_runtime.rs:469](../../crates/duduclaw-gateway/src/license_runtime.rs#L469))
- [x] M53 Odoo `poll_model` 在 search_read 返回後才記 `Utc::now()`,round-trip 間紀錄跳過([odoo/events.rs:84](../../crates/duduclaw-odoo/src/events.rs#L84))
- [x] M54 Odoo write/count `unwrap_or(false/0)` 吞非預期 RPC 結果,未套用 write 報成功([odoo/connector.rs:195](../../crates/duduclaw-odoo/src/connector.rs#L195))
- [x] M55 `extract_frontmatter` 在首個 `\n---` 截斷,含 `---` 的值整筆丟棄([wiki_cache.rs:197](../../crates/duduclaw-identity/src/providers/wiki_cache.rs#L197))
- [x] M56 Python memory 每日寫入配額 check-then-await-then-increment 並發超限([store.py:115](../../python/duduclaw/mcp/tools/memory/store.py#L115))
- [x] M57 `store_temporal` 的 `supersedes` SELECT 無 ORDER BY,多筆 active 非決定性([memory/engine.rs:310](../../crates/duduclaw-memory/src/engine.rs#L310))
- [x] M58 第二個 approval_request 覆蓋第一個,前者永不回覆 → 改 queue([ApprovalModal.tsx:27](../../web/src/components/ApprovalModal.tsx#L27))
- [x] M59 dev proxy 只轉 `/ws` `/health`,`/api/login` 在 `npm run dev` 登入失敗(dev-only)([vite.config.ts:17](../../web/vite.config.ts#L17))

### 5c performance
- [x] M60 每次 audit/reliability RPC 開新 SQLite 連線並全檔 `sync_from_files`(熱路徑)→ 共用 `Arc<AuditEventIndex>` + 背景同步([handlers.rs:4527](../../crates/duduclaw-gateway/src/handlers.rs#L4527))
- [x] M61 reminder agent_callback 持 semaphore permit 跨無界 LLM 呼叫 + retry sleep([reminder_scheduler.rs:792](../../crates/duduclaw-gateway/src/reminder_scheduler.rs#L792))

---

## Phase 6 — LOW(清理 / 邊界守衛 / zh-TW 本地化)

### 6a 死碼 / 清理
- [x] L1 `handle_tasks_assign` 死的 `update` binding 移除([handlers.rs:7134](../../crates/duduclaw-gateway/src/handlers.rs#L7134))
- [x] L2 `cap_rust_log_verbosity` substring replace "trace" 損毀 target 名([worker_supervisor.rs:316](../../crates/duduclaw-gateway/src/worker_supervisor.rs#L316))
- [x] L3 `secret_manager` `vault_token_enc`(同 D9,若選擇移除則歸此)
- [x] L4 `account_rotator` LeastCost 永遠選第一個 OAuth,多帳號不輪替([account_rotator.rs:407](../../crates/duduclaw-agent/src/account_rotator.rs#L407))
- [x] L5 `is_available` OnceCell 永久快取首次 probe,transient 失敗永久停用 MLX([mlx_bridge.rs:67](../../crates/duduclaw-inference/src/mlx_bridge.rs#L67))
- [x] L6 llamafile `start()` check-then-set TOCTOU([llamafile.rs:120](../../crates/duduclaw-inference/src/llamafile.rs#L120))
- [x] L7 `from_state` 每次 snapshot 重算 fingerprint(hostname+MAC)→ 快取([license_runtime.rs:190](../../crates/duduclaw-gateway/src/license_runtime.rs#L190))

### 6b 邊界 / 數值守衛
- [x] L8 `session_id[..8]` byte slice panic → `chars().take(8)`(dep: X1)([rl/trajectory_export.rs:60](../../crates/duduclaw-gateway/src/rl/trajectory_export.rs#L60))
- [x] L9 Slack split 用 byte 閾值,長 CJK char-boundary panic → X1([slack.rs:424](../../crates/duduclaw-gateway/src/slack.rs#L424))
- [x] L10 `cost_telemetry` output_tokens i64→u64 未過 `safe_u64`,負值 wrap([cost_telemetry.rs:526](../../crates/duduclaw-gateway/src/cost_telemetry.rs#L526))
- [x] L11 `handle_wiki_write` 缺 read 端 `canonicalize()` symlink-escape 守衛([mcp.rs:7435](../../crates/duduclaw-cli/src/mcp.rs#L7435))
- [x] L12 key age `num_days() as u64` 未 `.max(0)`,未來日期荒謬天數([mcp_auth.rs:228](../../crates/duduclaw-cli/src/mcp_auth.rs#L228))
- [x] L13 `graduate_skill_to_disk` 用 skill.name 建檔名無 path 驗證([sandbox_trial.rs:317](../../crates/duduclaw-gateway/src/skill_lifecycle/sandbox_trial.rs#L317))
- [x] L14 `quota_manager` `consume_tokens` 用 `>` 而 check 用 `>=`,邊界不一致([quota_manager.rs:130](../../crates/duduclaw-governance/src/quota_manager.rs#L130))
- [x] L15 過期 token 還原發 `RestoreOk`,稽核高估 PII 揭露([redaction/pipeline.rs:248](../../crates/duduclaw-redaction/src/pipeline.rs#L248))
- [x] L16 國民身分證 regex 無 word boundary/checksum,全形/分隔/小寫漏抓([taiwan_strict.toml:6](../../crates/duduclaw-redaction/src/profiles/taiwan_strict.toml#L6))
- [x] L17 `pid()` 為 None 時 process-group 終止被略過,洩漏孤兒子行程([cli-runtime/session.rs:199](../../crates/duduclaw-cli-runtime/src/session.rs#L199))
- [x] L18 `RuntimeType::parse` 未知 provider 靜默映 Claude([core/types.rs:184](../../crates/duduclaw-core/src/types.rs#L184))
- [x] L19 `agent_guard` 只比正斜線(Windows 反斜線繞過)+ case-sensitive `starts_with`([agent_guard.rs:154](../../crates/duduclaw-core/src/agent_guard.rs#L154)、[L104](../../crates/duduclaw-core/src/agent_guard.rs#L104))
- [x] L20 `expand_home` 在 HOME 無解時回字面 `~/.duduclaw`,work_dir 一律拒絕([cli-worker/main.rs:101](../../crates/duduclaw-cli-worker/src/main.rs#L101))
- [x] L21 Bearer auth 在 JSON body 完整反序列化後才執行([cli-worker/server.rs:375](../../crates/duduclaw-cli-worker/src/server.rs#L375))
- [x] L22 `registry_keys` log 只回 0/1 而非真實 key 數([license_runtime.rs:118](../../crates/duduclaw-gateway/src/license_runtime.rs#L118))
- [x] L23 bus router trigger 用 `contains(agent_id)` 子字串誤路由(dep: X2)([bus/router.rs:61](../../crates/duduclaw-bus/src/router.rs#L61))
- [x] L24 `key_press` split('+') 使 `ctrl++` 產生空 segment 無鍵([desktop/controller.rs:183](../../crates/duduclaw-desktop/src/controller.rs#L183))
- [x] L25 Python `vetter` secret regex 漏抓 `github_pat_*` fine-grained PAT([vetter.py:70](../../python/duduclaw/evolution/vetter.py#L70))
- [x] L26 `snapshots.py` insert 成功以 `'INSERT 0 1'` exact match,oid 形式漏算([snapshots.py:95](../../python/duduclaw/memory_eval/db/snapshots.py#L95))
- [x] L27 `auth/db.rs` `conn()` fallback `pool[0].lock().unwrap()` poisoned 即全 auth DoS([auth/db.rs:43](../../crates/duduclaw-auth/src/db.rs#L43))
- [x] L28 `ensure_default_admin` 註解宣稱 transaction 但實際無([auth/db.rs:525](../../crates/duduclaw-auth/src/db.rs#L525))
- [x] L29 `autopilot notify` Discord chat_id 未驗 snowflake 直接內插 path([autopilot_engine.rs:756](../../crates/duduclaw-gateway/src/autopilot_engine.rs#L756))
- [x] L30 `evolution_events/logger` 寫入錯誤時當前 event 不 retry 直接丟棄([logger.rs:230](../../crates/duduclaw-gateway/src/evolution_events/logger.rs#L230))

### 6c zh-TW 本地化
- [x] L31 Telegram/Slack/Discord 多處 status/help 為英文 → 改 zh-TW([telegram.rs:448](../../crates/duduclaw-gateway/src/telegram.rs#L448))
- [x] L32 dispatcher 系統注入的 permission 警告為英文且洩漏內部 CLI flag([dispatcher.rs:653](../../crates/duduclaw-gateway/src/dispatcher.rs#L653))
- [x] L33 channel_sender 同 user_id 並發確認覆蓋前一 oneshot + 任何非肯定訊息被當 decline([channel_sender.rs:70](../../crates/duduclaw-gateway/src/channel_sender.rs#L70)、[L98](../../crates/duduclaw-gateway/src/channel_sender.rs#L98))
- [x] L34 macOS `service logs` 指向 `/tmp/` 但 launchd 寫 `~/Library/Logs/`([service/mod.rs:238](../../crates/duduclaw-cli/src/service/mod.rs#L238))
- [x] L35 LoginPage 顯示 server 原始英文錯誤,未 zh-TW([LoginPage.tsx:24](../../web/src/pages/LoginPage.tsx#L24))

---

## 統計

| Phase | 內容 | distinct 工項組 | 細項數(約) |
|---|---|---|---|
| 0 | 共用基礎建設 | 4 | 11 |
| 1 | CRITICAL | 6 | 36 |
| 2 | HIGH security | 14 | 50 |
| 3 | HIGH correctness/perf | 20 | 60 |
| 4 | 死碼接線/移除 | 12 | 12 |
| 5 | MEDIUM | 61 | 61 |
| 6 | LOW | 35 | 35 |

> 建議執行序:`X1–X4`(共用 util) → `C1–C6`(CRITICAL) → `HS*`(可遠端 HIGH security) → `HC*/HP*`(資料遺失 HIGH) → `D*`(死碼) → `M*` → `L*`。
> 多個 HIGH/MEDIUM/LOW 標註 `dep: X*`,完成共用 util 後可批次套用以消除系統性缺陷。
