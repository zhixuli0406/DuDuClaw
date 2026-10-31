# Enterprise 雙因素登入：第一階段實作紀錄

> 原始紀錄的 artifacts 連結不在 repo 內。


狀態：IMPLEMENTED。Phase 1 程式與文件已實作；指定測試全部完成、0 failed：auth 100 passed，gateway mfa／login／otp 分別 8／42／38 passed，CLI mfa 3 passed。login 另有2個既有 ignored；實際 socket／原生平台／客戶驗收與獨立安全審查為 NOT RUN，不能視為已發版或正式環境驗收。此檔及 phase1-brief.md 皆為 gitignored 工作產物。

分支：`feat/enterprise-mfa-20261031`。未 commit、push、stash、切換分支或執行 workspace cargo fmt；未啟動審查子代理。使用既有 worktree，沒有新增或移除 worktree。新增 MFA 測試使用隔離的臨時資料夾，未對正式客戶設定或帳號操作。

## Token paths found

| 路徑 | 實作位置 | MFA 處理 |
|---|---|---|
| 密碼 `/api/login` | `crates/duduclaw-gateway/src/server.rs:3207`、`:3219` | 先經共用第二因素／強制註冊判斷；一般簽發再用交易確認未註冊，避免 enrollment 競爭 |
| 通道 OTP `/api/otp/verify` | `crates/duduclaw-gateway/src/server.rs:3368`、`:3379` | 相同 MFA 關卡；OTP 通過不代表已通過第二因素 |
| refresh `/api/refresh` | `crates/duduclaw-gateway/src/server.rs:3719`、`:3732` | Enterprise 強制且未註冊時拒絕；保留原 JWT session 身分 |
| Personal `/api/session/local` | `crates/duduclaw-gateway/src/server.rs:4399` | 原有免密碼簽發不變 |
| 新增 MFA 完成簽發 | `crates/duduclaw-gateway/src/server_mfa.rs:91` | 僅 verify 或 enrollment confirm 成功消耗憑證後呼叫 |

全 workspace 搜尋紀錄：`artifacts/mfa/token-paths.txt`。未找到其他正式 dashboard 使用者 JWT 簽發入口。`webchat.rs:1979` 及 server 中其他 `issue_access_token` 呼叫都在測試；CLI 的 `mcp_refresh::issue_refresh_token` 是 MCP client 憑證，與 dashboard JWT 分開。`duduclaw-auth/src/jwt.rs` 為上述呼叫的底層簽發 API。

## Schema added

全部以 `CREATE TABLE IF NOT EXISTS` 加到既有 `users.db`；不修改既有 `users` 欄位或資料，也不變更 User JSON 格式。

| 表格 | 欄位／用途 |
|---|---|
| `mfa_credentials` | `user_id` 主鍵、`secret_enc`、`last_step`、`failed_attempts`、`failure_window`；啟用狀態以此列存在為準，無法解密仍視為啟用並拒絕驗證 |
| `mfa_pending` | `user_id` 主鍵、`binding`、`secret_enc`、`expires_at`、`attempts`；尚未啟用的註冊，綁 dashboard 本人或註冊憑證雜湊 |
| `mfa_recovery_codes` | `(user_id, code_hash)` 複合主鍵；10 組128位元隨機備援碼，Argon2 雜湊保存，消耗即刪除，重產全部取代 |
| `mfa_challenges` | `token_hash` 主鍵、`user_id`、`first_factor`（password/channel_otp）、`purpose`（login/enroll）、`expires_at`、`attempts`、`consumed`；256位元隨機憑證只存 SHA-256 雜湊；5分鐘、最多5次、單次成功 |

索引：`idx_mfa_challenges_user(user_id)`。驗證、最後接受的 TOTP step、challenge 消耗、備援碼消耗及相關成功稽核都在 `BEGIN IMMEDIATE` 交易內；失敗嘗試也會提交。有效帳號另外有每15分鐘5次第二因素錯誤限制，HTTP 併用既有登入與 OTP 的 IP 限流。稽核沿用 `auth_audit_log`，不記錄密鑰、驗證碼或備援碼。

## Crypto key source

`<gateway home>/.keyfile` 的32位元組 AES-256 金鑰，與通道憑證的 `CryptoEngine`／`config_crypto::decrypt_value` 相同格式。auth crate 只接收 `MfaCipher` 介面，不取得或儲存機器金鑰。160位元 TOTP secret 以帳號 ID 與 secret 的 JSON 組合加密，解密後核對帳號，避免跨帳號交換 ciphertext。

第一次加密且金鑰確實不存在時，以 `create_new` 建立（Unix 0600）；已存在但不可讀或長度錯誤的金鑰不覆寫。解密絕不建立新金鑰，失敗時 TOTP 與備援碼都拒絕。驗證值使用 constant-time comparison；[RFC 6238 Appendix B](https://www.rfc-editor.org/rfc/rfc6238) 六個 SHA-1 向量與6位數截斷皆有測試。未新增 TOTP 套件，只引用 repo 已使用的 `hmac 0.12`、`sha1 0.10`、`subtle 2`。

## Live sessions when enforcement turns on

`[auth] mfa_enforced` 預設 false，只在 Enterprise 生效。未註冊且未強制時，密碼／通道 OTP 回應仍為原有 `access_token`、`refresh_token`、`user`。設定檔不可讀、無法解析或 Enterprise 的值型別不正確時拒絕；Personal 忽略可讀設定中的強制開關。

啟用強制後不立即撤銷 access token、不取消執行中的工作。未註冊者的 refresh 立即拒絕，原 access token 按自身期限自然到期，預設30分鐘。發現既有 WebSocket 原先不檢查 JWT 到期，因此補上限縮條件：Enterprise 已註冊或強制期間，於原 access token 到期後的下一次30秒心跳關閉；Personal 及未註冊且未強制的正常設定不改變。

已註冊帳號的舊 refresh token 保留原期限（預設7天），不追溯要求既有 session 提交第二因素。Personal 自願註冊會保護密碼及通道 OTP，但 `/api/session/local` 與 Personal WebSocket 行為維持原狀。

## What changed (file:line)

- `crates/duduclaw-auth/src/mfa.rs:36`：schema、TOTP、pending enrollment、持久 challenge、單次備援碼、驗證錯誤限制、交易內稽核與 reset。
- `crates/duduclaw-auth/src/db.rs:131`、`src/lib.rs:22`、`Cargo.toml:17`、根目錄 `Cargo.lock`：啟用 migration、公開模組與既有版本的 crypto 相依。
- `crates/duduclaw-gateway/src/mfa.rs:6`、`:48`：機器加密、嚴格設定讀取及 current Admin 檢查。
- `crates/duduclaw-gateway/src/server_mfa.rs:31`、`:122`、`:196`：共用第一因素判斷、refresh／socket 到期判斷、獨立 HTTP handlers。新 HTTP 成功回應加 `Cache-Control: no-store`。
- `crates/duduclaw-gateway/src/server.rs:2359`、`:3207`、`:3368`、`:3719`、`:15809`、`:16093`：路由及共用函式接線；大檔內未放入 MFA 引擎。
- `crates/duduclaw-gateway/src/handlers/mfa_rpc.rs:4`、`handlers/mod.rs:510`、`handlers/dispatch_org.rs:83`：七個 RPC 的明列權限、本人註冊與敏感操作、Admin overview/reset。未知 MFA 方法在 extension dispatch 前即拒絕。
- `crates/duduclaw-gateway/src/handlers/system_update_config.rs:41`、`handlers/config_raw_rpc.rs:850`、`handlers/config_schema.rs:73`：強制開關可寫、兩種 dashboard 寫入都重讀有效 Admin，typed schema 驗證布林值。
- `crates/duduclaw-cli/src/users_mfa_cmd.rs:23`、`src/lib.rs:1686`、`:6457`、`Cargo.toml:16`：`users mfa-reset <email>`、AI marker 拒絕、既有 Bash operator command guard 接線與正常 auth 相依。
- `crates/duduclaw-auth/src/mfa/tests.rs:1`、`crates/duduclaw-gateway/src/mfa_http_tests.rs:1`、MFA policy／authorization／CLI 檔內測試：單元及無 socket 的 handler 回歸測試。
- `CHANGELOG.md:5`、`docs/api/openapi.yaml:61`：Unreleased 與登入／MFA／refresh／RPC 契約，註明 Dashboard 畫面屬下一階段。
- `docs/features/05-security-defense.md:358`、`docs/features/zh-TW/05-security-defense.md:358`、`docs/features/ja-JP/05-security-defense.md:359`：三語安全流程與限制。
- `docs/guides/dashboard-settings.md:339`、`docs/guides/zh-TW/dashboard-settings.md:326`、`docs/guides/ja-JP/dashboard-settings.md:334`：三語開關操作。
- `docs/guides/deprecations.md:240`、`docs/guides/zh-TW/deprecations.md:200`、`docs/guides/ja-JP/deprecations.md:220`：三語條件式回應相容性說明，沒有移除舊端點。

### 最後管理員救援的範圍

CLI 是指定的最後管理員救援例外，不需要無法完成的 dashboard MFA／approval：交易內限制只有一位有效 Admin，且 email 必須對應該人。其他情況必須走目前 Admin 的 `users.mfa_reset`。共用 `ai_session_guard` 加 Codex／Claude marker、既有 Bash lane 都會拒絕 AI，成功稽核的 actor 標示「本機指令列（身分未驗證）」。環境變數與 Bash guard 不構成 OS 隔離，任意主機檔案寫入仍可繞過產品限制。

## Test result lines

所有 cargo 指令皆加 `CARGO_INCREMENTAL=0`、`-j 4`，一次只執行一個 cargo。

- `npm ci --offline && npm run build`（web）：exit 0，完成 TypeScript／Vite build。
- `cargo test -j 4 -p duduclaw-auth`：`test result: ok. 100 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 33.16s`。Doc-tests：0 passed、0 failed。證據 `artifacts/mfa/auth-test-final.log`。
- gateway `--lib mfa`：`test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 10291 filtered out; finished in 5.11s`。證據 `artifacts/mfa/gateway-mfa-test-final.log`。
- gateway `--lib login`：`test result: ok. 42 passed; 0 failed; 2 ignored; 0 measured; 10255 filtered out; finished in 3.04s`。證據 `artifacts/mfa/gateway-login-test.log`。
- gateway `--lib otp`：`test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 10261 filtered out; finished in 3.99s`。證據 `artifacts/mfa/gateway-otp-test.log`。
- CLI `--lib mfa`：`test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 2335 filtered out; finished in 0.51s`。證據 `artifacts/mfa/cli-mfa-test.log`。
- `git diff --check`：通過。
- OpenAPI：YAML 可解析、無重複鍵，共91條路徑，10個專用 MFA HTTP/RPC 路徑齊備。證據 `artifacts/mfa/openapi-check.log`。

初次 gateway 編譯發現 CLI 的 auth 只列於 dev-dependencies，已補上正常 dependencies；初次失敗紀錄保留在 `artifacts/mfa/gateway-mfa-test.log`。第二次執行 7 passed／1 failed：失敗斷言錯將 Personal 的可讀 `mfa_enforced=true` 設定視為應拒絕，與「Personal 忽略強制」規格矛盾；已修正該測試斷言，沒有放寬實作。失敗紀錄保留在 `artifacts/mfa/gateway-mfa-test-attempt2.log`，最終重跑另存檔。

## NOT RUN

- `cli_auth::tests::live_claude_login_streams_output`：既有 ignored，需要真的執行 `claude setup-token`。
- `handlers::tests::live_acl_tests::live_acl_latency_with_two_concurrent_logins`：既有 ignored，需另行指定效能量測。

- 真實 TCP／WebSocket 握手、30分鐘實際到期／重連與外部通道送達：依本次指定的 socket-binding 沙箱限制未執行；無 socket handler／認證函式測試不等同真實線路驗收。本次已執行的 gateway 篩選測試沒有 socket 拒絕錯誤。
- Windows／Linux 原生驗收、正式環境客戶驗收、完整 workspace 測試及 release gates。
- 獨立安全審查／審查子代理：依本次指令未執行。

## What is left

本次指定的 Phase 1 實作與 cargo 篩選測試沒有未完成項目；以下是明確保留的後續工作及驗收邊界。

- Dashboard 註冊、備援碼與第二因素登入 UI 為下一階段；正式啟用強制前需有支援新回應的客戶端。
- 在可綁 socket 的驗收環境驗證實際 `/ws` 拒絕註冊憑證、session 到期重連及通道 OTP 完整流程，再由審查者檢查 diff。尚未發版、部署或調整任何客戶的 `mfa_enforced`。

## Round 2

狀態：八項修補已實作；最終指定測試全部 exit 0：auth 104、gateway MFA 12／login 42／OTP 38、CLI MFA 4 passed，0 failed；login 另有 2 個既有 ignored。尚未完成實際線路／客戶驗收與獨立安全審查。這一節取代 Round 1 中「備援碼也需解密」、「已註冊者舊 refresh 保留」、「僅最後一位 Admin 可用 CLI」、「每次登入讀設定」等舊描述。

沿用 `feat/enterprise-mfa-20261031` 與既有 worktree；未 commit、push、stash、切換分支、執行 workspace cargo fmt 或啟動子代理。沒有修改正式帳號／設定、部署或發版；本 session 沒有新增 worktree，因此沒有自行刪除既有工作目錄。

### Per item — what changed (file:line)

| 項目 | 本輪修改與驗證範圍 |
|---|---|
| 1 HIGH：強制首次註冊的獨立證明 | `crates/duduclaw-auth/src/mfa/enrollment.rs:62`、`:92`、`:116`；`crates/duduclaw-gateway/src/server_mfa.rs:31`、`handlers/mfa_rpc.rs:89`。通道 OTP 使用既有 OTP 亂數產生器、Argon2 與 delivery 介面，但保存在專用註冊證明表，不能當成一般 OTP 登入；沒有已驗證通道時使用另一位目前 Admin 或主機操作員核發的 128-bit 隨機單次碼，只存 SHA-256，5 分鐘、只顯示一次、禁止 Admin 自行核發、稽核不含秘密。證明綁定 user／第一因素 challenge，最多 5 次嘗試，Admin 碼消耗與綁定同一交易。通道 OTP 作為第一因素時還需目前密碼。已開始但未確認的自願註冊，在政策切成強制後也不能跳過獨立證明。測試包含只有密碼無法開始強制註冊、跨帳號／challenge／用途拒絕、TTL、次數及單次消耗。 |
| 2 HIGH：JWT 不能單獨註冊 | `crates/duduclaw-auth/src/mfa/enrollment.rs:30`、`:51`；`crates/duduclaw-gateway/src/handlers/mfa_rpc.rs:30`、`:48`。begin 需要目前密碼，在 blocking pool 使用既有慢速密碼驗證並限流；token 綁定 transport 提供的 user＋JWT jti 與 5 分鐘期限。confirm 必須帶該 token 且同一工作階段；dashboard token 不能移到 HTTP enrollment 交換 session。替換維持先以新因素 disable、再以目前密碼重新註冊，不能直接覆寫啟用中的密鑰。測試包含無密碼、錯密碼、不同 jti／user、過期及 HTTP 交換拒絕。 |
| 3 HIGH：憑證 epoch 與舊 refresh | `crates/duduclaw-auth/src/mfa.rs:36`、`:561`，`jwt.rs:22`、`:146`，`user_row.rs:16`；`crates/duduclaw-gateway/src/server.rs:3724`、`:16381`，`handlers/live_context.rs:105`、`:117`，`server_mfa.rs:112`，`webchat.rs:640`。access／refresh 帶 epoch；省略 claim 是 0，序列化 0 時仍省略，User JSON 不新增欄位。enroll／disable／Admin 或 host reset 增加 epoch；曾啟用 MFA 的帳號變更／重設密碼也增加。沿用 fresh user read 與 ACL generation 快取，不新增每 frame DB 讀取。因素驗證與 session 簽發比對同一 epoch，並發重設不能簽出新的 epoch。測試覆蓋舊 access／refresh 拒絕、epoch 0 相容、變更密碼／停用／host reset 遞增。 |
| 4 HIGH：舊共用 token `/ws` | `crates/duduclaw-gateway/src/server_mfa.rs:69`，`server.rs:15973`，`handlers/mod.rs:466`。Enterprise 強制生效時拒絕共用 token 握手並回明確錯誤；既有共用 token 連線的 RPC 也拒絕。關閉強制與 Personal 保留原路徑。測試檢查實際握手使用的政策函式；真實 socket 握手為 NOT RUN。 |
| 5 HIGH：Admin 自我重設 | `crates/duduclaw-auth/src/mfa.rs:534`；`crates/duduclaw-gateway/src/handlers/mfa_rpc.rs:84`。拒絕 actor == target；同一交易重讀有效 Admin、驗證操作者新的 TOTP／單次備援碼，再清除目標資料、增加 epoch 並稽核。測試涵蓋 self reset、非 Admin、錯因素拒絕，以及另一位 Admin 成功重設與 challenge 作廢。 |
| 6 MEDIUM：Bash 指令拼法 | `crates/duduclaw-cli/src/lib.rs:473` 與 `users_mfa_cmd.rs:21`、`:75`。實際 Clap 測試發現原 `OpsCommands` 是 flattened group，原 parser 根本沒有 `ops` 層；新增獨立的 `ops users` 層並移除未受該拼法保護的頂層 `users`，其他既有 flattened 指令維持原路徑。Bash registry 改為 `["ops","users","mfa-reset"]`，同時保護新增 `mfa-enrollment-code`。測試從真正的 `Cli::command()` 取得路徑，使用 `Cli::try_parse_from` 驗證，再對照 Bash registry，避免文件／Clap／guard 再漂移。三語文件同步使用實際指令。 |
| 7 MEDIUM：金鑰遺失救援 | `crates/duduclaw-auth/src/mfa.rs:190`、`:512`；`crates/duduclaw-gateway/src/mfa.rs:17`；`crates/duduclaw-cli/src/users_mfa_cmd.rs:49`。備援碼不解密 TOTP；若 users.db 有啟用或待註冊 MFA 資料，缺少 `.keyfile` 會回清楚的主機救援錯誤，不建立新金鑰。主機 reset 改為任意帳號，包含多位 Admin；兩個 host 指令共用 AI-session 拒絕與稽核。測試涵蓋壞 cipher 仍可用單次備援碼、缺 key 且有資料不產生新 key、多 Admin CLI reset。金鑰輪替明列不在範圍。 |
| 8 MEDIUM：設定快照 | `crates/duduclaw-gateway/src/mfa.rs:54`、`:68`，`handlers/lifecycle.rs:38`，`handlers/config_commit.rs:330`，`server.rs:2291`。啟動載入、dashboard commit 後更新、外部變更每 30 秒重載；失敗保留最後有效值並警告。只有 Enterprise 從未取得有效快照才 fail closed；Personal 忽略，無 `[auth]` 為 false；登入路徑不再讀檔／解析。測試包含無初始快照、Personal、缺 `[auth]`、錯型別／格式、刪除 config 後保留 true 或 false。 |

相容性決策：依「未強制且從未註冊帳號維持既有客戶行為」的硬限制，從未註冊帳號的密碼變更不增加 epoch，維持 0；任何密碼變更仍清掉待註冊 challenge。曾註冊後即使停用／重設，epoch 仍保留非 0，後續密碼變更會遞增。這個邊界已寫入三語文件與回歸測試，沒有將所有既有客戶的密碼變更改為全面撤銷舊 session。

Schema：新增 `users.credential_epoch INTEGER NOT NULL DEFAULT 0`、`mfa_enrollment_proofs(token_hash,session,code_hash,kind,verified)`、`mfa_enrollment_codes(user_id,code_hash,expires_at)` 與密碼變更 trigger。現有已啟用 MFA 的 epoch 0 帳號遷移成 1。`User` 的新欄位以 serde skip 保持既有 JSON；共用 `USER_COLUMNS` 增加內部欄位，因此同步調整 password hash／組織樹 depth 的索引，以及 `UserContext` 各處 initializer 的預設 0。

文件：三語 `docs/features/05-security-defense.md`（含語系子目錄）、三語 `docs/guides/dashboard-settings.md`、`docs/api/openapi.yaml`、`CHANGELOG.md`，皆同步分階段啟用、獨立證明、密碼／jti、epoch、host 救援、快照與金鑰輪替範圍。Dashboard UI 仍為下一階段。

### Test result lines

全部使用 `CARGO_INCREMENTAL=0` 與 `-j 4`，依序執行，一次只有一個 cargo。


- `cargo test -j 4 -p duduclaw-auth`（exit 0），證據 `artifacts/mfa/round2-final-auth.log`：
  - `test result: ok. 104 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 24.60s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`

- `cargo test -j 4 -p duduclaw-gateway --lib mfa`（exit 0），證據 `artifacts/mfa/round2-final-gateway-mfa.log`：
  - `test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 10291 filtered out; finished in 7.41s`

- `cargo test -j 4 -p duduclaw-gateway --lib login`（exit 0），證據 `artifacts/mfa/round2-final-gateway-login.log`：
  - `test result: ok. 42 passed; 0 failed; 2 ignored; 0 measured; 10259 filtered out; finished in 3.18s`

- `cargo test -j 4 -p duduclaw-gateway --lib otp`（exit 0），證據 `artifacts/mfa/round2-final-gateway-otp.log`：
  - `test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 10265 filtered out; finished in 5.16s`

- `cargo test -j 4 -p duduclaw-cli --lib mfa`（exit 0），證據 `artifacts/mfa/round2-final-cli-mfa-retry.log`：
  - `test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 2335 filtered out; finished in 0.49s`

- `git diff --check`：通過。證據 `artifacts/mfa/round2-diff-check.log`。

- OpenAPI：92 paths、無重複鍵，可解析；新核發註冊碼與 fresh-factor reset schema 齊備。證據 `artifacts/mfa/round2-openapi-check.log`。

中途失敗也保留：首次 auth 編譯是 reset 舊測試簽章；接著 auth 的兩個組織樹測試揭露 `USER_COLUMNS` 增欄後的 depth 索引未移動，已修正。首次 gateway 編譯發現兩個 compact fixture 缺 `credential_epoch`，已補齊。修正後先有一輪 gateway MFA 12 passed，最後再重跑所有指定測試。記錄都在 `artifacts/mfa/round2-*.log`，CLI 實際 Clap 測試另外抓到原 `OpsCommands` flatten 導致沒有 `ops` 層，已修成真正的巢狀命令並重跑。最終結果以 `round2-final-*.log` 為準，CLI 使用 `round2-final-cli-mfa-retry.log`。

### NOT RUN

- 真實 TCP／WebSocket 綁定、實際 `/ws` 共用 token 握手、長時間到期／重連：依指定的 sandbox socket-binding 限制，NOT RUN。無 socket 的 handler／握手政策測試不代表實際線路驗收。
- 外部 Telegram／LINE／Discord／Slack OTP 真實送達、正式客戶驗收、Windows／Linux 原生驗收、完整 workspace 測試、部署與發版。
- 獨立安全審查與審查子代理：本輪依指示未啟動。
- `cli_auth::tests::live_claude_login_streams_output` 與 `handlers::tests::live_acl_tests::live_acl_latency_with_two_concurrent_logins`：指定 login 篩選中的既有 ignored 測試，未強行執行。

### Anything left

本輪 brief 指定的八項實作與篩選測試沒有剩餘未完成項目。 下一步是在允許 socket 的環境驗證實際握手、通道送達與重新登入，再進行獨立安全審查；本輪未發版。Dashboard 畫面與金鑰輪替不在本階段範圍。

## Round 3

狀態：IMPLEMENTED。七項修補與指定本地測試已完成：auth 107、gateway MFA 20／webchat 87／computer_use 225／login 42 passed，最終皆 0 failed；另有 15 個既有 ignored。實際線路、原生平台、客戶驗收及獨立安全審查仍為 NOT RUN。此節更新 Round 2 的長連線撤銷、通道證明、註冊碼及設定快照描述；前兩輪紀錄保留。

沿用 `feat/enterprise-mfa-20261031` 及既有 worktree。未 commit、push、stash、切換分支、執行 workspace cargo fmt 或啟動審查子代理；沒有新增 worktree，也沒有修改正式帳號或設定。預設 `mfa_enforced=false` 不變。

### Per item — what changed (file:line)

| 項目 | 本輪修正與設計 |
|---|---|
| 1 HIGH：既有 legacy `/ws` | `crates/duduclaw-gateway/src/server_mfa.rs:86`、`server.rs:16128`：心跳區分 legacy／JWT。強制生效時關閉 legacy socket，結束推播；JWT 以現有 live identity reader 比對 epoch，讀取失敗拒絕，適用 MFA 的 session 在原 JWT 到期後關閉。未註冊 JWT 不因強制切換立即終止；未強制 epoch 0 與 Personal 的既有到期政策保留。 |
| 2 HIGH：`/ws/chat` 持續驗證 | `crates/duduclaw-gateway/src/webchat.rs:513`、`:663`、`:851`、`:912`；`handlers/live_context.rs:136`：保留已驗證 claims 的 epoch／exp，在心跳及每次 user_message 前驗證。沿用同一 live reader 與 cache，訊息命中既有兩秒 push-cache 時窗時不查 SQLite；心跳強制重查 generation。到期判斷不依賴快取期限。public widget 不受 JWT gate 影響。 |
| 3 HIGH：viewer／takeover ticket | `crates/duduclaw-gateway/src/server.rs:16179`；`computer_use_sessions/live_ops.rs:295`、`live_view.rs:378`、`view_ws.rs:79`、`:252`；`review_evidence/audience.rs:426`、`:481`：傳遞 transport 所驗證的 JWT 期限；ticket 保留原 epoch、期限，入場 TTL 取 30 秒與 JWT 剩餘時間的較短值。入場及每 30 秒重驗檢查精確 epoch 和原 JWT 到期；身分重建讀目前 epoch，不再填 0。真實帳號缺少 JWT provenance 時不核發 ticket；既有 host/system 路徑保留。 |
| 4 HIGH：通道註冊證明 | `crates/duduclaw-auth/src/mfa.rs:43`、`:92`；`mfa/enrollment.rs:11`、`:70`；`otp.rs:129`；`crates/duduclaw-gateway/src/server.rs:3497`、`handlers/mfa_rpc.rs:94`：持久化強制啟用時間與每次已驗證綁定／重綁時間（Unix 微秒），只接受嚴格早於強制時間的綁定。未知歷史時間為 null，不自行假設早於強制。強制期間未註冊帳號的綁定在交易內確認另一位有效 Admin；原 host API 保留，拒絕 self-bind。`mfa.overview` 顯示各帳號 `channel_bindings` 供啟用前檢查。文件明列：密碼持有人在強制前預先植入的綁定無法辨識。 |
| 5 MEDIUM：Admin code 綁 challenge | `crates/duduclaw-auth/src/mfa/enrollment.rs:113`、`:128`；`mfa.rs:109`；`crates/duduclaw-gateway/src/server_mfa.rs:57`、`handlers/mfa_rpc.rs:61`、`:92`；`crates/duduclaw-cli/src/users_mfa_cmd.rs:19`、`:62`：首次註冊回傳 12 字元 `challenge_ref`，由使用者讀給 Admin。RPC／既有 host 指令要求該 reference，核發前確認唯一、尚未驗證、有效的待註冊 challenge；code 綁其完整雜湊，兌換與刪除都以 `(user_id, challenge)` 比對。沿用一次性、5 分鐘、雜湊保存及稽核；遷移作廢舊版未綁 challenge 的註冊碼。 |
| 6 MEDIUM：設定降級可見及鎖內驗權 | `crates/duduclaw-gateway/src/mfa.rs:68`、`:85`；`handlers/mfa_rpc.rs:94`、`system_status.rs:66`；`config_commit.rs:237`、`system_update_config.rs:952`、`config_raw_rpc.rs:958`：重載失敗保留最後有效值，記錄 degraded／warning，於 `mfa.overview.policy` 與降級時的 `system.status.mfa_policy` 顯示。最後有效值為 false 就保持 false；首次無有效快照的原拒絕政策不變。正常 system.status 不增加欄位。兩個設定寫入路徑都使用 guarded commit，在設定鎖內重查當前有效 Admin。 |
| 7 LOW：epoch 精確相等 | `crates/duduclaw-auth/src/mfa.rs:14`；`crates/duduclaw-gateway/src/server.rs:3732`、`:16389`；`handlers/live_context.rs:105`；`review_evidence/audience.rs:441`：共用 `credential_epoch_matches`，拒絕舊 epoch 與未來 epoch；`/ws`、`/ws/chat`、viewer／takeover、refresh、MFA session 發行均使用此判斷。 |

Schema：新增 `mfa_policy_state(id,enforced,enforced_at)`、`channel_identities.verified_at`、`mfa_enrollment_codes.challenge`。不增加一般 User／ChannelIdentity 的 JSON 欄位；時間只在 MFA overview 顯示。有效 true 的重新載入或重啟保留原啟用時間，false 後再次 true 才記錄新的邊界。

文件同步：三語 `docs/features/05-security-defense.md`、三語 `docs/guides/dashboard-settings.md`、`docs/api/openapi.yaml`、`CHANGELOG.md`。Dashboard MFA UI 仍屬下一階段；未加入 brief 以外的認證或核准機制。

### Test result lines

全部設定 `CARGO_INCREMENTAL=0`、`-j 4`，依序執行，一次只有一個 cargo。

- `cargo test -j 4 -p duduclaw-auth`（exit 0），證據 `artifacts/mfa/round3-final-auth.log`：
  - `test result: ok. 107 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 25.51s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`

- `cargo test -j 4 -p duduclaw-gateway --lib mfa`（exit 0），證據 `artifacts/mfa/round3-final-gateway-mfa.log`：
  - `test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 10291 filtered out; finished in 8.52s`

- `cargo test -j 4 -p duduclaw-gateway --lib webchat`（exit 0），證據 `artifacts/mfa/round3-final-gateway-webchat.log`：
  - `test result: ok. 87 passed; 0 failed; 0 ignored; 0 measured; 10224 filtered out; finished in 2.88s`

- `cargo test -j 4 -p duduclaw-gateway --lib login`（exit 0），證據 `artifacts/mfa/round3-final-gateway-login.log`：
  - `test result: ok. 42 passed; 0 failed; 2 ignored; 0 measured; 10267 filtered out; finished in 3.66s`

- `cargo test -j 4 -p duduclaw-gateway --lib computer_use`（重跑 exit 0），證據 `artifacts/mfa/round3-final-gateway-computer_use-retry.log`：
  - `test result: ok. 225 passed; 0 failed; 13 ignored; 0 measured; 10073 filtered out; finished in 3.13s`

- `git diff --check`：通過；`artifacts/mfa/round3-diff-check.log`。
- OpenAPI：92 paths、無重複鍵，可解析，Admin 註冊碼 schema 必填 `challenge_ref`；`artifacts/mfa/round3-openapi-check.log`。

中途紀錄保留：`round3-gateway-mfa.log` 是重複 `pub(crate)` 宣告造成的編譯失敗，已修正；`round3-gateway-mfa-retry.log` 為 16 passed／1 failed，測試更新誤把已註冊登入的兩欄回應也改成三欄，已恢復正確斷言。最後 MFA 20 項全過。第一次完整 computer_use 為 224 passed／1 failed／13 ignored；舊接管測試未帶 JWT 期限，已補上與 transport 相同的 task-local 期限情境後重跑，未放寬正式程式的檢查。

### NOT RUN

- 真實 TCP／WebSocket 綁定、`/ws`／`/ws/chat`／computer viewer 線路及實際 30 秒心跳：依指定 sandbox socket-binding 限制，NOT RUN；本輪無 socket 的測試不代表實際線路驗收。已執行篩選沒有把 socket 拒絕誤計為產品測試通過。
- 外部通道 OTP 實際送達、Windows／Linux 原生驗收、正式客戶驗收、完整 workspace 測試、部署及發版。
- 獨立安全審查與審查子代理：依本次指令未啟動。
- CLI `--lib mfa` 本輪未單獨執行；一般 CLI 程式由 gateway 測試相依完成編譯，沒有將編譯視為 CLI parser 測試通過。

- `computer_use_orchestrator::tests::real_docker_computer_use_session`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_orchestrator::tests::real_docker_pointer_actions_at_one_spot_are_fast_and_land`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::profile_docker::computer_profile_real_gateway_roundtrip_and_reset`：既有 ignored, needs Docker and DUDU_COMPUTER_USE_IMAGE with the profile helper。
- `computer_use_sessions::tests::real_docker_idle_session_is_reaped`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::real_docker_navigation_over_http`：既有 ignored, needs Docker, outbound network and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::real_docker_tool_session_over_http`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::team_docker::computer_team_native_dashboard_fixture`：既有 ignored, needs Docker, DUDU_COMPUTER_USE_IMAGE and an explicit native dashboard driver。
- `computer_use_sessions::tests::workspace_docker::computer_workspace_real_h8_survives_reap_and_restart`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::workspace_docker::computer_workspace_real_ht3_boot_sweep_removes_orphan`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::workspace_docker::computer_workspace_real_ht3_killed_container`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::workspace_docker::computer_workspace_real_ht3_stop_then_takeover`：既有 ignored, needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::workspace_docker::computer_workspace_real_ht6_weekly_report`：既有 ignored, needs Docker, outbound network and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `computer_use_sessions::tests::workspace_docker::computer_workspace_real_secret_markers_never_persist`：既有 ignored, needs Docker, outbound network and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)。
- `cli_auth::tests::live_claude_login_streams_output`：既有 ignored, live: runs real `claude setup-token`; needs claude installed。
- `handlers::tests::live_acl_tests::live_acl_latency_with_two_concurrent_logins`：既有 ignored, latency measurement; run explicitly。

### Anything left

本輪指定的七項修補、文件同步及五組 cargo 篩選沒有剩餘未完成項目；上方 NOT RUN 不屬於已驗收範圍。保持預設關閉，未提交、推送、部署或發版。Dashboard UI、真實 socket／通道送達與獨立安全審查仍待後續階段。

耗時說明：最後完整 gateway 建置 15 分 21 秒；補齊既有接管測試的 transport fixture 後，測試二進位重建又需 15 分鐘。為取得實際重跑結果，本輪超過原先約 45 分鐘的估時；沒有把尚未重跑的修正宣稱為通過。


## Round 4

### Changes (file:line)

- `crates/duduclaw-gateway/src/handlers/live_context.rs:56`：加入 `Valid`／`Revoked`／`Unavailable` 與每連線恢復計數。只有心跳消耗三次失敗窗口，Valid 重設；Revoked 立即關閉。關閉理由的日誌全域每 30 秒最多一行。強制讀取失敗清除舊快取，避免後續訊息退回舊的成功身分。
- `crates/duduclaw-gateway/src/server_mfa.rs:87`、`server.rs:16124`：dashboard 心跳區分撤銷與暫時不可讀；Unavailable 暫停新 RPC、回應、日誌及推播，第三次失敗心跳關閉。RPC 回應與 OS live events 在送出前核對 epoch，沿用既有兩秒身分快取；退出迴圈即丟棄該連線接收佇列。新連線 handshake 的失敗拒絕行為保留。
- `crates/duduclaw-gateway/src/webchat.rs:513`、`:856`、`:1428`：WebChat 使用相同三態及恢復窗口；暫停新訊息、設定更新與進行中回合的敏感輸出，回合內亦持續檢查心跳。Public widget 原路徑保留。
- `crates/duduclaw-gateway/src/handlers/push_filter.rs:174`：CLI login output／status 改用 `fresh_dashboard_context`，核對原連線 epoch，並保留逐段 fresh read、Admin 及 owner 條件。
- `crates/duduclaw-auth/src/mfa.rs:389`：合併可折疊的 enrollment `if`；`crates/duduclaw-cli/src/lib.rs:6460`：移除 `caller` 多餘借用。
- 回歸測試：`handlers/live_context.rs:353`（三態、恢復、窗口、撤銷、快取）、`handlers/push_filter.rs:292`（敏感逐段推播在讀取失敗與撤銷時拒絕）、`mfa_http_tests.rs:506`（真實 SQLite 讀取失敗／恢復、handshake 拒絕、撤銷後丟棄排隊回應及 OS 事件）。測試不綁定 socket。
- `CHANGELOG.md:20`：記錄第四輪安全修補。

### Test result lines

狀態：**PARTIAL**。四項程式修補已落地；指定 Cargo 驗證未全部完成。為遵守 30 分鐘時間限制，中止重新編譯，不把測試執行檔的結果冒充為三組 Cargo 指令全通過。

所有 Cargo 依序執行，`CARGO_INCREMENTAL=0`、`-j 4`，一次只有一個 Cargo。

- `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib mfa`（exit 0），`artifacts/mfa/round4-gateway-mfa.log`：
  - 首次建置耗時 19 分 32 秒。
  - `test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 10291 filtered out; finished in 7.63s`
- `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib webchat`：觸發重編譯；在時限內以中斷終止（exit 130）。日誌 `artifacts/mfa/round4-gateway-webchat.log`。未取得這次 Cargo 指令的 test result。
- 使用本輪 MFA 建置產生的 `target/debug/deps/duduclaw_gateway-f68c3a2bf7e242ab webchat`（exit 0），`artifacts/mfa/round4-webchat-binary.log`：
  - `test result: ok. 87 passed; 0 failed; 0 ignored; 0 measured; 10229 filtered out; finished in 2.49s`
- 同一執行檔以 `push_filter` 篩選（exit 0），`artifacts/mfa/round4-push_filter-binary.log`：
  - `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10315 filtered out; finished in 0.25s`
- 測試執行檔的 `invoked.timestamp` 為 04:26:38，正式程式修補均早於此時間；`handlers/push_filter.rs` 在 04:27:11 額外加入的讀取失敗／恢復斷言晚於此時間，不能宣稱已執行。該測試原有的 epoch 撤銷斷言已通過；Unavailable 的共用閘門、三次窗口及真實 SQLite 單次讀取失敗／恢復由 MFA 其餘回歸測試通過。最終工作目錄仍需完整重新建置驗證。
- `git diff --check`：exit 0；`artifacts/mfa/round4-diff-check.log`。

接續命令（一次一個）：

```bash
CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib mfa
CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib webchat
CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib push_filter
```

### NOT RUN

- `cargo test -j 4 -p duduclaw-gateway --lib push_filter`：30 分鐘時限內未啟動；僅有上列同一測試執行檔的篩選結果。
- 最終工作目錄的完整 Cargo 重建與 `push_filter` 最後補入的讀取失敗／恢復斷言：NOT RUN；webchat Cargo 重建已中止，不代表失敗測試或通過。
- 實際 TCP／WebSocket 綁定、`/ws`／`/ws/chat` 線路與真實 30 秒心跳：指定 sandbox socket-binding 限制，NOT RUN；無 socket 測試不代表線路驗收。
- 完整 workspace 測試、完整 clippy、Windows／Linux 原生驗收及正式客戶驗收。
- 獨立安全審查與 review subagents：依本次交辦未執行。
- Commit、push、stash、切換分支、workspace `cargo fmt`、部署與發版：依交辦未執行。


## Live-events integration (2026-10-31)

- `server.rs::handle_socket` now shares one local output-gate macro across RPC admission/responses, heartbeat/Pong, logs, OS events, run loss/steps, prepared-event lag and completed push delivery. Revoked sends Close and exits; Unavailable drops output without consuming recovery ticks outside heartbeats. The existing two-second identity-cache window remains in effect.
- One connection-local flag coalesces authorized output lost during a pause. The next successful gate (including an idle heartbeat) sends one `events_lost` before further frames. Dropped frames never allocate sequence numbers. The pending push restores `event_gate` before checking validity; run subscription updates and `run_command_pending` reset still precede the RPC response gate.
- Real-socket regression tests use isolated users.db fixtures, prepared-event and already-authorized step injection, and an awaited socket-task JoinHandle so a panic fails the test. They cover each revoked output source, consecutive paused events, a paused step/loss notice, event-triggered and idle-heartbeat recovery, and contiguous sequence numbers. These tests require TCP binding; compilation does not prove socket behavior.
- Send-path audit: handshake success/error/Close remains at the authentication boundary; all established-socket application output and explicit Ping/Pong sends pass the shared gate. `live_events.rs` writes only internal channels, never the socket; its existing authorization/queue checks remain unchanged. `PushGate`, `may_push_run_step` and `fresh_dashboard_context` were not changed.
- Transport caveat: Axum/Tungstenite automatically queues Pong on receiving Ping (see the local dependency documentation for `axum::extract::ws::Message`). The explicit application Pong path is gated, but that library-generated control reply is outside this gate; no application data is carried by it. Absolute suppression of all transport control frames during a pause is not provided by the current WebSocket API.
- Additional static finding (pre-existing, outside the MFA-output integration): the OS tail checks Admin only at `os.events.subscribe` (`handlers/dispatch_ops.rs`); subsequent OS sends check socket validity but do not re-check the current role. `user_update.rs` bumps ACL generation on role changes without retiring the credential epoch, so an active account downgraded from Admin can retain an existing OS subscription. This was not changed or verified over a live socket in this pass.


### Integration validation

All Cargo commands ran sequentially with `CARGO_INCREMENTAL=0` and `-j 4`. Logs: `artifacts/mfa/live-events-integration-20261031/` (gitignored).

- `cargo check -p duduclaw-gateway --tests`: final exit 0, `Finished dev profile [unoptimized + debuginfo] target(s) in 2m 52s`. Initial check exited 101 on two new test-channel `unwrap()` calls requiring `PreparedPush: Debug`; both changed to success assertions before the passing check. The rebase fixture in `mfa_http_tests.rs` also needed `prepared_events`.
- `cargo test -p duduclaw-gateway --lib mfa`: exit 0; `test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 10364 filtered out; finished in 8.04s`
- `cargo test -p duduclaw-gateway --lib live_event`: exit 0; `test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 10350 filtered out; finished in 2.59s`
- `cargo test -p duduclaw-gateway --lib push` (initial, socket setup refused): exit 101; `test result: FAILED. 52 passed; 7 failed; 0 ignored; 0 measured; 10330 filtered out; finished in 4.74s`
- `cargo test -p duduclaw-gateway --lib push` (after excluding refused socket fixtures): exit 0; `test result: ok. 52 passed; 0 failed; 0 ignored; 0 measured; 10337 filtered out; finished in 5.08s`

Each filter used `-- --skip server::live_events_socket_tests`. The final push retry additionally skipped exactly the seven setup failures below; the initial push log preserves their `PermissionDenied: Operation not permitted` errors. No assertion failure in the tested behavior was observed in those fixtures because socket setup failed first. These are NOT RUN for behavior acceptance, never counted as passing. The new socket tests were compiled by the passing check but never attempted to bind.

### NOT RUN (socket binding unavailable)

- `server::live_events_socket_tests::mfa_live_events_revoked_socket_closes_without_event_step_or_loss`
- `server::live_events_socket_tests::mfa_live_events_pause_two_events_and_step_then_recover_once_with_contiguous_sequence`
- `server::live_events_socket_tests::broker_database_bridge_reaches_ws_with_filtered_sequence_and_loss`
- `codrive::tests_mode::driving_mode_push_at_session_start_lands_in_the_report` (Unix socket bind refused)
- `line::ingress::f2_tests::failed_progress_push_never_changes_the_event_status` (TCP bind refused)
- `line::ingress::f2_tests::queue_wait_past_the_reply_window_pushes_to_the_same_conversation` (TCP bind refused)
- `line::ingress::f5_tests::expired_reply_token_pushes_the_answer_without_the_thinking_footer` (TCP bind refused)
- `line::ingress::f5_tests::redelivered_webhook_never_uses_its_reply_token_and_pushes` (TCP bind refused)
- `line::ingress::f5_tests::refused_reply_token_falls_back_to_push_when_allowed` (TCP bind refused)
- `line::ingress::f5_tests::refused_reply_token_pushes_the_answer_without_the_thinking_footer` (TCP bind refused)

Status: PARTIAL acceptance. The requested integration is implemented and compiles; runnable filtered tests passed. Real socket behavior remains unverified, and the automatic Pong limitation and pre-existing OS-role downgrade issue above remain. No commit, push, stash, branch switch, workspace formatting, network access or review subagents were used.
