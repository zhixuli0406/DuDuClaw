# DuDuClaw 全專案深度程式碼審查報告

## 總覽

本次審查涵蓋 30 個範圍,共驗證出 **96 項問題**:**6 CRITICAL**、**40 HIGH**(其中 CONFIRMED 居多,1 項 PLAUSIBLE)、**40 MEDIUM**、**24 LOW**。整體健康度呈現「核心架構完整但安全邊界系統性鬆動」的態勢。最突出的三大主題是:**(1) 授權/scope 強制執行的系統性缺口**——MCP 內部 principal 對最高風險工具完全無 scope 檢查、多條 channel/dashboard 路徑缺少 `check_agent!`、Odoo verb 分類繞過;**(2) 安全機制「fail-open」與「死碼」**——LINE/Feishu 簽章空金鑰放行、SOUL.md hash 刪除自癒、容器 sandbox 完全不套用隔離、screenshot 遮罩失敗洩漏、多個掃描器只掃 `proposal.content` 而非實際寫入的 `patch.content`;**(3) UTF-8/位元組切片 panic 與 pipe/buffer 死鎖** 散見於多處字串與子行程處理。此外有大量「已實作但從未被呼叫/從未被寫入」的死碼安全與學習機制(satisfaction 模型、sycophancy 偵測、rollback 完整性檢查、`allowed_tools` 白名單)。

---

## 嚴重問題 (CRITICAL / HIGH)

### 1. 預設 admin 密碼硬編碼為 "admin" — CRITICAL / security
[crates/duduclaw-auth/src/db.rs#L532](crates/duduclaw-auth/src/db.rs#L532)

`ensure_default_admin` 在 users 表為空時建立 `admin@local`,密碼字面值為 `"admin"`(`let default_password = "admin";`),且 `warn!` banner 直接把密碼印出來——文件註解卻宣稱會產生「random 24-char password」。`handle_login`([server.rs#L1115](crates/duduclaw-gateway/src/server.rs#L1115))接受此憑證並發出 Admin role JWT,`UserRole::Admin` 短路所有 ACL 檢查,且**無強制改密碼機制**(全 repo grep 無 `must_change_password`)。

**失敗情境**:任何能觸及 `/api/login` 的攻擊者以 `admin@local` / `admin` 登入,取得全部 agent、SOUL.md、skills、使用者管理的完整控制權。

**修復**:首次啟動真正產生隨機密碼(如註解所述),或設定 `must_change_password` 旗標阻擋首次登入後的任何操作直到改密碼;移除 banner 中的明文密碼。

### 2. MCP 內部 principal 對最高風險工具完全無 scope 強制 — CRITICAL / security
[crates/duduclaw-cli/src/mcp_auth.rs#L411](crates/duduclaw-cli/src/mcp_auth.rs#L411)

`tool_requires_scope()` 只對 memory/wiki/messaging/identity/odoo/audit 類工具回傳 `Some(scope)`;`execute_program`、`agent_remove`、`agent_update_soul`、`agent_update`、`spawn_agent`、`send_to_agent`、`evolution_toggle`、`schedule_task`、`delete_cron_task`、`shared_wiki_write`、`shared_wiki_delete` 全部落入 `_ => None`。dispatcher 的 scope gate([mcp_dispatch.rs#L128](crates/duduclaw-cli/src/mcp_dispatch.rs#L128))只在 `Some` 時拒絕,`None` 即略過檢查。external whitelist 只對 `is_external=true` 生效,內部 key `is_external=false` 完全跳過。

**失敗情境**:operator 發給低信任 sub-agent 一把 `scopes=["memory:read"]` 的內部 key,該 key 仍可呼叫 `execute_program`(PTC sandbox 任意程式執行)與 `agent_update_soul`(改寫任意 agent system prompt)。每把內部 key 對最高影響工具皆無最小權限約束。

**修復**:為所有狀態變更/高風險工具在 `tool_requires_scope` 補上對應 scope(例如新增 `Scope::AgentAdmin`、`Scope::Execute`),並讓未列舉工具 **fail-closed**(預設要求 Admin scope 而非 `None`)。

### 3. Redaction egress 還原路徑不檢查 RestoreScope — CRITICAL / security
[crates/duduclaw-redaction/src/egress.rs#L122](crates/duduclaw-redaction/src/egress.rs#L122)

`EgressEvaluator::decide` 的 `RestoreArgsMode::Restore` 分支對 vault entry 一律以 `entry.original` 取代 token,**從不呼叫 `entry.restore_scope.allows(...)`**,且 `decide()` 簽章根本沒有 `Caller` 參數可供評估 scope。對照 pipeline 還原路徑([pipeline.rs#L225](crates/duduclaw-redaction/src/pipeline.rs#L225))則正確強制 scope。`profiles/financial.toml` 定義了 `restore_scope = any_scope FinanceRead` 等敏感 PII。

**失敗情境**:任何能觸發 whitelisted 工具(`send_email`、`odoo.*`)的 agent/sub-agent,可讓帶有 `FinanceRead` 或 `Owner` scope 的 token 被無授權還原為明文並送往該工具,洩漏範圍化/Owner PII。

**修復**:把 `Caller` 一路 plumb 進 `decide()`(從 [mcp_redaction.rs#L138](crates/duduclaw-cli/src/mcp_redaction.rs#L138) 的 `decide_tool_call` 起),在 substitute 前比照 pipeline 呼叫 `restore_scope.allows(caller)`,拒絕時發 `RestoreDenied` 並保留 token。

### 4. License phone-home 刷新只驗簽章不驗 validate — CRITICAL / security
[crates/duduclaw-gateway/src/license_runtime.rs#L430](crates/duduclaw-gateway/src/license_runtime.rs#L430)

`do_phone_home_once` 在 `status=="active"` 時只呼叫 `registry.verify(&new_license)`(純 Ed25519 簽章驗證),隨即 `save_default` + 換入 `inner.license`,**從不呼叫 `new_license.validate()`**(過期/指紋/grace 全略過)。bootstrap 路徑 `load_and_validate`([L320](crates/duduclaw-gateway/src/license_runtime.rs#L320))則兩者皆做。request body 無 nonce/freshness,可被重放。

**失敗情境**:control-plane(有 bug、被入侵、或重放一個簽好的 `{"status":"active",...}`)回傳一張機器指紋屬於別台機器、或 `expires_at` 已過期的合法簽章 License,被持久化並啟用,繞過 per-machine 綁定與到期。CRL loop 只檢查訂閱撤銷,不會攔截。

**修復**:刷新路徑在 swap 前比照 bootstrap 呼叫 `validate(&current_fp, phone_home, grace)`,並對到期/指紋不符拒絕安裝;加入 nonce 防重放。

### 5. /ws/chat WebChat 通道完全無驗證 (CSWSH + 無認證) — CRITICAL / security
[web/src/stores/chat-store.ts#L60](web/src/stores/chat-store.ts#L60) · [crates/duduclaw-gateway/src/webchat.rs#L102](crates/duduclaw-gateway/src/webchat.rs#L102)

client `connect()` 以 `new WebSocket('/ws/chat')` 開啟,無 JWT/token/handshake(對照 `ws-client.ts` 會送 `{jwt}`)。後端 `ws_chat_handler` 不取 `HeaderMap`、不驗 JWT、**不做 Origin 檢查**,只做 per-IP rate limit,直接驅動主 agent 的 LLM/MCP 路徑(`build_reply_with_session`)。route 在 [server.rs#L974](crates/duduclaw-gateway/src/server.rs#L974) 以獨立 state 註冊,無 auth layer 包覆。相關 HIGH 重複項 [web#1] 專指 CSWSH 面向。

**失敗情境**:gateway 綁 `0.0.0.0`(文件範例)時,任何網路 client 直接開 `/ws/chat` 與主 agent 對話——消耗 LLM 預算、洩漏 agent/公司知識、觸發 MCP 工具呼叫,完全繞過保護其他 dashboard 介面的 LoginPage/JWT/RoleGuard。即使僅綁 loopback,使用者瀏覽惡意網頁時也會被 CSWSH(無 Origin 檢查)從瀏覽器情境驅動本機 agent。

**修復**:讓 `/ws/chat` 走與 `/ws` 相同的 connect handshake(要求 JWT/token)+ Origin 嚴格校驗;`WebChatState` 注入 `AuthManager`/`JwtConfig`。

### 6. 容器 PTC sandbox 不套用 network=none / tmpfs / memory 隔離 — CRITICAL / security
[crates/duduclaw-container/src/docker.rs#L64](crates/duduclaw-container/src/docker.rs#L64)(另:Apple 後端 [apple.rs#L74](crates/duduclaw-container/src/apple.rs#L74) 為 no-op stub,見 HIGH 項)

`DockerRuntime::create` 建立的 `HostConfig` 只設 `binds` + `readonly_rootfs`,`network_mode`/tmpfs/memory 全留 `Default(None)`。PTC sandbox([sandbox.rs#L358](crates/duduclaw-cli/src/ptc/sandbox.rs#L358))以 `network_access:false`(`// --network=none`)建 config,但該欄位在此路徑根本未被讀取;日誌 `network="none"` 是假的。正確實作存在於 `duduclaw-container/src/sandbox.rs` 但 PTC 不走它。

**失敗情境**:agent 撰寫的 Python/bash script 跑在 Docker 預設 bridge 網路上、具完整對外連線,可對 cloud metadata endpoint / 內部服務發動 SSRF 並外洩資料,儘管程式與註解宣稱網路已停用。Apple 後端在 macOS 15+(偏好 runtime)甚至完全不啟動容器卻回報成功,等同零隔離。

**修復**:`DockerRuntime::create` 依 `config.network_access` 設 `network_mode=Some("none")`、套用 tmpfs 與 memory limit(或直接改走已正確實作的 `run_sandboxed_with_env`);移除/實作 Apple 後端的 no-op stub,失敗時 fail-closed。

---

### HIGH 嚴重問題

> 為節省篇幅,以下 HIGH 項以「位置 / 維度 — 問題 — 失敗情境 — 修復」精煉呈現;跨範圍同根因者已合併。

#### 安全 (security)

**SOUL.md 內容安全 gate 只掃 `proposal.content` 而非實際寫入的 `patch.content`** — [crates/duduclaw-gateway/src/gvu/updater.rs#L389](crates/duduclaw-gateway/src/gvu/updater.rs#L389)(另 L407/L433)。L3 judge([loop_.rs#L340](crates/duduclaw-gateway/src/gvu/loop_.rs#L340))亦只看 narrative。三道 gate(prompt-injection scan、soul_scanner hidden-content、`must_not`/`must_always`/contract NFKC 檢查)全對 `proposal.content` 執行,但 `apply_patch_to_soul` 逐字寫入 `patch.content`,僅 L1 deterministic 檢查 patch payload。**情境**:LLM(或上游注入)送出無害的 `proposed_changes` 敘述 + 帶 HTML-comment/zero-width 隱藏指令或剝除 `must_always` 的 `soul_patch`,繞過除 L1 外所有內容安全檢查落入 SOUL.md。**修復**:對 `apply_patch_to_soul` 的最終結果(或 `patch.content` 本身)同樣執行 scan_input/scan_soul/contract 檢查,並讓 judge prompt 嵌入實際 patch。

**LINE webhook 簽章空金鑰 fail-open** — [crates/duduclaw-gateway/src/line.rs#L201](crates/duduclaw-gateway/src/line.rs#L201)。`read_line_config` 以 `unwrap_or_default()` 取 secret,`secret=""` 時 bot 仍啟動;`verify_signature` 的 `HmacSha256::new_from_slice(b"")` 對空 key 回 `Ok`,攻擊者計算 `base64(HMAC(key="", body))` 即通過。對照 whatsapp.rs/feishu.rs 有空 secret 防護。**情境**:operator 設了 token 卻忘了 secret,任何人可偽造簽章注入任意 user message → prompt injection / 未授權 agent 呼叫。**修復**:`secret.is_empty()` 時拒絕啟動 LINE bot。

**WebSocket Origin allowlist 用 `starts_with` 可被繞過** — [crates/duduclaw-gateway/src/server.rs#L1391](crates/duduclaw-gateway/src/server.rs#L1391)。`origin.starts_with("http://localhost")` 對 `http://localhost.evil.com`/`http://127.0.0.1.evil.com` 皆 true。**情境**:本機無 user/auth_token 時 `/ws` 落入 `admin_fallback()`([L1569](crates/duduclaw-gateway/src/server.rs#L1569)),攻擊者頁面可取得完整 admin RPC。**修復**:解析 URL authority 比對精確 host:port。

**wiki.*/tasks.*/activity.list/skills.content dispatch 無 role/agent-binding 檢查** — [crates/duduclaw-gateway/src/handlers.rs#L443](crates/duduclaw-gateway/src/handlers.rs#L443)(另 6971–7136)。對照 memory.* 有 `check_agent!(Viewer)`,wiki/tasks 路徑完全沒有,甚至不轉傳 `ctx`。**情境**:Employee role 僅綁 agent A,送 `wiki.read{agent_id:"agent-B"}` 讀到他隊私有 SOP;可對任意 agent `tasks.create/remove/assign` 並觸發 Autopilot delegate/run_skill 副作用。**修復**:這些 arm 補上 `check_agent!(Viewer)`/`require_role` 比照 memory.*。

**Container screenshot 遮罩 fail-open 洩漏敏感像素** — [crates/duduclaw-gateway/src/computer_use_orchestrator.rs#L495](crates/duduclaw-gateway/src/computer_use_orchestrator.rs#L495)。`detect_sensitive_regions` 任何失敗回 `Ok(vec![])`,caller 僅 `if !regions.is_empty()` 才遮罩,full-screen fail-safe 只在 Native mode 跑;且 `chromium-browser --evaluate-script` 並非有效 flag,偵測幾乎必失敗。**情境**:含密碼/信用卡欄位的 raw screenshot 預設被上傳至 Anthropic API 並送往 LINE/Telegram/Discord。**修復**:偵測失敗時 fail-closed(全螢幕遮罩或中止),修正容器內 DOM 偵測的呼叫介面。

**Skill 安全 gate 放行 Error 級發現** — [crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs#L338](crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs#L338)。`classify_risk` 把 max_severity `Error` 映為 `RiskLevel::Medium`,`passed = risk < High = true`。更嚴格的 `vetting::vet_synthesized_skill` 從未在畢業路徑被呼叫。**情境**:含 `subprocess.run(...)` + `curl ... -d @~/.ssh/id_rsa` 的合成 skill 通過,寫入 agent SKILLS dir 並畢業至全域 `~/.duduclaw/skills` 供所有 agent 載入。**修復**:`CodeExecution`/`DataExfiltration` 的 Error 應映為 High(阻擋),或畢業路徑改呼叫 vetting。

**SSE `stream_call` 無 conn_id 擁有權檢查 (跨租戶注入)** — [crates/duduclaw-cli/src/mcp_http_server.rs#L379](crates/duduclaw-cli/src/mcp_http_server.rs#L379)。handler 認證 caller 後,把 tool 結果 push 到 caller 提供的任意 `conn_id`;`SseEventStore` 不存 owning client_id。**情境**:Client B 以自己有效 key POST `?conn_id=<A的id>`,B 的完整 JSON-RPC `tool_result` 被廣播進 A 的 SSE stream。**修復**:`register_connection` 記錄 principal,push 前驗證 conn_id 屬於該 principal。

**odoo_execute 把所有呼叫歸類為 'execute' verb,繞過 write/create 限制** — [crates/duduclaw-cli/src/mcp.rs#L5290](crates/duduclaw-cli/src/mcp.rs#L5290)。`classify_odoo_call` 對 odoo_execute 硬回 `("execute", model)`,不看 `params["method"]`;`allowed_actions=["read","search","execute"]` 即放行 `method:"write"`/`action_archive`。**情境**:無 write 權限的 agent 透過 `odoo_execute{method:"write"}` 執行寫入/封存。**修復**:依 `params.method` 推導真實 verb(write/create/unlink/...)再做 `permits` 檢查。

**Odoo reqwest client 無 redirect policy → SSRF validator 可被 30x 繞過** — [crates/duduclaw-odoo/src/connector.rs#L78](crates/duduclaw-odoo/src/connector.rs#L78)。client 預設跟隨 10 次 redirect;`is_safe_odoo_url` 只檢查初始 URL 字串。**情境**:url 指向通過驗證的 `https://attacker.example.com`,對方 302 到 `http://169.254.169.254/`,authenticated POST 打到內部目標(配合 DNS rebinding 亦可)。**修復**:`.redirect(Policy::none())` 或每跳重新驗證 resolved socket addr(比照 web_fetch.rs/updater.rs)。

**Identity wiki_cache 對重複 channel handle 無去重 → 身分冒充** — [crates/duduclaw-identity/src/providers/wiki_cache.rs#L115](crates/duduclaw-identity/src/providers/wiki_cache.rs#L115)。`resolve_by_channel` 回傳 `read_dir`(非決定性順序)第一筆 match,文件卻宣稱會 skip 重複 handle。`identity/` namespace 預設 agent_writable。**情境**:攻擊者經 `shared_wiki_write` 寫入宣告受信者 Discord ID 的檔案,其 roles/project_ids 流入 SOUL.md 信任的 `<sender>` block,取得受害者角色/專案成員資格。**修復**:`iter_people` 對重複 handle 偵測並拒絕(或排序 + 唯一性檢查);考慮把 `identity/` 設為 read_only。

**SOUL.md drift 偵測在 hash 檔遺失時自癒** — [crates/duduclaw-security/src/soul_guard.rs#L168](crates/duduclaw-security/src/soul_guard.rs#L168)。`read_stored_hash` 回 `None` 時落入 None arm,把當前(可能被竄改的)SOUL.md 重新指紋化為信任 baseline 並回 `intact:true`。ASI fallback 從 attacker-writable `.soul_history` 讀取。**情境**:攻擊者編輯 SOUL.md 並刪 `~/.duduclaw/soul_hashes/<id>.hash`,下次 heartbeat 靜默 bless 惡意內容。**修復**:hash 遺失視為可疑(需明確 first-run 標記或外部信任根),不可自動信任當前內容。

**CapabilitiesConfig 永不讀 `allowed_tools` 白名單** — [crates/duduclaw-core/src/types.rs#L478](crates/duduclaw-core/src/types.rs#L478)。`disallowed_tools()` 只 push `denied_tools` + `computer`,從不讀 `self.allowed_tools`;唯一讀取處([handlers.rs#L4676](crates/duduclaw-gateway/src/handlers.rs#L4676))只是 dashboard badge。**情境**:operator 對不信任 sub-agent 設 `allowed_tools=["Read"]` 期望唯讀,agent 仍可 Write/Edit/Bash/delegate。**修復**:新增 `allowed_tools()` 方法並在所有 spawn 路徑(claude_runner.rs:1374、channel_reply.rs、agent/runner.rs、dispatcher.rs)以其建構 `--allowedTools`。

**CRL 撤銷可被重放舊的合法簽章 CRL 繞過** — [crates/duduclaw-gateway/src/license_runtime.rs#L506](crates/duduclaw-gateway/src/license_runtime.rs#L506)。`do_crl_fetch_once` 只 `verify()` + `is_revoked()`,不呼叫已存在的 `is_stale()`,無 `generated_at` 單調性/last-seen 持久化。**情境**:攻擊者(網路路徑/stale CDN/DNS hijack)重放 CRL_t0(撤銷前),客戶端永不降級,被撤銷 tier 持續有效。**修復**:強制 `is_stale()` 與 `generated_at` freshness floor,持久化 last-seen CRL 時戳。

**Per-account OAuth 隔離在 PTY worker 完全失效** — [crates/duduclaw-cli-worker/src/server.rs#L317](crates/duduclaw-cli-worker/src/server.rs#L317)(同根因另見 [pty_runtime.rs#L756](crates/duduclaw-gateway/src/pty_runtime.rs#L756))。`spawn_session_default` 把 `account_id` 綁到 `_account_id_for_future_env_injection` 卻不注入 per-account env;`InvokeParams` 連 env 欄位都沒有。**情境**:multi-OAuth rotation 下 account A/B 雖各建 session,所有 `claude` child 都用同一個 ambient `~/.claude` OAuth,per-account 24h billing cooldown 與 budget cap 被靜默繞過。**修復**:`InvokeParams` 加 env 欄位,spawn 時注入 `CLAUDE_CODE_OAUTH_TOKEN`/`CLAUDE_CONFIG_DIR`;在實作前讓 worker 對 account-rotation 設定 fail-fast 報錯。

**/ws/chat 缺 Origin 驗證 (CSWSH)** — [crates/duduclaw-gateway/src/webchat.rs#L102](crates/duduclaw-gateway/src/webchat.rs#L102)(與 CRITICAL #5 同檔不同面向,並列以利追蹤)。**修復**:同 #5。

#### 正確性 (correctness)

**Telegram Markdown parse-error 重試仍保留 `parse_mode=Markdown`** — [crates/duduclaw-gateway/src/telegram.rs#L1048](crates/duduclaw-gateway/src/telegram.rs#L1048)。fallback `SendMessage` 仍設 `parse_mode: Some("Markdown")`,重試同樣失敗,回覆被靜默丟棄。dispatcher.rs:2364 已示範正確作法(重試丟 parse_mode)。**修復**:重試時 `parse_mode: None`(該欄位 `skip_serializing_if=Option::is_none`,省略即純文字)。

**Discord embed 超過 6000 字總長 / 第 10 個 embed 後靜默丟棄** — [crates/duduclaw-gateway/src/channel_format.rs#L93](crates/duduclaw-gateway/src/channel_format.rs#L93)。`send_discord_message` 只 split `content`,不 split embeds,無 6000 字聚合上限檢查;`&embeds[..min(10)]` 丟棄第 10 個之後。**情境**:5000–8192 字回覆組成 2 個 embed(~7096 字)被 Discord 400 拒絕整則遺失;>40960 字者超過 10 embed 部分連同 footer 靜默消失。**修復**:跨多則訊息分批送 embeds,守住 6000 聚合上限,超過 10 embed 時續送下一則。

**`task_status` byte-slice 在多位元組 UTF-8 邊界 panic** — [crates/duduclaw-cli/src/mcp.rs#L3204](crates/duduclaw-cli/src/mcp.rs#L3204)。`&result.output[..min(200)]` 對 CJK 文字 byte 200 落在字元中間時 panic。**情境**:>200 byte 的中文 step output 使 `handle_task_status` 崩潰,有效 task_id 回不出結果。**修復**:用 `char_indices`/`floor_char_boundary` 安全截斷。

**Subprocess PTC 在 child.wait() 後才讀 stdout → >64KB 輸出 pipe 死鎖** — [crates/duduclaw-cli/src/ptc/sandbox.rs#L235](crates/duduclaw-cli/src/ptc/sandbox.rs#L235)。讀取只在 `Ok(Ok(status))` arm(wait 之後)。**情境**:輸出超過一個 pipe buffer 的 script 阻塞於 write(),wait 永不返回,timeout 殺掉並回 124,所有輸出遺失。**修復**:`tokio::join!(child.wait(), read stdout, read stderr)` 並行排空。

**Container PTC 不掛載 UDS socket / 不設 DUDUCLAW_PTC_SOCKET → MCP 工具呼叫全失敗** — [crates/duduclaw-cli/src/ptc/sandbox.rs#L348](crates/duduclaw-cli/src/ptc/sandbox.rs#L348)。`ContainerConfig` 無 env/command 欄位,socket 從未 bind-mount;Docker `create` 也不設 Cmd,user script 根本不執行,卻回 `exit_code:0`、`tool_calls_count:0`。**情境**:容器模式下任何依賴 `ptc_call` 的 script 靜默無效但回報成功。**修復**:`ContainerConfig`/`ContainerRuntime` trait 增 env 與 cmd 欄位,bind-mount socket path 並注入 env。

**worktree snap 在僅啟 cleanup 時仍 merge 進 main** — [crates/duduclaw-gateway/src/dispatcher.rs#L865](crates/duduclaw-gateway/src/dispatcher.rs#L865)。進入條件 `auto_merge || cleanup`,`determine_snap_action` 不知 auto_merge,有新 commit 即回 `MergeAndCleanup`。**情境**:`worktree_auto_merge=false` + `worktree_cleanup_on_exit=true` 時,sub-agent commit 未經同意被 push 上 main/master。**修復**:把 `auto_merge` 傳入 `determine_snap_action`,false 時降級為 `AutoCleanup`。

**avg_satisfaction 被讀但永不被寫 → satisfaction 模型死碼,predict 永遠回 0.7** — [crates/duduclaw-gateway/src/prediction/user_model.rs#L225](crates/duduclaw-gateway/src/prediction/user_model.rs#L225)。唯一寫入者 `update_from_feedback` 無生產呼叫者;per-conversation 的 `update_from_metrics` 不寫 avg_satisfaction。**情境**:`delta_satisfaction`(占 composite_error 30–45% 最大權重)永遠對凍結常數 0.7 計算,演化引擎從不學習每位 user 的滿意度 baseline。**修復**:把 `update_from_feedback` 接上 feedback 攝取路徑,或在 `update_from_metrics` 寫入推得的 satisfaction。

**evolution_events sync 以「掃描行數」推進 cursor → INSERT 失敗的行永久遺失** — [crates/duduclaw-gateway/src/evolution_events/query.rs#L286](crates/duduclaw-gateway/src/evolution_events/query.rs#L286)。`Err` arm 只 warn,`new_lines_indexed = already + scanned` 無條件 upsert。**情境**:transient SQLITE_BUSY/disk-full 使某行 INSERT 失敗,下次 sync 從 cursor 跳過,事件存在 JSONL 卻永遠缺席索引,腐蝕可靠度指標。**修復**:以「成功 inserted」推進 cursor,或對 transient INSERT 失敗保留 cursor 重試(區分 blank/malformed 的永久跳過)。

**`fallback_trigger_rate` 永遠為 0** — [crates/duduclaw-gateway/src/evolution_events/query.rs#L597](crates/duduclaw-gateway/src/evolution_events/query.rs#L597)。`llm_fallback_triggered` 不是 `AuditEventType` 變體(且實際寫往另一個 `security_audit.jsonl` pipeline),sync 反序列化失敗丟棄。**修復**:新增該 event type 變體並讓 `llm_fallback.rs` 寫入 evolution-events 軌道(或在 dashboard 標示此指標未實作)。

**Agent (MCP) 觸發的 task 狀態變更不產生 TaskStatusChanged 事件** — [crates/duduclaw-gateway/src/autopilot_engine.rs#L786](crates/duduclaw-gateway/src/autopilot_engine.rs#L786)。MCP 端 `tasks_complete` 等只發 `"task.updated"` → 映為 `TaskUpdated`,永不 `TaskStatusChanged`;dashboard 路徑卻會發。**情境**:`trigger_event="task_status_changed"` 的 autopilot rule 從 UI 觸發正常,從 agent 完成任務時靜默不觸發。**修復**:`row_to_event` 對狀態變更映射 `TaskStatusChanged`,或 MCP 端在狀態改變時發專屬事件。

**memory `search_facts` 未 sanitize FTS5 query → 特殊字元使 fact 注入與去重雙雙失效** — [crates/duduclaw-memory/src/engine.rs#L1074](crates/duduclaw-memory/src/engine.rs#L1074)。`search()`/`search_layer()` 會 strip 特殊字元並包成 phrase,`search_facts` 不會。更糟:authenticated turn 的 query 被前綴 `[sender_id: {id}]`(含冒號=FTS5 column operator),幾乎每則 authenticated channel turn 都 MATCH 失敗。**情境**:Key Facts 從不注入,且去重被跳過導致同一 fact 每 turn 重存、`key_facts` 無界成長。**修復**:`search_facts` 套用相同 sanitize + phrase wrapping。

**Resume download 在 server 回 200(全 body)時 append 損毀檔案** — [crates/duduclaw-inference/src/model_registry/downloader.rs#L160](crates/duduclaw-inference/src/model_registry/downloader.rs#L160)。append/truncate 依 `existing_size` 而非回應 status;200 通過 success guard,全新 body 被 append 在 stale N bytes 之後並 rename 為最終 .gguf。**情境**:CDN/mirror 忽略 Range 回 200 時,使用者得到損毀 GGUF,稍後 parse 失敗無提示。**修復**:`existing_size>0` 且 status 為 200(非 206)時 truncate 從 0 重新開始。

**GVU rollback 完整性 hash 計算後從不持久化 → 完整性檢查永遠跳過** — [crates/duduclaw-gateway/src/gvu/version_store.rs#L214](crates/duduclaw-gateway/src/gvu/version_store.rs#L214)。INSERT 無 `rollback_diff_hash` 欄,`row_to_version` 硬寫 `None`,`execute_rollback` 的 `if let Some(...)` 永不進入。配合 `decrypt_rollback` 失敗時靜默回 ciphertext。**情境**:rollback_diff 在靜止時損毀/解密失敗時,垃圾內容被當 rollback 寫入 SOUL.md 無偵測。**修復**:新增 DB 欄位、寫入 INSERT、`row_to_version` 讀回,使完整性檢查在 finalizer 路徑生效。

**Governance quota 在後續 deny 時不釋放預留 → 永久洩漏配額** — [crates/duduclaw-governance/src/evaluator.rs#L438](crates/duduclaw-governance/src/evaluator.rs#L438)(兩項同根因:Rate-deny 不回滾、read-only op 也預留)。`evaluate_quota` 通過即 `pending_tasks++`/`token_reserved++`,但後續 Rate/Lifecycle deny 或 read-only op 永不呼叫 release。**情境**:N 次 rate-blocked 嘗試或 N 次 read 後 `concurrent+pending >= max` 永久 deny 直到 24h reset。**修復**:deny 路徑回滾預留;`evaluate_quota` 依 op_type 區分 read(不預留 task slot);提供 RAII guard 確保配對。

**Redaction token hash 截斷至 32-bit + token 為唯一 PK → 碰撞 clobber 映射** — [crates/duduclaw-redaction/src/token.rs#L129](crates/duduclaw-redaction/src/token.rs#L129)。`session_hash` 取 4 bytes,vault PK 僅 `token`,`INSERT OR REPLACE`。~77k 值約 50% 碰撞。**情境**:第二筆 REPLACE 第一筆 → (a) 錯誤明文還原(跨實體 PII 洩漏)或 (b) lookup 回 None(token 卡住=資料遺失)。**修復**:hash 用足夠位元(≥16 bytes),PK 改為 `(token, agent_id, session_id)` 複合鍵或拒絕碰撞。

**PTY interactive invoke 不 sanitize prompt 內換行 → 多行 prompt 提早送出截斷問題** — [crates/duduclaw-cli-runtime/src/session.rs#L490](crates/duduclaw-cli-runtime/src/session.rs#L490)。raw 寫入 prompt + 單一 `\r`;`format_history_as_prompt` 保證多行。**情境**:第一個 `\n` 被當 Enter 送出,只回答第一行,殘餘行污染下次 invoke buffer。**修復**:bracketed-paste 包裹或 strip/escape 換行後再送 `\r`。

**`drain_buffer()` 不排空 reader mpsc channel → 前一 turn 殘位元組污染** — [crates/duduclaw-cli-runtime/src/pty.rs#L298](crates/duduclaw-cli-runtime/src/pty.rs#L298)。`rx_buffer` 僅由 `read_until` 填充,invoke 間殘位元組留在 channel;per-turn drain 是 no-op。**情境**:`try_extract_interactive_answer` 配對最後兩個 sentinel,前一 turn 的 close sentinel 可造成錯誤/空 payload。**修復**:per-invoke drain 比照 boot path 的 `drain_window`(loop `read_until` 短窗)。

**PtyPool.semaphores DashMap 無界成長** — [crates/duduclaw-cli-runtime/src/pool.rs#L297](crates/duduclaw-cli-runtime/src/pool.rs#L297)(performance/HIGH)。每次 `acquire` 插入,`tick_eviction`/`remove_if_present`/`shutdown` 只移除 `sessions`。**情境**:持有效 bearer 的 client 以不同 agent/account/model triple loop Invoke,每個 unique key 新建 `Arc<Semaphore>` 永不回收 → OOM。**修復**:eviction 路徑一併移除 semaphores,或上限制限。

**SSE 連線永不 evict + endpoint 無 rate limit** — [crates/duduclaw-cli/src/mcp_http_server.rs#L335](crates/duduclaw-cli/src/mcp_http_server.rs#L335)(performance/HIGH)。`stream_handler` 不呼叫 rate_limiter,`evict_idle` 無生產呼叫者,斷線不移除(只 drop rx,tx 留在 HashMap)。**情境**:單一 key loop open/drop 使 HashMap + 1024-slot ring buffer 無界成長至 OOM。**修復**:加 rate limit、背景排程呼叫 `evict_idle`、斷線時移除連線。

**bus_queue.jsonl 多行程 append 無 advisory lock → 行交錯損毀靜默遺失** — [crates/duduclaw-bridge/src/lib.rs#L44](crates/duduclaw-bridge/src/lib.rs#L44)。`MAX_PAYLOAD_SIZE=1MiB` 遠超 PIPE_BUF;多 Python channel + Rust gateway 並行 append。**情境**:兩個大寫入交錯成 malformed JSONL,dispatcher 解析失敗永久保留碎片,原訊息永不送達。**修復**:append 加 flock/fcntl advisory lock(跨行程)。

**circuit_breaker HALF_OPEN 無 timeout/re-arm → 洩漏的 probe slot 永久卡死** — [crates/duduclaw-durability/src/circuit_breaker.rs#L335](crates/duduclaw-durability/src/circuit_breaker.rs#L335)。HALF_OPEN arm 不檢查時間,probe `after_call` 若未呼叫(panic/cancel/dropped),`probe_inflight` 卡 1,後續永遠 `CircuitOpen`。**修復**:HALF_OPEN probe 加 deadline,逾時重新 arm;或以 RAII guard 配對 before/after。

**idempotency check/record TOCTOU + 文件宣稱的 `check_and_record` 不存在** — [crates/duduclaw-durability/src/idempotency.rs#L170](crates/duduclaw-durability/src/idempotency.rs#L170)(PLAUSIBLE,目前無生產 caller)。`check()` 用 read lock,`record()` 用 write lock,中間無原子性。**修復**:實作真正原子的 `check_and_record`(單一 write lock + CAS),更新文件。

**memory_store 在儲存前 HTML-escape 自然語言 → 永久腐蝕資料** — [python/duduclaw/mcp/tools/memory/validation.py#L190](python/duduclaw/mcp/tools/memory/validation.py#L190)。`_sanitize` = `html.escape`,read 端不 unescape。**情境**:`if x < 3 && y > 1` 被存成 `if x &lt; 3 &amp;&amp; y &gt; 1` 回傳給 agent,原文不可復原。**修復**:memory 是純文字,移除 escape(改在 HTML 呈現層 escape)。

---

## 中度問題 (MEDIUM)

> 以下為 UNVERIFIED 的 MEDIUM,依維度分組。

### security
- [crates/duduclaw-gateway/src/server.rs#L1741](crates/duduclaw-gateway/src/server.rs#L1741) — `/api/reliability/summary` 未驗證且每次 request 跑 `sync_from_files`,洩漏 per-agent 指標 + I/O amplification — 加 auth + 共用長壽索引。
- [crates/duduclaw-gateway/src/server.rs#L42](crates/duduclaw-gateway/src/server.rs#L42) — login rate limiter 僅以 email 為 key 且成功不重置,可鎖死任意已知帳號 15 分鐘 — 改 IP-scoped + 成功時重置。
- [crates/duduclaw-gateway/src/feishu.rs#L173](crates/duduclaw-gateway/src/feishu.rs#L173) — verification_token 未設時接受未簽章事件,即使設了也只比對靜態 token 無 request signature — 未設時拒絕啟動 + 驗證 Encrypt Key/signature。
- [crates/duduclaw-cli/src/mcp.rs#L3826](crates/duduclaw-cli/src/mcp.rs#L3826) — `evolution_toggle` 未呼叫 `is_valid_agent_id`,path traversal 讀寫任意 `*/agent.toml` — 補驗證。
- [crates/duduclaw-cli/src/mcp.rs#L7406](crates/duduclaw-cli/src/mcp.rs#L7406) — `handle_wiki_write` 無跨 agent visibility 檢查(read 端有),可注入他 agent wiki — 補 `check_wiki_visibility`。
- [crates/duduclaw-cli/src/mcp_auth.rs#L259](crates/duduclaw-cli/src/mcp_auth.rs#L259) — stdio MCP 在無 key 設定時 fail-open 授予 all-scopes Admin internal principal — 改 fail-closed。
- [crates/duduclaw-gateway/src/computer_use.rs#L137](crates/duduclaw-gateway/src/computer_use.rs#L137) — mask region u32 溢位 panic(debug)/wrap 略過遮罩(release) — `saturating_add` + clamp。
- [crates/duduclaw-gateway/src/pty_runtime.rs#L756](crates/duduclaw-gateway/src/pty_runtime.rs#L756) — per-account PtyPool key 不隔離 OAuth(同 worker HIGH 同根因)。
- [crates/duduclaw-gateway/src/evolution_events/logger.rs#L288](crates/duduclaw-gateway/src/evolution_events/logger.rs#L288) — `scrub_metadata` 只洗 top-level `critique`/`reason`,巢狀與 W19-P1 欄位(`last_error`/`violation_detail`)明文落盤 — 遞迴洗淨。
- [crates/duduclaw-cli/src/mcp_headers.rs#L184](crates/duduclaw-cli/src/mcp_headers.rs#L184) — malformed capabilities header 被當 permissive 放行 — 解析失敗應拒絕。
- [crates/duduclaw-security/src/key_vault.rs#L76](crates/duduclaw-security/src/key_vault.rs#L76) — `resolve_agent_keys` 無視 allowed_channels 一律給全域 Anthropic key(目前無 caller,latent)。
- [crates/duduclaw-security/src/action_claim_verifier.rs#L208](crates/duduclaw-security/src/action_claim_verifier.rs#L208) — target_id 無錨點 substring match,短 id 誤判 Verified — 改錨定/精確比對。
- [crates/duduclaw-governance/src/evaluator.rs#L350](crates/duduclaw-governance/src/evaluator.rs#L350) — 同時在 requires_approval 與 denied_scopes 的 scope 被導向 approval 而非硬拒 — denied 優先檢查。
- [crates/duduclaw-cli/src/wizard.rs#L578](crates/duduclaw-cli/src/wizard.rs#L578) — company name 未跳脫直接內插進 TOML 字串,可 TOML 注入/損毀 — 用 `toml::Value` 建構。
- [crates/duduclaw-cli/src/service/mod.rs#L177](crates/duduclaw-cli/src/service/mod.rs#L177) — macOS `service stop` 對 `lsof -ti :PORT` 所有 PID SIGKILL,可能殺到無關行程 — 驗證 executable 為 duduclaw。
- [crates/duduclaw-security/src/input_guard.rs#L136](crates/duduclaw-security/src/input_guard.rs#L136) — prompt-injection 掃描純英文 substring 易繞過,且阻擋時未發 audit event。
- [crates/duduclaw-auth/src/jwt.rs#L58](crates/duduclaw-auth/src/jwt.rs#L58) — 既存 jwt_secret 被信任不檢查/修復權限,world-readable 即可偽造 token — 載入時驗 + 修復 owner-only。
- [crates/duduclaw-gateway/src/server.rs#L1131](crates/duduclaw-gateway/src/server.rs#L1131) — 失敗登入只 warn! 不寫 `auth_audit_log`,brute-force 不可稽核 — 記錄失敗事件。
- [crates/duduclaw-bridge/src/lib.rs#L89](crates/duduclaw-bridge/src/lib.rs#L89) — `send_to_bus` 不驗 chat_id/sender/text 長度/字元集。
- [crates/duduclaw-odoo/src/agent_config.rs#L60](crates/duduclaw-odoo/src/agent_config.rs#L60) — `company_ids` 解析但從不套用到 RPC context,跨公司隔離是 no-op。
- [crates/duduclaw-gateway/src/handlers.rs#L6039](crates/duduclaw-gateway/src/handlers.rs#L6039) — `scrub_odoo_error` 只截 240 字不洗 URL/credential,短錯誤訊息洩漏連線 URL/token。
- [crates/duduclaw-identity/src/providers/notion.rs#L170](crates/duduclaw-identity/src/providers/notion.rs#L170) — Notion 非 2xx body 嵌入錯誤訊息透傳給 LLM/log(latent)。

### correctness
- [crates/duduclaw-gateway/src/auth.rs#L56](crates/duduclaw-gateway/src/auth.rs#L56) — Ed25519 challenge 存於共享 `Mutex<Option<..>>`,並發 handshake 互相 clobber — 改 per-connection challenge。
- [crates/duduclaw-gateway/src/channel_settings.rs#L151](crates/duduclaw-gateway/src/channel_settings.rs#L151) — `get_allowed_channels` 無 global-scope fallback,global 白名單被忽略=全放行。
- [crates/duduclaw-gateway/src/line.rs#L369](crates/duduclaw-gateway/src/line.rs#L369) — LINE 長回覆無分段,超 Flex 限制送不出。
- [crates/duduclaw-gateway/src/feishu.rs#L211](crates/duduclaw-gateway/src/feishu.rs#L211) — `strip_feishu_mentions` 在多位元組空白(U+3000)邊界切片 panic。
- [crates/duduclaw-gateway/src/discord_voice.rs#L123](crates/duduclaw-gateway/src/discord_voice.rs#L123) — `to_asr_pcm` 假設 stereo,mono decode 產生錯誤取樣率 PCM。
- [crates/duduclaw-gateway/src/dispatcher.rs#L1037](crates/duduclaw-gateway/src/dispatcher.rs#L1037) — `poll_pending_taskspecs` 狀態翻轉在 spawned task 內,double-pickup 視窗。
- [crates/duduclaw-gateway/src/task_spec.rs#L707](crates/duduclaw-gateway/src/task_spec.rs#L707) — `verify_step_auto` 只要 output 含 criterion 任一關鍵字即 PASS,agent 複述目標即通過。
- [crates/duduclaw-gateway/src/worktree.rs#L469](crates/duduclaw-gateway/src/worktree.rs#L469) — detached-HEAD 下 atomic_merge 的 merge commit 變不可達 + 並發 dispatch 工作樹被換掉。
- [crates/duduclaw-gateway/src/computer_use_orchestrator.rs#L837](crates/duduclaw-gateway/src/computer_use_orchestrator.rs#L837) — `targets_sensitive_input` 硬編 false,High-risk 敏感輸入分支不可達。
- [crates/duduclaw-gateway/src/pty_runtime.rs#L456](crates/duduclaw-gateway/src/pty_runtime.rs#L456) — cache-hit/spawn 指標在並發下用 global session_count 取樣,計數倒置。
- [crates/duduclaw-gateway/src/gvu/updater.rs#L697](crates/duduclaw-gateway/src/gvu/updater.rs#L697) — feedback rollback 門檻 `pre.positive_feedback_ratio > 0.0`,無 feedback 檔時(常態)永不觸發。
- [crates/duduclaw-gateway/src/gvu/loop_.rs#L340](crates/duduclaw-gateway/src/gvu/loop_.rs#L340) — L3 judge prompt 用 narrative 而非 patch,patch-based 提案 L3 成橡皮圖章(與 HIGH patch-gap 同主題)。
- [crates/duduclaw-gateway/src/gvu/updater.rs#L408](crates/duduclaw-gateway/src/gvu/updater.rs#L408) — soul scanner <70 severity 只 log 放行,可拆分多個低分隱藏內容繞過。
- [crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs#L189](crates/duduclaw-gateway/src/skill_lifecycle/security_scanner.rs#L189) — exfil URL 僅 Warning + markdown-link 豁免可走私(與 skill HIGH 同主題)。
- [crates/duduclaw-gateway/src/skill_lifecycle/sandbox_trial.rs#L147](crates/duduclaw-gateway/src/skill_lifecycle/sandbox_trial.rs#L147) — 明顯有害 skill 在湊滿 5 對話前無法早期 discard。
- [crates/duduclaw-gateway/src/prediction/metacognition.rs#L587](crates/duduclaw-gateway/src/prediction/metacognition.rs#L587) — crit_proportion 用 lifetime cumulative,一旦 >0.2 每次評估 ratchet down 直到 0.4 floor,誤觸 emergency GVU。
- [crates/duduclaw-gateway/src/prediction/router.rs#L213](crates/duduclaw-gateway/src/prediction/router.rs#L213) — anti-sycophancy guard 死碼,`ConsistencyTracker::record` 無呼叫者。
- [crates/duduclaw-gateway/src/rl/trajectory_export.rs#L47](crates/duduclaw-gateway/src/rl/trajectory_export.rs#L47) — RL trajectory tool_calls 永遠 None,reward 近乎常數 ~0.7,無學習訊號(performance/MEDIUM)。
- [crates/duduclaw-gateway/src/evolution_events/query.rs#L643](crates/duduclaw-gateway/src/evolution_events/query.rs#L643) — skill/fallback rate 用全事件分母,新增 governance/durability 事件機械性壓低比率。
- [crates/duduclaw-gateway/src/reminder_scheduler.rs#L696](crates/duduclaw-gateway/src/reminder_scheduler.rs#L696) — 批次狀態寫入失敗的 reminder 卡 Pending + fired_ids,永久跳過至重啟。
- [crates/duduclaw-memory/src/engine.rs#L584](crates/duduclaw-memory/src/engine.rs#L584) — `semantic_conflict_count` 比較 RFC3339 vs SQLite datetime 格式,字串比較不可靠(search() 已修)。
- [crates/duduclaw-inference/src/router.rs#L98](crates/duduclaw-inference/src/router.rs#L98) — fast-keyword 用 `contains`,"hi"/"list" 命中 "this"/"realistic" 誤導本地路由 — 改 word-boundary。
- [crates/duduclaw-agent/src/resolver.rs#L49](crates/duduclaw-agent/src/resolver.rs#L49) — agent 解析迭代 HashMap 非決定性順序,多 agent match 時路由不穩定。
- [crates/duduclaw-agent/src/registry.rs#L123](crates/duduclaw-agent/src/registry.rs#L123) — 同名 agent 目錄靜默互相覆蓋(last scan wins)無 WARN。
- [crates/duduclaw-governance/src/registry.rs#L196](crates/duduclaw-governance/src/registry.rs#L196) — policy merge 僅以 policy_id 去重,不同 type 同 id 的 agent policy 抹除 global policy。
- [crates/duduclaw-governance/src/approval.rs#L331](crates/duduclaw-governance/src/approval.rs#L331) — `cleanup_expired` 不移除 Approved/Rejected,pending map 無界成長。
- [crates/duduclaw-redaction/src/engine.rs#L42](crates/duduclaw-redaction/src/engine.rs#L42) — `from_specs` 不以 id 去重,跨 profile 同 id rule 雙重 compile/fire,違反 override-by-id 契約。
- [crates/duduclaw-redaction/src/pipeline.rs#L100](crates/duduclaw-redaction/src/pipeline.rs#L100) — 非 system-prompt 來源的 `Selective` 落入 full-redact arm,等同 On 過度遮罩。
- [crates/duduclaw-cli-runtime/src/session.rs#L650](crates/duduclaw-cli-runtime/src/session.rs#L650) — `collect_response` 丟棄 frame 後尾位元組不 re-buffer,reused session 遺失下次回應開頭。
- [crates/duduclaw-cli-runtime/src/session.rs#L571](crates/duduclaw-cli-runtime/src/session.rs#L571) — ReadTimeout 空 drain 持續 busy-spin 至 deadline,持 semaphore permit 達 3 分鐘。
- [crates/duduclaw-cli-worker/src/server.rs#L554](crates/duduclaw-cli-worker/src/server.rs#L554) — work_dir 分歧只在 cached spawn_cwd 為 Some 時警示,首次 None→後續 Some 靜默忽略,agent 無 per-agent MCP/CLAUDE.md。
- [crates/duduclaw-durability/src/retry.rs#L152](crates/duduclaw-durability/src/retry.rs#L152) — `delay_for_attempt` 在 capped delay 0 時 `gen_range(0.0..0.0)` panic。
- [crates/duduclaw-durability/src/checkpoint.rs#L144](crates/duduclaw-durability/src/checkpoint.rs#L144) — `max_checkpoints` 宣稱防 OOM 但 save() 從不強制(performance/MEDIUM)。
- [crates/duduclaw-license/src/license.rs#L115](crates/duduclaw-license/src/license.rs#L115) — `validate()` 不檢查 tier/deployment-mode 綁定,cloud-only tier 在 self-host 通過。
- [crates/duduclaw-gateway/src/license_runtime.rs#L469](crates/duduclaw-gateway/src/license_runtime.rs#L469) — 撤銷不持久化,`delete_default()` 失敗則重啟重新啟用至下次 24h CRL poll。
- [crates/duduclaw-odoo/src/events.rs#L84](crates/duduclaw-odoo/src/events.rs#L84) — `poll_model` 在 search_read 返回後才記 `Utc::now()`,round-trip 間寫入的紀錄永久跳過。
- [crates/duduclaw-odoo/src/connector.rs#L195](crates/duduclaw-odoo/src/connector.rs#L195) — write/count 以 `unwrap_or(false/0)` 吞掉非預期 RPC 結果,未套用的 write 報成功。
- [crates/duduclaw-identity/src/providers/wiki_cache.rs#L197](crates/duduclaw-identity/src/providers/wiki_cache.rs#L197) — `extract_frontmatter` 在首個 `\n---` substring 截斷,內含 `---` 的值使整筆紀錄靜默丟棄。
- [python/duduclaw/mcp/tools/memory/validation.py#L122](python/duduclaw/mcp/tools/memory/validation.py#L122) — `memory_search` 搜尋前 HTML-escape query,降低召回。
- [python/duduclaw/mcp/tools/memory/store.py#L115](python/duduclaw/mcp/tools/memory/store.py#L115) — 每日寫入配額 check-then-await-then-increment,並發可超限。
- [crates/duduclaw-memory/src/engine.rs#L310](crates/duduclaw-memory/src/engine.rs#L310) — `store_temporal` 的 `supersedes` 反指標 SELECT 無 ORDER BY,多筆 active 時非決定性(LOW 邊界)。
- [web/src/components/ApprovalModal.tsx#L27](web/src/components/ApprovalModal.tsx#L27) — 第二個 approval_request 覆蓋第一個,前者永不被回覆 — 改 queue。
- [web/vite.config.ts#L17](web/vite.config.ts#L17) — dev proxy 只轉 `/ws` `/health`,`/api/login` 等在 `npm run dev` 下登入失敗(dev-only)。
- [python/duduclaw/tools/agent_tools.py#L236](python/duduclaw/tools/agent_tools.py#L236) — `_set_status`/`agent_status` 對 `name` 無驗證,path traversal 讀寫(latent,security/MEDIUM)。

### performance
- [crates/duduclaw-gateway/src/handlers.rs#L4527](crates/duduclaw-gateway/src/handlers.rs#L4527) — 每次 audit/reliability RPC 開新 SQLite 連線並重跑 `sync_from_files` 全檔掃描,熱路徑 O(total-audit-history) — 共用長壽 `Arc<AuditEventIndex>` + 背景同步。
- [crates/duduclaw-gateway/src/reminder_scheduler.rs#L792](crates/duduclaw-gateway/src/reminder_scheduler.rs#L792) — agent_callback 持 semaphore permit 跨無界 LLM 呼叫 + 2s retry sleep,4 個慢 callback 卡死所有 reminder。

### usability
- [web/src/stores/auth-store.ts#L128](web/src/stores/auth-store.ts#L128) — refresh token 存 localStorage,XSS 可讀(security/MEDIUM)— 改 httpOnly+SameSite cookie。

---

## 輕微 / 清理建議 (LOW)

- [crates/duduclaw-gateway/src/handlers.rs#L7134](crates/duduclaw-gateway/src/handlers.rs#L7134) — `handle_tasks_assign` 死的 `update` binding,移除。
- [crates/duduclaw-gateway/src/channel_sender.rs#L70](crates/duduclaw-gateway/src/channel_sender.rs#L70) — 同 user_id 並發確認覆蓋前一 oneshot,前者靜默 decline;[L98](crates/duduclaw-gateway/src/channel_sender.rs#L98) — 任何非肯定訊息被當 decline 並消耗 pending。
- [crates/duduclaw-gateway/src/telegram.rs#L448](crates/duduclaw-gateway/src/telegram.rs#L448) — Telegram/Slack/Discord 多處 status/help 為英文,違反 zh-TW;[dispatcher.rs#L653](crates/duduclaw-gateway/src/dispatcher.rs#L653) — 系統注入的 permission 警告英文且洩漏內部 CLI flag。
- [crates/duduclaw-gateway/src/slack.rs#L424](crates/duduclaw-gateway/src/slack.rs#L424) — Slack split 用 byte 閾值,長 CJK 無換行可 char-boundary panic。
- [crates/duduclaw-gateway/src/worker_supervisor.rs#L316](crates/duduclaw-gateway/src/worker_supervisor.rs#L316) — `cap_rust_log_verbosity` substring replace "trace" 損毀 target 名。
- [crates/duduclaw-gateway/src/skill_lifecycle/sandbox_trial.rs#L317](crates/duduclaw-gateway/src/skill_lifecycle/sandbox_trial.rs#L317) — `graduate_skill_to_disk` 用 skill.name 建檔名無 path 驗證(MCP 路徑有 `is_safe_path_component`)。
- [crates/duduclaw-gateway/src/rl/trajectory_export.rs#L60](crates/duduclaw-gateway/src/rl/trajectory_export.rs#L60) — `session_id[..8]` byte slice 潛在 panic — 用 `chars().take(8)`。
- [crates/duduclaw-gateway/src/evolution_events/logger.rs#L230](crates/duduclaw-gateway/src/evolution_events/logger.rs#L230) — 寫入錯誤時當前 event 不 retry 直接丟棄。
- [crates/duduclaw-gateway/src/autopilot_engine.rs#L756](crates/duduclaw-gateway/src/autopilot_engine.rs#L756) — `notify` 的 Discord chat_id 未驗 snowflake 直接內插 path。
- [crates/duduclaw-gateway/src/cost_telemetry.rs#L526](crates/duduclaw-gateway/src/cost_telemetry.rs#L526) — output_tokens i64→u64 未過 `safe_u64`,負值 wrap。
- [crates/duduclaw-cli/src/mcp.rs#L7435](crates/duduclaw-cli/src/mcp.rs#L7435) — `handle_wiki_write` 缺 read 端的 `canonicalize()` symlink-escape 守衛。
- [crates/duduclaw-cli/src/mcp_auth.rs#L228](crates/duduclaw-cli/src/mcp_auth.rs#L228) — key age `num_days() as u64` 未 `.max(0)`,未來日期產生荒謬天數。
- [crates/duduclaw-memory/src/shared.rs#L225](crates/duduclaw-memory/src/shared.rs#L225) — shared.rs/federated.rs 未在 lib.rs 宣告(死碼),引用不存在的 `sanitize_fts5_query`,一旦接上即編譯失敗。
- [crates/duduclaw-inference/src/mlx_bridge.rs#L67](crates/duduclaw-inference/src/mlx_bridge.rs#L67) — `is_available` OnceCell 永久快取首次 probe,transient 失敗永久停用 MLX。
- [crates/duduclaw-inference/src/llamafile.rs#L120](crates/duduclaw-inference/src/llamafile.rs#L120) — `start()` check-then-set TOCTOU,並發兩個 start 各 spawn 一個 process。
- [crates/duduclaw-security/src/secret_manager/mod.rs#L165](crates/duduclaw-security/src/secret_manager/mod.rs#L165) — `vault_token_enc` 被解析但從不解密/建 adapter,文件誤導。
- [crates/duduclaw-agent/src/account_rotator.rs#L407](crates/duduclaw-agent/src/account_rotator.rs#L407) — LeastCost 永遠選第一個 OAuth,多帳號不輪替。
- [crates/duduclaw-governance/src/quota_manager.rs#L130](crates/duduclaw-governance/src/quota_manager.rs#L130) — `consume_tokens` 用 `>` 而 `check_token_budget` 用 `>=`,邊界語意不一致。
- [crates/duduclaw-redaction/src/pipeline.rs#L248](crates/duduclaw-redaction/src/pipeline.rs#L248) — 過期 token 還原發 `RestoreOk`,稽核高估 PII 揭露數。
- [crates/duduclaw-redaction/src/profiles/taiwan_strict.toml#L6](crates/duduclaw-redaction/src/profiles/taiwan_strict.toml#L6) — 國民身分證 regex 無 word boundary/checksum,全形/分隔/小寫變體漏抓。
- [crates/duduclaw-cli-runtime/src/session.rs#L199](crates/duduclaw-cli-runtime/src/session.rs#L199) — `pid()` 為 None 時 process-group 終止被略過,洩漏孤兒子行程。
- [crates/duduclaw-core/src/types.rs#L184](crates/duduclaw-core/src/types.rs#L184) — `RuntimeType::parse` 未知 provider 靜默映為 Claude,掩蓋 typo;[agent_guard.rs#L154](crates/duduclaw-core/src/agent_guard.rs#L154) — `.claude/agents/` 只比對正斜線,Windows 反斜線繞過守衛;[agent_guard.rs#L104](crates/duduclaw-core/src/agent_guard.rs#L104) — case-sensitive `starts_with`,大小寫不敏感 FS 上誤擋合法路徑。
- [crates/duduclaw-durability/src/circuit_breaker.rs#L400](crates/duduclaw-durability/src/circuit_breaker.rs#L400) — `after_call` 成功在無 probe inflight 時也增 consecutive_successes,可提前 close。
- [crates/duduclaw-cli-worker/src/main.rs#L101](crates/duduclaw-cli-worker/src/main.rs#L101) — `expand_home` 在 HOME 無解時回字面 `~/.duduclaw`,work_dir 驗證一律拒絕並誤導。
- [crates/duduclaw-cli-worker/src/server.rs#L375](crates/duduclaw-cli-worker/src/server.rs#L375) — Bearer auth 在 JSON body 完整反序列化後才執行;[crates/duduclaw-cli-worker/src/server.rs#L554] 見 MEDIUM。
- [crates/duduclaw-gateway/src/license_runtime.rs#L118](crates/duduclaw-gateway/src/license_runtime.rs#L118) — `registry_keys` log 只回 0/1 而非真實 key 數;[L190](crates/duduclaw-gateway/src/license_runtime.rs#L190) — `from_state` 每次 snapshot 重算 fingerprint(hostname+MAC 枚舉)— 快取(performance)。
- [crates/duduclaw-bus/src/router.rs#L61](crates/duduclaw-bus/src/router.rs#L61) — trigger 用 `contains(agent_id)` 子字串誤路由(latent crate)。
- [crates/duduclaw-desktop/src/controller.rs#L183](crates/duduclaw-desktop/src/controller.rs#L183) — `key_press` split('+') 使 `ctrl++` 產生空 segment,靜默無鍵。
- [crates/duduclaw-cli/src/service/mod.rs#L238](crates/duduclaw-cli/src/service/mod.rs#L238) — macOS `service logs` 指向 `/tmp/` 但 launchd 寫 `~/Library/Logs/`(usability)。
- [python/duduclaw/evolution/vetter.py#L70](python/duduclaw/evolution/vetter.py#L70) — secret regex 漏抓 `github_pat_*` fine-grained PAT。
- [python/duduclaw/memory_eval/db/snapshots.py#L95](python/duduclaw/memory_eval/db/snapshots.py#L95) — insert 成功以 `'INSERT 0 1'` exact match,oid 形式漏算。
- [crates/duduclaw-auth/src/db.rs#L43](crates/duduclaw-auth/src/db.rs#L43) — `conn()` fallback `pool[0].lock().unwrap()` 在 poisoned mutex panic,可致全 auth DoS;[db.rs#L525](crates/duduclaw-auth/src/db.rs#L525) — `ensure_default_admin` 註解宣稱 transaction 但實際無,靠 UNIQUE+INSERT OR IGNORE。
- [crates/duduclaw-identity/src/providers/notion.rs#L228](crates/duduclaw-identity/src/providers/notion.rs#L228) — `resolve_by_channel` 只取 `results[0]`,多筆同 handle 靜默歧義(同 wiki-cache 主題)。
- [web/src/pages/LoginPage.tsx#L24](web/src/pages/LoginPage.tsx#L24) — 登入直接顯示 server 原始英文錯誤,未 zh-TW 本地化。

---

## 跨領域主題

1. **授權 scope 在分發層系統性缺失 (fail-open by omission)**。重複出現於 MCP 內部 principal(`tool_requires_scope` 預設 `None`)、dashboard dispatch(wiki/tasks 缺 `check_agent!`)、wiki_write 無 visibility、odoo_execute verb 分類、stdio MCP fail-open。**系統性修復**:把授權改為「預設拒絕、明確放行」——未列舉工具/方法一律要求最高 scope,並以單一 enforcement helper 統一所有 dispatch 入口。

2. **安全/學習機制「已實作但從未被呼叫或從未被寫入」的死碼**。satisfaction 模型(`update_from_feedback`)、sycophancy 偵測(`ConsistencyTracker::record`)、rollback 完整性 hash、`allowed_tools` 白名單、`evict_idle`、`is_stale()`、`vet_synthesized_skill`、`SecretManager` vault adapter、shared/federated memory module 全屬此類。**修復**:建立「死碼/未接線安全功能」清單,逐一接線或刪除,並加整合測試確保安全 invariant 真的被執行(而非僅單元測試直接操作 DB/struct 繞過)。

3. **fail-open 安全閘**。容器隔離(Docker/Apple)、screenshot 遮罩、LINE/Feishu 簽章、SOUL.md hash 自癒、CRL freshness、license validate 全在「設定缺失/偵測失敗」時放行而非拒絕。**修復**:統一安全閘為 fail-closed,缺失設定時拒絕啟動該功能並明確報錯。

4. **UTF-8 byte-slice panic**。`task_status`、`strip_feishu_mentions`、Slack split、`trajectory_id`、`session_id[..8]` 皆對非 ASCII 用 byte 索引切片。**修復**:全 codebase 禁用 `&s[..n]` 形式截斷,改用 `char_indices`/`floor_char_boundary` 共用 util。

5. **「掃描/驗證的對象 ≠ 實際生效的 artifact」**。GVU 掃 `proposal.content` 但寫 `patch.content`;L3 judge 評 narrative;skill scanner 行級 + markdown-link 豁免。**修復**:所有內容安全檢查必須對「最終寫入/最終生效」的 bytes 執行。

6. **子字串/前綴比對缺邊界**(security + correctness 兼具)。Origin allowlist、agent trigger、agent_id 路由、inference keyword、action-claim、bus router、odoo URL 全用無錨點 `contains`/`starts_with`。**修復**:改 word-boundary/精確 host:port/錨定比對。

7. **多行程/並發共享狀態無鎖或 TOCTOU**。bus_queue.jsonl append、Ed25519 challenge、idempotency、quota、approval cleanup、llamafile start、auth pool poisoning。**修復**:跨行程用 advisory lock,行程內用原子 CAS 或單一 write lock。

8. **PTY worker 的 OAuth per-account 隔離未實作**(worker + pty_runtime 雙處),直接破壞 multi-OAuth rotation 這項招牌功能的計費/限流保證。

9. **zh-TW 本地化不一致**:多處 channel/login 錯誤訊息為英文並洩漏內部實作細節,違反 CLAUDE.md UX 要求。

---

## 建議優先順序

1. **立即修補 6 個 CRITICAL**(可遠端利用 / 完全繞過隔離):預設 admin 密碼、MCP 內部 scope gate、redaction egress scope、license 刷新 validate、`/ws/chat` 無認證、容器 sandbox 隔離(含 Apple stub)。這些是可直接導致 RCE、PII 洩漏、完整 admin 接管或授權繞過的問題。

2. **修補可遠端觸發的 HIGH security**:LINE 空簽章 fail-open、Origin allowlist 繞過、wiki/tasks dispatch 授權缺口、screenshot 遮罩 fail-open、skill gate 放行 Error、SSE 跨租戶注入、odoo_execute verb 繞過、Odoo SSRF redirect、identity 冒充、SOUL.md hash 自癒、`allowed_tools` 未強制、CRL 重放。

3. **修補導致靜默資料遺失/損毀的 HIGH correctness**:bus_queue 行交錯損毀、PTC pipe 死鎖、resume download 損毀、GVU patch-gap、rollback hash 死碼、memory search_facts、Telegram/Discord/LINE 訊息送不出、`task_status` panic、container PTC 工具橋斷裂、worktree 誤 merge。

4. **接線或移除死碼安全/學習機制**(主題 2、3):satisfaction/sycophancy 模型、rollback 完整性、`evict_idle`、CRL freshness、vetting、PtyPool semaphore eviction。

5. **建立共用 util 消除系統性缺陷**(主題 4、6):UTF-8 安全截斷、word-boundary 比對、跨行程 advisory lock、fail-closed 安全閘 helper、統一授權 enforcement 入口。

6. **MEDIUM 安全與 governance 一致性**:feishu 簽章、jwt_secret 權限、失敗登入稽核、quota release 回滾、policy merge 去重、path traversal 驗證。

7. **LOW 清理與 zh-TW 本地化**:死碼刪除、log 修正、訊息本地化、邊界值守衛。

相關檔案路徑均以上方 markdown 連結列出(皆為 repo 相對路徑;絕對路徑前綴為 `/Users/lizhixu/Project/DuDuClaw/`)。
