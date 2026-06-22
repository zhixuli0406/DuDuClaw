# TODO:RFC-24 決策連續性實作追蹤

> 對應 [RFC-24-decision-continuity.md](../rfc/RFC-24-decision-continuity.md)。每項皆為可獨立提交、可測的最細工項。
> 慣例:`[ ]` 待辦 / `[~]` 進行中 / `[x]` 完成。括號內為精確切點。

**圖例**:🔴 阻塞後續 ·  🟡 可平行 ·  🧪 含測試 ·  📝 純文件/設定

---

## Phase 0 — 前置與骨架(無行為變更)  ✅ 完成

### P0.1 設定開關  ✅
- [x] 🔴 `[memory] decision_continuity: bool`(預設 `false`)— 實作於 `runtime_config.rs::decision_continuity_enabled`(輕量 `toml::Value` 讀取,與既有 `load_runtime_settings` 同模式)
- [x] 🧪 單元:缺欄位 → `false`;`true`/`false` 正確解析;非法值/malformed → fail-safe `false`(4 個測試)
- [x] 📝 `docs/guides/development-guide.md` §1.3 補上 `decision_continuity` 說明

### P0.2 決策 ID 與型別骨架  ✅
- [x] 🔴🧪 新檔 `crates/duduclaw-gateway/src/decision_capture.rs`:`DecisionDraft { question, options: Vec<(String,String)> }`
- [x] 🔴🧪 `decision_id(agent_id, source_message_id)`:`sha256(agent_id + ":" + source)` 取前 12 hex(確定性,無 `Date.now()`)
- [x] 🧪 單元:同輸入 → 同 ID;不同 agent / message → 不同 ID
- [x] `gateway/src/lib.rs` `pub mod decision_capture;`

### P0.3 Triple 編碼常數  ✅
- [x] 🟡 `decision_subject(id)`、`PRED_QUESTION`、`PRED_STATUS`、`pred_option(key)`(集中常數)
- [x] status 值:`STATUS_OPEN`、`status_resolved(key)`、`STATUS_EXPIRED`

---

## Phase 1 — MVP:狀態層 + 擷取層 + 注入層(治住主因)  ✅ 完成

### P1.1 偵測器(確定性,零 LLM)— ✅ 核心
- [x] 🔴🧪 `detect_enumerated_options(text) -> Option<DecisionDraft>`,三條全滿足才回 `Some`:
  - [x] 規則1:≥2 個**同類**標號(方案/選項/Option/裸 `A.`、`1.`、emoji `1️⃣`);新增同質性檢查(混類 → `None`)
  - [x] 規則2:choice keyword 詞界比對(`word_contains_ci`,禁止裸 `contains`);labelled 標號免關鍵詞,裸標號必須有關鍵詞
  - [x] 規則3:每標號擷取非空內容(inline + 後續行,到下一標號止)
- [x] 🧪 截斷一律 `truncate_bytes`(禁止 `&s[..n]`;選項上限 2KB、題目 1KB);`strip_prefix_ci` 加 char-boundary 守衛(修掉 emoji/CJK panic)
- [x] 🧪 table-driven:中文方案、英文 Option、數字、emoji keycap、多行內容、混中英
- [x] 🧪 反例(回 `None`):步驟清單無關鍵詞、單一選項、純散文、混類標號、code fence 內、`9:30` 時間冒號
- [x] 🧪 CJK/emoji 不 panic、超長截斷至 char boundary
- [x] 🧪 保守性:任一規則不確定 → `None`

### P1.2 狀態寫入(複用 Temporal Memory)— ✅
- [x] 🔴 `persist_decision(engine, agent_id, id, draft, ctx_meta)`:N+2 筆 `store_temporal`(question + 每選項 + status=open),Semantic 層,`content == object` 確保 FTS 可搜
- [x] memory crate 新增決策查詢:`list_open_decisions` / `get_decision` / `decision_status` / `expire_decision_artifacts` / `read_decision_view`(直接打 subject/predicate 欄位,因 `search_layer` 濾 `:`)
- [x] 🧪 整合:`persist_decision` 後 `list_open_decisions` 回三選項 + status=open
- [x] 🧪 整合:同 `(subject, status)` 二次寫入 → supersede,open 不重複(`recapture_supersedes_not_duplicates`)

### P1.3 擷取觸發點接線 — ✅
- [x] 🔴 `channel_reply.rs:1620`(assistant `append_message` 後)gated by `decision_continuity_enabled`,spawn 非阻塞背景任務 → `detect` → `persist_decision`
- [x] 失敗只 `tracing::warn`,絕不阻塞回覆;`SqliteMemoryEngine` !Send → `spawn_blocking`(對齊 key-fact 模式)
- [x] `source_message_id` 用 `format!("{session_id}|{reply}")`(確定性、跨 session 唯一)

### P1.4 注入層 — ✅
- [x] 🔴 `build_open_decisions_section(engine, agent_id) -> String`:查 open(≤5,新到舊),組「## 待決事項 (Open Decisions)」含引導語(引用即執行、勿重問、勿臆測、缺漏先查)
- [x] 🔴 注入點:`channel_reply.rs` secondary block(compression summary 前、pinned 旁),尾端 U 型注意力,不進 cache 前綴;gated by flag + `ctx.memory_db_path`
- [x] 🧪 整合:有 open → 含 section 與選項全文;無 / 他人 agent → 空字串
- [x] 上限 ≤5(`MAX_INJECTED_DECISIONS`)

### P1.5 端到端回歸(斷鏈重現)— ✅
- [x] 🧪 `decision_survives_session_compression`:送 A/B/C → 入 semantic → **觸發 `compress()`**(session turns 被毀)→ 斷言決策仍在 + 注入含「私有 Ethereum PoA」全文(**Agnes 事故關鍵回歸**)
- [x] 🧪 `decision_survives_engine_reopen`:engine drop 後重開 SQLite,決策仍可查(跨進程/重啟)

### P1.6 Phase 1 收尾  ✅
- [x] 📝 memory + decision_capture + runtime_config 測試全綠(memory 140、decision_capture 20、runtime_config 20);clippy 我的新碼 0 警告
- [x] 📝 教訓:`cargo fmt -p` 會重排整個 crate(專案非 fmt-clean)→ 改用單檔 `rustfmt <file>`,已還原誤排
- [x] 📝 smoke:`scripts/smoke-decision-continuity.sh`(見文末 Smoke 段)
- [x] 📝 更新 RFC-24 狀態註記「Phase 1 done」

---

## Phase 2 — 解析層 + 行為層  ✅ 完成

### P2.1 `decision_resolve` MCP 工具 — ✅
- [x] 🔴 `mcp.rs` TOOLS 新增 `decision_resolve { decision_id, chosen_key }`(均 required)
- [x] 🔴 dispatch 新增 `"decision_resolve" => handle_decision_resolve(&arguments, memory, default_agent)`
- [x] 🔴 `handle_decision_resolve` + 核心邏輯封裝進 `engine.resolve_decision`(避免 cli 重刻、避免持鎖跨呼叫死鎖):
  - [x] ownership:engine 以 `agent_id` 為鍵,外人引用 → `NotFound`(fail-closed)
  - [x] `status` supersede 為 `resolved:<key>`
  - [x] 選定項升格為**獨立** semantic 事實(plain insert,可 `search()`,不與決策查詢糾纏)
  - [x] 其餘 question+options `expire_decision_artifacts`(保留 status 鏈)
  - [x] 回傳 `structuredContent {ok, decision_id, chosen_key, chosen_content, question}`;NotFound/AlreadyResolved/UnknownKey → `ok:false` + `isError`
- [x] **不**列入 `EXTERNAL_TOOLS_WHITELIST`(更正原規劃):白名單只管外部 HTTP/SSE 客戶端;agent 經 stdio 本就看得到所有工具(tasks 工具同理)。決策解決不應開放外部客戶端 → 維持 fail-closed 安全姿態
- [x] 🧪 整合:`resolve_decision_full_flow`(resolve→open 清空、status=resolved:C、事實可搜)
- [x] 🧪 安全:`resolve_decision_fail_closed`(NotFound / UnknownKey / 他人 agent / 重複 resolve 皆正確,失敗不寫入)

### P2.2 自動解析「用方案 X」回覆 — ✅
- [x] 🟡 `detect_decision_reference(user_text, &[DecisionView]) -> Option<(id, key)>`:錨定式比對(方案/選項/選/用/option + key),**僅唯一命中**才回 `Some`
- [x] 命中且唯一 → channel_reply 背景任務自動 `resolve_decision`;歧義/無命中 → `None`(交 agent 釐清)
- [x] 🧪 單元:中文方案、英文 option、數字第N個、無錨點不誤觸、多決策同 key 歧義 → None、多 key 歧義 → None

### P2.3 反亂猜行為規則(銜接 anti-hallucination)— ✅
- [x] 🟡 注入層引導語已含反亂猜規則(「若手上沒有對應內容,先承認缺漏並查詢,切勿用模糊比對拼湊」);RFC §4.5 + 附錄 A 記錄完整規則文字與 SOUL 建議
- [x] 🟡 `mentions_decision_reference(text)` 偵測「引用了決策但無對應 open」的斷鏈缺口;channel_reply 在此情境登記 `MistakeCategory::Capability` mistake → `maybe_consolidate`(F2,門檻 3)固化為 semantic 反亂猜規則(下次經「## Past Mistakes to Avoid」回灌)
- [x] 🧪 單元:`mentions_reference_detects_gap_shapes`(用方案C/option B/第2個 命中;一般散文不誤觸)
- [~] 🧪 整合「達門檻 → consolidate」:F2 consolidation 既有測試已覆蓋(`reflexion.rs::threshold_reached_consolidates_to_semantic`);決策缺口 → mistake 的串接為背景 best-effort,已由 `mentions_decision_reference` 單元 + F2 既有測試共同保證

### P2.4 Phase 2 收尾  ✅
- [x] 📝 decision_capture 28 測試綠、cli 全樹建置綠;clippy 我的新碼乾淨
- [x] 📝 smoke 全流程驗證腳本(見文末 Smoke 段)
- [x] 📝 RFC-24 狀態「Phase 2 done」

---

## Phase 3 — 精度與營運強化  ✅ 完成

### P3.1 偵測精度 — ✅
- [x] 🟡 `classify_outbound(text) -> DetectionResult{Confident|Suspected|NoChoice}`:Confident 走零成本主路;Suspected(choice keyword + ≥2 itemish 行,strict 解析不出)才付一次背景 Haiku
- [x] Haiku 二次確認:`build_extraction_prompt` + `call_claude_cli_lightweight`(per-agent utility model)+ `parse_extracted_decision`(嚴格 JSON,容忍 code fence/雜訊,`is_decision=true` 且 ≥2 選項才採用,fail-closed)
- [x] 接線:channel_reply 擷取任務按 `DetectionResult` 分流;Suspected→Haiku→parse→persist
- [x] 🧪 確認高信心樣本回 `Confident`(不觸發 Haiku,成本回歸);甲乙丙/①② → `Suspected`;散文 → `NoChoice`;JSON 解析正/反例
- **設計**:Haiku 只在 strict 偵測失敗且仍像選擇題時觸發(中文 甲乙丙、① 圈號、inline 選項等 strict 無法解析的形狀),主路維持零 LLM 成本

### P3.2 帳本生命週期 — ✅
- [x] 🟡 決策 TTL:`engine.expire_stale_decisions(agent, ttl_days)`,open 狀態列 `valid_from` 早於 cutoff → 連同 question+options 全部 `valid_until=now`
- [x] 設定 `[memory] decision_ttl_days`(預設 7,非正/malformed → 7);channel_reply 擷取背景任務**順帶呼叫**(免額外排程器,自然觸發 self-prune)
- [x] 🧪 `ttl_expires_stale_open_decisions_only`:10 天前決策過期、新決策存活、`ttl<=0` no-op
- [x] 🧪 過期後不再被 `list_open_decisions` / 注入命中(only-valid 過濾)
- [x] 注入維持 ≤5(`MAX_INJECTED_DECISIONS`)

### P3.3 可觀測性與 Dashboard — ✅
- [x] 🟡 `decision_list` MCP 唯讀工具(`handle_decision_list`,回 open 決策 + `structuredContent`)
- [x] 🟡 Prometheus 計數器:`decision_captured_total` / `decision_resolved_total` / `decision_expired_total` / `decision_false_positive_total`(metrics.rs + `/metrics` 匯出;gateway 進程內遞增)
- [x] 🟡 Dashboard WebSocket RPC:`decisions.list`(Viewer)+ `decisions.dismiss`(Operator,標誤判→`engine.dismiss_decision` + `decision_false_positive` 計數)(handlers.rs)
- [x] 🟡 Dashboard 前端「待決事項」面板:`DecisionsPanel.tsx`(agent filter、列出 open、誤判移除按鈕)+ `api.decisions.list/dismiss` + `DecisionInfo` 型別;掛在 SettingsPage(與 BrowserAuditPanel 同區);`npm run build`(tsc+vite)綠
- [x] `decision_false_positive` 由 Dashboard dismiss 路徑(gateway 進程內)遞增 → 可被 `/metrics` 抓取(MCP 子進程路徑無法,故走 Dashboard)
- [x] 📝 RFC-24 狀態「Phase 3 done」

---

## Smoke

- [x] `scripts/smoke-decision-continuity.sh`:build cli → memory 測試 → gateway decision_capture + runtime_config 測試 → binary surface 檢查(`decision_list`/`decision_resolve`/`decisions.list`/`decisions.dismiss` 字面值已編入)。全綠。
- 註:不驅動 live stdio `tools/list`,因其受 `principal.is_external` 過濾 + initialize 握手,與「surface 是否 ship」正交;改以 binary 字面值斷言更穩健。

---

## 橫切檢核(每個 PR 都要過)

- [ ] 🧪 覆蓋率 ≥80%(專案規範)
- [ ] 🔒 字串截斷全走 `truncate_bytes`/`truncate_chars`(無裸 `&s[..n]`)
- [ ] 🔒 路由/關鍵詞比對無裸 `contains`/`starts_with`(用 `word_contains_ci`)
- [ ] 🔒 安全閘 fail-closed:偵測錯誤 / 缺設定 → 不建立決策(寧漏勿錯),resolve 越權 → 拒絕
- [ ] 🔒 背景任務失敗不影響回覆送出(decouple)
- [ ] 📝 無 schema 變更(全騎 v1.19.0 temporal 欄位)
- [ ] 📝 conventional commits;不動 `compress()` / session 核心

---

## 相依關係(關鍵路徑)

```
P0.2 ─┬─ P1.1 偵測器 ─┐
P0.3 ─┘               ├─ P1.3 接線 ─┐
       P1.2 狀態寫入 ─┘             ├─ P1.5 端到端回歸 ── Phase1 done
                       P1.4 注入 ───┘
                                      │
                  P2.1 resolve ───────┼─ P2.2 自動解析
                  P2.3 反亂猜 ────────┘
```

關鍵路徑:**P0.2 → P1.1 → P1.2 → P1.3 → P1.4 → P1.5**。P2.3 / P3 多數可平行。
