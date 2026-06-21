# Session Memory Stack

> Instruction Pinning、Snowball Recap 與 Key-Fact Accumulator——三層廉價的機制，取代了一套 6,500 token 的重量級記憶系統。

---

## 比喻：主廚的便利貼

主廚可以把整本食譜背下來，但在晚餐尖峰時段他們不會這麼做。他們依賴三種快速參照面：

1. **掛在出菜口上方的訂單票** — 「12 桌：不要海鮮、甜點要全素。」每道菜出餐前都看一眼。
2. **塗寫在點菜單上、持續更新的摘要** — 「步驟 3 完成，醬汁需要加鹽。」翻炒之間再讀一次。
3. **備料檯旁的小卡片盒** — 數週累積下來的筆記（「Park 主廚討厭巴西里裝飾」）。只在某張卡片相關時才抽出來。

這些參照面都不是食譜本身。它們是*廉價、承重的參照面*，正好擺在主廚注意力本來就會經過的位置。DuDuClaw 的 session memory stack 建立在相同的理念上。

---

## 要解決的問題

DuDuClaw 曾短暫推出一套受 MemGPT 啟發的三層記憶系統（Core Memory、Recall Memory、Archival Bridge）。它能運作，但：

- **每次 prompt 多出 6,500 token 的膨脹** — 連短對話都要付出完整的記憶稅。
- **「迷失在中間」的注意力衰退** — 長的注入區塊反而降低回應品質，而非提升。
- **MCP 工具管線需要手動呼叫** — `core_memory_append`、`recall_search` 等——而 Agent 常常忘了呼叫。

v1.8.1 移除了全部 1,985 行。v1.8.6 用三層輕量參照面取而代之，整體便宜約 87%，並且位在模型本來就會注意的位置。

---

## 第 1 層：Instruction Pinning（v1.8.6 P0）

session 中第一則使用者訊息通常包含*核心任務*，之後的內容都是澄清。因此：

```
Turn 1: "Help me migrate this React app from CRA to Vite,
         keep the existing tests, and don't touch the auth flow."
     |
     v
Async Haiku extraction:
   "migrate React app CRA → Vite; preserve tests; don't touch auth flow"
     |
     v
Stored in: sessions.pinned_instructions (SQLite column)
     |
     v
Injected at: system prompt tail (high-attention U-shape position)
```

萃取以*非同步*方式執行——不會阻塞第一個回應。它是一項**中介資料任務（metadata task）**，所以使用 CLI 輕量路徑（`--effort medium --max-turns 1 --no-session-persistence --tools ""`），成本約為正常的 25-40%。

### 澄清累積

當 Agent 提出澄清問題（「我該保留 service worker 嗎？」）而使用者回答時，那個答案會附加到 pinned instructions——上限 1,000 字元以防止漂移。

```
Pinned instructions grow with clarifications:
  "migrate React app CRA → Vite; preserve tests;
   don't touch auth flow;
   [+] keep service worker behavior identical;
   [+] target Node 20 runtime"
```

### 為什麼放在 system prompt 尾端？

LLM 對上下文視窗的開頭與結尾投入不成比例的注意力（U 形曲線）。system prompt 的尾端是注意力最高的位置之一。Instruction Pinning 把任務陳述正好放在那裡——每一輪、每一次呼叫都在。

---

## 第 2 層：Snowball Recap（v1.8.6 P0）

每一輪都會在使用者訊息前面加上一個 `<task_recap>` 區塊：

```
<task_recap>
Pinned task: migrate React app CRA → Vite; preserve tests;
             don't touch auth flow
</task_recap>

Actual user turn: "what about the proxy config?"
```

「snowball（滾雪球）」這個名字源於這樣的事實：recap 會在整段對話中自然累積，而不需要重新 prompt LLM 去記住。它的 LLM 呼叫成本為零——純粹是字串串接。

結合 U 形注意力尾端效應，這代表模型在每一輪都「看得到」任務，完全不需要額外的 LLM 往返。

---

## 第 3 層：P2 Key-Fact Accumulator（v1.8.6）

有些事實並不專屬於某一個 session——它們描述的是跨越時間的使用者或專案。例如：

- 「使用者的部署目標是 Cloudflare Workers」
- 「偏好的測試函式庫是 vitest，不是 jest」
- 「程式碼庫使用 `pnpm`，絕不用 `npm i`」

MemGPT 的 Core Memory 嘗試捕捉這些，但每輪要花約 6,500 token。Key-Fact Accumulator 只用約 100-150 token 就做到。

### 運作方式

```
Each substantive turn (non-trivial content)
     |
     v
Async Haiku extraction (lightweight CLI path):
   "Extract 2-4 key facts about the user, project, or preferences.
    Skip ephemeral context."
     |
     v
Stored in: key_facts table (FTS5 indexed)
  ┌─────────────────────────────────────────┐
  │ id | agent_id | content | access_count  │
  │ timestamp | source_turn_id              │
  └─────────────────────────────────────────┘
     |
     v
Next turn's system prompt assembly:
  SELECT content FROM key_facts
  WHERE agent_id = ?
  ORDER BY fts5_rank(relevance) DESC
  LIMIT 3
     |
     v
Inject top-3 as ~100-150 tokens
```

每次注入都會增加 `access_count`——常用的事實會持續被顯示出來；一次性的事實則逐漸淡出。

### 與 MemGPT Core Memory 比較

| | MemGPT Core Memory | Key-Fact Accumulator |
|-|-|-|
| 注入大小 | ~6,500 tokens | ~100-150 tokens |
| 檢索方式 | 每次 prompt 都注入完整區塊 | 取 FTS5 排名前 3 |
| 呼叫方式 | 手動 MCP 工具 | 自動注入 |
| 儲存方式 | 持久區塊編輯 | 附加 + 存取追蹤 |
| 有效縮減 | 基準 | **−87%** |

---

## 原生多輪基礎（v1.8.1）

這三層都建立在固定的**原生 session handle** 之上：

```
Claude CLI --resume <session-id>
     |
     v
session-id = SHA-256(agent_id + channel_id + thread_id)
     |
     v
If --resume fails (stale handle, account rotation,
                   unknown stream-json error):
     ↓ auto-fallback
History-in-prompt (XML-delimited turns)
```

這修正了先前 Agnes 會在連續訊息之間失去上下文的行為（「幫我全部開啟」→「你指的是什麼？」）。session id 是確定性的，在*整個* thread 生命週期中保持穩定（v1.8.14 Discord 修正後：用 `is_thread || created_thread` 取代 `auto_thread && !is_thread`）。

### 受 Hermes 啟發的 Turn Trimming

過長的對話輪次（>800 字元）會在送往模型前先被裁剪：

```
Original turn: [850 chars of user input]
     |
     v
Trimmed: [first 300 chars] ... [trimmed 350 chars] ... [last 200 chars]
```

CJK 安全的字元層級切片——不會發生多位元組 codepoint panic。零 LLM 成本。在不喪失開頭意圖與最終指令的前提下，防止冗長貼上造成的 token 膨脹。

### Direct API 快取策略

當回退到 Direct API（`direct_api.rs`）時，請求採用 Anthropic 的「system_and_3」prompt 快取斷點配置——在 system prompt 與倒數第 3 個 assistant 輪次設置快取斷點。這在多輪對話上可達到約 75% 的快取命中率，純 system prompt 命中時可達 95% 以上。

---

## 與演化引擎的互動

session memory stack 並非與演化系統隔離：

- **預測錯誤**會比對模型實際說的內容與 pinned task 所預測的內容。重大偏差會觸發 GVU 反思。
- **Key facts** 會餵入 `external_factors`——使用者修正、偏好訊號——這些會驅動 SOUL.md 更新。
- **Session 壓縮**（50k token 門檻）會產生一份摘要，並注入到 *system prompt*，而非作為新的對話輪次。

---

## 為什麼這很重要

### 成本

輕量 CLI 路徑搭配只注入排名前 3 的 key facts，讓中介資料開銷維持在總 token 預算的 10% 以下——相較於 MemGPT 的 30-40%。

### 注意力品質

把 pinned instructions 與 key facts 放在 system prompt 尾端（高注意力），把 snowball recap 放在使用者訊息開頭（同樣高注意力），每一輪都讓任務陳述出現在*兩個*高注意力位置。模型不必在長上下文的中間翻找它們。

### 不依賴工具呼叫

舊的 MemGPT 設計需要模型主動呼叫 `core_memory_append`，Agent 有時會忘記。新的 stack 完全由注入驅動——不論模型是否配合或分心，它都能運作。

### 向後相容的降級

如果 Haiku 萃取失敗（rate limit、逾時），session 仍然可運作——只是該輪沒有 pinning／facts 的好處而已。不會有任何東西損壞。

---

## 總結

主廚不會在每道菜出餐前重新背一遍食譜。他們看一眼出菜口上方的便利貼、掃一下持續更新的點菜單，偶爾從備料盒抽出一張卡片。DuDuClaw 的 session memory stack 採用相同的架構：把廉價的參照面放在高注意力位置，而不是一個與實際工作爭搶資源的重量級記憶區塊。
