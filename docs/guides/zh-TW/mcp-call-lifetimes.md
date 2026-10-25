# 單次 MCP HTTP 操作的授權生命週期

更新日期：2026-10-25。狀態：`duduclaw-llm` 的未發布開發者 API，使用 `mcp-client` feature。

主機程式可以為 MCP 工具探索或單次工具呼叫提供新的 bearer 與該次操作持有的 lease。本文件說明共用傳輸 API；下述 bearer／lease API 保留為一般 Generic 路徑。未發布的 Google 個人掛載使用下節獨立 Closed 路徑，沒有新增儀表板開關。

## 主機 API 契約

| API | 契約 |
|---|---|
| `ScopedHttpRecipe::new(url, headers, operation_timeout)` | 不可變的連線描述。接受 HTTPS；HTTP 只接受解析後為 localhost 或 loopback 的位址。拒絕 URL 帳密／fragment、`Authorization`／`Proxy-Authorization`／`Cookie`／`Cookie2`／`Mcp-Session-Id` 標頭、不合法標頭、零逾時及超過 300 秒的逾時。不保存 bearer 或 MCP session，須由主機依可信設定建立。 |
| `RegistryClient::scoped_http(recipe, guard)` | 將連線描述與主機持有的 `Arc<dyn ToolInterceptor>` 配對。 |
| `ToolRegistry::from_client_slots_named(clients, filters)` | 同時容納 `Persistent` 與 scoped slot；scoped 名稱不可空白。探索工具前先把 guard 綁定到實際 client index，邏輯名稱或遠端 metadata 不能替換此綁定。 |
| `ToolInterceptor::prepare_client_call(server, operation, args)` | 處理 `ClientOperation::Discover` 或 `ClientOperation::Tool { name }`。回傳的 `PreparedClientCall` 包含最終參數、`Some(lease)` 與 `Some(fresh_http_bearer)`；任一授權欄位缺少便拒絕。 |
| `ToolCallLease::validate_before_send(&mut self)` | 唯讀、可重複呼叫的即時權限檢查，不可更新憑證或另外取得 lease。 |
| `SecretBearer::new(value)` | 持有本次新的 bearer，沒有 `Debug`、`Clone` 或序列化實作。 |

既有 interceptor 若只實作 `before_call` 或 `before_call_async`，無法授權 scoped slot：預設準備方法拒絕探索，工具呼叫也不會取得 bearer 或 lease。已有 guard 的 client index 不能再註冊另一個 guard。

## 單次操作的範圍

工具探索會在 `initialize`、initialized 通知與 `tools/list` 前先取得自己的授權。工具呼叫則等待實際 slot 的 mutex，準備授權，再建立新的 HTTP client 與 MCP session。同一份 bearer 與 lease 涵蓋本次握手及工具請求，不留給下一次操作使用。

每個 POST 送出前都檢查 lease，包含初始化與通知；回應解析完成、釋出內容前會再檢查，通知被接受後也會重驗。檢查不會重新選另一個帳號。主機須確保 lease 檢查的權限，對應準備階段回傳的同一份 bearer。

操作逾時包含等待 slot、準備、握手、HTTP 回應及 registry 結果處理；探索也有涵蓋準備與傳輸的逾時。正常完成、錯誤、逾時或取消，都會丟棄這次的 client 與 lease。此模式關閉 HTTP 重新導向與重試，結果未確認時不會自動重連或重播。需要跨多次操作保留同一個 session 的伺服器，不適用這個 scoped 模式。

## 結果與限制

準備失敗只回固定拒絕訊息；傳輸、回應重驗或逾時失敗回固定的「結果未確認」訊息，遠端可能已經執行。等待 slot 時逾時不代表請求已送出。丟棄本機 future 只能釋放本機授權，不能證明遠端寫入已取消；呼叫端不可自動重試結果未確認的操作。

Scoped 回應會把本次 bearer 從回傳 JSON 的 key 與 value 遮蔽，外部服務的原始錯誤本文及準備方法的任意錯誤文字不會釋出。`SecretBearer` 在 Drop 時會盡力覆寫自己持有的 buffer，但不保證呼叫端、allocator 或 HTTP library 留下的副本已清除。

`RegistryClient::Persistent` 保留既有連線與 `before_call_async` 路徑；scoped 準備方法取代該 slot 的 before hook，不會重複授權。原本的 filter、同名工具先到者優先、`after_call`、來源證明／CCR 與主機提示仍走 registry 流程。

## Google 主機的 Closed 接線（未發布原始碼草稿）

主機明確使用 `RegistryClient::scoped_http_closed(recipe, host_binding, guard)`。真正的 slot 建立私有建構子的 `ClosedClientTarget`，綁定 server、recipe、filter、原設定項目與絕對期限。`prepare_closed_client_call` 必須回傳可消耗一次的 `OwnedScopedOperation`，預設拒絕。回傳型別錯誤或最終參數不同時拒絕，不改走 Generic bearer／lease API。

Gateway 在移交中繼資料工作前取得真正發問者，固定原解析項目、原序號、完整 frame、操作 nonce 與原帳號選擇。三個 POST、JSON／SSE 解碼、文字拼接、已知 JSON 再解析、結果整理及最後 bearer／session 清理由憑證持有人完成，只交出型別化完成結果。Closed 請求及每個 POST 回覆各限 16 MiB，回覆不是整次累計限制；Closed 憑證更新與協定傳輸均停用環境 proxy、重新導向及重試。

同一期限延續到 registry 與外層 loop 結果 hook、來源複核、CCR 準備及下一次模型請求前的交付。逾期內容、帳號告知與新 CCR 代碼一起扣留。`ToolOutcome` 新增 crate 私有的選用欄位：啟用 `mcp-client` 的外部 Rust struct literal 需改用建構子；原建構子預設沒有 Closed policy，Generic／Persistent 原簽名保留。

同步金鑰圈與本機讀寫無法強制搶占，返回後與後續送出前重查期限。取消不撤回已開始的憑證更新、遠端效果或保存。CCR 沿用單次呼叫的中繼資料 tombstone 交易清理，已開始且有限的 SQLite 工作可能稍後完成。當機或清理失敗可能留下未公布資料列，直到既有到期清理；沒有新增持久逾時協調器，也不保證零保存。

新增 M1 測試僅有來源，尚未編譯／執行。關閉 feature 的建置、獨立程式與安全審查、完整 CI、真供應商及使用者驗收均待完成；服務帳號委派、Bridge 與其他 W3 路徑另行處理。


Closed 會暫存 provenance 與結果中繼資料，在同步 ledger／route 建構完成後重驗所有原期限。逾期項目會扣留，ledger 排除其 span 後重建，通過才提交。測試版本另須持有公司共用連線更新端點的 owned fixture；缺少時在解密與供應商送出前拒絕。Legacy 端點政策不變。

## 原始碼對照

- [MCP client 與 registry](../../../crates/duduclaw-llm/src/mcp_client.rs)
- [主機 interceptor 與授權型別](../../../crates/duduclaw-llm/src/tool_loop/interceptor.rs)
- [Scoped HTTP 測試情境](../../../crates/duduclaw-llm/src/mcp_client/scoped_http_tests.rs)

[English](../mcp-call-lifetimes.md) · [日本語](../ja-JP/mcp-call-lifetimes.md)

### W3 帳號來源

未發布的 W3 來源把公司服務帳號與授權代表接入同一 Closed 操作契約，並為原 Google 帳號核准 wire 增加服務帳號／Apps Script 標籤及可選的 `home_source` 分型資料；舊 Personal／OAuth JSON 仍省略此欄。使用 Rust struct literal 建立 `GoogleAccountApproval` 的外部呼叫者必須補 `home_source`；這是 Rust 來源相容性變更，不能宣稱公開結構逐位元不變。沒有新增依賴、權杖 schema 或核准 gate。W3 測試全部尚未編譯／執行。
