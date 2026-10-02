# 認知記憶系統

> 仿照人腦、帶遺忘曲線的記憶：agent 記住重要的事，忘掉不重要的事。

---

## 比喻：大腦怎麼整理記憶

想想你是怎麼記事情的：

- **「上週二我跟 Sarah 喝咖啡，她說她要換工作。」**：這是**情節記憶**（episodic memory），某個時間點發生的具體事件。
- **「Sarah 在行銷部門工作。」**：這是**語意記憶**（semantic memory），一個脫離特定事件的通用事實。

大腦會自然把兩者分開。有人問「Sarah 是做什麼的？」，你會直接取出那個事實，用不著把跟她的每段對話重播一遍。

時間一久，不重要的情節記憶會淡去。你不記得兩週前的週二午餐吃了什麼，卻記得老闆告訴你升遷消息的那頓午餐，因為那件事*重要*。

DuDuClaw 的記憶系統就是照這個架構設計的。

---

## 運作方式

### 兩種記憶庫

**情節記憶**：記錄具體事件，每筆都會存下：
- 時間戳記（什麼時候發生？）
- 標籤與來源事件（由什麼產生，例如一次預測觀察）
- 重要度（0–10）與存取紀錄（被想起的次數與最近一次時間）

範例：
```
[2026-04-05 14:30] User asked about Rust lifetimes in Discord.
  Struggled with 'static lifetime. Explained with analogy.
  importance: 4

[2026-04-06 09:15] User reported a bug in the billing module.
  Root cause: null check missing in invoice calculation.
  importance: 8
```

**語意記憶**：提煉後的事實與知識，不帶時間脈絡：
```
User is a backend developer focused on Rust.
User prefers analogy-based explanations.
The billing module has a history of null-related bugs.
```

### 記憶檢索：三維加權搜尋

agent 要回想某件事時，除了關鍵字，還會用三個維度替記憶排序：

```
Query: "Help me with a Rust lifetime issue"
     |
     v
For each memory entry, compute:
     |
     +---> Recency: How recently was this memory created/accessed?
     |       (Recent memories score higher)
     |
     +---> Importance: How significant was this event?
     |       (Critical decisions > casual chat)
     |
     +---> Relevance: How closely does it match the query?
             (Full-text keyword rank)
     |
     v
Final score = weighted combination of all three
     |
     v
Return top-N memories, sorted by score
```

權重是引擎內建的固定預設值（新近度 0.25、重要度 0.35、關鍵字相關度 0.35），無法逐 agent 設定。另有兩個訊號可以加分：知識圖譜訊號（0.15，在已儲存的主詞–述詞–受詞事實上跑 Personalized PageRank），以及向量相似度訊號（0.15，掛上 embedder 時才有）。最後每個分數再依記憶來源的可信度縮放（權重 0.10）。

這套做法參考史丹佛的 **Generative Agents** 論文。論文顯示，三維檢索比單純的關鍵字搜尋更接近人類的回想方式。

### 記憶衰減：遺忘曲線

記憶不該永遠留著。新近度分數依循 **Ebbinghaus 遺忘曲線**，每天有一個排程把已經淡去的記憶移進封存區：

```
Memory created
     |
     v
  Retrievability R = exp(-t / S)
  (t = days since last access, R starts at 1.0)
     |
     v
  Time passes without access...
     |
     v
  R decays toward 0
     |
     v
  Older than 30 days, importance below 3,
  not semantic, and R below 0.05?
     → Moved to the archive
     (No longer returned by retrieval;
      deleted after 90 days in the archive)
     |
     v
  If accessed again → t resets, so R is back to 1.0,
     and stability S grows with every access
```

穩定度 `S` 取決於重要度與被想起的次數：
- **重要度較高**（重要度 10 時最多 2 倍）：衰減較慢；重要度 3 以上永遠不會被封存
- **常被想起**：`S` 隨 `ln(1 + access_count)` 成長，上限 365 天
- **重要度低、從沒被想起**：衰減最快（基礎穩定度 14 天，再往下縮放）
- **語意記憶**永遠不會被封存

這樣記憶庫就不會無限膨脹。舊的、不重要的記憶自然淡出，檢索維持快速、聚焦。

---

## 全文搜尋

直接用關鍵字搜尋時，系統使用資料庫內建的全文搜尋：

```
User: "Find everything about the billing bug"
     |
     v
Full-text search index scans all memory content
     |
     v
Returns matches ranked by relevance
  - "User reported a bug in the billing module..."
  - "The billing module has a history of null-related bugs..."
  - "Fixed billing calculation for edge case..."
```

它和三維加權搜尋互補：知道自己要找*什麼*時用全文搜尋，需要依脈絡回想時用三維加權搜尋。

### 向量相似度

要找出沒有共用完整關鍵字、但內容相近的記憶時，引擎可以比對 embedding 向量：

```
Query: "invoice calculation error"
     |
     v
Convert to embedding vector
     |
     v
Cosine similarity against the agent's embedded memories
     |
     v
Results include memories about:
  - "billing module null check" (semantically related)
  - "price rounding issue in orders" (similar domain)
  - "tax calculation edge case" (conceptually adjacent)
```

內建的 embedder 是本機的字元 n-gram 雜湊 embedder（不用下載模型），比對的是重疊的字詞片段（中日韓文字也適用），不像神經網路 embedding 模型那樣比對語意。比對方式是逐筆掃描，沒有另建向量索引。gateway 的記憶引擎在 `[memory] novelty_gate` 開啟時（預設開啟）會掛上它；MCP 記憶工具則要設定 `DUDUCLAW_SEMANTIC_VECTORS=1` 才會掛上。

---

## 跨 agent 知識共享

記憶以 agent 為單位。記憶工具（`memory_search`、`memory_store`、`memory_read`…）只讀寫呼叫者自己的命名空間，也沒有逐筆記憶的共享等級。多個 agent 都需要的知識改放共享 wiki：

```
Agent A (customer support) needs product info
     |
     v
Search the shared wiki (wiki_search scope="shared")
     |
     v
Visibility check:
  Does wiki_visible_to allow this agent?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Return  Not
result  visible
```

知識分成兩層：
- **agent 記憶與 agent wiki**：只有擁有它的 agent
- **共享 wiki**（`~/.duduclaw/shared/wiki/`）：`wiki_visible_to` 權限允許的 agent

這和組織處理資訊的方式一樣：有些知識屬於部門，有些全公司共用，有些只給需要知道的人。

---

## Wiki 知識庫

在對話記憶之外，系統把結構化知識存成 wiki 頁面：

```
Knowledge source
  (wiki_write by an agent, operator edits,
   reference documents auto-filed from conversation)
     |
     v
Wiki page:
  - Markdown with frontmatter
  - Agent-local or shared scope
  - Indexed for full-text search
     |
     v
Knowledge base (searchable with wiki_search)
```

儀表板的知識中心頁面有一張**關聯圖**，顯示 wiki 頁面如何透過共同主題連在一起。參見 [Wiki 知識層](17-wiki-knowledge-layer.md)。

---

## 為什麼重要

### 個人化互動

有記憶的 agent 不會每次對話都從零開始。它記得使用者的偏好、過去的問題和溝通風格，體驗從「每次都在跟陌生人講話」變成「在跟認識你的人講話」。

### 知識累積

agent 會隨時間累積對自身領域的理解。客服 agent 會記住常見問題、已知的繞過方法、個別使用者的設定。這些知識跨 session 保留，回覆品質也跟著提升。

### 可擴展的記憶

遺忘曲線讓記憶不會無限成長。系統自然維持一組相關、近期的記憶，同時讓舊的、不重要的記憶淡去，不需要手動清理。

### 跨 agent 的智慧

有了共享 wiki，知識不會被困在單一 agent 裡。某個 agent 寫進去的產品洞見，可以在操作者設定的可見範圍內，同時服務客服、業務和文件 agent。

---

## 與其他系統的互動

- **預測引擎**：在通道回覆之後寫入情節觀察（`source_event = prediction_episodic`）。
- **對話萃取**：對話中的事實成為語意記憶；參考文件成為 wiki 頁面，記憶裡只留一個簡短指標。
- **記憶智慧**：時間性取代、reflexion 規則與來源可信度都建立在這個引擎上。參見 [記憶智慧](20-memory-intelligence.md)。
- **儀表板**：可在網頁介面查看記憶內容、搜尋，以及知識中心的關聯圖。

---

## 重點整理

記憶讓無狀態的聊天機器人變成有用的助理。DuDuClaw 依人類認知來設計記憶：情節與語意分離、依重要度加權的檢索、自然遺忘、共享的知識庫，讓 agent 能從每次互動中學習、記住、成長。
