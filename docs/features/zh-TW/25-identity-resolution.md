# 身分解析

> 「這個人是誰」的單一事實來源——一個 provider trait、三種後端，以及 Agent 每一輪都會讀到的 `<sender>` 區塊。

---

## 比喻：櫃檯接待員與通訊錄

想像一棟大樓前台坐著一位接待員。當訪客走進來，接待員不會用猜的。他會查閱公司通訊錄——那個權威系統會告訴你「這是 Ruby Lin，客戶 PM，已獲准進入 Alpha 與 Beta 兩個專案」。

但通訊錄伺服器偶爾會停機維護。一位稱職的接待員會在抽屜裡放一份列印的名冊——那是上次通訊錄還連得上時的快取副本。當線上系統離線時，他改查那張紙，而不是把所有人都擋在門外。

而且一旦接待員確認了你的身分，他不會讓每個部門再次驗證你。他會在你胸前別上一張 **訪客識別證**，上面寫著你的姓名、角色，以及可進入哪些樓層。你拜訪的每個部門都讀這張證，而不是重新去查通訊錄。

DuDuClaw 的身分解析正是如此：

- **通訊錄** 是上游 provider（`NotionIdentityProvider`）。
- **抽屜裡那份列印名冊** 是 wiki 快取（`WikiCacheIdentityProvider`）。
- **接待員的後備邏輯** 是 `ChainedProvider`（線上 → 快取）。
- **訪客識別證** 是注入到 Agent 系統提示中的 `<sender>` 區塊——解析一次，每一輪都讀。

---

## 要解決的問題

在 RFC-21 §1 之前，DuDuClaw 的 Agent 沒有辦法問「跟我說話的這個人是誰？」唯一可用的機制是對一個事先知道的路徑（例如 `identity/discord-users.md`）呼叫 `shared_wiki_read`。那個檔案只列了兩個人。其他所有人——團隊成員、客戶聯絡人、工程師——都是看不見的陌生人。

Agent 在 SOUL.md 裡宣告了像「拒絕非專案成員」這樣的規則，卻沒有名冊可以拿來評估這條規則，也沒有機制去查詢權威來源。邊界活在散文裡，而不是資料裡。

修正之道是 **系統層的權威，而非提示層的建議**：引入 `IdentityProvider` trait，把 wiki 從事實來源降格為透明快取，並把解析出的人物以結構化資料餵進系統提示——讓 SOUL.md 規則變成可評估，而不只是空談。

---

## Provider Trait

`IdentityProvider`（位於 `duduclaw-identity` crate）是一個小巧的 async trait。所有方法都是 async，因為正式環境的 provider（Notion、LDAP、自訂 HTTP）涉及網路 IO；但純本地的實作也遵循同一介面，因此可以在不更動呼叫點的情況下換進換出。

```
#[async_trait]
trait IdentityProvider: Send + Sync {
    async fn resolve_by_channel(channel, external_id)
        -> Result<Option<ResolvedPerson>, IdentityError>;

    async fn lookup_project_members(project_id)
        -> Result<Vec<ResolvedPerson>, IdentityError>;

    fn name(&self) -> &str;   // "notion" / "wiki-cache" / "chained"
}
```

### `Ok(None)` 的語意

一個關鍵的設計決定：當人物未知時，`resolve_by_channel` 回傳 `Ok(None)`。這是正常的「陌生人傳訊息」情況，明確地 **不是** 錯誤。`Err` 保留給真正的 provider 失敗——上游連不上、payload 格式錯誤、IO 故障。正是這個區分，讓鏈式 provider 能夠優雅降級。

---

## 三種 Provider

| Provider | 來源 | 行為 |
|----------|------|------|
| `WikiCacheIdentityProvider` | `<home>/shared/wiki/identity/people/*.md` | 從本地 Markdown 讀取 YAML frontmatter 記錄。單一格式錯誤的檔案會被跳過（伴隨 `tracing::warn!`），絕不會讓整個解析器掛掉。 |
| `NotionIdentityProvider` | Notion `databases/query` API | 查詢一列一人的 People DB；operator 透過可設定的 `field_map` 把邏輯欄位對應到 Notion 屬性名稱。5xx/網路 → `Unreachable`；4xx/schema → `Malformed`。 |
| `ChainedProvider` | 快取 → 上游 | 先試快取；未命中則落到上游；上游故障時降級為「無法解析」而非報錯。 |

### WikiCacheIdentityProvider schema

`identity/people/` 下每個 `*.md` 檔都帶有一個 YAML frontmatter 區塊；內文是 provider 會忽略的自由筆記：

```
---
person_id: person_2f9
display_name: Ruby Lin
roles: [customer-pm]
project_ids: [proj-alpha, proj-beta]
emails: [ruby@example.com]
channel_handles:
  discord: "1234567890"
  line: "Uabc"
---

關於 Ruby 的自由筆記——provider 永遠不會讀這裡。
```

### NotionIdentityProvider 欄位對應

Notion 的屬性名稱因部署而異，因此 operator 宣告一個 `NotionFieldMap`。預設值符合合理慣例，但每個欄位都可覆寫：

```
field_map = {
  name     = "Name",
  roles    = "Roles",
  projects = "Projects",
  channel_props = {
    discord  = "Discord ID",
    line     = "Line ID",
    telegram = "Telegram ID",
    email    = "Email",
  },
}
```

每次 `resolve_by_channel` 呼叫都會以一個 filter 查詢 `databases/query`，篩出 channel-handle 屬性等於該 `external_id` 的記錄。

---

## ChainedProvider 後備機制

`ChainedProvider` 就是接待員的後備大腦。它包住一個快速快取與一個緩慢的權威上游：

```
resolve_by_channel(channel, external_id)
        |
        v
  ┌─────────────────────────────┐
  │ 1. 快取快速路徑              │
  │    cache.resolve(...)       │
  └─────────────────────────────┘
        |
   Ok(Some) ──────────────► 回傳快取中的人物（短路）
        |
   Ok(None)〔未命中〕        Err〔快取故障〕
        |                        |
        |   warn! 「快取錯誤——落到上游」
        v                        v
  ┌─────────────────────────────┐
  │ 2. 上游緩慢路徑             │
  │    upstream.resolve(...)    │
  └─────────────────────────────┘
        |
   Ok(person) ────────────► 回傳上游結果
        |
   Err〔上游故障〕
        |
   warn! 「上游錯誤——降級為無法解析」
        v
   回傳 Ok(None)   ← Agent 把寄件者視為陌生人，而非硬錯誤
```

關鍵特性：當 Notion 連不上時，頻道回覆仍然繼續進行。Agent 只是看不到 `<sender>`，於是把訊息當成來自陌生人——正是接待員退回到列印名冊，絕不鎖上大門。

`lookup_project_members` 則反轉偏好：它 **先查上游**，因為專案成員資格正是那種會在快取裡漂移的資料。只有在上游出錯時才退回快取（並發出 `tracing::warn!` 讓 operator 注意到這次降級）。

---

## ResolvedPerson 記錄

成功的解析會回傳一個正規化的 `ResolvedPerson`。只有上游 provider 能產出這些；下游呼叫者收到的是不可變的查詢結果。

| 欄位 | 型別 | 意義 |
|------|------|------|
| `person_id` | `String` | 來自事實來源的穩定正規 id（例如 Notion page id）。視為不透明。 |
| `display_name` | `String` | 人類可讀的名稱，例如「Ruby Lin」。 |
| `roles` | `Vec<String>` | 領域角色，例如 `["customer-pm", "engineer"]`。 |
| `project_ids` | `Vec<String>` | 專案成員資格——「拒絕非專案成員」就是拿這個來評估。 |
| `emails` | `Vec<String>` | 相關的 email 位址；可能為空。 |
| `channel_handles` | `BTreeMap<String, String>` | `{channel-wire-name: external_id}`。用 `BTreeMap` 以確保序列化順序確定。 |
| `source` | `String` | 產出此記錄的 provider（`"notion"`、`"wiki-cache"`）。會被帶進審計日誌。 |
| `fetched_at` | `DateTime<Utc>` | 快取記錄帶快取寫入時間；即時記錄帶上游抓取時間。 |

`ChannelKind` 列舉涵蓋 Discord、Line、Telegram、Slack、Whatsapp、Feishu、Webchat 與 Email，外加一個 `Other(String)` 的萬用項，讓自架 webhook 或未來的頻道永遠不會解析失敗。

---

## identity_resolve MCP 工具

Agent 透過一個 MCP 工具觸及身分解析，並由專屬 scope 把關：

```
identity_resolve { channel, external_id }
        |
        v
  Scope 檢查：呼叫者 principal 必須持有 Scope::IdentityRead
        |  （缺少 scope → 拒絕，fail closed）
        v
  provider.resolve_by_channel(channel, external_id)
        |
        v
  ResolvedPerson JSON   ── 或 ──   null（未知寄件者）
```

這道 scope 閘門遵循 DuDuClaw「安全閘門 fail closed」的慣例——沒有 `Scope::IdentityRead` 的金鑰會被拒絕，絕不悄悄放行。

---

## `<sender>` 區塊：把身分當成資料

最有影響力的整合是自動發生的。當頻道訊息抵達時，gateway **每輪解析一次** 寄件者，並把一個以 XML 分隔的 `<sender>` 區塊注入系統提示：

```
系統提示
  ├─ SOUL.md（人格 + 規則）
  ├─ ## Your Team（子 Agent 名冊）
  ├─ <sender>                          ← 注入，每輪解析一次
  │    <person_id>person_2f9</person_id>
  │    <name>Ruby Lin</name>
  │    <roles>customer-pm</roles>
  │    <project_ids>proj-alpha, proj-beta</project_ids>
  │    <source>notion</source>
  │  </sender>
  └─ ... 其餘上下文
```

這就是訪客識別證。在這項功能之前，像「拒絕非專案成員」這樣的 SOUL.md 規則，需要 Agent 在推理途中記得去呼叫 `shared_wiki_read`——這一步它常常跳過。現在成員資料已經擺在它眼前，位於高注意力位置，每一輪都在。規則變成可從 Agent 已持有的資料來評估。

當 provider 未設定或寄件者未知時，不會注入 `<sender>` 區塊——Agent 只把訊息當成來自陌生人，套用 SOUL.md 的陌生人處理規則。

---

## 為什麼這很重要

### 系統層的權威，而非提示層的指望

SOUL.md 指令是盡力而為的——模型可能遵循也可能不遵循。身分解析把邊界搬到可以對照真實資料評估的地方。「拒絕非專案成員」不再是一句提示，而成為對 Agent 真正看得到的 `project_ids` 的檢查。

### 優雅降級，絕不鎖門

`ChainedProvider` 的軟失敗設計，意味著上游故障是降低保真度（未知寄件者），而不是中斷對話。Notion 的維護視窗不會拖垮你的 Agent——它們退回 wiki 快取，再退回陌生人處理，然後繼續回覆。

### wiki 成為快取，而非事實來源

把 `WikiCacheIdentityProvider` 變成三種後端之一，等於把共用 wiki 從「Agent 手刻身分查詢的地方」降格為「權威系統的透明快取」。這防止演化迴圈悄悄把 wiki 漂移成外部資料的失控副本。

### 以 trait 插拔，而非以 fork

由於每個後端都實作同一個 `IdentityProvider` trait，把 Notion 換成 LDAP 或自訂 HTTP 通訊錄只是更換 provider，而不是重寫頻道回覆路徑。`<sender>` 注入、MCP 工具與 scope 閘門全都維持不變。

---

## 總結

一位用猜的接待員是個風險。而一位會查通訊錄、在通訊錄停機時退回列印名冊、並替每位訪客別上識別證讓各部門無須重查的接待員——那才是你可以拿來建立規則的系統。DuDuClaw 的身分解析賦予每個 Agent 這樣一位接待員：一個 trait、三種 provider、優雅降級，以及一張 Agent 每輪都會讀的 `<sender>` 識別證。「這個人是誰？」不再是猜測，而成為一次查詢。
