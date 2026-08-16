# 自訂儀表板 Widget

> 你的儀表板、你的卡片:用白話描述,或直接寫 HTML,永遠跑在沙盒裡。

---

## 這是什麼

DuDuClaw 的首頁儀表板是一份 widget 清單(Agent 名冊、任務、通道健康度……)。Custom Widgets 讓你自己動手做卡片,擴充這份固定目錄,有兩條共用同一套 runtime 的製作路徑:

- **AI 引導流程**(所有使用者可用):挑資料來源、挑呈現風格,用自己的話描述想要什麼。模型生成一張 widget,你即時預覽,不滿意就回饋再跑一輪,滿意了再存檔。
- **原始 HTML**(管理員限定):完整的 HTML 編輯介面,搭配同一套即時預覽,設計給經銷商工程師為單一企業客戶客製部署用。Widget 以 `.json` 檔匯出/匯入,讓經銷商能把一張卡片從一個客戶部署搬到下一個。

存檔後的 widget 住在 **Widget Studio**(`/widgets`):可以分享給整個 instance、把別人分享的卡片加進自己的看板,或複製一份當起點。

## 沙盒

一個 custom widget 是單一、自成一體的 HTML 片段。儀表板把它渲染進一個 iframe,並套上:

- `sandbox="allow-scripts"`、且**不給** `allow-same-origin`:widget 拿到一個獨立的 origin,讀不到儀表板的 DOM、cookies、localStorage,也讀不到你的登入 token。
- 注入的 **Content-Security-Policy** 擋掉所有外部資源與網路呼叫(含 `fetch`/XHR)。widget 看得到的資料出不去。
- 注入的 SDK shim,是唯一一道資料門:

```js
const t = await duduclaw.call('tasks.summary');
// { total, by_status, completed_today, recent: [...] }
duduclaw.onTheme((mode) => { /* 'light' | 'dark' */ });
```

`duduclaw.call` 代理一份固定的**唯讀白名單**(`agents.summary`、`tasks.summary`、`cost.summary`、`channels.status`、`system.status`),一律用*當前檢視者*的 session 去呼叫,所以角色與資料範圍規則跟系統其他地方完全一致。不在名單上的一律拒絕,且呼叫依 widget 個別做速率限制。

主題自動跟隨儀表板(CSS 變數 `--fg`、`--muted`、`--accent`、`--card`、`--border`,外加一個 `data-theme` 屬性),frame 也會依內容自動調整大小。

## 為什麼靠白名單橋接,不靠信任

經銷商寫的 HTML 跑在客戶的儀表板上。沒有隔離的話,「讓工程師客製頁面」等於「讓任何編輯 widget 的人都能冒充登入中的管理員行事」。沙盒把這件事反過來:一個惡意或有 bug 的 widget,最糟也只能在自己的卡片裡畫出難看的東西。它無法提權、無法外洩資料(沒有網路出口),也看不到比正在看它的人原本就能看到的更多資料。

## 版面配置與分享

- 一張 widget 以 `custom:<id>` 項目加入你的看板,跟任何內建卡片一樣可排序、可隱藏。伺服器會拒絕引用你看不到的 widget 的版面項目(fail-closed)。
- 分享是整個 instance 範圍、由擁有者控制;管理員可以管理(移除)任何已分享的 widget。
- 主管用唯讀的*代理檢視*模式時,會看到下屬的 custom widget 內嵌渲染出來——套用跟看板其他部分相同的嚴格層級授權。

## 限制

| 項目 | 限制 |
|---|---|
| Widget HTML 大小 | 256 KB |
| 橋接呼叫 | 每個 widget 每秒 10 次 |
| 橋接方法 | 5 個唯讀摘要(fail-closed) |
| Widget 的網路出口 | 無(CSP 擋死) |

生成流程走帳號輪替的 Claude CLI 路徑,搭配 Direct API 備援,而且是零工具能力集。widget 生成過程永遠碰不到檔案,也跑不了指令。
