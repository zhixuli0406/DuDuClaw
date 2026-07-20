# 記憶智能

> 會彼此取代的事實、會變成規則的錯誤、可成批取回的記憶——三項升級疊加在現役記憶引擎之上，無須重寫 schema。

---

## 比喻：醫師的病歷

優秀的醫師不會把病歷當成一疊扁平的筆記，而是當成三個彼此相連的習慣來經營：

1. **事實有時間軸。**「病患服用 10mg 該藥物」這件事為真——*直到*劑量被調整。當記下新劑量時，舊的那一行不會被抹去，而是被標註「有效至 3 月 3 日」，由新的一行接手。詢問「去年冬天劑量是多少？」病歷就能從歷史中正確的那一刻給出答案。
2. **錯誤會變成準則。**當某種藥物交互作用第三次被漏掉之後，診所不會只修正那一個案例，而是寫下一條常設規則：「永遠檢查交互作用 X。」下一位醫師讀的是這條規則，而不是那三份事故報告。
3. **取回是成批進行的。**回顧病例時，醫師會用編號精準調出需要的那幾頁——而不是把整本病歷一張一張重看一遍。

DuDuClaw 的**記憶智能**（v1.19.0）賦予 Agent 同樣的三個習慣——以**非侵入式**方式建構在既有的 `SqliteMemoryEngine` 之上（不重寫 schema，`MemoryEntry` 維持不變）。

---

## 三項功能

| | 功能 | 作用 | 所在位置 |
|-|------|------|----------|
| **F1** | Temporal Memory | 事實獲得有效期區間 + 知識圖譜三元組；新事實取代舊事實並串成鏈 | `engine.rs` — `store_temporal`、`get_history`、`get_at` |
| **F2** | Reflexion Loop | 將近期未解決的錯誤注入 prompt（F2a）；將同類別 ≥3 筆錯誤整併為一條 semantic 規則（F2b） | `channel_reply.rs`、`reflexion.rs`、`MistakeNotebook` |
| **F3** | Batch Fetch | 單次呼叫以 ID 取回至多 100 筆記憶，部分命中時回傳 `missing_ids` | `engine.rs` — `get_by_ids`；MCP `memory_fetch_batch` |

三者皆建構於現役引擎——遷移是一個**冪等的 ALTER 迴圈**，而非重建。

---

## F1：Temporal Memory

### 新欄位（冪等遷移）

遷移迴圈新增九個可為 NULL／具常數預設值的欄位，使 `ALTER TABLE ... ADD COLUMN` 在既有資料列上合法，並另建兩個索引：

| 欄位 | 意義 |
|------|------|
| `valid_from` | 事實開始為真的時間（NULL ⇒ 回退到 `timestamp`） |
| `valid_until` | 事實不再為真的時間（NULL ⇒ 仍然有效） |
| `superseded_by` | 取代本列的那一列的 id |
| `supersedes` | 本列所取代的那一列的 id |
| `subject` / `predicate` / `object` | 知識圖譜三元組 |
| `confidence` | 0.0–1.0，預設為 1.0 |
| `metadata` | JSON 區塊，預設為 `{}` |

```sql
-- F1 Temporal Memory columns (v1.19.0) — all nullable / constant-default
ALTER TABLE memories ADD COLUMN valid_from    TEXT;
ALTER TABLE memories ADD COLUMN valid_until   TEXT;
ALTER TABLE memories ADD COLUMN superseded_by TEXT;
ALTER TABLE memories ADD COLUMN supersedes    TEXT;
ALTER TABLE memories ADD COLUMN subject       TEXT;
ALTER TABLE memories ADD COLUMN predicate     TEXT;
ALTER TABLE memories ADD COLUMN object        TEXT;
ALTER TABLE memories ADD COLUMN confidence    REAL NOT NULL DEFAULT 1.0;
ALTER TABLE memories ADD COLUMN metadata      TEXT NOT NULL DEFAULT '{}';

-- Triple index only covers currently-valid rows (cheap conflict lookup)
CREATE INDEX IF NOT EXISTS idx_memories_triple
    ON memories(agent_id, subject, predicate) WHERE valid_until IS NULL;
CREATE INDEX IF NOT EXISTS idx_memories_valid
    ON memories(agent_id, valid_until);
```

迴圈會吞掉 `duplicate column name` 錯誤，因此在已升級的資料庫上重跑屬於 no-op。

### 自動衝突解決

當 `store_temporal(entry, TemporalMeta)` 同時帶有 `subject` 與 `predicate` 時，引擎會把 `(agent_id, subject, predicate)` 視為一個事實識別。任何具有相同三元組、目前仍有效的資料列，會在插入新列之前被關閉：

```
store_temporal(agent="dudu",
               subject="user", predicate="deploy_target",
               object="Cloudflare Workers")
     |
     v
查詢 (dudu, user, deploy_target) 目前仍有效的資料列
     |
   找到？ ──否──> 直接 INSERT 新列（valid_until = NULL）
     |
    是
     |
     v
UPDATE 舊列：  valid_until = now
               superseded_by = <新 id>
     |
     v
INSERT 新列：  supersedes = <舊 id>
               valid_until = NULL   （目前有效）
```

兩列現在被串接成一條**取代鏈（supersession chain）**：

```
[ deploy_target = Vercel ]      [ deploy_target = Cloudflare Workers ]
  valid_from  : Jan 1            valid_from  : Mar 3
  valid_until : Mar 3   ───────► valid_until : NULL  （目前）
  superseded_by ──────────┘      supersedes ─────────┘
```

若沒有完整三元組，`store_temporal` 僅單純記錄一筆帶時間戳的事實——不發生取代。

### 預設過濾為「目前有效」

`search()` / `search_layer()` 會在每個查詢加上 `AND (m.valid_until IS NULL OR m.valid_until > now)`，因此一般取回只會回傳*當下*為真的事實。過期事實仍留在資料庫供查閱歷史，但絕不會洩漏進 prompt。

### 讀取時間軸

兩個讀取 API 揭露這條鏈，也都以 MCP 形式提供 `memory_get_history` / `memory_get_at`（scope `memory:read`）：

| API / MCP 工具 | 回傳 |
|-----|------|
| `get_history(agent, subject, predicate)` — `memory_get_history { subject, predicate }` | 完整取代鏈，由舊到新，含每筆的 `ingested_at`、`invalidated_by_event`/`invalidated_at` 與 `reaffirmed_by` |
| `get_at(agent, subject, predicate, at)` — `memory_get_at { subject, predicate, at }` | 某時間點仍有效的單一事實（`valid_from <= at AND (valid_until IS NULL OR valid_until > at)`） |

### Bi-temporal + build-time provenance（D1）

時序儲存追蹤**兩條**時間軸：`valid_from`/`valid_until`（world-time，事實為真的時段）與 `ingested_at`（transaction-time，系統得知該事實的時間）。取代由 world-time 的 `valid_from` 決定（非攝入順序），因此就算攝入次序被打亂（先得知離婚、後得知更早的結婚），任一時間點仍能解析出正確的事實：`valid_from` 早於現行事實的一筆，會以有界的*歷史區段*插入，不擾動現行事實。再次觀察到完全相同的事實（相同 subject/predicate/object ＋內容）會**再確認（reaffirm）**它：把新的 `source_event` 附加到 `reaffirmed_by`（上限 20）並累加 `access_count`，而不另開新列。當一筆事實被關閉時，關閉用的 `source_event` 與時間會蓋印到被取代的那一列（`invalidated_by_event`/`invalidated_at`）。

### 來源回溯（`memory_invalidate_by_origin`）

`invalidate_by_origin(agent, origin, since)`（MCP `memory_invalidate_by_origin`，scope `admin`）是投毒來源的補救閥門：它會讓某個**精確** `origin` 的所有現行有效事實過期（只過期、不刪除；比對採等值，絕不用子字串），並可選擇只限於 `since` 之後（含）得知的事實。凡 `derived_from` 引用到被清除 id 的事實，其 `origin_trust` 會被壓到 ≤ 0.1（源自投毒輸入的衍生物不能繼續被信任）。`search()` 會立即停止回傳這些被清除的事實，而 `get_history()` 仍保留完整的鏈，並標記 `invalidated_by_event = "origin_purge"`。

### 寫入端投毒防護（D2）

D1 讓你能*撤銷*一個投毒來源；D2 則在源頭就攔下大部分毒物（PoisonedRAG，arXiv:2402.07867）。自動蒸餾的寫入路徑在兩端設防：

- **寫入端掃描＋爆量偵測。**一筆蒸餾出的事實在儲存前，其內容與 `(subject, predicate, object)` 會先過共用的 prompt-injection 規則引擎：命中即**丟棄**該事實（fail-closed，絕不寫入），並記錄一筆 `prompt_injection` 安全稽核事件。另外有一個 per-`(agent, origin, subject)` 的滑動窗計數器（`knowledge_guard`，與 dispatch 斷路器同樣採持久化 ＋ advisory-lock 模式），當單一來源在窗內對同一 subject 寫入 `>= max_per_subject` 筆事實時，會把整批隔離（即「One Shot Dominance」／k-doc 模式）。被隔離的事實以 `quarantined = 1` 儲存，屬**惰性**：它們永不取代乾淨事實，並在人工裁決前被排除於所有取回讀取路徑（FTS、graph、vector、`list_recent`、`summarize`）之外。
- **處理流程。**一次隔離會發出一筆 `ApprovalBroker` 請求（`action_kind = "knowledge_quarantine"`）並送出 `knowledge.quarantined` 事件。核准 → 事實被釋放（`quarantined = 0`，現在可取回）；拒絕 → 事實過期（`invalidated_by_event = "quarantine_reject"`），其 `origin_trust` 壓到 ≤ 0.1；TTL 逾時視同拒絕（fail-closed）。

**排序端信任。**`origin_trust` 現在會參與取回排序（權重 `w_trust`，預設 0.10）：每個候選的分數會乘上 `(1 − w_trust) + w_trust · origin_trust`，讓未經驗證的頻道蒸餾事實（trust 0.3）無法勝過策展過的事實（trust 1.0）。在 HippoRAG-lite graph 中，一個三元組的邊會依其 `origin_trust` 加權，縮小低信任事實的 Personalized-PageRank 質量，直接抑制「單一投毒三元組被 PPR 放大兩跳」這條路徑。舊資料列（trust 1.0）的排序與 D2 之前逐位元組相同。

### 圖檢索演進（D3）

HippoRAG-lite graph 獲得四項各自獨立的改良（對齊 HippoRAG 2 + LightRAG）。每一項都是 fail-safe：在沒有 alias、圖很小、且 embedding seeding 關閉時，排序與先前的 per-query 建圖**逐位元組相同**。

- **持久化增量圖快取。**一旦某 Agent 累積大量事實，每次查詢都重建 Personalized-PageRank 圖就很浪費。圖現在會 per agent 快取（`RwLock`）並跨查詢重用；一個 per-agent 的**世代計數器**（generation counter）會被每一筆更動三元組的寫入（`store_temporal`／supersession、隔離釋放/拒絕、origin purge、decision 逾期、decay 歸檔、GDPR 抹除、Agent 重新指派）遞增，藉此讓過時的快取失效，使查詢永遠看到現行事實。快取只在超過 `GRAPH_CACHE_MIN_TRIPLES`（500）時啟用；低於此值時 per-query 建圖較便宜，故保留。
- **實體別名合併。**一張 `entity_alias(agent_id, canonical, alias)` 表會在建圖與 seeding 之前，把各種表面形式收斂到同一個節點，讓「老闆／李老闆／zhixu」不再是三座孤島。兩側都會正規化（trim + 轉小寫），別名鏈在儲存時攤平。透過 `memory_alias_add` / `memory_alias_list` MCP 工具管理（write／read scope）。沒有任何 alias 時，圖逐位元組相同。
- **述詞邊標籤。**每條 SPO 邊現在會附帶其 predicate 作為標籤（PPR 運算從不讀取它，故排序不變）。`engine.export_graph(agent, limit)` API 回傳一份可序列化的 `{ nodes, edges }` 快照（含仍待裁決的隔離事實，並加註旗標），供 D6 知識圖譜策展 UI 使用。
- **Embedding seeding（可選）。**當 `graph_embed_seed` 開啟**且**掛上 embedder 時，PPR 的 seed 會變成「whole-word FTS 實體命中」與「query embedding 最鄰近實體向量」（同模型 cosine，top-k）兩者的聯集。實體向量會惰性快取於 `entity_embedding`，embedding 失敗則回退到 FTS seeding。預設關閉（沒有 embedder 時為 no-op），呼應 HippoRAG 2 對「弱 embedder 會損失 recall」的提醒。

---

## F2：Reflexion Loop

F2 把**既有**的 `MistakeNotebook` 橋接進回答路徑——它不是一個新的儲存。觸發訊號是既有的 `ErrorCategory`（Significant／Critical，由 MetaCognition 自我調適）——**而非** GVU Verifier（後者驗證的是 SOUL.md 提案）。

### F2a — 將過去的錯誤注入 prompt

在 Agent 回答頻道訊息之前，會把它近期未解決的錯誤，以 `## Past Mistakes to Avoid` 標題浮現到 prompt 中：

```
頻道訊息抵達
     |
     v
擷取以空白分隔的關鍵字（≥3 字元，至多 12 個）
     |
   有關鍵字？ ──否──> query_by_agent(agent, 3)   ← CJK 近期回退
     |                                              （CJK 無空白 token）
    是
     |
     v
query_by_topic(keywords, agent, 3)   ← 主題範疇的回想
     |
   為空？ ──是──> query_by_agent(agent, 3)   ← 近期回退
     |
     v
附加到 prompt：
  ## Past Mistakes to Avoid
  - <錯誤 1 的 prompt 區塊>
  - <錯誤 2 的 prompt 區塊>
```

這把 `MistakeNotebook` 橋接到跨任務學習，讓 Agent 在相似主題上停止重蹈覆轍——而不只在 GVU SOUL.md 路徑內。

### F2b — 將同類別 ≥3 筆錯誤整併成一條規則

當同一個 `MistakeCategory` 累積 `>= DEFAULT_CONSOLIDATE_THRESHOLD`（= **3**）筆未解決項目時，`reflexion::maybe_consolidate` 會把它們合成為單一條 **semantic** 記憶規則，然後將來源標記為已解決：

```
Agent 的未解決錯誤，依 MistakeCategory 分組
     |
     v
count_unresolved_by_category(agent, Capability) = 3
     |
   < 3？ ──是──> 不動作
     |
   >= 3
     |
     v
query_unresolved_by_category(...)  → MistakeEntry[]
     |
     v
synthesize_rule(category, mistakes)   ← 確定性，不呼叫 LLM
  "Recurring capability issues consolidated from 3 past mistakes.
   Apply extra care: ..."
     |
     v
存為「一條」semantic 記憶   （source_event = "reflexion_consolidation"）
     |
     v
mark_resolved(source ids)   ← 那三筆原始錯誤現在已解決
```

合成是**抽離且確定性的**——沒有 LLM 往返。三件散落的事故收斂成一條 Agent 日後會讀到的常設規則。

```
之前：                           之後：
  ☒ 錯誤 A (capability)           ✓ A 已解決 ─┐
  ☒ 錯誤 B (capability)  ───►      ✓ B 已解決 ─┼─► 1 條 semantic 規則
  ☒ 錯誤 C (capability)           ✓ C 已解決 ─┘   "Apply extra care: ..."
```

---

## F3：Batch Fetch（`memory_fetch_batch`）

重建上下文往往意味著要以 id 取回許多特定項目。一次一個 MCP 呼叫去做既慢又囉嗦。`get_by_ids`（引擎）與 `memory_fetch_batch` MCP 工具可在單次呼叫取回至多 **100** 筆：

```
memory_fetch_batch { "ids": ["m_1", "m_2", "m_404", ...] }   （上限 100）
     |
     v
get_by_ids(namespace, ids)
  SELECT ... FROM memories WHERE agent_id = ? AND id IN (?,?,?...)
     |  （強制 namespace／所有權檢查——屬於其他
     |   namespace 的項目與不存在無法區分）
     v
切分請求的 id：
  命中    → memories[]
  缺失    → missing_ids[]   ← 不是錯誤
     |
     v
{ "memories": [...], "missing_ids": ["m_404"],
  "total_found": N, "total_missing": M }
```

關鍵性質：

- **硬上限 100**——`ids` 超過 100 會被拒絕，防止失控查詢。
- **部分命中不是錯誤**——命中的項目連同 `missing_ids` 清單一起回傳。
- **不洩漏存在性**——屬於其他 namespace 的項目與不存在的 id，兩者都落入 `missing_ids`。呼叫者無法探測其他 Agent 擁有什麼。

---

## 設定

沒有什麼需要開啟。記憶智能搭載在既有的記憶引擎上：

- **F1** 在呼叫者把 `subject` + `predicate` 傳給 `store_temporal` 的當下生效；單純儲存維持不變。
- **F2a** 只要 channel-reply 路徑上存在 `ctx.mistake_notebook` 就會觸發。
- **F2b** 使用 `DEFAULT_CONSOLIDATE_THRESHOLD = 3`。
- **F3** 以 `memory_fetch_batch` MCP 工具揭露，與其他每個記憶工具一樣受 scope 管控。

遷移在引擎初始化時自動執行——既有資料庫由冪等的 ALTER 迴圈就地升級。

### 寫入端投毒防護（D2）

寫入端的爆量偵測器預設開啟，可在 `config.toml` 調整。區段缺失或格式錯誤時會回退到下列預設值（fail-safe，偵測器維持開啟）：

```toml
[knowledge_guard]
enabled = true          # 同來源爆量偵測器的總開關。預設 true
window_secs = 3600      # 滑動窗長度（秒）。預設 3600（1 小時）
max_per_subject = 5     # 一個來源在窗內對同一 subject 可寫入的事實上限，超過即隔離。預設 5
```

寫入路徑上的注入掃描是無條件執行的（無設定項）。排序信任權重 `w_trust`（預設 0.10）位於 `RetrievalWeights`（per-engine，非 config 鍵）；當 `w_trust = 0.0` 時，排序與 D2 之前逐位元組相同。

---

## 為什麼這很重要

### 事實不再悄悄過期

在 F1 之前，一筆記憶會永遠寫著「部署目標是 Vercel」，即使使用者早已轉到 Cloudflare。現在舊事實被關閉、新事實接手，一般搜尋只會回傳*當下*為真的內容——而歷史仍可透過 `get_history` / `get_at` 查詢。

### 錯誤複利成能力

F2 在預測引擎的錯誤訊號與 Agent 未來行為之間閉環。錯誤不只是被記錄——它會在相似主題上被浮現（F2a），一旦重複發生，便固化為一條常設的 semantic 規則（F2b）。Agent 在模型不變的情況下變得更好。

### 取回不付往返稅

F3 把 N 次囉嗦的 MCP 呼叫變成一次，附帶乾淨的部分命中契約，且不會跨 namespace 洩漏。上下文重建變得便宜。

### 設計上即非侵入

這一切都無須重寫 schema 或新建 `MemoryEntry`。九個可為 NULL 的欄位、兩個索引、一個冪等遷移，以及一本早已存在的 notebook。整個功能疊加在現役引擎之上。

---

## 總結

一疊扁平的筆記什麼都不忘，也什麼都不學。一份好病歷兩者兼具：它替事實打上時間戳，讓舊的能優雅退場；它把重複的錯誤化為常設準則；它讓你一次就調出需要的那幾頁。記憶智能賦予每個 DuDuClaw Agent 這份病歷——建構於它早已擁有的記憶引擎之上。
