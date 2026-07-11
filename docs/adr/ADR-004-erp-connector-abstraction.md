# ADR-004: ERP connector abstraction (`trait ErpConnector`)

- Status: Accepted
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

DuDuClaw 的 ERP 橋接目前只有一個具體型別 `struct OdooConnector`
(`crates/duduclaw-odoo/src/connector.rs:103`),沒有任何抽象層。它提供
connect / execute_kw / search_read / create / write / count / version / status
一組方法,15 個 Odoo MCP tools(CRM / Sales / Inventory / Accounting,分派在
`crates/duduclaw-cli/src/mcp.rs`)全部直接吊在這個型別上。per-agent 憑證與
scope 隔離已在 RFC-21 §2 落地:`AgentOdooConfig` / `OdooConfigResolver`
(`crates/duduclaw-odoo/src/agent_config.rs`)、連線池 `OdooConnectorPool`
(`crates/duduclaw-cli/src/odoo_pool.rs:54`)以 `(agent_id, profile)` 為 key,
外加 `Scope::OdooRead / OdooWrite / OdooExecute`(`crates/duduclaw-cli/src/mcp_auth.rs:52`)。
問題是這套隔離機制是為 Odoo 量身寫的,換一個 ERP 就要整包重做。

客戶調研(§1)給出明確訊號:Odoo 的定位落在 15-50 人的公司,大型企業客戶
不會只跑 Odoo。要讓 SAP、ERPNext、Twenty 這類系統融進同一個 agent 平台,
得先有一層 adapter,而不是每接一家 ERP 就複製一次 `OdooConnector` 的血肉。

範式已經在專案裡跑了半年。`duduclaw-llm` 的 `trait ChatProvider`
(`crates/duduclaw-llm/src/provider.rs:103`)用 `#[async_trait]` 定義
`id()` / `complete()` / `stream()` 三個方法,底下掛 Anthropic / OpenAI / Gemini /
OpenAI-compat 四個實作,加上 `ModelRegistry` 這種資料驅動的能力表。同一個平台
在 LLM 這側已經證明「一個 trait + N 個 provider + registry」能長期維護。ERP
這側沒有理由重新發明。

trait 切面的完整規格(方法簽名、`duduclaw-erp` 骨架 crate 的拆分、ERPNext
實作細節)已由 `commercial/docs/TODO-feature-gaps-2026-07.md` §一 規劃完成,
那是調研產出。本 ADR 只做一件事:把那份規劃升格為正式決策,並記錄取捨。
規格不在此重述——執行時兩份文件同時打開。

## Decision

抽出 `trait ErpConnector`,比照 `ChatProvider` 的模式:`#[async_trait]` +
穩定的 `id()` + registry 化的能力宣告。trait 方法:

- `id()` — 穩定的 connector id(`"odoo"` / `"erpnext"` / …),對齊 `ChatProvider::id()`。
- `capabilities()` — 宣告支援的模型 / 動作 / webhook,讓上層資料驅動地路由。
- `search` / `read` / `create` / `update` — CRUD 四件套,對映 Odoo 現有的
  search_read / create / write。
- `execute` — 通用動作(對映 Odoo 的 execute_kw / 業務動作如 sale_confirm)。
- `webhook_subscribe`(optional)— 事件訂閱;不支援的 connector 回 not-supported,
  不強迫每家都實作。

**Odoo 是第一個實作**:`OdooConnector` 改為 `impl ErpConnector`,行為零變更,
既有 15 個 MCP tools 的輸出保持 byte-compatible,拿現有測試當回歸網。
**ERPNext 是第二個實作**,它的作用是驗證這層抽象——第二家接得進來,trait
才算真的抽對了。只有一個實作的 trait 是猜測,兩個才是證據。

**per-agent credential / scope / audit 三件套是 trait 合約的一部分,不是 Odoo
特例。** RFC-21 §2 那套 `(agent_id, profile)` 連線池、`allowed_models` /
`allowed_actions` 過濾、`profile` 進稽核記錄的機制,要上移成泛型:任何
`ErpConnector` 實作都透過共用的 `ConnectorPool<C>` 拿連線,都吃同一套 scope
檢查與稽核歸屬。新 connector 免費得到隔離,不會有人接 ERPNext 時忘了做權限
隔離。

**MCP tool 命名統一為 `erp_*`**(`erp_record_search` / `erp_record_create` /
`erp_record_update` / `erp_execute` …),舊的 `odoo_*` 名稱保留一個棄用週期的
別名。deprecation 期間兩套名字都能呼叫,期滿移除 `odoo_*`。這讓既有 agent 的
prompt 與 skill 不會在同一個版本內斷掉。

## Consequences

**得到的:** 接第二家(ERPNext)不再是複製貼上;隔離機制一次寫對、全部
connector 共享;業務對大型企業客戶有「抽象層就緒、規劃中 X 家」的明確話術
(見 `docs/features/erp-support-matrix.md`);MCP tool 命名收斂成 `erp_*`,
不再洩漏底層是哪家 ERP。

**付出的:** 抽 trait 有立即成本——要拆 `duduclaw-erp` 骨架 crate、把 Odoo
改成實作者、驗證 15 個 tools 回歸全綠,而這一切在 ERPNext 真的動工前不產生
任何對外可見的新功能。誠實的取捨:**現在抽 vs 等第二家再抽**。

我們選擇現在抽,理由是客戶語境把 ERP 擴充的優先級上修到本輪,ERPNext 已
排進 backlog(`IMPL-PLAN-remaining-gaps-2026-07.md` §E),trait 與第二個
connector 同步落地才能驗證抽象是否正確。等第二家動工時才抽,會逼著在時間
壓力下同時做「抽象 + 新實作 + 回歸」三件事,風險更高。代價是接受一段沒有
新功能、只有結構重整的工期,並靠 Odoo 既有測試把回歸風險壓住。

**棄用相容:** `odoo_*` 別名一版後移除。移除前 CHANGELOG 標 Deprecated,
移除時標 Removed,升級指引寫進 guide。

**若假設落空:** 如果 ERPNext 接入時發現 trait 切面漏了東西(例如某些 ERP
的 batch / transaction 語意 Odoo 沒有),以新 ADR 修訂本決策,不在此檔就地
擴張方法列表。
