# RFC-24:決策連續性(Decision Continuity)— 跨 session 的承諾與選單持久化

| 欄位 | 值 |
|------|----|
| 狀態 | Implemented(Phase 1-3 全部完成:狀態/擷取/注入/解析/行為/TTL/Haiku 二次確認/Dashboard 面板 + smoke) |
| 作者 | DuDuClaw Core |
| 建立日期 | 2026-06-22 |
| 目標版本 | v1.23.0 |
| 相關文件 | `commercial/docs/TODO-instruction-pinning.md`、`commercial/docs/TODO-anti-hallucination.md`、`commercial/docs/TODO-key-fact-accumulator.md`、`docs/architecture/evolution-engine.md` |
| 取代 / 擴充 | 擴充既有 `pinned_instructions`(session.rs)+ Temporal Memory F1(RFC 無,v1.19.0 實作) |

---

## 1. 問題陳述

### 1.1 觸發事故(Agnes 斷鏈)

一個 agent(Agnes)在某次對話結尾向使用者提出 A/B/C 三個方案,使用者於**稍後**回覆「用方案 C」。此時 agent 手上**沒有任何「方案 C 是什麼」的記錄**,於是:

1. 從歷史記錄裡撈到一筆**無關**的舊「方案 C」(2026-04-21 的區塊鏈方案),差點張冠李戴;
2. 只能誠實回報「脈絡已遺失」,反問使用者「方案 C 是關於什麼」。

這不是模型記性問題,而是**架構問題**:一個「未來會被引用的高價值決策狀態」,只存在於會被壓縮 / 丟棄的 conversation turns 裡,從未進入任何**獨立於對話記憶之外、且能跨 session 存活**的持久層。

### 1.2 為什麼「叫 agent 記得存 wiki」治不了

Session 是**被動斷裂**的(進程重啟、50k 壓縮、新 thread)。Agent 在 session 結束前根本沒有可靠的執行點去主動存檔。任何依賴「agent 紀律」的方案都會在真實負載下漏掉。根因解必須是**自動、與對話記憶解耦、由系統在訊息送出時觸發**。

---

## 2. 根因分析(基於現有程式碼)

斷鏈在三個情境都會發生(已與需求方確認三種都有)。對應到實際機制:

### L-A 狀態層:出站選單沒有獨立持久路徑
- 出站 assistant 回覆**有**入庫:`channel_reply.rs::build_reply_with_session_inner` → `session_mgr.append_message(session_id, "assistant", &reply, …)`(約 `channel_reply.rs:1614`)。
- 但它入的是 `session_messages` 表 —— 一個會被壓縮與截窗的易失層。選單內容沒有任何結構化、可定址(`方案C → 內容`)的副本。

### L-B Session 層:`--resume` 停用 + 破壞性壓縮 + 20-turn 截窗
- **`--resume` 實際停用**(`channel_reply.rs:3499-3503`):Claude CLI 100% 拒絕 `--resume`,系統一律改用 *history-in-prompt*,且 `format_history_as_prompt` **只保留最後 20 turns**(`channel_reply.rs:951-1027`,`max_history_turns = 20`)。→ 跨時間回到同一 thread,靠的是從 DB 重載最近 20 turn,**選單若超出窗就消失**。
- **`compress()` 是破壞性的**(`session.rs:278-309`):達 `COMPRESSION_THRESHOLD = 50_000`(`session.rs:15`)時 **DELETE 全部 turns**,只塞回一條 system-role Haiku bullet 摘要。背景 summarizer(`session_summarizer_task.rs`)亦只保留最後 3 turn 逐字。→「方案C = 私有 Ethereum PoA」正是會被摘要抹平的細節。

### L-C 行為層:斷鏈時 agent 會「亂猜歷史」
- 既有「## Past Mistakes to Avoid」(F2a Reflexion)與反幻覺規則,沒有覆蓋「使用者引用了我手上沒有的決策」這個 pattern。Agnes 因此用模糊比對撈到無關舊方案,而非先承認缺漏、查權威來源。

### 既有可複用資產(關鍵)
- **`pinned_instructions`**(`session.rs:126`):**已能撐過 `compress()`**(測試 `test_pinned_survives_compression`,`session.rs:744-765`)、在 prompt 尾端 U 型注意力位置注入(`channel_reply.rs:5196-5207`)、且會在「上一條 assistant 含『?』時把使用者回答累積進 pinned」(`channel_reply.rs:1034-1048`)。**這是現成最接近的鉤子**,但目前只擷取 turn 1 的「使用者任務」,不擷取 agent 出站選單。
- **Temporal Memory F1**(`duduclaw-memory/src/engine.rs`):`store_temporal(entry, TemporalMeta)`(`254-344`)寫入 **semantic 層,是獨立 SQLite store,`compress()` 完全碰不到**;`search()` 預設只回 `valid_until IS NULL OR valid_until > now`(`843`);相同 `(subject, predicate)` 自動 supersede 形成鏈(`273-312`);`get_history()` / `get_at()` 可回溯。→ **天然不死的決策載體 + 內建作廢語意。**
- **Pending tasks 注入範式**(`claude_runner.rs::build_pending_tasks_section`,`168-242`):已示範「取 ≤5 筆 → 排序 → 組 section 注入 system prompt」,可照抄給決策用。

---

## 3. 設計目標 / 非目標

### 目標
1. **G1 自動擷取**:agent 一旦送出含「列舉式選項」的訊息,系統自動把每個選項的**完整內容**存成可定址的結構化記錄,零 agent 紀律依賴。
2. **G2 跨界存活**:該記錄必須撐過 ① 50k 壓縮 ② 20-turn 截窗 ③ 進程重啟 ④ 新 thread / 新 session。
3. **G3 自動回灌**:下一輪(任何 session)若該 agent 有 open 決策,自動注入 system prompt,讓「用方案 C」可被解析。
4. **G4 解析即作廢**:使用者選定後,選定項落地為事實、其餘作廢、決策關閉,且保留可回溯鏈。
5. **G5 行為兜底**:agent 遇到「引用了我沒有的決策」時,先查決策帳本 / 承認缺漏,**禁止用模糊比對亂猜歷史**。

### 非目標
- 不重寫 `compress()` / session schema(維持非侵入,沿用 v1.19.0 的設計哲學)。
- 不啟用 Claude CLI `--resume`(已驗證不可行)。
- 不做跨 agent 的決策共享(本 RFC 限單一 agent 自己的決策連續性;跨 agent 走既有 shared wiki / task board)。
- 不偵測自然語言中的隱性承諾(僅偵測**結構化列舉**;隱性承諾留待 future work)。

---

## 4. 架構:五個切面

```
                 使用者訊息 in
                      │
   ┌──────────────────┼───────────────────────────────────┐
   │  [注入層 §4.3]   ▼                                     │
   │   system prompt += "## 待決事項 (Open Decisions)"      │
   │   (open decisions for this agent, ≤5)                 │
   └──────────────────┬───────────────────────────────────┘
                      ▼
              Claude 產生回覆
                      │
   ┌──────────────────┼───────────────────────────────────┐
   │  [擷取層 §4.2]   ▼  偵測出站文字是否含列舉式選項        │
   │   detect_enumerated_options(reply)                     │
   │     └─ 命中 → store_temporal(decision triples)         │  ←─ [狀態層 §4.1]
   │                (semantic 層,compress 碰不到)            │      Temporal Memory
   └──────────────────┬───────────────────────────────────┘
                      ▼
                  送出 channel
                      ⋮ (時間 / session / 進程斷裂都無妨)
                      ▼
              使用者:「用方案 C」
                      │
   ┌──────────────────┼───────────────────────────────────┐
   │  [解析層 §4.4]   ▼  注入的 open decision 已含 C 全文    │
   │   agent 解析 → 確認 → 呼叫 decision_resolve(id, "C")    │
   │     └─ 選定項 → 落地事實;其餘 supersede;決策關閉        │
   └──────────────────┬───────────────────────────────────┘
                      ▼
   [行為層 §4.5] 若引用的決策不存在 → 承認缺漏 + 查帳本,禁止亂猜
```

### 4.1 狀態層:DecisionLedger on Temporal Memory(semantic)

**不造新表**,直接複用 `store_temporal`。一筆決策 = 一組三元組:

| 用途 | subject | predicate | object | layer |
|------|---------|-----------|--------|-------|
| 決策題目 | `decision:<id>` | `question` | 題目全文 | Semantic |
| 選項 C 內容 | `decision:<id>` | `option:C` | 「私有 Ethereum PoA」全文 | Semantic |
| 選項 A / B … | `decision:<id>` | `option:A` … | 各選項全文 | Semantic |
| 狀態 | `decision:<id>` | `status` | `open` / `resolved:C` / `expired` | Semantic |

`TemporalMeta { subject, predicate, object, valid_from = now, valid_until = None, confidence = 1.0, metadata }`,`metadata` 存 `{channel, thread_id, created_turn, source_message_id}`。

- **存活**:semantic 層獨立於 `session_messages`,`compress()` 不觸碰 → G2 達成。
- **作廢**:`status` 用同 `(subject="decision:<id>", predicate="status")`,選定時 `store_temporal` 自動把舊 `open` supersede 成 `resolved:C`,鏈可回溯 → G4。
- **`<id>` 生成**:`sha256(agent_id + source_message_id)` 前 12 字元,去 `Date.now()` 依賴(沿用既有確定性 ID 慣例)。

> 替代方案(已評估,見 §10):擴充 `pinned_instructions` 或新建 `DecisionNotebook`。選 Temporal Memory 因為它**同時**提供「不死儲存 + 作廢語意 + 回溯鏈 + 預設只回 valid」,其餘方案都得自己補這幾塊。

### 4.2 擷取層:出站訊息自動偵測(核心,零紀律依賴)

切點:`channel_reply.rs` 在 `append_message(…, "assistant", &reply, …)` 之後(`~1614`)新增非阻塞背景任務 `capture_decision_if_any(agent_id, source_message_id, &reply, channel_ctx)`。

**偵測器 `detect_enumerated_options(text) -> Option<DecisionDraft>`(確定性,零 LLM 成本):**
- 規則(全部需滿足才算決策,fail-safe 偏保守):
  1. 文中出現 ≥2 個**同類標號**:`方案 [A-Z]`、`選項 [0-9]+`、`Option [A-Z]`、或行首 `[A-Z]\.` / `[0-9]+\.` / `[0-9]+️⃣`;
  2. 上下文出現決策性關鍵詞(`方案`/`選項`/`option`/`哪一個`/`你想`/`選擇`/`建議`,以 `duduclaw_core::word_contains_ci` 做詞界比對,**不可用裸 contains**);
  3. 每個選項擷取出非空內容(標號後到下一標號 / 段落止)。
- 命中 → 產出 `DecisionDraft { question, options: Vec<(key, content)> }` → 寫入狀態層(§4.1)。
- **保守失敗**:任一條件不確定就**不**建立決策(寧可漏記,不可亂記),與 `pinned_instructions` 的擷取哲學一致。
- **UTF-8 安全**:所有截斷走 `duduclaw_core::truncate_bytes` / `truncate_chars`,禁止 `&s[..n]`(CJK/emoji 會 panic)。

> 偵測精度不足時的升級路徑:背景 Haiku 二次確認(沿用 `run_utility_prompt`),僅在確定性偵測「疑似但不確定」時觸發,維持零成本主路。列為 Phase 2。

### 4.3 注入層:open decisions 回灌 system prompt

照抄 `build_pending_tasks_section` 範式,新增 `build_open_decisions_section(agent_id) -> String`:
- 查 `search_layer(agent_id, "decision:", Semantic, …)` 取 `status = open` 的決策(≤5,新到舊);
- 組成尾端 section,置於 pinned 之前/旁(U 型注意力):

```
## 待決事項 (Open Decisions)
你先前向使用者提出過以下選項,使用者可能會以「用方案 X / 選 X」回覆。
若使用者引用了某個選項,直接依此內容執行,不要重新詢問,也不要從歷史臆測:

[decision:a1b2c3] 題目:區塊鏈整合方案
  - A:公有鏈 + L2
  - B:聯盟鏈 Hyperledger
  - C:私有 Ethereum PoA
```

注入點:`channel_reply.rs:5196-5207`(pinned 注入處)同段落,或 `1051-1116` 的 secondary 區塊。**不進 prompt cache 前綴**(與 pending tasks 同策略,避免汙染快取)。

### 4.4 解析層:選定即落地

新增 MCP 工具 `decision_resolve(decision_id, chosen_key)`(註冊範式見 `mcp.rs:32` TOOLS / `6752` dispatch / `9368` handler):
- 把 `(decision:<id>, status)` supersede 成 `resolved:<key>`;
- 把選定項 `(decision:<id>, option:<key>)` 的內容**升格**為一筆 agent 的長期 semantic 事實(讓「這個決策最後選了 C = 私有 Ethereum PoA」可被未來 `search()` 正常命中);
- 其餘未選項標 `expired`(保留鏈,不刪)。
- Agent 在注入提示引導下,於確認使用者意圖後呼叫此工具。亦可由系統在偵測到明確「用方案 X」回覆時自動觸發(Phase 2)。

### 4.5 行為層:反亂猜兜底(銜接 TODO-anti-hallucination)

在 system prompt(或 SOUL.md 預設片段)加一條可被 F2 Reflexion 強化的規則:

> 當使用者引用一個你手上沒有完整內容的決策 / 方案 / 承諾(例如「用方案 C」但「待決事項」沒有對應項):**先承認缺漏並查詢決策帳本 / shared wiki,禁止用模糊字串比對從歷史記錄拼湊**。寧可反問,不可張冠李戴。

並把「引用了不存在的決策」登記為一個 `ErrorCategory`(Significant)訊號餵入既有 F2(`reflexion.rs::maybe_consolidate`),累積後固化為 semantic 規則 —— 讓系統**從斷鏈事故中學習**,而非每次重犯。

---

## 5. 資料模型摘要

```rust
// 擷取階段的中間結構(不落地,僅傳遞)
struct DecisionDraft {
    question: String,
    options: Vec<(String /*key, e.g. "C"*/, String /*content*/)>,
}

// 落地:N+2 筆 store_temporal 呼叫(question + status + 每個 option 一筆)
// subject 一律 "decision:<id>",靠 predicate 區分;valid_until=None 表 open
```

無 schema 變更。完全騎在 v1.19.0 既有的 temporal 欄位(`valid_from/valid_until/superseded_by/supersedes/subject/predicate/object/confidence/metadata`)上。

---

## 6. 與現有機制的關係

| 既有機制 | 本 RFC 的關係 |
|----------|----------------|
| `pinned_instructions`(撐過壓縮) | 同哲學,但 pinned 管「使用者任務」,本 RFC 管「agent 出站決策」。兩者並存、互補,皆注入尾端。 |
| history-in-prompt 20-turn 截窗 | 本 RFC 讓決策**獨立於**此窗存活;窗內仍照常提供近期上下文。 |
| `compress()` 破壞性壓縮 | 不改 compress;決策存在 semantic 層,壓縮碰不到。 |
| Temporal Memory F1 | 直接複用 `store_temporal` / `search_layer` / supersession,無新儲存。 |
| F2 Reflexion | §4.5 把斷鏈登記為 ErrorCategory,固化反亂猜規則。 |
| pending tasks 注入 | 注入範式照抄 `build_pending_tasks_section`。 |

---

## 7. 實作切點(精確)

| 階段 | 檔案:行 | 動作 |
|------|----------|------|
| 擷取觸發 | `crates/duduclaw-gateway/src/channel_reply.rs:~1614`(append_message assistant 之後) | spawn 非阻塞 `capture_decision_if_any` |
| 偵測器 | 新檔 `crates/duduclaw-gateway/src/decision_capture.rs` | `detect_enumerated_options` + 純函式單元測試 |
| 狀態寫入 | 複用 `crates/duduclaw-memory/src/engine.rs:254` `store_temporal` | N+2 筆 triple |
| 注入 | `crates/duduclaw-gateway/src/channel_reply.rs:5196`(pinned 注入處附近) | `build_open_decisions_section` |
| 解析工具 | `crates/duduclaw-cli/src/mcp.rs:32 / 6752 / 9368` | 註冊 + dispatch + handler `decision_resolve` |
| 行為規則 | SOUL.md 預設片段 / `reflexion.rs:27` | 反亂猜規則 + ErrorCategory 登記 |

---

## 8. Rollout 階段

- **Phase 1(MVP,狀態層 + 注入層)**:確定性偵測器 + `store_temporal` 落地 + `build_open_decisions_section` 注入。預設 **off**,per-agent `agent.toml [memory] decision_continuity = true` 開關(沿用既有 opt-in 慣例)。**此階段即可治住 Agnes 事故的主因。**
- **Phase 2(解析層 + 行為層)**:`decision_resolve` MCP 工具 + 自動偵測「用方案 X」回覆 + 反亂猜 SOUL 規則 + F2 登記。
- **Phase 3(精度強化)**:Haiku 二次確認可疑偵測;decision TTL(預設 7 天自動 `expired`)避免帳本膨脹;`decision_list` MCP 唯讀工具供 dashboard 呈現。

---

## 9. 風險與權衡

| 風險 | 緩解 |
|------|------|
| **偵測誤報**(把非決策當決策) | 偵測器保守:需同時滿足標號≥2 + 決策關鍵詞 + 內容非空;不確定就不記。詞界比對用 `word_contains_ci`,非裸 contains。 |
| **偵測漏報**(真決策沒抓到) | Phase 1 可接受(漏記比亂記安全);Phase 3 Haiku 補救。漏報時退化成現狀,不會更糟。 |
| **帳本膨脹** | Phase 3 TTL 自動 expire;注入只取 ≤5 筆 open;`search` 預設過濾 valid。 |
| **注入吃 context 預算** | 只注入 open 且 ≤5 筆;resolved/expired 不注入;不進 cache 前綴。 |
| **多 byte 截斷 panic** | 全程 `truncate_bytes`/`truncate_chars`,符合 2026-06 安全慣例 §1。 |
| **跨進程併發寫** | `store_temporal` 走 SQLite WAL;若涉 JSONL 旁路需 `with_file_lock`(本設計不寫 JSONL)。 |
| **隱私 / 冗餘** | 決策內容本就是 agent 已送出給使用者的訊息,不引入新的敏感資料面。 |

---

## 10. 替代方案(已評估)

| 方案 | 評估 | 結論 |
|------|------|------|
| **A. 擴充 `pinned_instructions`** | 已能撐過壓縮、已有注入點。但 pinned 是單一文字塊,無「每選項可定址」「作廢鏈」「only-valid 查詢」語意,得自己補。 | 否決(重造輪子) |
| **B. 新建 `DecisionNotebook`(仿 MistakeNotebook)** | 乾淨,但又是一張新表 + 新 CRUD + 新注入,且作廢/回溯要自刻。 | 否決(與 F1 重疊) |
| **C. Temporal Memory F1**(本案) | 不死儲存 + 作廢語意 + 回溯鏈 + only-valid 查詢全內建,非侵入。 | **採用** |
| **D. 啟用 Claude CLI `--resume`** | 已驗證 100% 被拒(`channel_reply.rs:3499`)。 | 不可行 |
| **E. 改 `compress()` 為非破壞性** | 高風險、侵入 session 核心、影響所有 agent。 | 否決(超出範圍) |

---

## 11. 測試計畫

- **單元(偵測器)**:中英混合、CJK、emoji 標號(`1️⃣`)、誤報樣本(列點但非決策)、漏報邊界;`detect_enumerated_options` 純函式,table-driven。
- **單元(狀態)**:`store_temporal` 後 `search_layer` 只回 open;`decision_resolve` 後舊 status supersede、選定項升格事實、其餘 expired;`get_history` 鏈完整。
- **整合(端到端斷鏈重現)**:
  1. agent 送 A/B/C → 斷言三選項入 semantic;
  2. **觸發 `compress()`** → 斷言決策仍在(關鍵回歸,對應 L-B);
  3. 新 session 第一輪 → 斷言 `build_open_decisions_section` 注入含 C 全文;
  4. 使用者「用方案 C」→ `decision_resolve` → 斷言落地 + 作廢。
- **回歸**:覆用既有 `test_pinned_survives_compression` 旁加 `test_decision_survives_compression`。
- 目標覆蓋率 ≥80%(專案規範)。

---

## 12. 未來工作

- 隱性承諾偵測(自然語言「我下週給你方案」)。
- 跨 agent 決策可見性(經 shared wiki SoT policy)。
- Dashboard「待決事項」面板(WebSocket RPC `decisions.list`)。
- 與 RFC-26 Live Forking 整合:分支決策的 ledger 化。

---

## 附錄 A:Agnes 事故在新架構下的重演

| 步驟 | 舊行為 | 新行為(Phase 1+2) |
|------|--------|---------------------|
| Agnes 送 A/B/C | 僅入 `session_messages`(易失) | 同時 `store_temporal` 三選項到 semantic |
| session 斷裂 / 壓縮 | 選項內容被 Haiku 摘要抹平 | semantic 不受 compress 影響,完整保留 |
| 使用者「用方案 C」 | 手上無 C,模糊比對撈到無關舊方案 | system prompt 已注入「待決事項」含 C 全文 |
| Agnes 回應 | 「脈絡遺失,請問 C 是什麼」 | 「方案 C(私有 Ethereum PoA),立即執行」+ `decision_resolve` |
| 若真的查無 | 亂猜歷史 | §4.5 規則:承認缺漏 + 查帳本,禁止臆測 |
