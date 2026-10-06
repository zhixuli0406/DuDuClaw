# DuDuClaw 完整功能清單

> v1.24.0 核心 + 2026-07/08 新增｜最後對照程式碼審閱：2026-10-02（v1.67.0）
>
> 說明：以下各章起初是 v1.24.0 基準，功能被移除或變更之處已更正。緊接其後的
> **新增**區塊涵蓋到 v1.61 為止的功能；之後的新增列在 `CHANGELOG.md`，那裡才是
> 權威清單。`ja-JP/` 與 `zh-TW/` 鏡像頁內容相同。

---

## 2026-08 新增（v1.54 – v1.61）

| 功能 | 說明 |
|------|------|
| 校準式 forward model + held-out 學習閘（v1.54） | 行動前的信心預測以 proper score（Brier/RPS，拒用 log score）計分並做 Murphy 分解，證據來自外部工具結果而非自我陳述；無程式化證據的歸納型教訓先進 shadow，樣本外以 Wilson 信賴下界（多候選 Bonferroni 校正）贏過凍結基準才轉正；只給三種誠實結論（SUPPORTED / CANDIDATE / INDISTINGUISHABLE_FROM_LUCK）。預設開啟，可在儀表板逐層關閉（[39-calibrated-forward-model.md](39-calibrated-forward-model.md)） |
| 通知治理（v1.55） | 所有主動推播必附 L1/L2/L3 級別；勿擾時段延後並合併投遞 L1/L2（L3 照發）；每日摘要預設關、無事不寄；每類通知的行動率量測（`notify.stats`，精確率 <50% 標記 broken）；決定卡按下後就地收斂；通道推播附儀表板深連結（[40-notification-governance.md](40-notification-governance.md)） |
| 統一待辦決定管線（v1.55） | 五種決定來源（goal needs_human／啟動核准／通用審批／安裝簽核／自動規則跳閘）收斂為單一 action-id 編碼、單一授權模型（關閉 goal 按鈕先前不驗身分的缺口）與統一收件匣；新增第四顆「交給我」接手動作；一人決定後所有收件者的卡片同步收斂 |
| 真人接手（v1.55） | 已驗證的管理者在通道對話中直接發言即接手：AI 對該對話暫停回覆（預設 60 分鐘，`/takeover` 查詢／延長／提前結束）；接手期間所有把 AI 訊息送進該對話的路徑一律凍結、延後或丟棄，L3 級審批照發。v1.56 起改為 opt-in（預設關），原預設開會讓個人版跟自己的 AI 對話被噤聲（[42-human-takeover.md](42-human-takeover.md)） |
| 常駐感知（v1.55） | 外部資料流（`http_poll` / `command` / `file_tail` / `websocket`，預設關）以 `tick` 事件接進 autopilot 匯流排，數值欄位自動衍生 `prev_`/`delta_`/`pct_` 比對欄位；喚醒 agent 前可選本地模型初篩；TickHub 記憶體環形緩衝；SSRF／DNS rebinding 防護；經真實行情流多輪活測強化（[41-resident-sensing.md](41-resident-sensing.md)） |
| 跨喚醒「近期自身行動」注入（v1.55） | 每次喚醒開場注入該 agent 近 24 小時實際工具呼叫的稽核摘要（含失敗與被攔截的行動），讓「你是否做過某事」以耐久紀錄為準，不再只信即時工具狀態 |
| 五通道引用回覆上下文（v1.55） | 在 Telegram / Discord / Slack / Teams / WhatsApp 回覆（引用）訊息時，被引用內容帶進 agent 輸入；mention-only 群組中回覆 bot 的訊息視同提及；Telegram 轉發訊息標注原始來源 |
| 白話化經驗法則（v1.55） | Playbook 規則以白話句呈現（零 LLM 模板改寫）並附「為什麼有這條」證據；通道指令 `/rules`；注入規則帶編號，AI 能指出回答依據的是哪一條 |
| Telegram Mini App 審批卡（v1.55） | 高風險審批卡可選掛「查看詳情」web-app 按鈕（完整說明、事前模擬後果、到期倒數、同意／拒絕）；`initData` 簽章驗證，授權與按鈕同一套（[43-telegram-miniapp.md](43-telegram-miniapp.md)） |
| 學習管線可觀測性（v1.56） | 記錄了 `source_facts` 的規則在來源事實被取代時標記 `source-stale`（注入時降權並標示）；達標後才被關卡擋下的整合失敗記錄原因（`consolidation_failures.jsonl`）；對話路徑的規則結算接上 held-out 閘；歸納型 shadow 候選在對話側也能累積樣本外紀錄 |
| 分版與設定硬化（v1.56） | 個人版併發上限限「同時執行的目標任務數」（預設 2，排隊不拒絕、fail-open；RFC-27），永不設 AI 員工數量上限；多人團隊版專屬畫面改由 gateway 分派入口在伺服器端把關；`agent.toml [model] account_pool` 真的會篩選帳號輪替候選；儀表板建立員工必須明確選模型 |
| 真 ACP server（v1.57） | `duduclaw acp` 實作 Agent Client Protocol v1（stdio JSON-RPC），Zed / JetBrains / nvim 的 agent panel 直連 AI 員工，走與通訊頻道同一條 gateway 回覆管線，即時串流 `tool_call` / `plan` / 訊息分塊；A2A 的 `acp server` 指令行為不變 |
| Remote MCP + OAuth 2.1（v1.57） | 規範原生 `POST /mcp` 端點（版本協商、無狀態模式、Origin 錨定白名單）+ 最小而完整的 OAuth 2.1 授權面（RFC 9728/8414/7591、PKCE S256、操作者同意、refresh 輪替），claude.ai 自訂連接器／Claude 行動版／MCP Inspector 可直連自架 DuDuClaw |
| 五通道文字裁決（v1.57） | 回覆決定卡並送出整句裁決詞（同意／拒絕／重試／完成／中止／暫停，中英皆可）就等於按下按鈕：Telegram / Discord / Slack / LINE / Teams，同一套授權、重複按壓保護與行動率記帳；補上智慧手錶一鍵決定的缺口 |
| 本地模型市集（v1.57） | 選用途 → 依本機記憶體算出的硬體適配燈 → 一鍵安裝（自動挑量化版本，來源為五家驗證過的 HF 發布者）；MoE 雙軌判定在 16GB 機器上為 30B-A3B 級模型標示「可 expert offload」（[45-local-model-marketplace.md](45-local-model-marketplace.md)） |
| 工作狀態：跨喚醒權威狀態（v1.57） | 每 agent 鍵值化工作狀態 + 交接註記，自動注入每一次喚醒（排程／心跳／目標迴圈／通道）作為唯一權威；只收顯式工具更新且 `reason` 必填 + 取代鏈歷史、`expected_value` CAS 防並行喚醒互蓋、`ttl_hours` 當日規則到期、32-key 上限；`[memory] working_state_enabled`，預設開（[44-working-state.md](44-working-state.md)） |
| 排程執行有記憶、看得到（v1.57） | 成功的排程／派工執行現在也餵進同一條蒸餾／知識管線（每 agent 每小時節流）並落執行紀錄頁。先前純排程驅動的 agent 跑再久也零記憶、零紀錄 |
| 生態系與散發面（v1.57） | 六個免費產業入門包（安全邊界一字不減）、pack registry 安裝／發佈（客戶端 sha256 + minisign 驗證）、CONTRIBUTING.md + pack 自製教學、公開網站聊天 widget（訪客模式，預設關）+ WordPress 外掛、Chrome / VS Code 擴充、穿戴裝置逐字稿直灌（`POST /ingest/transcript`）、LINE 加好友 QR/NFC 套件（當時一併加入的 `duduclaw tunnel` 輔助指令已於 2026-09 移除）；外部 MCP 工具面改 scope 驅動；Homebrew 通路廢棄 |
| 目標任務管理台 `/goals`（v1.58） | 儀表板直接指派目標給 AI 員工（與 `/goal` 同一套語義）、每目標完整逐輪執行時間軸（`tasks.timeline`）、人工介入就地操作；儀表板所有 needs_human 裁決統一走與通道按鈕相同的 fail-closed `tasks.goal_decide` 路徑 |
| 預測與驗證頁（v1.58） | LLM→LWM 迴圈可視化：預測 → 執行 → 觀測 → 對照；逐輪預測 vs 實際（`forward.chain`）；每 agent 預測能力判定卡（Brier + Murphy 分解，三態誠實標籤）；世界模型狀態桶首次可讀；MAV 逐面向裁決、執行紀錄連結、重派／無進展訊號與預測子誤差逐輪落庫 |
| 通道 OTP 候選鏈 + 設定整合（v1.58） | 登入驗證碼送信改為「全域 token 優先，再逐一嘗試各 agent 專屬 bot token」（去重、排序），修復 bot 綁到單一員工後 OTP 靜默失敗；agent 通道設定與通道管理共用同一個編輯對話框；側邊欄「新功能」（`newIn`）標籤機制上線 |
| 信念迴圈（v1.59） | 對外部世界的結構化信念記帳（`belief_submit` / `belief_settle` / `belief_stats` MCP 工具）；確定性三向 Brier 結算，對照提交時基準值；校準數字只計有交叉驗證的結算，目前沒有任何正式路徑提供交叉驗證，所以現有結算全是自報，另外計數、不算校準；校準統計與信念對照兩個程式化注入鉤點；/foresight 信念與驗證分頁（[46-belief-loop.md](46-belief-loop.md)） |
| 每目標契約欄位 + 自主研究（v1.59） | 建目標時可設 `duration_hours`（到期 → needs_human）與 `risk_boundary`（留空套五行基本款），逐輪注入並由 MAV safety 面向檢核；`/goal` 支援 `時限:`／`邊界:` 段；可勾選要求結構化預測；當日信念失準的員工自動獲派晚間研究目標 |
| 派工引擎預設開 + 排程器活性（v1.59） | `[dispatch] enabled` 預設改 true（指派目標開箱即跑），儀表板熱切換；`/healthz` 在 cron／heartbeat 迴圈停擺逾 5 分鐘時回 503：修復排程層全滅、容器卻連日顯示 healthy 的事故 |
| 兩段式裁決 + 判官硬化（v1.60） | MAV 判官團之前先跑便宜的第一階段評估器（`continue`/`candidate_complete`/`blocked`，預設開；任何故障降級直跑完整 MAV，絕不自動通過）；四條判官紀律（反棘輪、只稽核不自建證據、反契約外擴張、自稱完成不是證據）；修掉截斷面板與首 token `PASS` 誤判兩個 fail-open 洞；gap 指紋停滯偵測；提前收工偵測；`resume_on_restart` 預設 `pause` |
| 可換判官 seam（v1.60） | `[dispatch] judge = mav / evaluator_only / external / human_only`（`evaluator_only` 與 `human_only` 已在 v1.69.0 移除，設定檔殘留值的處理見[棄用與移除](../../guides/zh-TW/deprecations.md)）：外部判官任何故障一律降級回 MAV（變嚴、留稽核），其 feedback 視為未受信 DATA；未知值回退 `mav`；設定→自動化有下拉選擇器 |
| 目標契約凍結（v1.60） | 建立時把驗收標準凍結成不可變 `acceptance_criteria_baseline`，判官與評估器一律讀這份基準；agent 身分以 `tasks_update` 改 goal 任務的驗收標準、`title` 或 `description` 一律拒絕並留稽核；`/goal` 未帶標準時附四要素引導與 outcome 式標準建議 |
| 目標迴圈人為信號 + 准入排隊（v1.60） | needs_human 帶封閉六類 `pause_reason`（觸發現場靜態標記，絕不從 LLM 敘述反解）；逾時進度通報（`progress_report_minutes`）；零 LLM 工具連擊 advisory（3/5/8 逐級）；ephemeral spawn 超限改有界 FIFO 排隊（預設 `queue`）；預算耗盡改交「最佳輪成品」（確定性挑選 + 差距清單，不再空手升級） |
| Agent Mail（v1.60） | 每 agent 信箱（`/mail` 頁）：Gmail API／drop folder 入站，外發一律先建草稿等 ApprovalBroker 確認（背景 worker 是唯一寄信者），信件內容 DATA 圍欄，獨立不可外部授予的 scope，跨 agent 讀信過 delegation policy 判定（[47-agent-mail.md](47-agent-mail.md)） |
| Agent 組態 preset P1（v1.60） | `duduclaw preset` 指令族 + `agent create --preset`，可具名複用的組態組合；綁定權威存 `preset_bindings.toml`，解析結果物化到 agent 目錄之外（防自改繞過），org 欄位拒絕、敏感段剝除；內建 9 個部門 preset |
| 統一交辦面板 + 計畫模式 + 靈感畫廊（v1.60） | 所有入口共用同一個交辦面板（個人版終於有主要動作按鈕），問一問／交辦／「想一想」三模式。計畫模式先產 3-8 步計畫停在 needs_human 等核准，核准後以 `<execution_plan>` 一次性注入；已結束的目標可接「接著做」指示；靈感畫廊 `/gallery` 把 22 組產業團隊範例扇出成一鍵做同款卡片；任務詳情改四分頁（產物／檔案／變更／過程） |
| 產物 provenance + 交付安全（v1.60） | `artifacts.jsonl` 五種 origin 的 provenance ledger（declared / swept / uploaded / produced / unknown，exact／inferred 歸屬標示，絕不用時間窗猜測）；goal 驗收通過時封存產物進 `attachments/`（canonicalize 圈定、20MB/100MB 上限）；📎DELIVER 前的零 LLM 交付閘（零位元組／magic 不符／zip 損壞硬失敗）；`[limits]` DocumentLimits 守三個下游 office/zip 解析器；修補專家包 zip「header 謊報」繞過 |
| 憑證 P1 + secret 參照收斂（v1.60） | `secret://keychain` 與 `secret://file` 本機 backend、tick 來源 headers 支援 `secret://`、憑證來源總表卡 + `doctor --fix-residue`；`SecretRef`/`Secret` 型別收斂七套手刻解密方言（它們可能把 `secret://` 參照字面值當真憑證送給 vendor API）；WhatsApp webhook 驗簽改 fail-closed；ActionGuard 判官改吃 21 項封閉列舉 findings（攻擊者可控文字結構上進不了判官 prompt）；MCP key 輪替即時重載、`denied_tools`/`allowed_tools` 在 MCP 分派總門強制 |
| 十通道通知統一（v1.60） | autopilot `notify`、MCP `send_message`、提醒全部改走共用 `create_sender` 工廠，涵蓋十個通道（WebChat 誠實拒絕）；修復 autopilot Slack 通知從未送出的問題與 Google Chat / Teams 靜默跳過缺陷 |
| 進化量測硬化（v1.60） | AEE 提交閘拆 visible／held-out 兩個評測維度（fence-only：只否決不晉升）；冠軍 bootstrap 改同形量測；`duduclaw evolution clear-holdout-rotation` 操作者出口；每輪 14 個 harness 旋鈕快照進 `aee_round` 事件 |
| cron 星期慣例修正（v1.61，**BREAKING**） | 數字星期欄在解析時從 Unix crontab 慣例（0/7=週日、1-5=週一到五）轉譯成 `cron` crate 的 Quartz 序數，排程器／heartbeat／MCP 驗證／儀表板共用同一份 normaliser：先前 `* * 1-5` 實際排的是週日到週四（週日幽靈觸發 + 週五靜默跳過）；刻意照 Quartz 寫的排程升級後會位移一天 |
| `duduclaw migrate from claude-code`（v1.61） | 單向匯入 Claude Code 的 memory shard（→ semantic + SPO 時間記憶）、CLAUDE.md（→ agent wiki context 層，不佔注入預算）與對話逐字稿（噪音濾除只留人類 prompt + assistant 最終回覆，實測有效訊號僅約 1.5%）；一律 `origin=import`（trust ≤ 0.7）、當 DATA、預設去識別化、過注入掃描、skill 過安全掃描 fail-closed；未加 `--apply` 不會寫入任何東西 |
| 通道能力表（v1.61） | `channel_capabilities.rs` 單一權威表：11 通道 × 7 能力（檔案／照片上傳、互動按鈕、edit-in-place、typing、原生 markdown、引用回覆）+ 進度節流秒數；不支援的能力從靜默 no-op 改為留下結構化 log |
| minimal_context spawn 瘦身（v1.61） | 每次 spawn 官方 CLI 帶策展 `--tools` 清單 + `--setting-sources project,local`（保留 agent-file-guard hook）：實測固定開銷 35,892 → 10,974 tokens/次（約 69%）；`estimate_tokens` CJK 校準（修正約 22% 低估）；MCP `tools/list` 依呼叫者 capability 過濾（discoverable ⊆ callable） |
| 憑證 P2/P3（v1.61） | 零重啟輪換：帳號池寫入即失效 rotator 快取、Telegram 每輪重解析 token、六個 webhook 通道 inbound 驗簽 per-request、Odoo 下次呼叫即重連（Discord/Slack 長駐 WS 仍需重啟）；spawn env 改白名單擦洗（濾除所有 `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD`，vendor 金鑰改由呼叫端顯式注入）；per-agent `[capabilities] git_credentials`（預設關，opt-in）為 git push 類 agent 恢復 SSH/GPG，留稽核；`secret://` 收斂第二輪（account_rotator + mcp.rs） |
| 管理台與任務打磨（v1.61） | ⌘K 跨來源內容搜尋（對話／產物／記憶／wiki）、`/files` 搜尋 + 任務篩選 + 日期範圍、`/goals` 任務置頂／歸檔／重新命名 + 分頁（解除 20 筆硬上限）、唯讀 `/presets` 頁、mail 拒絕備註、`/goals` 詳情併入四分頁 `/tasks/:id` 頁 |

## 2026-08 新增（v1.53）

| 功能 | 說明 |
|------|------|
| 進化系統 v3：AEE + playbook | 預設進化標的從「整份改寫 SOUL.md」改為基因形 playbook 規則：Gate/Measure 閘門分離、champion + matches-or-improves 提交閘、條目級觀察窗；SOUL.md 對 agent 唯讀（[38-aee-playbook-evolution.md](38-aee-playbook-evolution.md)） |
| E1 條目斷言 + 反 reward-hacking 稽核 | 每條新規則必附可機器檢查的斷言，對錄製 transcript 做零 LLM 重放（`G-Assertions`）；提交前確定性篩查候選規則的題庫題面洩漏／恆真空話／失敗抑制 |
| 任務層前瞻模型 | goal loop 上的 predict-act-verify 世界模型：四階統計預測（冷啟動零 LLM）、觀察保真度分級（原生工具事件／只有稽核日誌／無）、`<state>` 狀態區塊 + `(state, action)` 訪問圖震盪偵測、確定性任務規則歸納；`[task_forward_model]`，v1.54 起預設開 |
| 派工證據落地預檢 | 驗收判官之前的零 LLM 證據檢查：最終回覆必須與真實的非錯誤工具結果重疊；自我回音排除名單 + 輸入重疊扣除，防自我證明；`[dispatch] grounding_precheck_enabled`，預設開 |
| 記憶新穎度閘門 | 語意層近重複寫入被擋下並記遙測（0.92 字元 n-gram cosine），防假驚訝；時間取代／再確認路徑豁免；`[memory] novelty_gate`，預設開 |
| 有證據才歸納的反思 | MistakeNotebook 條目附程式化抽取的 `TrajectoryEvidence`；查無證據的自述錯誤不再整併成經驗規則 |
| 行動前模擬審批 | `needs_human`／審批請求附三步模擬軌跡（15 秒上限，逾時降級為無模擬，不阻塞）；只引用唯讀 wiki namespace；儀表板渲染預覽 |
| Eval 錄製隔離 + 起步 CLI | `--record` 走臨時 `.mcp.json`（eval home + 佔位金鑰，生產零副作用、金鑰不外洩）；撞 max-turns 的失控 run 解析為 `error_max_turns`（可評測的失敗基線）；`duduclaw eval-scaffold` 從 SOUL 規則產生題目草稿；`duduclaw playbook migrate-soul` 把舊 SOUL 規則遷成 playbook 草稿 |
| 稽核日誌作為證據源 | `tool_calls.jsonl` 記錄遮罩後的 `result_text`/`input_text`（三段遮罩、16MB 輪替、0600）；系統發送者派工（goal-loop／cron／heartbeat／autopilot）歸屬到實際執行的 agent |

## 2026-07 中後期新增（v1.33 – v1.46）

| 功能 | 說明 |
|------|------|
| 統一 LLM provider 層（`duduclaw-llm`） | 一套正規化的 request/stream 形狀，涵蓋四種原生協議（Anthropic / OpenAI Responses / Gemini / OpenAI-compat，8 個 preset）；`ModelRegistry` 定價；stdio MCP client + provider 無關的 tool loop，讓 API 模式 agent 取得完整工具面 |
| Agent 行為評測（`duduclaw eval`） | 每 agent 的 golden-task 回歸：確定性 tool-call／regex／grounded 斷言 + 可選 LLM 判官；live 與 replay 兩種模式，exit code 可作 CI 閘 |
| HITL ApprovalBroker | 橫跨 MCP 工具／autopilot／bus 任務的單一中斷／審批原語；SQLite 落地，TTL 過期 = DENY（fail-closed） |
| OpenTelemetry GenAI 追蹤 | opt-in 的 `gen_ai.*` span，可經 OTLP 匯出至 Langfuse/Grafana/Jaeger/Datadog；關閉時零開銷 |
| 通道 UX 層 | 各平台 markdown 渲染、輸入中指示，以及跨 8 個外部通道就地編輯的即時 todo 看板進度 |
| 自主目標迴圈 | `/goal` → 迴圈跑到完成，配三面向 MAV 驗收判官；卡住時經通道按鈕升級給人（[34-goal-loop.md](34-goal-loop.md)） |
| 迭代式看板輪次 | Task board `revising` 狀態機，含逐輪明細歷史 |
| 可信記憶與判官強化（v1.41） | 寫入時 origin 綁定 + 抗 Sybil 再確認、GovMem 晉升閘、Janus 規則觀察期、PORTICO 任務範圍能力授權、trace-grounded 評測斷言 |
| OS 原生感知與主動關懷 | 檔案監看 + 前景感知 → footprint temporal memory（重啟可續的快照）、內建主動關懷檢查、LLM 評分主動閘、一鍵 OS 自動化範本（[33-os-native-perception.md](33-os-native-perception.md)） |
| Office 文件套件 | 真正的 docx/xlsx/pptx/pdf 產出、📎DELIVER 協議 + 未宣告產出 sweep、gateway 歸檔、含 LibreOffice 預覽的 Files 頁（[31-office-document-suite.md](31-office-document-suite.md)） |
| 專家包生態系 | 可安裝的 AI 團隊：經安全掃描的安裝管線、含分類／部門分組的內建產業目錄、LLM 引導的包自製、部門 × 職級組織定位與 `--attach-under`（[32-expert-packs.md](32-expert-packs.md)） |
| 錄製 → skill | 瀏覽器（Playwright trace+HAR，憑證就地遮罩）與桌面錄製器，蒸餾成須核准的 SKILL.md 草稿（[36-recording-to-skill.md](36-recording-to-skill.md)） |
| 照片 → 桌面寵物 | 本地照片 → 去背 → 像素量化 → Codex-Pets 8×9 spritesheet；自主遊蕩引擎移動真正的置頂視窗（[35-photo-desktop-pet.md](35-photo-desktop-pet.md)） |
| 能力功能開關 | 16 組白話功能群組疊在原始 allow/deny 工具清單之上，背後是有完整性防護的工具目錄 |
| Google Workspace / Notion / GitHub 原生工具 | Gmail/Calendar/Drive/Sheets、Notion、GitHub 的第一方 MCP 工具（v1.45；設定前預設隱藏） |
| 桌面殼層（Tauri 2） | 原生視窗包裝 gateway + 儀表板、tray、gateway 選擇器、透明桌面寵物 overlay 視窗 |
| 互動節奏防護 | 對話歷史框架 + 常駐節奏規則，打招呼永不重新觸發先前的重工具任務 |

## 2026-07 新增（v1.24.0 之後）

| 功能 | 說明 |
|------|------|
| Aider 式程式碼符號圖（`code_map` MCP 工具） | tree-sitter 符號圖疊在 HippoRAG-lite Personalized-PageRank 引擎上；依查詢相關度排序 repo 檔案 |
| 語意向量記憶（`w_vec`） | FTS/graph 之外的第三個 re-rank 訊號；零依賴、CJK-safe 的 `NgramHashEmbedder`，以 `DUDUCLAW_SEMANTIC_VECTORS=1` 開啟 |
| 跨 session 使用者畫像 | 每使用者偏好 traits（temporal supersession）→ session-stable 的 `## About This User` 回覆注入（來自 gateway 萃取與核准的審核）；`user_profile_record` / `user_profile_get` MCP 工具讀寫的是所有 gateway 啟動的員工共用的另一個命名空間，不會進入這個區塊（已知限制，v1.67.1） |
| GDPR 匯出／抹除 | `duduclaw export gdpr <contact>` / `duduclaw gdpr erase <contact> --confirm`（舊寫法 `gdpr export` 已在 v1.69.0 移除），涵蓋記憶（triple + 提及 + key_facts，四表級聯，SHA-256 tombstone）**與** session 儲存（`<channel>:<chat_id>` prefix） |
| Custom Dashboard Widgets | 在沙盒 runtime 中執行的 AI 引導或原始 HTML 儀表板卡片；Widget Studio 分享／匯入／匯出（[30-custom-widgets.md](30-custom-widgets.md)） |
| 預算斷路器 | 每 agent 滑動視窗硬上限（`[budget] daily_cap_cents`），到頂即於 choke-point 阻斷 LLM 呼叫；寫 `budget_events.jsonl` |
| 燒錢速率異常偵測 | 對每日花費做滾動平均＋標準差離群偵測（`cost_anomaly.rs`） |
| 稽核匯出 + SIEM sink | `duduclaw export audit`（原 `duduclaw audit`）：正規化並串流 JSONL 稽核軌跡至 NDJSON／webhook |
| 出站 guardrail hook | opt-in `[guardrails]`：送出前掃描憑證洩漏／injection echo／deny 詞／PII |
| CI 紅隊掃描 | `duduclaw redteam`／`duduclaw test`：由 `CONTRACT.toml` `must_not` 生成 11 種手法 × 中英文攻擊，跑過 input-guard 並建成覆蓋帳本；沒被擋下的單位標為「待活體驗證」，不算漏洞；`duduclaw test --emit-evals` 為它們產生 `duduclaw eval` 案例 |
| 安全姿態報告 | `duduclaw security`：現行防護的加權檢查清單 |
| 備份／還原 | `duduclaw backup` / `restore`：時間戳家目錄封存 + SHA-256 sidecar（還原時驗證） |
| Session 重播 | `duduclaw session replay <id>`：逐輪印出 session（可加 `--tools`） |
| MCP Bridge | `[[mcp.external]]`：掛載外部 MCP server，deny-by-default 工具過濾 + `env://` / `secret://` 憑證；各 SaaS recipe 見 `guides/mcp-bridge.md` |
| Secret manager 後端 | 1Password Connect + Infisical adapter；`secret://<backend>/<name>` 解析接入 MCP Bridge |
| MCP/skill 信任分級 | 依 repo 最後 push 時間 + 擁有者類型分 official / active / orphan |
| Email（經 Agent Mail） | `email.rs`（以 `lettre` 寄送 SMTP、RFC822 解析）自 v1.60 起由 Agent Mail 使用。沒有 IMAP 輪詢，email 也不是通道之一 |
| 通訊通道 | 現為**十一個**：在原本七個之上新增 Google Chat 與 Microsoft Teams，之後再加 WeCom 與 DingTalk（見下方通道表） |

---

## 核心架構

| 功能 | 說明 |
|------|------|
| Multi-Runtime AI Agent 平台 | 統一 `AgentRuntime` trait：`runtime_catalog.rs` 中有 13 個 runtime id，包含十二個 CLI 後端（Claude、Codex、Gemini（已棄用）、Antigravity、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode）與 OpenAI-compat HTTP，支援自動偵測（[13-multi-runtime.md](13-multi-runtime.md)） |
| MCP Server（JSON-RPC 2.0） | 透過 stdin/stdout 向 AI Runtime 暴露 249 個工具（v1.67.0；`tools/list` 只列出呼叫者可呼叫的工具）；註冊於 `<agent>/.mcp.json`（v1.8.5，Claude CLI `-p` 僅讀取專案層級），gateway 啟動時自動建立／修復 |
| ACP/A2A Server | 兩個指令：`duduclaw acp`（= `duduclaw acp client`），IDE agent panel 用的 Agent Client Protocol v1（Zed / JetBrains / nvim；`initialize` / `session/new` / `session/prompt` 串流，未設定時回 `AUTH_REQUIRED`）；`duduclaw acp server`（原 `acp-server`，已在 v1.69.0 移除），A2A 協定（`agent/discover` / `message/send` / `tasks/*`，`/.well-known/agent-card.json` Agent Card，另有 legacy `/agent.json` 別名） |
| Agent 目錄結構 | `.claude/`、`.mcp.json`、`SOUL.md`、`CLAUDE.md`、`CONTRACT.toml`、`agent.toml`、`wiki/`、`SKILLS/`、`memory/`、`tasks/`、`state/` |
| Sub-agent 編排 | `create_agent` / `spawn_agent` / `list_agents` + `reports_to` 階層 + D3.js 組織圖 + 系統 prompt 自動注入「## Your Team」 |
| DelegationEnvelope | 結構化交接協議：context / constraints / task_chain / expected_output |
| TaskSpec 工作流 | 多步驟任務規劃：dependency-aware 排程、auto-retry（3x）、replan（2x）、持久化 |
| 長回應分頁 | 子 Agent 回報超過通道 byte budget 時，以 `channel_format::split_text` 分頁並標示 `📨 **agent** 的回報 (1/N)` |
| 孤兒回應恢復 | `reconcile_orphan_responses` 重播 crash／Ctrl+C／hotswap 後殘留在 `bus_queue.jsonl` 的 `agent_response` callback |
| 檔案式 IPC | `bus_queue.jsonl` 跨 Agent 委派，最多 5 hop 追蹤 |
| Per-Agent Channel Token | `get_agent_channel_token` 優先讀取每 agent `bot_token_enc`（修復 Discord thread 跨 bot 401） |

## Multi-Runtime

| 功能 | 說明 |
|------|------|
| Claude Runtime | Claude Code SDK（`claude` CLI）+ JSONL streaming + `--resume` 多輪 |
| Codex Runtime | OpenAI Codex CLI + `--json` streaming 事件，以 `AGENTS.md` 檔案傳遞 system prompt |
| Gemini Runtime（v1.67.0 棄用，v1.71.0 移除；請改用 Antigravity） | Google Gemini CLI + `--output-format stream-json`，以 `GEMINI_SYSTEM_MD` env 傳遞 system prompt，approval mode 依該 agent 的 capabilities 推導（預設 `auto_edit`，唯讀 agent 另加 `--sandbox`，只有完整存取的 agent 才用 `yolo`）。Google 於 2026-06-18 退役個人版 Gemini CLI 後，保留給付費 `GEMINI_API_KEY` 用戶 |
| Antigravity Runtime（v1.24.0） | Google Antigravity CLI（`agy`，2026-06-18 Gemini CLI 後繼者），走 oneshot `agy -p --dangerously-skip-permissions --print-timeout 300s`。二進位自動解析（PATH → `~/.local/bin/agy`）；無 `--system` 旗標，故 system prompt + 歷史內嵌進 prompt（CJK-safe）；認證用 Google 登入（在主機終端機執行 `agy`）或 API key 模式（`config.toml [antigravity] auth = "api_key"` + Gemini API key）；MCP 工具註冊在各 agent 工作區的 `.agents/mcp_config.json`；自動把 agent 目錄預植進 agy 的 `trustedWorkspaces`（跨程序檔鎖）以免 headless 卡在信任提示；token 用量為估算（print 模式無統計） |
| Grok Runtime（R4） | xAI Grok CLI（「Grok Build」），走 oneshot `grok -p`（2026-07-13 對照 docs.x.ai 驗證）。二進位 `grok`（curl 安裝；第三方 `grok-cli` 作為備援探測）；`--model` 選模型；`--tools`/`--disallowed-tools` 限縮（+ `native_sandbox` 硬閘）；system prompt + 歷史內嵌進 prompt（CJK-safe）；duduclaw MCP server 以 `[mcp_servers.duduclaw]` TOML 寫入各 agent 的 `<agent_dir>/.grok/config.toml`（+ agent 身分經 spawn env 轉送）；以 `XAI_API_KEY` env 認證；token 用量為估算（純 stdout）。**殘餘項**（需實機 CLI）：`--tools` 清單分隔符、`mcp_servers` 的專案本地 `config.toml` 探索、真實用量的 `--output-format json` schema，以及完整 `--model` 清單（`grok models`），文件僅確認 `grok-4.5` / `grok-build-0.1` |
| OpenAI-compat Runtime | HTTP 端點（MiniMax / DeepSeek 等）REST API |
| RuntimeRegistry | 自動偵測已安裝 CLI，per-agent `[runtime]` 設定 |
| Cross-Provider Failover | `FailoverManager` 健康追蹤、冷卻、不可重試錯誤偵測 |

## Session 記憶堆疊（v1.8.1 + v1.8.6）

| 功能 | 說明 |
|------|------|
| 原生多輪 Session | Claude CLI `--resume` + SHA-256 確定性 session ID + history-in-prompt fallback（stale session、帳號輪替、unknown stream-json error） |
| Turn trimming | >800 chars → 首 300 + 尾 200 + `[trimmed N chars]`，CJK-safe 字元切片 |
| Prompt Cache 策略 | Direct API「system_and_3」斷點配置（未公布實測命中率） |
| 壓縮摘要注入 | 壓縮摘要（role=system）注入 system prompt，不放進對話輪次 |
| Instruction Pinning | 首則使用者訊息 → async Haiku 擷取 → `sessions.pinned_instructions` → 注入 system prompt 尾端 |
| Snowball Recap | 每輪 user message 前置 `<task_recap>`，零 LLM 成本、U-shape 注意力尾端 |
| Clarification 累積 | Agent 提問 + 使用者回答追加至 pinned instructions（≤1000 字元） |
| P2 Key-Fact Accumulator | 每輪實質對話 2-4 則事實 → `key_facts` FTS5 表 → 注入 top-3（約 100-150 tokens vs MemGPT 6,500，−87%） |
| CLI 輕量路徑 | `call_claude_cli_lightweight()`：`--effort medium --max-turns 1 --no-session-persistence --tools ""`，降低 25-40% 成本 |
| 穩定化旗標 | `--strict-mcp-config` + `--exclude-dynamic-system-prompt-sections`（節省 10-15% token）；`--bare` 於 v1.8.11 移除（破壞 OAuth keychain） |
| CJK-Safe 字串切片 | `duduclaw_core::truncate_bytes` / `truncate_chars` 取代 31 處不安全的 byte-index 切片 |

## 通訊通道（11 個）

| 通道 | 協定 |
|------|------|
| Telegram | Long polling，檔案／照片／貼圖／語音、forums/topics、mention-only、語音轉錄 |
| LINE | Webhook + HMAC-SHA256 簽章、貼圖、per-chat 設定 |
| Discord | Gateway WebSocket、斜線指令（`/ask /status /config /session /agent`）、語音頻道只在啟用非預設 `discord-voice` feature 的建置中提供（release binary 不含）、auto-thread（v1.8.14 起 session id 在整個 thread 生命週期內穩定）、embed 回覆 |
| Slack | Socket Mode、mention-only、thread 回覆 |
| WhatsApp | Cloud API webhook，簽章驗證 fail-closed |
| 飛書 | Open Platform v2 |
| Google Chat | Webhook（JWT 驗證）、service account 送信 |
| Microsoft Teams | Azure Bot / Connector v3（JWT 驗證） |
| 企業微信（WeCom） | HMAC-SHA1 簽章 + AES-256-CBC 訊息加密 |
| 釘釘（DingTalk） | HMAC-SHA256 簽章 + 時間窗 |
| WebChat | 內嵌 `/ws/chat` WebSocket + React 前端（Zustand store） |
| 通道熱啟停 | Dashboard 驅動動態啟停 |
| Media Pipeline | 自動縮放（max 1568px）+ MIME 偵測 + Vision 整合 |
| Sticker 系統 | LINE 貼圖目錄 + 情緒偵測 + Discord emoji 等價映射 |
| 通道失敗追蹤 | `channel_failures.jsonl` + `FailureReason` 分類（RateLimited/Billing/Timeout/BinaryMissing/SpawnError/EmptyResponse/NoAccounts/Unknown） |
| Discord Gateway 強化（v1.9.2） | 真正的 op 6 RESUME：跨重連保存 `session_id` + `resume_gateway_url` + sequence；`select!` 停滯看門狗於 2× 心跳沉默後中斷（修復 18 分鐘殭屍狀態）；心跳 channel 容量 1→16 配 `try_send`；op 9 依 `d.bool` 選 RESUME 或 IDENTIFY 並加 1-5s jitter；close code 4007/4009/4003 清除 session；backoff 上限 300s→60s；處理 `RESUMED` dispatch |

## 演化系統

> **進化系統 v3（2026-08-06）→ S11（2026-09-29）**：進化標的從「整份改寫
> `SOUL.md`」改為 **playbook**（小顆粒、可個別退休的基因形規則；`SOUL.md`
> 對 agent 唯讀），並於 2026-09-29 **直接移除** legacy SOUL 改寫路徑：
> `[evolution] legacy_soul_evolution` 逃生門、`SOUL.md` 版本化、24 小時觀察
> 期、自動回滾、超額 consolidate 重寫、deferred GVU 重試與
> `duduclaw evolution finalize` CLI 全部一併移除。`[evolution] gvu_enabled`
> 現在出廠即為 `true`。現行（也是唯一）引擎見
> [38-aee-playbook-evolution.md](38-aee-playbook-evolution.md) 與
> [evolution-engine.md](../../architecture/zh-TW/evolution-engine.md) 第十二章。

| 功能 | 說明 |
|------|------|
| 預測驅動引擎 | Active Inference + Dual Process Theory；依設計，多數對話不需要 LLM 呼叫即結束（未公布實測比例） |
| 雙系統路由器 | System 1（規則）／System 2（LLM 反思） |
| AEE（v3 預設） | Agentic Evolution Engine：Generator 內迴圈（≤3 輪）→ Gate（確定性、有否決權）／Measure（計分、無否決權）分離 → champion + matches-or-improves 提交閘 → 條目對自己連結的 eval case 各自 accept/rollback |
| Playbook（v3 預設） | 基因形行為規則（category/signals_match/eval_cases/success_streak），擴建自既有 rule_lifecycle 儲存，0.92 cosine 去重，容量 + 過期／封存生命週期 |
| MistakeNotebook | 跨迴圈錯誤記憶：記錄失敗模式、防止退化；條目現在附確定性 `TrajectoryEvidence`（哪個工具／斷言失敗），未經驗證的自述診斷不再參與反思整併（v3） |
| MetaCognition | 每 100 次預測自動校準誤差閾值，新增對稱回升規則，閾值不再單向漂移（v3 Phase 0） |
| 停滯偵測器（v3） | 每 30 分鐘掃描 `evolution.db` 的連續拒絕／連續 D 天零 apply／重複拒絕原因訊號，發到 Activity Feed + 儀表板 |
| ConversationOutcome | 零 LLM 對話結果偵測（TaskType / Satisfaction / Completion），zh-TW + en |
| Agent-as-Evaluator | 獨立 Evaluator Agent（Haiku 成本控制）進行對抗式驗證，輸出結構化 JSON 判定 |
| Orchestrator 範例 | 5 步規劃（Analyze → Decompose → Delegate → Evaluate → Synthesize）+ 複雜度路由；範例位於 `docs/examples/orchestrator/`，需手動複製（不會自動套用） |

## Wiki 知識分層（v1.8.9）

| 功能 | 說明 |
|------|------|
| 4 層架構 | L0 Identity / L1 Core / L2 Context / L3 Deep，受 Vault-for-LLM 啟發 |
| 信任權重 | `trust`（0.0-1.0）frontmatter；搜尋以 trust-weighted score 排名 |
| 自動注入 | `build_system_prompt()` 於 CLI／頻道／dispatcher 三條路徑注入 L0+L1 至 WIKI_CONTEXT |
| FTS5 全文索引 | SQLite `unicode61` tokenizer + CJK 支援，寫入／刪除時自動同步，`wiki_rebuild_fts` 手動重建 |
| 知識圖譜 | `wiki_graph` MCP 工具輸出 BFS 限制的 Mermaid 圖；節點形狀依分層而異 |
| Dedup 偵測 | `wiki_dedup`：標題匹配 + 標籤 Jaccard 相似度（≥0.8） |
| 反向 backlink 索引 | 掃描 `related` frontmatter + body markdown 連結，建立雙向對應 |
| 搜尋篩選 | `min_trust` / `layer` / `expand`（1-hop related/backlink 擴充） |
| 共享 Wiki | `~/.duduclaw/shared/wiki/` 跨 Agent SOP／政策／規格；`wiki_visible_to` 可見度控制；MCP 工具 `wiki_ls/read/write/search/stats/lint` 搭配 `scope="shared"`（六個 `shared_wiki_*` 寫法已在 v1.69.0 移除），另有 `shared_wiki_delete` 與 `wiki_share`；`.scope.toml` SoT 政策（見「身分與存取」） |
| CLAUDE_WIKI 模板 | 新 Agent 建立時納入 CLAUDE.md，提供 wiki MCP 工具使用指引 |

## 技能生態

| 功能 | 說明 |
|------|------|
| 6 階段生命週期 | 啟動 → 壓縮（三層漸進載入）→ 萃取 → 蒸餾 → 診斷 → 差距分析（[15-skill-lifecycle.md](15-skill-lifecycle.md)）；原本的「重構」階段沒有呼叫端，已於 2026-09 移除 |
| GitHub 即時索引 | Search API + 24h 本地快取 + 加權搜尋 |
| 技能市集 | Web Dashboard 瀏覽、安裝、安全掃描 |
| 技能自動合成 | 差距累積器 → 從情境記憶合成（Voyager 啟發）→ 沙箱試用（TTL）→ 跨 Agent 畢業；預設關（`agent.toml [evolution] skill_synthesis_enabled`） |
| 技能合成排程器（W19-P1，v1.22.0） | 讓「對話 → skill」萃取依間隔自主執行：`config.toml [skill_synthesis] auto_run / dry_run / interval_hours / lookback_days` + dashboard `skill_synthesis.get/update` RPC；`skill_synthesis_threshold` 為 `u32` 計數（修好 registry 掃描拒絕 `0.7` 的問題） |
| Skill 安全掃描器（Rust-native） | `skill_lifecycle::security_scanner` 掃描候選技能，無 Python 依賴 |

## 本地推論引擎

| 功能 | 說明 |
|------|------|
| OpenAI 相容 HTTP | 唯一出貨的 backend：llama-server／Ollama／vLLM／SGLang／llamafile。行程內的 llama.cpp、mistral.rs、MLX 三個 backend 已於 2026-09 移除（release binary 從未編譯過它們） |
| 信心路由器 | LocalFast / LocalStrong / CloudAPI 三層路由 + CJK-aware token 估算 |
| InferenceManager | 多模式自動切換：llamafile → Direct → OpenAI-compat → Cloud API |
| llamafile 管理 | 子程序生命週期、零安裝跨 6 OS 可攜推論 |
| 模型管理 | `model_search`（HuggingFace）/ `model_download`（resume + mirror）/ `model_recommend`（硬體感知） |

## Prompt 壓縮

| 功能 | 說明 |
|------|------|
| 回覆路徑預算管線 | TurnTrim → DropOldestToolEchoes → BisectAndSummarize，考量成本壓力，CJK-safe token 估算（[11-token-compression.md](11-token-compression.md)）。早期的 Meta-Token／LLMLingua-2／StreamingLLM 壓縮器與其 `compress_text` 工具已於 v1.33 移除 |
| 快取感知防護 | 近期快取效率高於 50% 且預算超出低於 15% 時，略過壓縮 |

## 語音管線

見 [14-voice-pipeline.md](14-voice-pipeline.md)。HTTP 端點與 Telegram 語音處理是分開接線的。

| 功能 | 說明 |
|------|------|
| HTTP 端點 | `POST /api/stt`（OpenAI 相容轉錄 API 或本機指令範本；`[voice]` 沒有 STT provider 時回 501）、`POST /api/tts`、`GET`/`POST /api/voice/config` |
| TTS provider | Piper（本機 ONNX 語音）／Edge TTS／MiniMax T2A（自動挑 CJK 或拉丁語系語音）／OpenAI TTS，由同一個 router 分派 |
| Telegram 語音 | 語音訊息以 OpenAI Whisper API 轉錄；`/voice` 回覆使用 Edge TTS。兩者都寫死在程式碼中，儀表板的語音設定不影響 Telegram |
| 不在 release binary 中 | 行程內 Whisper（`duduclaw-inference` 的 `whisper` feature）、ONNX embedding（`onnx` feature）與 Discord 語音頻道（`discord-voice` feature）只有在建置時啟用這些 feature 才會編譯 |
| 從未實作 | 先前這裡列過 SenseVoice、Deepgram、Silero VAD、`symphonia` 解碼與 LiveKit 語音房，這些都沒有任何程式碼 |

## 安全層

| 功能 | 說明 |
|------|------|
| `agent-file-guard` PreToolUse hook | `duduclaw hook agent-file-guard`（Rust 子命令，matcher `Write\|Edit\|MultiEdit\|NotebookEdit\|Bash`，由 `agent_hook_installer` 逐 agent 安裝，指令帶 `--agent` 與 `--home`）：擋正規樹外的 agent 結構檔、擋寫自己的 SOUL.md 與 CONTRACT.toml、擋跨 agent 寫入、擋員工寫 DuDuClaw 資料目錄裡自己 agent 目錄與 `attachments/` 以外的位置（以解析符號連結後的實際路徑判斷）、擋改自己 `agent.toml` 可編輯區段以外的內容；Bash 通道是啟發式，`Read` 不在涵蓋範圍；規則與限制見 [05-security-defense.md](05-security-defense.md) |
| `org_field_guard` | 同一個 hook 內的欄位級凍結：`[agent] reports_to`／`department`／`name`、整張 `[capabilities]` 表，以及 `config.toml [delegation]`／`[acp]`；內容無法解析或寫入意圖無法重建一律 fail-closed |
| `data-file-guard` PreToolUse hook | `duduclaw hook data-file-guard`（RFC-23 §14.4，H10 2026-09 起為 Rust 子命令，matcher `Read\|Bash`），只有去識別化生效時才武裝；本質是 `Bash` 檔名啟發式，並非沙箱 |
| Dashboard 認證 | JWT 帳號登入（Argon2id 密碼，`users.db`）或 gateway 管理員 token。早期的 Ed25519 挑戰回應路徑已移除，從來沒有任何設定能啟用它 |
| AES-256-GCM | API 金鑰靜態加密、per-agent 金鑰隔離 |
| Prompt Injection 掃描 | `input_guard`：11 類規則、阻擋門檻 60、先 NFKC 正規化、英文＋zh-TW 樣式、XML 分隔標籤保護 |
| SOUL.md 漂移偵測 | SHA-256 指紋比對 |
| CONTRACT.toml | 行為邊界 + `duduclaw test` 紅隊 CLI（9 個內建場景＋覆蓋帳本）；自動注入所有 runtime 的 system prompt |
| RBAC 矩陣（唯讀檢視） | 安全頁把每個 agent 的工具／網路／審批矩陣渲染出來，資料源是 `agent.toml [capabilities]`。`duduclaw-security::rbac` 模組已移除（零呼叫端），可編輯的權威來源就是各 agent 的 capability envelope |
| 統一多源審計日誌 | `audit.unified_log` 合併 `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl`；日誌頁提供來源篩選 + 嚴重度下拉 |
| JSONL 審計日誌 | 完整記錄工具呼叫，async 寫入 |
| Unicode 正規化 | NFKC 正規化，偵測同形字攻擊 |
| Action Claim Verifier | 工具呼叫聲明的簽章驗證 |
| 容器沙盒 | 兩條獨立路徑。任務沙箱（`agent.toml [container] sandbox_enabled`）：只支援 Docker，把被委派任務的 AI CLI 放進唯讀、非 root、有資源上限的容器，不能用時直接失敗（fail closed）（[指南](../../guides/zh-TW/task-sandbox.md)）。腳本沙箱（PTC `execute_program`、`duduclaw secaudit` 的 PoC 步驟）：Docker（Windows 先試 WSL2），`--network=none`、唯讀根檔案系統，只掛一個唯讀的私有腳本目錄；沙箱不能用時 PTC 拒絕執行，除非設 `script_when_unavailable = "run_unsandboxed"` |
| Secret 洩漏掃描 | 19 種 secret 樣式（Anthropic / OpenAI / AWS / GitHub / GitLab / Slack / Stripe / Google / SendGrid / JWT / PEM 金鑰，以及金鑰或密碼賦值）加上高熵檢查，供 skill 安全掃描器使用 |
| 敏感資料遮蔽（RFC-23，v1.14.0） | `duduclaw-redaction` crate：內部資料（Odoo／shared wiki／file tools）以 `<REDACT:CATEGORY:hash8>` token 取代後才送 LLM，受信出口（使用者通道回覆、白名單工具）自動還原；AES-256-GCM SQLite vault（per-agent 32-byte key，0o600）、TTL 7d 兩階段 GC、5 個內建 profile、五層 enable/disable resolver、JSONL audit 10MB rotation；2026-09 新增欄位級規則：`db_field`（Odoo `model.field`／`model.*` 語法糖）與通用 `json_path`，命中欄位整值 token 化，不比對內容樣態，並附 `duduclaw redaction verify` JSON 模式，可對範例工具結果證明規則會觸發；`db_field` 原本只認 Odoo 的對照表，2026-09 進一步抽成任何 MCP 工具都能綁的 `[redaction.data_sources.*]` 登錄表，去識別化的涵蓋範圍也首次跨出 DuDuClaw 自己的 MCP server，見下一列 |
| 資料來源與原生資料庫連接器（2026-09） | `[redaction.data_sources.<name>]` 登錄表（`tools`、`table_arg`／`table`、`record_paths`、`key_alias`）讓 `db_field` 規則的 `source` 能指名任何工具型資料來源，不再只有內建的 `odoo`；`duduclaw mcp-proxy`（spawn 時改寫 `.mcp.json`）把客戶自接的外部 stdio MCP server 導向與 DuDuClaw 自家 MCP server 同一套 egress／回傳去識別化，openai-compat 直連 API 的工具迴圈則由 `ToolInterceptor` 掛鉤做同一件事的行程內版本，HTTP/SSE MCP server 與 codex／gemini／antigravity runtime 目前尚未涵蓋；新增唯讀 `duduclaw-db` crate（sqlx：PostgreSQL／MySQL／SQLite，三層唯讀保證）提供四個 MCP 工具（`db_sources`／`db_tables`／`db_select`／`db_query`，`db_query` 只在 `allowed_tables = ["*"]` 時才開放），受 `Scope::DbRead`（`db:read`）與 deny-by-default 的 per-agent `[capabilities] db_sources` 授權雙重把關；dashboard 新增「資料來源」（兩分頁）與「資料表欄位規則」卡，含試跑與 `[redaction]` 設定損壞時的毒化橫幅；地端檔案（2026-09）補上另一段缺口：Claude CLI 內建 `Read`／`Bash` 不是 MCP 工具，從未經過去識別化節流點，新增三個 MCP 工具 `file_read`／`csv_read`／`xlsx_read`（路徑圍欄、`files:read` scope），內建 `duduclaw_files` 登錄來源讓 `db_field` 規則可寫 `customers.csv.name`／`客戶清單.xlsx.地址`，以及一個 PreToolUse hook `data-file-guard`（`[redaction] data_file_guard`，預設開）擋下內建讀檔路徑，誠實標明是檔名啟發式，並非沙箱；**AI 偵測與自訂規則（2026-09）**：新增 `type = "ner"` 規則類型與內建 `ai_pii` profile（「AI 智慧偵測」），在本機透過 ONNX Runtime 執行 OpenAI Privacy Filter（Apache-2.0）（`ort` `load-dynamic`，release binary 不連結 runtime；`redaction.model.install` 以固定的 sha256 下載模型與 runtime，另有 `.status`／`.cancel`／`.remove`；優先序低於所有 regex 規則，精確樣式優先；實測召回率如實公布，regex profile 仍作為第一層保持開啟）；儀表板自建的**自訂規則**（`~/.duduclaw/redaction/profiles/custom.toml`：資料類型名稱加關鍵字清單或樣式，每種規則類型都有 per-rule `enabled`，`[meta.labels]` 顯示名稱，以及供 TOML 規則包使用的 `redaction.custom_rules.*` 與 `redaction.profiles.import`／`.remove` RPC）、`redaction.suggest_pattern`（貼上 2–5 個範例，得到經驗證的樣式；本機推論 → utility model → 啟發式，絕不捏造樣式），以及對未儲存草稿規則執行的 `redaction.dry_run`（[55-data-sources.md](55-data-sources.md)） |

## 記憶系統

| 功能 | 說明 |
|------|------|
| 情節／語意分離 | Generative Agents 3D 加權檢索（Recency + Importance + Relevance） |
| FTS5 全文搜尋 | SQLite 內建 |
| 向量 re-rank 訊號 | 內建字元 n-gram 雜湊 embedder（`NgramHashEmbedder`，以 `DUDUCLAW_SEMANTIC_VECTORS=1` 開啟）；它比對的是表面片段，不是語意。ONNX embedder 需要非預設的 `onnx` 建置 feature，release binary 不含 |
| 記憶衰減排程 | 每日背景執行：低重要度 + 30 天以上歸檔，歸檔 + 90 天以上永久刪除 |
| 認知記憶 MCP 工具 | `memory_search_by_layer` / `memory_successful_conversations` / `memory_episodic_pressure` / `memory_consolidation_status` |
| Key-Fact Accumulator | `key_facts` + FTS5：跨 session 輕量記憶（見 Session 記憶堆疊） |
| Temporal Memory（F1，v1.19.0） | `memories` 經冪等遷移新增時序／知識圖譜欄位（`valid_from`/`valid_until`/`superseded_by`/`supersedes`/`subject`/`predicate`/`object`/`confidence`/`metadata`）；`store_temporal()` 對同一 `(agent, subject, predicate)` 自動衝突解析並串接 supersession chain（v1.67.1 起，寫入可信度不低於目前事實時才取代）；`search()` 預設只回傳現行有效列；`get_history()` / `get_at()` 提供鏈與時間點查詢 |
| Reflexion Loop（F2，v1.19.0） | 橋接既有 `MistakeNotebook`：F2a 將近期未解決錯誤注入作答 prompt（`## Past Mistakes to Avoid`，CJK-safe 比對 + recency fallback）；F2b 將 ≥3 則同 `MistakeCategory` 錯誤整併為一條語意記憶規則（`reflexion.rs`）後標記來源已解決。觸發訊號 = `ErrorCategory` Significant/Critical（MetaCognition 自適應） |
| `memory_fetch_batch`（F3，v1.19.0） | MCP 工具 + `get_by_ids` 一次以 ID 取回 ≤100 筆（命名空間／擁有權強制，部分命中 → `missing_ids`） |
| Bi-temporal + build-time provenance（D1） | `memories` 經冪等遷移新增 `ingested_at`（transaction-time 軸，有別於 world-time 的 `valid_from`）＋ `invalidated_by_event`/`invalidated_at`（哪個 source_event 於何時關閉一列）。`store_temporal()` 的取代由 world-time 的 `valid_from` 決定（可容忍亂序：較早的事實會以有界歷史區段插入而不擾動現行事實；無 `valid_from` 的寫入維持既有的攝入順序行為）；再次觀察到相同事實會**再確認**（metadata `reaffirmed_by`，≤20，並累加 `access_count`），不新增一列 |
| `memory_get_history` / `memory_get_at`（D1） | 時序讀取 API 的 MCP 揭露：完整取代鏈（含 provenance 欄位）與某 `(subject, predicate)` 三元組的時間點查詢（scope `memory:read`） |
| 取代時的可信度檢查（v1.67.1） | 寫入的有效 `origin_trust` 嚴格低於目前事實（儲存值以其類別上限封頂）時不能取代它；相同或更高照舊取代。對話事實與使用者輪廓特徵萃取被拒時會暫存，送到只能在儀表板決定的 `knowledge_quarantine` 審核（24 小時，每位 AI 員工每 UTC 日最多新增 20 筆，超過只寫稽核）；其他路徑略過或回傳錯誤。`user_profile` 來源上限 1.0 → 0.6。`config.toml [memory] supersession_trust_guard`（預設開）（[20-memory-intelligence.md](20-memory-intelligence.md#取代時的可信度檢查v1671)） |
| `memory_invalidate_by_origin`（D1） | 來源回溯原語：讓某**精確** `origin` 的所有現行有效事實過期（只過期、不刪除；可選限定某截止時間之後），並將 `origin_trust ≤ 0.1` 級聯至 `derived_from` 的後代；歷史保留（`invalidated_by_event = "origin_purge"`）。scope `admin`；v1.67.1 起以 AI 員工身分呼叫時只能處理 `channel` / `mcp_external` / `tool_echo`（其他拒絕並稽核 `memory_invalidate_refused`） |
| 圖檢索演進（D3） | HippoRAG-lite graph 獲得四項 fail-safe 改良（未啟用時逐位元組相同）：**(1)** per-agent 持久化圖快取（`RwLock`），由每筆更動三元組寫入遞增的 per-agent 世代計數器失效，僅在超過 `GRAPH_CACHE_MIN_TRIPLES = 500` 時啟用；**(2)** 透過 `entity_alias(agent_id, canonical, alias)` 的實體別名合併：在建圖＋seeding 前把表面形式收斂到同一節點，正規化＋鏈攤平；**(3)** 附掛到邊上的述詞邊標籤（PPR 不變），餵給 `engine.export_graph(agent, limit)` → 可序列化 `{nodes, edges}` 快照（隔離事實加註旗標）供 D6 策展 UI；**(4)** 可選的 embedding seeding（`graph_embed_seed`）：PPR seed ＝ whole-word FTS ∪ query embedding 最鄰近實體向量（同模型 cosine，top-k，惰性 `entity_embedding` 快取），預設關閉 |
| `memory_alias_add` / `memory_alias_list`（D3） | 管理實體別名的 MCP 工具：add 把 `alias` 收斂到某 `canonical` 實體（scope `memory:write`），list 回傳 `(canonical, alias)` 配對（scope `memory:read`）；命名空間隔離 |
| Decision Continuity（RFC-24，v1.23.0） | 當 agent 提出列舉式選項（方案 A/B/C），每個選項固化進 Temporal Memory 的 **semantic** 層（獨立於對話壓縮），待決事項每回合重新注入；稍後「用方案 C」（跨回合／session／程序）從持久狀態解析，不靠猜測。偵測確定性、零 LLM；`decision_resolve` / `decision_list` MCP 工具 + Dashboard 面板 + Prometheus 計數器；per-agent opt-in `[memory] decision_continuity = true`（TTL `decision_ttl_days`，預設 7） |

## 帳號與成本管理

| 功能 | 說明 |
|------|------|
| 多帳號輪替 | OAuth + API Key，4 種策略（Priority/LeastCost/RoundRobin/Failover） |
| 雙 dispatch 路徑 | 子 Agent dispatcher（`claude_runner::call_with_rotation`）與頻道回覆（`channel_reply::call_claude_cli_rotated`）皆走 rotator |
| CostTelemetry | SQLite token 追蹤 + 快取效率分析 + 200K 價格懸崖警告 |
| 預算管理 | 每帳號月度上限 + 冷卻 + 自適應路由（cache_eff <30% → 本地） |
| Direct API | 繞過 CLI，system prompt 加 `cache_control: ephemeral`；OAuth 帳號被限流時的付費備援（未公布實測命中率） |
| 通道失敗追蹤 | `channel_failures.jsonl` + 依失敗類別的 zh-TW 訊息 |
| 二進位探測 | `which_claude()` / `which_claude_in_home()` 掃描 Homebrew（Intel + Apple Silicon）／Bun／Volta／npm-global／`.claude/bin`／`.local/bin`／asdf／NVM |

## 瀏覽器自動化

| 功能 | 說明 |
|------|------|
| L1 `web_fetch_cached` | 經 SSRF 閘、帶磁碟快取的 HTTP GET（body 截斷在 6 萬字元） |
| L2 `web_extract` | 同一條抓取路徑＋CSS 選擇器擷取（`text`／`html`／`json`） |
| L3 headless（可選，外部） | 在該 agent 的 `.mcp.json` 註冊 Playwright 或 Browserbase MCP server；不在 binary 內，也不會自動降級過去 |
| L5 Computer Use | 由 `computer_use_orchestrator` 啟動的容器虛擬顯示器（映像 `ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>`，不會自動下載），由 AI 員工透過八個 `computer_*` MCP 工具驅動（`session_start`／`screenshot`／`click`／`type`／`key`／`scroll`／`navigate`／`session_stop`；session 由 gateway 持有，透過簽章的 loopback 路由連線，每位員工一個，不需要 API 金鑰，網路只通到 `[capabilities.computer_use_config] allowed_domains` 列出的主機）。聊天觸發的 gateway 迴圈與 `native` 主機桌面模式已移除 |
| 能力閘門 | `agent.toml [capabilities]` 預設拒絕（`computer_use`／`browser_via_bash`／`allowed_tools`／`denied_tools`）；`denied_tools` 同時以 `--disallowedTools` 與 MCP 分派總門兩處強制 |

## 容器沙盒

同一個名稱底下有兩條獨立的程式路徑。

| 功能 | 說明 |
|------|------|
| 任務沙箱 | 逐員工開啟（`agent.toml [container] sandbox_enabled = true`）。被委派的任務會在 Docker 容器裡執行該員工的 AI CLI：唯讀根檔案系統、非 root、丟棄所有 capability、記憶體／行程／CPU 上限、用完即丟的私有工作目錄、員工目錄以唯讀掛在 `/agent`。只有檔案與 shell 工具，沒有平台 MCP 工具。需要 `network_access = true` 與已拉到本機的 image，只支援 Docker。不能用時任務失敗（稽核 `task_sandbox_unavailable`），除非在 `config.toml [container.sandbox]` 設 `when_unavailable = "run_unsandboxed"`。開了沙箱的員工不會組成團隊（goal 回合在沙箱裡以 Solo 執行），也不會被信件叫醒；通道回覆、cron、提醒等對話類途徑仍在主機執行，並寫稽核事件 `task_sandbox_not_applied`。見[任務沙箱指南](../../guides/zh-TW/task-sandbox.md) |
| 腳本沙箱 | PTC `execute_program` 與 `duduclaw secaudit` 的 PoC 步驟使用。透過 `duduclaw-container`，macOS／Linux 用 Docker，Windows 先試 WSL2 再試 Docker（WSL2 尚未在真正的 Windows 主機上執行過）；不會選用 Apple Container 後端。與任務沙箱同一個 image，不自動下載；`--network=none`、唯讀根檔案系統、2 GiB／256 個行程／1 顆 CPU、`/tmp` tmpfs、600 秒硬上限，只掛一個唯讀的私有腳本目錄。不能用時 PTC 預設失敗（`[container.sandbox] script_when_unavailable`），PoC 永遠不在主機上執行（[指南](../../guides/zh-TW/task-sandbox.md)） |

## 排程系統

| 功能 | 說明 |
|------|------|
| CronScheduler | `cron_tasks.jsonl` + `cron_tasks.db` 永久化（v1.8.12）；排程以 `tasks_create` + `schedule` 建立（舊的 `schedule_task` 工具已在 v1.69.0 移除） |
| ReminderScheduler | 一次性提醒（相對 `5m`/`2h`/`1d` 或 ISO 8601），`direct` / `agent_callback` 兩種模式 |
| HeartbeatScheduler | 每 Agent 統一排程：bus polling + GVU 沉默喚醒 + cron |
| 排程器級任務板拉取（v1.9.3） | `poll_assigned_tasks` 移入 `HeartbeatScheduler::run` tick：每 30s 掃描整個 agent registry（不再略過 `enabled=false` 的 agent）；1 小時 LIKE-marker 冷卻防止 stampede |

## 任務板與 Activity Feed

| 功能 | 說明 |
|------|------|
| 任務板 | SQLite 後端任務管理：status / priority / assignment 追蹤 |
| Dashboard RPC | `tasks.list/create/update/remove/assign`、`activity.list` 供 Web UI |
| Agent MCP 工具 | `tasks_list`、`tasks_create`、`tasks_update`、`tasks_claim`、`tasks_complete`、`tasks_block`、`activity_list`、`activity_post`：Agent 可見自身佇列、認領工作、回報進度；改動或完成別的員工的任務需要委派關係，控制用 tag（`outcome:`／`grant:`／`auto-research`）AI 員工不能動 |
| 即時 Activity Feed | WebSocket 串流 activity 事件 |
| 系統 prompt 注入 | 待辦任務（最多 5 筆）自動注入 Agent system prompt |

## Autopilot 規則引擎

| 功能 | 說明 |
|------|------|
| 事件匯流排 | `tokio::broadcast`（容量 8192），新規則可訂閱的事件共 12 種：`task_created` / `task_updated` / `task_status_changed` / `activity_new` / `channel_message` / `agent_idle` / `run_at_risk` / `os_file` / `os_frontmost` / `tick` / `security_event` / `odoo_event`；`cron_tick` 從不送出，v1.67.1 起建立規則時拒絕（[23-autopilot-engine.md](23-autopilot-engine.md)） |
| 規則條件 | `all` / `any` + `eq/neq/in/not_in/gt/gte/lt/lte/contains` 運算子 |
| 動作型別 | `delegate`（enqueue bus task）、`notify`（通道）、`run_skill`（skill 名稱 + 目標經 alphanumeric allowlist + `canonicalize()` 路徑圍堵驗證） |
| 規則 CRUD | Dashboard RPC `autopilot.list/create/update/remove/history` + agent MCP `autopilot_list`；寫入時驗證觸發事件、條件與動作；沒有條件的規則每次事件都會執行（v1.67.1） |
| 三態斷路器 | 每規則 `Closed` / `Open` / `HalfOpen`：60s 內 10 次觸發轉 Open（60s 冷卻），再 HalfOpen probe；防止自我增強迴圈；轉換記入 history + Activity Feed |
| events.db 橋接 | SQLite（WAL + 單調遞增 id + 7 天 prune）取代舊 `events.jsonl`：無 rotation race、無 partial-line 風險 |

## 可靠性與治理

| 功能 | 說明 |
|------|------|
| LLM Fallback 鏈（`gateway/failover.rs::model`，v1.9.4） | 三層備援的第二層（帳號 → 模型 → runtime，2026-09-29 收斂）：主模型 timeout/503/429/overloaded 自動切換到較輕的 fallback 模型；純函數 `is_llm_fallback_error` / `should_attempt_model_fallback` 有單元測試，派工路徑統一呼叫 `FailoverManager::model_fallback_for` 取得決策；hard-deadline arm 回傳 `Err("hard timeout")` 確保 fallback 可靠觸發 |
| Evolution Events 系統（v1.9.4） | 30+ 事件 schema（`schema.rs`）、async batch+retry emitter（`emitter.rs`）、查詢介面（`query.rs`）、可靠性保證（`reliability.rs`）；HTTP 端點呈現於 Web `ReliabilityPage` |

## 身分與存取

| 功能 | 說明 |
|------|------|
| Identity Resolution（`duduclaw-identity`，RFC-21 §1，v1.11.0） | `IdentityProvider` async trait：`WikiCacheIdentityProvider`（`shared/wiki/identity/people/*.md`）、`NotionIdentityProvider`（Notion `databases/query` + `field_map`）、`ChainedProvider`（cache → upstream，故障時優雅降級） |
| `identity_resolve` MCP 工具 | 受 `Scope::IdentityRead` 閘控，回傳標準 `ResolvedPerson` 紀錄 |
| Sender 自動注入 | 頻道回覆將 XML 分隔的 `<sender>` 區塊注入 system prompt（每輪解析一次），使 SOUL.md「拒絕非成員」規則可由資料判定 |
| 共享 Wiki SoT 政策（RFC-21 §3，v1.11.0） | `~/.duduclaw/shared/wiki/.scope.toml` 宣告命名空間擁有權：`agent_writable`（預設）、`read_only { synced_from }`、`operator_only`；`wiki_write`（`scope="shared"`）與 `shared_wiki_delete` 遵循；`wiki_namespace_status` 揭示現行政策；檔案缺失／格式錯誤 ⇒ fail-safe 無政策 |

## Live Forking（RFC-26）

| 功能 | 說明 |
|------|------|
| Live Run Forking（`duduclaw-fork`） | 受 pydantic-deepagents 啟發的執行中分支：並行探索多種延續路徑 |
| AI Judge | 為並行分支評分以挑選最佳延續 |
| 預算控制 | `budget.rs` 限制 fork fan-out／成本 |
| 狀態 | 預設關（逐 agent `[fork] enabled`）。v1.67.0 中有四個發布流程測試在 Windows CI 上仍失敗，因此目前請勿在 Windows 上啟用（見 `CHANGELOG.md`） |

## CLI Runtime（一次性 PTY）

| 功能 | 說明 |
|------|------|
| 一次性 PTY 呼叫（`duduclaw-cli-runtime`） | 在真正的偽終端下 spawn CLI（Win 10 1809+ 用 ConPTY、Unix 經 `portable-pty` 用 openpty）並把 stdout 讀到 EOF，服務那些 stdout 接到 pipe 就拒跑的 CLI。使用者：Grok runtime 與 CLI 登入輔助流程。`clear_env` 把 gateway 的廠商 API 金鑰擋在子行程外；`deadline` 是絕對 wall-clock 上限。見 [27-pty-pool-runtime](27-pty-pool-runtime.md) |
| PTY session 連線池（**2026-09 移除**） | 長駐的 sentinel-framed `claude` REPL 連線池、`duduclaw-cli-worker` 子行程＋supervisor、`RuntimeMode::PtyPool`、`GET /api/runtime/status`、`pty_pool_*` / `worker_*` 指標與 `[runtime] pty_pool_enabled` / `worker_managed` 鍵全部移除。理由：它所保的 Anthropic 程式化用量拆分於 2026-06-15 暫停後從未恢復，而連線池的 session 沒有對話維度（跨對話 context 洩漏），無法安全啟用 |

## MCP HTTP/SSE 傳輸（W20）

| 功能 | 說明 |
|------|------|
| HTTP Server | `duduclaw http-server --bind 127.0.0.1:8765`：Bearer 認證 REST + SSE |
| 端點 | `POST /mcp/v1/call`（單次 JSON-RPC 工具呼叫）、`GET /mcp/v1/stream`（長連 SSE）、`POST /mcp/v1/stream/call`（async + SSE push）、`GET /healthz`（免認證） |
| 速率限制 | Token bucket `OpType::HttpRequest`，60 req/min |
| SSE 連線管理 | `mcp_sse_store.rs` 以 broadcast channel 管理 SSE 連線 |

## ERP 整合

| 功能 | 說明 |
|------|------|
| Odoo Bridge | 17 個 MCP 工具（CRM／銷售／庫存／會計）、JSON-RPC 中間層 |
| Edition Gate | CE/EE 自動偵測、功能閘門 |
| 事件同步 | 輪詢器（`[odoo] poll_enabled`）與 `POST /webhook/odoo`（`[odoo] webhook_enabled`，需設定共享密鑰），兩者預設皆關，產生 `odoo_event` 供 autopilot 規則使用 |
| Per-agent 認證隔離 | `OdooConnectorPool` 以 `(agent_id, profile)` 為鍵；audit 紀錄帶 `profile` + `ok=bool`（v1.11.0 / RFC-21 §2） |
| Dashboard 測試後再儲存 | `odoo.test` 接受 inline params；credential 留空 fallback 到已儲存金鑰；inline 模式同樣套用 SSRF / HTTPS / db-name 驗證鏈（v1.13.1） |

## RL 與可觀測性

| 功能 | 說明 |
|------|------|
| RL Trajectory Collector | 頻道互動期間寫入 `~/.duduclaw/rl_trajectories.jsonl` |
| Prometheus 指標 | `GET /metrics`：failover、wiki trust、decision continuity、prompt 壓縮、常駐感知（`tick_*`）、goal-loop 與 live-fork 計數器。六條從未遞增的 request／token／duration／session／channel／budget 序列已於 v1.66 移除；逐請求成本記錄在 `cost_telemetry.db` |
| Dashboard WS 心跳 | Server Ping 30s + 60s 空閒關閉；client `ping` RPC 25s |
| BroadcastLayer | tracing layer 即時串流日誌至 WebSocket 訂閱者 |

## 記憶評測與 Python 層

| 功能 | 說明 |
|------|------|
| LOCOMO 記憶評測（W21，v1.9.4） | `python/duduclaw/memory_eval/`：`retrieval_accuracy` / `retention_rate` / `locomo_integrity_check`；`cron_runner` 是手動執行的 CLI 入口（`python -m memory_eval.cron_runner smoke_test\|weekly_kpis\|monthly_locomo`），repo 中沒有任何東西排程它；5 分鐘 `smoke_test` P0；`build_golden_qa.py` 建立黃金 QA 集；200 筆 `data/golden_qa_set.jsonl`；`duduclaw-memory` 批次查詢 API |
| Python Agents 路由（v1.9.4） | `python/duduclaw/agents/`：能力導向路由（`capabilities/` manifest loader + matcher、`routing/` router + resolution + memory_resolver） |
| Python MCP 範圍強制（v1.9.4） | `python/duduclaw/mcp/`：API key 認證 + key masking；memory 工具（store/read/search/namespace/quota）在 `execute()` 入口嚴格強制範圍（`memory:write` / `memory:read`） |

## Web 儀表板

| 功能 | 說明 |
|------|------|
| 路由 | `web/src/App.tsx` 內約 74 條非轉址路由（另有約 30 條 legacy 轉址別名，讓舊書籤繼續可用）。四個殼：工作區（`/`、`/chat`、`/tasks`、`/goals`、`/inbox`、`/files`、`/mail`、`/timeline`、`/foresight`、`/gallery`、`/canvas`…）、AI 員工（`/agents`、`/agents/:id/:tab`、`/agents/new`、`/experts`、`/org`、`/presets`）、`/manage/*`（通道、日誌、帳務、成員、部門、經銷商、推理、本地模型、微調、可靠性、安全稽核、知識庫信任…）、`/app/system/*`（設定、安全、帳號、授權、因果、決策實驗室、CCR…），以及獨立頁（`/login`、`/welcome`、`/webchat`、`/console`、`/mascot-overlay`、`/pet-studio`、`/world`、`/launcher`）。側邊欄顯示什麼以 `web/src/components/layout/nav-model.ts` 為準 |
| 技術棧 | React 19 + TypeScript + Tailwind CSS 4 + Base UI + CVA |
| DuDuClaw 設計系統（mds） | 共用 `web/src/components/mds/` 元件庫（OKLCH token、四層表面、三層陰影、Inter／Geist Mono）+ `nav-model.ts` 分組側邊欄（個人／工作／公司／設定）+ `web/DESIGN.md` 設計規範；全頁面以共用元件建構，en/ja/zh i18n 同步 |
| 即時日誌串流 | BroadcastLayer tracing → WebSocket 推送 |
| Memory → Key Insights 分頁 | `key_facts` 卡片 + access_count badge + 時間戳 + 可收合的來源 metadata |
| Memory → 自主進化分頁 | 學習總覽、停滯警示、拒絕統計、白話 playbook 規則卡（可匯出 JSON、手動停用）（[38-aee-playbook-evolution.md](38-aee-playbook-evolution.md)） |
| Logs → 歷史分頁重寫 | 來源篩選 chips + per-source 計數 + 嚴重度下拉 + 嚴重度著色左框 + JSON 細節展開 |
| Toast 通知 | 模組級事件匯流排，max-5 queue，暖色 stone/amber/emerald/rose 變體，尊重 `prefers-reduced-motion` |
| OrgChart | D3.js 互動式 Agent 階層視覺化 |
| Session Replay | 對話回放 + 時間軸 |
| WikiGraph | 互動式知識圖譜 |
| 國際化 | zh-TW / en / ja-JP（600+ 翻譯鍵） |
| 深淺色主題 | 系統偏好 + 手動切換 |
| Experiment Logger | Trajectory recording，供 RL/RLHF 離線分析 |
| Marketplace RPC | `marketplace.list` 提供內建 MCP 目錄：v1.67.1 起共四張卡片（Playwright `@playwright/mcp`、Browserbase `@browserbasehq/mcp`、Filesystem、Memory），另合併 `~/.duduclaw/marketplace.json` 的項目 |
| Partner Portal | SQLite `PartnerStore` + profile/stats/customers CRUD + 7 個 RPC |

## 商業功能

| 功能 | 說明 |
|------|------|
| 授權分層 | `crates/duduclaw-license/src/tier.rs` 中有九個層級（opensource、hobby、solo、studio、business、partner、personal_pro_self_host、self_host_pro、oem）。能力閘是 `premium_templates`、`white_label` 與 `industry_evolution_params`；見 [LICENSING.md](../../../LICENSING.md) |
| 硬體指紋 | 授權綁定 |
| 產業模板 | 製造業／餐飲業／貿易業（免費）；付費產業包需要 `premium_templates` |
| CLI 工具 | 12+ 子命令 |
| Partner Portal | 多租戶經銷商介面 |
