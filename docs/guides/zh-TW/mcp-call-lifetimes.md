# 單次 MCP HTTP 操作的授權生命週期

更新日期：2026-10-24。狀態：`duduclaw-llm` 的未發布開發者 API，使用 `mcp-client` feature。

主機程式可以為 MCP 工具探索或單次工具呼叫提供新的 bearer 與該次操作持有的 lease。本文件說明共用傳輸 API；這項變更尚未接入 Gateway 或 Google 個人帳號流程，也沒有可供使用者切換的儀表板或設定開關。

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

## 原始碼對照

- [MCP client 與 registry](../../../crates/duduclaw-llm/src/mcp_client.rs)
- [主機 interceptor 與授權型別](../../../crates/duduclaw-llm/src/tool_loop/interceptor.rs)
- [Scoped HTTP 測試情境](../../../crates/duduclaw-llm/src/mcp_client/scoped_http_tests.rs)

[English](../mcp-call-lifetimes.md) · [日本語](../ja-JP/mcp-call-lifetimes.md)
